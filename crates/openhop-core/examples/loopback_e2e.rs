//! IslandHop 回环自检：连到本机已运行的 Server（默认 127.0.0.1:24950），
//! 验证共享口令 PSK 握手、会话与剪贴板通道是否建立。
//!
//! 用法（口令从环境变量传入，不写进源码）：
//! ```text
//! IH_PASSPHRASE='两端一致的共享口令' cargo run -p openhop-core --example loopback_e2e
//! ```
//! 注意：这是同机回环的协议层验证，不能替代两台真机之间的跨屏/剪贴板实测。

use openhop_core::{Config, Engine, Role};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let pass = std::env::var("IH_PASSPHRASE")
        .map_err(|_| anyhow::anyhow!("set IH_PASSPHRASE to the shared passphrase"))?;
    let path = std::env::temp_dir().join("islandhop-loopback-e2e.toml");
    let _ = std::fs::remove_file(&path);

    let mut cfg = Config::default();
    cfg.name = "LoopbackClient".into();
    cfg.role = Role::Client;
    cfg.passphrase = pass;
    cfg.port = 24950;
    cfg.server_addr = Some("127.0.0.1".into());
    cfg.enabled = true;
    cfg.clipboard_sync = true;

    let engine = Engine::start(cfg, Some(path.clone()))?;
    println!("engine started: client -> 127.0.0.1:24950 (passphrase auth)");

    let t0 = Instant::now();
    let mut ever_connected = false;
    while t0.elapsed() < Duration::from_secs(20) {
        let s = engine.status();
        if !s.peers.is_empty() {
            ever_connected = true;
        }
        println!(
            "t={:>5.1}s running={} msg={:?} peers={} error={:?}",
            t0.elapsed().as_secs_f32(),
            s.running,
            s.message,
            s.peers.len(),
            s.error
        );
        if ever_connected && t0.elapsed() > Duration::from_secs(6) {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    let _ = std::fs::remove_file(&path);
    println!("CONNECTED={ever_connected}");
    if ever_connected {
        Ok(())
    } else {
        anyhow::bail!("no session established with the local server")
    }
}
