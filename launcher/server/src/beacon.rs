//! 服务端自广播：每两秒往发现端口发一条 `ServerAnnounce`。
//!
//! 客户端大厅页只监听、不探测：服务器活着就一直在列表里，进程一停，
//! 广播断掉超过几秒客户端就把它从列表里剔掉。
//!
//! 广播里报的是**对外地址**（`--public-host` 解析出来的地址），不是监听地址：
//! 监听 `[::]:5099` 时如果广播 `[::]`，客户端会用广播包的源 IPv4 兜底，
//! 注册中心那条却是公网 IPv6，同一台服会在大厅里裂成两行。

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

use bevy::log::{info, warn};
use bevy::prelude::*;
use crab_feast_library::net::{DISCOVERY_MAGIC, DISCOVERY_PORT, ServerAnnounce, TICK_HZ};

use crate::{MAX_PLAYERS, Room};

/// 广播间隔。客户端那边 4 秒没收到就认为服务器下线（留两倍丢包容忍）。
const INTERVAL: f32 = 2.0;

/// 受限广播地址：局域网内所有主机都能收到，不需要知道网段。
const BROADCAST: IpAddr = IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255));

#[derive(Resource)]
pub struct Beacon {
    socket: UdpSocket,
    timer: Timer,
    name: String,
    /// 对外公布、客户端用来直连的地址（不是本地监听地址）。
    announce_addr: SocketAddr,
}

/// 在 `setup` 里调：建一个独立 UDP socket（不占用游戏端口）并开始广播。
///
/// `announce_addr` 是给客户端连的对外地址；`name` 是大厅显示名，
/// 和注册中心用同一个，没配就退回 `host · 端口`。
pub fn start(commands: &mut Commands, announce_addr: SocketAddr, name: Option<String>) {
    let socket = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) {
        Ok(socket) => socket,
        Err(error) => {
            warn!("服务器广播 socket 创建失败：{error}（大厅里不会出现这台服务器）");
            return;
        }
    };
    if let Err(error) = socket.set_broadcast(true) {
        warn!("服务器广播开关失败：{error}");
        return;
    }

    let mut timer = Timer::from_seconds(INTERVAL, TimerMode::Repeating);
    // 第一帧就发一条：不想让玩家在大厅里干等两秒才看见服务器
    timer.set_elapsed(Duration::from_secs_f32(INTERVAL));

    info!("服务器发现广播：{}（udp:{DISCOVERY_PORT}）", announce_addr);
    commands.insert_resource(Beacon {
        socket,
        timer,
        name: name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| server_name(announce_addr)),
        announce_addr,
    });
}

pub fn server_name(addr: SocketAddr) -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "crab-feast".to_string());
    // 同机多开时靠端口区分，名字本身不需要唯一
    format!("{host} · {}", addr.port())
}

/// 计时走**真实时间**：电台是发现用途，不该被客户端拖动的世界时间速度牵着走
/// （`apply_time_scale_requests` 会把服务端 `Time<Virtual>` 的 relative speed 改掉，
/// 慢放到 ×0.42 时 2.0s 的广播周期会被拉成 4.8s，客户端那边就看着大厅条目
/// 亮一下灭一下）。
pub fn broadcast(time: Res<Time<bevy::time::Real>>, mut beacon: ResMut<Beacon>, room: Res<Room>) {
    if !beacon.timer.tick(time.delta()).just_finished() {
        return;
    }

    let announce = ServerAnnounce {
        magic: DISCOVERY_MAGIC.to_string(),
        name: beacon.name.clone(),
        // 报对外地址；客户端不需要再用包源 IP 猜
        addr: beacon.announce_addr.to_string(),
        port: beacon.announce_addr.port(),
        players: room.avatar_of_link.len().min(MAX_PLAYERS as usize) as u8,
        max_players: MAX_PLAYERS,
        tick_hz: TICK_HZ as f32,
    };
    let payload = match serde_json::to_vec(&announce) {
        Ok(payload) => payload,
        Err(error) => {
            warn!("服务器广播序列化失败：{error}");
            return;
        }
    };
    if let Err(error) = beacon
        .socket
        .send_to(&payload, SocketAddr::new(BROADCAST, DISCOVERY_PORT))
    {
        // 广播失败通常是本机没有可用网卡/被防火墙拦了，不该刷屏
        debug!("服务器广播发送失败：{error}");
    }
}
