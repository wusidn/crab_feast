//! 无窗口的专用服务器（dedicated server）。
//!
//! 它只跑「权威模拟 + 复制」：物理、角色移动、状态广播，不加载任何美术资源，
//! 也不创建窗口。客户端 `crab_feast_pc` 通过 `--server <addr>` 连过来。
//!
//! 启动：`cargo run -p crab_feast_server -- --addr 0.0.0.0:5000`

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    time::Duration,
};

use avian3d::prelude::*;
use bevy::{
    log::{Level, LogPlugin},
    prelude::*,
    state::app::StatesPlugin,
    transform::TransformPlugin,
};
use crab_feast::anim::jump::{JumpKind, JumpProfile, JumpTunings};
use crab_feast::ground;
use crab_feast::level::{PLAYER_SPAWN_Y, spawn_static_level};
use crab_feast_launcher::config::{LauncherConfig, config_path_from_args};
use crab_feast_library::net::{
    AIR_ACCEL_RANGE, AIR_STEER_SECS_RANGE, ControlChannel, CrabProtocolPlugin, DEFAULT_ADDR,
    DEFAULT_REGISTRY_URL, JUMP_LAUNCH_VY_RANGE, JoinRequest, MOVE_SPEED_MPS, MoveInput,
    NETCODE_DEV_KEY, NETCODE_PROTOCOL_ID, NetJump, NetMotion, NetPlayerId, NetPos, NetTick,
    NetTimeScale, NetYaw, TICK_HZ, TimeScaleRequest, Welcome, air_steer_active, air_steer_velocity,
};
mod beacon;
mod registration;

use crab_feast_library::tuning;
use lightyear::{
    netcode::prelude::server::{NetcodeConfig, NetcodeServer},
    prelude::{server::*, *},
};

/// 跳跃的蓄力时长 / 离地速度由 `crab_feast::anim::jump::JumpTunings` 统一定义
/// （跑跳 / 站跳 / 上跳 / 下跳各一档）。服务端不再自己写死一份数字：客户端调哪一档，
/// 这里跟着一起变，两端不会再有「本地 2.6 / 服务端 5.0」的分叉。
///
/// 站 / 跑蓄力分档的地速分界（米/秒）。直接引用客户端资源参数表，两端不再各抄一份。
/// 只给测试断言用：正式分档走 `JumpProfile::for_jump`。
#[cfg(test)]
const RUN_JUMP_SPEED: f32 = crab_feast::anim::jump::RULES.run_jump_speed;
/// 贴地接触噪声允许的竖直速度（米/秒）。avian 的接触求解会留下 ±0.2 m/s
/// 量级的残余速度（实测 0.192 反复出现），拿 0.1 当界线会把贴地当成「在蹬升」，
/// 蓄力每 tick 被清掉、跳跃永远不触发。界线要拉开噪声一个量级。
const GROUND_NOISE_VY: f32 = 0.5;
/// 起跳请求到达时服务端还没判接地，保留多久重试窗口（秒）。
const REQUEST_RETRY_SECS: f32 = 0.1;
/// 起跳写速后的验证窗口（秒）。物理每 tick 都在解算，0.12s 足够看出根有没有被抬起来；
/// 再长就成了「人已经飞出去半米，服务端才回头认账」。
const LAUNCH_VERIFY_SECS: f32 = 0.12;
/// 验证时判定「根真的抬起来了」的最小位移（米）。
///
/// 顶着 5 m/s 的离地速度，0.12s 里正常起跳至少能抬 0.5m；被顶棚挡住时离散求解器
/// 也只来得及穿进去几厘米（随后被推回来）。取 0.15m：只认「像一次起跳」的抬升，
/// 被顶棚削成几厘米的那一下照样判失败。
const LAUNCH_MIN_RISE: f32 = 0.15;
/// 一个房间最多多少个玩家位。
///
/// `player_id` 是 u8，但真正的上限是「复制带宽 ÷ 每人下行」，
/// 100 是当前设计目标，超过就拒绝，别让编号偷偷回绕。
const MAX_PLAYERS: u8 = 100;

/// 房间状态：连接 → 角色实体 的映射。
#[derive(Resource, Default)]
struct Room {
    /// link 实体（lightyear 为每个连接生成一个）→ 该玩家的角色实体
    avatar_of_link: HashMap<Entity, Entity>,
}

/// 模拟 tick 计数：每发布一次状态自增，跟 `NetPos` 一起复制给客户端。
///
/// 客户端拿它当插值时间轴，不用收包时刻——网络抖动不会改变快照之间的模拟间隔。
#[derive(Resource, Default)]
struct NetTickCounter(u32);

/// 找出最小的空闲玩家编号；满员返回 `None`。
///
/// 不能只增不减：编号决定出生点的 X，一路递增的话每重连一次就往后挪 1.5m，
/// 几次之后就挪出地面，角色一出生就在空中往下掉。
fn free_player_id(used: &HashSet<u8>) -> Option<u8> {
    (0..MAX_PLAYERS).find(|id| !used.contains(id))
}

/// 出生位：编号铺成 20 列 × 5 行的网格，整块出生区都落在地面内。
///
/// - 行距 2m：z 只取 0/2/4/6/8，刚好避开场景方块占的 z∈[0.5,1.5] 与 [2.5,3.5]；
/// - 列距 1.5m：x 取 0…28.5，加胶囊半径也不出 30m 半宽的地面；
/// - 编号 0 仍然落在原点，单人局的出生位和以前一致。
fn spawn_point(player_id: u8) -> Vec3 {
    const COLUMNS: u8 = 20;
    const COLUMN_SPACING: f32 = 1.5;
    const ROW_SPACING: f32 = 2.0;
    let column = (player_id % COLUMNS) as f32;
    let row = (player_id / COLUMNS) as f32;
    Vec3::new(column * COLUMN_SPACING, PLAYER_SPAWN_Y, row * ROW_SPACING)
}

/// 标记：这个连接已经 Join 过，角色实体已创建（防止重复 Join 再建一个）。
#[derive(Component)]
struct Joined(u8);

fn main() {
    let args = parse_args();

    let mut app = App::new();
    // 无渲染的服务器仍要跑 Avian：它的碰撞体后端会读 Mesh 资产，
    // 所以资产系统必须就位（不加载任何美术资源，只是把类型注册起来）
    app.add_plugins(AssetPlugin::default()).init_asset::<Mesh>();
    app.add_message::<AssetEvent<Mesh>>();
    app.add_plugins((
        MinimalPlugins,
        LogPlugin {
            level: Level::INFO,
            filter: "wgpu=error,lightyear=info,lightyear_transport=info".to_string(),
            ..default()
        },
        TransformPlugin,
        // lightyear 的连接状态机要 StateTransition，MinimalPlugins 里没有
        StatesPlugin,
        PhysicsPlugins::default(),
        ServerPlugins {
            tick_duration: Duration::from_secs_f64(1.0 / TICK_HZ),
        },
        // 连接层（netcode）自己会装 connection::server::ConnectionPlugin，重复添加会 panic
        // 协议必须在 Server 插件之后、任何连接实体之前注册
        CrabProtocolPlugin,
    ))
    .insert_resource(Room::default())
    .init_resource::<NetTickCounter>()
    // 四档跳跃调参：站 / 跑 / 上 / 下。与客户端默认值同源，调一处两端一致。
    .init_resource::<JumpTunings>()
    .insert_resource(ServerAddr(args.addr))
    // 大厅显示名先解析一次，广播和注册中心共用，避免两边名字不一致
    .insert_resource(ServerDisplayName(args.display_name))
    // 注册配置在启动系统里消费一次；`None` 表示不注册（--no-registry / "off"）
    .insert_resource(RegistrySettings(args.registry))
    // 物理与轻年都跑 FixedUpdate，把步长对齐到同一个 tick，避免两边时钟各走各的
    .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
    .add_systems(Startup, setup)
    .add_systems(Update, (beacon::broadcast, publish_player_count))
    .add_observer(on_new_link)
    .add_observer(on_disconnect)
    .add_systems(
        Update,
        handle_join.after(lightyear::prelude::MessageSystems::ReceiveMessages),
    )
    // Avian 的物理跑在 FixedPostUpdate 里，所以：
    // - 输入写速度放在 FixedUpdate（天然早于物理）
    // - 状态发布放在 FixedPostUpdate 的 Writeback 之后（读到的才是本 tick 的结果）
    .add_systems(
        Update,
        collect_inputs.after(lightyear::prelude::MessageSystems::ReceiveMessages),
    )
    // 世界时间速度：客户端拖滑杆只是"提议"，真正的值在这里改写并复制下去
    .add_systems(
        Update,
        apply_time_scale_requests.after(lightyear::prelude::MessageSystems::ReceiveMessages),
    )
    .add_systems(FixedUpdate, drive_avatars)
    .add_systems(
        FixedPostUpdate,
        (measure_speed, face_movement, publish_state)
            .chain()
            .after(PhysicsSystems::Writeback),
    );

    if let Some(secs) = args.exit_after {
        app.insert_resource(ExitAfter(Timer::from_seconds(secs, TimerMode::Once)))
            .add_systems(Update, exit_when_elapsed);
    }

    if args.log_state {
        // FixedUpdate 是 60Hz，除以 60 就是每秒一次
        app.add_systems(FixedUpdate, log_state.run_if(every_second));
    }

    app.run();
}

/// 每 60 个固定 tick 放行一次。
fn every_second(mut counter: Local<u32>) -> bool {
    *counter = counter.wrapping_add(1);
    *counter % 60 == 0
}

#[derive(Resource)]
struct ServerAddr(SocketAddr);

/// 大厅显示名。`None`/空表示按 `host · 端口` 自动生成；广播和注册中心共用。
#[derive(Resource)]
struct ServerDisplayName(Option<String>);

/// 注册配置。`None` 表示不向注册中心报到（`--no-registry` 或配置成 `"off"`）。
///
/// 用资源而不是直接传参给 `setup`：`setup` 是 Startup 系统，参数越多越容易碰到
/// Bevy 的系统参数上限；配置在 `main` 里解析完就固定了，资源是最自然的载体。
#[derive(Resource)]
struct RegistrySettings(Option<registration::Config>);

/// 把房间人数同步给注册线程。心跳线程读原子，不碰 ECS，所以不用加锁。
fn publish_player_count(room: Res<Room>, registration: Option<Res<registration::Registration>>) {
    if let Some(registration) = registration {
        let players = room.avatar_of_link.len().min(MAX_PLAYERS as usize) as u8;
        registration.set_players(players);
    }
}

/// 世界时钟实体（`NetTimeScale` 的持有者，见 [`setup`]）。
#[derive(Component)]
struct WorldClock;

/// 把客户端的世界时间速度请求落成权威值。
///
/// 三件事一起做：夹区间 → 写 `NetTimeScale`（复制给所有端） → 改本地 `Time<Virtual>`。
/// 服务器自己也得慢下来，否则客户端慢放时服务器照旧 60Hz 推进，复制下来的位置依旧
/// 是"实时"的，慢放看到的就是"动画在原地抖腿"。
///
/// 只认最新一条：拖动时客户端每个节流窗口发一条，中间的旧值没有意义。
fn apply_time_scale_requests(
    mut links: Query<&mut MessageReceiver<TimeScaleRequest>>,
    mut clock: Query<&mut NetTimeScale, With<WorldClock>>,
    mut time: ResMut<Time<Virtual>>,
) {
    for mut receiver in links.iter_mut() {
        let mut newest = None;
        for request in receiver.receive() {
            newest = Some(request);
        }
        let Some(request) = newest else {
            continue;
        };
        let value = NetTimeScale::clamp_speed(request.value);
        let Some(mut scale) = clock.iter_mut().next() else {
            warn!("世界时钟实体不在，忽略时间速度请求");
            continue;
        };
        if (scale.0 - value).abs() < 1e-4 {
            continue;
        }
        scale.0 = value;
        time.set_relative_speed(value);
        info!("world time scale → ×{value:.2}");
    }
}

/// `--exit-after <秒>`：跑够时间自动退出。给 CI / 冒烟测试用，
/// 也免得手工起服务器时还得找进程杀。
#[derive(Resource)]
struct ExitAfter(Timer);

fn exit_when_elapsed(
    // 真实时间：`--exit-after` 是给 CI 的墙钟预算，别被慢放拉长
    time: Res<Time<bevy::time::Real>>,
    mut timer: ResMut<ExitAfter>,
    mut exit: MessageWriter<AppExit>,
) {
    if timer.0.tick(time.delta()).just_finished() {
        info!("--exit-after 到点，服务器退出");
        exit.write(AppExit::Success);
    }
}

/// 启动参数解析结果。
struct Args {
    addr: SocketAddr,
    exit_after: Option<f32>,
    log_state: bool,
    /// 大厅显示名：`--name` > launcher.json > `CRAB_SERVER_NAME`
    display_name: Option<String>,
    /// `None` = 不向注册中心报到
    registry: Option<registration::Config>,
}

fn parse_args() -> Args {
    let config = LauncherConfig::load(config_path_from_args().as_deref());
    let mut args = std::env::args().skip(1);
    // 优先级：--addr > launcher.json 的 server.addr > 协议默认地址
    let mut addr: Option<String> = None;
    let mut exit_after: Option<f32> = None;
    let mut log_state = false;
    // 注册相关：命令行 > launcher.json > 环境变量 > 内置默认
    let mut registry_url: Option<String> = None;
    let mut public_host = config.server.public_host.clone();
    let mut name = config.server.name.clone();
    let mut no_registry = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" => {
                if let Some(value) = args.next() {
                    addr = Some(value);
                }
            }
            "--config" => {
                // 已经提前读过，这里只跳过值
                let _ = args.next();
            }
            "--registry" => {
                if let Some(value) = args.next() {
                    registry_url = Some(value);
                }
            }
            "--no-registry" => no_registry = true,
            "--public-host" => {
                if let Some(value) = args.next() {
                    public_host = Some(value);
                }
            }
            "--name" => {
                if let Some(value) = args.next() {
                    name = Some(value);
                }
            }
            "--exit-after" => {
                if let Some(value) = args.next() {
                    exit_after = value.parse().ok();
                }
            }
            "--log-state" => log_state = true,
            "--help" | "-h" => {
                println!(
                    "crab-feast-server [--addr 0.0.0.0:5000] [--config launcher.json] \
                     [--registry http://127.0.0.1:8090] [--no-registry] \
                     [--public-host <域名/IP>] [--name <大厅显示名>] \
                     [--exit-after <秒>] [--log-state]\n\
                     不传 --addr 时读 launcher.json 的 server.addr。\n\
                     默认会向服务器注册中心报到；地址可用 CRAB_REGISTRY_URL 覆盖。\n\
                     对外地址默认取监听 IP；绑 0.0.0.0 时自动探测本机网卡，\n\
                     跨公网部署请用 --public-host 指定域名或公网 IP。"
                );
                std::process::exit(0);
            }
            other => eprintln!("未知参数 {other}，已忽略"),
        }
    }
    let addr = addr
        .or_else(|| config.server.addr.clone())
        .unwrap_or_else(|| DEFAULT_ADDR.to_string());
    let addr = addr.parse().unwrap_or_else(|error| {
        eprintln!("地址 {addr} 无法解析：{error}");
        std::process::exit(2);
    });

    let registry_url = registry_url
        .or_else(|| config.server.registry.clone())
        .or_else(|| env_nonempty("CRAB_REGISTRY_URL"))
        .unwrap_or_else(|| DEFAULT_REGISTRY_URL.to_string());
    let display_name = name.or_else(|| env_nonempty("CRAB_SERVER_NAME"));
    let registry = if no_registry || is_off(&registry_url) {
        None
    } else {
        Some(registration::Config {
            url: registry_url,
            public_host: public_host.or_else(|| env_nonempty("CRAB_PUBLIC_HOST")),
            name: display_name.clone(),
        })
    };

    Args {
        addr,
        exit_after,
        log_state,
        display_name,
        registry,
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// `"off"` / `"none"` / `"disabled"` / 空串都视为关闭注册。
fn is_off(url: &str) -> bool {
    let url = url.trim();
    url.is_empty()
        || url.eq_ignore_ascii_case("off")
        || url.eq_ignore_ascii_case("none")
        || url.eq_ignore_ascii_case("disabled")
}

/// 每秒打印一次所有玩家的位置与朝向：验证「输入→位移→复制」时不用开客户端观察器。
fn log_state(players: Query<(&NetPlayerId, &Transform, &NetYaw, &NetMotion)>) {
    for (id, transform, yaw, motion) in players.iter() {
        info!(
            "player {} pos {:?} yaw {:.1}° intent {:.2} speed {:.2} dir {:?}",
            id.0,
            transform.translation,
            yaw.0.to_degrees(),
            motion.intent,
            motion.speed,
            motion.dir
        );
    }
}

fn setup(
    mut commands: Commands,
    addr: Res<ServerAddr>,
    registry: Res<RegistrySettings>,
    display_name: Res<ServerDisplayName>,
) {
    // 一个 UDP endpoint：监听 addr，每个远端地址自动派生子 link 实体
    let server = commands
        .spawn((
            ServerUdpIo,
            LocalAddr(addr.0),
            // 连接层：netcode 负责握手、加密、超时踢人、重连 token。
            // 只有裸 UdpIo 的话实体永远不会进入 Connected，什么都不会发出去
            NetcodeServer::new(
                NetcodeConfig::default()
                    .with_protocol_id(NETCODE_PROTOCOL_ID)
                    .with_key(NETCODE_DEV_KEY)
                    .with_max_clients(128),
            ),
            Name::new("server"),
        ))
        .id();
    commands.trigger(Start { entity: server });

    // 关卡（地面 + 方块）：静态场景由两端各自构建，服务端这份就是权威碰撞几何。
    // 客户端用同一份 `LEVEL` 在本地建实体，所以这里既不需要复制，也不会两边分叉
    spawn_static_level(&mut commands);

    // 世界时钟：整局唯一一个「世界时间速度」的权威持有者。
    //
    // 它必须是一个**实体**而不是资源：lightyear 的复制只能挂在实体组件上。
    // 客户端不需要知道它是谁，只按「谁身上有 `NetTimeScale`」读值。
    commands.spawn((
        WorldClock,
        Replicate::default(),
        NetTimeScale::default(),
        Name::new("world-clock"),
    ));

    info!("dedicated server listening on {}", addr.0);

    // 广播和注册中心必须报同一个对外地址：绑定 [::] 时如果广播 [::]，
    // 客户端会退回广播包的源 IPv4，而注册中心是公网 IPv6，大厅里会裂成两行。
    let announce_addr = registry
        .0
        .as_ref()
        .and_then(|config| config.public_host.as_deref())
        .and_then(|host| registration::advertised_addr(host, addr.0.port()))
        .unwrap_or(addr.0);

    // 定时 UDP 广播自己的地址/端口/人数，客户端大厅直接收，不用手输地址。
    // 名字和注册中心用同一个：否则同一台服务器两条来源会各报一个名字，
    // 大厅合并成一行后名字会在两边之间来回跳。
    beacon::start(&mut commands, announce_addr, display_name.0.clone());

    // 向中央 Web 服务注册：跨网段/公网的大厅靠这条，不再依赖 UDP 广播。
    // 注册线程自己重试、心跳、退出注销，这里只管把它挂到 App 上。
    if let Some(config) = registry.0.clone() {
        let config = registration::Config {
            name: display_name.0.clone(),
            ..config
        };
        if let Some(registration) = registration::Registration::start(addr.0, config, MAX_PLAYERS) {
            commands.insert_resource(registration);
        }
    }
}

/// 连接建立：只把这条连接需要的收发能力装上，**不建角色**。
///
/// 角色要等客户端发 `JoinRequest` 才创建——连接 ≠ 玩家，
/// 这样"握手成功但没进房间"的连接不会占玩家位、也不会被复制给别人。
fn on_new_link(trigger: On<Add<LinkOf>>, mut commands: Commands, room: Res<Room>) {
    let link = trigger.entity;
    commands.entity(link).insert((
        ReplicationSender,
        MessageSender::<Welcome>::default(),
        MessageSender::<JoinRequest>::default(),
        InputGrace(0.0),
        LatestInput::default(),
        InputQueue {
            cap: 720,
            ..InputQueue::default()
        },
        // 编号只是给服务器日志/调试看的，真正的 player_id 在 Join 之后才有
        Name::new(format!("link-{}", room.avatar_of_link.len())),
    ));
}

/// 登录：收到 `JoinRequest` 才分配玩家编号、生成角色实体、回 `Welcome`。
///
/// 重复 Join 直接忽略（`Joined` 已存在），防止客户端重发消息把自己变成两个人。
fn handle_join(
    mut commands: Commands,
    mut room: ResMut<Room>,
    mut links: Query<(
        Entity,
        &mut MessageReceiver<JoinRequest>,
        Option<&Joined>,
        &mut MessageSender<Welcome>,
    )>,
    avatars: Query<&NetPlayerId>,
) {
    // 本帧已经用掉的编号也先记进来：同一帧两个连接同时 Join 时，
    // 后面那个不能看到同一个「空闲 0」而和前面的人分到同一个出生位
    let mut used: HashSet<u8> = avatars.iter().map(|player| player.0).collect();
    for (link, mut receiver, joined, mut sender) in links.iter_mut() {
        let mut requested = false;
        // `JoinRequest` 是空消息，收到几条都一样，只关心"有没有"
        for _ in receiver.receive() {
            requested = true;
        }
        if !requested {
            continue;
        }
        if let Some(joined) = joined {
            debug!(
                "重复 Join 已忽略（link {link:?} 已经是 player {}）",
                joined.0
            );
            continue;
        }

        let Some(player_id) = free_player_id(&used) else {
            warn!("房间已满（{MAX_PLAYERS}），拒绝 {link:?} 的 Join");
            continue;
        };
        used.insert(player_id);

        // 出生点按编号铺网格，避免多个胶囊叠在一起，也不会铺到地面外面
        let spawn = spawn_point(player_id);
        let avatar = commands
            .spawn((
                Transform::from_translation(spawn),
                RigidBody::Dynamic,
                Collider::capsule(0.2, 1.0),
                LockedAxes::ROTATION_LOCKED,
                LinearDamping(0.3),
                AngularDamping(0.3),
                LinearVelocity::ZERO,
                Replicate::default(),
                NetPlayerId(player_id),
                NetPos(spawn.to_array()),
                NetYaw(0.0),
                NetTick(0),
                NetMotion {
                    dir: [0.0, 0.0],
                    speed: 0.0,
                    intent: 0.0,
                },
                AvatarLocal::default(),
                Name::new(format!("player-{player_id}")),
            ))
            // 站着不动几秒物理会把刚体睡过去，之后写入的速度不再积分——服务端
            // 若也睡下去，玩家一推杆只会看到人极慢地蹭。与客户端同参数。
            // （不放进上面的元组：Bevy 的元组 Bundle 只实现到 15 个元素）
            .insert(SleepingDisabled)
            .id();

        room.avatar_of_link.insert(link, avatar);
        commands.entity(link).insert(Joined(player_id));
        sender.send::<ControlChannel>(Welcome { player_id });
        info!("join: link {link:?} → player {player_id}（角色实体 {avatar:?}）");
    }
}

/// 连接断开：把它的角色实体一起收掉，并释放映射。
///
/// 房间清空时顺手把世界时间速度还给 1.0：调试用的慢放不该留给下一批进来的人
/// （客户端那边 `DebugSettingsPlugin` 在退出对局时同样会复位）。
fn on_disconnect(
    trigger: On<Add<Disconnected>>,
    mut commands: Commands,
    mut room: ResMut<Room>,
    mut clock: Query<&mut NetTimeScale, With<WorldClock>>,
    mut time: ResMut<Time<Virtual>>,
) {
    let link = trigger.entity;
    let Some(avatar) = room.avatar_of_link.remove(&link) else {
        return;
    };
    commands.entity(avatar).despawn();
    info!("leave: link {link:?} 断开，角色实体 {avatar:?} 已回收");

    if room.avatar_of_link.is_empty()
        && let Some(mut scale) = clock.iter_mut().next()
    {
        if (scale.0 - 1.0).abs() > 1e-4 {
            scale.0 = 1.0;
            time.set_relative_speed(1.0);
            info!("房间清空，世界时间速度复位为 ×1.00");
        }
    }
}

/// 输入断流后多久把水平速度归零（秒）。
///
/// 输入通道是不可靠的，真丢包时不归零角色会一直按最后一次速度滑行。
const INPUT_GRACE_SECS: f32 = 0.2;

/// 这个连接最近一次收到的输入。
///
/// 收包在 `PreUpdate`、清空在 `Last`，而 `FixedUpdate` 的步数取决于帧率：
/// 逻辑帧落在哪些帧是不确定的。所以收包只负责「存下来」（`Update`），
/// 真正写速度放到 `FixedUpdate`，两边都不依赖对方的时间点。
#[derive(Component, Default)]
struct LatestInput(MoveInput);

/// 客户端输入按「该生效的 tick」排队。
///
/// 这是两端相位的对齐点。以前输入是「到达即执行」：客户端在自己那一帧就反向/松手，
/// 服务端要等包到（~3-6 tick）才做同一件事，于是两条轨迹整体错开
/// ≈ 速度 × 单程延迟（180° 甩头实测 20~35cm，和解只能把人往回拽一下）。
/// 客户端在每条输入里带上它本地预测的 tick（与服务端 `NetTick` 同一条时间轴），
/// 服务端排进缓冲、到点的那个 tick 再取用，两端就逐 tick 同源。
#[derive(Component, Default)]
struct InputQueue {
    pending: HashMap<u32, MoveInput>,
    /// 这里最多只留这么多条：客户端 tick 跑飞时不至于把内存堆爆
    cap: usize,
}

/// 缓冲里过期多久的输入直接扔掉（一个来回的量级，丢包时那条永远等不到它的 tick）
const INPUT_QUEUE_STALE_TICKS: u32 = 8;

impl InputQueue {
    /// 存一条按 tick 排队的输入。
    fn insert(&mut self, input: MoveInput) {
        if self.pending.len() >= self.cap {
            // 兜底：真堆满了就全丢，宁可退回「到达即生效」一 tick，也不能无限涨
            self.pending.clear();
        }
        self.pending.insert(input.tick, input);
    }

    /// 这一 tick 该用的输入。
    ///
    /// 正常情况是精确命中（客户端提前 ~一个来回把输入送进未来的 tick）。
    /// 万一客户端 tick 整体落后了（掉帧、拨钟、刚连上），精确命中就永远等不到，
    /// 这时候退一步取「最近一条已经过期的」，保证输入始终在推进，
    /// 而不是把人冻在最后一条上一直往前跑。
    fn take_for(&mut self, tick: u32) -> Option<MoveInput> {
        if let Some(input) = self.pending.remove(&tick) {
            return Some(input);
        }
        let mut newest: Option<u32> = None;
        for candidate in self.pending.keys().copied() {
            let behind = tick.wrapping_sub(candidate);
            let is_past = behind != 0 && behind < 1 << 31 && behind <= INPUT_QUEUE_STALE_TICKS;
            if is_past && newest.is_none_or(|best| tick.wrapping_sub(best) > behind) {
                newest = Some(candidate);
            }
        }
        newest.and_then(|tick| self.pending.remove(&tick))
    }

    /// 清掉已经错过生效点的残留（客户端落后于服务端 / 丢包 / tick 对不上时会有）
    fn prune(&mut self, now: u32) {
        self.pending.retain(|tick, _| {
            let behind = now.wrapping_sub(*tick);
            // `behind` 很小说明是过去（该删）；很大说明还在未来（留着等它到点）
            !(behind != 0 && behind < 1 << 31 && behind > INPUT_QUEUE_STALE_TICKS)
        });
    }
}

/// TEMP 诊断：每秒钟把「本 tick 的输入到底命中没命中」汇总一行。
#[derive(Default)]
struct InputDiag {
    exact: u32,
    fallback: u32,
    none: u32,
    sent: u32,
    queue_len: usize,
    applied_tick: u32,
    next_print: f32,
}

/// 距离「上一次收到输入」还剩多久，收到输入时重置为 `INPUT_GRACE_SECS`。
#[derive(Component)]
struct InputGrace(f32);

/// 服务端跳跃阶段，用来给远端动画定锚点（编码进 `NetJump.phase`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum JumpPhase {
    /// 站在地上，没有跳跃
    #[default]
    Ground,
    /// 已接受起跳请求，贴地蓄力
    Charge,
    /// 已写离地速度，滞空
    Air,
    /// 已触地，落地恢复
    Land,
}

impl JumpPhase {
    fn code(self) -> u8 {
        match self {
            JumpPhase::Ground => crab_feast_library::net::jump_phase::GROUND,
            JumpPhase::Charge => crab_feast_library::net::jump_phase::CHARGE,
            JumpPhase::Air => crab_feast_library::net::jump_phase::AIR,
            JumpPhase::Land => crab_feast_library::net::jump_phase::LAND,
        }
    }
}

/// 落地段保持多少个 tick 再宣布回地面。
///
/// 远端落地恢复段约 0.2s（12 tick），这里多留 2 tick：远端拿到 `LAND` 后按
/// 自己的剪辑恢复段播完，服务器提前回 `Ground` 也不会把恢复段截断。
const NET_LAND_TICKS: u32 = 14;

/// 跳跃蓄力状态（服务端本地，不进复制协议）。
///
/// 收到起跳上升沿后先贴地蓄力一段，蓄满才给离地速度；`prev_jump` 记录
/// 上一 tick 的按键电平，用来检测上升沿——按住不放落地后不会连跳。
#[derive(Component, Default)]
struct JumpCharge {
    /// 是否正在蓄力
    charging: bool,
    /// 已蓄力秒数
    elapsed: f32,
    /// 上一 tick 的起跳按键电平（只用来打日志）
    prev_jump: bool,
    /// 上一次见到过的起跳请求序号：`seq` 变化就是新的一跳。
    ///
    /// 不用「电平上升沿」判：输入是状态快照、通道不可靠，一次回落可能根本
    /// 没被观察到（被合包压平或那一帧丢包），那样这一跳只会被客户端演。
    prev_seq: u8,
    /// 已经收到、但到达时服务端判定「没接地」的新请求，还能重试多久。
    ///
    /// 客户端只在**它自己**判定的接地帧才开始起跳，服务端晚一两拍认同就会有
    /// 一次「请求已到、人却没接地」，当场丢掉的话这一跳只有客户端在演。
    retry_secs: f32,
    /// 这一跳的大类（由客户端上行 `jump_kind` 带上来）：决定取哪一档调参。
    kind: JumpKind,
    /// 起跳写速后还剩多久验证（秒）。`>0` 表示「已经写了离地速度，等物理交结果」。
    ///
    /// 写进 `LinearVelocity` 不等于人真的离地：顶棚、贴过来的别的刚体、
    /// 贴墙的摩擦都会在接触求解里把竖直速度吃掉。根没抬起来就必须及时退出
    /// 跳跃状态，不能留着一个「已经起跳」的假状态。
    verify_secs: f32,
    /// 写离地速度那一 tick 的根高度，跟当前高度比出「到底抬没抬」。
    launch_y: f32,
    /// 对外发布的阶段（`NetJump.phase`）
    phase: JumpPhase,
    /// 当前阶段已经过的 tick
    phase_ticks: u32,
    /// 这一跳的序号（= 上行 `jump_seq`），跟随 `NetJump` 一起复制
    seq: u8,
}

impl JumpCharge {
    /// 开始一跳：序号、阶段、计时一起重置
    fn begin_phase(&mut self, phase: JumpPhase, seq: u8) {
        self.phase = phase;
        self.phase_ticks = 0;
        self.seq = seq;
    }

    /// 阶段互换但序号不变（蓄力取消 / 触地 / 落地播完）
    fn move_phase(&mut self, phase: JumpPhase) {
        self.phase = phase;
        self.phase_ticks = 0;
    }
}

/// 消费消息，只把最新一条存到 link 上。
///
/// 必须排在 `MessageSystems::ReceiveMessages` 之后，否则读到的永远是空队列。
fn collect_inputs(
    mut links: Query<(
        &mut MessageReceiver<MoveInput>,
        &mut LatestInput,
        &mut InputGrace,
        &mut InputQueue,
    )>,
) {
    for (mut receiver, mut latest, mut grace, mut queue) in links.iter_mut() {
        // 一帧可能收到多条：同一个 tick 只留最后一条（输入是「当前状态」不是事件流），
        // 不同 tick 全部入队，由 `drive_avatars` 在各自该生效的 tick 取用
        for input in receiver.receive() {
            grace.0 = INPUT_GRACE_SECS;
            if input.tick == 0 {
                // 还没和服务端对上 tick：退回原样「到达即生效」
                latest.0 = input;
                continue;
            }
            if input.jump && !latest.0.jump {
                info!(
                    "server: 收到跳跃请求 seq={} kind={} tick={}",
                    input.jump_seq, input.jump_kind, input.tick
                );
            }
            queue.insert(input);
        }
    }
}

/// 落地段的目标速度：满速 × 转身降速。
///
/// `dir` 是输入的世界方向，`yaw` 是服务端权威朝向（弧度，0 = +Z，与客户端
/// `yaw_of` 同一约定）。夹角只按最小弧算，往左甩 180° 和往右甩 180° 一样该停。
///
/// 本 tick 的**目标**降速比例（只算夹角，不含时间斜坡）。
///
/// 同一份算式在 `crab_feast_library::tuning::ground_brake_target`，客户端预测端
/// 每 tick 调一次，两端才能逐 tick 对齐。
fn ground_brake_target(dir: Vec2, yaw: f32, turn_start_deg: f32, turn_min_scale: f32) -> f32 {
    tuning::ground_brake_target([dir.x, dir.y], yaw, turn_start_deg, turn_min_scale)
}

/// 地面速度目标（方向取输入方向，大小 = 满速 × 斜坡），见
/// [`tuning::ground_velocity`]。
fn ground_velocity(dir: Vec2, brake: f32) -> Vec2 {
    let v = tuning::ground_velocity([dir.x, dir.y], brake);
    Vec2::new(v[0], v[1])
}

/// 每个逻辑帧把最新输入写成速度，并推进宽限期。
fn drive_avatars(
    time: Res<Time<Fixed>>,
    // 物理探针的最小 App 里没有这两个：没有 tick 计数/队列就退回「到达即生效」
    // （探针本来就把输入直接塞在 `LatestInput` 里）
    counter: Option<Res<NetTickCounter>>,
    tunings: Res<JumpTunings>,
    mut links: Query<(
        Entity,
        &mut LatestInput,
        &mut InputGrace,
        Option<&mut InputQueue>,
    )>,
    mut avatars: Query<(
        Entity,
        &mut LinearVelocity,
        &mut AvatarIntent,
        &mut JumpCharge,
        &Transform,
        // 转身降速要用「身体还差多少没转过来」；单测里没有朝向组件就按 1.0 放行
        Option<&NetYaw>,
        // 刹车力按本 tick 的转速给：人还没开始转的那几帧不刹
        Option<&FacingRate>,
        // 降速斜坡：刹不住惯性就会一帧掉到下限
        &mut TurnBrake,
    )>,
    room: Res<Room>,
    spatial: SpatialQuery,
    mut diag: Local<std::collections::HashMap<Entity, InputDiag>>,
) {
    let dt = time.delta_secs();
    // 本 tick 的输入该是哪一条：`NetTickCounter` 在 `FixedPostUpdate` 每步加一，
    // 所以在 `FixedUpdate` 里它是「已完成步数」，本步要完成的是它的下一个编号
    let desired = counter
        .as_ref()
        .map_or(0, |counter| counter.0.wrapping_add(1));
    for (link, mut latest, mut grace, queue) in links.iter_mut() {
        let stat = diag.entry(link).or_default();
        if let Some(mut queue) = queue {
            queue.prune(desired);
            let exact = queue.pending.contains_key(&desired);
            if let Some(input) = queue.take_for(desired) {
                if exact {
                    stat.exact += 1;
                } else {
                    stat.fallback += 1;
                }
                latest.0 = input;
            } else {
                stat.none += 1;
            }
            stat.queue_len = queue.pending.len();
        }
        stat.applied_tick = latest.0.tick;
        let Some(&avatar) = room.avatar_of_link.get(&link) else {
            continue;
        };
        let Ok((
            entity,
            mut velocity,
            mut intent,
            mut charge,
            transform,
            net_yaw,
            facing_rate,
            mut brake,
        )) = avatars.get_mut(avatar)
        else {
            continue;
        };

        if grace.0 <= 0.0 {
            // 断流：水平速度归零，重力照常。意向也归零，动画回 Idle。
            // 只清意向，别把 `measured` 一起清掉——实测速度要自然衰减，
            // 一拍归零会让停步那一帧的姿态回弹
            velocity.0.x = 0.0;
            velocity.0.z = 0.0;
            intent.dir = [0.0; 2];
            intent.speed = 0.0;
            charge.charging = false;
            charge.elapsed = 0.0;
            charge.prev_jump = false;
            charge.verify_secs = 0.0;
            charge.move_phase(JumpPhase::Ground);
            // 断流也把斜坡往回爬，别让下一条输入从「还在刹车」的格子里起步
            brake.0 = tuning::ramp_turn_brake(brake.0, 1.0, 0.0, dt);
            continue;
        }
        grace.0 -= dt;

        let dir = Vec2::new(latest.0.dir[0], latest.0.dir[1]);
        let direction = dir.normalize_or_zero();
        // 物理速度是「想走」的结果，会被墙/方块吃掉；动画与转向要的是意向，
        // 所以这里两个都记：velocity 交给物理，intent 交给表现层
        intent.dir = direction.to_array();
        intent.speed = if direction.length_squared() > 1e-6 {
            MOVE_SPEED_MPS * latest.0.force.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let yaw = net_yaw.map_or(0.0, |yaw| yaw.0);
        // 降速斜坡**每个 tick 都推**，空中也推：空中不改地速，但绝不能让两端的斜坡
        // 停在不同的格子上——落地那一 tick 速度是从坡上取的，差一格就是一步。
        let target = ground_brake_target(
            direction,
            yaw,
            latest.0.turn_brake_start_deg,
            latest.0.turn_brake_min_scale,
        );
        brake.0 =
            tuning::ramp_turn_brake(brake.0, target, facing_rate.map_or(0.0, |rate| rate.0), dt);

        if charge.phase != JumpPhase::Air {
            // 转身降速：夹角大时压到下限、**不归零**，甩头时人还往前走，只是慢下来。
            // 旋钮来自客户端上行（`turn_brake_start_deg` / `turn_brake_min_scale`），
            // 两端用同一条曲线 + 同一个斜坡才能逐 tick 对齐。
            let ground = if direction.length_squared() > 1e-6 {
                let target = ground_velocity(direction, brake.0);
                // 反向按有限加速度掰（和客户端同一个函数）：先刹到 0 再反向加速
                let stepped = tuning::ground_velocity_step(
                    [velocity.0.x, velocity.0.z],
                    target.to_array(),
                    tuning::REVERSE_ACCEL_MPS2,
                    dt,
                );
                Vec2::new(stepped[0], stepped[1])
            } else {
                // 松手：没有方向就没有位移（斜坡归位是另一回事，不能凭空推人）
                Vec2::ZERO
            };
            velocity.0.x = ground.x;
            velocity.0.z = ground.y;
        } else {
            // 滞空：**保动量、限转向**——不写目标速度，只把当前水平速度以有限加速度
            // 转向输入方向，速度大小原样保留。松手时上面 `intent.speed` 会归零，
            // 但这里根本不读它，所以「松手」不会把动量抽走。
            //
            // 必须和客户端 `movement_system` 逐 tick 一致（同一份
            // `air_steer_velocity`，同一套参数）：一端给空中控制、另一端不给，
            // 同一跳的落点就差一截，预测端只好被和解来回拽。
            //
            // 「起跳后多久」用 `phase_ticks * dt` 算，和服务端能拿到的东西一致；
            // 客户端也按「起跳后经过多久」判，两端窗口对齐。见 `air_steer_active`。
            let air_elapsed = charge.phase_ticks as f32 * dt;
            let accel = latest
                .0
                .air_accel
                .clamp(AIR_ACCEL_RANGE.0, AIR_ACCEL_RANGE.1);
            let steer_secs = latest
                .0
                .air_steer_secs
                .clamp(AIR_STEER_SECS_RANGE.0, AIR_STEER_SECS_RANGE.1);
            // 加速度按摇杆量级缩放，和客户端一致：`dir` 的长度就是摇杆偏转量
            let push = (dir.length() * latest.0.force.clamp(0.0, 1.0)).clamp(0.0, 1.0);
            if air_steer_active(air_elapsed, steer_secs) {
                let steered = air_steer_velocity(
                    [velocity.0.x, velocity.0.z],
                    [direction.x, direction.y],
                    accel * push,
                    dt,
                );
                velocity.0.x = steered[0];
                velocity.0.z = steered[1];
            }
        }

        // 跳跃：先贴地蓄力，蓄满才给离地速度（见 [`advance_charge`]）
        // 接地用脚下射线而不是绝对高度：站在 1m 方块上（胶囊中心 y≈1.7）也要能跳
        let grounded = ground::grounded(&spatial, entity, transform, velocity.0.y, GROUND_NOISE_VY);
        // 这一跳走哪一档由客户端在起跳帧带上来；蓄力期间每 tick 更新一次，
        // 客户端的消息比上升沿晚一帧也不会用错档。
        charge.kind = JumpKind::from_code(latest.0.jump_kind);

        // 上一 tick 刚写过离地速度：拿这一 tick 的物理结果验证到底起没起跳。
        // 写进 `LinearVelocity` 只是「请求」，物理会照样把它压回去。
        if charge.verify_secs > 0.0 {
            charge.verify_secs -= dt;
            if transform.translation.y - charge.launch_y > LAUNCH_MIN_RISE {
                // 根真的抬起来了：起跳成立，结束验证
                charge.verify_secs = 0.0;
            } else if charge.verify_secs <= 0.0 {
                // 根一直没抬起来：顶棚 / 贴身的别的刚体 / 摩擦把起跳吃掉了。
                // 及时退出跳跃状态，别留一个「已经在飞」的假状态。
                charge.verify_secs = 0.0;
                charge.charging = false;
                charge.elapsed = 0.0;
                charge.retry_secs = 0.0;
                charge.move_phase(JumpPhase::Ground);
                info!(
                    "server: 起跳失败（根未抬升），退出跳跃状态 y={:.3} vy={:.3} grounded={grounded}",
                    transform.translation.y, velocity.0.y
                );
            }
        }
        let was_charging = charge.charging;
        let launched = advance_charge(
            &mut charge,
            latest.0.jump,
            latest.0.jump_seq,
            grounded,
            velocity.0.y,
            intent.measured,
            &tunings,
            dt,
        );
        // 新的一跳：蓄力一开始就发布，远端才能演蹲姿，而不是等离地了再补动作
        if !was_charging && charge.charging {
            charge.begin_phase(JumpPhase::Charge, latest.0.jump_seq);
        }
        if let Some(default_vy) = launched {
            // 起跳速度由客户端设置面板决定，随快照上行：两端同一个数，跳起来才一样高。
            // 0 / 越界值（老协议、坏包）退回档位表的默认值，绝不按 0 起跳。
            let vy = if latest.0.jump_vy > 0.0 {
                latest
                    .0
                    .jump_vy
                    .clamp(JUMP_LAUNCH_VY_RANGE.0, JUMP_LAUNCH_VY_RANGE.1)
            } else {
                default_vy
            };
            velocity.0.y = vy;
            charge.begin_phase(JumpPhase::Air, latest.0.jump_seq);
            // 写速只是请求：从下一 tick 起拿根高度验证物理有没有真的把人抬起来
            charge.verify_secs = LAUNCH_VERIFY_SECS;
            charge.launch_y = transform.translation.y;
            // 现场排查用：`CRAB_LOG_SRV=1` 看服务端到底给了多少离地速度、从哪个高度给的。
            if std::env::var("CRAB_LOG_SRV").is_ok() {
                info!(
                    "srv launch vy={vy:.2} y={:.3} xz={:.2},{:.2} grounded={grounded} measured={:.2} kind={:?}",
                    transform.translation.y,
                    transform.translation.x,
                    transform.translation.z,
                    intent.measured,
                    charge.kind
                );
            }
        }
        // 逐帧：起跳后把竖直速度与高度打出来，用来区分「初速度不够」和「人被卡住」
        if std::env::var("CRAB_LOG_SRV").is_ok() && (charge.charging || velocity.0.y.abs() > 0.02) {
            info!(
                "srv av y={:.3} vy={:.3} xz={:.2},{:.2} grounded={grounded} charging={} el={:.3}",
                transform.translation.y,
                velocity.0.y,
                transform.translation.x,
                transform.translation.z,
                charge.charging,
                charge.elapsed
            );
        }

        // 对外阶段推进：Charge 被取消 → Ground；Air 触地 → Land；Land 播完 → Ground。
        // 远端动画按阶段 + `phase_ticks` 直接排剪辑，不再靠位置差观察。
        match charge.phase {
            JumpPhase::Charge => {
                if !charge.charging {
                    charge.move_phase(JumpPhase::Ground);
                }
            }
            JumpPhase::Air => {
                // 起跳后前几 tick 射线还贴着地，别一离地就判落地
                if charge.phase_ticks >= 4 && grounded && velocity.0.y <= GROUND_NOISE_VY {
                    charge.move_phase(JumpPhase::Land);
                }
            }
            JumpPhase::Land => {
                if charge.phase_ticks >= NET_LAND_TICKS {
                    charge.move_phase(JumpPhase::Ground);
                }
            }
            JumpPhase::Ground => {}
        }
        if charge.phase != JumpPhase::Ground {
            charge.phase_ticks = charge.phase_ticks.saturating_add(1);
        }

        // 现场排查：`CRAB_LOG_INPUT=1` 时每秒一行，看服务端到底吃到哪一条输入
        if std::env::var("CRAB_LOG_INPUT").is_ok() && time.elapsed_secs() >= stat.next_print {
            stat.next_print = time.elapsed_secs() + 1.0;
            info!(
                "srv input: desired={desired} applied_t={} exact={} fallback={} none={} qlen={} \
                 dir=({:.2},{:.2}) force={:.2} pos=({:.2},{:.2},{:.2}) y={:.2} phase={:?}",
                stat.applied_tick,
                stat.exact,
                stat.fallback,
                stat.none,
                stat.queue_len,
                latest.0.dir[0],
                latest.0.dir[1],
                latest.0.force,
                transform.translation.x,
                transform.translation.y,
                transform.translation.z,
                transform.translation.y,
                charge.phase
            );
            stat.exact = 0;
            stat.fallback = 0;
            stat.none = 0;
            stat.sent = 0;
        }
    }
}

/// 推进跳跃蓄力状态。返回本 tick 要写进 `LinearVelocity.y` 的离地速度（`None` = 还没起跳）。
///
/// - 起跳必须是按键**上升沿**：按住不放落地后不会二次起跳，对齐单机的边沿锁存；
/// - 蓄力期间被推离地面（走出平台边缘）取消蓄力；
/// - 蓄力时长与离地速度都按**这一跳的档位**取（站 / 跑 / 上 / 下），
///   与客户端起跳剪辑用的是同一份表，两端离地帧和高度才对得上。
fn advance_charge(
    charge: &mut JumpCharge,
    jump: bool,
    jump_seq: u8,
    grounded: bool,
    upward_speed: f32,
    ground_speed: f32,
    tunings: &JumpTunings,
    dt: f32,
) -> Option<f32> {
    // 已经离地（明显向上速度）就清掉残留计时，别把上一跳的蓄力带进落地。
    // 低于 `GROUND_NOISE_VY` 的接触补偿噪声不算离地，别误清蓄力。
    if upward_speed > GROUND_NOISE_VY {
        charge.charging = false;
        charge.elapsed = 0.0;
    }
    // 新的一跳 = 序号变了（不依赖电平回落被观察到）；电平仍要压住，
    // 客户端在等窗口/落地期间会把电平让出来，那段时间不该被认成请求。
    let pressed = jump && jump_seq != charge.prev_seq;
    if pressed {
        charge.prev_seq = jump_seq;
        // 新请求覆盖旧的验证：上一跳的「抬没抬」跟这一跳无关
        charge.verify_secs = 0.0;
        // 到达时没接地：留着 0.1s 重试窗口，别把请求当场判死
        charge.retry_secs = if grounded { 0.0 } else { REQUEST_RETRY_SECS };
    }
    charge.prev_jump = jump;

    if !charge.charging {
        let want = pressed || charge.retry_secs > 0.0;
        if want && grounded && upward_speed <= GROUND_NOISE_VY {
            charge.charging = true;
            charge.elapsed = 0.0;
            charge.retry_secs = 0.0;
        } else if charge.retry_secs > 0.0 {
            charge.retry_secs = (charge.retry_secs - dt).max(0.0);
        }
        return None;
    }

    // 蓄力中被推离地面（走出平台边缘）→ 取消
    if !grounded {
        charge.charging = false;
        charge.elapsed = 0.0;
        return None;
    }
    charge.elapsed += dt;
    let profile = JumpProfile::for_jump(charge.kind, ground_speed);
    let tuning = tunings.get(profile);
    if charge.elapsed >= tuning.charge_secs {
        charge.charging = false;
        charge.elapsed = 0.0;
        Some(tuning.net_launch_vy)
    } else {
        None
    }
}

/// 这一 tick 玩家**想怎么走**（来自上行输入，不是物理结果）。
///
/// 为什么不能拿 `LinearVelocity` 顶替：撞墙/撞方块时物理解算会把水平速度清掉，
/// 表现出来就是两个毛病——墙前转身突然停住（看着像「移动方向不对」），
/// 以及动画在 Run / Idle 之间以半个多秒的节奏反复切（看着像「动画状态不对」）。
/// 单机那边 `LocomotionIntent` 本来就取自输入而不是速度，服务器这里对齐同一套口径。
#[derive(Component, Default)]
struct AvatarIntent {
    /// 世界空间 XZ 单位方向（`speed > 0` 时有效）
    dir: [f32; 2],
    /// 目标地速（米/秒）。撞墙时照样非零：人是被挡住的，不是不想走。
    /// 只给转向用：转的是「想走哪边」，不是「实际跑到哪」
    speed: f32,
    /// **实测**水平速度（米/秒，按位置差做一阶低通）。发布给客户端的动画只用它：
    /// 意向是「打算跑多快」，实测是「真跑了多快」，两者顶墙时能差 3 米/秒，
    /// 拿意向驱动动画就是人在墙前人抖腿
    measured: f32,
    /// 上一 tick 的世界位置，用来算位置差
    last: [f32; 3],
    /// `last` 是否已经写过（第一 tick 没有上一位置）
    primed: bool,
}

/// 实测水平速度的一阶低通时间常数（秒）。取 0.1 左右：比一个跑动周期短，
/// 又足够长到不会被「顶墙那一 tick 物理解算清零」的抖动带节奏
const MEASURE_TAU: f32 = 0.1;

/// 从位置差量出实际水平速度。
///
/// 为什么不直接读 `LinearVelocity`：那一 tick 写进去的是**指令速度**（3 米/秒），
/// 物理解算之后才变成真实结果，而解算结果在墙前会是 0/3 之间跳——低通一过，
/// 3 和 0 各占一半就变成 1.5，动画就稳定卡在「走」档原地小跑。用位置差算，
/// 顶墙就是稳定的 0，滑墙就是稳定的滑动速度，都是真的。
fn measure_speed(time: Res<Time<Fixed>>, mut avatars: Query<(&Transform, &mut AvatarIntent)>) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let k = (dt / MEASURE_TAU).clamp(0.0, 1.0);
    for (transform, mut intent) in avatars.iter_mut() {
        let now = transform.translation;
        if intent.primed {
            let delta = Vec2::new(now.x - intent.last[0], now.z - intent.last[2]);
            let raw = delta.length() / dt;
            intent.measured += (raw - intent.measured) * k;
        }
        intent.last = now.to_array();
        intent.primed = true;
    }
}

/// 角色朝向：追着移动方向转过去，角速度受限（与客户端 `facing_system` 同一个算式）。
///
/// 以前这里是 `yaw = 速度方向` 的瞬时赋值，联网后玩家看到的就是「一推杆人瞬间转身」，
/// 单机那套「身体有重量」的节奏整个丢了。朝向只进复制、不参与物理（`LockedAxes`
/// 锁了绕 Y 的旋转），所以放服务器做既权威又一致：所有客户端看到的转身节奏相同。
///
/// 客户端那套还多一层「腾空期折速」（`AIR_TURN`），门控输入是动画剪辑的触地权重，
/// 服务器没有动画层，这里不给近似——宁可全程按 90°/s 转，也不拿错的相位去折速。
///
/// 追的是**输入意向**而不是实测速度：顶在方块上时速度会被物理解算清成 0，
/// 拿速度当方向的话人就不转了（玩家看到的是「推着摇杆，人却保持原朝向」）。
fn face_movement(
    time: Res<Time<Fixed>>,
    mut avatars: Query<(&AvatarIntent, &mut NetYaw, &mut FacingRate)>,
) {
    let dt = time.delta_secs();
    for (intent, mut yaw, mut rate) in avatars.iter_mut() {
        let planar = Vec2::new(intent.dir[0], intent.dir[1]);
        if intent.speed <= 0.05 || planar.length_squared() <= 1e-6 {
            // 站住不动：保持当前朝向，角速度归零（下次起步重新加速）
            rate.0 = 0.0;
            continue;
        }
        let goal = planar.x.atan2(planar.y);
        let error = tuning::wrap_pi(goal - yaw.0);
        let (next_yaw, next_rate) = tuning::step_yaw(yaw.0, rate.0, error, dt);
        yaw.0 = next_yaw;
        rate.0 = next_rate;
    }
}

/// 转身降速的斜坡状态（比例 0~1）。
///
/// 夹角是瞬时量，直接乘速度会一帧从满速掉到下限；这里按时间滑下去、再滑回来，
/// 甩头才像「惯性刹住」而不是被拉住。客户端用同名资源（`input::TurnBrakeRamp`），
/// 两端每 tick 调同一个 [`tuning::ramp_turn_brake`]，帧率不同也走同一条时间曲线。
///
/// 默认 1.0：出生就是满速档，不用先爬一段。
#[derive(Component)]
struct TurnBrake(f32);

impl Default for TurnBrake {
    fn default() -> Self {
        Self(1.0)
    }
}

/// 朝向限速的跨帧状态（度/秒）。
///
/// 没有它，「起步第一帧就满速」会把角加速度那一段吃掉——单机的手感全靠它。
#[derive(Component, Default)]
struct FacingRate(f32);

/// 服务端本地表现 / 蓄力状态打包成一个 Bundle。
///
/// 这些组件都不进复制协议；打包是为了把 spawn 元组压到 Bevy 元组 Bundle
/// 上限（15 个元素）以内——直接平铺会多出一个元素。
#[derive(Bundle, Default)]
struct AvatarLocal {
    facing: FacingRate,
    intent: AvatarIntent,
    jump_charge: JumpCharge,
    /// 复制给远端的跳跃阶段（打包进来是为了不超 Bevy 的 15 元素 Bundle 上限）
    net_jump: NetJump,
    /// 转身降速的斜坡（比例 0~1）：和客户端同一个 `ramp_turn_brake` 逐 tick 推
    turn_brake: TurnBrake,
}

/// 把物理结果写成协议组件，交给复制层。
/// 把服务端状态写成协议组件，交给复制层。
///
/// `NetPos` 取物理结果（真实位置），`NetMotion` 把三件事分开发布：
/// 方向 = 意向、速度 = 实测、主动度 = 有没有自己的输入。
///
/// 动画层要的是「玩家想怎么走」，不是「这一 tick 被墙推回了多少」：拿速度当
/// 动画输入的话，顶墙时速度会在 3 → 0 → 3 之间跳，远端动画就在 Run / Idle 反复切。
/// 反过来，被人推着走的时候意向是 0 而位置一直在动，位置差算出来的速度不为 0，
/// 只看速度就会让人「被搬走的同时原地跑步」，所以要单独给一个主动度。
fn publish_state(
    mut tick: ResMut<NetTickCounter>,
    mut avatars: Query<(
        &Transform,
        &mut NetPos,
        &mut NetMotion,
        &mut NetTick,
        &mut NetJump,
        &JumpCharge,
        &AvatarIntent,
    )>,
) {
    // 一个 tick 只自增一次：这个系统排在 `FixedPostUpdate`，每个定步长跑一遍
    tick.0 = tick.0.wrapping_add(1);
    for (transform, mut pos, mut motion, mut net_tick, mut net_jump, charge, intent) in
        avatars.iter_mut()
    {
        pos.0 = transform.translation.to_array();
        net_tick.0 = tick.0;
        // 跳跃阶段与序号一起发布：远端按 phase + ticks 排剪辑，不再从位置差里猜
        net_jump.seq = charge.seq;
        net_jump.phase = charge.phase.code();
        net_jump.ticks = charge.phase_ticks.min(u16::MAX as u32) as u16;
        net_jump.kind = charge.kind.code();
        motion.dir = intent.dir;
        motion.speed = intent.measured;
        // `intent.speed` 是目标地速（= 顶速 × 输入力度），无输入时就是 0
        motion.intent = if intent.speed > 0.0 {
            (intent.speed / MOVE_SPEED_MPS).clamp(0.0, 1.0)
        } else {
            0.0
        };
    }
}

#[cfg(test)]
mod jump_charge_tests {
    use super::*;

    const DT: f32 = 1.0 / TICK_HZ as f32;

    fn tunings() -> JumpTunings {
        JumpTunings::default()
    }

    /// 服务端写入的离地速度（站立档，也是物理探针用的那一档）
    fn stand_launch() -> f32 {
        tunings().get(JumpProfile::Stand).net_launch_vy
    }

    fn charge() -> JumpCharge {
        JumpCharge::default()
    }

    #[test]
    fn rising_edge_begins_but_does_not_launch_immediately() {
        let mut c = charge();
        // 上升沿当帧进入蓄力，不给离地速度
        assert!(advance_charge(&mut c, true, 1, true, 0.0, 0.0, &tunings(), DT).is_none());
        assert!(c.charging);
        // 只攒了一帧，远没到站立蓄力时长
        assert!(advance_charge(&mut c, true, 1, true, 0.0, 0.0, &tunings(), DT).is_none());
    }

    #[test]
    fn stand_charge_full_launches_once() {
        let mut c = charge();
        let mut launched = false;
        let mut ticks = 0;
        while !launched && ticks < 120 {
            launched = advance_charge(&mut c, true, 1, true, 0.0, 0.0, &tunings(), DT).is_some();
            ticks += 1;
        }
        assert!(launched, "站立档蓄满应给一次离地速度");
        assert!(!c.charging);
        // 已经离地，按住也不会二次起跳
        assert!(
            advance_charge(&mut c, true, 1, false, stand_launch(), 0.0, &tunings(), DT).is_none()
        );
    }

    #[test]
    fn held_jump_does_not_double_jump_after_landing() {
        let mut c = charge();
        // 先蓄满跳起来
        for _ in 0..60 {
            advance_charge(&mut c, true, 1, true, 0.0, 0.0, &tunings(), DT);
        }
        // 空中：向上速度清掉残留蓄力
        advance_charge(&mut c, true, 1, false, stand_launch(), 0.0, &tunings(), DT);
        // 落地但按键仍按住：上升沿已过，不重新蓄力、不连跳
        assert!(advance_charge(&mut c, true, 1, true, 0.0, 0.0, &tunings(), DT).is_none());
        assert!(!c.charging);
    }

    #[test]
    fn charge_cancelled_when_walking_off_edge() {
        let mut c = charge();
        advance_charge(&mut c, true, 1, true, 0.0, 0.0, &tunings(), DT);
        assert!(c.charging);
        // 走出平台边缘 → 取消蓄力，自由下落
        advance_charge(&mut c, true, 1, false, 0.0, 0.0, &tunings(), DT);
        assert!(!c.charging);
    }

    #[test]
    fn run_charge_is_shorter_than_stand() {
        let mut c = charge();
        let mut launched = false;
        let mut ticks = 0;
        while !launched && ticks < 120 {
            launched = advance_charge(&mut c, true, 1, true, 0.0, RUN_JUMP_SPEED, &tunings(), DT)
                .is_some();
            ticks += 1;
        }
        assert!(launched);
        // 奔跑档 0.05s ≈ 3 帧；站立档 0.45s ≈ 27 帧
        assert!(ticks <= 4, "奔跑蓄力应在几帧内完成，实际 {ticks} 帧");
    }

    #[test]
    fn air_jump_is_ignored() {
        let mut c = charge();
        assert!(advance_charge(&mut c, true, 1, false, 1.0, 0.0, &tunings(), DT).is_none());
        assert!(!c.charging);
    }

    #[test]
    fn micro_upward_contact_velocity_does_not_clear_charge() {
        let mut c = charge();
        // 贴地静止时 avian 接触补偿留下的微小正速度（实测 ~5e-10 m/s），不是真离地。
        // 早期用严格 `> 0` 判定离地，会被这个噪声每帧清掉蓄力，原地跳永远蓄不满、
        // 根位置不抬升。这里锁定：低于容差的接触噪声不得打断蓄力。
        let noise: f32 = 5.3e-10;
        let mut launched = false;
        let mut ticks = 0;
        while !launched && ticks < 120 {
            launched = advance_charge(&mut c, true, 1, true, noise, 0.0, &tunings(), DT).is_some();
            ticks += 1;
        }
        assert!(launched, "接触补偿的微小正速度不应打断蓄力");
    }

    #[test]
    fn solver_contact_noise_does_not_clear_charge() {
        // 实测贴地时接触求解会留下 0.192 m/s 量级的残余正速度（客户端日志反复出现）。
        // 旧的 0.1 界线把它当成「已经在蹬升」，蓄力每 tick 被清掉 → 按了跳没反应。
        let mut c = charge();
        let mut launched = false;
        let mut ticks = 0;
        while !launched && ticks < 120 {
            launched =
                advance_charge(&mut c, true, 1, true, 0.192_409_41, 0.0, &tunings(), DT).is_some();
            ticks += 1;
        }
        assert!(launched, "0.19 m/s 的贴地噪声不应打断蓄力");
    }
}

/// 物理基准探针：复刻服务端角色的刚体/碰撞体/阻尼参数，给定站立档离地速度
/// 初速度后跑物理积分，断言能明显抬升。用来回归排除「avian 速度→位移积分
/// 链路本身不升空」这类物理层问题；联网跳不起来时先看这里是否通过。
#[cfg(test)]
mod physics_probe {
    use super::*;
    use bevy::time::TimeUpdateStrategy;

    #[test]
    fn capsule_peak_height() {
        let mut app = App::new();
        app.add_plugins(AssetPlugin::default()).init_asset::<Mesh>();
        app.add_message::<AssetEvent<Mesh>>();
        app.add_plugins((MinimalPlugins, TransformPlugin, PhysicsPlugins::default()))
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
                1.0 / 60.0,
            )));
        app.finish();

        app.world_mut().spawn((
            RigidBody::Static,
            Collider::cuboid(20.0, 1.0, 20.0),
            Transform::from_xyz(0.0, -0.5, 0.0),
        ));
        let body = app
            .world_mut()
            .spawn((
                RigidBody::Dynamic,
                Collider::capsule(0.2, 1.0),
                LockedAxes::ROTATION_LOCKED,
                LinearDamping(0.3),
                AngularDamping(0.3),
                LinearVelocity::ZERO,
                Transform::from_xyz(0.0, 1.0, 0.0),
            ))
            .id();
        app.update();

        app.world_mut()
            .entity_mut(body)
            .insert(LinearVelocity::from(Vec3::new(
                0.0,
                JumpTunings::default().get(JumpProfile::Stand).net_launch_vy,
                0.0,
            )));

        let mut peak = 1.0f32;
        for _ in 0..180 {
            app.update();
            let tr = app.world().entity(body).get::<Transform>().unwrap();
            peak = peak.max(tr.translation.y);
        }
        let end: f32 = app
            .world()
            .entity(body)
            .get::<Transform>()
            .unwrap()
            .translation
            .y;
        println!("PROBE peak_y={peak} end_y={end}");
        assert!(peak > 1.5, "峰值应明显高于初始 1.0，实际 {peak}");
    }

    /// 朝前的正常跑动不能被转身降速误伤：yaw 与输入同向时必须就是满速
    #[test]
    fn ground_brake_uses_the_authoritative_yaw() {
        /// 返回 `(第一次采样时刻的位移, 总位移, 最终 yaw 弧度)`：只测总位移看不出
        /// 「先刹住再起步」的曲线，必须把转身段单独量出来
        fn run_case(dir: [f32; 2], yaw: f32, ticks: u32) -> (f32, f32, f32) {
            let mut app = App::new();
            app.add_plugins(AssetPlugin::default()).init_asset::<Mesh>();
            app.add_message::<AssetEvent<Mesh>>();
            app.add_plugins((MinimalPlugins, TransformPlugin, PhysicsPlugins::default()))
                .init_resource::<JumpTunings>()
                .add_systems(FixedUpdate, drive_avatars)
                .add_systems(FixedPostUpdate, face_movement)
                .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
                    1.0 / 60.0,
                )));
            app.finish();
            app.world_mut().spawn((
                RigidBody::Static,
                Collider::cuboid(40.0, 1.0, 40.0),
                Transform::from_xyz(0.0, -0.5, 0.0),
            ));
            let link = app
                .world_mut()
                .spawn((
                    LatestInput(MoveInput {
                        dir,
                        force: 1.0,
                        ..MoveInput::default()
                    }),
                    InputGrace(INPUT_GRACE_SECS),
                ))
                .id();
            let avatar = app
                .world_mut()
                .spawn((
                    RigidBody::Dynamic,
                    Collider::capsule(0.2, 1.0),
                    LockedAxes::ROTATION_LOCKED,
                    LinearDamping(0.3),
                    AngularDamping(0.3),
                    LinearVelocity::ZERO,
                    JumpCharge::default(),
                    AvatarIntent::default(),
                    TurnBrake::default(),
                    FacingRate::default(),
                    NetYaw(yaw),
                    Transform::from_xyz(0.0, 1.0, 0.0),
                ))
                .id();
            app.world_mut().insert_resource(Room {
                avatar_of_link: HashMap::from([(link, avatar)]),
            });
            for _ in 0..20 {
                app.update();
            }
            let start = app
                .world()
                .entity(avatar)
                .get::<Transform>()
                .unwrap()
                .translation;
            let sample_at = (ticks / 3).max(1);
            // 甩头后位移方向会和输入方向正交甚至反向（U 形弧），净位移会被抵消，
            // 所以量**路程**：每 tick 的位移长度累加，才看得出「一直在走，只是变慢」
            let mut early_path = 0.0f32;
            let mut path = 0.0f32;
            let mut last = start;
            for tick in 0..ticks {
                if tick == sample_at {
                    early_path = path;
                }
                if let Some(mut gr) = app.world_mut().get_mut::<InputGrace>(link) {
                    gr.0 = INPUT_GRACE_SECS;
                }
                app.update();
                let now = app
                    .world()
                    .entity(avatar)
                    .get::<Transform>()
                    .unwrap()
                    .translation;
                let step = Vec2::new(now.x - last.x, now.z - last.z).length();
                path += step;
                last = now;
            }
            let end = last;
            let yaw_now = app
                .world()
                .entity(avatar)
                .get::<NetYaw>()
                .unwrap()
                .0
                .to_degrees();
            let dist = end - start;
            println!(
                "BRAKE dir={dir:?} yaw0={:.1}° → yaw={yaw_now:.1}° net=({:.2},{:.2}) path={:.2}m early={:.2}m",
                yaw.to_degrees(),
                dist.x,
                dist.z,
                path,
                early_path
            );
            (early_path, path, yaw_now)
        }

        // 正前方（+Z）：yaw 与输入同向，必须满速
        let (_, forward_path, _) = run_case([0.0, 1.0], 0.0, 120);
        let forward_speed = forward_path / 2.0;
        assert!(
            // 起步有 0.15s 的加速曲线（20 m/s²），两秒平均会少走约 0.22m
            (forward_speed - MOVE_SPEED_MPS).abs() < 0.3,
            "正前方没跑满速：{forward_speed:.2} m/s"
        );
        // 正后方：先按惯性刹住、再低速折线转过去。
        // 第一秒只该滑出一小段（约一个刹车距离），既不是满速冲出去、也不是站住不动
        let (early_path, back_path, back_yaw) = run_case([0.0, -1.0], 0.0, 180);
        assert!(back_yaw.abs() > 170.0, "转身没转过来：{back_yaw:.1}°");
        // 顶速是 3m/s：第一秒滑到 2 米以上就等于根本没减速。
        // 下界留 0.5 是不许「原地站住」——惯性要留着（用户明确要这个体感）
        assert!(
            early_path < MOVE_SPEED_MPS * 0.7,
            "180° 甩头第一秒滑了 {early_path:.2} 米，还在按满速冲"
        );
        assert!(
            early_path > 0.5,
            "180° 甩头第一秒只走了 {early_path:.2} 米，刹得太死"
        );
        // 3 秒的路程要明显少于满速的 9 米（那段是低速折线），但也不能停住
        // 3 秒满速是 9 米；转完 1.1 秒之后会重新提速，所以上限放到 3.2×顶速
        assert!(
            back_path > MOVE_SPEED_MPS * 1.5 && back_path < MOVE_SPEED_MPS * 3.2,
            "折线转弯的路程不对：{back_path:.2} 米"
        );
    }

    /// 走真实 `FixedUpdate` 调度驱动 `drive_avatars`：先让胶囊在地面平衡贴地，
    /// 再喂一条按住的 jump 输入，蓄力满后由系统写出离地速度，读根位置峰值。
    /// 验证「状态机触发 → velocity.y 写出 → avian 积分抬升根」整条链路通畅；
    /// 回归早期「贴地接触噪声被误判离地、蓄力反复清零、根不抬升」的问题。
    #[test]
    fn drive_avatar_jump_lifts_root() {
        let mut app = App::new();
        app.add_plugins(AssetPlugin::default()).init_asset::<Mesh>();
        app.add_message::<AssetEvent<Mesh>>();
        app.add_plugins((MinimalPlugins, TransformPlugin, PhysicsPlugins::default()))
            .init_resource::<JumpTunings>()
            .add_systems(FixedUpdate, drive_avatars)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
                1.0 / 60.0,
            )));
        app.finish();

        app.world_mut().spawn((
            RigidBody::Static,
            Collider::cuboid(20.0, 1.0, 20.0),
            Transform::from_xyz(0.0, -0.5, 0.0),
        ));

        let link = app
            .world_mut()
            .spawn((
                LatestInput(MoveInput::default()),
                InputGrace(INPUT_GRACE_SECS),
            ))
            .id();
        let avatar = app
            .world_mut()
            .spawn((
                RigidBody::Dynamic,
                Collider::capsule(0.2, 1.0),
                LockedAxes::ROTATION_LOCKED,
                LinearDamping(0.3),
                AngularDamping(0.3),
                LinearVelocity::ZERO,
                JumpCharge::default(),
                AvatarIntent::default(),
                TurnBrake::default(),
                Transform::from_xyz(0.0, 1.0, 0.0),
            ))
            .id();

        app.world_mut().insert_resource(Room {
            avatar_of_link: HashMap::from([(link, avatar)]),
        });

        let room = app.world().resource::<Room>();
        assert_eq!(room.avatar_of_link.len(), 1);
        let av = app.world().entity(avatar);
        assert!(av.get::<LinearVelocity>().is_some());
        assert!(av.get::<AvatarIntent>().is_some());
        assert!(av.get::<JumpCharge>().is_some());

        // 先空跑若干帧让胶囊在地面平衡贴地，复现真实「站定后再起跳」的状态，
        // 避免出生下坠撞地的反弹速度误清蓄力。
        for _ in 0..30 {
            app.update();
        }
        if let Some(mut li) = app.world_mut().get_mut::<LatestInput>(link) {
            li.0.jump = true;
            // 服务端按序号认「新的一跳」：只抬电平不抬序号不算请求
            li.0.jump_seq = 1;
        }

        let mut peak = 1.0f32;
        for _ in 0..200 {
            // 模拟客户端每帧都在发包：持续刷新宽限期，避免断流分支掐断蓄力
            if let Some(mut gr) = app.world_mut().get_mut::<InputGrace>(link) {
                gr.0 = INPUT_GRACE_SECS;
            }
            app.update();
            let tr = app.world().entity(avatar).get::<Transform>().unwrap();
            peak = peak.max(tr.translation.y);
        }
        let end: f32 = app
            .world()
            .entity(avatar)
            .get::<Transform>()
            .unwrap()
            .translation
            .y;
        let vel = app.world().entity(avatar).get::<LinearVelocity>().unwrap();
        let ch = app.world().entity(avatar).get::<JumpCharge>().unwrap();
        println!(
            "DRIVE peak={peak} end={end} vy={:.3} charging={} elapsed={:.3} prev_jump={}",
            vel.0.y, ch.charging, ch.elapsed, ch.prev_jump
        );
        assert!(peak > 1.5, "驱动链路应把根明显抬升，峰值 {peak}");
    }

    /// 站在 1m 方块顶上（胶囊中心 y≈1.7）也必须能起跳。
    ///
    /// 旧实现用绝对高度 `y <= 1.15` 判接地，一上方块就永远「不在地上」，
    /// 蓄力起跳直接取消——站在方块上怎么按都不跳。改成脚下射线后与台面高度无关。
    #[test]
    fn drive_avatar_jump_lifts_root_on_raised_ground() {
        let mut app = App::new();
        app.add_plugins(AssetPlugin::default()).init_asset::<Mesh>();
        app.add_message::<AssetEvent<Mesh>>();
        app.add_plugins((MinimalPlugins, TransformPlugin, PhysicsPlugins::default()))
            .init_resource::<JumpTunings>()
            .add_systems(FixedUpdate, drive_avatars)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
                1.0 / 60.0,
            )));
        app.finish();

        // 1m 高的台子，顶面 y = 1.0
        app.world_mut().spawn((
            RigidBody::Static,
            Collider::cuboid(4.0, 1.0, 4.0),
            Transform::from_xyz(0.0, 0.5, 0.0),
        ));

        let link = app
            .world_mut()
            .spawn((
                LatestInput(MoveInput::default()),
                InputGrace(INPUT_GRACE_SECS),
            ))
            .id();
        let avatar = app
            .world_mut()
            .spawn((
                RigidBody::Dynamic,
                Collider::capsule(0.2, 1.0),
                LockedAxes::ROTATION_LOCKED,
                LinearDamping(0.3),
                AngularDamping(0.3),
                LinearVelocity::ZERO,
                JumpCharge::default(),
                AvatarIntent::default(),
                TurnBrake::default(),
                Transform::from_xyz(0.0, 2.0, 0.0),
            ))
            .id();

        app.world_mut().insert_resource(Room {
            avatar_of_link: HashMap::from([(link, avatar)]),
        });

        for _ in 0..40 {
            app.update();
        }
        let rest_y = app
            .world()
            .entity(avatar)
            .get::<Transform>()
            .unwrap()
            .translation
            .y;
        assert!(
            (rest_y - 1.7).abs() < 0.1,
            "应落在方块顶上（y≈1.7），实际 {rest_y}"
        );

        if let Some(mut li) = app.world_mut().get_mut::<LatestInput>(link) {
            li.0.jump = true;
            // 服务端按序号认「新的一跳」：只抬电平不抬序号不算请求
            li.0.jump_seq = 1;
        }
        let mut peak = rest_y;
        for _ in 0..200 {
            if let Some(mut gr) = app.world_mut().get_mut::<InputGrace>(link) {
                gr.0 = INPUT_GRACE_SECS;
            }
            app.update();
            let tr = app.world().entity(avatar).get::<Transform>().unwrap();
            peak = peak.max(tr.translation.y);
        }
        assert!(
            peak > rest_y + 0.4,
            "方块顶上应能起跳，静止 {rest_y} 峰值 {peak}"
        );
    }

    /// 头顶贴着刚体（顶棚只留 2cm 净空）时，写入的离地速度会被接触求解当场吃掉。
    /// 根抬不起来就必须及时退出跳跃状态：不能留着 `charging` / 验证计时在那儿，
    /// 更不能让客户端一直等一个不会发生的离地。
    #[test]
    fn blocked_jump_exits_jump_state() {
        let mut app = App::new();
        app.add_plugins(AssetPlugin::default()).init_asset::<Mesh>();
        app.add_message::<AssetEvent<Mesh>>();
        app.add_plugins((MinimalPlugins, TransformPlugin, PhysicsPlugins::default()))
            .init_resource::<JumpTunings>()
            .add_systems(FixedUpdate, drive_avatars)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
                1.0 / 60.0,
            )));
        app.finish();

        // 地面：顶面 y = 0
        app.world_mut().spawn((
            RigidBody::Static,
            Collider::cuboid(20.0, 1.0, 20.0),
            Transform::from_xyz(0.0, -0.5, 0.0),
        ));
        // 顶棚：底面 y = 1.42，胶囊站着时头顶 1.40，只留 2cm
        app.world_mut().spawn((
            RigidBody::Static,
            Collider::cuboid(2.0, 1.0, 2.0),
            Transform::from_xyz(0.0, 1.92, 0.0),
        ));

        let link = app
            .world_mut()
            .spawn((
                LatestInput(MoveInput::default()),
                InputGrace(INPUT_GRACE_SECS),
            ))
            .id();
        let avatar = app
            .world_mut()
            .spawn((
                RigidBody::Dynamic,
                Collider::capsule(0.2, 1.0),
                LockedAxes::ROTATION_LOCKED,
                LinearDamping(0.3),
                AngularDamping(0.3),
                LinearVelocity::ZERO,
                JumpCharge::default(),
                AvatarIntent::default(),
                TurnBrake::default(),
                Transform::from_xyz(0.0, 0.7, 0.0),
            ))
            .id();
        app.world_mut().insert_resource(Room {
            avatar_of_link: HashMap::from([(link, avatar)]),
        });

        // 先贴地站定
        for _ in 0..40 {
            app.update();
        }
        let rest_y = app
            .world()
            .entity(avatar)
            .get::<Transform>()
            .unwrap()
            .translation
            .y;

        if let Some(mut li) = app.world_mut().get_mut::<LatestInput>(link) {
            li.0.jump = true;
            // 服务端按序号认「新的一跳」：只抬电平不抬序号不算请求
            li.0.jump_seq = 1;
        }
        let mut peak = rest_y;
        let mut cleared_at = None;
        let mut ever_charging = false;
        for tick in 0..240 {
            if let Some(mut gr) = app.world_mut().get_mut::<InputGrace>(link) {
                gr.0 = INPUT_GRACE_SECS;
            }
            app.update();
            let tr = app.world().entity(avatar).get::<Transform>().unwrap();
            peak = peak.max(tr.translation.y);
            let ch = app.world().entity(avatar).get::<JumpCharge>().unwrap();
            ever_charging |= ch.charging;
            if tick > 0 && ever_charging && !ch.charging && ch.verify_secs <= 0.0 {
                cleared_at = cleared_at.or(Some(tick));
            }
        }

        let ch = app.world().entity(avatar).get::<JumpCharge>().unwrap();
        assert!(
            peak - rest_y < 0.15,
            "顶棚只留 2cm，根不该被抬起来：静止 {rest_y} 峰值 {peak}"
        );
        assert!(
            !ch.charging && ch.verify_secs <= 0.0 && ch.retry_secs <= 0.0,
            "被阻挡的起跳必须退出跳跃状态，实际 charging={} verify={} retry={}",
            ch.charging,
            ch.verify_secs,
            ch.retry_secs
        );
        // 蓄力 0.45s（约 27 tick）+ 验证 0.12s（约 8 tick）：0.8s 内必须收干净
        let cleared_at = cleared_at.unwrap_or(u32::MAX);
        assert!(cleared_at < 60, "退出跳跃状态太慢：第 {cleared_at} tick");
    }

    /// 空中推杆只能**限速**改落点：速度大小原样保留，方向慢慢转过去。
    ///
    /// 旧规则是「一个字都不许动」（落点全锁死），手感上太苛刻；新规则是
    /// 落地段的转身降速：180° 甩头降到下限（不停住）、90° 侧移几乎不受影响、松手就是 0。
    ///
    /// 这条守的是玩家实测的那个问题：拉满反向推杆时人还按满速往后滑，
    /// 混合空间却在反放跑 / 横移之间来回交叉，两套腿叠出残影。
    #[test]
    fn ground_velocity_brakes_on_reversal() {
        let start = tuning::TURN_BRAKE_START_DEG_DEFAULT;
        let floor = tuning::TURN_BRAKE_MIN_SCALE_DEFAULT;
        let dt = 1.0 / 60.0;

        /// 朝同一方向推：身体按 `step_yaw` 转、刹车按本 tick 转速推，
        /// 速度按 `ground_velocity_step` 在速度层逼近目标。
        /// 返回（最低比例, 末帧比例, 末帧朝向, 末帧速度, 走过的路程, 最大横向偏移,
        ///       过程中的最低速度——反向要经过 0）
        fn run(
            dir: Vec2,
            yaw0: f32,
            v0: Vec2,
            ticks: u32,
            start: f32,
            floor: f32,
            dt: f32,
        ) -> (f32, f32, f32, Vec2, f32, f32, f32) {
            let mut brake = 1.0f32;
            let mut yaw = yaw0;
            let mut rate = 0.0f32;
            let mut velocity = v0;
            let mut min_scale = 1.0f32;
            let mut path = 0.0f32;
            let mut lateral = 0.0f32;
            let mut min_speed = v0.length();
            for _ in 0..ticks {
                let target = ground_brake_target(dir, yaw, start, floor);
                brake = tuning::ramp_turn_brake(brake, target, rate, dt);
                min_scale = min_scale.min(brake);
                let goal = ground_velocity(dir, brake);
                let stepped = tuning::ground_velocity_step(
                    velocity.to_array(),
                    goal.to_array(),
                    tuning::REVERSE_ACCEL_MPS2,
                    dt,
                );
                velocity = Vec2::new(stepped[0], stepped[1]);
                min_speed = min_speed.min(velocity.length());
                path += velocity.length() * dt;
                // 只量「和输入方向垂直」的那一轴：输入沿 ±Z 时看 x，沿 ±X 时看 y
                lateral = lateral.max(if dir.x.abs() < 1e-6 {
                    velocity.x.abs()
                } else {
                    velocity.y.abs()
                });
                // 和服务端 `face_movement` 同一条转向
                if dir.length_squared() > 1e-6 {
                    let error = tuning::wrap_pi(dir.x.atan2(dir.y) - yaw);
                    let (next, next_rate) = tuning::step_yaw(yaw, rate, error, dt);
                    yaw = next;
                    rate = next_rate;
                }
            }
            (min_scale, brake, yaw, velocity, path, lateral, min_speed)
        }

        let cruise = Vec2::new(0.0, MOVE_SPEED_MPS);

        // 正前方：满速，斜坡不动
        let (_, scale, _, velocity, _, lateral, _) =
            run(Vec2::new(0.0, 1.0), 0.0, cruise, 10, start, floor, dt);
        assert!(
            (velocity.length() - MOVE_SPEED_MPS).abs() < 1e-4,
            "{velocity:?}"
        );
        assert!((scale - 1.0).abs() < 1e-6);
        assert!(lateral < 1e-4);

        // 甩头第一帧：转速还没起来，刹车给力是 0，速度几乎没掉
        let target = ground_brake_target(Vec2::new(0.0, -1.0), 0.0, start, floor);
        let brake = tuning::ramp_turn_brake(1.0, target, 0.0, dt);
        assert!(brake > 0.99, "第一帧就刹死了：{brake}");

        // 180° 甩头：这是玩家要的「刹住 → 倒退走 → 再转身」
        let (min_scale, _, yaw, velocity, _, lateral, min_speed) =
            run(Vec2::new(0.0, -1.0), 0.0, cruise, 120, start, floor, dt);
        assert!(
            (min_scale - floor).abs() < 1e-4,
            "180° 该压到下限 {floor}，实测最低 {min_scale}"
        );
        // 速度必须过零（先刹住、再反向），而不是一帧翻过去
        assert!(min_speed < 0.2, "反向要经过静止，实测最低 {min_speed}");
        // 两秒后是在下限上倒退走，方向就是输入方向
        assert!(
            velocity.y < -MOVE_SPEED_MPS * floor * 0.9,
            "该倒着往回走了：{velocity:?}"
        );
        // 全程不许横向偏出去（之前取身体朝向时这里会跑出 1.6 m）
        assert!(lateral < 1e-4, "反向过程划出圆弧了：横向 {lateral}");
        assert!(yaw.abs() < 180f32.to_radians());
        assert!(floor > 0.0, "下限不能归零");

        // 90° 侧移：夹角正好在「开始刹车」的边上，速度一帧就换向、腿还在原地转。
        // 身体转得快，夹角很快掉到 90° 以下，刹车就先松手了，所以只到中间档
        let (min_scale, _, _, velocity, _, lateral, _) =
            run(Vec2::new(1.0, 0.0), 0.0, cruise, 60, start, floor, dt);
        assert!(
            min_scale < 0.7 && min_scale > floor + 0.05,
            "90° 应该是中间档，实测 {min_scale}"
        );
        // 换向现在是一条受加速度约束的曲线（不许一帧翻过去），所以过渡期里
        // 垂直分量会从 3 掉到 0——只验「末态」在新方向上，不再拿全程最大值卡它
        assert!(
            velocity.x > 2.0 && velocity.y.abs() < 1e-3,
            "90° 侧移最后该沿着新方向跑：{velocity:?}"
        );
        assert!(lateral <= MOVE_SPEED_MPS + 1e-4, "{lateral}");

        // 过渡段：误差 60° 介于满速和下限之间
        let yaw0 = std::f32::consts::PI - 60f32.to_radians();
        let (_, scale, _, _, _, _, _) = run(
            Vec2::new(0.0, -1.0),
            yaw0,
            Vec2::new(0.0, -MOVE_SPEED_MPS),
            8,
            start,
            floor,
            dt,
        );
        assert!(
            scale > floor + 0.05 && scale < 1.0 - 0.05,
            "60° 应该是中间值，实测 {scale}"
        );

        // 转过来之后：满速沿新方向，降速不漏到稳态
        let (_, scale, yaw, velocity, _, _, _) = run(
            Vec2::new(0.0, -1.0),
            std::f32::consts::PI,
            Vec2::new(0.0, -MOVE_SPEED_MPS),
            60,
            start,
            floor,
            dt,
        );
        assert!(scale > 0.99, "转身完成后该回满速，实测 {scale}");
        assert!(
            velocity.y < -MOVE_SPEED_MPS * 0.9 && yaw.abs() > 90f32.to_radians(),
            "{velocity:?}"
        );

        // 旋钮直接生效
        let (min_scale, _, _, _, _, _, _) =
            run(Vec2::new(0.0, -1.0), 0.0, cruise, 120, start, floor, dt);
        assert!((min_scale - floor).abs() < 1e-4);
        let target = ground_brake_target(Vec2::new(0.0, -1.0), 0.0, start, 1.0);
        assert_eq!(target, 1.0, "下限 1.0 应完全不降速");

        // 松手：目标回到满速，斜坡自己往回爬
        let target = ground_brake_target(Vec2::ZERO, 2.0, start, floor);
        assert_eq!(target, 1.0);
        let brake = tuning::ramp_turn_brake(0.3, target, 0.0, dt);
        assert!(brake > 0.3, "松手后斜坡该往回爬");
        assert_eq!(ground_velocity(Vec2::ZERO, 0.0), Vec2::ZERO);
    }

    /// 「保动量、限转向」。但「推杆当场把落点掰过去」仍然是错的：
    /// 0.2 秒内转过的角度必须远小于 90°。
    ///
    /// 这条和客户端 `movement_system` 是同一条规则（同一个 `air_steer_velocity`）：
    /// 一端给空中控制、另一端不给，同一跳的落点就会差一截，预测端只好被和解来回拽。
    #[test]
    fn airborne_input_steers_gradually_without_changing_speed() {
        let mut app = App::new();
        app.add_plugins(AssetPlugin::default()).init_asset::<Mesh>();
        app.add_message::<AssetEvent<Mesh>>();
        app.add_plugins((MinimalPlugins, TransformPlugin, PhysicsPlugins::default()))
            .init_resource::<JumpTunings>()
            .add_systems(FixedUpdate, drive_avatars)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
                1.0 / 60.0,
            )));
        app.finish();

        app.world_mut().spawn((
            RigidBody::Static,
            Collider::cuboid(20.0, 1.0, 20.0),
            Transform::from_xyz(0.0, -0.5, 0.0),
        ));

        let link = app
            .world_mut()
            .spawn((
                LatestInput(MoveInput::default()),
                InputGrace(INPUT_GRACE_SECS),
            ))
            .id();
        let avatar = app
            .world_mut()
            .spawn((
                RigidBody::Dynamic,
                Collider::capsule(0.2, 1.0),
                LockedAxes::ROTATION_LOCKED,
                LinearDamping(0.3),
                AngularDamping(0.3),
                LinearVelocity::ZERO,
                JumpCharge::default(),
                AvatarIntent::default(),
                TurnBrake::default(),
                // 脸已经朝 +X：地速方向取身体朝向，起步不该先转个 90° 的弧
                NetYaw(std::f32::consts::FRAC_PI_2),
                FacingRate::default(),
                Transform::from_xyz(0.0, 1.0, 0.0),
            ))
            .id();
        app.world_mut().insert_resource(Room {
            avatar_of_link: HashMap::from([(link, avatar)]),
        });

        // 站在地上，朝 +X 跑起来。和真客户端一样，每条快照都带上跳跃手感参数
        // （缺了就是 0 = 完全锁死，那是另一个设计，不是这里要测的）
        if let Some(mut li) = app.world_mut().get_mut::<LatestInput>(link) {
            li.0.dir = [1.0, 0.0];
            li.0.force = 1.0;
            li.0.air_accel = crab_feast_library::net::AIR_ACCEL_DEFAULT;
            li.0.air_steer_secs = crab_feast_library::net::AIR_STEER_SECS_DEFAULT;
            li.0.jump_vy = crab_feast_library::net::JUMP_LAUNCH_VY_DEFAULT;
        }
        for _ in 0..30 {
            if let Some(mut gr) = app.world_mut().get_mut::<InputGrace>(link) {
                gr.0 = INPUT_GRACE_SECS;
            }
            app.update();
        }
        let vx_run = app
            .world()
            .entity(avatar)
            .get::<LinearVelocity>()
            .unwrap()
            .0
            .x;
        assert!(vx_run > 2.5, "先要跑起来，实际 vx={vx_run}");

        // 起跳
        if let Some(mut li) = app.world_mut().get_mut::<LatestInput>(link) {
            li.0.jump = true;
            li.0.jump_seq = 1;
        }
        let mut vx_at_takeoff = 0.0f32;
        let mut airborne = false;
        for _ in 0..120 {
            if let Some(mut gr) = app.world_mut().get_mut::<InputGrace>(link) {
                gr.0 = INPUT_GRACE_SECS;
            }
            app.update();
            let ch = app.world().entity(avatar).get::<JumpCharge>().unwrap();
            if ch.phase == JumpPhase::Air {
                airborne = true;
                vx_at_takeoff = app
                    .world()
                    .entity(avatar)
                    .get::<LinearVelocity>()
                    .unwrap()
                    .0
                    .x;
                break;
            }
        }
        assert!(airborne, "没进入滞空阶段");

        // 空中把摇杆掰到 +Z（和起跳方向正交）
        if let Some(mut li) = app.world_mut().get_mut::<LatestInput>(link) {
            li.0.dir = [0.0, 1.0];
        }
        for _ in 0..12 {
            if let Some(mut gr) = app.world_mut().get_mut::<InputGrace>(link) {
                gr.0 = INPUT_GRACE_SECS;
            }
            app.update();
        }

        let velocity = app
            .world()
            .entity(avatar)
            .get::<LinearVelocity>()
            .unwrap()
            .0;
        let intent = app.world().entity(avatar).get::<AvatarIntent>().unwrap();
        let speed_before = vx_at_takeoff.abs();
        let speed_now = velocity.x.hypot(velocity.z);
        assert!(
            speed_now > speed_before * 0.9 && speed_now <= speed_before * 1.02,
            "空中的速度大小必须保下来：起跳 {speed_before:.2} → 现在 {speed_now:.2}"
        );
        // 0.2s × 4 m/s² 在 3 m/s 上只该转过十几度：vz 明显非零，但远不到把 vx 掰没
        assert!(velocity.z > 0.3, "0.2 秒还一点没转：vz={:.2}", velocity.z);
        assert!(
            velocity.z < 1.5 && velocity.x > speed_before * 0.7,
            "空中推杆把落点瞬间掰过去了：v=({:.2},{:.2})",
            velocity.x,
            velocity.z
        );
        assert!(
            (intent.dir[1] - 1.0).abs() < 1e-3 && intent.dir[0].abs() < 1e-3,
            "朝向意向应当跟着摇杆：{:?}",
            intent.dir
        );
    }
}

/// 出生位分配的回归：编号只增不减会把玩家挪出地面，一出生就在空中下坠。
#[cfg(test)]
mod spawn_tests {
    use super::*;

    /// 编号复用：空房间回到 0，有断档就用最小的空位
    #[test]
    fn free_player_id_reuses_the_lowest_gap() {
        assert_eq!(free_player_id(&HashSet::new()), Some(0));
        let used = HashSet::from([0, 2, 3]);
        assert_eq!(free_player_id(&used), Some(1), "断档里的 1 才是下一个");
        let full: HashSet<u8> = (0..MAX_PLAYERS).collect();
        assert_eq!(free_player_id(&full), None, "满员不能再发编号");
    }

    /// 每个编号的出生位都必须落在地面里、不压方块、脚底贴着地面
    #[test]
    fn spawn_points_stay_inside_the_ground_and_clear_the_blocks() {
        for id in 0..MAX_PLAYERS {
            let p = spawn_point(id);
            assert!(
                p.y >= crab_feast::level::PLAYER_CAPSULE_HALF_HEIGHT,
                "id {id}: 出生高度 {p:?} 低于胶囊半高"
            );
            assert!(
                p.x + 0.2 < crab_feast::level::GROUND_HALF,
                "id {id}: {p:?} 超出地面 X"
            );
            assert!(
                p.z.abs() + 0.2 < crab_feast::level::GROUND_HALF,
                "id {id}: {p:?} 超出地面 Z"
            );
            for cube in crab_feast::level::CUBE_CENTERS {
                let clear_x = (p.x - cube.x).abs() >= 0.5 + 0.2;
                let clear_z = (p.z - cube.z).abs() >= 0.5 + 0.2;
                assert!(
                    clear_x || clear_z,
                    "id {id}: 出生点 {p:?} 和方块 {cube:?} 重叠"
                );
            }
        }
    }
}

#[cfg(test)]
mod input_queue_tests {
    use super::*;

    /// 输入按「该生效的 tick」取用：早到的留着、到点的那一 tick 才被取走
    #[test]
    fn queue_hands_out_the_input_for_its_own_tick() {
        let mut queue = InputQueue {
            cap: 16,
            ..InputQueue::default()
        };
        for tick in [7u32, 8, 9] {
            queue.insert(MoveInput {
                tick,
                dir: [tick as f32, 0.0],
                ..MoveInput::default()
            });
        }
        // 还没到点的 tick 取不到，也不该被吞掉
        assert!(queue.take_for(6).is_none());
        let got = queue.take_for(8).expect("第 8 tick 的输入该在队里");
        assert_eq!(got.tick, 8);
        assert_eq!(got.dir[0], 8.0);
        // 同一个 tick 重复发送（一帧多帧率）只留最后一条
        queue.insert(MoveInput {
            tick: 10,
            dir: [1.0, 0.0],
            ..MoveInput::default()
        });
        queue.insert(MoveInput {
            tick: 10,
            dir: [2.0, 0.0],
            ..MoveInput::default()
        });
        assert_eq!(queue.take_for(10).unwrap().dir[0], 2.0);
    }

    /// 错过生效点太久的残留会被清掉；还在未来的必须留着等它到点
    #[test]
    fn prune_drops_only_stale_entries() {
        let mut queue = InputQueue {
            cap: 16,
            ..InputQueue::default()
        };
        for tick in [5u32, 6, 20] {
            queue.insert(MoveInput {
                tick,
                ..MoveInput::default()
            });
        }
        queue.prune(20);
        assert!(queue.take_for(5).is_none(), "落后 15 tick 的输入该扔掉");
        assert!(queue.take_for(6).is_none());
        assert!(queue.take_for(20).is_some(), "到点的那条要还在");
    }

    /// 客户端 tick 整体落后时：精确命中不了也不能把输入吞掉，取最近的过期那条顶上
    #[test]
    fn lagging_client_still_advances() {
        let mut queue = InputQueue {
            cap: 16,
            ..InputQueue::default()
        };
        for tick in [2u32, 3, 4] {
            queue.insert(MoveInput {
                tick,
                dir: [tick as f32, 0.0],
                ..MoveInput::default()
            });
        }
        // 服务端已经走到第 6 tick，客户端只送到第 4 tick
        let got = queue.take_for(6).expect("落后的输入也要顶上，不能冻住");
        assert_eq!(got.tick, 4, "应该取最近的一条");
        assert!(queue.take_for(7).is_some(), "剩下第 3 tick 那条继续顶");
        assert!(queue.take_for(8).is_some());
        assert!(queue.take_for(9).is_none(), "一条不剩就沿用上一条");
    }

    /// 兜底：客户端 tick 跑飞时不能无限堆
    #[test]
    fn queue_is_capped() {
        let mut queue = InputQueue {
            cap: 4,
            ..InputQueue::default()
        };
        for tick in 0..10u32 {
            queue.insert(MoveInput {
                tick: tick + 1,
                ..MoveInput::default()
            });
        }
        assert!(queue.pending.len() <= 4, "{}", queue.pending.len());
    }
}
