//! IslandHop 的 OpenHop 跨屏 / 剪贴板同步功能。
//!
//! 这是**岛仔本体的一个设置分组**，不是独立窗口：面板渲染在岛仔原有的
//! 「设置」窗口里（`extras.rs` 的 `SETTINGS_GROUPS` 第 13 项），引擎在应用
//! 启动时按 `config.toml` 的 `enabled` 后台自启，用户不需要打开任何界面
//! 就能键鼠共享 / 剪贴板同步。
//!
//! 岛仔上游把页面数量硬编码成 `[bool; 9]` 并散落在大量 `match page` 分支里，
//! 硬插第 10 个功能页会牵动设置持久化与录制测试，因此这里只加设置分组，
//! 不动 102 个业务模块与 612KB 的 UI 测试基线。
//!
//! 配置改用 `openhop_core::Config::default_path()` 的 IslandHop 独立目录
//! （macOS：`~/Library/Application Support/IslandHop/config.toml`），端口组也是
//! 独立的 TCP 24950 / UDP 24951：不与用户仍在运行的旧版 OpenHop
//! （`~/Library/Application Support/openhop/config.toml`，24850/24851）互相
//! 抢端口、抢组播发现组，也不会回写污染旧版配置。

use eframe::egui::{self, Color32, RichText, Ui};
use openhop_core::platform;
use openhop_core::protocol::{Ext, IslandSync, DEFAULT_PORT, DISCOVERY_PORT};
use openhop_core::{Config, Engine, Role, Status};
use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Duration;

/// UI 偏好单独存一个文件，不写进 config.toml（那个文件由引擎回写，混用会丢配置）。
fn prefs_path() -> PathBuf {
    Config::default_path().with_file_name("ui-prefs.toml")
}

fn read_hide_permission_hint() -> bool {
    std::fs::read_to_string(prefs_path())
        .is_ok_and(|s| s.lines().any(|l| l.trim() == "hide_permission_hint = true"))
}

fn write_hide_permission_hint(value: bool) {
    let path = prefs_path();
    if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
    let _ = std::fs::write(path, format!("hide_permission_hint = {value}\n"));
}

/// 打开 macOS 隐私设置对应子页；只在用户点按钮时调用。
fn open_privacy_pane(pane: &str) {
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
    let _ = std::process::Command::new("open").arg(url).spawn();
}

/// 回环自测残留：openhop-core 的 `examples/loopback_e2e.rs` 会把
/// `name = "LoopbackClient"` / `127.0.0.1` 写进 layout.screens（x=-2）。
/// 它不是真实对端，不能当成一台显示器参与排列与显示。
fn is_loopback_entry(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("LoopbackClient")
}

/// UI 状态挂在线程局部：所有 egui 渲染都在主线程，避免 `Engine` 的 Send 约束，
/// 也不必改动上游 `App` 结构体（它有 600KB 的 UI 测试基线依赖）。
/// 第一次访问时才初始化 = 应用启动时引擎按 `enabled` 自启。
thread_local! {
    static STATE: RefCell<Option<CrossScreen>> = const { RefCell::new(None) };
}

/// 访问唯一的跨屏状态。`Shell::new` 会主动调一次，保证打开软件即启动引擎。
pub fn with<R>(f: impl FnOnce(&mut CrossScreen) -> R) -> R {
    STATE.with(|cell| {
        let mut slot = cell.borrow_mut();
        let cross = slot.get_or_insert_with(CrossScreen::new);
        f(cross)
    })
}

/// 渲染在岛仔「设置 → 局域网跨屏」分组里。
pub fn settings_ui(ui: &mut Ui) {
    with(|cross| cross.render(ui));
}

// ---------------- IslandHop 业务状态同步 ----------------
// 复用 OpenHop 已经建好的加密 TCP 连接（`Msg::Ext` / `Hub::tell`），
// 不另起端口、不复制一份发现/配对/转发逻辑。以下三个函数由
// `sync_state::SyncManager` 在应用主线程的 1 秒轮询里调用；Engine 是
// !Send，只能待在 thread_local 里，所以不要把它们挪到后台线程。

/// 把本机岛仔业务状态快照广播给所有对端（`to="*"`，Server 自动转发）。
/// 返回 false 表示引擎未启动，调用方应保留脏标记，连上后重发。
pub fn sync_broadcast(rev: u64, updated_at: i64, payload: String) -> bool {
    with(|cross| {
        let Some(engine) = &cross.engine else { return false; };
        let sync = IslandSync {
            device: cross.cfg.name.clone(),
            rev,
            updated_at,
            payload,
        };
        engine.hub().tell("*", Ext::IslandState(sync));
        true
    })
}

/// 取走自上次调用以来收到的所有对端快照。
pub fn sync_inbox() -> Vec<IslandSync> {
    with(|cross| cross.engine.as_ref().map(|e| e.hub().take_island_states()).unwrap_or_default())
}

/// 本机在局域网里的设备名，用于过滤自己广播出去的回声。
pub fn sync_device() -> String {
    with(|cross| cross.cfg.name.clone())
}

/// `(已连接对端数, 本机是否为主设备 Server)`。主设备在 rev 相同时胜出。
pub fn sync_link() -> (usize, bool) {
    with(|cross| match &cross.engine {
        Some(e) => {
            let s = e.status();
            (s.peers.len(), matches!(s.role, Role::Server))
        }
        None => (0, false),
    })
}

/// 打开设置窗口并跳到跨屏分组（热键 / 启动参数用）。
pub fn open_settings_tab(app: &mut crate::app::App) {
    app.open_settings_tab(13);
}

enum Action {
    Start,
    Stop,
    Apply,
    Pair(String, String),
    PairAddr(String, String),
    Place(String, i32),
    AddScreen(String),
    /// 删除一条对端屏幕名（改名 / 换机后的旧名残留）。本机占位块不可删。
    RemoveScreen(String),
    /// 重命名一条对端屏幕名：保留位置，只改显示名（如 MacBook → MacBook-Pro）。
    RenameScreen(String, String),
}

pub struct CrossScreen {
    pub path: PathBuf,
    pub cfg: Config,
    pub engine: Option<Engine>,
    pub error: Option<String>,
    /// config.toml 是否解析成功。失败时禁止写回，避免用默认值把用户的
    /// 共享口令 / 端口 / 排列静默覆盖掉（本机曾因 TOML 表冲突踩过这个坑）。
    cfg_loaded: bool,

    name: String,
    pass: String,
    port: String,
    pair_code: String,
    direct_addr: String,
    new_screen: String,
    /// 对端重命名：当前正在改的名字 + 输入框缓冲。None 表示未进入重命名。
    rename_target: Option<String>,
    rename_buf: String,
    last_notes: Vec<String>,
    /// 用户勾选「不再显示权限相关提示」后，不再每次启动刷一屏授权指引。
    hide_permission_hint: bool,
}

impl CrossScreen {
    /// 读配置并按 `enabled` 决定是否立刻启动引擎。
    /// 启动失败（例如 macOS 尚未授权辅助功能）不会让整个应用起不来，
    /// 错误会显示在设置面板里，用户授权后可点「启动」重试。
    pub fn new() -> Self {
        let path = Config::default_path();
        let (cfg, load_error) = match Config::load(&path) {
            Ok(c) => (c, None),
            Err(e) => (
                Config::default(),
                Some(format!(
                    "读取配置失败，已用默认值：{e:#}。为避免覆盖原文件，本次不会写回 config.toml；\
                     请修正该文件后重启 IslandHop。"
                )),
            ),
        };
        let cfg_loaded = load_error.is_none();
        // 回环自测残留（LoopbackClient = 127.0.0.1, x=-2）会被当成一台真实
        // 显示器，还会把本机排到左边两格外。只在运行时内存里剔除，交给引擎
        // 的也是干净 layout；不主动改写用户磁盘上的 config.toml。
        let mut cfg = cfg;
        cfg.layout.screens.retain(|s| !is_loopback_entry(&s.name));
        let mut this = Self {
            name: cfg.name.clone(),
            pass: cfg.passphrase.clone(),
            port: cfg.port.to_string(),
            pair_code: String::new(),
            direct_addr: String::new(),
            new_screen: String::new(),
            rename_target: None,
            rename_buf: String::new(),
            last_notes: Vec::new(),
            hide_permission_hint: read_hide_permission_hint(),
            path,
            cfg,
            engine: None,
            error: load_error,
            cfg_loaded,
        };
        if this.cfg.enabled {
            this.start();
        }
        this
    }

    pub fn start(&mut self) {
        self.engine = None; // Drop 会 shutdown，先释放端口再重绑
        // 配置解析失败时不把路径交给引擎：引擎会在运行中用默认值回写，
        // 把用户原有的口令 / 端口 / 排列全部抹掉。
        let config_path = if self.cfg_loaded { Some(self.path.clone()) } else { None };
        match Engine::start(self.cfg.clone(), config_path) {
            Ok(engine) => {
                self.engine = Some(engine);
                self.error = None;
            }
            Err(e) => {
                self.error = Some(format!("{e:#}"));
            }
        }
    }

    pub fn stop(&mut self) {
        self.engine = None;
        self.error = None;
    }

    fn status(&self) -> Option<Status> {
        self.engine.as_ref().map(|e| e.status())
    }

    /// 把表单写回配置、落盘并重启引擎。角色 / 口令 / 排列都必须重启生效。
    fn apply(&mut self) {
        self.cfg.name = self.name.trim().to_string();
        if self.cfg.name.is_empty() {
            self.cfg.name = "computer".into();
            self.name = self.cfg.name.clone();
        }
        self.cfg.passphrase = self.pass.clone();
        self.cfg.port = self.port.trim().parse().unwrap_or(DEFAULT_PORT);
        self.port = self.cfg.port.to_string();
        self.cfg.enabled = true;
        if let Err(e) = self.cfg.save(&self.path) {
            self.error = Some(format!("保存配置失败：{e}"));
            return;
        }
        // 用户在表单里显式保存 = 用合法 TOML 修好了坏配置，恢复写回能力。
        self.cfg_loaded = true;
        self.start();
    }

    fn handle(&mut self, action: Action) {
        match action {
            Action::Start => {
                self.cfg.enabled = true;
                if self.cfg_loaded { let _ = self.cfg.save(&self.path); }
                self.start();
            }
            Action::Stop => {
                self.cfg.enabled = false;
                if self.cfg_loaded { let _ = self.cfg.save(&self.path); }
                self.stop();
            }
            Action::Apply => self.apply(),
            Action::Pair(device, code) => {
                if let Some(e) = &self.engine { e.pair(device, code); }
            }
            Action::PairAddr(addr, code) => {
                if let Some(e) = &self.engine { e.pair_addr(addr, code); }
            }
            Action::Place(name, x) => {
                self.cfg.layout.place(&name, x, 0);
                if let Some(e) = &self.engine { e.set_layout(self.cfg.layout.clone()); }
                if self.cfg_loaded { let _ = self.cfg.save(&self.path); }
            }
            Action::AddScreen(name) => {
                if !name.trim().is_empty() {
                    let next = self.cfg.layout.screens.len() as i32 + 1;
                    self.cfg.layout.place(name.trim(), next, 0);
                    if let Some(e) = &self.engine { e.set_layout(self.cfg.layout.clone()); }
                    if self.cfg_loaded { let _ = self.cfg.save(&self.path); }
                }
            }
            Action::RemoveScreen(name) => {
                // 只删对端条目：本机是布局里的隐含 SERVER(0,0)，从不落在
                // layout.screens 里，这里 remove 只可能命中历史残留的对端名。
                self.cfg.layout.remove(&name);
                if let Some(e) = &self.engine { e.set_layout(self.cfg.layout.clone()); }
                if self.cfg_loaded { let _ = self.cfg.save(&self.path); }
                self.last_notes.retain(|line| !line.starts_with(&format!("{name} · ")));
            }
            Action::RenameScreen(from, to) => {
                let to = to.trim().to_string();
                if to.is_empty() || to == from || is_loopback_entry(&to) { return; }
                let Some(pos) = self.cfg.layout.screens.iter().position(|s| s.name == from) else { return; };
                let (x, y) = { let s = &self.cfg.layout.screens[pos]; (s.x, s.y) };
                // 先清掉旧名与目标名的残留，再用 place 放回原坐标；place 内部
                // 自带同名去重，避免改名后出现两条显示器。对端不会落在 (0,0)
                // （那是本机 SERVER 位），所以 place 不会被 (0,0) 守卫跳过。
                self.cfg.layout.remove(&from);
                self.cfg.layout.remove(&to);
                self.cfg.layout.place(&to, x, y);
                if let Some(e) = &self.engine { e.set_layout(self.cfg.layout.clone()); }
                if self.cfg_loaded { let _ = self.cfg.save(&self.path); }
                for line in &mut self.last_notes {
                    if line.starts_with(&format!("{from} · ")) {
                        *line = line.replacen(from.as_str(), to.as_str(), 1);
                    }
                }
            }
        }
    }


    fn render(&mut self, ui: &mut Ui) {
        // 引擎状态是外部线程推进的，面板打开期间定时刷新。
        ui.ctx().request_repaint_after(Duration::from_millis(500));
        if let Some(e) = &self.engine {
            for note in e.take_notes() {
                let line = format!("{} · {}", note.title, note.body);
                if !self.last_notes.contains(&line) { self.last_notes.push(line); }
            }
            while self.last_notes.len() > 8 { self.last_notes.remove(0); }
        }
        let status = self.status();
        let mut actions: Vec<Action> = Vec::new();

        self.view_header(ui, status.as_ref(), &mut actions);
        ui.add_space(10.0);
        self.view_steps(ui, status.as_ref());
        ui.add_space(10.0);
        self.view_devices(ui, status.as_ref(), &mut actions);
        ui.add_space(10.0);
        self.view_pair_mode(ui, status.as_ref());
        ui.add_space(10.0);
        self.view_canvas(ui, status.as_ref(), &mut actions);
        ui.add_space(8.0);
        self.view_data_sync(ui);
        ui.add_space(8.0);

        egui::CollapsingHeader::new("高级设置 · 角色 / 名称 / 口令 / 权限")
            .default_open(false).show(ui, |ui| {
                self.view_permissions(ui);
                ui.separator();
                self.view_config(ui, &mut actions);
                ui.separator();
                self.view_direct(ui, &mut actions);
                ui.separator();
                self.view_actions(ui, &mut actions);
            });
        egui::CollapsingHeader::new("运行状态与日志")
            .default_open(false).show(ui, |ui| {
                self.view_status(ui, status.as_ref());
                self.view_platform_hint(ui);
            });

        for action in actions { self.handle(action); }
    }

    /// 两台电脑互联后的业务数据同步状态（喝水/天气/久坐/待办/账号清单）。
    fn view_data_sync(&self, ui: &mut Ui) {
        ui.label(RichText::new("双机数据同步").size(13.0).strong());
        ui.label(RichText::new(format!("● {}", crate::sync_state::status_text()))
            .size(11.5)
            .color(Color32::from_rgb(76, 209, 160)));
        ui.label(RichText::new("喝水 / 天气 / 久坐 / 待办 / 邮箱 / 日历 / AI 额度账号自动互通（Mac 为数据主设备）；\n邮箱授权码、日历密码与 API Key 随账号克隆到对端本机钥匙串，不落明文文件")
            .size(10.0)
            .color(ui.visuals().weak_text_color()));
    }

    fn theme_colors(ui: &Ui) -> (Color32, Color32, Color32, Color32) {
        let v = ui.visuals();
        let card = if v.dark_mode { Color32::from_rgb(38, 41, 48) } else { Color32::from_rgb(244, 246, 250) };
        let border = if v.dark_mode { Color32::from_rgb(62, 67, 78) } else { Color32::from_rgb(216, 221, 230) };
        (card, border, v.text_color(), v.strong_text_color())
    }

    /// 顶部：大号电源开关 + 当前状态，一眼看出「开着没」。
    fn view_header(&self, ui: &mut Ui, status: Option<&Status>, actions: &mut Vec<Action>) {
        let (card, border, text, strong) = Self::theme_colors(ui);
        let on = self.engine.is_some();
        let h = 58.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 12.0, card);
        p.rect_stroke(rect, 12.0, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);

        // 开关轨道
        let track = egui::Rect::from_min_size(rect.min + egui::vec2(16.0, 15.0), egui::vec2(50.0, 28.0));
        let green = Color32::from_rgb(52, 178, 116);
        p.rect_filled(track, 14.0, if on { green } else { Color32::from_gray(120) });
        let knob_x = if on { track.right() - 17.0 } else { track.left() + 14.0 };
        p.circle_filled(egui::pos2(knob_x, track.center().y), 11.0, Color32::WHITE);
        let hit = ui.interact(track.expand(5.0), ui.id().with("cross-power"), egui::Sense::click());
        if hit.clicked() { actions.push(if on { Action::Stop } else { Action::Start }); }

        p.text(rect.min + egui::vec2(80.0, 9.0), egui::Align2::LEFT_TOP,
            "局域网键鼠共享 / 剪贴板同步", egui::FontId::proportional(14.0), strong);
        let (dot, line) = match status {
            Some(s) => (green, format!("● 运行中 · {:?} · TCP {}", s.role, s.port)),
            None => (Color32::from_rgb(215, 85, 80), "● 未启动 · 打开左侧开关即可启用".into()),
        };
        p.text(rect.min + egui::vec2(80.0, 30.0), egui::Align2::LEFT_TOP, line,
            egui::FontId::proportional(11.5), dot);
        let _ = text;
        if let Some(s) = status {
            // 两行各自「左标题 / 右状态」，避免长状态串把右侧小字压掉（旧版重叠）。
            p.text(rect.right_top() + egui::vec2(-12.0, 11.0), egui::Align2::RIGHT_TOP,
                format!("已连接 {} 台", s.peers.len()), egui::FontId::proportional(10.0),
                ui.visuals().weak_text_color());
            let ip = s.addresses.first().cloned().unwrap_or_default();
            p.text(rect.right_top() + egui::vec2(-12.0, 32.0), egui::Align2::RIGHT_TOP, ip,
                egui::FontId::proportional(9.5), ui.visuals().weak_text_color());
        }
    }

    /// 三步连接进度，把「现在卡在哪一步」画出来。
    fn view_steps(&self, ui: &mut Ui, status: Option<&Status>) {
        let (card, border, _, strong) = Self::theme_colors(ui);
        let h = 52.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 10.0, card);
        p.rect_stroke(rect, 10.0, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);
        let labels = ["1  本机就绪", "2  发现设备", "3  已连接"];
        let done = [
            status.is_some(),
            status.is_some_and(|s| !s.discovered.is_empty() || !s.peers.is_empty()),
            status.is_some_and(|s| !s.peers.is_empty()),
        ];
        let green = Color32::from_rgb(52, 178, 116);
        let idle = Color32::from_gray(if ui.visuals().dark_mode { 95 } else { 175 });
        let w = rect.width() / 3.0;
        for i in 0..3 {
            let cx = rect.left() + w * (i as f32 + 0.5);
            let cy = rect.top() + 18.0;
            if i > 0 {
                let from = rect.left() + w * (i as f32 - 0.5) + 12.0;
                let to = cx - w * 0.5 + 12.0;
                p.line_segment([egui::pos2(from, cy), egui::pos2(to, cy)],
                    egui::Stroke::new(2.0, if done[i] { green } else { idle }));
            }
            p.circle_filled(egui::pos2(cx, cy), 9.0, if done[i] { green } else { idle });
            // IslandHop自定义 CJK 字体缺 U+2713，之前画「✓」会变成豆腐块，改用白色内点。
            p.circle_filled(egui::pos2(cx, cy), 3.0, Color32::WHITE);
            p.text(egui::pos2(cx, rect.top() + 36.0), egui::Align2::CENTER_CENTER, labels[i],
                egui::FontId::proportional(11.0), if done[i] { strong } else { ui.visuals().weak_text_color() });
        }
    }

    /// 设备卡片：图标 / 名称 / 系统 / 地址 / 连接状态 / 操作按钮。
    fn view_devices(&mut self, ui: &mut Ui, status: Option<&Status>, actions: &mut Vec<Action>) {
        let (card, border, text, strong) = Self::theme_colors(ui);
        ui.label(RichText::new("附近的电脑").size(14.0).strong().color(strong));
        ui.add_space(4.0);
        let Some(s) = status else {
            ui.label(RichText::new("引擎未启动，尚未扫描局域网。打开上方开关后会自动发现同网段设备。")
                .size(11.0).color(ui.visuals().weak_text_color()));
            return;
        };
        if s.discovered.is_empty() && s.peers.is_empty() {
            let dots = ".".repeat((ui.input(|i| i.time) * 2.0) as usize % 4);
            ui.label(RichText::new(format!("◌ 正在扫描局域网{dots} · 暂未发现对端")).size(11.5).color(strong));
            // 空扫描结果几乎都是环境问题（防火墙 / 网段 / 虚拟网卡），不是口令：
            // 发现包不携带口令，口令只在后续 TCP 握手校验。给出可逐项核对的清单，
            // 并把"直连兜底"入口点明，避免用户对着"正在扫描"无限等待。
            ui.add_space(2.0);
            for line in [
                "1. 对端也要打开本机开关并保持在同一个 Wi-Fi / 网段（不能用访客网络或 AP 隔离）。".to_string(),
                format!("2. 两台机器都放行防火墙入站：UDP {}（发现）与 TCP {}（连接）。", DISCOVERY_PORT, DEFAULT_PORT),
                "3. 暂时退出 VPN / 加速器 / 虚拟网卡，它们会抢走广播出口。".to_string(),
                "4. 仍搜不到：用下方「直接输入地址连接」填对端 IP 手动连一次。".to_string(),
            ] {
                ui.label(RichText::new(line).size(10.5).color(ui.visuals().weak_text_color()));
            }
            return;
        }
        let needs_code = s.needs_pairing;
        for d in &s.discovered {
            let connected = s.peers.iter().any(|p| p.name == d.name || p.addr == d.addr.to_string());
            let paired = s.paired.iter().any(|p| p.device == d.device);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 56.0), egui::Sense::hover());
            let p = ui.painter();
            p.rect_filled(rect, 10.0, card);
            p.rect_stroke(rect, 10.0, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);
            // 显示器图标
            let icon = egui::Rect::from_min_size(rect.min + egui::vec2(14.0, 12.0), egui::vec2(32.0, 24.0));
            p.rect_filled(icon, 4.0, Color32::from_rgb(58, 118, 196));
            p.rect_filled(egui::Rect::from_min_size(icon.min + egui::vec2(3.0, 3.0), egui::vec2(26.0, 16.0)),
                2.0, Color32::from_rgb(18, 28, 44));
            p.rect_filled(egui::Rect::from_min_size(icon.min + egui::vec2(12.0, 24.0), egui::vec2(8.0, 5.0)),
                1.0, Color32::from_rgb(58, 118, 196));
            let os = format!("{:?}", d.os);
            let name = if d.name.trim().is_empty() { "未命名电脑" } else { &d.name };
            p.text(rect.min + egui::vec2(58.0, 9.0), egui::Align2::LEFT_TOP,
                name, egui::FontId::proportional(13.0), strong);
            p.text(rect.min + egui::vec2(58.0, 28.0), egui::Align2::LEFT_TOP,
                format!("{os} · {:?} · {}", d.role, d.addr), egui::FontId::proportional(10.5),
                ui.visuals().weak_text_color());
            let (dot, label) = if connected {
                (Color32::from_rgb(52, 178, 116), "● 已连接")
            } else if paired {
                (Color32::from_rgb(70, 140, 210), "● 已配对 · 连接中")
            } else if needs_code {
                (Color32::from_rgb(200, 150, 45), "● 待配对")
            } else {
                (Color32::from_rgb(52, 178, 116), "● 口令一致 · 自动连接")
            };
            p.text(rect.min + egui::vec2(58.0, 43.0), egui::Align2::LEFT_TOP, label,
                egui::FontId::proportional(10.0), dot);
            if needs_code && !connected {
                let btn = egui::Rect::from_min_size(
                    egui::pos2(rect.right() - 88.0, rect.center().y - 13.0), egui::vec2(74.0, 26.0));
                p.rect_filled(btn, 7.0, Color32::from_rgb(58, 118, 196));
                p.text(btn.center(), egui::Align2::CENTER_CENTER, "配对", egui::FontId::proportional(11.5), Color32::WHITE);
                if ui.interact(btn, ui.id().with(&d.device), egui::Sense::click()).clicked() {
                    actions.push(Action::Pair(d.device.clone(), self.pair_code.trim().to_string()));
                }
            } else {
                p.text(rect.right_top() + egui::vec2(-14.0, 24.0), egui::Align2::RIGHT_TOP,
                    if connected { "使用中" } else { "自动" }, egui::FontId::proportional(10.5),
                    ui.visuals().weak_text_color());
            }
            let _ = text;
        }
    }

    /// 连接方式可视化：口令模式明确「无需配对码」，配对码模式才显示 6 位大字。
    fn view_pair_mode(&mut self, ui: &mut Ui, status: Option<&Status>) {
        let (card, border, _, strong) = Self::theme_colors(ui);
        ui.label(RichText::new("连接方式").size(14.0).strong().color(strong));
        ui.add_space(4.0);
        let passphrase = !self.pass.trim().is_empty();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), if passphrase { 60.0 } else { 88.0 }), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 10.0, card);
        p.rect_stroke(rect, 10.0, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);
        let green = Color32::from_rgb(52, 178, 116);
        if passphrase {
            p.text(rect.min + egui::vec2(14.0, 9.0), egui::Align2::LEFT_TOP,
                "● 共享口令模式 · 无需配对码", egui::FontId::proportional(13.0), green);
            p.text(rect.min + egui::vec2(14.0, 29.0), egui::Align2::LEFT_TOP,
                "两端口令一致时引擎自动发现并直连，不用点「配对」、不用输 6 位码。", egui::FontId::proportional(11.0), strong);
            p.text(rect.min + egui::vec2(14.0, 45.0), egui::Align2::LEFT_TOP,
                "若一直连不上，检查对端是否也填了同一个口令（改口令后需重启引擎）。",
                egui::FontId::proportional(10.0), ui.visuals().weak_text_color());
        } else {
            let code = status.and_then(|s| s.pairing_code.clone()).unwrap_or_else(|| "––– –––".into());
            p.text(rect.min + egui::vec2(14.0, 8.0), egui::Align2::LEFT_TOP,
                "配对码模式（未设置共享口令）", egui::FontId::proportional(11.5), strong);
            p.text(rect.min + egui::vec2(14.0, 25.0), egui::Align2::LEFT_TOP,
                "本机配对码", egui::FontId::proportional(10.5), ui.visuals().weak_text_color());
            p.text(rect.min + egui::vec2(110.0, 21.0), egui::Align2::LEFT_TOP, code,
                egui::FontId::proportional(22.0), green);
            // 对端配对码输入（只有口令为空、且客户端需要配对时才用得上）
            p.text(rect.right_top() + egui::vec2(-14.0, 58.0), egui::Align2::RIGHT_TOP,
                "在设备卡片点「配对」完成连接", egui::FontId::proportional(10.0), ui.visuals().weak_text_color());
            let row = egui::Rect::from_min_size(rect.min + egui::vec2(14.0, 54.0), egui::vec2(rect.width() - 28.0, 22.0));
            ui.put(row, egui::TextEdit::singleline(&mut self.pair_code).desired_width(90.0).hint_text("对端 6 位码"));
        }
    }

    /// IP 直连放在高级设置里：正常自动发现场景下不需要这一行。
    fn view_direct(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("IP 直连").size(11.0));
            ui.add(egui::TextEdit::singleline(&mut self.direct_addr).desired_width(150.0).hint_text("192.168.1.15"));
            if ui.small_button("连接").clicked() && !self.direct_addr.trim().is_empty() {
                actions.push(Action::PairAddr(self.direct_addr.trim().to_string(), self.pair_code.trim().to_string()));
            }
        });
    }

    /// 屏幕排列画布：显示器矩形按 x 排列，当前光标所在设备高亮。
    fn view_canvas(&mut self, ui: &mut Ui, status: Option<&Status>, actions: &mut Vec<Action>) {
        let (card, border, _, strong) = Self::theme_colors(ui);
        ui.label(RichText::new("屏幕排列 · 鼠标从哪条边切过去").size(14.0).strong().color(strong));
        ui.add_space(4.0);
        let h = 86.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 10.0, card);
        p.rect_stroke(rect, 10.0, egui::Stroke::new(1.0, border), egui::StrokeKind::Inside);
        let mut screens: Vec<(String, i32)> = vec![("本机".into(), 0)];
        let me = self.cfg.name.trim().to_string();
        for s in &self.cfg.layout.screens {
            // openhop-core/examples/loopback_e2e.rs 回环自测会把
            // "LoopbackClient" = 127.0.0.1 写进 layout（x=-2）。它不是真实对端，
            // 画出来就是莫名其妙的第三台显示器，还会把本机排到左侧两格外。
            if is_loopback_entry(&s.name) { continue; }
            // 改名后旧版 OpenHop 会把同一台机器记成两个名字；x=0 且名字等于
            // 本机名时不再重复画一张卡（用户截图里出现过 3 台显示器）。
            if s.x == 0 && (s.name.trim() == "本机" || s.name.trim().eq_ignore_ascii_case(&me)) { continue; }
            screens.push((s.name.clone(), s.x));
        }
        screens.sort_by_key(|(_, x)| *x);
        let active = status.map(|s| s.active.clone()).unwrap_or_default();
        let unit = 92.0;
        let total = unit * screens.len() as f32 + 10.0 * (screens.len().saturating_sub(1) as f32);
        let mut x = rect.center().x - total / 2.0;
        for (name, _) in &screens {
            let r = egui::Rect::from_min_size(egui::pos2(x, rect.top() + 13.0), egui::vec2(unit, 50.0));
            let here = active.is_empty() && name == "本机" || active == *name;
            p.rect_filled(r, 7.0, if here { Color32::from_rgb(52, 106, 176) } else { Color32::from_rgb(58, 64, 76) });
            p.rect_stroke(r, 7.0, egui::Stroke::new(if here { 2.0 } else { 1.0 },
                if here { Color32::from_rgb(105, 180, 255) } else { border }),
                egui::StrokeKind::Inside);
            // 屏幕里的菜单栏
            p.rect_filled(egui::Rect::from_min_size(r.min + egui::vec2(5.0, 5.0), egui::vec2(unit - 10.0, 8.0)),
                2.0, Color32::from_gray(28));
            p.text(r.min + egui::vec2(unit / 2.0, 32.0), egui::Align2::CENTER_CENTER, name,
                egui::FontId::proportional(11.0), Color32::WHITE);
            x += unit + 10.0;
        }
        p.text(rect.left_bottom() + egui::vec2(12.0, -10.0), egui::Align2::LEFT_BOTTOM,
            format!("● 光标当前在：{}", if active.is_empty() { "本机" } else { &active }),
            egui::FontId::proportional(10.0), Color32::from_rgb(52, 178, 116));
        let peer_count = self.cfg.layout.screens.iter().filter(|s| !is_loopback_entry(&s.name)).count();
        ui.label(RichText::new(format!("对端屏幕（{} 台）· 可重命名 / 删除", peer_count))
            .size(11.5).color(Color32::from_rgb(150, 165, 185)));
        let screens = self.cfg.layout.screens.clone();
        for s in &screens {
            if is_loopback_entry(&s.name) { continue; }
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}  x={}", s.name, s.x)).size(10.5));
                if ui.small_button("◀").clicked() { actions.push(Action::Place(s.name.clone(), s.x - 1)); }
                if ui.small_button("▶").clicked() { actions.push(Action::Place(s.name.clone(), s.x + 1)); }
                if ui.small_button("✎").on_hover_text("重命名（改成对端真实主机名）").clicked() {
                    self.rename_target = Some(s.name.clone());
                    self.rename_buf = s.name.clone();
                }
                if ui.small_button("✕").on_hover_text("删除这条对端屏幕名").clicked() {
                    actions.push(Action::RemoveScreen(s.name.clone()));
                    if self.rename_target.as_deref() == Some(s.name.as_str()) { self.rename_target = None; }
                }
            });
            // 该条目正在重命名：就地展开输入框，避免每台机器各占一个状态字段。
            if self.rename_target.as_deref() == Some(s.name.as_str()) {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.rename_buf).desired_width(150.0));
                    if ui.small_button("确定").clicked() {
                        actions.push(Action::RenameScreen(s.name.clone(), self.rename_buf.clone()));
                        self.rename_target = None;
                    }
                    if ui.small_button("取消").clicked() { self.rename_target = None; }
                });
            }
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.new_screen).desired_width(120.0).hint_text("加入对端屏幕名"));
            if ui.small_button("添加").clicked() {
                actions.push(Action::AddScreen(self.new_screen.clone()));
                self.new_screen.clear();
            }
        });
        ui.label(RichText::new(format!("本机：{}（居中固定，无需添加；鼠标移到画布边缘切到相邻对端）", self.cfg.name.trim()))
            .size(10.0).color(Color32::from_rgb(120, 140, 165)));
    }
    /// 权限状态一目了然；系统授权窗口只在用户主动点按钮时才弹。
    fn view_permissions(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("本机权限（不会自动弹授权窗口）").size(13.0).strong());
        let (accessibility, input_monitoring) = platform::permission_state();
        let ok = Color32::from_rgb(45, 155, 95);
        let bad = Color32::from_rgb(205, 90, 70);
        ui.horizontal_wrapped(|ui| {
            ui.label("辅助功能");
            ui.label(RichText::new(if accessibility { "已授权" } else { "未授权" }).color(if accessibility { ok } else { bad }));
            ui.label("·  输入监控");
            ui.label(RichText::new(if input_monitoring { "已授权" } else { "未授权" }).color(if input_monitoring { ok } else { bad }));
        });
        #[cfg(target_os = "macos")]
        {
            ui.horizontal_wrapped(|ui| {
                if ui.small_button("请求授权").clicked() { platform::request_permissions(); }
                if ui.small_button("辅助功能设置").clicked() { open_privacy_pane("Privacy_Accessibility"); }
                if ui.small_button("输入监控设置").clicked() { open_privacy_pane("Privacy_ListenEvent"); }
            });
        }
        let mut hide = self.hide_permission_hint;
        if ui.checkbox(&mut hide, "不再显示权限相关提示").changed() {
            self.hide_permission_hint = hide;
            write_hide_permission_hint(hide);
        }
        if !self.hide_permission_hint && !(accessibility && input_monitoring) {
            ui.label(RichText::new("键鼠跨屏需要「辅助功能 + 输入监控」两项都授权；授权后彻底退出 IslandHop 再重开。").size(10.5).color(bad));
        }
    }

    fn view_status(&self, ui: &mut Ui, status: Option<&Status>) {
        ui.label(RichText::new("运行状态").size(13.0).strong());
        let Some(s) = status else {
            ui.label(RichText::new("● 未启动").color(Color32::from_rgb(215, 85, 80)));
            if let Some(e) = &self.error {
                if !self.hide_permission_hint {
                    ui.label(RichText::new(e).size(11.0).color(Color32::from_rgb(205, 90, 70)));
                }
            }
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("● 运行中").color(Color32::from_rgb(45, 155, 95)));
            ui.label(format!("角色 {:?}", s.role));
            ui.label(format!("本机 {}", s.name));
            ui.label(format!("端口 {}", s.port));
        });
        ui.label(RichText::new(&s.message).size(11.5));
        if let Some(e) = &s.error {
            let is_permission_hint = e.contains("授权") || e.contains("权限")
                || e.contains("输入监控") || e.contains("辅助功能");
            if !(self.hide_permission_hint && is_permission_hint) {
                ui.label(RichText::new(format!("引擎错误：{e}")).size(11.5).color(Color32::from_rgb(205, 90, 70)));
            }
        }
        if let Some(e) = &s.pair_error {
            ui.label(RichText::new(format!("配对失败：{e}")).size(11.5).color(Color32::from_rgb(205, 90, 70)));
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("本机地址：");
            ui.label(RichText::new(if s.addresses.is_empty() { "—".into() } else { s.addresses.join("  ") }).size(11.0));
        });
        ui.label(format!("已连接 {} 台 · 发现 {} 台 · 已配对 {} 台", s.peers.len(), s.discovered.len(), s.paired.len()));
        for p in &s.peers {
            ui.label(RichText::new(format!("  ▸ {} ({:?}) @ {}", p.name, p.os, p.addr)).size(11.0));
        }
        if !s.active.is_empty() {
            ui.label(RichText::new(format!("光标当前在：{}", s.active)).size(11.0).color(Color32::from_rgb(45, 110, 190)));
        }
        if s.needs_pairing {
            ui.label(RichText::new("需要先与主控端配对：在下方填入它显示的 6 位配对码。").size(11.5).color(Color32::from_rgb(180, 130, 30)));
        }
        if let Some(code) = &s.pairing_code {
            ui.horizontal(|ui| {
                ui.label("本机配对码：");
                ui.label(RichText::new(code).size(18.0).strong().color(Color32::from_rgb(45, 155, 95)));
                ui.label(RichText::new("（在受控端输入）").size(10.5));
            });
        }
        if !self.last_notes.is_empty() {
            ui.add_space(3.0);
            ui.label(RichText::new("最近事件").size(11.5).strong());
            for n in self.last_notes.iter().rev().take(4) {
                ui.label(RichText::new(n).size(10.5));
            }
        }
    }

    fn view_config(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.label(RichText::new("本机配置").size(13.0).strong());
        ui.horizontal(|ui| {
            ui.label("角色");
            let server = self.cfg.role == Role::Server;
            if ui.selectable_label(server, "主控（共享本机键鼠）").clicked() { self.cfg.role = Role::Server; }
            if ui.selectable_label(!server, "受控（被对端控制）").clicked() { self.cfg.role = Role::Client; }
        });
        ui.horizontal(|ui| {
            ui.label("名称");
            ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(180.0));
            ui.label("端口");
            ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(70.0));
        });
        ui.horizontal(|ui| {
            ui.label("共享口令");
            ui.add(egui::TextEdit::singleline(&mut self.pass).password(true).desired_width(180.0).hint_text("两端一致；留空则走配对码"));
        });
        ui.checkbox(&mut self.cfg.allow_control, "允许对端控制本机屏幕");
        ui.checkbox(&mut self.cfg.share_input, "共享本机键鼠到对端");
        ui.checkbox(&mut self.cfg.clipboard_sync, "剪贴板同步");
        ui.checkbox(&mut self.cfg.swap_cmd_ctrl, "跨 macOS / Windows 自动换算 Cmd 与 Ctrl");
        ui.horizontal(|ui| {
            if ui.button("保存并重启引擎").clicked() { actions.push(Action::Apply); }
            ui.label(RichText::new("改角色 / 口令 / 端口后必须重启引擎").size(10.5));
        });
    }

    fn view_actions(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.label(RichText::new("引擎控制").size(13.0).strong());
        ui.horizontal(|ui| {
            if ui.button("启动").clicked() { actions.push(Action::Start); }
            if ui.button("停止").clicked() { actions.push(Action::Stop); }
            if let Some(e) = &self.error {
                if !self.hide_permission_hint {
                    ui.label(RichText::new(e).size(10.5).color(Color32::from_rgb(205, 90, 70)));
                }
            }
        });
    }

    fn view_platform_hint(&self, ui: &mut Ui) {
        ui.add_space(4.0);
        if cfg!(target_os = "macos") {
            if !self.hide_permission_hint {
                ui.label(RichText::new("macOS：应用已用固定证书签名，授权一次即可长期有效；只有更换签名证书或移走 /Applications 下的包才需重新勾。").size(10.5));
            }
        } else {
            ui.label(RichText::new("IslandHop 使用独立端口 24950/24951 与独立配置目录，可与旧版 OpenHop 并存；但两者都会注入键鼠事件，实际跨屏时请只启用其中一个。").size(10.5));
        }
        ui.label(RichText::new("提示：应用自带的剪贴板历史会把对端同步过来的内容也记一条，属正常现象，不会形成同步循环（引擎内部已做回声抑制）。").size(10.0));
        ui.label(RichText::new(format!("配置文件：{}", self.path.display())).size(9.5));
    }
}
