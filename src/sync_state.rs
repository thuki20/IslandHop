//! IslandHop：两台电脑互联后，把「主设备」的岛仔业务数据同步到对端。
//!
//! 走 OpenHop 已建好的加密 TCP 连接（`Msg::Ext::IslandState` / `Hub::tell`），
//! 不另起端口、不重复实现发现/配对/转发。设计口径：
//!
//! * **同步什么**：`settings.txt` 里与设备无关的白名单键（喝水 / 久坐 / 专注 /
//!   天气城市 / 常用偏好）、`todo.txt`、`water.txt` + `water-extra.txt`、
//!   `shelf.txt`，以及 `ai-quota.json` 的**账号元数据**。
//! * **不同步什么**：`display_id` / `dock` / `island_offset` / `avoid_apps` 等
//!   设备相关键；天气缓存（两台出口 IP 不同，各机自拉）；久坐计时与番茄钟剩余
//!   时间（纯本地会话）；剪贴板历史（OpenHop 已有实时剪贴板同步）；以及所有
//!   凭据文件 `credentials/*.bin`、邮件账号密码、Keychain / DPAPI 绑定的 token。
//! * **冲突口径**：`rev` 大者胜；`rev` 相同比 `updated_at`；仍相同则 OpenHop 的
//!   `Role::Server`（主设备）胜出。收到并应用后立刻重算本地指纹，避免两台互推回环。
//! * **落盘**：一律「先写盘、再刷新内存」。上游 `App::new()` 与跨天 `tick()` 都会
//!   用磁盘文件覆盖内存，不落盘会在重启/跨天后回退。
//!
//! 账号只同步**清单元数据**（id/provider/alias/slot/enabled/pinned），`source`
//! 登录文件路径与凭据留在本机；对端新增的账号会以 `source=""` 建立在清单里，
//! 需要各自登录一次才能读数。这不是遗漏，是跨机凭据无法通用。

use crate::app::App;
use crate::model;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 允许跨机覆盖的 `settings.txt` 键。设备相关键一律留在本机。
const SYNC_SETTING_KEYS: &[&str] = &[
    // 喝水 / 久坐 / 专注
    "water_minutes", "serving_ml", "water_start", "water_end",
    "sit_remind", "sit_minutes", "lunch_rest", "lunch_start", "lunch_end",
    "focus_minutes", "rest_minutes",
    // 天气（只同步城市与收藏，缓存各机自拉）
    "weather_city", "weather_position", "weather_saved",
    // 通用偏好
    "clock_12h", "clock_period", "show_battery", "pet", "minimal",
    "skin", "island_shape", "settings_theme", "visible_pages", "page_order",
    "resident_order", "notifications", "sound", "lyrics_enabled", "system_hud",
];

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct SyncPayload {
    /// 白名单键值（合并进本机 settings.txt，不整表覆盖）
    pub settings: Vec<(String, String)>,
    pub todo: Option<String>,
    pub water: Option<String>,
    pub water_extra: Option<String>,
    pub shelf: Option<String>,
    /// 账号清单元数据 JSON（不含本机登录文件路径与凭据）
    pub accounts: Option<String>,
}

fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// 与 `model::read_kv` 同格式（key=value，跳过 BOM），只取白名单键。
fn read_settings(path: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new(); };
    let mut out = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let Some((k, v)) = line.split_once('=') else { continue; };
        let (k, v) = (k.trim(), v.trim());
        if SYNC_SETTING_KEYS.contains(&k) {
            out.push((k.to_owned(), v.to_owned()));
        }
    }
    out
}

/// 抽掉 `source`（本机登录 JSON 绝对路径），只保留账号身份与展示字段。
fn account_metadata(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let accounts = value.get("accounts")?.as_array()?;
    let list: Vec<serde_json::Value> = accounts
        .iter()
        .map(|a| {
            serde_json::json!({
                "id": a.get("id"), "provider": a.get("provider"), "alias": a.get("alias"),
                "slot": a.get("slot"), "enabled": a.get("enabled"), "pinned": a.get("pinned"),
            })
        })
        .collect();
    serde_json::to_string(&serde_json::json!({
        "accounts": list,
        "colors": value.get("colors").cloned().unwrap_or_default(),
    }))
    .ok()
}

/// 账号求并集：对端账号补进本机清单（保留本机同名账号的 `source`），
/// 不删本机独有账号、不覆盖任何凭据文件。
fn merge_accounts(path: &Path, remote: Option<&str>) {
    let Some(remote) = remote else { return; };
    let Ok(rv) = serde_json::from_str::<serde_json::Value>(remote) else { return; };
    let Some(rarr) = rv.get("accounts").and_then(|a| a.as_array()) else { return; };
    let mut local: serde_json::Value = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let existing = local
        .get("accounts")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let mut merged: Vec<serde_json::Value> = Vec::new();
    for r in rarr {
        let Some(id) = r.get("id").and_then(|v| v.as_str()) else { continue; };
        let mut entry = existing
            .iter()
            .find(|l| l.get("id").and_then(|v| v.as_str()) == Some(id))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"id": id, "source": ""}));
        for key in ["provider", "alias", "slot", "enabled", "pinned"] {
            if let Some(value) = r.get(key) {
                entry[key] = value.clone();
            }
        }
        if entry.get("source").is_none() {
            entry["source"] = serde_json::json!("");
        }
        merged.push(entry);
    }
    for l in &existing {
        let id = l.get("id").and_then(|v| v.as_str());
        if id.is_none() || merged.iter().any(|m| m.get("id").and_then(|v| v.as_str()) == id) {
            continue;
        }
        merged.push(l.clone());
    }
    local["accounts"] = serde_json::json!(merged);
    if let Some(colors) = rv.get("colors") {
        if colors.is_object() {
            local["colors"] = colors.clone();
        }
    }
    if let Ok(raw) = serde_json::to_vec_pretty(&local) {
        let _ = model::atomic_write(path, &raw);
    }
}

/// 生成本机快照（不含 rev / device，便于两端比较「数据是否一致」）。
pub fn snapshot() -> String {
    let dir = model::config_dir();
    let payload = SyncPayload {
        settings: read_settings(&dir.join("settings.txt")),
        todo: read_text(&dir.join("todo.txt")),
        water: read_text(&dir.join("water.txt")),
        water_extra: read_text(&dir.join("water-extra.txt")),
        shelf: read_text(&dir.join("shelf.txt")),
        accounts: account_metadata(&dir.join("ai-quota.json")),
    };
    serde_json::to_string(&payload).unwrap_or_default()
}

/// 应用对端快照：先落盘、再刷新内存。`today` 来自 `Env`，保证当日饮水不跨天重置。
pub fn apply(app: &mut App, payload: &str, today: &str, now: f64) -> bool {
    let Ok(doc) = serde_json::from_str::<SyncPayload>(payload) else { return false; };
    if doc.settings.is_empty() && doc.todo.is_none() && doc.water.is_none() {
        return false; // 空快照不覆盖本机数据
    }
    let dir: PathBuf = model::config_dir();
    let _ = std::fs::create_dir_all(&dir);

    // settings.txt：白名单键合并，保留本机 display_id / dock / 路径等键。
    let settings_path = dir.join("settings.txt");
    let mut kv: Vec<(String, String)> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(&settings_path) {
        for line in text.trim_start_matches('\u{feff}').lines() {
            if let Some((k, v)) = line.split_once('=') {
                kv.push((k.trim().to_owned(), v.trim().to_owned()));
            }
        }
    }
    for (k, v) in &doc.settings {
        if !SYNC_SETTING_KEYS.contains(&k.as_str()) {
            continue;
        }
        kv.retain(|(existing, _)| existing != k);
        kv.push((k.clone(), v.clone()));
    }
    let body: String = kv.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    if model::atomic_write(&settings_path, body.as_bytes()).is_err() {
        return false;
    }

    let write = |name: &str, value: Option<&String>| {
        if let Some(text) = value {
            let _ = model::atomic_write(&dir.join(name), text.as_bytes());
        }
    };
    write("todo.txt", doc.todo.as_ref());
    write("water.txt", doc.water.as_ref());
    write("water-extra.txt", doc.water_extra.as_ref());
    write("shelf.txt", doc.shelf.as_ref());
    merge_accounts(&dir.join("ai-quota.json"), doc.accounts.as_deref());

    app.apply_synced_state(today, now);
    true
}

impl App {
    /// 同步落地后刷新内存。单独放这里是因为 `App` 的数据字段是 `pub(crate)`，
    /// binary crate 的 `Shell` 拿不到，只能在库内改。
    pub(crate) fn apply_synced_state(&mut self, today: &str, now: f64) {
        self.settings = model::Settings::load();
        self.todos = model::TodoList::load();
        self.shelf = model::Shelf::load();
        self.water = model::Water::load(today);
        self.quota = crate::quota_ui::State::load();
        // 别让「刚同步完」立刻触发一轮喝水 / 久坐提醒。
        self.last_reminder = now;
        self.sit_start = now;
        self.sit_alerted = 0.0;
        self.sit_t.snap(0.0);
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

static STATUS: OnceLock<Mutex<String>> = OnceLock::new();

/// 设置面板里显示的一行同步状态。
pub fn status_text() -> String {
    STATUS
        .get_or_init(|| Mutex::new("等待连接".into()))
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
}

fn set_status(text: String) {
    if let Ok(mut s) = STATUS.get_or_init(|| Mutex::new(String::new())).lock() {
        *s = text;
    }
}

/// 每秒由 `Shell::update` 调一次。**必须在主线程**：OpenHop `Engine` 是 !Send，
/// 待在 `cross_screen` 的 thread_local 里。
pub struct SyncManager {
    rev: u64,
    updated_at: i64,
    last: String,
    device: String,
    last_peers: usize,
    last_push: Instant,
    clock: Option<Instant>,
}

impl Default for SyncManager {
    fn default() -> Self { Self::new() }
}

impl SyncManager {
    pub fn new() -> Self {
        Self {
            rev: 0,
            updated_at: 0,
            last: snapshot(),
            device: crate::cross_screen::sync_device(),
            last_peers: 0,
            last_push: Instant::now() - Duration::from_secs(60),
            clock: None,
        }
    }

    pub fn tick(&mut self, app: &mut App, today: &str, now: f64) {
        // 首帧先不动作，给引擎一点时间与对端建连；之后严格 1 秒一次。
        let ready = self.clock.is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
        if !ready { return; }
        self.clock = Some(Instant::now());

        let (peers, is_server) = crate::cross_screen::sync_link();
        let connected = peers > 0;

        // ---- 收：LWW 裁决 ----
        let mut applied = 0usize;
        for msg in crate::cross_screen::sync_inbox() {
            if msg.device == self.device { continue; }
            if msg.payload == self.last && msg.rev == self.rev { continue; }
            let accept = msg.rev > self.rev
                || (msg.rev == self.rev && msg.updated_at > self.updated_at)
                || (msg.rev == self.rev && msg.updated_at == self.updated_at && !is_server);
            if !accept { continue; }
            if apply(app, &msg.payload, today, now) {
                self.rev = msg.rev;
                self.updated_at = msg.updated_at;
                // 关键：重算本地指纹，否则会把刚收到的数据原样回推造成无限互推。
                self.last = snapshot();
                applied += 1;
            }
        }

        // ---- 发：本地变更 / 新连接 / 15 秒心跳 ----
        let current = snapshot();
        let changed = current != self.last;
        let joined = connected && self.last_peers == 0;
        let heartbeat = connected && self.last_push.elapsed() >= Duration::from_secs(15);
        if connected && (changed || joined || heartbeat) {
            if changed || joined {
                self.rev = self.rev.saturating_add(1);
                self.updated_at = now_millis();
            }
            if crate::cross_screen::sync_broadcast(self.rev, self.updated_at, current.clone()) {
                self.last = current;
                self.last_push = Instant::now();
            }
        }
        self.last_peers = peers;

        set_status(if !connected {
            "未连接对端 · 本地数据保持独立".into()
        } else if applied > 0 {
            format!("已从对端同步 · rev {} · {}", self.rev, describe(&self.last))
        } else {
            format!("已连接 {} 台 · 数据一致 · rev {} · {}", peers, self.rev, describe(&self.last))
        });
    }
}

/// 给设置面板看的极简摘要，不泄露具体待办内容。
fn describe(payload: &str) -> String {
    let Ok(doc) = serde_json::from_str::<SyncPayload>(payload) else { return "快照异常".into(); };
    let todos = doc
        .todo
        .as_deref()
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    let accounts = doc
        .accounts
        .as_deref()
        .and_then(|a| serde_json::from_str::<serde_json::Value>(a).ok())
        .and_then(|v| v.get("accounts")?.as_array().map(|x| x.len()))
        .unwrap_or(0);
    format!("设置 {} 项 / 待办 {} 条 / 账号 {} 个", doc.settings.len(), todos, accounts)
}
