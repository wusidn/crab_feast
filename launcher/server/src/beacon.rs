//! 服务端自广播：每两秒往发现端口发一条 `ServerAnnounce`。
//!
//! 客户端大厅页只监听、不探测：服务器活着就一直在列表里，进程一停，
//! 广播断掉超过几秒客户端就把它从列表里剔掉。地址绑到 `0.0.0.0` 时客户端
//! 用广播包的源 IP 兜底，所以服务端不需要知道自己该报哪张网卡的地址。

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
    addr: SocketAddr,
}

/// 在 `setup` 里调：建一个独立 UDP socket（不占用游戏端口）并开始广播。
pub fn start(commands: &mut Commands, addr: SocketAddr) {
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

    info!("服务器发现广播：{}（udp:{DISCOVERY_PORT}）", addr);
    commands.insert_resource(Beacon {
        socket,
        timer,
        name: server_name(addr),
        addr,
    });
}

fn server_name(addr: SocketAddr) -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "crab-feast".to_string());
    // 同机多开时靠端口区分，名字本身不需要唯一
    format!("{host} · {}", addr.port())
}

pub fn broadcast(time: Res<Time>, mut beacon: ResMut<Beacon>, room: Res<Room>) {
    if !beacon.timer.tick(time.delta()).just_finished() {
        return;
    }

    let announce = ServerAnnounce {
        magic: DISCOVERY_MAGIC.to_string(),
        name: beacon.name.clone(),
        // 如实报监听地址：绑 0.0.0.0 时客户端会用源 IP 替换
        addr: beacon.addr.to_string(),
        port: beacon.addr.port(),
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
