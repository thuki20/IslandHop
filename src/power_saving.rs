//! 电池供电省电：通电时全速运行，用电池时降低帧率与后台刷新频率。
//!
//! 设计目标（用户诉求）：在不影响正常使用的前提下尽量降低功耗。
//! - 「正常使用」= 交互（悬停/展开/拖拽/按键）必须保持 60fps 手感，绝不降档；
//! - 「省电」只作用于静置时的常驻动画（宠物/媒体/环境动效）与后台采样。
//!
//! 主循环每帧调用 [`sync`] 把「是否电池供电」和「用户开关」写入全局原子量；
//! 帧率汇聚点与后台线程只需读 [`active`] / [`scale_repaint`] / [`sample_interval`]，
//! 无需到处传递电池状态。全部无锁、跨平台（Windows/macOS/Linux 都能编译）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 用户设置：电池供电时启用省电（默认开）。
static ENABLED: AtomicBool = AtomicBool::new(true);
/// 当前是否真的处于电池供电（由主循环每帧刷新）。
static ON_BATTERY: AtomicBool = AtomicBool::new(false);

/// 用户开关变化（来自设置页）。
pub fn set_enabled(on: bool) { ENABLED.store(on, Ordering::Relaxed); }
pub fn enabled() -> bool { ENABLED.load(Ordering::Relaxed) }

/// 主循环每帧调用：更新供电状态。
/// `charging == false && battery.is_some()` 视为用电池；台式机（无电池）永远全速。
pub fn sync(charging: bool, battery: Option<u8>) {
    ON_BATTERY.store(!charging && battery.is_some(), Ordering::Relaxed);
}

/// 当前是否处于省电状态。
pub fn active() -> bool { ENABLED.load(Ordering::Relaxed) && ON_BATTERY.load(Ordering::Relaxed) }

/// 后台采样间隔倍率：省电时放宽到 2 倍（采样频率减半）。
pub fn sample_multiplier() -> u32 { if active() { 2 } else { 1 } }

/// 按当前供电状态放大一个采样间隔。
pub fn sample_interval(base: Duration) -> Duration { base.saturating_mul(sample_multiplier()) }

/// 缩放重绘间隔。交互档（≤ 1/60s）原样保留，保证手感；
/// 静置动画档（媒体 30fps / 宠物 20fps / 睡眠 10fps / 静态 1fps）在省电时
/// 统一放宽到约 2 倍（30→15、20→10、10→5），静态 1fps 不再放慢（已经很省）。
/// 返回 None 表示该档位不该被缩放（交还调用方原值）。
pub fn scale_repaint(interval: Duration) -> Duration {
    if !active() { return interval; }
    let interactive = Duration::from_secs_f64(1.0 / 60.0);
    // 交互档或更快：一律不降，保护手感。
    if interval <= interactive { return interval; }
    // 静态 1fps 已经足够省，不再放慢（否则提醒/同步延迟明显）。
    if interval >= Duration::from_secs(1) { return interval; }
    // 中间动画档：放宽 2 倍。
    interval.saturating_mul(2).min(Duration::from_secs(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default_state_is_full_speed() {
        set_enabled(false);
        ON_BATTERY.store(true, Ordering::Relaxed);
        assert!(!active());
        assert_eq!(sample_multiplier(), 1);
        let d = Duration::from_millis(33);
        assert_eq!(scale_repaint(d), d);
    }

    #[test]
    fn ac_power_is_full_speed() {
        set_enabled(true);
        sync(true, Some(80)); // 通电
        assert!(!active());
        assert_eq!(scale_repaint(Duration::from_millis(50)), Duration::from_millis(50));
    }

    #[test]
    fn desktop_without_battery_is_full_speed() {
        set_enabled(true);
        sync(false, None); // 台式机无电池
        assert!(!active());
    }

    #[test]
    fn on_battery_scales_animations_but_not_interaction() {
        set_enabled(true);
        sync(false, Some(55)); // 用电池
        assert!(active());
        // 交互档 60fps 不降。
        let fast = Duration::from_secs_f64(1.0 / 60.0);
        assert_eq!(scale_repaint(fast), fast);
        // 媒体 30fps → 15fps。
        assert_eq!(scale_repaint(Duration::from_millis(33)), Duration::from_millis(66));
        // 宠物 20fps → 10fps。
        assert_eq!(scale_repaint(Duration::from_millis(50)), Duration::from_millis(100));
        // 静态 1fps 不再放慢。
        assert_eq!(scale_repaint(Duration::from_secs(1)), Duration::from_secs(1));
        assert_eq!(sample_multiplier(), 2);
    }
}
