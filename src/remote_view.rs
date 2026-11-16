//! IslandHop 屏幕镜像（最小原型）：把对端整屏画面拉到本机显示。
//!
//! OpenHop 引擎（openhop-core::extras::Hub）里已有完整的远程画面链路：
//! SCREEN 整屏采集、WinFrame/WinAck 帧协议、JPEG 增量 patch、48KB bulk 通道。
//! 本模块只补上应用层缺的那一半：后台收帧解码 + 画面合成 + 供 UI 渲染。
//!
//! 设计约束：
//! - Hub 是 Arc<Hub>（内部全 Mutex/Condvar，Send+Sync），可安全 clone 到后台线程；
//!   Engine 本身 !Send，留在 cross_screen 的 thread_local 里，不跨线程。
//! - frame() 是阻塞等待，必须在后台线程调用，绝不能阻塞 egui 主线程。
//! - 只做"单向看对端"：本机发起 open(对端, SCREEN)，不共享本机画面。

use eframe::egui::ColorImage;
use openhop_core::extras::{FrameWait, Hub};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 一帧画面合成后的 RGBA 画布。主线程据此生成 egui 纹理。
#[derive(Clone)]
pub struct Picture {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
    pub generation: u64,
}

/// 后台线程与主线程共享的查看器状态。
pub struct ViewerState {
    pub picture: Option<Picture>,
    pub paused: Option<String>,
    pub closed: bool,
    pub frames_received: u64,
}

impl Default for ViewerState {
    fn default() -> Self {
        Self { picture: None, paused: None, closed: false, frames_received: 0 }
    }
}

/// 一个正在进行的画面流。drop 时通知后台线程退出并关闭流。
pub struct ViewerSession {
    stream: u64,
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<ViewerState>>,
    last_remote: Arc<AtomicU64>,
}

impl ViewerSession {
    pub fn stream(&self) -> u64 {
        self.stream
    }

    /// 主线程取走当前画面（克隆）。有新帧才 generation 变化。
    pub fn picture(&self) -> Option<Picture> {
        self.state.lock().ok().and_then(|s| s.picture.clone())
    }

    pub fn status(&self) -> (Option<String>, bool, u64) {
        self.state.lock().map(|s| (s.paused.clone(), s.closed, s.frames_received))
            .unwrap_or((None, false, 0))
    }
}

impl Drop for ViewerSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// 启动一个画面流的后台接收：阻塞取帧 → 解码 JPEG patch → 合成 RGBA。
/// 调用前 hub.open() 已发出 WinOpen 并建立了 viewer 流。
pub fn spawn_receiver(hub: Arc<Hub>, stream: u64, width: usize, height: usize) -> ViewerSession {
    let stop = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(ViewerState::default()));
    let last_remote = Arc::new(AtomicU64::new(0));
    let (stop_t, state_t, last_t) = (stop.clone(), state.clone(), last_remote.clone());

    let _ = std::thread::Builder::new()
        .name(format!("islandhop-view-{stream}"))
        .spawn(move || {
            let mut canvas = vec![0u8; width * height * 4];
            let mut gen = 0u64;
            while !stop_t.load(Ordering::Acquire) {
                let after = last_t.load(Ordering::Acquire);
                match hub.frame(stream, after, Duration::from_millis(250)) {
                    FrameWait::Frames(frames) => {
                        for frame in frames {
                            gen += 1;
                            last_t.store(frame.remote, Ordering::Release);
                            // 全帧 patch（覆盖整张）时重新分配画布，兼容分辨率变化。
                            if frame.patches.iter().any(|p| p.x == 0 && p.y == 0 && p.w == frame.w && p.h == frame.h) {
                                canvas = vec![0u8; frame.w as usize * frame.h as usize * 4];
                            }
                            for p in frame.patches.iter() {
                                composite_patch(&mut canvas, width, height, p.x, p.y, p.w, p.h, &p.jpeg);
                            }
                        }
                        if let Ok(mut s) = state_t.lock() {
                            s.frames_received += 1;
                            s.paused = None;
                            s.picture = Some(Picture {
                                w: width, h: height, rgba: canvas.clone(), generation: gen,
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

    ViewerSession { stream, stop, state, last_remote }
}

/// 把一个 JPEG patch 解码后合成到 RGBA 画布的 (x,y,w,h) 区域。
/// 解码失败或越界坐标会被安全忽略，不会污染已有画面。
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

/// 把 Picture 转成 egui 纹理用的 ColorImage。
pub fn to_color_image(pic: &Picture) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([pic.w, pic.h], &pic.rgba)
}

/// 管理本机正在看的画面流 + egui 窗口。放在 cross_screen 的 thread_local 里
/// （ViewerSession 含线程句柄，不跨线程），由主线程驱动 poll / paint。
pub struct ViewerHub {
    sessions: Vec<ViewerSession>,
    /// stream → 对端名 / 标题，供窗口标题与状态显示。
    titles: std::collections::HashMap<u64, (String, String)>,
    /// stream → 纹理，按 generation 增量更新，避免每帧重建 GPU 纹理。
    textures: std::collections::HashMap<u64, (u64, eframe::egui::TextureHandle)>,
    /// 用户对"对端请求看本机屏幕"的统一处置（单向原型：默认拒绝并提示）。
    last_notice: Option<String>,
}

impl Default for ViewerHub {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            titles: std::collections::HashMap::new(),
            textures: std::collections::HashMap::new(),
            last_notice: None,
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
        // 限制原型画布上限，避免超大桌面分配过多内存。
        let w = w.min(3840);
        let h = h.min(2160);
        let Some(stream) = hub.open(peer, openhop_core::wins::SCREEN, None, false) else {
            return Err(format!("无法连接 {peer} 的屏幕（可能未在线或未授权）"));
        };
        let session = spawn_receiver(hub.clone(), stream, w, h);
        self.titles.insert(stream, (peer.to_string(), format!("{peer} 的屏幕")));
        self.sessions.push(session);
        Ok(())
    }

    /// 关闭一个画面流。
    pub fn close(&mut self, stream: u64, hub: &Arc<Hub>) {
        self.sessions.retain(|s| s.stream() != stream);
        self.titles.remove(&stream);
        self.textures.remove(&stream);
        hub.close(stream);
    }

    /// 处理引擎推来的 UI 事件：请求看本机屏幕（单向原型拒绝）、关闭画面、文本通知。
    pub fn poll_events(&mut self, hub: &Arc<Hub>) {
        let events = hub.take_ui();
        for ev in events {
            match ev {
                openhop_core::extras::UiEvent::HelpAsk { id, from, screen } => {
                    hub.help_answer(id, false);
                    self.last_notice = Some(if screen {
                        format!("{from} 请求查看本机屏幕，已拒绝（当前仅支持单向观看对端）")
                    } else {
                        format!("{from} 请求查看本机窗口，已拒绝")
                    });
                }
                openhop_core::extras::UiEvent::CloseViewer { stream } => {
                    self.sessions.retain(|s| s.stream() != stream);
                    self.titles.remove(&stream);
                    self.textures.remove(&stream);
                }
                openhop_core::extras::UiEvent::Notice { title, body, .. } => {
                    self.last_notice = Some(format!("{title}：{body}"));
                }
                _ => {}
            }
        }
    }

    /// 渲染所有画面窗口。每个窗口从共享状态取最新画面，按 generation 更新纹理。
    pub fn paint(&mut self, ctx: &eframe::egui::Context) {
        // 先收集本帧需要的 stream 与画面，避免同时持有 self 的可变借用。
        let snapshots: Vec<(u64, String, Option<Picture>, (Option<String>, bool, u64))> = self
            .sessions
            .iter()
            .map(|s| {
                let title = self.titles.get(&s.stream())
                    .map(|(_, t)| t.clone())
                    .unwrap_or_else(|| format!("屏幕 {}", s.stream()));
                (s.stream(), title, s.picture(), s.status())
            })
            .collect();

        for (stream, title, pic, (_paused, closed, frames)) in snapshots {
            if closed {
                continue;
            }
            let mut open = true;
            let tex_id = pic.as_ref().and_then(|pic| {
                let need_new = self.textures.get(&stream)
                    .is_none_or(|(gen, _)| *gen != pic.generation);
                if need_new {
                    let tex = ctx.load_texture(
                        format!("remote-view-{stream}"),
                        to_color_image(pic),
                        eframe::egui::TextureOptions::LINEAR,
                    );
                    self.textures.insert(stream, (pic.generation, tex));
                }
                self.textures.get(&stream).map(|(_, t)| t.id())
            });
            let (w, h) = pic.as_ref().map(|p| (p.w, p.h)).unwrap_or((640, 400));
            let window_title = format!("{title}{}", if frames == 0 { " · 连接中…" } else { "" });
            eframe::egui::Window::new(window_title)
                .id(eframe::egui::Id::new(("remote-view-window", stream)))
                .open(&mut open)
                .resizable(true)
                .default_size([(w.min(1024)) as f32, (h.min(720)) as f32])
                .show(ctx, |ui| {
                    if let Some(tex) = tex_id {
                        let available = ui.available_size();
                        let scale = (available.x / w as f32).min(available.y / h as f32).min(1.0);
                        let size = eframe::egui::vec2(w as f32 * scale, h as f32 * scale);
                        ui.centered_and_justified(|ui| {
                            ui.add(eframe::egui::Image::new((tex, size)));
                        });
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label("正在接收对端画面…");
                        });
                    }
                });
            if !open {
                // 用户点了窗口关闭按钮：这里只从 self 移除，实际 hub.close 需要 Arc<Hub>，
                // 由 close_closed_streams 在持有 hub 的地方完成。
                self.sessions.retain(|s| s.stream() != stream);
                self.textures.remove(&stream);
                self.titles.remove(&stream);
            }
        }
    }
}

/// macOS 屏幕录制权限引导文案。只读检测在 macos_sys 里做，这里给跨平台 UI 用。
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
        let pic = Picture { w: 3, h: 2, rgba: vec![255; 3 * 2 * 4], generation: 1 };
        let img = to_color_image(&pic);
        assert_eq!(img.size, [3, 2]);
        // egui 0.31 的 ColorImage::pixels 是 Color32 数组（每像素一个），3×2=6。
        assert_eq!(img.pixels.len(), 6);
    }

    #[test]
    fn viewer_state_defaults_are_empty() {
        let s = ViewerState::default();
        assert!(s.picture.is_none() && s.paused.is_none() && !s.closed && s.frames_received == 0);
    }
}
