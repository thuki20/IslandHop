//! macOS 系统数据源（IslandHop 新增）。
//!
//! 岛仔上游把 `system / native / tray / wechat` 整模块用 `#[cfg(windows)]` 排除，
//! 因此 macOS 上电池、音量、网络、闲置时间、媒体、文件打开全是空值或空操作，
//! 灵动岛收起状态只剩时钟和天气。本模块补上这些数据源：
//!
//! - 电池：`/usr/bin/pmset -g batt`
//! - 音量：`/usr/bin/osascript` 读写系统输出音量与静音
//! - 网络：`/sbin/route -n get default`（有默认网关即视为在线）
//! - 闲置：`CGEventSourceSecondsSinceLastEventType`（无需辅助功能权限）
//! - 媒体：AppleScript 读 Music / Spotify，2 秒节流避免刷爆 osascript
//! - 文件：`/usr/bin/open` / `open -R`
//!
//! 采集全部在后台线程，主线程只读最新快照；采样失败一律返回 None，绝不让
//! 系统命令拖慢渲染或让应用起不来。

use eframe::egui;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub battery: Option<u8>,
    pub charging: bool,
    pub volume: Option<(u32, bool)>,
    pub network: Option<bool>,
    pub idle: Option<f64>,
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    Command::new(program).args(args).output().ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// 返回 (剩余电量百分比, 是否接着电源)。台式机无电池时返回 (None, true)。
pub fn power() -> (Option<u8>, bool) {
    let Some(out) = run("/usr/bin/pmset", &["-g", "batt"]) else { return (None, true) };
    let mut battery = None;
    for line in out.lines() {
        let Some(pos) = line.find('%') else { continue };
        let digits: String = line[..pos].chars().rev().take_while(|c| c.is_ascii_digit())
            .collect::<Vec<_>>().into_iter().rev().collect();
        if let Ok(v) = digits.parse::<u8>() { battery = Some(v); break; }
    }
    // 电池行出现 discharging 才是真正在用电池；AC Power / charged / charging 都算接着电源。
    let charging = !out.lines().any(|l| l.contains("discharging"));
    (battery, charging)
}

/// 系统输出音量 (0..100, 是否静音)。
pub fn volume() -> Option<(u32, bool)> {
    let v = run("/usr/bin/osascript", &["-e", "output volume of (get volume settings)"])?
        .trim().parse::<u32>().ok()?;
    let muted = run("/usr/bin/osascript", &["-e", "output muted of (get volume settings)"])
        .is_some_and(|s| s.trim() == "true");
    Some((v.min(100), muted))
}

pub fn set_volume(v: u32) {
    let script = format!("set volume output volume {}", v.min(100));
    let _ = Command::new("/usr/bin/osascript").args(["-e", &script]).spawn();
}

pub fn toggle_mute() {
    let muted = run("/usr/bin/osascript", &["-e", "output muted of (get volume settings)"])
        .is_some_and(|s| s.trim() == "true");
    let script = if muted { "set volume without output muted" } else { "set volume with output muted" };
    let _ = Command::new("/usr/bin/osascript").args(["-e", script]).spawn();
}

/// 是否有可用的默认路由。只判断本机网络栈，不做外网探测（不产生流量、不卡顿）。
pub fn network() -> Option<bool> {
    let out = run("/sbin/route", &["-n", "get", "default"])?;
    Some(out.contains("interface:") && out.contains("gateway:"))
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: u32, event_type: u32) -> f64;
}

/// 距上次任意 HID 输入的秒数。公开 API，不需要辅助功能授权。
pub fn idle_seconds() -> Option<f64> {
    // kCGEventSourceStateHIDSystemState = 1；kCGAnyInputEventType = 0xFFFFFFFF
    let v = unsafe { CGEventSourceSecondsSinceLastEventType(1, 0xFFFF_FFFF) };
    (v.is_finite() && v >= 0.0).then_some(v)
}

pub fn open_path(p: &Path) -> Result<(), String> {
    Command::new("/usr/bin/open").arg(p).spawn()
        .map(|_| ()).map_err(|e| format!("无法打开：{e}"))
}

pub fn reveal(p: &Path) {
    let _ = Command::new("/usr/bin/open").arg("-R").arg(p).spawn();
}

pub fn open_url(url: &str) {
    let _ = Command::new("/usr/bin/open").arg(url).spawn();
}

/// AppleScript 版文件选择（macOS 无 Win32 IFileDialog 等价物时用）。
/// 返回用户选中的 POSIX 路径；取消时返回空。
pub fn pick_files() -> Vec<std::path::PathBuf> {
    let script = "set f to choose file with multiple selections allowed\n\
                  set out to \"\"\n\
                  repeat with x in f\n\
                    set out to out & (POSIX path of x) & linefeed\n\
                  end repeat\n\
                  return out";
    run("/usr/bin/osascript", &["-e", script]).map(|s| {
        s.lines().filter(|l| !l.trim().is_empty()).map(std::path::PathBuf::from).collect()
    }).unwrap_or_default()
}

/// 媒体查询缓存：smtc 的轮询线程 250ms 一次，osascript 不能跟着跑。
static MEDIA: Mutex<Option<(Instant, Option<crate::smtc::NowPlaying>)>> = Mutex::new(None);

pub fn now_playing() -> Option<crate::smtc::NowPlaying> {
    let Ok(mut slot) = MEDIA.lock() else { return None };
    if let Some((at, ref cached)) = *slot {
        if at.elapsed() < Duration::from_secs(2) { return cached.clone(); }
    }
    let np = query_media();
    *slot = Some((Instant::now(), np.clone()));
    np
}

fn process_running(name: &str) -> bool {
    Command::new("/usr/bin/pgrep").args(["-x", name]).output()
        .is_ok_and(|o| o.status.success())
}

/// mediaremote-adapter 资源目录（.app/Contents/Resources/mediaremote-adapter）。
/// 开发机直跑二进制时不存在，返回 None 走 AppleScript 兜底。
fn adapter_paths() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?.join("Resources").join("mediaremote-adapter");
    let script = dir.join("mediaremote-adapter.pl");
    let framework = dir.join("MediaRemoteAdapter.framework");
    (script.is_file() && framework.is_dir()).then(|| (script, framework))
}

/// 用 mediaremote-adapter 读系统「正在播放」（macOS 15.4+ 对第三方封锁了
/// MediaRemote；该工具借 /usr/bin/perl 的可信身份 com.apple.perl5 调用，
/// 因此网易云、QQ音乐等任意播放器都能读到，含播放位置）。
fn adapter_now_playing() -> Option<crate::smtc::NowPlaying> {
    let (script, framework) = adapter_paths()?;
    let out = Command::new("/usr/bin/perl").arg(&script).arg(&framework)
        .args(["get", "--now", "--no-artwork"]).output().ok()?;
    if !out.status.success() { return None; }
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    adapter_json_to_now_playing(value)
}

/// 适配器 JSON → NowPlaying。键名来自上游 v0.7.7（packaging/mra/LICENSE）。
fn adapter_json_to_now_playing(value: serde_json::Value) -> Option<crate::smtc::NowPlaying> {
    use serde_json::Value;
    let object = value.as_object()?; // 无媒体播放时输出 null
    let title = object.get("title").and_then(Value::as_str).unwrap_or_default().trim().to_string();
    if title.is_empty() { return None; }
    let text = |key: &str| object.get(key).and_then(Value::as_str).unwrap_or_default().trim().to_string();
    let num = |key: &str| object.get(key).and_then(Value::as_f64).unwrap_or_default();
    Some(crate::smtc::NowPlaying {
        title,
        artist: text("artist"),
        album: text("album"),
        // bundle id 形如 com.netease.163music / com.apple.Music；显示用取尾段。
        source: object.get("bundleIdentifier").and_then(Value::as_str)
            .and_then(|id|id.rsplit('.').next()).unwrap_or("MediaRemote").to_string(),
        song_id: None,
        playing: object.get("playing").and_then(Value::as_bool).unwrap_or(false),
        position: num("elapsedTimeNow"),
        sampled_at: Some(Instant::now()),
        event_time_ms: None,
        duration: num("duration"),
        artwork: None,
        playback_type: crate::smtc::MediaKind::Music,
        volume: None,
    })
}

fn query_media() -> Option<crate::smtc::NowPlaying> {
    // 首选 mediaremote-adapter：覆盖全部播放器（含网易云/QQ 音乐），带实时进度；
    // 适配器缺失（旧包/开发机直跑）或失败时退回 Music / Spotify 的 AppleScript。
    if let Some(media) = adapter_now_playing() { return Some(media); }
    for app in ["Music", "Spotify"] {
        if !process_running(app) { continue; }
        let script = format!(
            "tell application \"{app}\"\n\
               if player state is stopped then return \"\"\n\
               set t to current track\n\
               return (name of t) & \"␟\" & (artist of t) & \"␟\" & (duration of t as text) & \"␟\" & (player position as text) & \"␟\" & (player state as text)\n\
             end tell"
        );
        let Some(out) = run("/usr/bin/osascript", &["-e", &script]) else { continue };
        let parts: Vec<&str> = out.trim().split('␟').collect();
        if parts.len() < 5 || parts[0].trim().is_empty() { continue; }
        let duration = parts[2].trim().replace(',', ".").parse::<f64>().unwrap_or(0.0);
        let position = parts[3].trim().replace(',', ".").parse::<f64>().unwrap_or(0.0);
        return Some(crate::smtc::NowPlaying {
            title: parts[0].trim().to_string(),
            artist: parts[1].trim().to_string(),
            album: String::new(),
            source: app.to_string(),
            song_id: None,
            playing: parts[4].trim().eq_ignore_ascii_case("playing"),
            position,
            sampled_at: Some(Instant::now()),
            event_time_ms: None,
            duration,
            artwork: None,
            playback_type: crate::smtc::MediaKind::Music,
            volume: None,
        });
    }
    None
}

/// IOKit 电源源事件上下文：回调里立即采一帧快照推送。
struct PowerNotify { tx: mpsc::Sender<Snapshot>, ctx: egui::Context }

/// 电源源变化回调（插拔电源、充放电状态/电量变化时系统立即触发）。
/// 参考 status-trio（lingyired/status-trio）的事件驱动方案：通知一到就采样，
/// 延迟从最多 1.5s 轮询降到毫秒级；采样失败不影响轮询兜底。
unsafe extern "C" fn on_power_change(context: *mut std::ffi::c_void) {
    let state = &*(context as *const PowerNotify);
    let (battery, charging) = power();
    let snap = Snapshot { battery, charging, volume: volume(), network: network(), idle: idle_seconds() };
    let _ = state.tx.send(snap);
    state.ctx.request_repaint();
}

/// 后台采样。每 1.5 秒一次兜底轮询；充放电变化另有 IOKit IOPS 事件通知
/// 线程即时推送（见 `on_power_change`），两层保证「无延迟」。
pub fn start(ctx: egui::Context) -> Receiver<Snapshot> {
    let (tx, rx) = mpsc::channel();
    {
        let tx = tx.clone();
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new().name("islandhop-power-notify".into()).spawn(move || unsafe {
            #[link(name = "IOKit", kind = "framework")]
            extern "C" {
                fn IOPSNotificationCreateRunLoopSource(
                    callback: unsafe extern "C" fn(*mut std::ffi::c_void),
                    context: *mut std::ffi::c_void,
                ) -> *mut std::ffi::c_void;
            }
            #[link(name = "CoreFoundation", kind = "framework")]
            extern "C" {
                fn CFRunLoopGetCurrent() -> *mut std::ffi::c_void;
                fn CFRunLoopAddSource(rl: *mut std::ffi::c_void, source: *mut std::ffi::c_void, mode: *const std::ffi::c_void);
                fn CFRunLoopRun();
                /// 必须用真正的 CFString 常量：运行循环模式按 CFString 指针/内容识别，
                /// 传裸 C 字符串会在 CFRunLoopAddSource 里直接崩溃（实测 SIGTRAP）。
                static kCFRunLoopDefaultMode: *const std::ffi::c_void;
            }
            let state = Box::into_raw(Box::new(PowerNotify { tx, ctx })).cast::<std::ffi::c_void>();
            let source = IOPSNotificationCreateRunLoopSource(on_power_change, state);
            if source.is_null() { return; } // 注册失败也不影响轮询兜底
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
            CFRunLoopRun();
        });
    }
    let _ = std::thread::Builder::new().name("islandhop-mac-sys".into()).spawn(move || loop {
        let (battery, charging) = power();
        let snap = Snapshot { battery, charging, volume: volume(), network: network(), idle: idle_seconds() };
        if tx.send(snap).is_err() { break; }
        ctx.request_repaint();
        std::thread::sleep(Duration::from_millis(1500));
    });
    rx
}

#[cfg(test)]
mod adapter_tests {
    use super::*;
    use serde_json::json;

    // 用实机（macOS 15.8 / 网易云）抓到的真实输出截断 artwork 后构造。
    #[test]
    fn adapter_json_maps_netease_now_playing() {
        let media = adapter_json_to_now_playing(json!({
            "playbackRate": 0, "album": "Going Seventeen", "elapsedTimeNow": 206.88,
            "elapsedTime": 206.88, "bundleIdentifier": "com.netease.163music",
            "processIdentifier": 53005, "title": "글쎄", "duration": 231.899,
            "artist": "SEVENTEEN", "playing": false
        })).expect("网易云的正在播放必须能映射");
        assert_eq!(media.title, "글쎄");
        assert_eq!(media.artist, "SEVENTEEN");
        assert_eq!(media.album, "Going Seventeen");
        assert_eq!(media.source, "163music");
        assert_eq!(media.duration, 231.899);
        assert_eq!(media.position, 206.88);
        assert!(!media.playing);
        assert!(media.is_music());
        assert!(media.sampled_at.is_some());
    }

    #[test]
    fn adapter_json_rejects_null_and_empty_title() {
        assert!(adapter_json_to_now_playing(serde_json::Value::Null).is_none(), "无媒体播放时适配器输出 null");
        assert!(adapter_json_to_now_playing(json!({"title": "  "})).is_none(), "空标题不能当成有效媒体");
    }
}
