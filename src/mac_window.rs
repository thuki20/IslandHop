//! IslandHop 的 macOS 平台层。
//!
//! 岛仔上游是 Windows 专用：102 个业务模块里 261 处 `cfg(windows)`、
//! **0 处 `target_os = "macos"`**，非 Windows 分支基本是空实现
//! （`smtc::query` 返回 `None`、`clipboard::watch` 返回空 `Watcher`、
//! `display_layout::connected` 返回 `Vec::new()`）。
//! 这里补齐 IslandHop 在 Mac 上最影响体验的两件事：窗口层级与刘海几何读取。
//! 其余平台模块见 README 的「Mac 平台层缺口清单」。

/// 把 ROOT 窗口抬到 `NSStatusWindowLevel(25)`，高于菜单栏
/// `NSMainMenuWindowLevel(24)`，否则 macOS 会把请求 `y=0` 的窗口强制
/// 挤到菜单栏下方（实测停在 `y=38`），岛就不在屏幕顶部。
///
/// `eframe` 的 `with_always_on_top()` 只给到 `NSFloatingWindowLevel(3)`。
/// 返回 true 表示已成功设置，调用方应据此只执行一次并重新下发窗口坐标。
#[cfg(target_os = "macos")]
pub fn elevate(frame: &eframe::Frame) -> bool {
    use objc2::runtime::{AnyObject, Bool};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = frame.window_handle() else { return false };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return false };
    // raw-window-handle 0.6 只暴露 NSView，NSWindow 要经 [view window] 取。
    let view = h.ns_view.as_ptr() as *mut AnyObject;
    if view.is_null() { return false; }
    let ns: *mut AnyObject = unsafe { objc2::msg_send![view, window] };
    if ns.is_null() { return false; }
    unsafe {
        let _: () = objc2::msg_send![ns, setLevel: 25isize];
        // canJoinAllSpaces(1) | stationary(16) | fullScreenAuxiliary(256)
        let _: () = objc2::msg_send![ns, setCollectionBehavior: (1usize | 16 | 256)];
        let _: () = objc2::msg_send![ns, setHasShadow: Bool::NO];
        let _: () = objc2::msg_send![ns, setOpaque: Bool::NO];
        // 启动反馈：上游 `with_active(false)` 会让 macOS 完全不激活本应用，
        // 用户双击后看不到任何变化（无菜单栏切换、无 Dock 弹跳），体感是「打不开」。
        // 这里激活一次：菜单栏切到本应用、Dock 图标亮起，作为「已成功打开」的明确信号。
        let app: *mut AnyObject = objc2::msg_send![objc2::class!(NSApplication), sharedApplication];
        if !app.is_null() {
            let _: () = objc2::msg_send![app, activateIgnoringOtherApps: Bool::YES];
        }
    }
    true
}

#[cfg(not(target_os = "macos"))]
pub fn elevate(_frame: &eframe::Frame) -> bool { true }

/// 主屏刘海尺寸（逻辑点）。无刘海机型返回 None。
/// 复用 OpenHop 引擎里已验证的 `safeAreaInsets` 实现，不另造一套。
#[cfg(target_os = "macos")]
pub fn notch_size() -> Option<(f64, f64)> {
    openhop_core::extras::system::notch()
}

#[cfg(not(target_os = "macos"))]
pub fn notch_size() -> Option<(f64, f64)> { None }
