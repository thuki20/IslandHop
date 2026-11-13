//! IslandHop 的 OpenHop 跨屏控制台。
//!
//! 设计约束：**岛仔 102 个模块与 612KB 的 UI 测试基线一律不改**。
//! 上游把页面数量硬编码成 `[bool; 9]` / `[usize; 9]` 并散落在大量
//! `match page` 分支里，硬插第 10 页会牵动设置持久化与录制测试。
//! 所以键鼠共享 / 剪贴板同步以「后台引擎 + 独立 viewport 控制台」接入：
//! 引擎在 `Shell::new` 里启动，`update()` 每帧只做一次状态读取。
//!
//! 配置改用 `openhop_core::Config::default_path()` 的 IslandHop 独立目录
//! （macOS：`~/Library/Application Support/IslandHop/config.toml`），端口组也是
//! 独立的 TCP 24950 / UDP 24951：不与用户仍在运行的旧版 OpenHop
//! （`~/Library/Application Support/openhop/config.toml`，24850/24851）互相
//! 抢端口、抢组播发现组，也不会回写污染旧版配置。代价是旧版配对关系不继承，
//! 双端都装 IslandHop 后重新配对一次即可。

use eframe::egui::{self, Color32, RichText};
use openhop_core::{Config, Engine, Role, Status};
use openhop_core::protocol::{DEFAULT_PORT, DISCOVERY_PORT};
use std::path::PathBuf;
use std::time::Duration;

const ID: &str = "islandhop-cross-screen";

enum Action {
    Start,
    Stop,
    Apply,
    Pair(String, String),
    PairAddr(String, String),
    Place(String, i32),
    AddScreen(String),
}

pub struct CrossScreen {
    pub path: PathBuf,
    pub cfg: Config,
    pub engine: Option<Engine>,
    pub error: Option<String>,
    pub open: bool,

    name: String,
    pass: String,
    port: String,
    pair_code: String,
    direct_addr: String,
    new_screen: String,
    last_notes: Vec<String>,
}

impl CrossScreen {
    /// 读配置并按 `enabled` 决定是否立刻启动引擎。
    /// 启动失败（例如 macOS 尚未授权辅助功能）不会让整个应用起不来，
    /// 错误会显示在控制台里，用户授权后可点「启动」重试。
    pub fn new() -> Self {
        let path = Config::default_path();
        let (cfg, load_error) = match Config::load(&path) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(format!("读取配置失败，已用默认值：{e:#}"))),
        };
        let mut this = Self {
            name: cfg.name.clone(),
            pass: cfg.passphrase.clone(),
            port: cfg.port.to_string(),
            pair_code: String::new(),
            direct_addr: String::new(),
            new_screen: String::new(),
            last_notes: Vec::new(),
            path,
            cfg,
            engine: None,
            error: load_error,
            open: false,
        };
        if this.cfg.enabled {
            this.start();
        }
        this
    }

    pub fn start(&mut self) {
        self.engine = None; // Drop 会 shutdown，先释放端口再重绑
        match Engine::start(self.cfg.clone(), Some(self.path.clone())) {
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
        self.start();
    }

    fn handle(&mut self, action: Action) {
        match action {
            Action::Start => {
                self.cfg.enabled = true;
                let _ = self.cfg.save(&self.path);
                self.start();
            }
            Action::Stop => {
                self.cfg.enabled = false;
                let _ = self.cfg.save(&self.path);
                self.stop();
            }
            Action::Apply => self.apply(),
            Action::Pair(device, code) => {
                if let Some(e) = &self.engine {
                    e.pair(device, code);
                }
            }
            Action::PairAddr(addr, code) => {
                if let Some(e) = &self.engine {
                    e.pair_addr(addr, code);
                }
            }
            Action::Place(name, x) => {
                self.cfg.layout.place(&name, x, 0);
                if let Some(e) = &self.engine {
                    e.set_layout(self.cfg.layout.clone());
                }
                let _ = self.cfg.save(&self.path);
            }
            Action::AddScreen(name) => {
                if !name.trim().is_empty() {
                    let next = self.cfg.layout.screens.len() as i32 + 1;
                    self.cfg.layout.place(name.trim(), next, 0);
                    if let Some(e) = &self.engine {
                        e.set_layout(self.cfg.layout.clone());
                    }
                    let _ = self.cfg.save(&self.path);
                }
            }
        }
    }

    /// 每帧调用。窗口关闭时直接 return，egui 会回收该 viewport。
    pub fn ui(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let status = self.status();
        if let Some(e) = &self.engine {
            for note in e.take_notes() {
                let line = format!("{} · {}", note.title, note.body);
                if !self.last_notes.contains(&line) {
                    self.last_notes.push(line);
                }
            }
            while self.last_notes.len() > 8 {
                self.last_notes.remove(0);
            }
        }
        let mut actions: Vec<Action> = Vec::new();
        let mut closed = false;
        let id = egui::ViewportId::from_hash_of(ID);
        ctx.show_viewport_immediate(
            id,
            egui::ViewportBuilder::default()
                .with_title("跨屏控制 · IslandHop")
                .with_inner_size([620.0, 660.0])
                .with_min_inner_size([480.0, 420.0])
                .with_window_level(egui::WindowLevel::Normal),
            |ctx, _| {
                if ctx.input(|i| i.viewport().close_requested()) {
                    closed = true;
                }
                ctx.request_repaint_after(Duration::from_millis(400));
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(Color32::from_rgb(24, 25, 28)).inner_margin(18.0))
                    .show(ctx, |ui| {
                        ui.spacing_mut().item_spacing.y = 7.0;
                        ui.label(RichText::new("跨屏控制 · IslandHop").size(19.0).strong().color(Color32::WHITE));
                        ui.label(RichText::new("OpenHop 引擎：局域网键鼠共享 / 剪贴板同步").size(11.0).color(Color32::from_gray(155)));
                        ui.separator();

                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            self.view_status(ui, status.as_ref());
                            ui.separator();
                            self.view_pairing(ui, status.as_ref(), &mut actions);
                            ui.separator();
                            self.view_config(ui, &mut actions);
                            ui.separator();
                            self.view_layout(ui, &mut actions);
                            ui.separator();
                            self.view_actions(ui, &mut actions);
                            self.view_platform_hint(ui);
                        });
                    });
            },
        );
        if closed {
            self.open = false;
        }
        for action in actions {
            self.handle(action);
        }
    }

    fn view_status(&self, ui: &mut egui::Ui, status: Option<&Status>) {
        ui.label(RichText::new("运行状态").size(13.0).strong());
        let Some(s) = status else {
            ui.label(RichText::new("● 未启动").color(Color32::from_rgb(235, 120, 110)));
            if let Some(e) = &self.error {
                ui.label(RichText::new(e).size(11.0).color(Color32::from_rgb(240, 150, 120)));
            }
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("● 运行中").color(Color32::from_rgb(110, 215, 150)));
            ui.label(format!("角色 {:?}", s.role));
            ui.label(format!("本机 {}", s.name));
            ui.label(format!("端口 {}", s.port));
        });
        ui.label(RichText::new(&s.message).size(11.5).color(Color32::from_gray(200)));
        if let Some(e) = &s.error {
            ui.label(RichText::new(format!("引擎错误：{e}")).size(11.5).color(Color32::from_rgb(240, 150, 120)));
        }
        if let Some(e) = &s.pair_error {
            ui.label(RichText::new(format!("配对失败：{e}")).size(11.5).color(Color32::from_rgb(240, 150, 120)));
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("本机地址：");
            ui.label(RichText::new(if s.addresses.is_empty() { "—".into() } else { s.addresses.join("  ") }).size(11.0).color(Color32::from_gray(170)));
        });
        ui.label(format!("已连接 {} 台 · 发现 {} 台 · 已配对 {} 台", s.peers.len(), s.discovered.len(), s.paired.len()));
        for p in &s.peers {
            ui.label(RichText::new(format!("  ▸ {} ({:?}) @ {}", p.name, p.os, p.addr)).size(11.0).color(Color32::from_gray(190)));
        }
        if !s.active.is_empty() {
            ui.label(RichText::new(format!("光标当前在：{}", s.active)).size(11.0).color(Color32::from_rgb(150, 200, 245)));
        }
        if s.needs_pairing {
            ui.label(RichText::new("需要先与主控端配对：在下方填入它显示的 6 位配对码。").size(11.5).color(Color32::from_rgb(240, 200, 120)));
        }
        if let Some(code) = &s.pairing_code {
            ui.horizontal(|ui| {
                ui.label("本机配对码：");
                ui.label(RichText::new(code).size(18.0).strong().color(Color32::from_rgb(120, 225, 170)));
                ui.label(RichText::new("（在受控端输入）").size(10.5).color(Color32::from_gray(150)));
            });
        }
        if !self.last_notes.is_empty() {
            ui.add_space(3.0);
            ui.label(RichText::new("最近事件").size(11.5).strong());
            for n in self.last_notes.iter().rev().take(4) {
                ui.label(RichText::new(n).size(10.5).color(Color32::from_gray(165)));
            }
        }
    }

    fn view_pairing(&mut self, ui: &mut egui::Ui, status: Option<&Status>, actions: &mut Vec<Action>) {
        ui.label(RichText::new("配对 / 发现的设备").size(13.0).strong());
        if let Some(s) = status {
            if s.discovered.is_empty() {
                ui.label(RichText::new(format!("尚未发现同网段设备。请确认对端已启动、口令一致、防火墙放行 TCP {} / UDP {}。", DEFAULT_PORT, DISCOVERY_PORT)).size(11.0).color(Color32::from_gray(160)));
            }
            for d in &s.discovered {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} · {:?} · {:?} · {}", d.name, d.os, d.role, d.addr)).size(11.0));
                    if ui.small_button("配对").clicked() {
                        actions.push(Action::Pair(d.device.clone(), self.pair_code.trim().to_string()));
                    }
                });
            }
        }
        ui.horizontal(|ui| {
            ui.label("配对码：");
            ui.add(egui::TextEdit::singleline(&mut self.pair_code).desired_width(90.0).hint_text("6 位数字"));
            ui.label(RichText::new("由主控端显示").size(10.5).color(Color32::from_gray(150)));
        });
        ui.horizontal(|ui| {
            ui.label("IP 直连：");
            ui.add(egui::TextEdit::singleline(&mut self.direct_addr).desired_width(160.0).hint_text("192.168.1.15"));
            if ui.small_button("连接").clicked() && !self.direct_addr.trim().is_empty() {
                actions.push(Action::PairAddr(self.direct_addr.trim().to_string(), self.pair_code.trim().to_string()));
            }
        });
    }

    fn view_config(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.label(RichText::new("本机配置").size(13.0).strong());
        ui.horizontal(|ui| {
            ui.label("角色");
            let server = self.cfg.role == Role::Server;
            if ui.selectable_label(server, "主控（共享本机键鼠）").clicked() {
                self.cfg.role = Role::Server;
            }
            if ui.selectable_label(!server, "受控（被对端控制）").clicked() {
                self.cfg.role = Role::Client;
            }
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
            if ui.button("保存并重启引擎").clicked() {
                actions.push(Action::Apply);
            }
            ui.label(RichText::new("改角色 / 口令 / 端口后必须重启引擎").size(10.5).color(Color32::from_gray(150)));
        });
    }

    fn view_layout(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.label(RichText::new("屏幕排列（主控端生效）").size(13.0).strong());
        ui.label(RichText::new("本机固定在 (0,0)；左右位置决定鼠标从哪条边切过去。").size(10.5).color(Color32::from_gray(150)));
        ui.horizontal(|ui| {
            ui.label(RichText::new("▣ 本机 (0,0)").size(11.5).color(Color32::from_rgb(140, 205, 245)));
        });
        let screens = self.cfg.layout.screens.clone();
        for s in &screens {
            ui.horizontal(|ui| {
                ui.label(format!("▣ {}  → x={}", s.name, s.x));
                if ui.small_button("◀ 左移").clicked() {
                    actions.push(Action::Place(s.name.clone(), s.x - 1));
                }
                if ui.small_button("右移 ▶").clicked() {
                    actions.push(Action::Place(s.name.clone(), s.x + 1));
                }
            });
        }
        ui.horizontal(|ui| {
            ui.label("加入屏幕：");
            ui.add(egui::TextEdit::singleline(&mut self.new_screen).desired_width(130.0).hint_text("对端显示的名称"));
            if ui.small_button("添加到右侧").clicked() {
                actions.push(Action::AddScreen(self.new_screen.clone()));
                self.new_screen.clear();
            }
        });
    }

    fn view_actions(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.label(RichText::new("引擎控制").size(13.0).strong());
        ui.horizontal(|ui| {
            if ui.button("启动").clicked() {
                actions.push(Action::Start);
            }
            if ui.button("停止").clicked() {
                actions.push(Action::Stop);
            }
            if let Some(e) = &self.error {
                ui.label(RichText::new(e).size(10.5).color(Color32::from_rgb(240, 150, 120)));
            }
        });
    }

    fn view_platform_hint(&self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        if cfg!(target_os = "macos") {
            ui.label(RichText::new("macOS：需在「系统设置 → 隐私与安全性 → 辅助功能 / 输入监控」勾选 IslandHop，并彻底退出重开后才生效。TCC 按应用路径授权，换成 /Applications 下的包要重新勾。").size(10.5).color(Color32::from_rgb(220, 195, 130)));
        } else {
            ui.label(RichText::new("IslandHop 使用独立端口 24950/24951 与独立配置目录，可与旧版 OpenHop 并存；但两者都会注入键鼠事件，实际跨屏时请只启用其中一个。").size(10.5).color(Color32::from_rgb(220, 195, 130)));
        }
        ui.label(RichText::new("提示：岛仔自带的剪贴板历史会把 OpenHop 从对端写入的内容也记一条，属正常现象，不会形成同步循环（OpenHop 引擎内部已做回声抑制）。").size(10.0).color(Color32::from_gray(140)));
        ui.label(RichText::new(format!("配置文件：{}", self.path.display())).size(9.5).color(Color32::from_gray(120)));
    }
}
