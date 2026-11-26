//! 客户端联网接入（v1：权威服务器 + 状态复制，不含预测）。
//!
//! 登录流程：
//! 1. `Lobby` 点「加入」后进 `Login`，`connect` 在这一步建连接实体（netcode 握手）；
//! 2. 握手成功（`Connected`）才发 `JoinRequest`——**连接 ≠ 玩家**；
//! 3. 服务端收到 Join 才分配 `player_id`、生成角色实体、回 `Welcome`；
//! 4. 客户端收到 `Welcome` → 进 `Game` 建场景，复制下来的那个实体被认领成 [`Avatar`]。
//!
//! 渲染取舍（权威快照 + 直接写位置，外加一层纯视觉插值）：
//! - 角色实体一律由服务端复制，客户端不本地生成，避免"两个人在同一个角色上写 Transform"；
//! - **所有玩家（含本地）都是 `Kinematic`**：`Position` / `Rotation` 每帧直接由
//!   `NetPos` / `NetYaw` 赋值（见 `net_pose.rs`），客户端不做预测、不做残差修正；
//! - `Transform` 只有一份是插值出来的（[`PoseSmoothing`]）：在两份快照之间推进，
//!   固定落后一个可配的 tick 数（本地 1.25 / 远端 3.0，即 21 / 50 ms）。它不预测、不外推、不回写物理，只为把 60 Hz 采样
//!   落在显示器帧上产生的台阶铺平；余量见底/积压时靠 ±10% 的时间伸缩排掉，
//!   不靠硬夹（硬夹在真实网络上就是"冻结一帧再补格"）；
//! - 本地角色挂 `Avatar`，交给动画系统那套单实例管线（混合空间 / 脚部 IK）；
//! - 管线要的动画参数（[`LocomotionIntent`] / [`FacingState`]）在联网模式下没有本地物理
//!   去写，由 `drive_net_animation` 每帧从复制下来的 `NetMotion` / `NetYaw` 翻译过来——
//!   漏了这步，角色会以 Idle 姿态在地面上滑行；
//! - 其他玩家按 `PlayerModelAssets` 各自实例化一份模型，姿态同样由混合空间求值。

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use avian3d::prelude::*;
use bevy::{
    animation::{AnimatedBy, AnimationTargetId},
    ecs::system::SystemParam,
    prelude::*,
    window::WindowOccluded,
};
use crab_feast_library::net::{
    ControlChannel, CrabProtocolPlugin, JoinRequest, MOVE_SPEED_MPS, MoveInput, NetJump, NetMotion,
    NetPlayerId, NetPos, NetTimeScale, NetYaw, TICK_HZ, TimeScaleRequest, Welcome,
};
use lightyear::netcode::prelude::{
    Authentication,
    client::{NetcodeClient, NetcodeConfig},
};
use lightyear::prelude::{client::*, *};

use crate::anim::core::{SpaceEvalContext, evaluate_space};
use crate::anim::jump::{
    JumpTunings, RemoteJump, blend_clip_pose, remote_jump_setup, uplink_jump_level,
};
use crate::anim::{
    AnimCatalog, AnimSelection, AnimationSet, ClipSet, FootIk, FootIkSet, JumpAction, JumpSettings,
    MixerCache, Skeleton, build_skeleton,
};
use crate::camera::CameraFacing;
use crate::ground::foot_ground_dist;
use crate::input::{
    ControlSet, FacingState, JumpInput, LocomotionIntent, MovementInput, wrap_pi,
};
use crate::net_pose::{PoseSmoothing, apply_authoritative_pose, drive_net_animation, smooth_avatar_pose};
use crate::predict::{
    PredClock, Prediction, anchor_clock, pred_reconcile, pred_record, pred_render, pred_velocity,
};
use crate::scene::Avatar;
use crate::state::{AbortSession, AutoJoin, LobbyNotice, NetDiagnostics, SessionMode};
use crate::ui::debug::{DebugTimeScale, TimeScaleChanged};
use crate::{BUILD_TAG, GameState};

/// 服务器地址，由 launcher 通过 `--server` 写入。
#[derive(Resource, Debug, Clone, Copy)]
pub struct NetConfig {
    pub server: SocketAddr,
    /// netcode 的客户端编号：同一台服务器上每个客户端必须不同
    pub client_id: u64,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            server: crab_feast_library::net::DEFAULT_ADDR
                .parse()
                .expect("默认地址应可解析"),
            client_id: 1,
        }
    }
}

/// 本机控制的玩家编号（由服务器的 `Welcome` 告知）。
#[derive(Resource, Debug, Clone, Copy)]
pub struct LocalPlayer(pub u8);

/// 登录阶段。只在日志/调试面板用，逻辑一律以「有没有 `LocalPlayer`」为准。
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoginPhase {
    /// 还没进场景（单机模式或主菜单）
    #[default]
    Idle,
    /// 连接实体已建，netcode 握手进行中
    Connecting,
    /// 握手完成，已发 `JoinRequest`，等 `Welcome`
    Handshaking,
    /// 进了房间，拿到 `player_id`
    InGame(u8),
}

/// 前后台切换的现场记录。
///
/// iOS 进后台会冻结所有 socket，服务端按 netcode 默认 `timeout_seconds = 15`
/// 把连接判死，玩家回来看到的是「连接服务器超时」。所以挂起时主动断开、
/// 回前台再重连，两头都不留半死不活的连接。
#[derive(Resource, Default, Debug)]
pub struct SuspendState {
    /// 挂起前连着的服务器
    pending: Option<SocketAddr>,
    /// 已收到「回前台」，等残留 client 收干净就重连
    want_resume: bool,
}

pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            ClientPlugins {
                tick_duration: core::time::Duration::from_secs_f64(1.0 / TICK_HZ),
            },
            // 连接层（连接层 = netcode）会自己装上 connection::client::ConnectionPlugin，
            // 这里再手动加一次会 panic（plugin was already added）
            // 协议必须在 Client 插件之后注册，且两端用同一个插件
            CrabProtocolPlugin,
        ))
        .add_systems(Startup, setup_visual_assets)
        // 物理步长与服务器 tick 对齐：两端都是 60Hz，静态场景与角色碰撞在同一
        // 时间粒度上演算；步长不一致时，同一段位移在两侧会解出不同的接触结果
        .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
        .init_resource::<NetConfig>()
        // 本地预测的时间轴：上锚后每个定步长 +1
        .init_resource::<PredClock>()
        .init_resource::<LoginPhase>()
        .init_resource::<SuspendState>()
        // 世界时间速度上行：拖动滑杆 → 节流发包；服务端复制回来的值 → 写回资源
        .init_resource::<TimeScaleUplink>()
        // 只在 `Game` 里跑：调试面板本身也只在对局中出现，退出对局后残留的复制值
        // 不该把大厅的时间又拽回慢放
        // lightyear 的客户端时钟同步在 `Last` 里绝对写 `Time<Virtual>` 的速度，会把
        // 调试滑杆的倍数擦掉（表现为「资源 ×0.20、世界还是 1×」）。本帧的 delta 在
        // `First`/`TimeSystems` 里算，所以在那里按「同步基准 × 调试倍数」重写一次。
        .add_systems(
            First,
            apply_effective_time_scale.before(bevy::time::TimeSystems),
        )
        .add_systems(
            Update,
            (
                // 顺序要紧：先读本地改动（进静默窗），再拿复制值对齐。
                // 反过来的话，拖动第一帧服务端上一版的值会先把本地拽回旧值。
                send_time_scale_request,
                apply_replicated_time_scale,
            )
                .chain()
                .run_if(in_state(GameState::Game)),
        )
        // 登录阶段才建连接；大厅与本地游戏都不该有客户端实体
        .add_systems(OnEnter(GameState::Login), connect)
        // 前后台切换：挂起时立刻断开、回前台重连。挂在 `Update` 上不带状态过滤，
        // 大厅/登录/对局都要能反应；重连必须晚一帧做，见 `resume_reconnect`。
        .add_systems(Update, (abort_login, watch_occlusion, resume_reconnect))
        .add_systems(OnExit(GameState::Game), leave_game)
        .add_observer(spawn_remote_visual)
        .add_observer(on_connecting)
        .add_observer(send_join_request)
        .add_observer(on_disconnected)
        .add_systems(Update, prepare_player_models)
        // 远端模型的根位移锁必须排在动画求值之后、变换传播之前：
        // 动画每帧都会把 Hips 往前推，我们在同一步把它拉回原位，让「看得见的身体」
        // 和复制下来的根节点位置严格重合
        .add_systems(
            PostUpdate,
            (
                drive_remote_pose,
                // 髋的水平锁必须在姿态写完之后：混合出来的髋位同样带根位移
                lock_remote_hips,
            )
                .chain()
                .after(bevy::app::AnimationSystems)
                // 脚部 IK 解的是「这一帧最终的姿态」：远端姿态必须先写完再让它解，
                // 否则 IK 拿到的是一帧前的腿，脚会被锁在上一帧的位置上
                .before(FootIkSet)
                .before(bevy::transform::TransformSystems::Propagate)
                // 首启（热更资源还没落地）时下面这些资源全都不存在，`Res<T>` 校验
                // 会直接 abort。资产就绪之前让这两个系统整个让路。
                .run_if(remote_pose_ready),
        )
        // 预测端每个定步长跑一遍：先按输入推速度（物理在 `FixedPostUpdate` 积分），
        // 积分完再把结果记进轨迹环并推进 tick。顺序靠 `PhysicsSystems::Writeback` 保证。
        // 视觉转身：把「根节点硬转」摊到支撑期。必须排在 `drive_pose` 之后
        // （本帧的支撑权重已经写进 `FootIk`）、`FootIkSet` 之前（IK 要看到本帧的根朝向）
        .add_systems(
            PostUpdate,
            crate::anim::turn_visual::follow_visual_yaw
                .after(crate::anim::driver::drive_pose)
                .before(FootIkSet),
        )
        .add_systems(FixedUpdate, pred_velocity)
        .add_systems(FixedPostUpdate, pred_record.after(PhysicsSystems::Writeback))
        .add_systems(
            Update,
            (
                // 相机这一帧要读角色的 Transform，所以复制状态必须先落地；
                // 复制状态落地后立刻翻译成动画参数，动画驱动这一帧才有东西可读
                (
                    on_welcome,
                    claim_local_avatar,
                    // 本地预测：先上锚（第一份快照给出服务端 tick），再拿这一份快照和解。
                    // 这两步只作用于挂 `Prediction` 的本地角色；远端玩家仍是下面的直写 + 插值。
                    anchor_clock,
                    pred_reconcile,
                    // 远端：`Position` / `Rotation` 取服务端快照的权威值，
                    // 再把 `NetMotion` 翻译成本地动画参数；最后只对**视觉** `Transform`
                    // 做一层两快照之间的插值（不外推、不回写 `Position`）
                    apply_authoritative_pose,
                    smooth_avatar_pose,
                    // 本地：插值 + 视觉软偏移（校正差值在这里被慢慢吃掉）
                    pred_render,
                    drive_net_animation,
                )
                    .chain()
                    .before(ControlSet::Camera),
                // 模型必须等"谁是本地玩家"定下来再挂：本地角色走动画系统的单实例管线，
                // 提前挂简易模型会在认领那一帧闪一下
                (attach_player_models, setup_clip_models)
                    .chain()
                    .after(claim_local_avatar),
                // 自动跳覆盖 JumpInput，要排在键盘跳采样之后、上行发包之前
                auto_jump_input
                    .after(ControlSet::Move)
                    .before(AnimationSet::Publish)
                    .run_if(in_state(GameState::Game)),
                // 输入要在移动系统把摇杆方向折算成世界方向之后采样；
                // 排在动画 Drive 之后，才能读到本帧动作层是否已放行起跳窗口；
                // 出大厅/断线后连接实体已不在，这里也不会再发包
                send_input
                    .after(auto_jump_input)
                    .after(AnimationSet::Drive)
                    .run_if(in_state(GameState::Game)),
            ),
        );
    }
}

#[derive(Resource)]
struct RemoteVisuals {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

fn setup_visual_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(RemoteVisuals {
        // 与服务器碰撞体同参数：半径 0.2、圆柱段 1.0
        mesh: meshes.add(Capsule3d::new(0.2, 1.0)),
        material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.85, 0.35, 0.25),
            ..default()
        }),
    });
}

fn local_bind_addr(server: SocketAddr) -> SocketAddr {
    if server.is_ipv6() {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::from(([0, 0, 0, 0], 0))
    }
}

/// iOS 真机第一次访问局域网时会触发「本地网络」权限；如果用户拒绝，
/// sendto 会失败或包被系统丢掉，表现就是永远「正在连接」。
/// 真连接之前先用一个临时 UDP socket 发一个探测包：失败就把系统错误
/// 带回大厅，至少能区分「socket/权限问题」和「网络路径问题」。
#[cfg(target_os = "ios")]
fn ios_udp_probe(server: SocketAddr) -> std::io::Result<()> {
    let bind = local_bind_addr(server);
    let socket = std::net::UdpSocket::bind(bind)?;
    socket.send_to(b"CRAB-FE-PROBE", server)?;
    info!("iOS UDP 预检成功：{bind} → {server}");
    Ok(())
}

#[cfg(not(target_os = "ios"))]
fn ios_udp_probe(_server: SocketAddr) -> std::io::Result<()> {
    Ok(())
}

fn connect(
    mut commands: Commands,
    config: Res<NetConfig>,
    mode: Res<SessionMode>,
    existing: Query<Entity, With<Client>>,
    mut notice: ResMut<LobbyNotice>,
    // 同一个系统里既要写又要读同一种消息会撞 B0002（`Res<Messages<T>>` 重复借用，
    // 表现是一进登录就直接 non-unwinding panic），所以用 `ParamSet` 串行拿两次访问。
    // 第二个参数只用来丢弃过期消息，见下面的说明
    mut abort: ParamSet<(MessageWriter<AbortSession>, MessageReader<AbortSession>)>,
    mut next: ResMut<NextState<GameState>>,
    mut diagnostics: ResMut<NetDiagnostics>,
) {
    // 前后台切换时 `occluded=true` / `false` 会攒到同一帧一起投递，挂起分支写下的
    // `AbortSession` 要留到下一帧（`OnEnter(Login)`）才被 `abort_login` 读到 —— 那时
    // 新连接已经建好，这条过期消息会把刚建好的客户端当旧连接断掉，表现为「回前台
    // 一秒内闪一下登录 → 直接回大厅报连不上」。进这里是新一轮连接，先把它吃掉。
    if abort.p1().read().next().is_some() {
        crate::assetpack::log_line("connect: 丢弃一条过期的 AbortSession");
    }

    // 只有「从大厅点了某台服务器加入」才建连接；本地游戏走的是纯单机路径
    let SessionMode::Online { server } = *mode else {
        info!("net: 本地会话，不建立服务器连接");
        return;
    };

    let bind = local_bind_addr(server);
    diagnostics.0 = format!("build {BUILD_TAG}｜目标 {server}｜本地 {bind}｜预检中…");
    crate::assetpack::log_line(&format!("connect: 开始，目标 {server}，本地 {bind}"));

    if let Err(error) = ios_udp_probe(server) {
        error!("iOS UDP 预检失败：{error}");
        crate::assetpack::log_line(&format!("connect: 预检失败 {error}"));
        diagnostics.0 = format!("build {BUILD_TAG}｜目标 {server}｜本地 {bind}｜预检失败：{error}");
        notice.0 = Some(format!(
            "UDP 发送失败：{error}（请在 设置 → 隐私与安全性 → 本地网络 中允许 Crab Feast）"
        ));
        abort.p0().write(AbortSession);
        next.set(GameState::Lobby);
        return;
    }
    diagnostics.0 = format!("build {BUILD_TAG}｜目标 {server}｜本地 {bind}｜预检已发送");
    crate::assetpack::log_line("connect: 预检通过，客户端实体创建");

    // 上一次失败的连接可能还留着实体：先收掉，保证同一时间只有一个 client
    for entity in &existing {
        commands.entity(entity).try_despawn();
    }

    // 连接层：lightyear 的"裸 UdpIo"只是链路，握手/加密/Connected 状态由 netcode 负责。
    // 开发期用明文私钥自签 token；上线要换成后端签发的 Authentication::Token
    let netcode = NetcodeClient::new(
        Authentication::Manual {
            server_addr: server,
            client_id: config.client_id,
            private_key: crab_feast_library::net::NETCODE_DEV_KEY,
            protocol_id: crab_feast_library::net::NETCODE_PROTOCOL_ID,
        },
        NetcodeConfig::default(),
    )
    .expect("netcode 客户端初始化失败");

    let entity = commands
        .spawn((
            Client,
            netcode,
            UdpIo::default(),
            // 本地 socket 必须和服务器地址同族：给 IPv6 服务器从一个
            // 0.0.0.0 的 IPv4 socket 发包会直接 EAFNOSUPPORT，表现就是
            // 大厅点「加入」后永远停在「正在连接」。
            LocalAddr(local_bind_addr(server)),
            PeerAddr(server),
            ReplicationReceiver,
            MessageManager::default(),
            // 注册了 ClientToServer 方向后 sender 本应自动插，这里显式写清楚，
            // 免得协议改动时静默丢输入
            MessageSender::<MoveInput>::default(),
            // 世界时间速度的上行也走同一套：拖动滑杆时偶尔发一条，不能让协议改动
            // 把这条请求悄悄丢掉（丢了表现为"滑杆动了，别的端没动"）
            MessageSender::<TimeScaleRequest>::default(),
            Name::new("net-client"),
        ))
        .id();

    commands.trigger(Connect { entity });
    info!(
        "connecting to {} as client {} (entity {entity:?})",
        server, config.client_id
    );
}

/// 玩家在登录页点了取消：把正在握手/已连上的客户端断开。
///
/// 先把阶段清回 `Idle`，这样随后的 `Disconnected` 不会当成「掉线」再弹一次提示。
fn abort_login(
    mut requests: MessageReader<AbortSession>,
    mut phase: ResMut<LoginPhase>,
    clients: Query<Entity, With<Client>>,
    mut commands: Commands,
) {
    if requests.read().next().is_none() {
        return;
    }
    info!("net: 登录被取消，断开客户端");
    crate::assetpack::log_line(&format!(
        "abort_login: 断开 {} 个 client，phase={}",
        clients.iter().count(),
        match &*phase {
            LoginPhase::Idle => "Idle",
            LoginPhase::Connecting => "Connecting",
            LoginPhase::Handshaking => "Handshaking",
            LoginPhase::InGame(_) => "InGame",
        }
    ));
    *phase = LoginPhase::Idle;
    for entity in &clients {
        commands.trigger(Disconnect { entity });
    }
}

/// 退出 `Game`：本机的玩家身份与连接状态一起收掉。
///
/// 正常断线时 `on_disconnected` 已经处理过；这里兜住「退出场景但连接还在」的情况
/// （例如调试时直接切状态），避免下次登录复用一个旧连接。
fn leave_game(
    mut commands: Commands,
    mut phase: ResMut<LoginPhase>,
    mut uplink: ResMut<TimeScaleUplink>,
    clients: Query<Entity, With<Client>>,
) {
    *phase = LoginPhase::Idle;
    crate::assetpack::log_line(&format!(
        "leave_game: 断开 {} 个 client",
        clients.iter().count()
    ));
    // 退出对局时清掉还没发完的时间速度提议：不然下一局连上新服务器时，
    // 那条陈旧的 pending 会被发出去，把**别人**那台服务器的世界也拖慢
    *uplink = TimeScaleUplink::default();
    commands.remove_resource::<LocalPlayer>();
    for entity in &clients {
        commands.trigger(Disconnect { entity });
    }
}

/// 进后台 / 回前台。
///
/// 判据是 `WindowOccluded`：bevy 0.20 的 `AppLifecycle` 只有赋值、没有
/// `write_message`（全 crate 搜不到构造处），iOS 上真正能收到的是 winit 从
/// `applicationDidEnterBackground` 发出的 `WindowEvent::Occluded`。
///
/// 注意这两个事件**几乎总是攒到回前台那一帧一起投递**（挂起期间不跑帧），
/// 所以这里的挂起分支只负责「干净地断」，重连交给 `resume_reconnect` 延后一帧做。
fn watch_occlusion(
    mut occluded: MessageReader<WindowOccluded>,
    state: Res<State<GameState>>,
    mode: Res<SessionMode>,
    mut phase: ResMut<LoginPhase>,
    mut suspend: ResMut<SuspendState>,
    clients: Query<Entity, With<Client>>,
    mut commands: Commands,
    mut notice: ResMut<LobbyNotice>,
) {
    for event in occluded.read() {
        let current = state.get();
        let mode_now = *mode;
        crate::assetpack::log_line(&format!(
            "lifecycle: occluded={} state={current:?} mode={mode_now:?} pending={:?}",
            event.occluded, suspend.pending
        ));

        if event.occluded {
            // 只有联机会话需要记现场；本地单机没有网络可断
            if !matches!(current, GameState::Game | GameState::Login) {
                continue;
            }
            let SessionMode::Online { server } = mode_now else {
                continue;
            };
            if suspend.pending.is_some() {
                continue;
            }
            suspend.pending = Some(server);
            suspend.want_resume = false;
            notice.0 = None;
            // 先清阶段再断：`on_disconnected` 只看得到 `Idle`，不会当成掉线回大厅
            *phase = LoginPhase::Idle;
            for entity in &clients {
                commands.trigger(Disconnect { entity });
            }
            crate::assetpack::log_line(&format!(
                "lifecycle: 挂起，主动断开 {server}（断开 {} 个 client）",
                clients.iter().count()
            ));
        } else if suspend.pending.is_some() {
            suspend.want_resume = true;
            crate::assetpack::log_line("lifecycle: 回前台，等旧 client 落地后重连");
        }
    }
}

/// 回前台后的重连，**必须比断开晚一帧以上**。
///
/// 同一帧里「断旧 client」和「建新 client」一起做，老的 netcode 客户端和它的
/// socket 还没释放，新客户端建出来会立刻被置成 `Disconnected`（实测 entity 997v1：
/// `phase=Connecting` 就掉线），表现就是刚回来又弹「连不上」。所以先等旧 client
/// 从世界里面消失，再进登录流程。
fn resume_reconnect(
    mut suspend: ResMut<SuspendState>,
    mut mode: ResMut<SessionMode>,
    mut phase: ResMut<LoginPhase>,
    mut auto: ResMut<AutoJoin>,
    state: Res<State<GameState>>,
    clients: Query<Entity, With<Client>>,
    mut commands: Commands,
    mut notice: ResMut<LobbyNotice>,
    mut next: ResMut<NextState<GameState>>,
) {
    if !suspend.want_resume {
        return;
    }
    if !clients.is_empty() {
        // `Disconnect` 事件已经发过了，这里只把还没收干净的实体清掉；
        // 它自己的 socket 在实体销毁时才释放，所以必须等这一帧结束
        for entity in &clients {
            commands.entity(entity).try_despawn();
        }
        crate::assetpack::log_line(&format!(
            "lifecycle: 等旧 client 落地，先清掉 {} 个残留",
            clients.iter().count()
        ));
        return;
    }
    let Some(server) = suspend.pending else {
        suspend.want_resume = false;
        return;
    };
    suspend.pending = None;
    suspend.want_resume = false;
    *phase = LoginPhase::Idle;
    *mode = SessionMode::Online { server };
    notice.0 = None;
    let current = state.get();
    crate::assetpack::log_line(&format!("lifecycle: 重连 {server}（当时状态 {current:?}）"));
    if matches!(current, GameState::Login) {
        // 握手被挂起打断了：先去大厅，再靠 `AutoJoin` 重新进登录 ——
        // 同一个状态没法靠 `NextState` 重跑 `OnEnter(Login)`
        auto.0 = Some(SessionMode::Online { server });
        next.set(GameState::Lobby);
    } else {
        next.set(GameState::Login);
    }
}

/// 远端实体复制下来后补上渲染：服务器只发状态，不发美术。
///
/// 同时把角色放进客户端物理世界：静态场景两端各自构建（见
/// [`crate::level::spawn_static_level`]），所以这里挂的碰撞体有真实
/// 几何可解 —— 射线、接触查询都靠它。
///
/// 刚体是**运动学**的：位移由服务器的 `NetPos` 直接写（见 `net_pose.rs`），
/// 物理只负责扫掠与查询，不反过来积分位置。本地玩家也是这一档，认领时不再换刚体类型。
fn spawn_remote_visual(
    trigger: On<Add<NetPos>>,
    mut commands: Commands,
    visuals: Res<RemoteVisuals>,
) {
    info!("net: remote avatar spawned {:?}", trigger.entity);
    commands.entity(trigger.entity).insert((
        Mesh3d(visuals.mesh.clone()),
        MeshMaterial3d(visuals.material.clone()),
        Visibility::Visible,
        // 与服务器同参数：半径 0.2、圆柱段 1.0
        Collider::capsule(0.2, 1.0),
        // Avian 0.8 的刚体只有 Dynamic / Static / Kinematic 三档：
        // Kinematic 不参与积分，组件由我们自己写，正好匹配"位置由服务器给"
        RigidBody::Kinematic,
        // 视觉插值：远端玩家和本地玩家一样，60Hz 快照直写会一格一格跳。
        // 远端要吸收过网抖动，缓冲比本地预测端更厚（见 `net_pose` 的两个滞后常量）
        PoseSmoothing::remote(),
    ));
}

fn on_connecting(trigger: On<Add<Connecting>>, mut phase: ResMut<LoginPhase>) {
    info!("net: connecting {:?}", trigger.entity);
    *phase = LoginPhase::Connecting;
}

/// 握手成功才发 `JoinRequest`：连接建立 ≠ 玩家入场。
fn send_join_request(
    trigger: On<Add<Connected>>,
    mut phase: ResMut<LoginPhase>,
    mut senders: Query<&mut MessageSender<JoinRequest>>,
) {
    info!("net: connected {:?} → 发 JoinRequest", trigger.entity);
    *phase = LoginPhase::Handshaking;
    for mut sender in senders.iter_mut() {
        sender.send::<ControlChannel>(JoinRequest {});
    }
}

fn on_disconnected(
    trigger: On<Add<Disconnected>>,
    mut phase: ResMut<LoginPhase>,
    mut commands: Commands,
    state: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    mut notice: ResMut<LobbyNotice>,
) {
    // 连接实体刚 spawn 出来时会先带一个 `Disconnected` 再转 `Connecting`，
    // 那种不算"掉线"：只有已经处于 Connecting/Handshaking/InGame 的断开才算
    crate::assetpack::log_line(&format!(
        "on_disconnected: entity={:?} phase={}",
        trigger.entity,
        match &*phase {
            LoginPhase::Idle => "Idle",
            LoginPhase::Connecting => "Connecting",
            LoginPhase::Handshaking => "Handshaking",
            LoginPhase::InGame(_) => "InGame",
        }
    ));
    let was_in_game = matches!(*phase, LoginPhase::InGame(_));
    let was_active = !matches!(*phase, LoginPhase::Idle) || was_in_game;
    *phase = LoginPhase::Idle;
    commands.remove_resource::<LocalPlayer>();

    if !was_active {
        debug!("net: disconnected {:?}（尚未开始连接）", trigger.entity);
        return;
    }

    warn!("net: disconnected {:?}，回到大厅", trigger.entity);
    crate::assetpack::log_line(&format!(
        "net: disconnected（was_in_game={was_in_game}）→ 回大厅"
    ));
    notice.0 = Some(if was_in_game {
        "与服务器断开连接".to_string()
    } else {
        "无法连接服务器，请检查地址或稍后重试".to_string()
    });
    if matches!(
        *state.get(),
        GameState::Login | GameState::Game | GameState::Pause
    ) {
        next.set(GameState::Lobby);
    }
    // 断开后的连接实体已经没用了，别让它留到下一次登录
    commands.entity(trigger.entity).try_despawn();
}

fn on_welcome(
    mut commands: Commands,
    mut phase: ResMut<LoginPhase>,
    mut clients: Query<&mut MessageReceiver<Welcome>>,
    mut next: ResMut<NextState<GameState>>,
) {
    for mut receiver in clients.iter_mut() {
        for welcome in receiver.receive() {
            info!("join: Welcome player_id={} → InGame", welcome.player_id);
            crate::assetpack::log_line(&format!("join: Welcome player_id={}", welcome.player_id));
            *phase = LoginPhase::InGame(welcome.player_id);
            commands.insert_resource(LocalPlayer(welcome.player_id));
            // 登录完成：场景/角色/相机在 `Game` 的 `OnEnter` 里建
            next.set(GameState::Game);
        }
    }
}

/// 认领「哪个复制下来的实体是我」：给它挂 `Avatar` 标记，相机就会跟它。
///
/// 刚体保持 `Kinematic`：位置与朝向都由 `NetPos` / `NetYaw` 直接写（`net_pose.rs`），
/// 客户端不做本地模拟。动画参数由 `drive_net_animation` 从 `NetMotion` 翻译，
/// 所以这里只补 `LocomotionIntent` / `FacingState` 两个组件，不挂朝向控制器。
fn claim_local_avatar(
    local: Option<Res<LocalPlayer>>,
    avatars: Query<(Entity, &NetPlayerId, Option<&Children>), Without<Avatar>>,
    clip_roots: Query<(), With<ClipModelRoot>>,
    mut commands: Commands,
    // 每认领一次都要重新上锚：上一局的 tick 时间轴跟这一局的服务器毫无关系
    mut clock: ResMut<PredClock>,
) {
    let Some(local) = local else {
        return;
    };
    for (entity, id, children) in avatars.iter() {
        if id.0 != local.0 {
            continue;
        }
        info!("net: claiming local avatar {:?}", entity);
        *clock = PredClock::default();
        // 本地角色交给动画系统：它自己会挂模型、跑混合空间与脚部 IK。
        // 之前先挂上的简易模型和占位胶囊都要撤掉，否则两套模型叠在一起
        if let Some(children) = children {
            for child in children.iter() {
                if clip_roots.contains(child) {
                    commands.entity(child).try_despawn();
                }
            }
        }
        commands
            .entity(entity)
            .insert((
                Avatar,
                // 动画驱动的输入。联网模式下角色的位移由服务器算，这三个组件
                // 没有被本地物理/输入写的来源，改由 `drive_net_animation` 从复制状态维护。
                // `PlanarMotion` 是混合空间 `yaw` 轴的实测位移来源：联机角色是
                // `Kinematic`、物理速度恒为 0，不补这一份转身时腿会继续朝正前方迈
                LocomotionIntent::default(),
                FacingState::default(),
                // 视觉插值的状态：认领前挂在远端路径上，这里换成**本地**的缓冲。
                // 这条时间轴由 `pred_record` 在定步长里直接喂，没有到达抖动，
                // 只需要夹住两份快照的一点余量（远端那 50 ms 的余量会白加在输入延迟上）
                PoseSmoothing::local(),
                // 本地预测核：自己按 60 Hz 推位移与朝向，快照只用来校正。
                // `PlanarMotion` 不再挂——预测端有真实 `LinearVelocity`，
                // 动画的 `yaw` 轴直接读实测位移，比服务端那份晚一个 RTT 的值更准。
                Prediction::default(),
                // 预测端要有真实物理：换成 Dynamic（服务端复制下来的默认是 Kinematic），
                // 阻尼/锁定轴与单机角色同参数，手感才不会两边不一样。
                RigidBody::Dynamic,
                SleepingDisabled,
                LinearDamping(0.3),
                AngularDamping(0.3),
                LockedAxes::ROTATION_LOCKED,
                LinearVelocity::ZERO,
            ))
            .remove::<ClipBody>()
            .remove::<Mesh3d>()
            .remove::<MeshMaterial3d<StandardMaterial>>();
    }
}

/// 远端角色模型相对挂点的偏移，与动画系统里的 `MODEL_BASE` 保持一致。
const MODEL_BASE: Vec3 = Vec3::new(0.0, -0.7, 0.0);

/// 客户端为所有玩家实例化模型要用的资产。
///
/// 单体例动画管线（`AnimRuntime`）只管本地角色，其他玩家按这里的固定候选各挂一份模型。
/// 剪辑不在这里存：远端姿态由动画系统的 [`ClipSet`]（当前 rig 的剪辑表）求值，
/// 只要远端 rig 和它是同一个角色，动画 target 就一一对得上。
#[derive(Resource)]
struct PlayerModelAssets {
    rig: Handle<WorldAsset>,
    /// 这个 rig 属于哪个角色（用来核对 `ClipSet` 是不是同一套骨架）
    character: String,
}

/// 「这个玩家实体已经挂过模型」的标记。
#[derive(Component)]
struct ClipBody;
/// 模型实例根（`WorldAssetRoot` 持有者），认领本地角色时要能删掉它。
#[derive(Component)]
struct ClipModelRoot;
/// 模型实例挂给哪个玩家实体（模型是玩家的子节点，但反向查更省事）。
#[derive(Component)]
struct ModelOwner(Entity);
/// 模型场景已载入、动画图已建好。
#[derive(Component)]
struct ClipReady;
/// 远端角色的混合空间骨架实例。
///
/// 本地角色走资源版的 [`crate::anim::PoseDriver`]（全工程只有一份），远端每个
/// 模型各持一份骨架映射与相位，用同一套 `evaluate_space` 求值：
/// 方向差给 yaw、实测速度给 speed，侧移/倒退/转身才会真的混出对应剪辑，
/// 而不是一直播正前方的走跑。
#[derive(Component, Default)]
struct RemotePose {
    skeleton: Option<Skeleton>,
    /// 跨帧保留的归一化相位（每具骨架各走各的）
    phase: f32,
    /// 本帧的 `yaw` / `speed` 参数
    params: std::collections::HashMap<String, f32>,
    /// 平滑后的主动度（0 = 纯被动位移，1 = 自己在走）。单 tick 的输入抖动
    /// 或丢包不该让姿态在一帧里从跑到站，所以按 [`ACTIVE_TAU`] 低通
    active: f32,
    /// 上一帧混合输出的等效地速（米/秒，1× 播放时这具姿态自己走多快）。
    /// 用它算播放速率：脚底步幅和服务器给的实测速度对不上就会滑步，
    /// 而这一帧的等效地速要等求值完才知道，只能拿上一帧的（差一帧，看不出来）
    natural_speed: f32,
}

/// 远端主动度的过渡时间（秒）。和本地 `PARAM_SLEW` 的 20/秒 差不多量级，
/// 既能吃掉单帧抖动，松手时也不会看出「瞬间定住」
const ACTIVE_TAU: f32 = 0.05;

/// 角色清单扫完后准备远端模型资产（只做一次）。
fn prepare_player_models(
    mut commands: Commands,
    catalog: Res<AnimCatalog>,
    asset_server: Res<AssetServer>,
    existing: Option<Res<PlayerModelAssets>>,
) {
    if existing.is_some() {
        return;
    }
    let Some(def) = catalog.character(0) else {
        return;
    };
    if def.clips.is_empty() {
        return;
    }
    let rig: Handle<WorldAsset> = asset_server.load(format!("{}#Scene0", def.rig));
    info!(
        "net: 远端角色资产就绪 {}（{} 条剪辑，姿态由混合空间求值）",
        def.name,
        def.clips.len()
    );
    commands.insert_resource(PlayerModelAssets {
        rig,
        character: def.name.clone(),
    });
}

/// 给每个（非本地的）玩家实体挂一份模型实例。
///
/// 胶囊在这时保留，等模型真的载入完（`setup_clip_models`）再撤，避免出现"人不见了"的一帧。
fn attach_player_models(
    mut commands: Commands,
    assets: Option<Res<PlayerModelAssets>>,
    players: Query<(Entity, Has<Avatar>, Has<ClipBody>), With<NetPlayerId>>,
) {
    let Some(assets) = assets else {
        return;
    };
    for (player, is_local, has_body) in players.iter() {
        if is_local || has_body {
            continue;
        }
        commands.spawn((
            WorldAssetRoot(assets.rig.clone()),
            Transform::from_translation(MODEL_BASE),
            ChildOf(player),
            ClipModelRoot,
            ModelOwner(player),
        ));
        commands.entity(player).insert(ClipBody);
        info!("net: 玩家 {:?} 模型实例已创建（等场景载入）", player);
    }
}

/// 模型场景载入后挂上混合空间骨架，并把占位胶囊撤掉。
fn setup_clip_models(
    mut commands: Commands,
    roots: Query<(Entity, &ModelOwner), (With<ClipModelRoot>, Without<ClipReady>)>,
    children: Query<&Children>,
    names: Query<&Name>,
    targets: Query<(), With<AnimationTargetId>>,
    players: Query<Entity, With<AnimationPlayer>>,
    transforms: Query<&Transform>,
) {
    for (root, owner) in roots.iter() {
        // 场景是异步实例化的，AnimationPlayer 可能这一帧还没出现，下一帧再试
        if find_descendant(root, &children, |entity| players.contains(entity)).is_none() {
            continue;
        }
        // 远端不再用 AnimationPlayer 播单条剪辑：动画交给混合空间求值（见
        // `drive_remote_pose`），这里只把骨架实例挂上，等模型骨骼就绪后建映射。
        commands.entity(root).insert((
            RemotePose {
                skeleton: None,
                phase: 0.0,
                params: std::collections::HashMap::new(),
                active: 0.0,
                natural_speed: 0.0,
            },
            // 远端也要播跳跃：没有输入可猜，只能观察复制下来的轨迹（见 `RemoteJump`）
            RemoteJump::default(),
            ClipReady,
            // 远端也要脚部 IK：`attach_rigs` 只认 `AnimModelRoot`，这具模型根没有那个标记
            crate::anim::FootIkTarget,
        ));
        // 根位移锁：把所有名叫 Hips 的骨骼节点都挂上水平锁定
        //
        // 不能只锁一个：这套 rig 里 `Ctrl_Hips` 是控制曲线、`mixamorig:Hips` 才是
        // 真正被蒙皮的那根髋骨，剪辑的根位移是烘在后者上的。
        // 基准取绑定姿态的局部位移（各剪辑的髋原地位置一致），换剪辑不用重取。
        let bones: Vec<BoneLock> = find_all_descendants(root, &children, |entity| {
            targets.contains(entity)
                && names
                    .get(entity)
                    .map(|name| name.as_str().contains("Hips"))
                    .unwrap_or(false)
        })
        .into_iter()
        .filter_map(|entity| {
            let base = transforms.get(entity).ok()?.translation.to_array();
            Some(BoneLock { entity, base })
        })
        .collect();
        if bones.is_empty() {
            warn!(
                "net: 玩家 {:?} 模型里没找到 Hips，远端会有根位移漂移",
                owner.0
            );
        } else {
            commands.entity(owner.0).insert(RemoteHips { bones });
        }
        // 模型到位，撤掉占位胶囊
        commands
            .entity(owner.0)
            .remove::<Mesh3d>()
            .remove::<MeshMaterial3d<StandardMaterial>>();
        info!(
            "net: 玩家 {:?} 模型载入完成，骨骼就绪后由混合空间驱动",
            owner.0
        );
    }
}

/// 远端模型里 Hips 的水平锁定状态。
///
/// 这些剪辑本身带根位移：原地循环播放时髋骨会一点点往前跑，跑到剪辑末尾再跳回起点。
/// 本地角色有 `anim::character::apply_root_motion` 把髋的水平分量锁回原点，
/// 远端角色只有一具朴素 `AnimationPlayer`，没人锁——所以看到的就是
/// 「人跑得比它真实位置快/偏、停下才回位」。这里补同一件事：每帧把髋的水平分量
/// 锁回绑定姿态，只留竖直起伏。
#[derive(Component)]
struct RemoteHips {
    bones: Vec<BoneLock>,
}

struct BoneLock {
    entity: Entity,
    /// 绑定姿态的髋部局部位移（各剪辑的髋原地位置一致，换剪辑不用重取）
    base: [f32; 3],
}

fn lock_remote_hips(models: Query<&RemoteHips>, mut transforms: Query<&mut Transform>) {
    for rig in models.iter() {
        for bone in &rig.bones {
            let Ok(mut transform) = transforms.get_mut(bone.entity) else {
                continue;
            };
            transform.translation.x = bone.base[0];
            transform.translation.z = bone.base[2];
        }
    }
}

/// 广度优先收集所有满足条件的后代。
fn find_all_descendants(
    root: Entity,
    children: &Query<&Children>,
    matches: impl Fn(Entity) -> bool,
) -> Vec<Entity> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        if entity != root && matches(entity) {
            found.push(entity);
        }
        if let Ok(list) = children.get(entity) {
            stack.extend(list.iter());
        }
    }
    found
}

/// 广度优先找第一个满足条件的后代（骨骼层次可能比场景本体晚一点出现）。
fn find_descendant(
    root: Entity,
    children: &Query<&Children>,
    matches: impl Fn(Entity) -> bool,
) -> Option<Entity> {
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        if entity != root && matches(entity) {
            return Some(entity);
        }
        if let Ok(list) = children.get(entity) {
            stack.extend(list.iter());
        }
    }
    None
}

/// 远端角色也用混合空间驱动姿态：方向差 → `yaw`，实测速度 → `speed`。
///
/// 每具骨架各有一份映射与相位，求值函数和本地角色完全相同，所以「斜着走」「倒退」
/// 「转身时先侧步再正走」这些本地看得见的效果，远端也会有。
#[allow(clippy::too_many_arguments)]
/// `drive_remote_pose` 的参数已经顶到 Bevy 的单系统参数上限，把「档位表 + 诊断去重标记」
/// 裹成一个参数：跳跃档位跟着 `JumpTunings` 走，远端与本地用同一张表。
#[derive(SystemParam)]
struct RemotePoseJump<'w, 's> {
    tunings: Res<'w, JumpTunings>,
    mismatched: Local<'s, bool>,
    /// 服务端复制的跳跃阶段（`NetJump`）：有它就不再用位置差观察
    jumps: Query<'w, 's, &'static NetJump>,
}

/// 远端姿态系统的前置条件。
///
/// 全新安装（或热更包还没下完）时 `AnimCatalog` 是空的，`PlayerModelAssets`
/// 根本不会被 `prepare_player_models` 插入，它们和 `MixerCache`/`ClipSet`/
/// `AnimSelection` 一起缺席。少了这道判断，第一帧的 `Res<T>` 校验就会 panic 并
/// abort 整个进程 —— 表现就是「装上后一点就闪退」。
fn remote_pose_ready(
    models: Option<Res<PlayerModelAssets>>,
    cache: Option<Res<MixerCache>>,
    clips: Option<Res<ClipSet>>,
    catalog: Option<Res<AnimCatalog>>,
    selection: Option<Res<AnimSelection>>,
) -> bool {
    models.is_some()
        && cache.is_some()
        && clips.is_some()
        && catalog.is_some()
        && selection.is_some()
}

fn drive_remote_pose(
    time: Res<Time>,
    assets: Res<PlayerModelAssets>,
    cache: Res<MixerCache>,
    clips: Res<ClipSet>,
    catalog: Res<AnimCatalog>,
    selection: Res<AnimSelection>,
    clip_assets: Res<Assets<AnimationClip>>,
    mut rigs: Query<(Entity, &mut RemotePose, &mut RemoteJump, &ModelOwner)>,
    mut foot_iks: Query<&mut FootIk>,
    motions: Query<(&NetMotion, &NetYaw)>,
    spatial: SpatialQuery,
    mut jump_cfg: RemotePoseJump,
    children: Query<&Children>,
    targets: Query<(&AnimationTargetId, Option<&AnimatedBy>)>,
    names: Query<&Name>,
    mut transforms: Query<&mut Transform>,
) {
    // 混合空间定义与本地共用同一份「移动混合」文档；没有就什么都不做
    let Some(space) = cache
        .mixers
        .iter()
        .find(|mixer| mixer.key == "locomotion")
        .map(|mixer| mixer.space.clone())
    else {
        return;
    };
    // 剪辑与骨骼的 target 必须来自同一个角色，否则求出来的姿态对不上这具骨架
    if clips.character() != Some(assets.character.as_str()) {
        if !*jump_cfg.mismatched {
            warn!(
                "net: 当前剪辑表属于 {:?}，远端模型是 {}，远端姿态先不驱动",
                clips.character(),
                assets.character
            );
            *jump_cfg.mismatched = true;
        }
        return;
    }
    *jump_cfg.mismatched = false;
    let axes = space.params.clone();
    // 混合空间用的 dt 截顶（防掉帧时相位一次跳太远）；跳跃层要用真实帧间距
    let raw_dt = time.delta_secs();
    let dt = raw_dt.min(0.05);

    for (root, mut rig, mut jump, owner) in rigs.iter_mut() {
        let RemotePose {
            skeleton,
            phase,
            params,
            active,
            natural_speed,
        } = &mut *rig;
        // 模型是异步实例化的，骨骼要到场景载入后才在
        if skeleton.is_none() {
            *skeleton = build_skeleton(
                owner.0,
                &children,
                &targets,
                &names,
                &mut transforms,
                &clips,
                &clip_assets,
            );
        }
        let Some(rig_skeleton) = skeleton.as_ref() else {
            continue;
        };

        let phase_now = *phase;
        // 跳跃层要用的实测速度与方向（远端没有输入，只能拿这两个量去挑剪辑）
        let mut jump_speed = 0.0f32;
        let mut jump_dir = Vec3::ZERO;

        // 参数：方向差取「意图方向 − 当前脸向」，速度量级取实测
        let [yaw, speed] = match motions.get(owner.0) {
            Ok((motion, facing)) => {
                let planar = Vec2::new(motion.dir[0], motion.dir[1]);
                // 速度档位 = 实测速度 × 平滑主动度。`intent == 0` 表示这一帧的
                // 位移跟自己无关（被人推、被平台拖），这时档位收敛到 0 → 混合空间
                // 走 speed=0 那一行（整行都是 Idle），人就不会边被搬走边跑步
                let want = if motion.intent > 1e-3 { 1.0 } else { 0.0 };
                *active += (want - *active) * (dt / ACTIVE_TAU).min(1.0);
                let measured = (motion.speed / MOVE_SPEED_MPS).clamp(0.0, 1.0) * *active;
                jump_speed = motion.speed;
                jump_dir = Vec3::new(motion.dir[0], 0.0, motion.dir[1]);
                if planar.length_squared() <= 1e-6 {
                    [0.0, measured]
                } else {
                    // 单位统一用弧度算完再转角度：`atan2` 与 `NetYaw` 都是弧度
                    let target = planar.x.atan2(planar.y);
                    let error = wrap_pi(target - facing.0).to_degrees();
                    // 混合空间的 yaw 右正左负，`FacingState.error_deg` 左转为正，取反
                    [-error, measured]
                }
            }
            Err(_) => [0.0, 0.0],
        };
        for (index, axis) in axes.iter().enumerate() {
            params.insert(axis.clone(), [yaw, speed][index.min(1)]);
        }

        // 播放速率：让「这具姿态自己走多远」和「服务器说它走了多远」对上。
        // 混合输出的等效地速 `ground_speed` 是 1× 播放时的米/秒，所以
        // `实测 / 等效` 就是该乘的倍率；站着（实测≈0）或纯 Idle 时没得对，保持 1×。
        // 参考的是上一帧的等效地速：这一帧的要等求值完才有，差一帧看不出来
        let measured_mps = speed * MOVE_SPEED_MPS;
        let natural = *natural_speed;
        let speed_scale = if measured_mps > 0.05 && natural > 0.05 {
            (measured_mps / natural).clamp(0.35, 3.0)
        } else {
            1.0
        };

        // 远端跳跃层：不猜按键，只看这具身体什么时候离地、什么时候触地。
        // 复制位置是阶跃到达的，用**真实帧间距**差分竖直速度（不能用混合空间那个
        // 截到 50ms 的 dt，否则低帧时速度被系统性放大）
        let player_transform = transforms.get(owner.0).ok().map(|t| *t);
        let jump_height = player_transform.and_then(|t| {
            let filter = SpatialQueryFilter::from_excluded_entities([owner.0]);
            foot_ground_dist(&spatial, &filter, &t)
        });
        let hips_id = rig_skeleton.hips;
        let gait = catalog
            .character(selection.character)
            .and_then(|def| def.gaits.get("Running"));
        let filter = SpatialQueryFilter::from_excluded_entities([owner.0]);
        let setup = |variant| {
            let transform = player_transform?;
            let hips = hips_id?;
            remote_jump_setup(
                &spatial,
                &filter,
                &transform,
                jump_speed,
                jump_dir,
                Some(phase_now),
                gait,
                variant,
                hips,
                &clips,
                &clip_assets,
                &jump_cfg.tunings,
            )
        };
        // 有复制的跳跃状态就按服务端阶段排剪辑（起跳/触地时刻权威）；
        // 老版本服务端没这个组件时退回观察式，至少不会不演
        if let Ok(net_jump) = jump_cfg.jumps.get(owner.0) {
            let net_jump = *net_jump;
            jump.drive_net(raw_dt, &net_jump, setup);
        } else {
            jump.observe(
                raw_dt,
                player_transform.map(|t| t.translation.y).unwrap_or(0.0),
                jump_height,
                setup,
            );
        }

        let mut context = SpaceEvalContext {
            params,
            clips: &clips,
            clip_assets: &clip_assets,
            targets: &rig_skeleton.targets,
            rest: &rig_skeleton.rest,
            phase,
            dt,
            speed_scale,
            hips: rig_skeleton.hips,
        };
        let mut output = evaluate_space(&space, &mut context);

        // 起跳 / 滞空 / 落地期间按权重叠上跳跃剪辑。髋在滞空段钉在「剪辑离地帧的髋高」
        // 上并按权重与走路姿态插值：物理那边抬根，动画这边不能再抬，否则人会浮起来
        let jump_weight = jump.weight();
        let base_hips_y = rig_skeleton
            .hips
            .and_then(|h| output.pose.get(h))
            .map(|bone| bone.translation.y);
        if jump_weight > 0.0 {
            blend_clip_pose(
                jump.clip(),
                jump.clip_time(),
                jump_weight,
                &clips,
                &clip_assets,
                &rig_skeleton.targets,
                &rig_skeleton.rest,
                &mut output.pose,
            );
        }
        if jump.is_airborne()
            && let (Some(hips), Some(base)) = (rig_skeleton.hips, base_hips_y)
        {
            output
                .pose
                .set_translation_y(hips, base.lerp(jump.takeoff_hips(), jump_weight));
        }

        for (id, bone) in output.pose.iter() {
            if let Some(entity) = rig_skeleton.by_id.get(id)
                && let Ok(mut transform) = transforms.get_mut(*entity)
            {
                bone.apply_to(&mut transform);
            }
        }
        *natural_speed = output.ground_speed;

        // 脚部 IK 的输入：支撑权重直接取混合输出（和本地同一份语义），
        // 「静止」用实测速度判 —— 本地那边读的是混合输出的地速，等价
        if let Ok(mut ik) = foot_iks.get_mut(root) {
            ik.contacts = output.contacts;
            // 滞空时不能算「静止」：落地淡出的最后几帧会被拿去标定脚掌俯角
            ik.resting = measured_mps < 0.05 && speed.abs() < 0.02 && jump_weight == 0.0;
        }
    }
}

/// 调试用：`--auto-walk <秒>` 时自动把摇杆往前推，跑联调脚本用。
#[derive(Resource, Debug)]
pub struct AutoWalk(pub Timer);

/// 调试用：`--auto-jump <周期秒>` 时自动点按跳跃（先按住一段再松开，产生上升沿）。
#[derive(Resource, Debug)]
pub struct AutoJump {
    /// 两次起跳的间隔（秒）
    period: f32,
    /// 距下次起跳剩余（秒）
    timer: f32,
    /// 当前是否处于按住段
    held: bool,
    /// 已按住秒数
    hold: f32,
}

impl AutoJump {
    pub fn new(period: f32) -> Self {
        Self {
            period,
            timer: 0.6,
            held: false,
            hold: 0.0,
        }
    }
}

/// 自动跳：把 [`JumpInput`] 在 Activated / Idle 间翻转，模拟真实点按的上升沿。
fn auto_jump_input(time: Res<Time>, auto: Option<ResMut<AutoJump>>, mut jump: ResMut<JumpInput>) {
    let Some(mut auto) = auto else {
        return;
    };
    let dt = time.delta_secs();
    if auto.held {
        auto.hold += dt;
        if auto.hold > 0.5 {
            auto.held = false;
            auto.timer = auto.period;
        }
    } else {
        auto.timer -= dt;
        if auto.timer <= 0.0 {
            auto.held = true;
            auto.hold = 0.0;
        }
    }
    *jump = if auto.held {
        JumpInput::Activated
    } else {
        JumpInput::Idle
    };
}

/// 每帧上传本帧输入。
fn send_input(
    mut senders: Query<&mut MessageSender<MoveInput>, With<Client>>,
    mut last_level: Local<bool>,
    input: Res<MovementInput>,
    jump: Res<JumpInput>,
    facing: Res<CameraFacing>,
    time: Res<Time>,
    auto_walk: Option<ResMut<AutoWalk>>,
    action: Option<Res<JumpAction>>,
    jump_settings: Option<Res<JumpSettings>>,
    movement_settings: Option<Res<crate::input::MovementSettings>>,
    // 生效的摇杆输入（含「扫过摇杆中心」的保持）。
    // 必须发它而不是原始摇杆：拇指从一边扫到另一边时那几帧原始值 ≈ 0，
    // 服务端会当成没输入、把人当场停住，客户端却还在跑，两端立刻错开。
    effective_move: Option<Res<crate::input::EffectiveMove>>,
    // 本地预测的时间轴：发出去的输入要带「它该在哪个 tick 生效」
    clock: Option<Res<PredClock>>,
) {
    let mut auto_walk = auto_walk;
    if let Some(walk) = auto_walk.as_mut()
        && !walk.0.is_finished()
    {
        walk.0.tick(time.delta());
    }
    let auto = auto_walk
        .as_ref()
        .map(|walk| !walk.0.is_finished())
        .unwrap_or(false);

    let (local, force) = if auto {
        // 摇杆 y 轴与键盘同约定：W/上推 = -Vec2::Y（前向，朝相机视线方向）。
        // 与本地 `auto_walk_diag` 注入的方向必须一致，否则上行把人往后推、
        // 本地预测往前跑，两端位置反向分叉，和解系统只能反复硬吸附往回拽。
        (Vec2::new(0.0, -1.0), 1.0)
    } else {
        match effective_move.as_deref() {
            Some(effective) if effective.is_active() => (effective.local_stick(), effective.force),
            Some(_) => (Vec2::ZERO, 0.0),
            None => match *input {
                MovementInput::Idle => (Vec2::ZERO, 0.0),
                MovementInput::Activated { direction, force } => (direction, force),
            },
        }
    };
    let jump_pressed = matches!(*jump, JumpInput::Activated);
    // 上行电平分三种：
    // - 等窗口（Queued）：先压住电平，窗口打开、动作层真正起跳那一帧再发上升沿；
    //   服务端就在接地状态下开始蓄力，不会在跨步相把角色蹬离地面。
    // - 动作执行中：电平保持为真。玩家早松手（点按起跳）也不会漏掉这一跳，
    //   落地回到地面后电平才回落，不会误触发第二跳。
    // - 地面：跟随按键，和单机的边沿锁存一致。
    // 关键：**相位窗口只推迟上升沿，绝不吞掉请求**——等窗口超时后动作层照样起跳，
    // 这里照样把上升沿发出去。
    let jump = action
        .as_ref()
        .map(|action| uplink_jump_level(action.state, action.is_queued(), jump_pressed))
        .unwrap_or(jump_pressed);
    // 现场排查用：`CRAB_LOG_JUMP=1` 看上行电平什么时候翻起来、当时动作层在哪个状态。
    if std::env::var("CRAB_LOG_JUMP").is_ok() && jump != *last_level {
        let state = action
            .as_ref()
            .map(|action| format!("{:?}", action.state))
            .unwrap_or_else(|| "-".to_string());
        info!(
            "uplink jump level -> {} state={} pressed={}",
            jump, state, jump_pressed
        );
        *last_level = jump;
    }

    // 摇杆是相对相机的：往上推 = 屏幕深处 = 视线水平朝向
    let forward = facing.view_forward();
    let right = Vec3::new(-forward.z, 0.0, forward.x);
    let world = right * local.x + forward * (-local.y);
    if std::env::var("CRAB_LOG_YAW").is_ok() {
        info!("uplink: auto={} local=({:.2},{:.2}) world=({:.2},{:.2}) fwd=({:.2},{:.2})", auto, local.x, local.y, world.x, world.z, forward.x, forward.z);
    }
    // 没有预测核 / 还没上锚时发 0，服务端退回「到达即生效」。
    // 上锚之后发的是**下一个要跑的 tick**：服务端把这条排进未来的那一 tick，
    // 两端逐 tick 同源，不会因为「谁先收到包」整体错开一段相位。
    let tick = match clock.as_deref() {
        Some(clock) if clock.anchored => clock.tick,
        _ => 0,
    };
    let message = MoveInput {
        tick,
        dir: [world.x, world.z],
        force,
        jump,
        // 起跳大类跟着上行一起发：服务端据此取**同一档**的蓄力时长与离地速度。
        // 动作层在地面时保留上一跳的 kind，但上升沿只会在 `begin` 那一帧出现，
        // 那一帧的 kind 已经是新的一跳。
        jump_kind: action
            .as_ref()
            .map(|action| action.kind.code())
            .unwrap_or(0),
        jump_seq: action.as_ref().map(|action| action.jump_seq()).unwrap_or(0),
        // 跳跃手感也随每条快照上行：服务端拿同一组数推人，空中转多快、跳多高
        // 两端必须同源。玩家在对局中拖滑杆，下一条快照就带过去，不必重连。
        //
        // 缺席时发 0：服务端把 0 当作「用我自己的默认值」，老客户端不会把服务端
        // 的起跳速度当成 0 而跳不起来。
        jump_vy: jump_settings.as_ref().map_or(0.0, |s| s.launch_vy),
        air_accel: jump_settings.as_ref().map_or(0.0, |s| s.air_accel),
        air_steer_secs: jump_settings.as_ref().map_or(0.0, |s| s.air_steer_secs),
        // 转身降速的两个旋钮同理：服务端按同一组数算地速，预测端才不会被拽
        turn_brake_start_deg: movement_settings
            .as_ref()
            .map_or(0.0, |s| s.turn_brake_start_deg),
        turn_brake_min_scale: movement_settings
            .as_ref()
            .map_or(0.0, |s| s.turn_brake_min_scale),
    };
    for mut sender in senders.iter_mut() {
        sender.send::<ControlChannel>(message);
    }
}

// ==== 世界时间速度（调试面板滑杆）的联网部分 ====
//
// 权威在服务端：客户端拖动只发一条请求，服务端夹好区间、写 `NetTimeScale`（复制给
// 所有端）、同时改它自己的 `Time<Virtual>`。客户端收到复制值后写回本地
// `DebugTimeScale`，再由 `ui::debug::apply_time_scale` 落到本机时间系统。
// 单机模式没有这些系统（也没开 `net` feature），滑杆直接生效。

/// 拖动滑杆时的上行节流（秒，真实时间）。
///
/// 拖一次会产生几十条 `ValueChange`，每条都发包会把可靠控制通道塞满，而服务端只认
/// 最后一条。0.1s 一条在拖动时是 10 次/秒，滑杆看着跟手，也不至于让服务端每帧
/// 改一次时间比例。
const TIME_SCALE_SEND_INTERVAL: f32 = 0.1;

/// 自己刚改过速度之后，这段时间内不采纳服务器的复制值（秒，真实时间）。
///
/// 上行到回执有几十毫秒的来回，拖动中如果每条回执都照单全收，滑块会被拽回上一个
/// 位置闪一下。留一个短窗口，松手后最终值照样会落下来（含服务端夹取后的值）。
const TIME_SCALE_ECHO_GRACE: f32 = 0.25;

/// 世界时间速度的上行状态（只在客户端用）。
#[derive(Resource, Default, Debug)]
struct TimeScaleUplink {
    /// 还没发出去的最新值
    pending: Option<f32>,
    /// 距离上次发包过了多久（真实秒）
    since_send: f32,
    /// 还需要忽略服务端回执多久（真实秒）
    grace: f32,
}

/// 把「用户拖了滑杆」翻译成节流后的上行请求。
fn send_time_scale_request(
    time: Res<Time<Real>>,
    mut requests: MessageReader<TimeScaleChanged>,
    mut uplink: ResMut<TimeScaleUplink>,
    mut senders: Query<&mut MessageSender<TimeScaleRequest>, With<Client>>,
) {
    for request in requests.read() {
        uplink.pending = Some(request.speed);
        // 本地一动手就进「回执静默窗」，而不是等第一条请求真正发出去才进：
        // 拖动的最初 0.1s 里还没有请求上行，服务端上一版的值会顺着复制回来，
        // 把滑杆和 `Time<Virtual>` 一起拽回旧值（实测 ×1.00 亮一下再回到 ×0.20）
        uplink.grace = TIME_SCALE_ECHO_GRACE;
    }
    uplink.since_send += time.delta_secs();
    let Some(value) = uplink.pending else {
        return;
    };
    if uplink.since_send < TIME_SCALE_SEND_INTERVAL {
        return;
    }
    let mut sent = false;
    for mut sender in senders.iter_mut() {
        sender.send::<ControlChannel>(TimeScaleRequest { value });
        sent = true;
    }
    if sent {
        uplink.pending = None;
        uplink.since_send = 0.0;
        uplink.grace = TIME_SCALE_ECHO_GRACE;
    }
}

/// 把「调试倍数」和「lightyear 的时钟同步基准」合成为本帧的虚拟时间速度。
///
/// `LocalTimelineSyncPlugin::update_virtual_time` 跑在 `Last`，每帧**绝对**写一次
/// `Time<Virtual>`：同步成功时写 `sync.relative_speed()`，否则写 `1.0`。所以
/// `ui::debug::apply_time_scale` 在 `Update` 写的 0.2 到下一帧就被擦回 1.0，
/// 帧 delta 依旧是实时——这就是「服务端慢了、客户端没慢」的原因。
///
/// 这里不读上一帧的 `relative_speed()`（否则会和自己的乘积自我累积），只读同步基准，
/// 所以每帧结果都是 `基准 × 倍数`，稳定不漂。
fn apply_effective_time_scale(
    sync: Option<Res<LocalTimelineSync>>,
    scale: Res<DebugTimeScale>,
    mut time: ResMut<Time<Virtual>>,
) {
    let base = sync
        .filter(|sync| sync.is_synced())
        .map(|sync| sync.relative_speed())
        // 预测窗口把速度压到 0 时别把它抬起来
        .filter(|speed| *speed > 0.0)
        .unwrap_or(1.0);
    let speed = base * crab_feast_library::world_time::clamp_scale(scale.speed);
    if (time.relative_speed() - speed).abs() > 1e-4 {
        time.set_relative_speed(speed);
    }
}

/// 把服务端复制下来的世界时间速度写回本地资源。
///
/// 不直接写 `Time<Virtual>`：写资源就行，`ui::debug::apply_time_scale` 每帧对齐，
/// 这样「单机滑杆 / 服务器复制值」两条来源共用同一段落地的代码，不会出现
/// "客户端时间改了但滑杆还停在旧位置"。
fn apply_replicated_time_scale(
    time: Res<Time<Real>>,
    mut uplink: ResMut<TimeScaleUplink>,
    replicas: Query<&NetTimeScale>,
    mut scale: ResMut<DebugTimeScale>,
) {
    uplink.grace = (uplink.grace - time.delta_secs()).max(0.0);
    let Some(replicated) = replicas.iter().next() else {
        return;
    };
    // 自己刚动过：这条回执多半是自己那个旧值，别把滑杆拽回去
    if uplink.grace > 0.0 {
        if (scale.speed - replicated.0).abs() > 1e-4 {
            debug!("net: 忽略回执 ×{:.2}（自己刚改过）", replicated.0);
        }
        return;
    }
    let value = replicated.0;
    if (scale.speed - value).abs() > 1e-4 {
        info!("net: 世界时间速度同步为 ×{value:.2}");
        scale.speed = value;
    }
}

#[cfg(test)]
mod time_scale_tests {
    use super::*;

    #[test]
    fn local_bind_matches_server_address_family() {
        let v4: SocketAddr = "127.0.0.1:5099".parse().unwrap();
        let v6: SocketAddr = "[2409:8a3c:424:5ff0:1cda:21aa:9ba1:d1cd]:5099"
            .parse()
            .unwrap();
        assert!(local_bind_addr(v4).is_ipv4());
        assert!(local_bind_addr(v6).is_ipv6());
        assert_eq!(local_bind_addr(v4).port(), 0);
        assert_eq!(local_bind_addr(v6).port(), 0);
    }

    /// 客户端收到复制值后必须真的把 `Time<Virtual>` 改掉，不然「滑杆同步了但世界没变」
    #[test]
    fn replicated_value_reaches_virtual_time() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<DebugTimeScale>();
        app.init_resource::<TimeScaleUplink>();
        app.add_systems(
            Update,
            (
                apply_replicated_time_scale,
                crate::ui::debug::apply_time_scale,
            ),
        );
        app.world_mut().spawn(NetTimeScale(0.2));
        // 资源与「资源→虚拟时间」两段在不同插件里，帧内顺序不保证：最多慢一帧
        app.update();
        app.update();
        let speed = app.world().resource::<Time<Virtual>>().relative_speed();
        assert!(
            (speed - 0.2).abs() < 1e-4,
            "复制值应落到虚拟时间，实际 {speed}"
        );
    }

    /// lightyear 的客户端同步在 `Last` 每帧把虚拟时间速度绝对写回 1.0：
    /// 调试倍数必须压得住它，否则会出现「资源 ×0.20、世界还是 1×」
    #[test]
    fn last_schedule_overwrite_does_not_undo_the_scale() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<DebugTimeScale>();
        app.world_mut().resource_mut::<DebugTimeScale>().speed = 0.2;
        // 模拟 `LocalTimelineSyncPlugin::update_virtual_time` 的绝对覆盖
        app.add_systems(Last, |mut time: ResMut<Time<Virtual>>| {
            time.set_relative_speed(1.0);
        });
        app.add_systems(
            First,
            apply_effective_time_scale.before(bevy::time::TimeSystems),
        );
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            core::time::Duration::from_millis(16),
        ));
        app.update();
        app.update();
        let delta = app.world().resource::<Time<Virtual>>().delta_secs();
        assert!(
            (delta - 0.0032).abs() < 5e-4,
            "虚拟时间 delta 应该约 3.2ms（16ms × 0.2），实际 {delta}"
        );
    }

    /// 刚拖动、请求还没发出去的那几帧，服务端上一版的复制值不能把本地拽回去
    #[test]
    fn local_change_is_not_pulled_back_by_stale_echo() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<DebugTimeScale>();
        app.init_resource::<TimeScaleUplink>();
        app.add_message::<TimeScaleChanged>();
        app.add_systems(
            Update,
            (
                send_time_scale_request,
                apply_replicated_time_scale,
                crate::ui::debug::apply_time_scale,
            )
                .chain(),
        );
        app.world_mut().spawn(NetTimeScale(1.0));
        // 拖动（观察者）把资源写成 ×0.20，同时发出消息
        app.world_mut().resource_mut::<DebugTimeScale>().speed = 0.2;
        app.world_mut()
            .write_message(TimeScaleChanged { speed: 0.2 });
        app.update();
        app.update();
        let scale = app.world().resource::<DebugTimeScale>().speed;
        let speed = app.world().resource::<Time<Virtual>>().relative_speed();
        assert!(
            (scale - 0.2).abs() < 1e-4,
            "本地拖动值被回执拽回去了：{scale}"
        );
        assert!((speed - 0.2).abs() < 1e-4, "虚拟时间没跟上拖动：{speed}");
    }
}
