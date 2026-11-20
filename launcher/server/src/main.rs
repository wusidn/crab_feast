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
use crab_feast::anim::jump::{JumpKind, JumpProfile, JumpTunings};
use crab_feast::ground;
use crab_feast::level::spawn_static_level;
use crab_feast_launcher::config::{LauncherConfig, config_path_from_args};
use crab_feast_library::net::{
    ControlChannel, CrabProtocolPlugin, DEFAULT_ADDR, JoinRequest, MOVE_SPEED_MPS, MoveInput,
    NETCODE_DEV_KEY, NETCODE_PROTOCOL_ID, NetMotion, NetPlayerId, NetPos, NetTimeScale, NetYaw,
    TICK_HZ, TimeScaleRequest, Welcome,
};
mod beacon;

use crab_feast_library::tuning;
use lightyear::{
    netcode::prelude::server::{NetcodeConfig, NetcodeServer},
    prelude::{server::*, *},
};

/// 跳跃的蓄力时长 / 离地速度由 `crab_feast::anim::jump::JumpTunings` 统一定义
/// （跑跳 / 站跳 / 上跳 / 下跳各一档）。服务端不再自己写死一份数字：客户端调哪一档，
/// 这里跟着一起变，两端不会再有「本地 2.6 / 服务端 5.0」的分叉。
///
/// 站 / 跑蓄力分档的地速分界（米/秒），与客户端 `RUN_JUMP_SPEED` 对齐。
/// 只给测试断言用：正式分档走 `JumpProfile::for_jump`。
#[cfg(test)]
const RUN_JUMP_SPEED: f32 = 1.5;
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
    // 四档跳跃调参：站 / 跑 / 上 / 下。与客户端默认值同源，调一处两端一致。
    .init_resource::<JumpTunings>()
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
                AvatarLocal::default(),
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

/// 距离「上一次收到输入」还剩多久，收到输入时重置为 `INPUT_GRACE_SECS`。
#[derive(Component)]
struct InputGrace(f32);

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
}

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
            if input.jump && !latest.0.jump {
                info!(
                    "server: 收到跳跃请求 seq={} kind={}",
                    input.jump_seq, input.jump_kind
                );
            }
            latest.0 = input;
            grace.0 = INPUT_GRACE_SECS;
        }
    }
}

/// 每个逻辑帧把最新输入写成速度，并推进宽限期。
fn drive_avatars(
    time: Res<Time<Fixed>>,
    tunings: Res<JumpTunings>,
    mut links: Query<(Entity, &LatestInput, &mut InputGrace)>,
    mut avatars: Query<(Entity, &mut LinearVelocity, &mut AvatarIntent, &mut JumpCharge, &Transform)>,
    room: Res<Room>,
    spatial: SpatialQuery,
) {
    let dt = time.delta_secs();
    for (link, latest, mut grace) in links.iter_mut() {
        let Some(&avatar) = room.avatar_of_link.get(&link) else {
            continue;
        };
        let Ok((entity, mut velocity, mut intent, mut charge, transform)) = avatars.get_mut(avatar)
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

        // 跳跃：先贴地蓄力，蓄满才给离地速度（见 [`advance_charge`]）
        // 接地用脚下射线而不是绝对高度：站在 1m 方块上（胶囊中心 y≈1.7）也要能跳
        let grounded = ground::grounded(
            &spatial,
            entity,
            transform,
            velocity.0.y,
            GROUND_NOISE_VY,
        );
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
                info!(
                    "server: 起跳失败（根未抬升），退出跳跃状态 y={:.3} vy={:.3} grounded={grounded}",
                    transform.translation.y, velocity.0.y
                );
            }
        }
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
        if let Some(vy) = launched {
            velocity.0.y = vy;
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
        if std::env::var("CRAB_LOG_SRV").is_ok()
            && (charge.charging || velocity.0.y.abs() > 0.02)
        {
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
        assert!(advance_charge(&mut c, true, 1, false, stand_launch(), 0.0, &tunings(), DT).is_none());
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
            launched = advance_charge(&mut c, true, 1, true, 0.0, RUN_JUMP_SPEED, &tunings(), DT).is_some();
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
            launched = advance_charge(&mut c, true, 1, true, 0.192_409_41, 0.0, &tunings(), DT).is_some();
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
        let end: f32 = app.world().entity(body).get::<Transform>().unwrap().translation.y;
        println!("PROBE peak_y={peak} end_y={end}");
        assert!(peak > 1.5, "峰值应明显高于初始 1.0，实际 {peak}");
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
            .spawn((LatestInput(MoveInput::default()), InputGrace(INPUT_GRACE_SECS)))
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
                Transform::from_xyz(0.0, 1.0, 0.0),
            ))
            .id();

        app.world_mut().insert_resource(Room {
            next_player_id: 0,
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
        let end: f32 = app.world().entity(avatar).get::<Transform>().unwrap().translation.y;
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
            .spawn((LatestInput(MoveInput::default()), InputGrace(INPUT_GRACE_SECS)))
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
                Transform::from_xyz(0.0, 2.0, 0.0),
            ))
            .id();

        app.world_mut().insert_resource(Room {
            next_player_id: 0,
            avatar_of_link: HashMap::from([(link, avatar)]),
        });

        for _ in 0..40 {
            app.update();
        }
        let rest_y = app.world().entity(avatar).get::<Transform>().unwrap().translation.y;
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
            .spawn((LatestInput(MoveInput::default()), InputGrace(INPUT_GRACE_SECS)))
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
                Transform::from_xyz(0.0, 0.7, 0.0),
            ))
            .id();
        app.world_mut().insert_resource(Room {
            next_player_id: 0,
            avatar_of_link: HashMap::from([(link, avatar)]),
        });

        // 先贴地站定
        for _ in 0..40 {
            app.update();
        }
        let rest_y = app.world().entity(avatar).get::<Transform>().unwrap().translation.y;

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
}
