//! 诊断：广播发现被路由器过滤时，靠「记得的地址」直连对端是否成立。
//!
//! 不写死任何口令：全部从环境变量读。
//!
//! ```text
//! IH_PASSPHRASE='共享口令' IH_PEER_IP=192.168.1.16 cargo run -p openhop-core --example seed_probe
//! ```

use openhop_core::{Config, Engine, Role};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let pass = std::env::var("IH_PASSPHRASE").map_err(|_| anyhow::anyhow!("set IH_PASSPHRASE"))?;
    let peer_ip = std::env::var("IH_PEER_IP").unwrap_or_else(|_| "192.168.1.16".into());
    let path = std::env::temp_dir().join("islandhop-seed-probe.toml");
    let _ = std::fs::remove_file(&path);

    let mut cfg = Config::default();
    cfg.name = std::env::var("IH_NAME").unwrap_or_else(|_| "SeedProbe".into());
    if let Ok(id) = std::env::var("IH_DEVICE_ID") {
        cfg.device_id = id;
    }
    cfg.role = Role::Client;
    cfg.passphrase = pass;
    cfg.port = 24950;
    cfg.enabled = true;
    cfg.clipboard_sync = false;
    // 关键：模拟真实配置里「记得的地址」，不设 server_addr（模拟发现不到的场景）。
    cfg.last_ips.insert("MacBook-Pro".into(), peer_ip.clone());

    let engine = Engine::start(cfg, Some(path))?;
    println!("engine started: client, 发现列表为空时按 last_ips={peer_ip} 兜底直连");

    let t0 = Instant::now();
    let mut connected_at: Option<f32> = None;
    while t0.elapsed() < Duration::from_secs(20) {
        let s = engine.status();
        if !s.peers.is_empty() && connected_at.is_none() {
            connected_at = Some(t0.elapsed().as_secs_f32());
            println!("✅ 已连上: {:?}", s.peers.iter().map(|p| p.name.clone()).collect::<Vec<_>>());
        }
        if (t0.elapsed().as_millis() / 1000) % 2 == 0 {
            println!(
                "t={:>5.1}s discovered={:?} peers={} msg={:?} err={:?}",
                t0.elapsed().as_secs_f32(),
                s.discovered.iter().map(|d| d.name.clone()).collect::<Vec<_>>(),
                s.peers.len(),
                s.message,
                s.error
            );
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    match connected_at {
        Some(t) => println!("RESULT=OK 首次连接耗时 {t:.1}s"),
        None => println!("RESULT=FAIL 20 秒内未连上"),
    }
    Ok(())
}
