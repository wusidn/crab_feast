//! 局域网服务器发现：监听服务端的 UDP 广播，维护一份「当前能连的服务器」。
//!
//! 服务端每两秒往 [`DISCOVERY_PORT`] 广播一条 [`ServerAnnounce`]；这里用一个
//! 后台线程收包（阻塞 socket 不能塞进 Bevy 的帧循环里），结果写进
//! [`ServerBrowser`]。大厅 UI 只读快照，超过 [`STALE_AFTER`] 没再出现的服务器
//! 自动从列表里消失。
//!
//! 同机多开客户端时，所有进程都绑同一个端口，所以 unix 上必须开 `SO_REUSEPORT`；
//! 只开 `SO_REUSEADDR` 的话第二个客户端会 bind 失败，只能看到一个空列表。

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use bevy::log::{info, warn};
use bevy::prelude::*;
use crab_feast_library::net::{DISCOVERY_MAGIC, DISCOVERY_PORT, ServerAnnounce};
use socket2::{Domain, Protocol, Socket, Type};

/// 一条广播多久没再收到就从列表里消失。
///
/// 广播间隔 2 秒，留两倍余量：丢一两个包不至于让服务器在列表里闪烁。
const STALE_AFTER: Duration = Duration::from_secs(4);

/// 大厅列表里的一台服务器。
#[derive(Clone, Debug)]
pub struct DiscoveredServer {
    pub addr: SocketAddr,
    pub name: String,
    pub players: u8,
    pub max_players: u8,
    pub last_seen: Instant,
}

/// 服务器列表的共享存储：后台线程写，UI 帧里读。
#[derive(Resource)]
pub struct ServerBrowser {
    inner: Arc<Mutex<HashMap<SocketAddr, DiscoveredServer>>>,
    stop: Arc<AtomicBool>,
}

impl ServerBrowser {
    /// 启动监听线程。端口绑不上（被别的程序占了且不支持复用）时只报一次警告，
    /// 大厅会显示「正在搜索」，本地游戏仍然可用。
    pub fn start() -> Self {
        let inner = Arc::new(Mutex::new(HashMap::<SocketAddr, DiscoveredServer>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let store = inner.clone();
        let stop_flag = stop.clone();
        if let Err(error) = thread::Builder::new()
            .name("server-discovery".to_string())
            .spawn(move || listen(store, stop_flag))
        {
            warn!("服务器发现线程启动失败：{error}");
        }
        Self { inner, stop }
    }

    /// 当前在线的服务器，按名字排序（名字相同按地址）。
    pub fn snapshot(&self) -> Vec<DiscoveredServer> {
        let Ok(mut guard) = self.inner.lock() else {
            return Vec::new();
        };
        let now = Instant::now();
        guard.retain(|_, server| now.duration_since(server.last_seen) <= STALE_AFTER);
        let mut servers: Vec<_> = guard.values().cloned().collect();
        servers.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.addr.cmp(&b.addr)));
        servers
    }
}

impl Drop for ServerBrowser {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub struct DiscoveryPlugin;

impl Plugin for DiscoveryPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ServerBrowser::start());
    }
}

fn listen(store: Arc<Mutex<HashMap<SocketAddr, DiscoveredServer>>>, stop: Arc<AtomicBool>) {
    let socket = match bind() {
        Ok(socket) => socket,
        Err(error) => {
            warn!("服务器发现端口 {DISCOVERY_PORT} 绑定失败：{error}（列表会是空的）");
            return;
        }
    };
    info!("正在监听服务器广播 udp:{DISCOVERY_PORT}");
    let mut buffer = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        let (len, source) = match socket.recv_from(&mut buffer) {
            Ok(packet) => packet,
            // 读超时：回到循环顶部检查停止标志
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => {
                warn!("服务器广播接收失败：{error}");
                thread::sleep(Duration::from_millis(300));
                continue;
            }
        };
        let Ok(text) = std::str::from_utf8(&buffer[..len]) else {
            continue;
        };
        let Ok(announce) = serde_json::from_str::<ServerAnnounce>(text) else {
            continue;
        };
        if announce.magic != DISCOVERY_MAGIC {
            continue;
        }
        let addr = announce_addr(&announce, source);
        let Ok(mut guard) = store.lock() else {
            continue;
        };
        let name = announce.name.clone();
        let previous = guard.insert(
            addr,
            DiscoveredServer {
                addr,
                name: announce.name,
                players: announce.players,
                max_players: announce.max_players,
                last_seen: Instant::now(),
            },
        );
        if previous.is_none() {
            info!("发现服务器 {name}：{addr}");
        }
    }
}

/// 广播里写的地址能直接用就用；写的是 `0.0.0.0`（服务端绑了所有网卡）
/// 就退回「包的源 IP + 广播里的端口」。
fn announce_addr(announce: &ServerAnnounce, source: SocketAddr) -> SocketAddr {
    match announce.addr.parse::<SocketAddr>() {
        Ok(addr) if !addr.ip().is_unspecified() => addr,
        _ => SocketAddr::new(source.ip(), announce.port),
    }
}

fn bind() -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DISCOVERY_PORT);
    socket.bind(&address.into())?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    Ok(socket.into())
}
