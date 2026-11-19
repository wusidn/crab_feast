//! 客户端联网接入（v1：权威服务器 + 状态复制，不含预测）。
//!
//! 登录流程：
//! 1. `connect` 在进场景时建连接实体（netcode 握手）；
//! 2. 握手成功（`Connected`）才发 `JoinRequest`——**连接 ≠ 玩家**；
//! 3. 服务端收到 Join 才分配 `player_id`、生成角色实体、回 `Welcome`；
//! 4. 客户端收到 `Welcome` → [`LocalPlayer`]，复制下来的那个实体被认领成 `Avatar`。
//!
//! 渲染取舍：
//! - 角色实体一律由服务端复制，客户端不本地生成，避免"两个人在同一个角色上写 Transform"；
//! - 本地角色挂 `Avatar`，交给动画系统那套单实例管线（混合空间 / 脚部 IK）；
//! - 管线要的动画参数（[`LocomotionIntent`] / [`FacingState`]）在联网模式下没有本地物理
//!   去写，由 `drive_net_animation` 每帧从复制下来的 `NetMotion` / `NetYaw` 翻译过来——
//!   漏了这步，角色会以 Idle 姿态在地面上滑行；
//! - 其他玩家按 `PlayerModelAssets` 各自实例化一份模型 + 简易剪辑动画（Idle/Walk/Run 按
//!   `NetMotion.speed` 切），胶囊只当"模型还没加载完"的占位。

use std::net::SocketAddr;

use avian3d::prelude::*;
use bevy::{
    animation::{AnimatedBy, AnimationTargetId},
    prelude::*,
};
use crab_feast_library::net::{
    ControlChannel, CrabProtocolPlugin, JoinRequest, MOVE_SPEED_MPS, MoveInput, NetMotion,
    NetPlayerId, NetPos, NetYaw, TICK_HZ, Welcome,
};
use lightyear::netcode::prelude::{
    Authentication,
    client::{NetcodeClient, NetcodeConfig},
};
use lightyear::prelude::{client::*, *};

use crate::GameState;
use crate::anim::core::{SpaceEvalContext, evaluate_space};
use crate::anim::{AnimCatalog, ClipSet, FootIk, FootIkSet, MixerCache, Skeleton, build_skeleton};
use crate::camera::CameraFacing;
use crate::input::{ControlSet, FacingState, JumpInput, LocomotionIntent, MovementInput, wrap_pi};
use crate::scene::Avatar;

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
        .init_resource::<LoginPhase>()
        .add_systems(OnEnter(GameState::Game), connect)
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
                .before(bevy::transform::TransformSystems::Propagate),
        )
        .add_systems(
            Update,
            (
                // 相机这一帧要读角色的 Transform，所以复制状态必须先落地；
                // 复制状态落地后立刻翻译成动画参数，动画驱动这一帧才有东西可读
                (
                    on_welcome,
                    claim_local_avatar,
                    apply_net_transform,
                    drive_net_animation,
                )
                    .chain()
                    .before(ControlSet::Camera),
                // 模型必须等"谁是本地玩家"定下来再挂：本地角色走动画系统的单实例管线，
                // 提前挂简易模型会在认领那一帧闪一下
                (attach_player_models, setup_clip_models)
                    .chain()
                    .after(claim_local_avatar),
                // 输入要在移动系统把摇杆方向折算成世界方向之后采样
                send_input.after(ControlSet::Move),
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

fn connect(mut commands: Commands, config: Res<NetConfig>) {
    // 连接层：lightyear 的"裸 UdpIo"只是链路，握手/加密/Connected 状态由 netcode 负责。
    // 开发期用明文私钥自签 token；上线要换成后端签发的 Authentication::Token
    let netcode = NetcodeClient::new(
        Authentication::Manual {
            server_addr: config.server,
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
            LocalAddr(SocketAddr::from(([0, 0, 0, 0], 0))),
            PeerAddr(config.server),
            ReplicationReceiver,
            MessageManager::default(),
            // 注册了 ClientToServer 方向后 sender 本应自动插，这里显式写清楚，
            // 免得协议改动时静默丢输入
            MessageSender::<MoveInput>::default(),
            Name::new("net-client"),
        ))
        .id();

    commands.trigger(Connect { entity });
    info!(
        "connecting to {} as client {} (entity {entity:?})",
        config.server, config.client_id
    );
}

/// 远端实体复制下来后补上渲染：服务器只发状态，不发美术。
///
/// 同时把角色放进客户端物理世界：静态场景两端各自构建（见
/// [`crab_feast_library::level::spawn_static_level`]），所以这里挂的碰撞体有真实
/// 几何可解 —— 射线、接触查询、将来的本地预测都靠它。
///
/// 刚体是**运动学**的：位移仍由服务器的 `NetPos` 写，物理只负责扫掠与查询，
/// 不反过来积分位置。要变成"客户端自己跑角色物理"，把这里换成 `Dynamic` +
/// lightyear 的预测/回滚即可，静态场景这一层已经就位。
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
) {
    // 连接实体刚 spawn 出来时会先带一个 `Disconnected` 再转 `Connecting`，
    // 那种不算"掉线"，只有已经进房间之后的断开才值得报
    let was_in_game = matches!(*phase, LoginPhase::InGame(_));
    *phase = LoginPhase::Idle;
    commands.remove_resource::<LocalPlayer>();
    if was_in_game {
        warn!(
            "net: disconnected {:?}（掉线，登录状态回到 Idle）",
            trigger.entity
        );
    } else {
        debug!("net: disconnected {:?}（尚未进房间）", trigger.entity);
    }
}

fn on_welcome(
    mut commands: Commands,
    mut phase: ResMut<LoginPhase>,
    mut clients: Query<&mut MessageReceiver<Welcome>>,
) {
    for mut receiver in clients.iter_mut() {
        for welcome in receiver.receive() {
            info!("join: Welcome player_id={} → InGame", welcome.player_id);
            *phase = LoginPhase::InGame(welcome.player_id);
            commands.insert_resource(LocalPlayer(welcome.player_id));
        }
    }
}

/// 认领「哪个复制下来的实体是我」：给它挂 `Avatar` 标记，相机就会跟它。
fn claim_local_avatar(
    local: Option<Res<LocalPlayer>>,
    avatars: Query<(Entity, &NetPlayerId, Option<&Children>), Without<Avatar>>,
    clip_roots: Query<(), With<ClipModelRoot>>,
    mut commands: Commands,
) {
    let Some(local) = local else {
        return;
    };
    for (entity, id, children) in avatars.iter() {
        if id.0 != local.0 {
            continue;
        }
        info!("net: claiming local avatar {:?}", entity);
        // 本地角色交给动画系统：它自己会挂模型、跑混合空间与脚部 IK。
        // 之前先挂上的简易模型和占位胶囊都要撤掉，否则两套模型叠在一起
        if let Some(children) = children {
            for child in children.iter() {
                if clip_roots.contains(child) {
                    commands.entity(child).despawn();
                }
            }
        }
        commands
            .entity(entity)
            .insert((
                Avatar,
                // 动画驱动的输入。联网模式下角色的位移由服务器算，这两个组件
                // 没有被本地物理/输入写的来源，改由 `drive_net_animation` 从复制状态维护
                LocomotionIntent::default(),
                FacingState::default(),
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

/// 复制下来的状态 → 本地 Transform。
fn apply_net_transform(mut remotes: Query<(&NetPos, &NetYaw, &mut Transform)>) {
    for (pos, yaw, mut transform) in remotes.iter_mut() {
        transform.translation = Vec3::from_array(pos.0);
        transform.rotation = Quat::from_rotation_y(yaw.0);
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

/// 联网模式下本地角色的表现层：动画想定与朝向一律由服务端下发的状态驱动。
///
/// 位移由服务器算，本地不再有「摇杆 → `LocomotionIntent`」的来源；不接管的话
/// 认领来的角色会一直停在默认想定（站着），看起来就是「联网后本地角色没有动画」。
/// `dir` 是服务端的**意向方向**，`speed` 是**实测**速度，两者分开下发正是为了
/// 顶墙/被推时不把「想走」退化成「不动」。
fn drive_net_animation(
    local: Option<Res<LocalPlayer>>,
    mut avatars: Query<(
        &NetPlayerId,
        &NetMotion,
        &NetYaw,
        &mut LocomotionIntent,
        &mut FacingState,
    )>,
) {
    let Some(local) = local else {
        return;
    };
    for (id, motion, facing, mut intent, mut state) in avatars.iter_mut() {
        if id.0 != local.0 {
            continue;
        }
        let planar = Vec3::new(motion.dir[0], 0.0, motion.dir[1]);
        // 主动度是「这一帧有没有自己的输入」：被人推着滑、被平台带走时它是 0，
        // 想定必须清零回到 Idle。只看实测速度的话，人会被搬走的同时原地跑步
        if motion.intent > 1e-3 && planar.length_squared() > 1e-6 {
            intent.direction = planar.normalize();
            intent.magnitude = (motion.speed / MOVE_SPEED_MPS).clamp(0.0, 1.0);
            let target = planar.x.atan2(planar.z);
            state.error_deg = wrap_pi(target - facing.0).to_degrees();
        } else {
            intent.direction = Vec3::ZERO;
            intent.magnitude = 0.0;
            state.error_deg = 0.0;
        }
        state.yaw = facing.0;
        state.yaw_rate = 0.0;
    }
}

/// 远端角色也用混合空间驱动姿态：方向差 → `yaw`，实测速度 → `speed`。
///
/// 每具骨架各有一份映射与相位，求值函数和本地角色完全相同，所以「斜着走」「倒退」
/// 「转身时先侧步再正走」这些本地看得见的效果，远端也会有。
fn drive_remote_pose(
    time: Res<Time>,
    assets: Res<PlayerModelAssets>,
    cache: Res<MixerCache>,
    clips: Res<ClipSet>,
    clip_assets: Res<Assets<AnimationClip>>,
    mut rigs: Query<(Entity, &mut RemotePose, &ModelOwner)>,
    mut foot_iks: Query<&mut FootIk>,
    motions: Query<(&NetMotion, &NetYaw)>,
    children: Query<&Children>,
    targets: Query<(&AnimationTargetId, Option<&AnimatedBy>)>,
    names: Query<&Name>,
    mut transforms: Query<&mut Transform>,
    mut mismatched: Local<bool>,
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
        if !*mismatched {
            warn!(
                "net: 当前剪辑表属于 {:?}，远端模型是 {}，远端姿态先不驱动",
                clips.character(),
                assets.character
            );
            *mismatched = true;
        }
        return;
    }
    *mismatched = false;
    let axes = space.params.clone();
    let dt = time.delta_secs().min(0.05);

    for (root, mut rig, owner) in rigs.iter_mut() {
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
        let output = evaluate_space(&space, &mut context);
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
            ik.resting = measured_mps < 0.05 && speed.abs() < 0.02;
        }
    }
}

/// 调试用：`--auto-walk <秒>` 时自动把摇杆往前推，跑联调脚本用。
#[derive(Resource, Debug)]
pub struct AutoWalk(pub Timer);

/// 每帧上传本帧输入。
fn send_input(
    mut senders: Query<&mut MessageSender<MoveInput>, With<Client>>,
    input: Res<MovementInput>,
    jump: Res<JumpInput>,
    facing: Res<CameraFacing>,
    time: Res<Time>,
    auto_walk: Option<ResMut<AutoWalk>>,
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
        (Vec2::new(0.0, -1.0), 1.0)
    } else {
        match *input {
            MovementInput::Idle => (Vec2::ZERO, 0.0),
            MovementInput::Activated { direction, force } => (direction, force),
        }
    };
    let jump = matches!(*jump, JumpInput::Activated);

    // 摇杆是相对相机的：往上推 = 屏幕深处 = 视线水平朝向
    let forward = facing.view_forward();
    let right = Vec3::new(-forward.z, 0.0, forward.x);
    let world = right * local.x + forward * (-local.y);
    let message = MoveInput {
        dir: [world.x, world.z],
        force,
        jump,
    };
    for mut sender in senders.iter_mut() {
        sender.send::<ControlChannel>(message);
    }
}
