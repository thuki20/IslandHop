//! Zero-config discovery: every running instance announces itself every 2
//! seconds and listens for others, on every network interface.
//!
//! - IPv4 broadcast on each interface (home/office networks, and direct
//!   cables on Windows/macOS, which self-assign 169.254.x.x addresses).
//! - IPv6 link-local multicast on each interface. Every OS gives every
//!   connected port an `fe80::` address with no router or DHCP, so two
//!   computers joined by an Ethernet, USB-C or Thunderbolt cable always find
//!   each other, while each keeps its own Wi-Fi connection.
//!
//! Beacons carry no secrets: a name, OS, role and port. Authentication
//! happens in the encrypted handshake.

use crate::config::Role;
use crate::protocol::{Os, DISCOVERY_PORT, PROTOCOL_VERSION};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAGIC: [u8; 4] = *b"OHOP";
const INTERVAL: Duration = Duration::from_secs(2);
const EXPIRY: Duration = Duration::from_secs(7);
/// Link-local multicast group for OpenHop beacons ("OHOP").
pub const GROUP_V6: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0x4f48, 0x4f50);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Beacon {
    pub magic: [u8; 4],
    pub version: u32,
    /// Random per run (so we can ignore our own beacons).
    pub id: u64,
    /// Stable per installation (used for pairing).
    pub device: String,
    pub name: String,
    pub os: Os,
    pub role: Role,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct Discovered {
    pub name: String,
    pub device: String,
    pub os: Os,
    pub role: Role,
    pub version: u32,
    /// Best address to connect to.
    pub addr: SocketAddr,
    /// "network" (through your router) or "direct" (cable / link-local only).
    pub link: &'static str,
    #[serde(skip)]
    pub last_seen: Instant,
    #[serde(skip)]
    v4: Option<(SocketAddr, Instant)>,
    #[serde(skip)]
    v6: Option<(SocketAddr, Instant)>,
}

pub struct Discovery {
    peers: Arc<Mutex<HashMap<u64, Discovered>>>,
    stop: Arc<AtomicBool>,
    /// 已知对端 IPv4（上次成功连接的地址 / 手动地址）。路由器不转发客户端间
    /// 广播时，靠向这些地址单播 beacon 兜底发现。
    unicast: Arc<Mutex<Vec<Ipv4Addr>>>,
    /// 本机信标字节：收到对端包时单播回信，让被路由器过滤的一方也能被发现。
    payload: Arc<Vec<u8>>,
    /// 单播发送端（v4），与广播共用 socket 但不依赖广播可达。
    probe: UdpSocket,
}

/// 单播兜底目标是否可加入：只接受可路由的普通 IPv4，过滤掉 unspecified / 广播 / 回环 / 组播。
fn valid_unicast_target(ip: IpAddr) -> Option<Ipv4Addr> {
    match ip {
        IpAddr::V4(v4) if !v4.is_unspecified() && !v4.is_broadcast()
            && !v4.is_loopback() && !v4.is_multicast() => Some(v4),
        _ => None,
    }
}

impl Discovery {
    /// 运行时加入一个单播兜底目标（对端的 IPv4）。重复加入会去重。
    /// 广播/组播照常发，单播是额外兜底，不冲突。
    pub fn add_unicast_target(&self, ip: IpAddr) {
        if let Some(v4) = valid_unicast_target(ip) {
            let mut list = self.unicast.lock();
            if !list.contains(&v4) {
                list.push(v4);
                log::info!("discovery: 加入单播兜底目标 {v4}");
            }
        }
    }
}

/// Windows：独占绑定（SO_EXCLUSIVEADDRUSE）。本机的 SO_REUSEADDR 在 Windows 语义
/// 是「任何进程都能再绑同一端口」，正是残留/重复实例互相抢占 UDP 发现端口、
/// beacon 被投给已死 socket 导致「永远扫描不到」的根源。独占后第二个绑定者
/// 会明确拿到 AddrInUse，而不是静默变聋。Unix 语义安全，保持 reuse 不变。
#[cfg(windows)]
fn set_exclusive_bind(s: &Socket) -> std::io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Networking::WinSock::{setsockopt, SOCKET, SO_EXCLUSIVEADDRUSE, SOL_SOCKET};
    let val: i32 = 1;
    let rc = unsafe {
        setsockopt(
            SOCKET(s.as_raw_socket() as usize),
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            Some(&val.to_ne_bytes()),
        )
    };
    if rc == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

fn bind_v4() -> std::io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    #[cfg(windows)]
    set_exclusive_bind(&s)?;
    #[cfg(not(windows))]
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(target_os = "solaris")))]
    s.set_reuse_port(true)?;
    s.set_broadcast(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT)).into())?;
    let s: UdpSocket = s.into();
    s.set_read_timeout(Some(Duration::from_millis(500)))?;
    Ok(s)
}

fn bind_v6() -> std::io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_only_v6(true)?;
    #[cfg(windows)]
    set_exclusive_bind(&s)?;
    #[cfg(not(windows))]
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(target_os = "solaris")))]
    s.set_reuse_port(true)?;
    s.set_multicast_loop_v6(false)?;
    s.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, DISCOVERY_PORT)).into())?;
    let s: UdpSocket = s.into();
    s.set_read_timeout(Some(Duration::from_millis(500)))?;
    Ok(s)
}

/// Up, non-loopback interfaces: (IPv6 index if it has a link-local address, IPv4 broadcast addresses).
fn interfaces() -> Vec<(Option<u32>, Vec<Ipv4Addr>)> {
    netdev::get_interfaces()
        .into_iter()
        .filter(|i| i.is_up() && !i.is_loopback())
        .map(|i| {
            let v6 = i.ipv6.iter().any(|n| (n.addr().segments()[0] & 0xffc0) == 0xfe80).then_some(i.index);
            let bc = i.ipv4.iter().map(|n| n.broadcast()).filter(|b| !b.is_loopback()).collect();
            (v6, bc)
        })
        .collect()
}

pub fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

impl Discovery {
    /// Start announcing this computer and listening for others.
    pub fn start(device: String, name: String, role: Role, port: u16) -> std::io::Result<Discovery> {
        let v4 = bind_v4().map_err(|e| {
            // 端口被占（独占绑定后第二个实例会到这里）：给一条可行动的提示，
            // 而不是裸系统错误码。
            if e.kind() == std::io::ErrorKind::AddrInUse {
                std::io::Error::new(
                    e.kind(),
                    format!("发现端口 {DISCOVERY_PORT} 已被占用：可能有另一个本应用实例正在运行（含异常退出残留），或端口被其他程序抢占。请结束旧实例（任务管理器结束 daozai.exe）或重启后再试。"),
                )
            } else {
                e
            }
        })?;
        let v6 = match bind_v6() {
            Ok(s) => Some(s),
            Err(e) => {
                log::info!("IPv6 discovery unavailable ({e}); direct-cable discovery limited to IPv4");
                None
            }
        };
        let id: u64 = crate::files::new_id();
        let beacon = Beacon { magic: MAGIC, version: PROTOCOL_VERSION, id, device, name, os: Os::current(), role, port };
        let payload = Arc::new(bincode::serialize(&beacon).expect("beacon"));
        // 广播常被路由器/AP 隔离吃掉，单播回信与单播兜底共用一个发送端。
        let probe = v4.try_clone()?;
        let peers: Arc<Mutex<HashMap<u64, Discovered>>> = Default::default();
        let stop = Arc::new(AtomicBool::new(false));
        let unicast: Arc<Mutex<Vec<Ipv4Addr>>> = Arc::new(Mutex::new(Vec::new()));

        // Sender: every interface, both IP versions.
        {
            let v4 = v4.try_clone()?;
            let v6 = v6.as_ref().map(|s| s.try_clone()).transpose()?;
            let stop = stop.clone();
            let unicast = unicast.clone();
            let payload = payload.clone();
            std::thread::Builder::new().name("discovery-tx".into()).spawn(move || {
                let mut joined: HashSet<u32> = HashSet::new();
                while !stop.load(Ordering::Relaxed) {
                    let mut targets: HashSet<Ipv4Addr> = HashSet::from([Ipv4Addr::BROADCAST]);
                    // 单播兜底：路由器AP 常丢弃客户端之间的广播，直接向已知对端 IP 发 beacon。
                    targets.extend(unicast.lock().iter().copied());
                    for (idx6, bcasts) in interfaces() {
                        targets.extend(bcasts);
                        if let (Some(idx), Some(s6)) = (idx6, v6.as_ref()) {
                            // Join on interfaces that appeared since last time (cable plugged in).
                            if joined.insert(idx) {
                                if let Err(e) = s6.join_multicast_v6(&GROUP_V6, idx) {
                                    log::debug!("join multicast on if {idx}: {e}");
                                }
                            }
                            let dest = SocketAddrV6::new(GROUP_V6, DISCOVERY_PORT, 0, idx);
                            if let Err(e) = s6.send_to(payload.as_slice(), dest) {
                                log::debug!("beacon v6 if {idx}: {e}");
                            }
                        }
                    }
                    for t in targets {
                        let _ = v4.send_to(payload.as_slice(), SocketAddr::from((t, DISCOVERY_PORT)));
                    }
                    std::thread::sleep(INTERVAL);
                }
            })?;
        }

        // Receivers.
        for sock in std::iter::once(v4).chain(v6) {
            let peers = peers.clone();
            let stop = stop.clone();
            let payload = payload.clone();
            std::thread::Builder::new().name("discovery-rx".into()).spawn(move || {
                let mut buf = [0u8; 1500];
                // 同一 IP 5 秒内只回一次，避免两台机器互相刷单播。
                let mut replied: HashMap<IpAddr, Instant> = HashMap::new();
                while !stop.load(Ordering::Relaxed) {
                    let Ok((n, from)) = sock.recv_from(&mut buf) else {
                        continue;
                    };
                    let Ok(b) = bincode::deserialize::<Beacon>(&buf[..n]) else {
                        continue;
                    };
                    if b.magic != MAGIC || b.id == id {
                        continue;
                    }
                    record(&peers, b, from);
                    // 广播被路由器过滤时，对端只能靠这条单播回信认识本机：
                    // 只要一方发出可被收到的包，双方的设备列表都能恢复。
                    let due = replied.get(&from.ip())
                        .map(|t| t.elapsed() > Duration::from_secs(5))
                        .unwrap_or(true);
                    if due {
                        replied.insert(from.ip(), Instant::now());
                        let _ = sock.send_to(payload.as_slice(), SocketAddr::new(from.ip(), DISCOVERY_PORT));
                    }
                }
            })?;
        }
        Ok(Discovery { peers, stop, unicast, payload, probe })
    }

    pub fn peers(&self) -> Vec<Discovered> {
        let mut map = self.peers.lock();
        map.retain(|_, p| p.last_seen.elapsed() < EXPIRY);
        // The same computer may restart with a new run id; keep the newest.
        let mut by_device: HashMap<String, Discovered> = HashMap::new();
        for p in map.values() {
            match by_device.get(&p.device) {
                Some(q) if q.last_seen >= p.last_seen => {}
                _ => {
                    by_device.insert(p.device.clone(), p.clone());
                }
            }
        }
        let mut v: Vec<_> = by_device.into_values().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn servers(&self) -> Vec<Discovered> {
        self.peers().into_iter().filter(|p| p.role == Role::Server).collect()
    }

    /// 向指定地址单播本机信标。路由器过滤广播 / AP 隔离时，只要还记得对端 IP，
    /// 单播一次就能让双方重新互相认识（对端收到后会按上面的逻辑回一条单播）。
    pub fn announce_to(&self, addrs: &[SocketAddr]) {
        for a in addrs {
            let _ = self.probe.send_to(self.payload.as_slice(), SocketAddr::new(a.ip(), DISCOVERY_PORT));
        }
    }
}

fn record(peers: &Mutex<HashMap<u64, Discovered>>, b: Beacon, from: SocketAddr) {
    let now = Instant::now();
    let addr = match from {
        SocketAddr::V6(a) => SocketAddr::V6(SocketAddrV6::new(*a.ip(), b.port, 0, a.scope_id())),
        SocketAddr::V4(a) => SocketAddr::new(IpAddr::V4(*a.ip()), b.port),
    };
    let mut map = peers.lock();
    let e = map.entry(b.id).or_insert_with(|| Discovered {
        name: b.name.clone(),
        device: b.device.clone(),
        os: b.os,
        role: b.role,
        version: b.version,
        addr,
        link: "network",
        last_seen: now,
        v4: None,
        v6: None,
    });
    e.name = b.name;
    e.role = b.role;
    e.last_seen = now;
    match addr {
        SocketAddr::V4(_) => e.v4 = Some((addr, now)),
        SocketAddr::V6(_) => e.v6 = Some((addr, now)),
    }
    // Prefer a routable IPv4 address seen recently, else IPv6 link-local.
    let fresh = |x: &Option<(SocketAddr, Instant)>| x.filter(|(_, t)| t.elapsed() < EXPIRY).map(|(a, _)| a);
    let v4 = fresh(&e.v4);
    let v6 = fresh(&e.v6);
    e.addr = match (v4, v6) {
        (Some(a), _) if !is_link_local(a.ip()) => a,
        (_, Some(b)) => b,
        (Some(a), None) => a,
        (None, None) => addr,
    };
    e.link = if is_link_local(e.addr.ip()) { "direct" } else { "network" };
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_routable_v4_then_link_local_v6() {
        let peers: Mutex<HashMap<u64, Discovered>> = Default::default();
        let b =
            Beacon { magic: MAGIC, version: PROTOCOL_VERSION, id: 7, device: "d".into(), name: "mac".into(), os: Os::MacOs, role: Role::Server, port: 24950 };
        let v6: SocketAddr = "[fe80::1%3]:5000".parse().unwrap();
        record(&peers, b.clone(), v6);
        let p = peers.lock()[&7].clone();
        assert_eq!(p.link, "direct");
        assert_eq!(p.addr.port(), 24950);
        record(&peers, b, "192.168.1.9:5000".parse().unwrap());
        let p = peers.lock()[&7].clone();
        assert_eq!(p.addr, "192.168.1.9:24950".parse::<SocketAddr>().unwrap());
        assert_eq!(p.link, "network");
    }

    // 单播兜底目标：只接受可路由 IPv4，过滤掉广播/回环/组播/unspecified/IPv6。
    #[test]
    fn unicast_target_filters_invalid_addresses() {
        assert_eq!(valid_unicast_target("192.168.1.20".parse().unwrap()), Some("192.168.1.20".parse().unwrap()));
        assert_eq!(valid_unicast_target("0.0.0.0".parse().unwrap()), None);
        assert_eq!(valid_unicast_target("255.255.255.255".parse().unwrap()), None);
        assert_eq!(valid_unicast_target("127.0.0.1".parse().unwrap()), None);
        assert_eq!(valid_unicast_target("224.0.0.1".parse().unwrap()), None);
        assert_eq!(valid_unicast_target("fe80::1".parse().unwrap()), None);
    }
}
