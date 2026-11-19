//! 无窗口的专用服务器（dedicated server）。
//!
//! 它只跑「权威模拟 + 复制」：物理、角色移动、状态广播，不加载任何美术资源，
//! 也不创建窗口。客户端 `crab_feast_pc` 通过 `--server <addr>` 连过来。
//!
//! 启动：`cargo run -p crab_feast_server -- --addr 0.0.0.0:5000`

use std::{collections::HashMap, net::SocketAddr, time::Duration};

use avian3d::prelude::*;
use bevy::{
    log::{Level, LogPlugin},
    prelude::*,
    state::app::StatesPlugin,
    transform::TransformPlugin,
};
use crab_feast_launcher::config::{LauncherConfig, config_path_from_args};
use crab_feast_library::level::spawn_static_level;
use crab_feast_library::net::{
    ControlChannel, CrabProtocolPlugin, DEFAULT_ADDR, JoinRequest, MOVE_SPEED_MPS, MoveInput,
    NETCODE_DEV_KEY, NETCODE_PROTOCOL_ID, NetMotion, NetPlayerId, NetPos, NetYaw, TICK_HZ, Welcome,
};
mod beacon;

use crab_feast_library::tuning;
use lightyear::{
    netcode::prelude::server::{NetcodeConfig, NetcodeServer},
    prelude::{server::*, *},
};

/// 起跳初速度（米/秒）。
const JUMP_SPEED: f32 = 5.0;
/// 认为「站在地上」的高度阈值（胶囊半高 1.0 + 半径 0.2，地面在 y≈0）。
const GROUND_EPSILON: f32 = 1.15;
/// 一个房间最多多少个玩家位。
///
/// `player_id` 是 u8，但真正的上限是「复制带宽 ÷ 每人下行」，
/// 100 是当前设计目标，超过就拒绝，别让编号偷偷回绕。
const MAX_PLAYERS: u8 = 100;

/// 房间状态：连接 → 角色实体 的映射，以及下一个玩家编号。
#[derive(Resource, Default)]
struct Room {
    next_player_id: u8,
    /// link 实体（lightyear 为每个连接生成一个）→ 该玩家的角色实体
    avatar_of_link: HashMap<Entity, Entity>,
}

/// 标记：这个连接已经 Join 过，角色实体已创建（防止重复 Join 再建一个）。
#[derive(Component)]
struct Joined(u8);

fn main() {
    let (addr, exit_after, log_state_enabled) = parse_args();

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
    .insert_resource(ServerAddr(addr))
    // 物理与轻年都跑 FixedUpdate，把步长对齐到同一个 tick，避免两边时钟各走各的
    .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
    .add_systems(Startup, setup)
    .add_systems(Update, beacon::broadcast)
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
    .add_systems(FixedUpdate, drive_avatars)
    .add_systems(
        FixedPostUpdate,
        (measure_speed, face_movement, publish_state)
            .chain()
            .after(PhysicsSystems::Writeback),
    );

    if let Some(secs) = exit_after {
        app.insert_resource(ExitAfter(Timer::from_seconds(secs, TimerMode::Once)))
            .add_systems(Update, exit_when_elapsed);
    }

    if log_state_enabled {
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

/// `--exit-after <秒>`：跑够时间自动退出。给 CI / 冒烟测试用，
/// 也免得手工起服务器时还得找进程杀。
#[derive(Resource)]
struct ExitAfter(Timer);

fn exit_when_elapsed(
    time: Res<Time>,
    mut timer: ResMut<ExitAfter>,
    mut exit: MessageWriter<AppExit>,
) {
    if timer.0.tick(time.delta()).just_finished() {
        info!("--exit-after 到点，服务器退出");
        exit.write(AppExit::Success);
    }
}

fn parse_args() -> (SocketAddr, Option<f32>, bool) {
    let config = LauncherConfig::load(config_path_from_args().as_deref());
    let mut args = std::env::args().skip(1);
    // 优先级：--addr > launcher.json 的 server.addr > 协议默认地址
    let mut addr: Option<String> = None;
    let mut exit_after: Option<f32> = None;
    let mut log_state = false;
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
            "--exit-after" => {
                if let Some(value) = args.next() {
                    exit_after = value.parse().ok();
                }
            }
            "--log-state" => log_state = true,
            "--help" | "-h" => {
                println!(
                    "crab-feast-server [--addr 0.0.0.0:5000] [--config launcher.json] \
                     [--exit-after <秒>] [--log-state]\n\
                     不传 --addr 时读 launcher.json 的 server.addr。"
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
    (addr, exit_after, log_state)
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

fn setup(mut commands: Commands, addr: Res<ServerAddr>) {
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

    info!("dedicated server listening on {}", addr.0);

    // 定时 UDP 广播自己的地址/端口/人数，客户端大厅直接收，不用手输地址
    beacon::start(&mut commands, addr.0);
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
) {
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

        let player_id = room.next_player_id;
        if player_id >= MAX_PLAYERS {
            warn!("房间已满（{MAX_PLAYERS}），拒绝 {link:?} 的 Join");
            continue;
        }
        room.next_player_id += 1;

        // 出生点沿 X 轴拉开，避免多个胶囊叠在一起
        let spawn_x = (player_id as f32) * 1.5;
        let avatar = commands
            .spawn((
                Transform::from_xyz(spawn_x, 1.0, 0.0),
                RigidBody::Dynamic,
                Collider::capsule(0.2, 1.0),
                LockedAxes::ROTATION_LOCKED,
                LinearDamping(0.3),
                AngularDamping(0.3),
                LinearVelocity::ZERO,
                Replicate::default(),
                NetPlayerId(player_id),
                NetPos([spawn_x, 1.0, 0.0]),
                NetYaw(0.0),
                NetMotion {
                    dir: [0.0, 0.0],
                    speed: 0.0,
                    intent: 0.0,
                },
                FacingRate::default(),
                AvatarIntent::default(),
                Name::new(format!("player-{player_id}")),
            ))
            .id();

        room.avatar_of_link.insert(link, avatar);
        commands.entity(link).insert(Joined(player_id));
        sender.send::<ControlChannel>(Welcome { player_id });
        info!("join: link {link:?} → player {player_id}（角色实体 {avatar:?}）");
    }
}

/// 连接断开：把它的角色实体一起收掉，并释放映射。
fn on_disconnect(trigger: On<Add<Disconnected>>, mut commands: Commands, mut room: ResMut<Room>) {
    let link = trigger.entity;
    let Some(avatar) = room.avatar_of_link.remove(&link) else {
        return;
    };
    commands.entity(avatar).despawn();
    info!("leave: link {link:?} 断开，角色实体 {avatar:?} 已回收");
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

/// 距离「上一次收到输入」还剩多久，收到输入时重置为 `INPUT_GRACE_SECS`。
#[derive(Component)]
struct InputGrace(f32);

/// 消费消息，只把最新一条存到 link 上。
///
/// 必须排在 `MessageSystems::ReceiveMessages` 之后，否则读到的永远是空队列。
fn collect_inputs(
    mut links: Query<(
        &mut MessageReceiver<MoveInput>,
        &mut LatestInput,
        &mut InputGrace,
    )>,
) {
    for (mut receiver, mut latest, mut grace) in links.iter_mut() {
        let mut newest = None;
        // 一帧可能收到多条，只认最后一条（输入是「当前状态」不是「事件流」）
        for message in receiver.receive() {
            newest = Some(message);
        }
        if let Some(input) = newest {
            latest.0 = input;
            grace.0 = INPUT_GRACE_SECS;
        }
    }
}

/// 每个逻辑帧把最新输入写成速度，并推进宽限期。
fn drive_avatars(
    time: Res<Time<Fixed>>,
    mut links: Query<(Entity, &LatestInput, &mut InputGrace)>,
    mut avatars: Query<(&mut LinearVelocity, &mut AvatarIntent, &Transform)>,
    room: Res<Room>,
) {
    let dt = time.delta_secs();
    for (link, latest, mut grace) in links.iter_mut() {
        let Some(&avatar) = room.avatar_of_link.get(&link) else {
            continue;
        };
        let Ok((mut velocity, mut intent, transform)) = avatars.get_mut(avatar) else {
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
        velocity.0.x = direction.x * MOVE_SPEED_MPS;
        velocity.0.z = direction.y * MOVE_SPEED_MPS;

        // 跳跃：只在地面附近接受，避免空中连跳
        if latest.0.jump && transform.translation.y <= GROUND_EPSILON {
            velocity.0.y = JUMP_SPEED;
        }
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

/// 朝向限速的跨帧状态（度/秒）。
///
/// 没有它，「起步第一帧就满速」会把角加速度那一段吃掉——单机的手感全靠它。
#[derive(Component, Default)]
struct FacingRate(f32);

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
fn publish_state(mut avatars: Query<(&Transform, &mut NetPos, &mut NetMotion, &AvatarIntent)>) {
    for (transform, mut pos, mut motion, intent) in avatars.iter_mut() {
        pos.0 = transform.translation.to_array();
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
