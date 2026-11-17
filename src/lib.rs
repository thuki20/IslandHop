//! 岛仔：Windows 顶部「灵动岛」。
//!
//! - `app` / `pages`：与平台无关的界面与动效（外壳与录制器共用）
//! - `anim` / `paint` / `shape` / `theme`：弹簧、矢量绘制、面板轮廓、配色字体
//! - `model` / `lyrics` / `smtc` / `icons`：数据、歌词、系统媒体、文件图标
//! - `native` / `tray`：仅 Windows 的窗口区域、命中穿透与托盘

/// 更新交接进行中标志。交接期间（安装包校验通过、update.ps1 已启动）主程序的
/// 任何异常退出都由安装脚本负责拉起新版本——绝不能触发自动重启：替补实例会跟
/// 安装脚本拉起的新实例抢单实例互斥体，弹出「已在运行，但暂时无法打开窗口」
/// 错误框（v0.6.84 实测）。标志放在 lib 侧，bin 的 diagnostics 与 lib 的 updates 共用。
static UPDATE_HANDOFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 进入更新交接时调用（进程生命周期内不可逆：交接成功后主程序必然退出）。
pub fn begin_update_handoff() {
    UPDATE_HANDOFF.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// bin 侧 diagnostics 据此跳过自动重启与模态弹窗，让主程序尽快退出交给安装脚本。
pub fn update_handoff_active() -> bool {
    UPDATE_HANDOFF.load(std::sync::atomic::Ordering::SeqCst)
}

pub mod display_layout;
/// IslandHop 新增：OpenHop 跨屏引擎 + 内嵌在设置窗口的面板。
pub mod cross_screen;
/// IslandHop 新增：屏幕镜像最小原型（复用 OpenHop 引擎的 SCREEN 采集 / WinFrame 链路）。
pub mod remote_view;
/// IslandHop 新增：两台电脑互联后同步岛仔业务数据（喝水 / 天气 / 久坐 / 待办 / 账号清单）。
pub mod sync_state;
/// IslandHop 新增：macOS 窗口层级 / 点击穿透 / 刘海几何（岛仔上游 0 处 macOS 分支）。
pub mod mac_window;
mod mail_privacy;
pub mod poll_clock;
/// IslandHop 新增：电池供电省电（通电全速 / 用电池降帧降刷新，交互档不降）。
pub mod power_saving;
pub mod resources;
pub mod anim;
pub mod oled_shift;
pub mod app;
pub mod messenger;
pub mod child_process;
mod settings_window;
pub mod bt;
pub mod calendar;
mod calendar_ui;
mod local_calendar;
mod calendar_editor;
pub mod glass;
pub mod skin_paint;
pub mod icons;
pub mod input;
pub mod lyrics;
pub mod mail;
pub mod mail_format;
mod mail_reader;
pub mod mail_manage;
mod mail_manage_ui;
pub mod clipboard;
pub mod side_dock;
mod task_picker;
pub mod model;
pub mod renderer;
pub mod otp;
pub mod quota;
mod quota_ui;
pub mod weather;
mod weather_ui;
mod pages;
mod extras;
mod pet;
mod pet_expressions;
mod bar_layout;
mod resident_settings;
mod avoidance;
mod focus_bar;
#[cfg(windows)]
pub mod backdrop;
#[cfg(windows)]
pub mod composition;
#[cfg(windows)]
pub mod system;
pub mod paint;
pub mod shape;
mod fluid;
pub mod smtc;
pub mod music_volume;
/// IslandHop：macOS 系统数据源（上游整模块 Windows-only，Mac 上补电池/音量/网络/闲置/媒体/文件）。
#[cfg(target_os = "macos")]
pub mod macos_sys;
pub mod theme;
pub mod settings_theme;
mod updates;
#[cfg(windows)]
pub use updates::record_install_size;

#[cfg(windows)]
pub mod native;
#[cfg(windows)]
pub mod wechat;
#[cfg(windows)]
pub mod tray;

#[cfg(test)]
mod ui_tests;
#[cfg(test)]
#[path = "bin/daozai-record/raster.rs"]
mod preview_raster;
