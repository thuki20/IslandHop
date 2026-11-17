//! IslandHop：两台电脑互联后，把「主端」的岛仔业务数据**单向镜像**到从端。
//!
//! 走 OpenHop 已建好的加密 TCP 连接（`Msg::Ext::IslandState` / `Hub::tell`），
//! 不另起端口、不重复实现发现/配对/转发。设计口径：
//!
//! * **主从角色**：macOS 是唯一数据主端（`is_data_master()`），Windows 是从端。
//!   主端只发不收；从端只收不发、对主端快照**整表替换**。这样：
//!   - 主端的删除/编辑立即传播到从端（整表替换语义，删除自然传播，无需墓碑）；
//!   - 从端的任何本地变动**永远不会写回主端**——彻底杜绝
//!     「从端删掉的东西被主端并集又同步回来」的回灌问题。
//! * **同步什么**：`settings.txt` 里与设备无关的白名单键（喝水 / 久坐 / 专注 /
//!   天气城市 / 常用偏好 / 岛上歌词）、`todo.txt`、`water.txt` + `water-extra.txt`、
//!   `shelf.txt`、`ai-quota.json` 的**账号元数据**（镜像时按 id 回填本机
//!   `source` 登录路径与 `slot` 凭据槽位）、邮件账号清单（`mail-accounts.json`，
//!   本身无密码）、日历账号配置（`calendar.json`）、本机日程事件
//!   （`local-calendar.json`）——**只同步账号"信息"（清单/别名/平台），不再克隆
//!   任何账号密码 / 授权码**（2026-11-16 用户拍板：从端不登录账号、不获取信息，
//!   凭据一律留在各自本机 keyring，永不推送、永不写入从端）。
//! * **不同步什么**：账号密码 / 邮箱授权码 / 日历密码 / AI 额度 API Key（凭据）；
//!   `display_id` / `dock` / `island_offset` / `avoid_apps` 等
//!   设备相关键；天气缓存（两台出口 IP 不同，各机自拉）；久坐计时与番茄钟剩余
//!   时间（纯本地会话）；剪贴板历史（OpenHop 已有实时剪贴板同步）；以及
//!   DPAPI / 登录文件路径绑定的凭据文件 `credentials/*.bin`（豆包等需要各自重登）。
//! * **从端可删改**：账号 / 邮箱 / 日历的删除入口在从端可用（`request_deletion`
//!   本地立即生效并把删除键转发主端执行，队列落盘持久化，重启/离线都不丢）；
//!   信息本身整表镜像自主跨屏；邮件收取等本机读操作照常执行。
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
    /// 从端→主端的**删除请求**（键格式 `acct:<id>` / `mail:<邮箱小写>` / `cal:<平台:用户名>`）。
    /// 主端收到后在本机执行删除，再随下一次快照镜像回所有从端——
    /// 删除从此在两台机器上都生效且永不复活。旧端无此字段（serde default 空表）。
    #[serde(default)]
    pub deletions: Vec<String>,
}

/// 从端删除请求队列：(待发主端, 待确认)。从端 UI 删除账号时调用 `request_deletion`，
/// `SyncManager::tick` 把待发键注入下一帧控制帧；主端删除完成、快照里不再出现
/// 该条目后，待确认键才清除——期间镜像会跳过这些条目，杜绝「删了又复活」。
///
/// **持久化**（2026-11-16）：此前队列纯内存，从端删完若在主端确认前重启（或离线
/// 删除后一直没连上），pending/outbox 全丢，下次主端心跳又把账号镜像回来——
/// 即用户反馈的「删了立马同步回来」。现在每次变更都写 `deletions.json`，启动时
/// 加载，删除请求跨重启、跨离线持久保留，直到主端确认执行。
fn deletions_path() -> PathBuf { model::config_dir().join("deletions.json") }

fn load_deletions(path: &Path) -> (Vec<String>, Vec<String>) {
    let Ok(raw) = std::fs::read(path) else { return (Vec::new(), Vec::new()); };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else { return (Vec::new(), Vec::new()); };
    let list = |key: &str| -> Vec<String> {
        v.get(key).and_then(|a| a.as_array()).map(|arr| arr.iter()
            .filter_map(|x| x.as_str().map(str::to_owned)).collect()).unwrap_or_default()
    };
    (list("outbox"), list("pending"))
}

fn save_deletions(path: &Path, outbox: &[String], pending: &[String]) {
    let body = serde_json::json!({"outbox": outbox, "pending": pending});
    if let Ok(raw) = serde_json::to_vec_pretty(&body) {
        let _ = model::atomic_write(path, &raw);
    }
}

fn deletion_queue() -> &'static Mutex<(Vec<String>, Vec<String>)> {
    static QUEUE: OnceLock<Mutex<(Vec<String>, Vec<String>)>> = OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(load_deletions(&deletions_path())))
}

/// 从端请求删除一个账号：本地立即生效（UI 已改），删除键转发主端执行。
pub fn request_deletion(key: String) {
    let Ok(mut queue) = deletion_queue().lock() else { return; };
    if !queue.0.contains(&key) { queue.0.push(key.clone()); }
    if !queue.1.contains(&key) { queue.1.push(key); }
    save_deletions(&deletions_path(), &queue.0, &queue.1);
}

/// 仍待主端确认的删除键（镜像时跳过这些条目，不复活）。
fn pending_deletions() -> Vec<String> {
    deletion_queue().lock().map(|q| q.1.clone()).unwrap_or_default()
}

/// 取走待发的删除键。
fn drain_deletion_outbox() -> Vec<String> {
    let Ok(mut queue) = deletion_queue().lock() else { return Vec::new(); };
    let out = std::mem::take(&mut queue.0);
    save_deletions(&deletions_path(), &queue.0, &queue.1);
    out
}

/// 主端已执行删除（快照里不再出现）→ 从待确认列表移除，镜像恢复正常跟随。
fn confirm_deletions(confirmed: &[String]) {
    let Ok(mut queue) = deletion_queue().lock() else { return; };
    let before_len = queue.1.len();
    queue.1.retain(|k| !confirmed.contains(k));
    if queue.1.len() != before_len { save_deletions(&deletions_path(), &queue.0, &queue.1); }
}

/// 主端快照里是否还有这个键（用于确认从端删除请求已被主端执行）。
fn master_still_has(doc: &SyncPayload, key: &str) -> bool {
    if let Some(id) = key.strip_prefix("acct:") {
        let Some(accounts) = doc.accounts.as_deref().and_then(|a| serde_json::from_str::<serde_json::Value>(a).ok()) else { return false; };
        return accounts.get("accounts").and_then(|a| a.as_array()).is_some_and(|list| {
            list.iter().any(|a| a.get("id").and_then(|v| v.as_str()) == Some(id))
        });
    }
    if let Some(email) = key.strip_prefix("mail:") {
        let Some(list) = doc.mail.as_deref().and_then(|a| serde_json::from_str::<serde_json::Value>(a).ok()) else { return false; };
        return list.as_array().is_some_and(|arr| arr.iter().any(|a| {
            a.get("email").and_then(|v| v.as_str()).map(|e| e.to_ascii_lowercase())
                == Some(email.to_ascii_lowercase())
        }));
    }
    if let Some(ckey) = key.strip_prefix("cal:") {
        let Some(cfg) = doc.calendar.as_deref().and_then(|a| serde_json::from_str::<serde_json::Value>(a).ok()) else { return false; };
        return cfg.get("accounts").and_then(|a| a.as_array()).is_some_and(|arr| arr.iter().any(|a| {
            // credential_key 的口径 = 「平台:用户名小写」，外层已有 cal: 前缀。
            format!("{}:{}",
                a.get("provider").and_then(|v| v.as_str()).unwrap_or_default(),
                a.get("username").and_then(|v| v.as_str()).unwrap_or_default().to_ascii_lowercase()) == ckey
        }));
    }
    false
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

/// 主端角色判定：macOS 是唯一数据主端，Windows 是从端。
/// OpenHop 的 `Role::Server` 是键鼠主控端（=Windows），与数据主设备无关，不能沿用。
pub fn is_data_master() -> bool {
    cfg!(target_os = "macos")
}

/// 从端判定：账号/邮箱/日历信息整表镜像自数据主端。删除入口在从端可用
/// （转主端执行），不再一概禁用。
pub fn is_slave() -> bool {
    !is_data_master()
}

/// 账号清单镜像：以主端清单为准**整表替换**（主端删除的账号随之消失），
/// 但按 id 回填本机 `source`（登录文件路径）与既有 `slot`（凭据槽位），
/// 保证本机已登录的账号不因镜像掉登录。`pending` 里是从端已删、主端尚未确认的
/// `acct:<id>` 键——镜像时跳过，防止主端快照把刚删的账号送回来。返回是否写盘。
fn mirror_accounts(path: &Path, remote: Option<&str>, pending: &[String]) -> bool {
    let Some(remote) = remote else { return false; };
    let Ok(rv) = serde_json::from_str::<serde_json::Value>(remote) else { return false; };
    let Some(rarr) = rv.get("accounts").and_then(|a| a.as_array()) else { return false; };
    let mut local: serde_json::Value = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let existing = local
        .get("accounts")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let mut mirrored: Vec<serde_json::Value> = Vec::new();
    for r in rarr {
        let Some(id) = r.get("id").and_then(|v| v.as_str()) else { continue; };
        if pending.iter().any(|k| k == &format!("acct:{id}")) { continue; }
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
        mirrored.push(entry);
    }
    if mirrored == existing { return false; }
    local["accounts"] = serde_json::json!(mirrored);
    if let Some(colors) = rv.get("colors") {
        if colors.is_object() {
            local["colors"] = colors.clone();
        }
    }
    if let Ok(raw) = serde_json::to_vec_pretty(&local) {
        return model::atomic_write(path, &raw).is_ok();
    }
    false
}

/// 邮件账号清单镜像：主端数组原文整表替换（含删空——「[]」是合法的镜像结果）。
/// `pending` 里是从端已删、主端尚未确认的 `mail:<邮箱小写>` 键，镜像时跳过。
fn mirror_mail(path: &Path, remote: Option<&str>, pending: &[String]) -> bool {
    let Some(remote) = remote else { return false; };
    let Ok(rv) = serde_json::from_str::<serde_json::Value>(remote) else { return false; };
    let Some(arr) = rv.as_array() else { return false; };
    let kept: Vec<&serde_json::Value> = arr.iter().filter(|a| {
        let email = a.get("email").and_then(|v| v.as_str()).unwrap_or_default().to_ascii_lowercase();
        !pending.iter().any(|k| k == &format!("mail:{email}"))
    }).collect();
    let local = std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).unwrap_or(serde_json::json!([]));
    if local.as_array().is_some_and(|l| l.len() == kept.len()
        && l.iter().zip(kept.iter()).all(|(a, b)| a == *b)) { return false; }
    let payload: Vec<serde_json::Value> = kept.into_iter().cloned().collect();
    if let Ok(raw) = serde_json::to_vec_pretty(&serde_json::json!(payload)) {
        return model::atomic_write(path, &raw).is_ok();
    }
    false
}

/// 日历账号配置镜像：主端文档原文整表替换（含 `active` 指向）。
/// `pending` 里是从端已删、主端尚未确认的 `cal:<平台:用户名>` 键，镜像时跳过。
fn mirror_calendar(path: &Path, remote: Option<&str>, pending: &[String]) -> bool {
    let Some(remote) = remote else { return false; };
    let Ok(rv) = serde_json::from_str::<serde_json::Value>(remote) else { return false; };
    if !rv.is_object() { return false; }
    let mut doc = rv.clone();
    if let Some(accounts) = doc.get_mut("accounts").and_then(|a| a.as_array_mut()) {
        accounts.retain(|a| {
            let key = format!("cal:{}:{}",
                a.get("provider").and_then(|v|v.as_str()).unwrap_or_default(),
                a.get("username").and_then(|v|v.as_str()).unwrap_or_default().to_ascii_lowercase());
            !pending.iter().any(|k| k == &key)
        });
    }
    // active 指向被 pending 屏蔽掉的账号时，沿用本机 active（若它仍在剩余清单里），
    // 否则顺延到剩余第一个——等主端确认删除后的快照再最终对齐。
    if let Some(accounts) = doc.get("accounts").and_then(|a| a.as_array()) {
        let active = doc.get("active").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let providers: Vec<String> = accounts.iter()
            .filter_map(|a| a.get("provider").and_then(|v| v.as_str()).map(str::to_string)).collect();
        if !providers.is_empty() && !providers.iter().any(|p| p == &active) {
            let local_active = local_active_provider(path);
            if providers.iter().any(|p| p == &local_active) {
                doc["active"] = serde_json::json!(local_active);
            } else {
                doc["active"] = serde_json::json!(providers[0]);
            }
        }
    }
    let local = std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).unwrap_or(serde_json::json!({}));
    if local == doc { return false; }
    if let Ok(raw) = serde_json::to_vec_pretty(&doc) {
        return model::atomic_write(path, &raw).is_ok();
    }
    false
}

/// 本机日程事件镜像：主端数组原文整表替换。
fn mirror_local_calendar(path: &Path, remote: Option<&str>) -> bool {
    let Some(remote) = remote else { return false; };
    let Ok(rv) = serde_json::from_str::<serde_json::Value>(remote) else { return false; };
    let Some(arr) = rv.as_array() else { return false; };
    let local = std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).unwrap_or(serde_json::json!([]));
    if local.as_array() == Some(arr) { return false; }
    if let Ok(raw) = serde_json::to_vec_pretty(&serde_json::json!(arr)) {
        return model::atomic_write(path, &raw).is_ok();
    }
    false
}

/// 本机 calendar.json 当前的活动平台（无配置时为空串）。
fn local_active_provider(path: &Path) -> String {
    std::fs::read(path).ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("active").and_then(|a| a.as_str()).map(str::to_string))
        .unwrap_or_default()
}

/// 从端凭据剪除：镜像后，主端已删除的账号凭据在本机 keyring 里也删掉。
/// 只删「镜像前本机存在、主端快照里已不存在」的键，避免误删本机独有凭据。
fn prune_slave_secrets(
    pre_mail: &[crate::mail::Account],
    pre_calendar: &[crate::calendar::Config],
    pre_apiq_slots: &[String],
    master_keys: &[String],
) {
    let has = |key: &str| master_keys.iter().any(|k| k == key);
    // 邮箱授权码
    for account in pre_mail {
        let key = format!("mail:{}", account.email.to_ascii_lowercase());
        if !has(&key) { crate::mail::secrets::delete(&account.email); }
    }
    // 日历密码
    for cfg in pre_calendar {
        let key = format!("cal:{}", crate::calendar::credential_key(cfg));
        if !has(&key) { crate::calendar::secrets::delete(cfg); }
    }
    // AI 额度 API Key（走既有清理 journal，安全异步删除）
    for slot in pre_apiq_slots {
        if slot.is_empty() { continue; }
        let key = format!("apiq:{slot}");
        if !has(&key) { let _ = crate::quota::api::queue_cleanup(slot); }
    }
}

/// 主端执行从端转发的删除请求（主端唯一接受的「收」）：键格式
/// `acct:<id>` / `mail:<邮箱>` / `cal:<平台:用户名>`。删除发生在主端本机，
/// 之后的快照广播会把删除镜像到所有从端。返回是否有任何变更。
fn apply_master_deletions(deletions: &[String]) -> bool {
    use crate::quota::Provider;
    let mut changed = false;
    for key in deletions {
        if let Some(id) = key.strip_prefix("acct:") {
            let mut cfg = crate::quota::Config::load();
            let Some(account) = cfg.accounts.iter().find(|a| a.id == id).cloned() else { continue; };
            // 与 quota_ui::quota_remove_account 相同的清理路径：API-Key 先排队删凭据，
            // 登录态类（豆包/Grok/Gemini 等）走各自的 forget。
            if account.provider.api_key() {
                if let Some(slot) = &account.slot { let _ = crate::quota::api::queue_cleanup(slot); }
            }
            cfg.accounts.retain(|a| a.id != id);
            cfg.colors.remove(id);
            if cfg.selected == id {
                cfg.selected = cfg.accounts.iter().find(|a| a.enabled).map(|a| a.id.clone()).unwrap_or_default();
            }
            if cfg.save_checked().is_err() { continue; }
            match account.provider {
                Provider::Doubao => { let _ = crate::quota::doubao::forget(&account); },
                Provider::GrokBot => { let _ = crate::quota::grok::forget(&account); },
                Provider::Gemini => { let _ = crate::quota::gemini::forget(&account); },
                Provider::DeepSeek => { let _ = crate::quota::deepseek::forget(&account); },
                Provider::WorkBuddy => { let _ = crate::quota::workbuddy::forget(&account); },
                _ => {},
            }
            changed = true;
        } else if let Some(email) = key.strip_prefix("mail:") {
            let mut accounts = crate::mail::load_accounts();
            let before = accounts.len();
            accounts.retain(|a| !a.email.eq_ignore_ascii_case(email));
            if accounts.len() != before {
                crate::mail::save_accounts(&accounts);
                crate::mail::secrets::delete(email);
                changed = true;
            }
        } else if let Some(ckey) = key.strip_prefix("cal:") {
            let mut accounts = crate::calendar::load_configs();
            let Some(position) = accounts.iter().position(|cfg| crate::calendar::credential_key(cfg) == ckey) else { continue; };
            let removed = accounts.remove(position);
            // 活动账号顺延到剩余第一个；删空时保持被删账号的平台占位。
            let active = accounts.first().map(|cfg| cfg.provider).unwrap_or(removed.provider);
            if crate::calendar::save_configs(&accounts, active).is_ok() {
                crate::calendar::secrets::delete(&removed);
                changed = true;
            }
        }
    }
    changed
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
        // 账号密码/授权码不再跨机同步（2026-11-16 用户拍板）：Windows 从端
        // 不登录账号、不获取信息，只镜像账号"信息"供查看/删改；凭据留在各自
        // 本机 keyring，永不推送、永不克隆。故快照恒不含 secrets。
        secrets: Vec::new(),
        version: env!("CARGO_PKG_VERSION").into(),
        // 快照不带删除请求：删除键只出现在从端的控制帧里。
        deletions: Vec::new(),
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
/// **已停用**（2026-11-16）：账号密码不再跨机同步，snapshot 恒发空 secrets；
/// 保留实现仅供对照，勿再接入同步链路。
#[allow(dead_code)]
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
    // AI 额度 API Key：Windows 凭据管理器才有；macOS 端 api_quota 本就不支持
    // 安全保存 API Key，且凭据同步已停用——这里绝不触碰 keyring（会触发授权弹窗）。
    #[cfg(windows)]
    for account in quota.accounts.iter().filter(|a|a.provider.api_key()) {
        let Some(slot)=account.slot.as_deref() else {continue};
        if slot.is_empty() {continue}
        if let Ok(entry)=keyring::Entry::new("Daozai.ApiQuota",slot) {
            if let Ok(key)=entry.get_password() { out.push((format!("apiq:{slot}"),key)); }
        }
    }
    #[cfg(not(windows))] let _=&quota;
    if let Ok(mut cache)=secret_cache().lock() {
        cache.0=fingerprint;cache.1=out.clone();
    }
    out
}

/// 把对端克隆来的凭据写进本机 keyring。返回成功写入的数量。
/// **已停用**（2026-11-16）：账号密码不再跨机同步；保留实现仅供对照。
#[allow(dead_code)]
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
            // 凭据同步已停用；macOS 端 api_quota 不支持 API Key 安全保存，
            // 绝不直接调用 keyring（会触发系统授权弹窗）。
            #[cfg(windows)] {
                match keyring::Entry::new("Daozai.ApiQuota",slot) {
                    Ok(entry)=>entry.set_password(value).map_err(|e|format!("{e}")),
                    Err(e)=>Err(format!("{e}")),
                }
            }
            #[cfg(not(windows))] { let _=slot; Err("当前系统不支持保存 API Key".into()) }
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

/// 从端应用主端快照：**整表镜像**，先落盘、再刷新内存。`today` 来自 `Env`，
/// 保证当日饮水不跨天重置。主端的删除/编辑随整表替换自然传播；
/// 凭据随之克隆与剪除（主端删掉的凭据本机也清掉）。
pub fn apply_mirror(app: &mut App, payload: &str, today: &str, now: f64) -> bool {
    let Ok(doc) = serde_json::from_str::<SyncPayload>(payload) else { return false; };
    let empty = doc.settings.is_empty() && doc.todo.is_none() && doc.water.is_none()
        && doc.water_extra.is_none() && doc.shelf.is_none() && doc.accounts.is_none()
        && doc.mail.is_none() && doc.calendar.is_none() && doc.local_calendar.is_none()
        && doc.secrets.is_empty();
    if empty { return false; } // 空快照（旧客户端占位帧）不覆盖本机数据
    let dir: PathBuf = model::config_dir();
    let _ = std::fs::create_dir_all(&dir);

    // 镜像前的本机账号清单：凭据剪除只针对「镜像前存在、主端已删除」的键。
    let pre_mail = crate::mail::load_accounts();
    let pre_calendar = crate::calendar::load_configs();
    let pre_apiq_slots: Vec<String> = crate::quota::Config::load()
        .accounts.iter().filter(|a| a.provider.api_key())
        .filter_map(|a| a.slot.clone()).collect();

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

    // 账号类整表镜像（幂等：内容没变不写盘）。pending = 从端已删、主端尚未
    // 执行的删除键：镜像时跳过这些条目，主端确认执行后（快照不再出现）自动恢复跟随。
    let pending = pending_deletions();
    let before = union_fingerprint(&dir);
    let mirrored = mirror_accounts(&dir.join("ai-quota.json"), doc.accounts.as_deref(), &pending)
        | mirror_mail(&dir.join("mail-accounts.json"), doc.mail.as_deref(), &pending)
        | mirror_calendar(&dir.join("calendar.json"), doc.calendar.as_deref(), &pending)
        | mirror_local_calendar(&dir.join("local-calendar.json"), doc.local_calendar.as_deref());
    // 账号密码/授权码不再跨机克隆（2026-11-16 用户拍板）：从端只接收账号
    // "信息"（清单/别名/平台），凭据一律留在各自本机 keyring。主端快照已不含
    // secrets，这里不再写入；从端历史遗留的克隆凭据用空 master_keys 剪除干净。
    let master_keys: Vec<String> = doc.secrets.iter().map(|(k, _)| k.clone()).collect();
    prune_slave_secrets(&pre_mail, &pre_calendar, &pre_apiq_slots, &master_keys);
    // 主端已执行删除 → 从待确认列表移除。
    let confirmed: Vec<String> = pending.iter().filter(|k| !master_still_has(&doc, k)).cloned().collect();
    confirm_deletions(&confirmed);

    // 只在确实有变化时刷新内存，避免 15 秒心跳反复重载打断编辑。
    let changed = mirrored || union_fingerprint(&dir) != before;
    if changed { app.apply_synced_state(today, now); }
    changed
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
        let master = is_data_master();

        // ---- 收：主端只发不收（唯一例外=从端的删除请求）；从端无条件整表镜像 ----
        // 主端忽略一切业务数据：Windows 从端的数据永远不写回 Mac，
        // 从源头上杜绝「从端删掉的东西被主端并集又同步回来」的回灌。
        let mut applied = 0usize;
        for msg in crate::cross_screen::sync_inbox() {
            if msg.device == self.device { continue; }
            // 版本诊断：旧客户端没有 version 字段（serde default 为空串），
            // 设置页会提示「旧版（未上报版本）」——这是账号没同步过来的头号根因。
            if let Ok(doc) = serde_json::from_str::<SyncPayload>(&msg.payload) {
                self.peer_version = if doc.version.is_empty() { "旧版（未上报版本）".into() } else { doc.version };
            }
            if master {
                // 主端唯一的「收」：从端的删除请求。在本机执行后随下一次快照广播镜像回去。
                if let Ok(doc) = serde_json::from_str::<SyncPayload>(&msg.payload) {
                    if !doc.deletions.is_empty() && apply_master_deletions(&doc.deletions) {
                        // 不更新 self.last：发送分支的指纹比对会发现数据变了，自动广播新快照。
                    }
                }
                continue;
            }
            // 从端：镜像幂等——与上次应用的内容相同就跳过。
            if msg.payload == self.last { continue; }
            if apply_mirror(app, &msg.payload, today, now) {
                self.rev = msg.rev;
                self.updated_at = msg.updated_at;
                // 关键：重算本地指纹，否则会把刚收到的数据当作本地变更再广播造成回环。
                self.last = snapshot();
                applied += 1;
            }
        }

        // ---- 发：主端广播快照（本地变更 / 新连接 / 15 秒心跳）；从端只发删除请求控制帧 ----
        if connected && master {
            let current = snapshot();
            let changed = current != self.last;
            let joined = self.last_peers == 0;
            let heartbeat = self.last_push.elapsed() >= Duration::from_secs(15);
            if changed || joined || heartbeat {
                if changed || joined {
                    self.rev = self.rev.saturating_add(1);
                    self.updated_at = now_millis();
                }
                if crate::cross_screen::sync_broadcast(self.rev, self.updated_at, current.clone()) {
                    self.last = current;
                    self.last_push = Instant::now();
                }
            }
        } else if connected {
            // 从端的反向控制面：只有删除请求键，不带任何业务数据。
            let outbox = drain_deletion_outbox();
            if !outbox.is_empty() {
                let mut frame = snapshot();
                if let Ok(mut doc) = serde_json::from_str::<SyncPayload>(&frame) {
                    doc.deletions = outbox;
                    if let Ok(text) = serde_json::to_string(&doc) { frame = text; }
                }
                self.rev = self.rev.saturating_add(1);
                self.updated_at = now_millis();
                crate::cross_screen::sync_broadcast(self.rev, self.updated_at, frame);
            }
        }
        self.last_peers = peers;

        let peer = if self.peer_version.is_empty() { "等待对端数据".to_string() }
            else { format!("对端 {}", self.peer_version) };
        let role = if master { "主端（数据以本机为准）" } else { "从端（镜像主端数据）" };
        set_status(if !connected {
            format!("未连接对端 · 本地数据保持独立 · {role}")
        } else if applied > 0 {
            format!("{role} · 已应用主端快照 rev {} · {} · {}", self.rev, describe(&self.last), peer)
        } else if master {
            format!("主端 · 已连接 {} 台 · 数据一致 · rev {} · {} · {}", peers, self.rev, describe(&self.last), peer)
        } else {
            format!("从端 · 已连接 {} 台 · 镜像一致 · {} · {}", peers, describe(&self.last), peer)
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
    fn deletion_queue_survives_restart_and_clears_on_confirm(){
        let dir=tmpdir("deletions");
        let path=dir.join("deletions.json");
        // 从端删除两个账号后重启（重新 load）：pending/outbox 都还在，不复活。
        save_deletions(&path,&["acct:a".into(),"mail:b@x.com".into()].to_vec(),
                       &["acct:a".into(),"mail:b@x.com".into()].to_vec());
        let (outbox,pending)=load_deletions(&path);
        assert_eq!(outbox,vec!["acct:a","mail:b@x.com"]);
        assert_eq!(pending,vec!["acct:a","mail:b@x.com"]);
        // 主端确认执行后清空 pending，但 outbox 已 drain（模拟 tick 已转发）。
        save_deletions(&path,&[],&["acct:a".into()].to_vec());
        let (outbox2,pending2)=load_deletions(&path);
        assert!(outbox2.is_empty());
        assert_eq!(pending2,vec!["acct:a"],"未确认的删除跨重启仍屏蔽，不复活");
        // 全部确认。
        save_deletions(&path,&[],&[]);
        assert_eq!(load_deletions(&path),(Vec::new(),Vec::new()));
        let _=std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_never_carries_account_passwords(){
        // 账号密码/授权码永不进快照：从端只镜像账号信息，不克隆凭据。
        let raw=super::snapshot();
        let payload:serde_json::Value=serde_json::from_str(&raw).unwrap();
        assert!(payload.get("secrets").and_then(|s|s.as_array()).is_some_and(|a|a.is_empty()),
                "快照 secrets 必须恒空：{payload}");
    }

    #[test]
    fn mail_mirror_replaces_whole_table_and_propagates_deletion(){
        let dir=tmpdir("mail");
        let path=dir.join("mail-accounts.json");
        std::fs::write(&path,br#"[{"email":"Me@QQ.com","provider":"qq","enabled":true},{"email":"old@163.com","provider":"163","enabled":true}]"#).unwrap();
        // 主端只剩一个账号（old@163.com 已在主端删除）→ 镜像后从端也只剩一个。
        assert!(mirror_mail(&path,Some(r#"[{"email":"me@qq.com","provider":"qq","enabled":false}]"#),&[]));
        let mirrored:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let list=mirrored.as_array().unwrap();
        assert_eq!(list.len(),1,"主端删除的账号随整表替换消失，不再回灌");
        assert_eq!(list[0]["email"],serde_json::json!("me@qq.com"));
        // 再推相同内容：幂等，不写盘。
        assert!(!mirror_mail(&path,Some(r#"[{"email":"me@qq.com","provider":"qq","enabled":false}]"#),&[]));
        // 主端删空：镜像为空数组，这是合法结果。
        assert!(mirror_mail(&path,Some("[]"),&[]));
        let emptied:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(emptied.as_array().unwrap().len(),0,"主端删空的清单必须传播到从端");
        let _=std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mail_mirror_skips_pending_deletions_until_master_confirms(){
        let dir=tmpdir("mailp");
        let path=dir.join("mail-accounts.json");
        // 从端已删 keep@p.com（本地文件里已没有），pending 里还挂着删除键；
        // 主端快照仍带着该账号（还没执行删除）→ 镜像不能把它送回来（返回值不重要）。
        std::fs::write(&path,br#"[{"email":"other@p.com","provider":"qq","enabled":true}]"#).unwrap();
        let _=mirror_mail(&path,Some(r#"[{"email":"keep@p.com","provider":"qq","enabled":true},{"email":"other@p.com","provider":"qq","enabled":true}]"#),&["mail:keep@p.com".to_string()]);
        let mirrored:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(mirrored.as_array().unwrap().len(),1,"pending 删除键在主端确认前必须屏蔽对应条目");
        assert_eq!(mirrored.as_array().unwrap()[0]["email"],serde_json::json!("other@p.com"));
        let _=std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn master_still_has_checks_all_key_kinds(){
        let doc=SyncPayload{
            accounts:Some(r#"{"accounts":[{"id":"a1"}]}"#.into()),
            mail:Some(r#"[{"email":"A@B.com"}]"#.into()),
            calendar:Some(r#"{"accounts":[{"provider":"feishu","username":"Bob"}],"active":"feishu"}"#.into()),
            ..Default::default()
        };
        assert!(master_still_has(&doc,"acct:a1"));
        assert!(!master_still_has(&doc,"acct:a2"));
        assert!(master_still_has(&doc,"mail:a@b.com"),"邮箱键大小写不敏感");
        assert!(!master_still_has(&doc,"mail:other@b.com"));
        assert!(master_still_has(&doc,"cal:feishu:bob"),"日历键按「平台:用户名小写」匹配");
        assert!(!master_still_has(&doc,"cal:feishu:alice"));
    }

    #[test]
    fn accounts_mirror_keeps_local_source_and_slot(){
        let dir=tmpdir("acct");
        let path=dir.join("ai-quota.json");
        std::fs::write(&path,r#"{"accounts":[{"id":"a1","provider":"api","alias":"本机已登录","source":"C:/login/a1.json","slot":"ab12","enabled":true,"pinned":false}],"colors":{}}"#.as_bytes()).unwrap();
        // 主端改了别名、删掉了另一账号、新增一个：source/slot 按 id 回填，删除传播。
        assert!(mirror_accounts(&path,Some(r#"{"accounts":[{"id":"a1","provider":"api","alias":"主端新别名","slot":"ab12","enabled":true,"pinned":true},{"id":"a2","provider":"api","alias":"新账号","slot":"cd34","enabled":true,"pinned":false}],"colors":{}}"#),&[]));
        let mirrored:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let list=mirrored["accounts"].as_array().unwrap();
        assert_eq!(list.len(),2);
        assert_eq!(list[0]["alias"],serde_json::json!("主端新别名"),"别名等展示字段跟随主端");
        assert_eq!(list[0]["source"],serde_json::json!("C:/login/a1.json"),"本机登录路径按 id 回填，镜像不掉登录");
        assert_eq!(list[0]["slot"],serde_json::json!("ab12"),"凭据槽位回填，克隆来的 API Key 才能读得到");
        let _=std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calendar_mirror_replaces_config_and_active(){
        let dir=tmpdir("cal");
        let path=dir.join("calendar.json");
        std::fs::write(&path,br#"{"accounts":[{"provider":"feishu","username":"a"}],"active":"feishu"}"#).unwrap();
        assert!(mirror_calendar(&path,Some(r#"{"accounts":[{"provider":"apple","username":"b"}],"active":"apple"}"#),&[]));
        let mirrored:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(mirrored["accounts"].as_array().unwrap().len(),1);
        assert_eq!(mirrored["active"],serde_json::json!("apple"),"整表替换连 active 一起跟随主端");
        // pending 删除键屏蔽对应账号：主端还没删，从端不复活。
        assert!(!mirror_calendar(&path,Some(r#"{"accounts":[{"provider":"feishu","username":"a"},{"provider":"apple","username":"b"}],"active":"feishu"}"#),&["cal:feishu:a".to_string()]));
        let mirrored:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(mirrored["accounts"].as_array().unwrap().len(),1);
        let _=std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_calendar_mirror_replaces_events(){
        let dir=tmpdir("local");
        let path=dir.join("local-calendar.json");
        std::fs::write(&path,br#"[{"uid":"u1","title":"a","start":100,"end":200,"all_day":false,"reminder":15}]"#).unwrap();
        assert!(mirror_local_calendar(&path,Some(r#"[{"uid":"u1","title":"a","start":100,"end":200,"all_day":false,"reminder":15},{"uid":"u2","title":"b","start":300,"end":400,"all_day":false,"reminder":null}]"#)));
        let mirrored:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(mirrored.as_array().unwrap().len(),2,"主端事件整表镜像到从端");
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
        mirror_mail(&dir.join("mail-accounts.json"),Some(r#"[{"email":"a@b.com","provider":"qq","enabled":true}]"#),&[]);
        assert_ne!(union_fingerprint(&dir),before,"账号新增后指纹必须变化，才会刷新内存");
        let _=std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_master_is_macos_only(){
        // 主从角色口径：macOS 是唯一数据主端；本测试在两端编译运行，
        // 双向锚定 is_data_master 的平台语义，防止日后被误改。
        assert_eq!(is_data_master(), cfg!(target_os = "macos"));
        assert_eq!(is_slave(), cfg!(not(target_os = "macos")));
    }
}
