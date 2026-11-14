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
use openhop_core::protocol::{DEFAULT_PORT, DISCOVERY_PORT};
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
        let mut this = Self {
            name: cfg.name.clone(),
            pass: cfg.passphrase.clone(),
            port: cfg.port.to_string(),
            pair_code: String::new(),
            direct_addr: String::new(),
            new_screen: String::new(),
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

        ui.label(RichText::new("后台自动运行：勾选后每次打开 IslandHop 即启用，无需再打开这个页面。").size(11.5));
        ui.add_space(6.0);
        let mut enabled = self.cfg.enabled;
        if ui.checkbox(&mut enabled, "启用局域网键鼠共享 / 剪贴板同步").changed() {
            actions.push(if enabled { Action::Start } else { Action::Stop });
        }
        ui.add_space(4.0);

        self.view_permissions(ui);
        ui.separator();
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

        for action in actions { self.handle(action); }
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

    fn view_pairing(&mut self, ui: &mut Ui, status: Option<&Status>, actions: &mut Vec<Action>) {
        ui.label(RichText::new("配对 / 发现的设备").size(13.0).strong());
        if let Some(s) = status {
            if s.discovered.is_empty() {
                ui.label(RichText::new(format!("尚未发现同网段设备。请确认对端已启动、口令一致、防火墙放行 TCP {} / UDP {}。", DEFAULT_PORT, DISCOVERY_PORT)).size(11.0));
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
            ui.label(RichText::new("由主控端显示").size(10.5));
        });
        ui.horizontal(|ui| {
            ui.label("IP 直连：");
            ui.add(egui::TextEdit::singleline(&mut self.direct_addr).desired_width(160.0).hint_text("192.168.1.15"));
            if ui.small_button("连接").clicked() && !self.direct_addr.trim().is_empty() {
                actions.push(Action::PairAddr(self.direct_addr.trim().to_string(), self.pair_code.trim().to_string()));
            }
        });
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

    fn view_layout(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.label(RichText::new("屏幕排列（主控端生效）").size(13.0).strong());
        ui.label(RichText::new("本机固定在 (0,0)；左右位置决定鼠标从哪条边切过去。").size(10.5));
        ui.horizontal(|ui| {
            ui.label(RichText::new("▣ 本机 (0,0)").size(11.5).color(Color32::from_rgb(45, 110, 190)));
        });
        let screens = self.cfg.layout.screens.clone();
        for s in &screens {
            ui.horizontal(|ui| {
                ui.label(format!("▣ {}  → x={}", s.name, s.x));
                if ui.small_button("◀ 左移").clicked() { actions.push(Action::Place(s.name.clone(), s.x - 1)); }
                if ui.small_button("右移 ▶").clicked() { actions.push(Action::Place(s.name.clone(), s.x + 1)); }
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
        ui.label(RichText::new("提示：岛仔自带的剪贴板历史会把对端同步过来的内容也记一条，属正常现象，不会形成同步循环（引擎内部已做回声抑制）。").size(10.0));
        ui.label(RichText::new(format!("配置文件：{}", self.path.display())).size(9.5));
    }
}
