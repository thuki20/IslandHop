//! IslandHop 的 macOS 平台层。
//!
//! 岛仔上游是 Windows 专用：102 个业务模块里 261 处 `cfg(windows)`、
//! **0 处 `target_os = "macos"`**，非 Windows 分支基本是空实现。
//! Windows 上 `native.rs` 会用 `SetWindowRgn` 把透明画布裁成「岛体命中区」，
//! 区域外点击穿透到下层窗口；macOS 没有这层，一个 960×270 的无边框置顶
//! 窗口会把屏幕顶部整条吃掉，表现就是「很多地方鼠标都点不了」。
//! 这里补上等价能力：按岛体命中矩形动态设置 `NSWindow.ignoresMouseEvents`，
//! 并用全局鼠标坐标换算出窗口本地坐标供 egui 判定悬停/展开。

/// 把 ROOT 窗口抬到 `NSStatusWindowLevel(25)`，高于菜单栏
/// `NSMainMenuWindowLevel(24)`，否则 macOS 会把请求 `y=0` 的窗口强制
/// 挤到菜单栏下方（实测停在 `y=38`），岛就不在屏幕顶部。
///
/// `eframe` 的 `with_always_on_top()` 只给到 `NSFloatingWindowLevel(3)`。
/// 返回 true 表示已成功设置，调用方应据此只执行一次并重新下发窗口坐标。
#[cfg(target_os = "macos")]
pub fn elevate(frame: &eframe::Frame) -> bool {
    imp::elevate(frame)
}

#[cfg(not(target_os = "macos"))]
pub fn elevate(_frame: &eframe::Frame) -> bool { true }

/// 鼠标在 ROOT 窗口内的本地逻辑坐标（左上原点，与 egui 一致）。
/// 鼠标不在窗口矩形内时返回 `None`。
///
/// 不用 `ctx.input(|i| i.pointer.latest_pos())`：窗口一旦
/// `ignoresMouseEvents=YES`，winit 收不到鼠标移动，egui 的指针坐标会停在上次
/// 的值，岛就再也不会响应悬停。全局 `NSEvent.mouseLocation` 不依赖事件流，
/// 也不需要「输入监控」权限（只有监听事件流才需要）。
#[cfg(target_os = "macos")]
pub fn cursor_local(ppp: f32) -> Option<eframe::egui::Pos2> {
    imp::cursor_local(ppp)
}

#[cfg(not(target_os = "macos"))]
pub fn cursor_local(_ppp: f32) -> Option<eframe::egui::Pos2> { None }

/// 每帧按岛体命中矩形决定主窗口是否穿透鼠标。
/// `cursor` 为全局鼠标换算出的窗口本地坐标；`hits` 是 `FrameOut` 里的
/// `hit` / `notice_hit` / `file_drop_zone`，均为窗口本地逻辑矩形。
/// 命中区外一律穿透，点击落到下面的应用。返回是否处于穿透状态，
/// 调用方据此提高重绘频率（穿透时窗口收不到 mousemove）。
#[cfg(target_os = "macos")]
pub fn sync_passthrough(cursor: Option<eframe::egui::Pos2>, hits: &[eframe::egui::Rect]) -> bool {
    let inside = cursor.is_some_and(|p| hits.iter().any(|r| r.expand(10.0).contains(p)));
    imp::set_passthrough(!inside);
    !inside
}

#[cfg(not(target_os = "macos"))]
pub fn sync_passthrough(_cursor: Option<eframe::egui::Pos2>, _hits: &[eframe::egui::Rect]) -> bool { false }

/// 主屏刘海尺寸（逻辑点）。无刘海机型返回 None。
/// 复用 OpenHop 引擎里已验证的 `safeAreaInsets` 实现，不另造一套。
#[cfg(target_os = "macos")]
pub fn notch_size() -> Option<(f64, f64)> {
    openhop_core::extras::system::notch()
}

#[cfg(not(target_os = "macos"))]
pub fn notch_size() -> Option<(f64, f64)> { None }

#[cfg(target_os = "macos")]
mod imp {
    use eframe::egui::Pos2;
    use objc2::encode::{Encode, Encoding, RefEncode};
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

    /// `NSWindow*`。eframe 的 `Frame` 只在 `update()` 里可用，而光标换算每帧
    /// 都要做，所以在 `elevate()` 时缓存一次（窗口生命周期 = 应用生命周期）。
    static WINDOW: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    static IGNORING: AtomicBool = AtomicBool::new(false);

    type CGFloat = f64;

    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    struct NSPoint { x: CGFloat, y: CGFloat }
    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    struct NSSize { width: CGFloat, height: CGFloat }
    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    struct NSRect { origin: NSPoint, size: NSSize }

    // msg_send! 要求返回值实现 Encode；按 objc2 官方 core-graphics 范例手写。
    unsafe impl Encode for NSPoint {
        const ENCODING: Encoding = Encoding::Struct("CGPoint", &[CGFloat::ENCODING, CGFloat::ENCODING]);
    }
    unsafe impl RefEncode for NSPoint {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
    }
    unsafe impl Encode for NSSize {
        const ENCODING: Encoding = Encoding::Struct("CGSize", &[CGFloat::ENCODING, CGFloat::ENCODING]);
    }
    unsafe impl RefEncode for NSSize {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
    }
    unsafe impl Encode for NSRect {
        const ENCODING: Encoding = Encoding::Struct("CGRect", &[NSPoint::ENCODING, NSSize::ENCODING]);
    }
    unsafe impl RefEncode for NSRect {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
    }

    fn window() -> *mut AnyObject { WINDOW.load(Ordering::Relaxed) as *mut AnyObject }

    pub fn elevate(frame: &eframe::Frame) -> bool {
        let Ok(handle) = frame.window_handle() else { return false };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return false };
        // raw-window-handle 0.6 只暴露 NSView，NSWindow 要经 [view window] 取。
        let view = h.ns_view.as_ptr() as *mut AnyObject;
        if view.is_null() { return false }
        let ns: *mut AnyObject = unsafe { msg_send![view, window] };
        if ns.is_null() { return false }
        unsafe {
            let _: () = msg_send![ns, setLevel: 25isize];
            // canJoinAllSpaces(1) | stationary(16) | fullScreenAuxiliary(256)
            let _: () = msg_send![ns, setCollectionBehavior: (1usize | 16 | 256)];
            let _: () = msg_send![ns, setHasShadow: Bool::NO];
            let _: () = msg_send![ns, setOpaque: Bool::NO];
            // 启动反馈：上游 `with_active(false)` 会让 macOS 完全不激活本应用，
            // 用户双击后看不到任何变化（体感是「打不开」）。
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            if !app.is_null() {
                let _: () = msg_send![app, activateIgnoringOtherApps: Bool::YES];
            }
        }
        WINDOW.store(ns as *mut c_void, Ordering::Relaxed);
        true
    }

    pub fn cursor_local(ppp: f32) -> Option<Pos2> {
        let ns = window();
        if ns.is_null() { return None }
        let mouse: NSPoint = unsafe { msg_send![class!(NSEvent), mouseLocation] };
        let frame: NSRect = unsafe { msg_send![ns, frame] };
        // AppKit 坐标系原点在左下，egui 在左上；两者都是逻辑点，无需除以 ppp。
        let x = mouse.x - frame.origin.x;
        let y = frame.origin.y + frame.size.height - mouse.y;
        if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0
            || x > frame.size.width || y > frame.size.height
        {
            return None;
        }
        // ppp 仅用于防御异常值；正常路径不改变坐标含义。
        let _ = ppp;
        Some(Pos2::new(x as f32, y as f32))
    }

    pub fn set_passthrough(ignore: bool) {
        let ns = window();
        if ns.is_null() { return }
        if IGNORING.swap(ignore, Ordering::Relaxed) == ignore { return }
        let value = if ignore { Bool::YES } else { Bool::NO };
        unsafe { let _: () = msg_send![ns, setIgnoresMouseEvents: value]; }
    }
}
