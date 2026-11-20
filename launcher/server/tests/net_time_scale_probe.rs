//! 世界时间速度复制链路的无头探针（临时验证用）。
//!
//! 起一个只有 ServerPlugins 的 App 和一个只有 ClientPlugins 的 App，走真实的
//! UDP + netcode，确认 `NetTimeScale` 会不会从服务端复制到客户端。

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use bevy::{prelude::*, time::TimeUpdateStrategy, state::app::StatesPlugin};
use crab_feast_library::net::{
    CrabProtocolPlugin, NETCODE_DEV_KEY, NETCODE_PROTOCOL_ID, NetTimeScale, TICK_HZ,
};
use lightyear::{
    netcode::prelude::{
        Authentication,
        client::{NetcodeClient, NetcodeConfig},
        server::{NetcodeConfig as ServerNetcodeConfig, NetcodeServer},
    },
    prelude::{client::*, server::*, *},
};

const ADDR: &str = "127.0.0.1:5177";

fn build_server(addr: SocketAddr) -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        StatesPlugin,
        ServerPlugins {
            tick_duration: Duration::from_secs_f64(1.0 / TICK_HZ),
        },
        CrabProtocolPlugin,
    ))
    .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(16)));
    app.add_observer(|trigger: On<Add<LinkOf>>, mut commands: Commands| {
        commands.entity(trigger.entity).insert(ReplicationSender);
    });

    let server = app
        .world_mut()
        .spawn((
            ServerUdpIo,
            LocalAddr(addr),
            NetcodeServer::new(
                ServerNetcodeConfig::default()
                    .with_protocol_id(NETCODE_PROTOCOL_ID)
                    .with_key(NETCODE_DEV_KEY)
                    .with_max_clients(8),
            ),
        ))
        .id();
    app.world_mut().trigger(Start { entity: server });
    app.world_mut().spawn((Replicate::default(), NetTimeScale(1.0)));
    app.finish();
    app
}

fn build_client(addr: SocketAddr) -> (App, Arc<Mutex<Vec<(f32, bool)>>>) {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        StatesPlugin,
        ClientPlugins {
            tick_duration: Duration::from_secs_f64(1.0 / TICK_HZ),
        },
        CrabProtocolPlugin,
    ))
    .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(16)));

    let netcode = NetcodeClient::new(
        Authentication::Manual {
            server_addr: addr,
            client_id: 7,
            private_key: NETCODE_DEV_KEY,
            protocol_id: NETCODE_PROTOCOL_ID,
        },
        NetcodeConfig::default(),
    )
    .expect("netcode client");

    let client = app
        .world_mut()
        .spawn((
            Client,
            netcode,
            UdpIo::default(),
            LocalAddr(SocketAddr::from(([0, 0, 0, 0], 0))),
            PeerAddr(addr),
            ReplicationReceiver,
            MessageManager::default(),
        ))
        .id();
    app.world_mut().trigger(Connect { entity: client });

    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    app.add_systems(
        Update,
        move |replicas: Query<&NetTimeScale>,
              connected: Query<Entity, With<Connected>>,
              mut last: Local<Option<f32>>| {
            let is_connected = !connected.is_empty();
            let value = replicas.iter().next().map(|v| v.0);
            if *last != value {
                *last = value;
                sink.lock().unwrap().push((value.unwrap_or(-1.0), is_connected));
            }
        },
    );
    app.finish();
    (app, seen)
}

#[test]
fn net_time_scale_replicates_to_client() {
    let addr: SocketAddr = ADDR.parse().unwrap();

    // App 不是 Send（运行器闭包），只能在目标线程里构造
    let server_thread = thread::spawn(move || {
        let mut server = build_server(addr);
        for frame in 0..400 {
            if frame == 200 {
                let mut clock = server
                    .world_mut()
                    .query::<&mut NetTimeScale>()
                    .iter_mut(server.world_mut())
                    .next()
                    .expect("世界时钟不存在");
                clock.0 = 0.2;
                println!("[server] 第 {frame} 帧把时间速度改成 0.2");
            }
            server.update();
            thread::sleep(Duration::from_millis(1));
        }
    });

    // 等服务器先把 endpoint 绑上，再起客户端
    thread::sleep(Duration::from_millis(100));
    let (mut client, seen) = build_client(addr);

    for _ in 0..400 {
        client.update();
        thread::sleep(Duration::from_millis(1));
    }
    server_thread.join().unwrap();

    let seen = seen.lock().unwrap().clone();
    println!("[client] 观察到的 NetTimeScale 变化：{seen:?}");
    assert!(
        seen.iter().any(|(value, _)| (value - 0.2).abs() < 1e-4),
        "客户端没有收到 0.2，实际观察到 {seen:?}"
    );
}
