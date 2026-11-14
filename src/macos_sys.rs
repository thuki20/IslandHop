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

fn query_media() -> Option<crate::smtc::NowPlaying> {
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

/// 后台采样。每 1.5 秒一次，音量与闲置变化足够跟手，系统命令开销可忽略。
pub fn start(ctx: egui::Context) -> Receiver<Snapshot> {
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new().name("islandhop-mac-sys".into()).spawn(move || loop {
        let (battery, charging) = power();
        let snap = Snapshot { battery, charging, volume: volume(), network: network(), idle: idle_seconds() };
        if tx.send(snap).is_err() { break; }
        ctx.request_repaint();
        std::thread::sleep(Duration::from_millis(1500));
    });
    rx
}
