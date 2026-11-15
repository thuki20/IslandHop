//! IslandHop：两台电脑互联后，把「主设备」的岛仔业务数据同步到对端。
//!
//! 走 OpenHop 已建好的加密 TCP 连接（`Msg::Ext::IslandState` / `Hub::tell`），
//! 不另起端口、不重复实现发现/配对/转发。设计口径：
//!
//! * **同步什么**：`settings.txt` 里与设备无关的白名单键（喝水 / 久坐 / 专注 /
//!   天气城市 / 常用偏好 / 岛上歌词）、`todo.txt`、`water.txt` + `water-extra.txt`、
//!   `shelf.txt`、`ai-quota.json` 的**账号元数据**、邮件账号清单
//!   （`mail-accounts.json`，本身无密码）、日历账号配置（`calendar.json`）、
//!   本机日程事件（`local-calendar.json`，按 uid 求并集），以及**跨机凭据克隆**
//!   （邮箱授权码 / 日历密码 / AI 额度 API Key，经加密通道写入对端本机 keyring）。
//! * **不同步什么**：`display_id` / `dock` / `island_offset` / `avoid_apps` 等
//!   设备相关键；天气缓存（两台出口 IP 不同，各机自拉）；久坐计时与番茄钟剩余
//!   时间（纯本地会话）；剪贴板历史（OpenHop 已有实时剪贴板同步）；以及
//!   DPAPI / 登录文件路径绑定的凭据文件 `credentials/*.bin`（豆包等需要各自重登）。
//! * **冲突口径**：`rev` 大者胜；`rev` 相同比 `updated_at`；仍相同则 **Mac 胜出**
//!   （「所有数据以 mac 为主」——OpenHop 的 Role::Server 是键鼠主控端，与数据
//!   主设备无关）。注意：**账号 / 日历 / 邮件 / 凭据是并集型数据，不参与 LWW
//!   裁决**，任何快照都无条件求并集，否则 Mac 会把 Windows 整包拒收，
//!   邮箱与飞书/钉钉账号永远克隆不过来。收到并应用后立刻重算本地指纹，
//!   避免两台互推回环。
//! * **落盘**：一律「先写盘、再刷新内存」。上游 `App::new()` 与跨天 `tick()` 都会
//!   用磁盘文件覆盖内存，不落盘会在重启/跨天后回退。

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
    // 消息 / 会议提醒开关（企业微信、飞书、钉钉等）
    "read_notifications", "wechat_notifications", "wecom_notifications",
    "feishu_notifications", "dingtalk_notifications", "bluetooth_notifications",
    // 邮箱与日程偏好（账号清单本身走下面的 mail / calendar 通道）
    "mail_enabled", "mail_sync_operations", "mail_code", "mail_interval",
    "calendar_prompt", "calendar_auto_sync",
    // 岛上歌词（桌面歌词）：开关与时间偏移、通知预览
    "island_lyrics", "lyric_delay_ms", "notification_preview",
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
    /// 邮件账号清单（mail-accounts.json 原文；文件里没有密码字段，密码走 `secrets`）
    pub mail: Option<String>,
    /// 日历账号配置（calendar.json 原文；密码走 `secrets`）
    pub calendar: Option<String>,
    /// 本机日程事件（local-calendar.json 原文，按 uid 求并集）
    pub local_calendar: Option<String>,
    /// 跨机克隆的凭据：键带服务前缀（mail:邮箱 / cal:平台:用户名 / apiq:槽位），
    /// 值是明文密码 / 授权码 / API Key。只走 OpenHop 的加密 TCP 通道，
    /// 不落任何明文文件；两端都用本机 keyring 存储。
    pub secrets: Vec<(String, String)>,
    /// 发送端版本。旧客户端（v0.6.72 及以前）没有这个字段，反序列化后为空串，
    /// 设置页会显示「旧版（未上报版本）」——这也是「账号/凭据没同步过来」最常见
    /// 的根因：对端根本不会发送 mail / calendar / secrets 这些字段。
    #[serde(default)]
    pub version: String,
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
        mail: read_text(&dir.join("mail-accounts.json")),
        calendar: read_text(&dir.join("calendar.json")),
        local_calendar: read_text(&dir.join("local-calendar.json")),
        secrets: collect_secrets(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    serde_json::to_string(&payload).unwrap_or_default()
}

/// 凭据缓存：键 = 账号清单指纹，值 = 已收集的凭据。
fn secret_cache() -> &'static Mutex<(String, Vec<(String, String)>)> {
    static CACHE: OnceLock<Mutex<(String, Vec<(String, String)>)>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new((String::new(), Vec::new())))
}

/// 收集跨机可克隆的凭据（邮箱授权码 / 日历密码 / AI 额度 API Key）。
/// keyring 读取有成本（macOS 还可能弹授权），按账号清单指纹缓存，清单不变就复用。
fn collect_secrets() -> Vec<(String, String)> {
    let mail = read_text(&model::config_dir().join("mail-accounts.json")).unwrap_or_default();
    let calendar = read_text(&model::config_dir().join("calendar.json")).unwrap_or_default();
    let quota = crate::quota::Config::load();
    // 指纹只含账号身份（不含内容），清单稳定时不在每次快照都读 keyring。
    let fingerprint = format!("{}|{}|{}",
        mail.split('"').filter(|s|s.contains('@')).collect::<Vec<_>>().join(","),
        // 用日历配置原文而不是长度：新增/删除账号但字节数巧合相同时也要重新收集凭据。
        calendar,
        quota.accounts.iter().filter(|a|a.provider.api_key()).map(|a|a.slot.clone().unwrap_or_default()).collect::<Vec<_>>().join(","));
    if let Ok(cache)=secret_cache().lock() {
        if cache.0==fingerprint { return cache.1.clone(); }
    }
    let mut out=Vec::new();
    // 邮箱授权码：mail::secrets 以邮箱为键。
    for account in crate::mail::load_accounts() {
        if let Ok(password)=crate::mail::secrets::load(&account.email) {
            out.push((format!("mail:{}",account.email.to_ascii_lowercase()),password));
        }
    }
    // 日历密码：以「平台:用户名」为键。
    for cfg in crate::calendar::load_configs() {
        if let Ok(password)=crate::calendar::secrets::load(&cfg) {
            out.push((format!("cal:{}",crate::calendar::credential_key(&cfg)),password));
        }
    }
    // AI 额度 API Key：API-Key 类账号以槽位为键，与 api_quota 同一服务名。
    for account in quota.accounts.iter().filter(|a|a.provider.api_key()) {
        let Some(slot)=account.slot.as_deref() else {continue};
        if slot.is_empty() {continue}
        if let Ok(entry)=keyring::Entry::new("Daozai.ApiQuota",slot) {
            if let Ok(key)=entry.get_password() { out.push((format!("apiq:{slot}"),key)); }
        }
    }
    if let Ok(mut cache)=secret_cache().lock() {
        cache.0=fingerprint;cache.1=out.clone();
    }
    out
}

/// 把对端克隆来的凭据写进本机 keyring。返回成功写入的数量。
fn apply_secrets(secrets:&[(String,String)])->usize {
    let mut saved=0;
    for (key,value) in secrets {
        if value.is_empty() {continue}
        let result=if let Some(email)=key.strip_prefix("mail:") {
            crate::mail::secrets::save(email,value).map(|_|())
        } else if let Some(rest)=key.strip_prefix("cal:") {
            // 找到本机同键位的日历账号再写（save 需要 Config 结构）。
            crate::calendar::load_configs().into_iter()
                .find(|cfg|crate::calendar::credential_key(cfg)==rest)
                .map(|cfg|crate::calendar::secrets::save(&cfg,value).map(|_|()))
                .unwrap_or(Err("本机没有对应日历账号".into()))
        } else if let Some(slot)=key.strip_prefix("apiq:") {
            match keyring::Entry::new("Daozai.ApiQuota",slot) {
                Ok(entry)=>entry.set_password(value).map_err(|e|format!("{e}")),
                Err(e)=>Err(format!("{e}")),
            }
        } else {continue};
        if result.is_ok() {saved+=1;}
    }
    // 凭据变了：下次快照要重新收集。
    if saved>0 { if let Ok(mut cache)=secret_cache().lock() {cache.0.clear();} }
    saved
}

/// 账号类文件的合并指纹，用来判断「并集型数据」这次是否真的有写入。
/// 避免每次心跳都重载内存状态、打断用户当前正在编辑的内容。
fn union_fingerprint(dir: &Path) -> String {
    ["ai-quota.json", "mail-accounts.json", "calendar.json", "local-calendar.json"]
        .iter()
        .map(|name| read_text(&dir.join(name)).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\u{1}")
}

/// 应用对端快照：先落盘、再刷新内存。`today` 来自 `Env`，保证当日饮水不跨天重置。
///
/// `overwrite=true` 表示 LWW 裁决本机落败，覆盖型数据（设置 / 待办 / 饮水 / 便签）
/// 整表接收；`overwrite=false` 时**只做账号类求并集**。
///
/// 账号 / 日历 / 邮件 / 凭据属于「并集型」数据，不受「Mac 为主」的 LWW 裁决影响：
/// 否则两端 rev 相同时 Mac 会把整个 Windows 快照拒收，Windows 上的邮箱、
/// 飞书 / 钉钉日历、AI 额度账号与授权码就永远克隆不过来。
pub fn apply(app: &mut App, payload: &str, today: &str, now: f64, overwrite: bool) -> bool {
    let Ok(doc) = serde_json::from_str::<SyncPayload>(payload) else { return false; };
    let empty = doc.settings.is_empty() && doc.todo.is_none() && doc.water.is_none()
        && doc.water_extra.is_none() && doc.shelf.is_none() && doc.accounts.is_none()
        && doc.mail.is_none() && doc.calendar.is_none() && doc.local_calendar.is_none()
        && doc.secrets.is_empty();
    if empty { return false; } // 空快照不覆盖本机数据
    let dir: PathBuf = model::config_dir();
    let _ = std::fs::create_dir_all(&dir);

    if overwrite {
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
            if !SYNC_SETTING_KEYS.contains(&k.as_str()) { continue; }
            kv.retain(|(existing, _)| existing != k);
            kv.push((k.clone(), v.clone()));
        }
        let body: String = kv.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        if model::atomic_write(&settings_path, body.as_bytes()).is_err() { return false; }

        let write = |name: &str, value: Option<&String>| {
            if let Some(text) = value { let _ = model::atomic_write(&dir.join(name), text.as_bytes()); }
        };
        write("todo.txt", doc.todo.as_ref());
        write("water.txt", doc.water.as_ref());
        write("water-extra.txt", doc.water_extra.as_ref());
        write("shelf.txt", doc.shelf.as_ref());
    }

    // 账号类无条件求并集（幂等：内容没变就不写盘）。
    let before = union_fingerprint(&dir);
    merge_accounts(&dir.join("ai-quota.json"), doc.accounts.as_deref());
    merge_mail(&dir.join("mail-accounts.json"), doc.mail.as_deref());
    merge_calendar(&dir.join("calendar.json"), doc.calendar.as_deref());
    merge_local_calendar(&dir.join("local-calendar.json"), doc.local_calendar.as_deref());
    let secrets = apply_secrets(&doc.secrets);
    let union_changed = secrets > 0 || union_fingerprint(&dir) != before;
    if secrets > 0 { set_status(format!("已从对端克隆 {secrets} 个账号凭据")); }

    // 只在确实有变化时刷新内存，避免 15 秒心跳反复重载打断编辑。
    if overwrite || union_changed { app.apply_synced_state(today, now); }
    overwrite || union_changed
}

/// 邮件账号求并集（按 email 去重）：对端账号补进本机清单，本机同名账号保留。
/// 清单本身不含密码（密码在 keyring，走 `secrets` 通道）。
fn merge_mail(path: &Path, remote: Option<&str>) {
    let Some(remote) = remote else { return; };
    let Ok(rv) = serde_json::from_str::<serde_json::Value>(remote) else { return; };
    let Some(rarr) = rv.as_array() else { return; };
    let key = |a: &serde_json::Value| a.get("email").and_then(|v| v.as_str()).map(str::to_ascii_lowercase);
    let local: Vec<serde_json::Value> = std::fs::read(path).ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!([]))
        .as_array().cloned().unwrap_or_default();
    let mut merged = local.clone();
    for r in rarr {
        let Some(rkey) = key(r) else { continue };
        if merged.iter().any(|l| key(l).as_deref() == Some(rkey.as_str())) { continue; }
        merged.push(r.clone());
    }
    if merged.len() != local.len() {
        if let Ok(raw) = serde_json::to_vec_pretty(&merged) {
            let _ = model::atomic_write(path, &raw);
        }
    }
}

/// 日历账号求并集（按「平台:用户名」去重）：单账号对象与多账号数组两种格式都兼容。
fn merge_calendar(path: &Path, remote: Option<&str>) {
    let Some(remote) = remote else { return; };
    let Ok(rv) = serde_json::from_str::<serde_json::Value>(remote) else { return; };
    let key = |a: &serde_json::Value| format!("{}:{}", 
        a.get("provider").and_then(|v|v.as_str()).unwrap_or_default(),
        a.get("username").and_then(|v|v.as_str()).unwrap_or_default().to_ascii_lowercase());
    let accounts_of = |v: &serde_json::Value| -> Vec<serde_json::Value> {
        if let Some(list)=v.get("accounts").and_then(|a|a.as_array()) { list.clone() }
        else if v.get("username").is_some() { vec![v.clone()] } else { vec![] }
    };
    let remote_accounts = accounts_of(&rv);
    if remote_accounts.is_empty() { return; }
    let local: serde_json::Value = std::fs::read(path).ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let mut local_accounts = accounts_of(&local);
    let mut merged = local_accounts.clone();
    for r in &remote_accounts {
        let rkey = key(r);
        if rkey.is_empty() || merged.iter().any(|l|key(l)==rkey) { continue; }
        merged.push(r.clone());
    }
    if merged.len() == local_accounts.len() { return; }
    let mut doc=local.clone();
    doc["accounts"]=serde_json::json!(merged);
    // 活动账号指向新补进的账号，让对端账号在克隆后立即生效。
    if let Some(active)=merged.last() {
        if let Some(k)=active.get("provider").and_then(|v|v.as_str()) { doc["active"]=serde_json::json!(k); }
    }
    if let Ok(raw)=serde_json::to_vec_pretty(&doc) { let _ = model::atomic_write(path, &raw); }
}

/// 本机日程事件按 uid 求并集：两台机器都能添加，互补不覆盖。
fn merge_local_calendar(path: &Path, remote: Option<&str>) {
    let Some(remote) = remote else { return; };
    let Ok(rv) = serde_json::from_str::<serde_json::Value>(remote) else { return; };
    let Some(rarr) = rv.as_array() else { return; };
    let uid = |a: &serde_json::Value| a.get("uid").and_then(|v|v.as_str()).map(str::to_owned);
    let mut local: Vec<serde_json::Value> = std::fs::read(path).ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!([]))
        .as_array().cloned().unwrap_or_default();
    let mut added = 0;
    for r in rarr {
        let Some(ruid) = uid(r) else { continue };
        if local.iter().any(|l|uid(l).as_deref()==Some(ruid.as_str())) { continue; }
        local.push(r.clone());added+=1;
    }
    if added>0 {
        if let Ok(raw)=serde_json::to_vec_pretty(&local) { let _ = model::atomic_write(path, &raw); }
    }
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
        // 邮件账号清单与日历/本机日程从磁盘重载（对端克隆来的账号立即生效）。
        // 日历连接状态随 feeds 重建，由周期轮询重新握手。
        self.mail.accounts = crate::mail::load_accounts();
        self.mail_accounts_changed();
        let calendar_form = std::mem::take(&mut self.calendar.form);
        let calendar_note = std::mem::take(&mut self.calendar.note);
        self.calendar = crate::app::CalendarState::load();
        self.calendar.form = calendar_form;
        self.calendar.note = calendar_note;
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
    /// 对端客户端版本（诊断用）：空 = 还没收到对端数据；「旧版」= 对端没上报版本。
    peer_version: String,
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
            peer_version: String::new(),
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

        let (peers, _is_server) = crate::cross_screen::sync_link();
        let connected = peers > 0;

        // ---- 收：LWW 裁决 ----
        // 冲突口径：rev 大者胜；rev 相同比 updated_at；完全相同则 **Mac 胜出**
        // （用户拍板「所有数据以 mac 为主」；OpenHop 的 Role::Server 是键鼠
        // 主控端=Windows，与数据主设备无关，不能沿用）。
        let mut applied = 0usize;
        for msg in crate::cross_screen::sync_inbox() {
            if msg.device == self.device { continue; }
            // 版本诊断：旧客户端没有 version 字段（serde default 为空串），
            // 设置页会提示「旧版（未上报版本）」——这是账号没同步过来的头号根因。
            if let Ok(doc) = serde_json::from_str::<SyncPayload>(&msg.payload) {
                self.peer_version = if doc.version.is_empty() { "旧版（未上报版本）".into() } else { doc.version };
            }
            if msg.payload == self.last && msg.rev == self.rev { continue; }
            let accept = msg.rev > self.rev
                || (msg.rev == self.rev && msg.updated_at > self.updated_at)
                || (msg.rev == self.rev && msg.updated_at == self.updated_at && !cfg!(target_os = "macos"));
            // accept=false 也要跑一遍：账号/日历/邮件/凭据无条件求并集，
            // 否则「Mac 为主」会把 Windows 的整包拒掉，账号永远克隆不过来。
            if apply(app, &msg.payload, today, now, accept) {
                if accept {
                    self.rev = msg.rev;
                    self.updated_at = msg.updated_at;
                }
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

        let peer = if self.peer_version.is_empty() { "等待对端数据".to_string() }
            else { format!("对端 {}", self.peer_version) };
        set_status(if !connected {
            "未连接对端 · 本地数据保持独立".into()
        } else if applied > 0 {
            format!("已从对端同步 · rev {} · {} · {}", self.rev, describe(&self.last), peer)
        } else {
            format!("已连接 {} 台 · 数据一致 · rev {} · {} · {}", peers, self.rev, describe(&self.last), peer)
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

#[cfg(test)]
mod merge_tests {
    use super::*;

    fn tmpdir(name:&str)->PathBuf {
        let dir=std::env::temp_dir().join(format!("daozai-sync-{}-{}",name,std::process::id()));
        let _=std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn mail_merge_unions_accounts_by_email_and_keeps_local(){
        let dir=tmpdir("mail");
        let path=dir.join("mail-accounts.json");
        std::fs::write(&path,br#"[{"email":"Me@QQ.com","provider":"qq","enabled":true}]"#).unwrap();
        merge_mail(&path,Some(r#"[{"email":"me@qq.com","provider":"qq","enabled":false},{"email":"work@163.com","provider":"163","enabled":true}]"#));
        let merged:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let list=merged.as_array().unwrap();
        assert_eq!(list.len(),2,"同名邮箱（大小写不敏感）保留本机，新增对端独有账号");
        assert_eq!(list[0]["enabled"],serde_json::json!(true),"本机同名账号的开关不能被对端覆盖");
        assert_eq!(list[1]["email"],serde_json::json!("work@163.com"));
        let _=std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calendar_merge_unions_accounts_and_activates_the_newest(){
        let dir=tmpdir("cal");
        let path=dir.join("calendar.json");
        std::fs::write(&path,br#"{"accounts":[{"provider":"feishu","username":"a"}],"active":"feishu"}"#).unwrap();
        merge_calendar(&path,Some(r#"{"accounts":[{"provider":"apple","username":"b"}],"active":"apple"}"#));
        let merged:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(merged["accounts"].as_array().unwrap().len(),2);
        assert_eq!(merged["active"],serde_json::json!("apple"),"克隆后活动账号指向新补进的账号");
        // 再推一次相同内容：幂等，不产生重复。
        merge_calendar(&path,Some(r#"{"accounts":[{"provider":"apple","username":"b"}],"active":"apple"}"#));
        let again:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(again["accounts"].as_array().unwrap().len(),2);
        let _=std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_calendar_merge_unions_by_uid(){
        let dir=tmpdir("local");
        let path=dir.join("local-calendar.json");
        std::fs::write(&path,br#"[{"uid":"u1","title":"a","start":100,"end":200,"all_day":false,"reminder":15}]"#).unwrap();
        merge_local_calendar(&path,Some(r#"[{"uid":"u1","title":"a","start":100,"end":200,"all_day":false,"reminder":15},{"uid":"u2","title":"b","start":300,"end":400,"all_day":false,"reminder":null}]"#));
        let merged:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(merged.as_array().unwrap().len(),2,"按 uid 求并集，两端添加的事件互补");
        let _=std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_setting_keys_cover_island_lyrics(){
        for key in ["island_lyrics","lyric_delay_ms","notification_preview"] {
            assert!(SYNC_SETTING_KEYS.contains(&key),"桌面歌词相关键 {key} 必须在同步白名单里");
        }
    }

    #[test]
    fn sync_setting_keys_cover_mail_calendar_and_messengers(){
        for key in ["wecom_notifications","feishu_notifications","dingtalk_notifications",
                    "read_notifications","wechat_notifications","bluetooth_notifications",
                    "mail_enabled","mail_sync_operations","mail_code","mail_interval",
                    "calendar_prompt","calendar_auto_sync"] {
            assert!(SYNC_SETTING_KEYS.contains(&key),"{key} 必须在同步白名单里");
        }
    }

    #[test]
    fn legacy_payload_without_version_still_deserializes(){
        // Windows 端若仍是旧版，payload 没有 version / mail / calendar / secrets；
        // 新版必须能解析并在设置页提示「旧版（未上报版本）」。
        let doc:SyncPayload=serde_json::from_str(r#"{"settings":[],"secrets":[]}"#).unwrap();
        assert_eq!(doc.version,"");
        assert!(doc.mail.is_none()&&doc.calendar.is_none()&&doc.secrets.is_empty());
    }

    #[test]
    fn union_fingerprint_detects_cloned_accounts(){
        let dir=tmpdir("fp");
        let before=union_fingerprint(&dir);
        merge_mail(&dir.join("mail-accounts.json"),Some(r#"[{"email":"a@b.com","provider":"qq","enabled":true}]"#));
        assert_ne!(union_fingerprint(&dir),before,"账号新增后指纹必须变化，才会刷新内存");
        let _=std::fs::remove_dir_all(&dir);
    }
}
