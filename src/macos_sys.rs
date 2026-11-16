//! macOS 系统数据源（IslandHop 新增）。
//!
//! 岛仔上游把 `system / native / tray / wechat` 整模块用 `#[cfg(windows)]` 排除，
//! 因此 macOS 上电池、音量、网络、闲置时间、媒体、文件打开全是空值或空操作，
//! 灵动岛收起状态只剩时钟和天气。本模块补上这些数据源：
//!
//! - 电池：IOKit 直读 IORegistry `AppleSmartBattery`（借鉴 StatusTrio，零子进程、
//!   事件级实时，且能拿充电功率）；读不到才回退 `/usr/bin/pmset -g batt`
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
    /// 正在充电时的实时功率（瓦）。None = 未在充电（含接电已充满）。
    pub watts: Option<f32>,
    pub volume: Option<(u32, bool)>,
    pub network: Option<bool>,
    pub idle: Option<f64>,
}

/// AppleSmartBattery 直读结果（纯数据，解析逻辑在 [`smart_from`] 可单测）。
struct SmartBattery {
    percent: Option<u8>,
    /// 接着电源（含充满挂机）。
    external: bool,
    /// 正在充电（IsCharging）。
    charging_now: bool,
    watts: Option<f32>,
}

/// 从 AppleSmartBattery 属性字典挑出的标量 → 电池状态（纯函数，可测）。
/// - 百分比：`CurrentCapacity` 直接是 0-100；个别固件给 mAh 时按 `MaxCapacity` 换算。
/// - 功率：|Amperage(mA)| × Voltage(mV) / 1e6，仅在 IsCharging 时给出，<0.5W 视为未充电。
///   注意 AppleSmartBattery 的电流**符号约定随固件而异**：多数机型充电为负（电流流入电池），
///   也有固件充电给正——按绝对值取幅值，充电与否只信 IsCharging 标志。
fn smart_from(
    capacity: Option<f64>,
    max_capacity: Option<f64>,
    amperage: Option<f64>,
    voltage: Option<f64>,
    is_charging: bool,
    external: bool,
) -> SmartBattery {
    let percent = capacity.map(|c| {
        let pct = if c <= 100.0 || max_capacity.map_or(true, |m| m <= 0.0) { c }
            else { c / max_capacity.unwrap_or(100.0) * 100.0 };
        pct.round().clamp(0.0, 100.0) as u8
    });
    let watts = match (amperage, voltage) {
        // 接电源就算功率（电流取绝对值，兼容充电为负/正两种固件符号约定）。
        // 不再要求 is_charging：满电 / 优化电池充电时 IsCharging=false 但依然插着
        // 电源，此时用户要的是看实时功率（含涓流），而不是退回百分比。
        // 未接电源（用电池）时 external=false，一律不算功率 → 绘制层显示百分比。
        (Some(a), Some(v)) if external && a != 0.0 && v > 0.0 => {
            let w = (a.abs() * v / 1.0e6) as f32;
            (w >= 0.5).then_some(w)
        },
        _ => None,
    };
    SmartBattery { percent, external, charging_now: is_charging && external, watts }
}

/// IOKit 直读 IORegistry 的 `AppleSmartBattery`：无子进程、毫秒级，
/// 充电功率（InstantAmperage × Voltage）只有这里能拿到（借鉴 StatusTrio）。
#[cfg(target_os = "macos")]
fn read_smart_battery() -> Option<SmartBattery> {
    use std::ffi::{c_char, c_void, CStr};
    const KUTF8: u32 = 0x0800_0100;
    const SINT64: isize = 4;
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(port: u32, matching: *mut c_void) -> u32;
        fn IORegistryEntryCreateCFProperties(
            entry: u32, properties: *mut *mut c_void, allocator: *mut c_void, options: u32,
        ) -> i32;
        fn IOObjectRelease(object: u32) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(alloc: *const c_void, c_str: *const c_char, encoding: u32) -> *const c_void;
        fn CFDictionaryGetValue(the_dict: *const c_void, key: *const c_void) -> *const c_void;
        fn CFNumberGetValue(number: *const c_void, the_type: isize, value_ptr: *mut c_void) -> bool;
        fn CFBooleanGetValue(boolean: *const c_void) -> bool;
        fn CFRelease(cf: *mut c_void);
    }
    const KEY: &[(&[u8], bool)] = &[
        (b"CurrentCapacity\0", true), (b"MaxCapacity\0", true),
        (b"InstantAmperage\0", true), (b"Amperage\0", true), (b"Voltage\0", true),
        (b"IsCharging\0", false), (b"ExternalConnected\0", false),
    ];
    unsafe {
        let matching = IOServiceMatching(CStr::from_bytes_with_nul(b"AppleSmartBattery\0").unwrap().as_ptr());
        if matching.is_null() { return None; }
        let service = IOServiceGetMatchingService(0, matching); // 0 = kIOMasterPortDefault
        if service == 0 { return None; }
        let mut props: *mut c_void = std::ptr::null_mut();
        let status = IORegistryEntryCreateCFProperties(service, &mut props, std::ptr::null_mut(), 0);
        let _ = IOObjectRelease(service);
        if status != 0 || props.is_null() { return None; }
        // (键, 是否数字)；布尔键用 CFBoolean 读。
        let mut values: Vec<Option<f64>> = Vec::new();
        let mut booleans: Vec<bool> = Vec::new();
        for &(bytes, numeric) in KEY {
            let name = CStr::from_bytes_with_nul(bytes).unwrap();
            let cf_key = CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), KUTF8);
            let value = CFDictionaryGetValue(props, cf_key);
            CFRelease(cf_key.cast_mut());
            if numeric {
                let mut out: i64 = 0;
                values.push((!value.is_null()
                    && CFNumberGetValue(value, SINT64, (&mut out as *mut i64).cast()))
                    .then_some(out as f64));
            } else {
                booleans.push(!value.is_null() && CFBooleanGetValue(value));
            }
        }
        CFRelease(props);
        // KEY 顺序：CurrentCapacity, MaxCapacity, InstantAmperage, Amperage, Voltage | IsCharging, ExternalConnected
        Some(smart_from(
            values[0], values[1],
            values[2].or(values[3]), values[4],
            booleans[0], booleans[1],
        ))
    }
}
#[cfg(not(target_os = "macos"))]
fn read_smart_battery() -> Option<SmartBattery> { None }

fn run(program: &str, args: &[&str]) -> Option<String> {
    Command::new(program).args(args).output().ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// 返回 (剩余电量百分比, 是否接着电源, 充电功率瓦数)。
/// 台式机无电池时返回 (None, true, None)。
pub fn power() -> (Option<u8>, bool, Option<f32>) {
    if let Some(b) = read_smart_battery() {
        return (b.percent, b.external, b.watts);
    }
    let Some(out) = run("/usr/bin/pmset", &["-g", "batt"]) else { return (None, true, None) };
    let mut battery = None;
    for line in out.lines() {
        let Some(pos) = line.find('%') else { continue };
        let digits: String = line[..pos].chars().rev().take_while(|c| c.is_ascii_digit())
            .collect::<Vec<_>>().into_iter().rev().collect();
        if let Ok(v) = digits.parse::<u8>() { battery = Some(v); break; }
    }
    // 电池行出现 discharging 才是真正在用电池；AC Power / charged / charging 都算接着电源。
    let charging = !out.lines().any(|l| l.contains("discharging"));
    (battery, charging, None)
}

/// macOS 开机自启：LaunchAgent（`~/Library/LaunchAgents/com.ncsai.islandhop.plist`）。
/// 不用 SMAppService——它要求 App 打包成 .app 且签名约束多，对裸二进制/开发构建
/// 不生效；用户级 LaunchAgent + `launchctl load` 对任何形态都有效，无需管理员权限。
pub mod autostart {
    const LABEL: &str = "com.ncsai.islandhop";

    fn plist_path() -> Option<std::path::PathBuf> {
        std::env::var_os("HOME")
            .map(|home| std::path::PathBuf::from(home).join("Library").join("LaunchAgents").join(format!("{LABEL}.plist")))
    }

    /// 开机自启当前是否开启（以 LaunchAgent plist 是否存在为准）。
    pub fn enabled() -> bool {
        plist_path().is_some_and(|path| path.exists())
    }

    /// 开/关开机自启：写或删 LaunchAgent plist，并同步 launchctl 加载/卸载。
    pub fn set(on: bool) -> Result<(), String> {
        let path = plist_path().ok_or("读不到 HOME 环境变量，无法定位 LaunchAgents 目录")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建 LaunchAgents 目录失败：{e}"))?;
        }
        let path_text = path.display().to_string();
        if on {
            let exe = std::env::current_exe().map_err(|e| format!("定位当前程序失败：{e}"))?;
            let plist = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key><array><string>{}</string></array>
    <key>RunAtLoad</key><true/>
    <key>ProcessType</key><string>Interactive</string>
</dict></plist>
"#,
                exe.display()
            );
            std::fs::write(&path, plist).map_err(|e| format!("写 LaunchAgent 失败：{e}"))?;
            // 不要吞掉 launchctl 的返回码：plist 写成功但加载失败时若返回 Ok，
            // UI 会误报"已开启开机自启"而实际没生效——正是"打不开自启"的静默失败。
            let result = std::process::Command::new("/bin/launchctl")
                .args(["load", &path_text]).output()
                .map_err(|e| format!("执行 launchctl load 失败：{e}"))?;
            if !result.status.success() {
                let err = String::from_utf8_lossy(&result.stderr).trim().to_owned();
                return Err(format!("开机自启未生效（launchctl load 失败）：{}",
                    if err.is_empty() { "未知错误".into() } else { err }));
            }
            Ok(())
        } else {
            // 卸载当前会话里已加载的 Agent（未加载时报错没关系），再删 plist。
            let _ = std::process::Command::new("/bin/launchctl").args(["unload", &path_text]).output();
            match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("删除 LaunchAgent 失败：{e}")),
            }
        }
    }
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
    let (battery, charging, watts) = power();
    let snap = Snapshot { battery, charging, watts, volume: volume(), network: network(), idle: idle_seconds() };
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
        let (battery, charging, watts) = power();
        let snap = Snapshot { battery, charging, watts, volume: volume(), network: network(), idle: idle_seconds() };
        if tx.send(snap).is_err() { break; }
        ctx.request_repaint();
        std::thread::sleep(Duration::from_millis(1500));
    });
    rx
}

#[cfg(test)]
mod battery_tests {
    use super::*;

    // 96W 充电器给 MacBook 充电的典型读数：InstantAmperage 约 6200mA、Voltage 约 11600mV。
    #[test]
    fn charging_reports_watts_and_percent() {
        let b = smart_from(Some(76.0), Some(5322.0), Some(6200.0), Some(11600.0), true, true);
        assert_eq!(b.percent, Some(76));
        assert!(b.external);
        assert!(b.charging_now);
        let watts = b.watts.expect("充电中必须给出功率");
        assert!((watts - 71.9).abs() < 1.0, "6200mA×11600mV≈71.9W，实际 {watts}");
    }

    // 大部分 MacBook 固件充电时 InstantAmperage 为负（电流流入电池）：
    // 必须按绝对值取幅值，否则充电时电量环内永远不显示功率（0.6.77 实测问题）。
    #[test]
    fn charging_with_negative_amperage_still_reports_watts() {
        let b = smart_from(Some(76.0), Some(5322.0), Some(-6200.0), Some(11600.0), true, true);
        let watts = b.watts.expect("充电中（电流为负的固件）也必须给出功率");
        assert!((watts - 71.9).abs() < 1.0, "|-6200mA|×11600mV≈71.9W，实际 {watts}");
    }

    // 放电：Amperage 为负，不给功率；插电充满：IsCharging=false，不给功率但仍算接着电源。
    #[test]
    fn discharging_and_fully_charged_have_no_watts() {
        let b = smart_from(Some(64.0), None, Some(-1420.0), Some(11400.0), false, false);
        assert_eq!(b.percent, Some(64));
        assert!(!b.external && !b.charging_now && b.watts.is_none());
        let full = smart_from(Some(100.0), None, Some(0.0), Some(12800.0), false, true);
        assert!(full.external && !full.charging_now && full.watts.is_none(), "充满挂机不算充电");
    }

    // 用户实测：优化电池充电 / 满电附近 IsCharging=false，但依然插着电源且有
    // 涓流电流——这种情形也要显示实时功率，不能退回百分比。
    #[test]
    fn external_but_not_flagged_charging_still_reports_watts() {
        let b = smart_from(Some(92.0), None, Some(-235.0), Some(11800.0), false, true);
        assert!(b.external && !b.charging_now, "未报充电标志");
        assert!(b.watts.is_some_and(|w| (w - 2.77).abs() < 0.1), "235mA×11.8V≈2.77W：{:?}", b.watts);
        // 同样电流但拔掉电源 → 不显示功率，绘制层走百分比。
        let off = smart_from(Some(92.0), None, Some(-235.0), Some(11800.0), false, false);
        assert!(off.watts.is_none(), "用电池时显示百分比");
    }

    // 个别固件 CurrentCapacity 给 mAh（>100），要按 MaxCapacity 换算成百分比。
    #[test]
    fn mah_capacity_is_converted_to_percent() {
        let b = smart_from(Some(2800.0), Some(5088.0), None, None, false, false);
        assert_eq!(b.percent, Some(55), "2800/5088≈55%");
    }

    // 台式机/读不到电池：百分比缺失但不炸。
    #[test]
    fn missing_capacity_stays_none() {
        let b = smart_from(None, None, None, None, false, true);
        assert_eq!(b.percent, None);
        assert!(b.external);
    }

    // 实机验证（默认忽略，发版前跑一次）：IOKit 直读必须拿到本机电池百分比，
    // 且与 pmset 的读数一致；充电功率可打印核对。
    // `cargo test --lib live_smart_battery -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_smart_battery_reads_real_values() {
        let (pct, external, watts) = power();
        println!("IOKit 直读：percent={pct:?} external={external} watts={watts:?}");
        let pmset = std::process::Command::new("/usr/bin/pmset").args(["-g", "batt"])
            .output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        println!("pmset 对照：{}", pmset.lines().take(2).collect::<Vec<_>>().join(" | "));
        assert!(pct.is_some(), "本机（MacBook）有电池，IOKit 直读必须拿到百分比");
        if let (Some(a), Some(b)) = (pct, pmset.lines().next().and_then(|l| l.split('%').next().and_then(|s| s.split_whitespace().last().and_then(|d| d.parse::<u8>().ok())))) {
            assert_eq!(a, b, "IOKit 与 pmset 的百分比应一致");
        }
    }
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
