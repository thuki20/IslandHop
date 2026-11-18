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
pub fn cursor_local(scale: f32) -> Option<eframe::egui::Pos2> {
    imp::cursor_local(scale)
}

#[cfg(not(target_os = "macos"))]
pub fn cursor_local(_scale: f32) -> Option<eframe::egui::Pos2> { None }

/// 按「岛体大小」倍率缩放主窗口画布（macOS；Windows 由 place_on_display 调整）。
#[cfg(target_os = "macos")]
pub fn resize_canvas(scale: f32) { imp::resize_canvas(scale) }

#[cfg(not(target_os = "macos"))]
pub fn resize_canvas(_scale: f32) {}

/// 每帧按岛体命中矩形决定主窗口是否穿透鼠标。
/// `cursor` 为全局鼠标换算出的窗口本地坐标；`hits` 是 `FrameOut` 里的
/// `hit` / `notice_hit` / `file_drop_zone`，均为窗口本地逻辑矩形。
/// 命中区外一律穿透，点击落到下面的应用。返回是否处于穿透状态，
/// 调用方据此提高重绘频率（穿透时窗口收不到 mousemove）。
#[cfg(target_os = "macos")]
pub fn sync_passthrough(cursor: Option<eframe::egui::Pos2>, hits: &[eframe::egui::Rect]) -> bool {
    // 外扩只留 1pt 抗抖：以前外扩 10pt，加上 hit_rect 自身的外扩，岛体下方
    // 会多出一条十几 pt 的隐形命中带——视觉透明却吞掉菜单栏正下方的点击/拖拽，
    // 也是「刘海正下方误触」的来源。命中必须与可见形状对齐。
    let inside = cursor.is_some_and(|p| hits.iter().any(|r| r.expand(1.0).contains(p)));
    if std::env::var_os("ISLANDHOP_PT_LOG").is_some() {
        static LAST: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(-1);
        let now = i8::from(inside);
        if LAST.swap(now, std::sync::atomic::Ordering::Relaxed) != now {
            eprintln!("[PT] cursor={cursor:?} pass_through={}", !inside);
        }
    }
    let recovered = imp::set_passthrough(!inside);
    if recovered {
        // 穿透期间系统丢弃了全部 mouseMoved：恢复接收后 egui 没有指针位置，
        // press 起点缺失 → click 永远判定失败（「岛内按钮点不动」根因之一）。
        // 这里补发一条当前位置的 mouseMoved 让 egui 重建指针状态。
        imp::synthesize_mouse_moved();
    }
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

/// 刘海机型收起胶囊需要避开的中间空洞宽度（刘海宽 + 两侧各 12pt 呼吸位）。
/// 本机 MacBook Pro 实测：刘海 185×32 逻辑点，而岛仔收起胶囊内容宽约 185pt，
/// 正好整条被物理刘海挡住；把常驻图标分列两侧后才能看见。
/// 无刘海机型返回 None，布局保持上游原样。
/// 把应用切到前台。设置窗口用 `with_active(false)` 创建，macOS 上不会自动
/// 激活，用户点了设置却看不到窗口（体感「偶尔打不开」），所以每次请求打开时
/// 主动激活一次。
#[cfg(target_os = "macos")]
pub fn activate_app() { imp::activate_app(); }

#[cfg(not(target_os = "macos"))]
pub fn activate_app() {}

/// 把指定标题的辅助窗口固定在 `NSStatusWindowLevel+1`（26），高于 ROOT 岛体（25）。
///
/// ROOT 常驻 25 层，而 eframe 的独立 viewport 默认只有 `Normal(0)`：更新弹窗
/// 会被岛体和所有前台应用窗口压在最底下，点它也不会改变 NSWindow 的 level，
/// 表现就是「弹窗闪到底层 / 一点就掉下去」。每帧校验一次，只在 level 不符时
/// 才 setLevel，成本可忽略；点击、切换应用都不会再改变窗口层级。
#[cfg(target_os = "macos")]
pub fn raise_window(title: &str, level: isize) { imp::raise_window(title, level); }

#[cfg(not(target_os = "macos"))]
pub fn raise_window(_title: &str, _level: isize) {}

#[cfg(target_os = "macos")]
pub fn notch_gap() -> Option<f32> {
    use std::sync::OnceLock;
    static GAP: OnceLock<Option<f32>> = OnceLock::new();
    *GAP.get_or_init(|| notch_size().map(|(w, _)| w as f32 + 24.0))
}

#[cfg(not(target_os = "macos"))]
pub fn notch_gap() -> Option<f32> { None }

/// **macOS 刘海机型专用**灵动岛布局参数：`(中心不可用宽度, 刘海高度)`，单位逻辑点。
///
/// 物理刘海是屏幕上的一块死区，Windows 没有，所以两端不能共用同一套收起态：
/// * Windows / 无刘海 Mac：常驻图标在顶边胶囊里横向排开（上游原样）。
/// * 有刘海 Mac：胶囊向下延长，常驻图标整体落到刘海**下方**的单行「下挂托盘」里，
///   中心不再放任何内容；同时隐藏时钟（macOS 右上角系统菜单栏已有时间）。
///
/// 用 `not(test)` 隔离：上游 612KB 录制测试跑在本机 MacBook 上，若测试构建也读到
/// 真实刘海尺寸，像素断言会随硬件变化。测试构建一律返回 None，保持上游基线。
#[cfg(all(target_os = "macos", not(test)))]
pub fn notch_layout() -> Option<(f32, f32)> {
    notch_size().map(|(w, h)| (w as f32 + 24.0, (h as f32).clamp(24.0, 48.0)))
}

#[cfg(not(all(target_os = "macos", not(test))))]
pub fn notch_layout() -> Option<(f32, f32)> { None }

#[cfg(target_os = "macos")]
mod imp {
    use eframe::egui::Pos2;
    use objc2::encode::{Encode, Encoding, RefEncode};
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicPtr, Ordering};

    /// `NSWindow*`。eframe 的 `Frame` 只在 `update()` 里可用，而光标换算每帧
    /// 都要做，所以在 `elevate()` 时缓存一次（窗口生命周期 = 应用生命周期）。
    static WINDOW: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

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

    pub fn activate_app() {
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            if !app.is_null() {
                let _: () = msg_send![app, activateIgnoringOtherApps: Bool::YES];
            }
        }
    }

    /// 按标题找到辅助窗口并固定 level。ROOT 是 25，更新窗用 26。
    pub fn raise_window(title: &str, level: isize) {
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            if app.is_null() { return; }
            let windows: *mut AnyObject = msg_send![app, windows];
            if windows.is_null() { return; }
            let count: usize = msg_send![windows, count];
            for i in 0..count {
                let w: *mut AnyObject = msg_send![windows, objectAtIndex: i];
                if w.is_null() { continue; }
                let t: *mut AnyObject = msg_send![w, title];
                if t.is_null() { continue; }
                let utf8: *const std::ffi::c_char = msg_send![t, UTF8String];
                if utf8.is_null() { continue; }
                if std::ffi::CStr::from_ptr(utf8).to_bytes() != title.as_bytes() { continue; }
                let current: isize = msg_send![w, level];
                if current != level { let _: () = msg_send![w, setLevel: level]; }
                break;
            }
        }
    }

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

    pub fn cursor_local(scale: f32) -> Option<Pos2> {
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
        // 岛体大小缩放：窗口按倍率放大（AppKit 点），egui 逻辑点 = AppKit 点 / 倍率。
        let scale = if scale.is_finite() && scale > 0.01 { scale } else { 1.0 } as f64;
        Some(Pos2::new((x / scale) as f32, (y / scale) as f32))
    }

    /// 把主窗口（透明画布）按岛体大小倍率缩放：AppKit 点尺寸 = 画布逻辑尺寸 × 倍率。
    /// vendored eframe 的 `scale_root_input` 会把 egui 的 screen_rect / 指针坐标
    /// / 字体栅格化换算到同一套逻辑点，UI 布局代码完全不用感知倍率。
    /// 顶边保持不动（AppKit 原点在左下），高度变化向下伸展。
    pub fn resize_canvas(scale: f32) {
        let scale = if scale.is_finite() && scale > 0.01 { scale } else { return };
        let ns = window();
        if ns.is_null() { return }
        unsafe {
            let frame: NSRect = msg_send![ns, frame];
            let w = (crate::app::CANVAS_W * scale) as f64;
            let h = (crate::app::canvas_h() * scale) as f64;
            if (frame.size.width - w).abs() < 0.5 && (frame.size.height - h).abs() < 0.5 { return; }
            let top = frame.origin.y + frame.size.height;
            let mut next = frame;
            next.size.width = w;
            next.size.height = h;
            next.origin.y = top - h;
            let _: Bool = msg_send![ns, setFrame: next display: Bool::YES animate: Bool::NO];
        }
    }

    pub fn set_passthrough(ignore: bool) -> bool {
        let ns = window();
        if ns.is_null() { return false }
        // 以 NSWindow 的真实值为准，不信任本地缓存：系统/其他代码可能已把它
        // 改回 NO（例如窗口重新成为 key），缓存不同步会让透明画布永久吃掉点击，
        // 表现就是「岛下方那片区域点不动」。
        let current: Bool = unsafe { msg_send![ns, ignoresMouseEvents] };
        let was_ignoring = current.as_bool();
        if was_ignoring == ignore { return false }
        let value = if ignore { Bool::YES } else { Bool::NO };
        unsafe { let _: () = msg_send![ns, setIgnoresMouseEvents: value]; }
        if std::env::var_os("ISLANDHOP_PT_LOG").is_some() {
            eprintln!("[PT] apply ignoresMouseEvents={ignore}");
        }
        was_ignoring && !ignore
    }

    /// 合成一条当前位置的 `kCGEventMouseMoved`。窗口恢复接收事件的瞬间调用，
    /// 让 winit/egui 立刻拿到指针位置（否则 press 无起点，click 判定失败）。
    pub fn synthesize_mouse_moved() {
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGEventCreateMouseEvent(source: *mut c_void, mouse_type: u32, cursor: CGPoint, button: u32) -> *mut c_void;
            fn CGEventPost(tap: u32, event: *mut c_void);
            fn CFRelease(cf: *mut c_void);
        }
        #[repr(C)]
        struct CGPoint { x: CGFloat, y: CGFloat }
        unsafe {
            let mouse: NSPoint = msg_send![class!(NSEvent), mouseLocation];
            let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
            if screen.is_null() { return }
            let frame: NSRect = msg_send![screen, frame];
            // AppKit 坐标系原点在左下，CoreGraphics 在左上。
            let pt = CGPoint { x: mouse.x, y: frame.origin.y + frame.size.height - mouse.y };
            // kCGEventMouseMoved = 5, kCGHIDEventTap = 0, kCGMouseButtonLeft = 0
            let ev = CGEventCreateMouseEvent(std::ptr::null_mut(), 5, pt, 0);
            if !ev.is_null() {
                CGEventPost(0, ev);
                CFRelease(ev);
            }
        }
    }

    /// 主窗口左上角的全局逻辑坐标（AppKit 左下原点已换算）。
    /// 供引擎「UI 排除区」使用：岛体在跨屏边缘上，不能当作跨屏意图。
    pub fn window_origin_top_left() -> Option<(f32, f32)> {
        let ns = window();
        if ns.is_null() { return None }
        unsafe {
            let frame: NSRect = msg_send![ns, frame];
            let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
            if screen.is_null() { return None }
            let sf: NSRect = msg_send![screen, frame];
            let top = sf.origin.y + sf.size.height - frame.origin.y - frame.size.height;
            Some((frame.origin.x as f32, top as f32))
        }
    }

    /// `NSScreen*` 的边界（逻辑点，egui 左上原点）。
    fn display_bounds(screen: *mut AnyObject) -> Option<crate::display_layout::Bounds> {
        if screen.is_null() { return None }
        unsafe {
            let f: NSRect = msg_send![screen, frame];
            let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
            if main.is_null() { return None }
            let mf: NSRect = msg_send![main, frame];
            // AppKit 全局坐标原点在主屏左下；egui 用主屏左上。
            let top = (mf.origin.y + mf.size.height - f.origin.y - f.size.height) as i32;
            Some(crate::display_layout::Bounds {
                left: f.origin.x as i32,
                top,
                right: (f.origin.x + f.size.width) as i32,
                bottom: top + f.size.height as i32,
            })
        }
    }

    pub fn main_display_bounds() -> Option<crate::display_layout::Bounds> {
        unsafe { display_bounds(msg_send![class!(NSScreen), mainScreen]) }
    }

    /// 窗口所在屏幕 → 窗口本地逻辑矩形。
    pub fn screen_rect_local() -> Option<eframe::egui::Rect> {
        let ns = window();
        if ns.is_null() { return None }
        unsafe {
            let frame: NSRect = msg_send![ns, frame];
            // 找包含窗口中心的屏（多屏时不是 mainScreen）。
            let cx = frame.origin.x + frame.size.width / 2.0;
            let cy = frame.origin.y + frame.size.height / 2.0;
            let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
            if screens.is_null() { return None }
            let count: usize = msg_send![screens, count];
            for i in 0..count {
                let screen: *mut AnyObject = msg_send![screens, objectAtIndex: i];
                let f: NSRect = msg_send![screen, frame];
                let contains = cx >= f.origin.x && cx <= f.origin.x + f.size.width
                    && cy >= f.origin.y && cy <= f.origin.y + f.size.height;
                if !contains { continue }
                let (ox, _oy) = window_origin_top_left()?;
                // 窗口本地 = 屏幕 frame − 窗口原点（都是逻辑点，AppKit→egui 已换算）。
                let left = f.origin.x as f32 - ox;
                let top = (frame.origin.y + frame.size.height) - (f.origin.y + f.size.height);
                return Some(eframe::egui::Rect::from_min_size(
                    eframe::egui::Pos2::new(left, top as f32),
                    eframe::egui::Vec2::new(f.size.width as f32, f.size.height as f32),
                ));
            }
            None
        }
    }

    /// 系统菜单栏高度：主屏顶部到 visibleFrame 顶部的距离。
    ///
    /// 必须用「frame 顶 − visibleFrame 顶」，不能直接拿两个高度相减：
    /// Dock 在底部时 visibleFrame 同时缩小了菜单栏和 Dock 两段，
    /// 高度差 = 菜单栏 + Dock（本机实测 103pt），护栏直接把它拒掉、
    /// 返回 None，刘海左右分列布局（notch_slots）就永远不会生效，
    /// 常驻内容会整排压在摄像头刘海死区上。
    pub fn menu_bar_height() -> Option<f32> {
        unsafe {
            let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
            if screen.is_null() { return None }
            let frame: NSRect = msg_send![screen, frame];
            let visible: NSRect = msg_send![screen, visibleFrame];
            let h = (frame.origin.y + frame.size.height) - (visible.origin.y + visible.size.height);
            // 合理性护栏：菜单栏高度只可能是几十个点（刘海机 37-38 / 普通机 24-25）。
            (h > 10.0 && h < 64.0).then(|| h as f32)
        }
    }
}

/// macOS 系统菜单栏高度（逻辑点）：刘海机型约 37-38，普通机型约 24-25。
/// 取 `NSScreen.mainScreen` 的 frame 与 visibleFrame 高度差。
/// 岛体高度必须与菜单栏完全一致，否则矮一节、菜单栏下缘的部分区域点不到。
/// 测试构建返回 None（调用方退回 BAR_H 兜底值，与 notch_layout 同一隔离策略）。
#[cfg(all(target_os = "macos", not(test)))]
pub fn menu_bar_height() -> Option<f32> { imp::menu_bar_height() }

#[cfg(not(all(target_os = "macos", not(test))))]
pub fn menu_bar_height() -> Option<f32> { None }

/// 主窗口左上角的全局逻辑坐标（顶层转发，实现见 `imp`）。
#[cfg(target_os = "macos")]
pub fn window_origin_top_left() -> Option<(f32, f32)> { imp::window_origin_top_left() }

#[cfg(not(target_os = "macos"))]
pub fn window_origin_top_left() -> Option<(f32, f32)> { None }

/// 主屏的边界（逻辑点，egui 左上原点）。
/// Windows 端有 `display_layout::connected()`，macOS 一直缺——所以
/// 「水平位置 / 左右停靠」在 Mac 上从未生效（窗口永远启动时居中一次）。
///
/// 测试构建返回 None：上游 600KB 录制/像素基线以「无屏幕信息」为基准，
/// 真实屏幕矩形会让视口 640×270 的浮层避让断言随硬件变化（同 `notch_layout`）。
#[cfg(all(target_os = "macos", not(test)))]
pub fn main_display_bounds() -> Option<crate::display_layout::Bounds> {
    imp::main_display_bounds()
}

#[cfg(not(all(target_os = "macos", not(test))))]
pub fn main_display_bounds() -> Option<crate::display_layout::Bounds> { None }

/// 窗口所在屏幕换算成窗口本地的逻辑矩形（egui 左上原点）。
/// 供引擎的拖动换边、浮层避让使用（`Env.screen`）。测试构建同样返回 None。
#[cfg(all(target_os = "macos", not(test)))]
pub fn screen_rect_local() -> Option<eframe::egui::Rect> {
    imp::screen_rect_local()
}

#[cfg(not(all(target_os = "macos", not(test))))]
pub fn screen_rect_local() -> Option<eframe::egui::Rect> { None }
