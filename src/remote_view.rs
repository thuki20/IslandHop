//! IslandHop 远程桌面：把对端整屏拉到本机显示，并直接键鼠操控。
//!
//! OpenHop 引擎（openhop-core::extras::Hub）里已有完整的远程画面链路：
//! SCREEN 整屏采集、WinFrame/WinAck 帧协议、JPEG 增量 patch、48KB bulk 通道，
//! 以及 WinInput → 对端注入。本模块补齐应用层：后台收帧解码、画面合成、
//! egui 渲染与输入回传。
//!
//! 设计约束：
//! - Hub 是 Arc<Hub>（内部全 Mutex/Condvar，Send+Sync），可安全 clone 到后台线程；
//!   Engine 本身 !Send，留在 cross_screen 的 thread_local 里，不跨线程。
//! - frame() 是阻塞等待，必须在后台线程调用，绝不能阻塞 egui 主线程。
//! - 性能：画面用 Arc<Vec<u8>> 共享，后台回收上一帧缓冲；GPU 纹理原地 set()
//!   而不是每帧新建，避免 1080p 下每帧 8MB 复制 + 纹理重建造成的卡顿。

use eframe::egui::{
    Color32, ColorImage, Context, Event, Id, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect,
    RichText, TextureOptions, Vec2,
};
use openhop_core::extras::{FrameWait, Hub};
use openhop_core::protocol::{MouseButton, WinEvent};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 一帧画面合成后的 RGBA 画布。共享引用，避免每帧 8MB 拷贝。
#[derive(Clone)]
pub struct Picture {
    pub w: usize,
    pub h: usize,
    pub rgba: Arc<Vec<u8>>,
    pub generation: u64,
}

/// 后台线程与主线程共享的查看器状态。
pub struct ViewerState {
    pub picture: Option<Picture>,
    pub paused: Option<String>,
    pub closed: bool,
    pub frames_received: u64,
    /// 主线程上传纹理后归还的 RGBA 缓冲，接收线程下一轮取回复用（零拷贝）。
    pub recycled: Option<Vec<u8>>,
    /// 会话创建时间：长时间无帧时给出明确提示，避免永远停在「连接中…」而不知道原因。
    pub created: Instant,
}

impl Default for ViewerState {
    fn default() -> Self {
        Self { picture: None, paused: None, closed: false, frames_received: 0, recycled: None, created: Instant::now() }
    }
}

/// 一个正在进行的画面流。drop 时通知后台线程退出并关闭流。
pub struct ViewerSession {
    stream: u64,
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<ViewerState>>,
    last_remote: Arc<AtomicU64>,
    /// 主线程侧的指针移动节流（对端 core 里还有一层 50ms 注入节流）。
    input_at: Arc<Mutex<Instant>>,
}

impl ViewerSession {
    pub fn stream(&self) -> u64 {
        self.stream
    }

    /// 主线程取走当前画面。Arc 克隆是浅拷贝。
    pub fn picture(&self) -> Option<Picture> {
        self.state.lock().ok().and_then(|s| s.picture.clone())
    }

    pub fn status(&self) -> (Option<String>, bool, u64, std::time::Duration) {
        self.state.lock().map(|s| (s.paused.clone(), s.closed, s.frames_received, s.created.elapsed()))
            .unwrap_or((None, false, 0, std::time::Duration::ZERO))
    }
}

impl Drop for ViewerSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// 启动一个画面流的后台接收：阻塞取帧 → 解码 JPEG patch → 合成 RGBA。
pub fn spawn_receiver(hub: Arc<Hub>, stream: u64, width: usize, height: usize) -> ViewerSession {
    let stop = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(ViewerState::default()));
    let last_remote = Arc::new(AtomicU64::new(0));
    let input_at = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1)));
    let (stop_t, state_t, last_t) = (stop.clone(), state.clone(), last_remote.clone());

    let _ = std::thread::Builder::new()
        .name(format!("islandhop-view-{stream}"))
        .spawn(move || {
            // 双缓冲：接收线程持有一块可写画布，主线程用完后通过 state 归还。
            // 关键不变量：交给 egui 的 rgba 长度必须恒等于 w*h*4，否则
            // ColorImage::from_rgba_unmultiplied 的 assert 会打崩 UI 线程（表现为卡死）。
            // v0.6.94 的旧实现把画布 replace 成空 Vec 再靠 try_unwrap 回收，
            // 主线程每帧持旧帧导致 unwrap 必败 → 第二帧起画布是空的 → 卡死。
            let mut canvas = vec![0u8; width * height * 4];
            let mut gen = 0u64;
            while !stop_t.load(Ordering::Acquire) {
                // 取回主线程已经替换掉的上一帧缓冲，复用其内存。
                if let Ok(mut s) = state_t.lock() {
                    if let Some(back) = s.recycled.take() {
                        if back.len() == width * height * 4 {
                            canvas = back;
                        }
                    }
                }
                let after = last_t.load(Ordering::Acquire);
                match hub.frame(stream, after, Duration::from_millis(250)) {
                    FrameWait::Frames(frames) => {
                        for frame in frames {
                            gen += 1;
                            last_t.store(frame.remote, Ordering::Release);
                            // 全帧 patch（覆盖整张）时重置画布，兼容分辨率变化。
                            if frame.patches.iter().any(|p| p.x == 0 && p.y == 0 && p.w == frame.w && p.h == frame.h) {
                                if canvas.len() != frame.w as usize * frame.h as usize * 4 {
                                    canvas = vec![0u8; frame.w as usize * frame.h as usize * 4];
                                } else {
                                    canvas.fill(0);
                                }
                            }
                            // 安全护栏：画布尺寸异常（patch 越界/分辨率变化丢帧）时重建，
                            // 保证交给 egui 的 rgba 长度恒等于 w*h*4。
                            if canvas.len() != width * height * 4 {
                                canvas = vec![0u8; width * height * 4];
                            }
                            for p in frame.patches.iter() {
                                composite_patch(&mut canvas, width, height, p.x, p.y, p.w, p.h, &p.jpeg);
                            }
                        }
                        // 画布本体 move 进 Arc：零拷贝。用完后主线程把旧缓冲放回
                        // ViewerState::recycled，下一轮这里取回复用，稳态零分配。
                        let shared = Arc::new(std::mem::replace(&mut canvas, Vec::new()));
                        if let Ok(mut s) = state_t.lock() {
                            s.frames_received += 1;
                            s.paused = None;
                            // 旧帧若已无人引用（主线程上一帧已画完），回收它的内存给下一轮用。
                            if let Some(old) = s.picture.take() {
                                if Arc::strong_count(&old.rgba) == 1 {
                                    if let Ok(buf) = Arc::try_unwrap(old.rgba) {
                                        s.recycled = Some(buf);
                                    }
                                }
                            }
                            // canvas 本体已 move 给 shared，必须立即补一块，绝不发布空画布。
                            if canvas.is_empty() {
                                canvas = s.recycled.take()
                                    .filter(|b| b.len() == width * height * 4)
                                    .unwrap_or_else(|| vec![0u8; width * height * 4]);
                            }
                            s.picture = Some(Picture {
                                w: width, h: height, rgba: shared, generation: gen,
                            });
                        }
                    }
                    FrameWait::Paused(reason) => {
                        if let Ok(mut s) = state_t.lock() { s.paused = Some(reason); }
                    }
                    FrameWait::Closed => {
                        if let Ok(mut s) = state_t.lock() { s.closed = true; }
                        break;
                    }
                    FrameWait::Timeout => {}
                }
            }
            // 线程退出时关闭引擎里的流，通知对端停止推画面。
            hub.close(stream);
        });

    ViewerSession { stream, stop, state, last_remote, input_at }
}

/// 把一个 JPEG patch 解码后合成到 RGBA 画布的 (x,y,w,h) 区域。
fn composite_patch(canvas: &mut [u8], cw: usize, ch: usize, x: u32, y: u32, w: u32, h: u32, jpeg: &[u8]) -> bool {
    if w == 0 || h == 0 {
        return false;
    }
    let decoded = (|| -> Option<image::RgbaImage> {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(jpeg)).with_guessed_format().ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        Some(reader.decode().ok()?.to_rgba8())
    })();
    let Some(img) = decoded else { return false; };
    let (x, y, w, h) = (x as usize, y as usize, w as usize, h as usize);
    if x >= cw || y >= ch {
        return false;
    }
    let copy_w = w.min(cw - x).min(img.width() as usize);
    let copy_h = h.min(ch - y).min(img.height() as usize);
    let mut placed = false;
    for row in 0..copy_h {
        let src = row * img.width() as usize * 4;
        let dst = ((y + row) * cw + x) * 4;
        let len = copy_w * 4;
        if dst + len <= canvas.len() && src + len <= img.as_raw().len() {
            canvas[dst..dst + len].copy_from_slice(&img.as_raw()[src..src + len]);
            placed = true;
        }
    }
    placed
}

/// 画一路画面的内容区（纹理或等待提示）。返回图片所在矩形供输入映射。
/// 抽成独立函数供独立视口与降级 egui 窗口共用。
fn paint_view_body(
    ui: &mut eframe::egui::Ui,
    tex_id: Option<eframe::egui::TextureId>,
    w: usize,
    h: usize,
    paused: Option<&str>,
    waited: Duration,
    focused: bool,
    mut out_rect: Option<&mut Option<Rect>>,
) {
    if let Some(tex) = tex_id {
        let available = ui.available_size();
        let scale = (available.x / w.max(1) as f32).min(available.y / h.max(1) as f32).min(1.0);
        let size = Vec2::new(w as f32 * scale, h as f32 * scale);
        let resp = ui.add(
            eframe::egui::Image::new((tex, size))
                .sense(eframe::egui::Sense::click_and_drag())
                .fit_to_exact_size(size),
        );
        if let Some(slot) = out_rect.as_deref_mut() { *slot = Some(resp.rect); }
        if focused {
            ui.label(RichText::new("已连接键鼠 · 点画面外其他窗口即可切回本机").size(10.0).weak());
        }
    } else {
        ui.centered_and_justified(|ui| {
            if let Some(reason) = paused.filter(|r| !r.is_empty()) {
                ui.vertical(|ui| {
                    ui.label(format!("⏳ {reason}"));
                    ui.label(RichText::new("在对端 IslandHop 设置里勾选「允许对端控制本机屏幕」后即可观看").size(11.0).weak());
                });
            } else if waited >= Duration::from_secs(5) {
                ui.vertical(|ui| {
                    ui.label("⏳ 连接超时，对端没有推送画面");
                    ui.label(RichText::new("请确认：对端 IslandHop 在线 · 已勾选「允许对端控制本机屏幕」· 两端已配对").size(11.0).weak());
                });
            } else {
                ui.label("正在接收对端画面…");
            }
        });
    }
}

/// 连接等待提示的纯逻辑：一帧未到时按「对端暂停原因 → 超时引导授权 → 连接中」给出可操作提示。
/// 有帧后不再显示连接提示。抽成纯函数便于确定性单测。
fn connect_hint_text(frames: u64, paused: Option<&str>, waited: Duration) -> String {
    if frames != 0 {
        return String::new();
    }
    if let Some(reason) = paused.filter(|r| !r.is_empty()) {
        return format!(" · {reason}");
    }
    if waited >= Duration::from_secs(5) {
        return " · 连接超时 · 请在对端设置勾选「允许对端控制本机屏幕」".to_string();
    }
    " · 连接中…".to_string()
}

/// Picture 的像素长度必须与声明尺寸一致。egui 的 ColorImage 构造在此处有 assert，
/// 不一致会 panic 并打崩 UI 线程——所有交给 egui 的画面都必须先过这一关。
fn picture_valid(pic: &Picture) -> bool {
    pic.w > 0 && pic.h > 0 && pic.rgba.len() == pic.w * pic.h * 4
}

pub fn to_color_image(pic: &Picture) -> ColorImage {
    debug_assert!(picture_valid(pic), "Picture 像素长度与尺寸不符，交给 egui 会 panic");
    ColorImage::from_rgba_unmultiplied([pic.w, pic.h], &pic.rgba[..])
}

/// 修饰键（HID page 0x07）按下/释放变化。egui 的 Key 枚举没有 Shift/Ctrl 本身，
/// 只能从 Modifiers 差分得到；漏发会导致大写/快捷键失效。
fn modifier_changes(old: Modifiers, new: Modifiers) -> Vec<(u16, bool)> {
    let mut out = Vec::new();
    if old.ctrl != new.ctrl { out.push((0xE0, new.ctrl)); }
    if old.shift != new.shift { out.push((0xE1, new.shift)); }
    if old.alt != new.alt { out.push((0xE2, new.alt)); }
    if old.command != new.command { out.push((0xE3, new.command)); }
    out
}

/// egui 逻辑键 → USB HID usage id（与 openhop-core::keys 的线格式一致）。
/// 带 Shift 的符号落在基础键上，修饰键由 modifier_changes 负责。
fn key_hid(key: eframe::egui::Key) -> Option<u16> {
    use eframe::egui::Key as K;
    let k = key as u16;
    // egui::Key 没有 repr，不能做模式区间；用判别值区间覆盖连续的三段。
    if (K::A as u16..=K::Z as u16).contains(&k) { return Some(0x04 + k - K::A as u16); }
    if (K::Num1 as u16..=K::Num9 as u16).contains(&k) { return Some(0x1E + k - K::Num1 as u16); }
    if (K::F1 as u16..=K::F24 as u16).contains(&k) { return Some(0x3A + k - K::F1 as u16); }
    let hid = match key {
        K::Num0 => 0x27,
        K::Enter => 0x28, K::Escape => 0x29, K::Backspace => 0x2A, K::Tab => 0x2B,
        K::Space => 0x2C,
        K::Minus => 0x2D, K::Equals | K::Plus => 0x2E,
        K::OpenBracket | K::OpenCurlyBracket => 0x2F,
        K::CloseBracket | K::CloseCurlyBracket => 0x30,
        K::Backslash | K::Pipe => 0x31,
        K::Semicolon | K::Colon => 0x33,
        K::Quote => 0x34, K::Backtick => 0x35,
        K::Comma => 0x36, K::Period => 0x37,
        K::Slash | K::Questionmark => 0x38,
        K::Exclamationmark => 0x1E,
        K::Insert => 0x49, K::Home => 0x4A, K::PageUp => 0x4B, K::Delete => 0x4C,
        K::End => 0x4D, K::PageDown => 0x4E,
        K::ArrowRight => 0x4F, K::ArrowLeft => 0x50, K::ArrowDown => 0x51, K::ArrowUp => 0x52,
        // Copy/Cut/Paste 是编辑命令而非物理键，输入法层已处理，不往对端发。
        _ => return None,
    };
    Some(hid)
}

fn mouse_hid(button: PointerButton) -> Option<MouseButton> {
    match button {
        PointerButton::Primary => Some(MouseButton::Left),
        PointerButton::Secondary => Some(MouseButton::Right),
        PointerButton::Middle => Some(MouseButton::Middle),
        PointerButton::Extra1 => Some(MouseButton::Back),
        PointerButton::Extra2 => Some(MouseButton::Forward),
    }
}

/// 管理本机正在看的画面流 + egui 窗口。放在 cross_screen 的 thread_local 里。
pub struct ViewerHub {
    sessions: Vec<ViewerSession>,
    /// stream → 对端名 / 标题，供窗口标题与状态显示。
    titles: std::collections::HashMap<u64, (String, String)>,
    /// stream → (generation, w, h, 纹理)。generation 不变时跳过上传，
    /// 尺寸变化才重建纹理；避免每帧把同一帧 8MB 重新传给 GPU。
    textures: std::collections::HashMap<u64, (u64, usize, usize, eframe::egui::TextureHandle)>,
    last_notice: Option<String>,
    /// 当前接受键鼠输入的画面流（点击画面取得焦点）。
    focused: Option<u64>,
    mods: Modifiers,
}

impl Default for ViewerHub {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            titles: std::collections::HashMap::new(),
            textures: std::collections::HashMap::new(),
            last_notice: None,
            focused: None,
            mods: Modifiers::NONE,
        }
    }
}

impl ViewerHub {
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn count(&self) -> usize {
        self.sessions.len()
    }

    pub fn take_notice(&mut self) -> Option<String> {
        self.last_notice.take()
    }

    /// 发起观看对端整屏。hub.open() 内部发 WinOpen 并建 viewer 流。
    pub fn open_screen(&mut self, hub: &Arc<Hub>, peer: &str) -> Result<(), String> {
        let (w, h) = hub.status_of(peer)
            .and_then(|s| {
                let x2 = s.monitors.iter().map(|m| m.x + m.w).max();
                let y2 = s.monitors.iter().map(|m| m.y + m.h).max();
                match (x2, y2) { (Some(x), Some(y)) if x > 0 && y > 0 => Some((x as usize, y as usize)), _ => None }
            })
            .unwrap_or((1280, 800));
        let w = w.min(3840);
        let h = h.min(2160);
        let Some(stream) = hub.open(peer, openhop_core::wins::SCREEN, None, false) else {
            return Err(format!("无法连接 {peer} 的屏幕（可能未在线或未授权）"));
        };
        self.open_stream(hub, stream, peer, format!("{peer} 的屏幕"), w, h);
        Ok(())
    }

    /// 接入一个已经由 hub.open() 建好的流（应用列表里打开某个窗口也走这里）。
    pub fn open_stream(&mut self, hub: &Arc<Hub>, stream: u64, peer: &str, title: String, w: usize, h: usize) {
        let (w, h) = (w.clamp(1, 3840), h.clamp(1, 2160));
        let session = spawn_receiver(hub.clone(), stream, w, h);
        self.titles.insert(stream, (peer.to_string(), title));
        self.sessions.push(session);
        self.focused = Some(stream);
    }

    /// 关闭一个画面流。
    pub fn close(&mut self, stream: u64, hub: &Arc<Hub>) {
        self.sessions.retain(|s| s.stream() != stream);
        self.titles.remove(&stream);
        self.textures.remove(&stream);
        if self.focused == Some(stream) { self.focused = None; }
        hub.close(stream);
    }

    /// 处理引擎推来的 UI 事件：对端请求看本机屏幕、关闭画面、文本通知。
    pub fn poll_events(&mut self, hub: &Arc<Hub>, allow_control: bool) {
        let events = hub.take_ui();
        for ev in events {
            match ev {
                openhop_core::extras::UiEvent::HelpAsk { id, from, screen } => {
                    // 本机勾选了「允许对端控制本机屏幕」才接受，避免被静默观看。
                    hub.help_answer(id, allow_control);
                    if !allow_control {
                        self.last_notice = Some(if screen {
                            format!("{from} 请求查看本机屏幕，已拒绝（设置里可开启「允许对端控制本机屏幕」）")
                        } else {
                            format!("{from} 请求查看本机窗口，已拒绝")
                        });
                    } else {
                        self.last_notice = Some(format!("{from} 正在查看本机{}", if screen { "屏幕" } else { "窗口" }));
                    }
                }
                openhop_core::extras::UiEvent::CloseViewer { stream } => {
                    self.sessions.retain(|s| s.stream() != stream);
                    self.titles.remove(&stream);
                    self.textures.remove(&stream);
                    if self.focused == Some(stream) { self.focused = None; }
                }
                openhop_core::extras::UiEvent::Notice { title, body, .. } => {
                    self.last_notice = Some(format!("{title}：{body}"));
                }
                _ => {}
            }
        }
    }

    /// 每个画面流注册为一个独立 OS 窗口（deferred viewport）。
    /// 主视口就是灵动岛（几百像素），egui::Window 会被 clamp 在里面，画面根本没地方显示；
    /// 独立视口才有真正的桌面窗口尺寸，且输入事件按视口隔离，不会被岛 UI 抢走。
    pub fn paint(&mut self, ctx: &Context, hub: Option<&Arc<Hub>>, self_arc: Arc<Mutex<ViewerHub>>) {
        use eframe::egui::{ViewportBuilder, ViewportId};

        // 先收集本帧的标题/尺寸快照（不在回调里访问 self，避免重复上锁与借用）。
        let infos: Vec<(u64, String, usize, usize)> = self
            .sessions
            .iter()
            .filter(|s| !s.status().1)
            .map(|s| {
                let title = self.titles.get(&s.stream())
                    .map(|(_, t)| t.clone())
                    .unwrap_or_else(|| format!("屏幕 {}", s.stream()));
                let (w, h) = s.picture().map(|p| (p.w, p.h)).unwrap_or((1280, 800));
                (s.stream(), title, w, h)
            })
            .collect();

        let hub = hub.cloned();
        for (stream, title, w, h) in infos {
            let viewer = self_arc.clone();
            let hub = hub.clone();
            let win_w = w.min(1920).max(480) as f32;
            let win_h = h.min(1080).max(320) as f32;
            let builder = ViewportBuilder::default()
                .with_title(&title)
                .with_inner_size([win_w, win_h])
                .with_resizable(true)
                .with_taskbar(true);
            let vid = ViewportId::from_hash_of(format!("islandhop-remote-{stream}"));
            ctx.show_viewport_deferred(vid, builder, move |vp_ctx, class| {
                if class == eframe::egui::ViewportClass::Embedded {
                    // 后端不支持多视口时退回旧行为：在主视口内画一个 egui 窗口。
                    if let Ok(mut v) = viewer.lock() {
                        v.paint_embedded(vp_ctx, stream, hub.as_ref());
                    }
                    return;
                }
                // 用户点了窗口关闭按钮 → 关闭这一路流。
                let close = vp_ctx.input(|i| i.viewport().close_requested());
                let mut v = match viewer.lock() {
                    Ok(v) => v,
                    Err(e) => e.into_inner(),
                };
                if close {
                    if let Some(hub) = hub.as_ref() { v.close(stream, hub); }
                    else { v.drop_stream(stream); }
                    return;
                }
                v.paint_one(vp_ctx, stream, hub.as_ref());
            });
        }
    }

    /// 后端不支持独立视口时的降级：在主视口内画 egui 窗口（旧行为）。
    fn paint_embedded(&mut self, ctx: &Context, stream: u64, hub: Option<&Arc<Hub>>) {
        let title = self.titles.get(&stream)
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| format!("屏幕 {stream}"));
        let (paused, closed, frames, waited) = self.sessions.iter()
            .find(|s| s.stream() == stream)
            .map(|s| s.status())
            .unwrap_or((None, true, 0, std::time::Duration::ZERO));
        if closed { return; }
        let pic = self.sessions.iter().find(|s| s.stream() == stream).and_then(|s| s.picture());
        let mut open = true;
        let tex_id = self.upload_texture(ctx, stream, pic.as_ref());
        let (w, h) = pic.as_ref().map(|p| (p.w, p.h)).unwrap_or((1280, 800));
        let hint = connect_hint_text(frames, paused.as_deref(), waited);
        let controllable = hub.is_some();
        eframe::egui::Window::new(format!("{title}{hint}{}", if controllable { " · 可操控" } else { "" }))
            .id(Id::new(("remote-view-embedded", stream)))
            .open(&mut open)
            .resizable(true)
            .default_size([(w.min(1024)) as f32, (h.min(720)) as f32])
            .show(ctx, |ui| {
                paint_view_body(ui, tex_id, w, h, paused.as_deref(), waited,
                    self.focused == Some(stream), None);
            });
        if !open {
            if let Some(hub) = hub { self.close(stream, hub); }
            else { self.drop_stream(stream); }
        }
    }

    /// 纹理上传：尺寸变化才重建，generation 变化才 set，避免每帧重传 8MB。
    fn upload_texture(&mut self, ctx: &Context, stream: u64, pic: Option<&Picture>) -> Option<eframe::egui::TextureId> {
        let pic = pic?;
        if !picture_valid(pic) { return None; }
        let (need_new_tex, need_upload) = match self.textures.get(&stream) {
            Some((gen, w, h, _)) => (*w != pic.w || *h != pic.h, *gen != pic.generation),
            None => (true, true),
        };
        if need_new_tex {
            let tex = ctx.load_texture(format!("remote-view-{stream}"), to_color_image(pic), TextureOptions::LINEAR);
            self.textures.insert(stream, (pic.generation, pic.w, pic.h, tex));
        } else if need_upload {
            if let Some((gen, _, _, tex)) = self.textures.get_mut(&stream) {
                tex.set(to_color_image(pic), TextureOptions::LINEAR);
                *gen = pic.generation;
            }
        }
        self.textures.get(&stream).map(|(_, _, _, t)| t.id())
    }

    fn drop_stream(&mut self, stream: u64) {
        self.sessions.retain(|s| s.stream() != stream);
        self.titles.remove(&stream);
        self.textures.remove(&stream);
        if self.focused == Some(stream) { self.focused = None; }
    }

    /// 在独立视口里渲染一路画面，并处理该视口的键鼠输入。
    fn paint_one(&mut self, ctx: &Context, stream: u64, hub: Option<&Arc<Hub>>) {
        let (paused, closed, frames, waited) = self.sessions.iter()
            .find(|s| s.stream() == stream)
            .map(|s| s.status())
            .unwrap_or((None, true, 0, std::time::Duration::ZERO));
        if closed { return; }
        let pic = self.sessions.iter().find(|s| s.stream() == stream).and_then(|s| s.picture());
        let (w, h) = pic.as_ref().map(|p| (p.w, p.h)).unwrap_or((1280, 800));
        let tex_id = self.upload_texture(ctx, stream, pic.as_ref());
        let mut focus = self.focused;

        eframe::egui::CentralPanel::default().show(ctx, |ui| {
            let mut input_rect = None;
            paint_view_body(ui, tex_id, w, h, paused.as_deref(), waited,
                focus == Some(stream), Some(&mut input_rect));

            // 用本视口自己的输入（ctx.input 在 deferred 回调里读到的是当前视口），
            // 因此岛 UI 的点击不会混进来，无需再判断「点在其他窗口」。
            let clicked_image = input_rect.map(|r| {
                ui.input(|i| i.pointer.button_down(eframe::egui::PointerButton::Primary)
                    && i.pointer.latest_pos().is_some_and(|p| r.contains(p)))
            }).unwrap_or(false);
            if clicked_image { focus = Some(stream); }

            if let (Some(rect), Some(hub)) = (input_rect, hub) {
                let input_at = self.sessions.iter().find(|s| s.stream() == stream).map(|s| s.input_at.clone());
                if let Some(input_at) = input_at {
                    self.send_input(ctx, hub, stream, rect, w, h, &input_at);
                }
            }
        });
        self.focused = focus.filter(|s| self.sessions.iter().any(|v| v.stream() == *s));
    }

    /// 把本机键鼠事件送到对端。只处理聚焦的那一路画面。
    fn send_input(&mut self, ctx: &Context, hub: &Arc<Hub>, stream: u64, rect: Rect, w: usize, h: usize, input_at: &Arc<Mutex<Instant>>) {
        let (mods, events, pointer) = ctx.input(|i| (
            i.modifiers,
            i.events.clone(),
            i.pointer.latest_pos(),
        ));
        // egui 的 Key 枚举不含 Shift/Ctrl 本身，必须靠 Modifiers 差分补发。
        for (key, down) in modifier_changes(self.mods, mods) {
            hub.input(stream, WinEvent::Key { key, down });
        }
        self.mods = mods;

        let to_stream = |pos: Pos2| -> (i32, i32) {
            let x = ((pos.x - rect.min.x) / rect.width().max(1.0) * w as f32).round() as i32;
            let y = ((pos.y - rect.min.y) / rect.height().max(1.0) * h as f32).round() as i32;
            (x.clamp(0, w as i32 - 1), y.clamp(0, h as i32 - 1))
        };
        let inside = |pos: Pos2| rect.contains(pos);
        let mut consumed_key = false;
        let mut release_focus = false;

        for ev in &events {
            match ev {
                Event::PointerButton { pos, button, pressed, .. } if inside(*pos) => {
                    if let Some(b) = mouse_hid(*button) {
                        let (x, y) = to_stream(*pos);
                        hub.input(stream, WinEvent::Button { button: b, down: *pressed, x, y });
                    }
                }
                Event::MouseWheel { delta, unit, .. } if pointer.is_some_and(inside) => {
                    // core 侧按像素注入；行/页单位粗换算，方向保持一致。
                    let k = match unit { MouseWheelUnit::Line => 32.0, MouseWheelUnit::Page => 600.0, _ => 1.0 };
                    let (x, y) = to_stream(pointer.unwrap());
                    let dx = (delta.x * k).round() as i32;
                    let dy = (delta.y * k).round() as i32;
                    if dx != 0 || dy != 0 {
                        hub.input(stream, WinEvent::Wheel { dx, dy, x, y });
                    }
                }
                Event::Key { key, physical_key, pressed, repeat, .. } => {
                    // Esc 仍送给对端（很多应用用它退出全屏），同时把控制交还本机。
                    if *pressed && *key == eframe::egui::Key::Escape { release_focus = true; }
                    if !*repeat {
                        if let Some(hid) = key_hid(physical_key.unwrap_or(*key)) {
                            hub.input(stream, WinEvent::Key { key: hid, down: *pressed });
                        }
                    }
                    consumed_key = true;
                }
                // 文本事件给对端用途有限（物理键已发），别让 egui 输入框抢走。
                Event::Text(_) | Event::Ime(_) => consumed_key = true,
                _ => {}
            }
        }
        if consumed_key {
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, Event::Key { .. } | Event::Text(_) | Event::Ime(_))));
        }
        if release_focus { self.focused = None; }

        // 指针移动：24ms 节流；拖动时也不放宽——对端 core 已有 50ms 注入节流，
        // 这里再密只会堆网络包，反而让按键/点击排队。
        if let Some(pos) = pointer {
            if inside(pos) || ctx.input(|i| i.pointer.button_down(PointerButton::Primary)
                || i.pointer.button_down(PointerButton::Secondary)) {
                let mut at = input_at.lock().unwrap_or_else(|e| e.into_inner());
                if at.elapsed() >= Duration::from_millis(24) {
                    let (x, y) = to_stream(pos);
                    hub.input(stream, WinEvent::Move { x, y });
                    *at = Instant::now();
                }
            }
        }
    }
}

/// macOS 屏幕录制权限引导文案。
pub fn screen_record_permission_hint() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "要让别的电脑看到本机屏幕，需在 系统设置 → 隐私与安全性 → 屏幕录制 中允许 IslandHop。\
         单向观看对端不需要此权限。"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "Windows 端整屏采集无需额外权限。"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_ignores_empty_and_out_of_bounds() {
        let mut canvas = vec![0u8; 8 * 8 * 4];
        assert!(!composite_patch(&mut canvas, 8, 8, 0, 0, 0, 0, b""), "空 patch 应忽略");
        assert!(!composite_patch(&mut canvas, 8, 8, 100, 100, 2, 2, b""), "越界 patch 应忽略");
    }

    #[test]
    fn color_image_from_picture_keeps_dimensions() {
        let pic = Picture { w: 3, h: 2, rgba: Arc::new(vec![255; 3 * 2 * 4]), generation: 1 };
        let img = to_color_image(&pic);
        assert_eq!(img.size, [3, 2]);
        assert_eq!(img.pixels.len(), 6);
    }

    #[test]
    fn viewer_state_defaults_are_empty() {
        let s = ViewerState::default();
        assert!(s.picture.is_none() && s.paused.is_none() && !s.closed && s.frames_received == 0 && s.recycled.is_none());
    }

    /// 授权/超时提示回归：连不上时必须给出可操作指引，不能永远只显示「连接中…」。
    #[test]
    fn connect_hint_explains_why_no_frames_arrive() {
        // 已有帧：不显示任何连接提示。
        assert_eq!(connect_hint_text(3, Some("正在等待接受"), Duration::from_secs(60)), "");
        // 对端回了暂停原因：原样显示（如等待授权）。
        assert_eq!(connect_hint_text(0, Some("正在等待 MacBook-Pro 接受"), Duration::from_secs(1)),
                   " · 正在等待 MacBook-Pro 接受");
        // 空原因不当作暂停提示，按时间长短走连接中/超时。
        assert_eq!(connect_hint_text(0, Some(""), Duration::from_secs(1)), " · 连接中…");
        assert_eq!(connect_hint_text(0, None, Duration::from_secs(4)), " · 连接中…");
        let timeout = connect_hint_text(0, None, Duration::from_secs(5));
        assert!(timeout.contains("连接超时") && timeout.contains("允许对端控制本机屏幕"),
                "超时应引导去对端勾选授权，实际: {timeout}");
    }

    /// v0.6.94 卡死回归：尺寸声明正常但像素为空的 Picture 必须被判为无效，
    /// 绝不能送进 ColorImage::from_rgba_unmultiplied（那里 assert 会打崩 UI 线程）。
    #[test]
    fn empty_or_mismatched_picture_is_rejected_before_egui() {
        let empty = Picture { w: 1920, h: 1080, rgba: Arc::new(Vec::new()), generation: 1 };
        assert!(!picture_valid(&empty), "空画布一律拒绝");
        let short = Picture { w: 4, h: 4, rgba: Arc::new(vec![0; 4 * 4 * 3]), generation: 1 };
        assert!(!picture_valid(&short), "像素长度不足一律拒绝");
        let zero = Picture { w: 0, h: 0, rgba: Arc::new(Vec::new()), generation: 1 };
        assert!(!picture_valid(&zero), "零尺寸一律拒绝");
        let good = Picture { w: 2, h: 2, rgba: Arc::new(vec![255; 16]), generation: 1 };
        assert!(picture_valid(&good), "正常画面必须通过");
    }

    #[test]
    fn modifier_changes_follow_the_modifier_state() {
        let a = modifier_changes(Modifiers::NONE, Modifiers::SHIFT);
        assert_eq!(a, vec![(0xE1, true)]);
        let b = modifier_changes(Modifiers::SHIFT, Modifiers::NONE);
        assert_eq!(b, vec![(0xE1, false)]);
        let c = modifier_changes(Modifiers::NONE, Modifiers::NONE);
        assert!(c.is_empty());
    }

    #[test]
    fn egui_keys_map_to_hid_usages() {
        assert_eq!(key_hid(eframe::egui::Key::A), Some(0x04));
        assert_eq!(key_hid(eframe::egui::Key::Z), Some(0x1D));
        assert_eq!(key_hid(eframe::egui::Key::Num1), Some(0x1E));
        assert_eq!(key_hid(eframe::egui::Key::Num0), Some(0x27));
        assert_eq!(key_hid(eframe::egui::Key::Enter), Some(0x28));
        assert_eq!(key_hid(eframe::egui::Key::ArrowUp), Some(0x52));
        assert_eq!(key_hid(eframe::egui::Key::F12), Some(0x45));
        assert_eq!(key_hid(eframe::egui::Key::Copy), None);
    }
}
