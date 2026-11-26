//! 看向（look-at）：把「偏航 / 俯仰」两个维度程序化地摊到整条脊椎上。
//!
//! 为什么不用剪辑：`LookLeft` / `LookRight` 只能覆盖两个方向，斜上方、斜下方
//! 这类组合要么没有素材，要么靠两段剪辑交叉淡入 —— 中间态里脖子和胸的份额
//! 是两份动画各自带的，谁多拧一点不受控制，转到一半最容易出现「头拧过 90°、
//! 锁骨纹丝不动」的假人感。这里改成在**动画姿态之上**叠加一个纯粹的偏航 + 俯仰
//! 旋转，方向连续、份额可控。
//!
//! 三条约束决定观感：
//!
//! 1. **每根骨骼只转自己那一份，且不超过解剖上限。**
//!    腰椎（`Spine`）几乎不参与转身，胸椎（`Spine1` / `Spine2`）承担一部分，
//!    脖子（`Neck`）承担最多，头骨（`Head`）收尾。份额和上限见 [`LOOK_BONES`]。
//!    把总角度先按份额分，再逐根夹到上限、把被夹掉的部分按剩余份额二次分配，
//!    所以不管输入多大，单根骨骼都不会拧成橡皮。
//! 2. **不同方向不同上限。** 偏航左右 120°、抬头 40°、低头 35°（默认），
//!    进入上限前的最后 30% 用 smoothstep 压扁：输入再大也只是逼近上限，
//!    不会在阈值上「啪」一下停住。
//! 3. **头颈先到位，躯干再跟上。** 人瞟一眼是脖子转、身子基本不动；持续盯着看
//!    才会把胸椎慢慢带过来。实现上是两条时间常数不同的通道：
//!    - 颈椎通道的目标 = `总角度 − 脊柱当前角度`，跟得快（[`LookSettings::cervical_secs`]）；
//!    - 脊柱通道的目标 = `总角度 × 脊柱份额`，跟得慢（[`LookSettings::spine_secs`]）。
//!    两通道之和恒等于总角度，所以头不会「等」身体；等身子转过来，脖子再慢慢
//!    退回自己那一份。快速一瞥只动头，长看才带肩，就是靠这个。
//!
//! 数学上，每根骨骼拿到的是**模型空间**里的 `Ry(yaw) * Rx(-pitch)`，再换算到
//! 父骨骼空间左乘到当前姿态上：
//!
//! ```text
//! q_local_new = (P⁻¹ · R · P) · q_local_anim
//! ```
//!
//! `P` 是父骨骼**当前**的世界旋转（逐根累加得到，已经包含动画和本层刚写进去的
//! 偏航/俯仰）。这样即使动画本身把脊柱弯着，偏航也是绕「当下这根脊椎的轴」转，
//! 而不是绕世界 Y 轴硬拧 —— 后者在弯腰时会变成侧倾。
//!
//! 坐标约定（与 `input::yaw_of` 一致）：模型正前方 +Z、上 +Y、角色左侧 +X。
//! 偏航为正 = 向左转，俯仰为正 = 抬头。左右 65° 时颈椎占 37°，胸椎 27°，
//! 腰椎 5° 左右，和真人「回头看」的分配接近。

use bevy::prelude::*;
use bevy::transform::TransformSystems;

use crate::GameState;
use crate::camera::CameraFacing;
use crate::input::{FacingState, OrbitLock, camera_view_yaw, wrap_pi};

use super::AnimationSet;
use super::character::AnimModelRoot;
use super::driver::drive_pose;
use super::foot_ik::FootIkSet;

pub struct LookPlugin;

/// 看向链上的一根骨骼。只认这五段，别的骨骼（手臂、髋、控制骨）一律不碰。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookBoneKind {
    /// 腰椎（Mixamo `Spine`）
    Spine,
    /// 下胸椎（`Spine1`）
    Spine1,
    /// 上胸椎（`Spine2`；没有时可用 `Chest` / `UpperChest` 顶上）
    Spine2,
    /// 颈椎（`Neck`）
    Neck,
    /// 头骨（`Head`，即寰枕关节）
    Head,
}

impl LookBoneKind {
    /// 索引顺序同时是「从腰到头的传递顺序」，份额表按它排列
    pub const ALL: [Self; 5] = [
        Self::Spine,
        Self::Spine1,
        Self::Spine2,
        Self::Neck,
        Self::Head,
    ];

    fn index(self) -> usize {
        match self {
            Self::Spine => 0,
            Self::Spine1 => 1,
            Self::Spine2 => 2,
            Self::Neck => 3,
            Self::Head => 4,
        }
    }
}

/// 一根骨骼的份额与解剖上限（角度单位为度）。
///
/// 份额是**整条链**上的占比，按两个轴分开：轴向旋转（偏航）和屈伸（俯仰）
/// 在人体里根本不是同一套能力——腰椎几乎不能拧、能弯；颈椎反过来。所以
/// 一味的「一个份额走两个轴」会让抬头把整条腰掰弯。默认表里偏航是
/// 脊柱 0.50 / 颈椎 0.50，俯仰是 0.22 / 0.78。
///
/// 上限是单根骨骼这辈子都不该超过的角度，用来兜住极端输入，
/// 正常范围内的分配由份额决定。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LookBoneProfile {
    pub kind: LookBoneKind,
    /// 整条链上的偏航（轴向旋转）份额
    pub yaw_share: f32,
    /// 整条链上的俯仰（屈伸）份额
    pub pitch_share: f32,
    /// 偏航上限（左右对称）
    pub yaw_cap: f32,
    /// 抬头上限
    pub pitch_up_cap: f32,
    /// 低头上限
    pub pitch_down_cap: f32,
}

/// 默认份额与上限。
///
/// **偏航**（轴向旋转，节点从下到上递增）：腰椎本身几乎不能拧（L1–L5 合计
/// 约 5°），胸椎才是转身的主力（合计 35~40°），颈椎再补 45~50°。所以份额
/// 取 0.10 / 0.16 / 0.24 / 0.32 / 0.18 —— 脊柱组 0.50、颈椎组 0.50。65° 的
/// 看向量落到每根是 6.5 / 10.4 / 15.6 / 20.8 / 11.7，胸口相对髋转了 32°，
/// 不是只有脖子的「假人扭头」。
///
/// **俯仰**（屈伸）完全是另一套能力：腰椎能弯（前屈 40°+），胸椎伸展只有
/// 10~15°，脖子和头才是抬头低头的主力。所以份额取 0.05 / 0.07 / 0.10 /
/// 0.42 / 0.36 —— 脊柱组只有 0.22，抬头 40° 时腰只贡献 2° 左右，
/// 不会「一抬头整个人往后折」。
///
/// 上限是各段的**解剖硬顶**：腰椎拧 10°、下胸 20°、上胸 30°（胸椎合计 40~50°）、
/// 颈椎 38°、头 26°。这条链的总容量 60 + 64 = 124°，刚好罩住 120° 的默认限幅，
/// 所以镜头绕到正后方时肩膀能真的跟着拧过去，而不是被截断在 90°。
/// 再往后的 180° 靠拧腰做不到——真人得挪脚转身。
pub const LOOK_BONES: [LookBoneProfile; 5] = [
    LookBoneProfile {
        kind: LookBoneKind::Spine,
        yaw_share: 0.10,
        pitch_share: 0.05,
        yaw_cap: 10.0,
        pitch_up_cap: 5.0,
        pitch_down_cap: 8.0,
    },
    LookBoneProfile {
        kind: LookBoneKind::Spine1,
        yaw_share: 0.16,
        pitch_share: 0.07,
        yaw_cap: 20.0,
        pitch_up_cap: 6.0,
        pitch_down_cap: 9.0,
    },
    LookBoneProfile {
        kind: LookBoneKind::Spine2,
        yaw_share: 0.24,
        pitch_share: 0.10,
        yaw_cap: 30.0,
        pitch_up_cap: 8.0,
        pitch_down_cap: 11.0,
    },
    LookBoneProfile {
        kind: LookBoneKind::Neck,
        yaw_share: 0.32,
        pitch_share: 0.42,
        yaw_cap: 38.0,
        pitch_up_cap: 24.0,
        pitch_down_cap: 28.0,
    },
    LookBoneProfile {
        kind: LookBoneKind::Head,
        yaw_share: 0.18,
        pitch_share: 0.36,
        yaw_cap: 26.0,
        pitch_up_cap: 18.0,
        pitch_down_cap: 22.0,
    },
];

/// 看向的全局参数。逐帧读取，改这里当帧生效；每个角色还能用 [`LookAt::weight`]
/// 再做二级权重（联机远端、过场、死亡等）。
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct LookSettings {
    /// 总开关。关掉时目标权重降到 0，姿态平滑回动画，不会硬切
    pub enabled: bool,
    /// 是否由相机自动驱动本地角色。手动控制（AI、剧情、联机远端）时置 false，
    /// 自己写 [`LookAt`] 即可
    pub drive_from_camera: bool,
    /// 全局权重（0..1），和 [`LookAt::weight`] 相乘
    pub weight: f32,
    /// 向左看的上限（度）
    pub yaw_left: f32,
    /// 向右看的上限（度）
    pub yaw_right: f32,
    /// 抬头上限（度）
    pub pitch_up: f32,
    /// 低头上限（度）
    pub pitch_down: f32,
    /// 大角度回头时开始带低头的偏航阈值（度）。真人侧头看肩后，颈椎不是
    /// 纯水平绕 Y，会顺带往下压一点
    pub yaw_dip_start: f32,
    /// 超过阈值后每多 1° 偏航补多少度低头（度）。0 = 关闭这个耦合
    pub yaw_dip_per_deg: f32,
    /// 上限前的软饱和起点（0..1）。0.7 = 超过 70% 上限后开始压扁，
    /// 到上限处斜率为 0，再大硬夹住
    pub soft_zone: f32,
    /// 输入角速度上限（度/秒）：镜头甩得再快，头也不会瞬移
    pub max_speed: f32,
    /// 输入层时间常数（秒）：抹掉镜头的高频抖动
    pub input_secs: f32,
    /// 颈椎通道时间常数（秒）：小 = 头先到位
    pub cervical_secs: f32,
    /// 脊柱通道时间常数（秒）：大 = 躯干后跟，快速一瞥基本不动
    pub spine_secs: f32,
    /// 权重淡入淡出时间常数（秒）
    pub blend_secs: f32,
    /// 每根骨骼的份额与上限
    pub profiles: [LookBoneProfile; 5],
}

impl Default for LookSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            drive_from_camera: true,
            weight: 1.0,
            yaw_left: 120.0,
            yaw_right: 120.0,
            pitch_up: 40.0,
            pitch_down: 35.0,
            yaw_dip_start: 50.0,
            yaw_dip_per_deg: 0.18,
            soft_zone: 0.7,
            max_speed: 220.0,
            input_secs: 0.12,
            cervical_secs: 0.10,
            spine_secs: 0.45,
            blend_secs: 0.18,
            profiles: LOOK_BONES,
        }
    }
}

impl LookSettings {
    pub fn profile(&self, kind: LookBoneKind) -> &LookBoneProfile {
        &self.profiles[kind.index()]
    }
}

/// 看向请求：写在模型根实体上（和 [`AnimModelRoot`] 同一个实体）。
///
/// 角度是**相对角色正前方**的，不是相对相机：偏航 + 左、俯仰 + 抬头。
/// 用 [`LookAt::from_world_dir`] 可以把世界空间方向换算成这个约定。
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct LookAt {
    /// 偏航（度）：+ 向左，- 向右
    pub yaw: f32,
    /// 俯仰（度）：+ 抬头，- 低头
    pub pitch: f32,
    /// 本角色权重（0..1）
    pub weight: f32,
}

impl Default for LookAt {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: 0.0,
            weight: 1.0,
        }
    }
}

impl LookAt {
    pub fn new(yaw: f32, pitch: f32) -> Self {
        Self {
            yaw,
            pitch,
            weight: 1.0,
        }
    }

    /// 世界空间方向 → 相对角色朝向的角度。
    ///
    /// `character_yaw` 就是 `FacingState::yaw` / `AvatarYaw` 的绕 Y 角；
    /// 方向不需要归一化（只取水平偏角和仰角）。
    pub fn from_world_dir(dir: Vec3, character_yaw: f32) -> Self {
        if dir.length_squared() <= f32::EPSILON {
            return Self::default();
        }
        // 转到角色局部空间：+Z 正前、+X 左
        let local = Quat::from_rotation_y(-character_yaw) * dir;
        let yaw = local.x.atan2(local.z).to_degrees();
        let horizontal = local.xz().length();
        let pitch = local.y.atan2(horizontal).to_degrees();
        Self::new(yaw, pitch)
    }

    /// 看向世界空间里的一个点（`eye` 是角色眼睛位置，通常取头骨世界坐标）
    pub fn toward_point(eye: Vec3, point: Vec3, character_yaw: f32) -> Self {
        Self::from_world_dir(point - eye, character_yaw)
    }

    /// 输入已经落在限制外时先夹一次，避免把没有意义的大角度送进平滑器
    /// （`LookSettings` 会再按方向做软饱和）
    pub fn limited(mut self, yaw_left: f32, yaw_right: f32, up: f32, down: f32) -> Self {
        self.yaw = self.yaw.clamp(-yaw_right, yaw_left);
        self.pitch = self.pitch.clamp(-down, up);
        self
    }
}

/// 一个角色看向层的运行时状态：骨骼绑定、两条通道的平滑值和权重。
#[derive(Component, Default)]
pub struct LookState {
    bind: Option<LookBind>,
    /// 输入层（总角度，已限幅、已平滑）——两通道之和的目标
    input_yaw: f32,
    input_pitch: f32,
    /// 脊柱通道当前角度
    spine_yaw: f32,
    spine_pitch: f32,
    /// 颈椎通道当前角度
    cervical_yaw: f32,
    cervical_pitch: f32,
    /// 当前生效权重（0..1），`LookAt::weight × LookSettings::weight`
    weight: f32,
}

impl LookState {
    /// 此刻实际应用的总偏航（脊柱 + 颈椎），供调试 / 联机采样
    pub fn applied_yaw(&self) -> f32 {
        self.spine_yaw + self.cervical_yaw
    }

    /// 此刻实际应用的总俯仰
    pub fn applied_pitch(&self) -> f32 {
        self.spine_pitch + self.cervical_pitch
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LookGroup {
    Spine,
    Cervical,
}

impl LookBoneKind {
    fn group(self) -> LookGroup {
        match self {
            Self::Spine | Self::Spine1 | Self::Spine2 => LookGroup::Spine,
            Self::Neck | Self::Head => LookGroup::Cervical,
        }
    }
}

struct LookSlot {
    /// 在 `path` 里的下标
    path_index: usize,
    kind: LookBoneKind,
}

/// 绑定结果：从模型根下一层到 `Head` 的完整骨骼路径 + 其中要驱动的几根。
///
/// 路径里包含 `Armature`、`Hips` 这些**不驱动**的骨骼，因为算父骨骼世界旋转
/// 时必须把它们也算进去（Mixamo 的 Armature 带 -90° 的 Z-up 转换）。
struct LookBind {
    path: Vec<Entity>,
    slots: Vec<LookSlot>,
}

impl Plugin for LookPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LookSettings>()
            // 先确保本地角色带上组件，再由相机写角度；同帧顺序由 chain 保证
            .add_systems(
                Update,
                (ensure_look_targets, drive_look_from_camera)
                    .chain()
                    .in_set(AnimationSet::Drive)
                    .run_if(in_state(GameState::Game)),
            )
            .add_systems(
                PostUpdate,
                apply_look
                    // 动画写骨（含动作层）、脚 IK 都改完，才轮到看向
                    .after(drive_pose)
                    .after(FootIkSet)
                    .before(TransformSystems::Propagate)
                    .run_if(in_state(GameState::Game)),
            );
    }
}

/// 本地角色模型根自动挂上看向组件；远端角色按需自己插 [`LookAt`]
fn ensure_look_targets(
    mut commands: Commands,
    models: Query<Entity, (With<AnimModelRoot>, Without<LookAt>)>,
) {
    for model in &models {
        commands
            .entity(model)
            .insert((LookAt::default(), LookState::default()));
    }
}

/// 相机相对角色朝向的偏角 → 看向输入。
///
/// 语义是「看向镜头**朝向**的方向」，不是「回头看镜头」：镜头绕到侧面时，
/// 上半身拧到镜头正对的方向（TPS 的瞄准跟随）；镜头俯视时视线朝下，角色跟着
/// 低头。偏航为正 = 向左拧，和 `input::yaw_of` 同约定。
///
/// **只有按住 Ctrl（[`OrbitLock`]）才驱动**。那套操作里镜头只是被拿去观察，
/// 行进方向已经单独冻住了，所以上半身可以把镜头朝向往回拧——镜头绕到侧面、
/// 角色还在直线走的时候，头会跟着镜头指向转过去（最多 65°，见 [`LOOK_BONES`]）。
///
/// 不按住时 `weight` 置 0：平时转镜头不带动上半身，身体转向由 `facing_system`
/// 负责。权重按 [`LookSettings::blend_secs`] 淡入淡出，按下 / 松开不会硬切。
///
/// 真正写骨骼的是 [`apply_look`]，这里只负责把「想看哪里」写成角度。
fn drive_look_from_camera(
    settings: Res<LookSettings>,
    camera: Res<CameraFacing>,
    // 最小 App（单测）里没有输入插件，缺席就永远不驱动
    orbit_lock: Option<Res<OrbitLock>>,
    avatars: Query<&FacingState>,
    mut models: Query<&mut LookAt, With<AnimModelRoot>>,
) {
    if !settings.drive_from_camera {
        return;
    }

    let Some(facing) = avatars.iter().next() else {
        return;
    };

    let held = orbit_lock.as_deref().is_some_and(OrbitLock::drives_look);
    // 角度按「镜头朝向」算好，靠权重开关：淡出期间**角度留着不动**，
    // 按下那一刻只是权重从 0 涨到 1，头是平稳转过去的；松开时也不会
    // 在淡出的 0.18 s 里把脖子甩回 0°
    let weight = if settings.enabled && held { 1.0 } else { 0.0 };

    for mut look in &mut models {
        if held {
            let (yaw, pitch) = camera_look_angles(&camera, facing.yaw);
            look.yaw = yaw;
            look.pitch = pitch;
        }
        look.weight = weight;
    }
}

/// 相机位姿 → 看向角度（度），相对角色正前方。
///
/// 取镜头的**朝向**：偏航 = 视线水平朝向 − 角色朝向；俯仰 = −机位仰角
/// （机位在上、视线朝下时角色低头）。
fn camera_look_angles(camera: &CameraFacing, facing_yaw: f32) -> (f32, f32) {
    // 镜头视线的水平朝向 - 角色朝向 = 上半身要拧的角度；正 = 向左
    let yaw = wrap_pi(camera_view_yaw(camera) - facing_yaw).to_degrees();
    // 相机 pitch > 0 = 机位在上、视线朝下，角色跟着往下看（正俯仰 = 抬头）
    let pitch = -camera.pitch.to_degrees();
    (yaw, pitch)
}

/// 每帧求解并把偏航 / 俯仰叠加到骨骼上。
#[allow(clippy::too_many_arguments)]
fn apply_look(
    time: Res<Time>,
    settings: Res<LookSettings>,
    mut models: Query<(Entity, &LookAt, &mut LookState)>,
    children: Query<&Children>,
    parents: Query<&ChildOf>,
    names: Query<&Name>,
    mut transforms: Query<&mut Transform>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }

    for (entity, request, mut state) in &mut models {
        let LookState {
            bind,
            input_yaw,
            input_pitch,
            spine_yaw,
            spine_pitch,
            cervical_yaw,
            cervical_pitch,
            weight,
        } = &mut *state;

        // 骨架可能比模型实体晚出来：没绑上就每帧重试一次（绑定成功后会短路）
        if bind.is_none() {
            *bind = try_bind(entity, &children, &parents, &names);
        }

        let stale = bind
            .as_ref()
            .map(|bound| bound.path.iter().any(|bone| !transforms.contains(*bone)))
            .unwrap_or(false);
        if stale {
            *bind = None;
            continue;
        }
        let Some(bound) = bind.as_ref() else {
            continue;
        };

        // 1. 权重：目标值淡入淡出，不做硬切
        let target_weight = if settings.enabled {
            settings.weight.clamp(0.0, 1.0) * request.weight.clamp(0.0, 1.0)
        } else {
            0.0
        };
        *weight = approach(*weight, target_weight, settings.blend_secs, dt);

        // 链的容量先算出来：配置的上限（yaw_left/right）只是「最多允许拧多少」，
        // 真正能拧出来的受各根骨骼的解剖上限约束。两者取小，避免输入量超过
        // 链的容纳能力、被 distribute 静态截断（表现就是「头停在 90° 不动」）。
        let limits = group_limits(&bound.slots, &settings);
        let yaw_left = settings.yaw_left.min(limits.yaw_capacity);
        let yaw_right = settings.yaw_right.min(limits.yaw_capacity);

        // 2. 输入限幅：先按方向夹到上限，再叠角速度上限
        let yaw_target = soft_limit(
            request.yaw * *weight,
            yaw_right,
            yaw_left,
            settings.soft_zone,
        );
        *input_yaw = approach_limited(
            *input_yaw,
            yaw_target,
            settings.input_secs,
            settings.max_speed,
            dt,
        );

        // 2b. 大角度回头的「顺带低头」。注意不能并进上面的 pitch 通道再分配：
        // 偏航是绕 Y 的，各节中间再插俯仰，前面几节压下去的量会被后面几节的
        // 偏航转成横向（实测只剩 1/3）。所以这个低头单独结算，最后在**头骨
        // 自己的局部轴**上补一次俯仰（见第 5 步），方向永远是真往下。
        // 用已平滑的实际偏航推导，低头跟着回头一起长出来；小角度不参与。
        let dip = yaw_dip(*input_yaw, &settings) * *weight;
        let pitch_target = soft_limit(
            request.pitch * *weight,
            settings.pitch_down,
            settings.pitch_up,
            settings.soft_zone,
        );
        *input_pitch = approach_limited(
            *input_pitch,
            pitch_target,
            settings.input_secs,
            settings.max_speed,
            dt,
        );

        // 3. 两通道：颈椎目标 = 总角度 - 脊柱当前，脊柱目标 = 总角度 × 脊柱份额
        // （偏航、俯仰各用各的份额比：拧是整个胸廓的事，仰头不是）

        let spine_goal_yaw = *input_yaw * limits.spine_yaw_ratio;
        let spine_goal_pitch = *input_pitch * limits.spine_pitch_ratio;
        *spine_yaw = approach(*spine_yaw, spine_goal_yaw, settings.spine_secs, dt);
        *spine_pitch = approach(*spine_pitch, spine_goal_pitch, settings.spine_secs, dt);

        let cervical_goal_yaw =
            (*input_yaw - *spine_yaw).clamp(-limits.cervical_yaw_cap, limits.cervical_yaw_cap);
        let cervical_goal_pitch = {
            let cap = if *input_pitch >= 0.0 {
                limits.cervical_up_cap
            } else {
                limits.cervical_down_cap
            };
            (*input_pitch - *spine_pitch).clamp(-cap, cap)
        };
        *cervical_yaw = approach(*cervical_yaw, cervical_goal_yaw, settings.cervical_secs, dt);
        *cervical_pitch = approach(
            *cervical_pitch,
            cervical_goal_pitch,
            settings.cervical_secs,
            dt,
        );

        // 4. 组内按份额分配，每根不超过自己的上限
        let mut angles = [(0.0f32, 0.0f32); LookBoneKind::ALL.len()];
        allocate_group(
            &bound.slots,
            LookGroup::Spine,
            *spine_yaw,
            *spine_pitch,
            &settings,
            &mut angles,
        );
        allocate_group(
            &bound.slots,
            LookGroup::Cervical,
            *cervical_yaw,
            *cervical_pitch,
            &settings,
            &mut angles,
        );

        // 5. 沿链写入：父骨骼当前世界旋转 P 逐根累加
        let mut accumulated = Quat::IDENTITY;
        for (index, entity) in bound.path.iter().enumerate() {
            let Ok(mut transform) = transforms.get_mut(*entity) else {
                // 父链断了就没法把旋转换算到正确的轴上，后面的骨骼宁可不写
                break;
            };
            let local = transform.rotation;

            if let Some(slot) = bound.slots.iter().find(|slot| slot.path_index == index) {
                let (yaw, pitch) = angles[slot.kind.index()];
                let rotation = Quat::from_rotation_y(yaw.to_radians())
                    * Quat::from_rotation_x(-pitch.to_radians());
                let delta = accumulated.inverse() * rotation * accumulated;
                let mut posed = delta * local;
                // 头骨是链上最后一节：右乘 = 在头自己的轴系里补俯仰。
                // 此时头已经完成偏航，绕局部 X 压下去就是「视线往肩后下方落」，
                // 不会有任何中间骨骼的偏航把它转歪。
                if slot.kind == LookBoneKind::Head {
                    posed *= Quat::from_rotation_x(-dip.to_radians());
                }
                transform.rotation = posed;
                accumulated *= posed;
            } else {
                accumulated *= local;
            }
        }
    }
}

/// 组内分配：把 `yaw` / `pitch` 按份额分给同组骨骼，结果写进 `angles[kind]`。
fn allocate_group(
    slots: &[LookSlot],
    group: LookGroup,
    yaw: f32,
    pitch: f32,
    settings: &LookSettings,
    angles: &mut [(f32, f32); LookBoneKind::ALL.len()],
) {
    let mut yaw_weights = [0.0f32; LookBoneKind::ALL.len()];
    let mut pitch_weights = [0.0f32; LookBoneKind::ALL.len()];
    let mut yaw_caps = [0.0f32; LookBoneKind::ALL.len()];
    let mut pitch_caps = [0.0f32; LookBoneKind::ALL.len()];
    let mut count = 0usize;

    for slot in slots.iter().filter(|slot| slot.kind.group() == group) {
        let profile = settings.profile(slot.kind);
        yaw_weights[count] = profile.yaw_share;
        pitch_weights[count] = profile.pitch_share;
        yaw_caps[count] = profile.yaw_cap;
        // 俯仰按这次的方向选上限：抬头一套、低头一套
        pitch_caps[count] = if pitch >= 0.0 {
            profile.pitch_up_cap
        } else {
            profile.pitch_down_cap
        };
        count += 1;
    }
    if count == 0 {
        return;
    }

    let mut yaw_out = [0.0f32; LookBoneKind::ALL.len()];
    let mut pitch_out = [0.0f32; LookBoneKind::ALL.len()];
    distribute(
        yaw,
        &yaw_weights[..count],
        &yaw_caps[..count],
        &mut yaw_out[..count],
    );
    distribute(
        pitch,
        &pitch_weights[..count],
        &pitch_caps[..count],
        &mut pitch_out[..count],
    );

    for (k, slot) in slots
        .iter()
        .filter(|slot| slot.kind.group() == group)
        .enumerate()
    {
        angles[slot.kind.index()] = (yaw_out[k], pitch_out[k]);
    }
}

/// 两组的份额比（偏航、俯仰各一份）与颈椎组的方向上限。
///
/// 按实际找到的骨骼重算：缺一两根时份额仍然归一，不会因为少绑一根就让总量
/// 缩水。偏航和俯仰分开算，因为两套能力本来就不同（见 [`LOOK_BONES`]）。
struct GroupLimits {
    spine_yaw_ratio: f32,
    spine_pitch_ratio: f32,
    /// 整条链的偏航容量（= 各根 yaw_cap 之和）
    yaw_capacity: f32,
    cervical_yaw_cap: f32,
    cervical_up_cap: f32,
    cervical_down_cap: f32,
}

fn group_limits(slots: &[LookSlot], settings: &LookSettings) -> GroupLimits {
    let mut spine_yaw = 0.0;
    let mut spine_yaw_cap = 0.0;
    let mut spine_pitch = 0.0;
    let mut cervical_yaw = 0.0;
    let mut cervical_pitch = 0.0;
    let mut yaw_cap = 0.0;
    let mut up_cap = 0.0;
    let mut down_cap = 0.0;

    for slot in slots {
        let profile = settings.profile(slot.kind);
        match slot.kind.group() {
            LookGroup::Spine => {
                spine_yaw += profile.yaw_share;
                spine_yaw_cap += profile.yaw_cap;
                spine_pitch += profile.pitch_share;
            }
            LookGroup::Cervical => {
                cervical_yaw += profile.yaw_share;
                cervical_pitch += profile.pitch_share;
                yaw_cap += profile.yaw_cap;
                up_cap += profile.pitch_up_cap;
                down_cap += profile.pitch_down_cap;
            }
        }
    }

    let ratio = |part: f32, whole: f32| if whole > f32::EPSILON { part / whole } else { 0.0 };
    GroupLimits {
        spine_yaw_ratio: ratio(spine_yaw, spine_yaw + cervical_yaw),
        spine_pitch_ratio: ratio(spine_pitch, spine_pitch + cervical_pitch),
        // 两组 caps 之和：配置的上限再大，链也只能拧出这么多
        yaw_capacity: spine_yaw_cap + yaw_cap,
        cervical_yaw_cap: yaw_cap,
        cervical_up_cap: up_cap,
        cervical_down_cap: down_cap,
    }
}

/// 按权重分总量，每根不超过 `caps`；被夹掉的部分按剩余权重二次分配。
///
/// 全程保持符号（负 = 反方向），最后总和最多等于 `Σ caps`。调用方保证
/// `weights` / `caps` / `out` 等长。
fn distribute(total: f32, weights: &[f32], caps: &[f32], out: &mut [f32]) {
    debug_assert_eq!(weights.len(), caps.len());
    debug_assert_eq!(weights.len(), out.len());
    out.fill(0.0);

    if total.abs() <= 1e-6 {
        return;
    }
    let sign = total.signum();
    let mut remaining = total;
    let mut active: Vec<usize> = weights
        .iter()
        .zip(caps.iter())
        .enumerate()
        .filter(|(_, (weight, cap))| **weight > 0.0 && **cap > 0.0)
        .map(|(index, _)| index)
        .collect();

    // 每轮至少夹满一根骨骼，最多 n 轮
    for _ in 0..weights.len() {
        if active.is_empty() || remaining.abs() <= 1e-5 {
            break;
        }
        let weight_sum: f32 = active.iter().map(|&i| weights[i]).sum();
        if weight_sum <= 1e-6 {
            break;
        }

        let mut placed = 0.0;
        let mut next = Vec::new();
        for &i in &active {
            let want = remaining.abs() * weights[i] / weight_sum;
            let room = (caps[i] - out[i].abs()).max(0.0);
            let add = want.min(room) * sign;
            out[i] += add;
            placed += add;
            if caps[i] - out[i].abs() > 1e-5 {
                next.push(i);
            }
        }
        if placed.abs() <= 1e-6 {
            break;
        }
        remaining -= placed;
        active = next;
    }
}

/// 大角度回头时顺带的一点低头（返回负值 = 低头，度）。
///
/// 纯偏航是「假人扭头」：真人扭转颈椎时轴是斜的，角度一大，视线会自然落到
/// 肩后偏低的位置。这里用一个简单的线性耦合（超过阈值后每度偏航补 `per_deg`
/// 度低头），量不大（120° 时约 10°），但足以让大角度回头不像风扇在转。
fn yaw_dip(yaw: f32, settings: &LookSettings) -> f32 {
    let excess = (yaw.abs() - settings.yaw_dip_start).max(0.0);
    -(excess * settings.yaw_dip_per_deg)
}

/// 方向相关的软限幅：`value` 在 `±limit × soft_zone` 内原样通过，之后用
/// smoothstep 压到上限，到上限处斜率为 0（不会「啪」一下停住），再大硬夹。
fn soft_limit(value: f32, negative_limit: f32, positive_limit: f32, soft_zone: f32) -> f32 {
    let limit = if value >= 0.0 {
        positive_limit
    } else {
        negative_limit
    };
    if limit <= 0.0 {
        return 0.0;
    }

    let knee = limit * soft_zone.clamp(0.0, 0.95);
    let magnitude = value.abs();
    if magnitude <= knee {
        return value;
    }

    let span = limit - knee;
    let t = ((magnitude - knee) / span).clamp(0.0, 1.0);
    // smoothstep：两端斜率为 0，中段近似线性
    let eased = t * t * (3.0 - 2.0 * t);
    (knee + span * eased).copysign(value)
}

/// 指数趋近，时间常数 `secs`；`secs <= 0` 视为硬切
fn approach(current: f32, target: f32, secs: f32, dt: f32) -> f32 {
    if secs <= 0.0 {
        return target;
    }
    let t = 1.0 - (-dt / secs).exp();
    current + (target - current) * t
}

/// 角速度受限的指数趋近（`max_speed` 单位度/秒）
fn approach_limited(current: f32, target: f32, secs: f32, max_speed: f32, dt: f32) -> f32 {
    let goal = approach(current, target, secs, dt);
    let max_step = max_speed.max(0.0) * dt;
    let delta = (goal - current).clamp(-max_step, max_step);
    current + delta
}

/// 绑定：从模型根找到 `Head`，把「模型根 → Head」的整条路径缓存下来。
fn try_bind(
    root: Entity,
    children: &Query<&Children>,
    parents: &Query<&ChildOf>,
    names: &Query<&Name>,
) -> Option<LookBind> {
    let head = find_bone(root, children, names, LookBoneKind::Head)?;
    let mut found = Vec::new();
    for kind in LookBoneKind::ALL {
        if kind == LookBoneKind::Head {
            found.push((kind, head));
        } else if let Some(entity) = find_bone(root, children, names, kind) {
            found.push((kind, entity));
        }
    }

    // Head 到模型根的祖先链（模型根本身不算：看向角度是相对模型空间量的）
    let mut ordered = vec![head];
    loop {
        let parent = parents
            .get(*ordered.last()?)
            .map(|child_of| child_of.parent())
            .ok()?;
        if parent == root {
            break;
        }
        ordered.push(parent);
        if ordered.len() > 64 {
            return None;
        }
    }
    ordered.reverse();

    let mut slots: Vec<LookSlot> = Vec::with_capacity(found.len());
    let mut last_index = 0usize;
    for (kind, entity) in found {
        let Some(path_index) = ordered.iter().position(|candidate| *candidate == entity) else {
            // 不在 Head 的祖先链上（不同分支的同名骨），忽略
            continue;
        };
        if path_index <= last_index {
            continue;
        }
        last_index = path_index;
        slots.push(LookSlot { path_index, kind });
    }

    if slots.is_empty() {
        return None;
    }

    info!(
        "看向绑定：{} 根骨骼，链长 {}（{}）",
        slots.len(),
        ordered.len(),
        slots
            .iter()
            .map(|slot| format!("{:?}@{}", slot.kind, slot.path_index))
            .collect::<Vec<_>>()
            .join(" ")
    );

    Some(LookBind {
        path: ordered,
        slots,
    })
}

/// 只看名字找骨：跳过控制器 / 辅助骨，认 Mixamo 的 `mixamorig:Spine1` 这类命名。
fn find_bone(
    root: Entity,
    children: &Query<&Children>,
    names: &Query<&Name>,
    kind: LookBoneKind,
) -> Option<Entity> {
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        if entity != root
            && names
                .get(entity)
                .ok()
                .is_some_and(|name| classify(name.as_str()) == Some(kind))
        {
            return Some(entity);
        }
        if let Ok(list) = children.get(entity) {
            stack.extend(list.iter());
        }
    }
    None
}

/// 骨骼名 → 看向链上的语义段。只认骨头，不认控制骨、辅助骨、末端标记。
fn classify(name: &str) -> Option<LookBoneKind> {
    // "mixamorig:Spine1" → "spine1"；"Armature|Spine" → "spine"
    let bare = name.rsplit([':', '|']).next().unwrap_or(name);
    let normalized: String = bare
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect();

    if normalized.contains("ctrl")
        || normalized.contains("helper")
        || normalized.contains("free")
        || normalized.contains("ik")
        || normalized.contains("twist")
        || normalized.contains("end")
        || normalized.contains("top")
    {
        return None;
    }

    match normalized.as_str() {
        "spine" => Some(LookBoneKind::Spine),
        "spine1" | "spine01" => Some(LookBoneKind::Spine1),
        // 上胸椎：Mixamo 叫 Spine2，其他 rig 常见 Chest / UpperChest
        "spine2" | "spine02" | "chest" | "upperchest" => Some(LookBoneKind::Spine2),
        "neck" => Some(LookBoneKind::Neck),
        "head" => Some(LookBoneKind::Head),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    /// 收敛断言用宽松容差：指数趋近永远不会「正好」到目标
    fn near(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn distribute_is_proportional_without_caps() {
        let weights = [0.5, 0.5];
        let caps = [100.0, 100.0];
        let mut out = [0.0; 2];
        distribute(40.0, &weights, &caps, &mut out);
        assert!(close(out[0], 20.0) && close(out[1], 20.0), "{out:?}");
    }

    #[test]
    fn distribute_redistributes_overflow() {
        // 第一根只能吃 5°，剩下的 35° 全给第二根
        let weights = [0.5, 0.5];
        let caps = [5.0, 100.0];
        let mut out = [0.0; 2];
        distribute(40.0, &weights, &caps, &mut out);
        assert!(close(out[0], 5.0), "{out:?}");
        assert!(close(out[1], 35.0), "{out:?}");
    }

    #[test]
    fn distribute_never_exceeds_chain_caps() {
        let weights = [0.08, 0.14, 0.20];
        let caps = [8.0, 10.0, 14.0];
        let mut out = [0.0; 3];
        distribute(200.0, &weights, &caps, &mut out);
        assert!(close(out[0], 8.0) && close(out[1], 10.0) && close(out[2], 14.0));
    }

    #[test]
    fn distribute_keeps_sign() {
        let weights = [0.5, 0.5];
        let caps = [100.0, 100.0];
        let mut out = [0.0; 2];
        distribute(-40.0, &weights, &caps, &mut out);
        assert!(close(out[0], -20.0) && close(out[1], -20.0), "{out:?}");
    }

    #[test]
    fn soft_limit_passes_inside_knee_and_caps_outside() {
        assert!(close(soft_limit(10.0, 35.0, 40.0, 0.7), 10.0));
        assert!(close(soft_limit(100.0, 35.0, 40.0, 0.7), 40.0));
        assert!(close(soft_limit(-100.0, 35.0, 40.0, 0.7), -35.0));
        // 软区里已经压扁：34° 的输入不再等比例输出
        assert!(soft_limit(32.0, 35.0, 40.0, 0.7) < 32.0);
        assert!(soft_limit(50.0, 35.0, 40.0, 0.7) == 40.0);
    }

    #[test]
    fn classify_mixamo_names() {
        assert_eq!(classify("mixamorig:Spine"), Some(LookBoneKind::Spine));
        assert_eq!(classify("mixamorig:Spine1"), Some(LookBoneKind::Spine1));
        assert_eq!(classify("mixamorig:Spine2"), Some(LookBoneKind::Spine2));
        assert_eq!(classify("mixamorig:Neck"), Some(LookBoneKind::Neck));
        assert_eq!(classify("mixamorig:Head"), Some(LookBoneKind::Head));
        assert_eq!(classify("mixamorig:HeadTop_End"), None);
        assert_eq!(classify("Ctrl_Spine2"), None);
        assert_eq!(classify("Hips_Free_Helper"), None);
    }

    #[test]
    fn camera_drive_engages_only_while_orbit_lock_is_held() {
        let mut app = App::new();
        app.init_resource::<LookSettings>()
            .insert_resource(CameraFacing {
                yaw: std::f32::consts::FRAC_PI_2, // 镜头绕到角色右侧
                pitch: 0.0,
            })
            .insert_resource(OrbitLock::default())
            .add_systems(Update, drive_look_from_camera);

        // `FacingState` 是角色身上的组件（取第一个角色），这里和模型放同一个实体
        let model = app
            .world_mut()
            .spawn((
                AnimModelRoot,
                FacingState::default(),
                LookAt::default(),
            ))
            .id();
        app.update();
        assert_eq!(
            app.world().get::<LookAt>(model).unwrap().weight,
            0.0,
            "没按 Ctrl 时镜头怎么转都不该驱动上半身"
        );

        // 按住 Ctrl：看向镜头的朝向（+90 侧绕 → 视线朝 +X → 头向左拧 90°）
        app.world_mut().resource_mut::<OrbitLock>().held = true;
        app.update();
        let look = *app.world().get::<LookAt>(model).unwrap();
        assert_eq!(look.weight, 1.0);
        assert!(close(look.yaw, 90.0), "{}", look.yaw);

        // 松开：权重回 0，角度留着（淡出期间不跳）
        app.world_mut().resource_mut::<OrbitLock>().held = false;
        app.update();
        let look = *app.world().get::<LookAt>(model).unwrap();
        assert_eq!(look.weight, 0.0);
        assert!(close(look.yaw, 90.0));
    }

    #[test]
    fn camera_angles_use_the_view_direction_not_the_camera_position() {
        let camera = |yaw_deg: f32| CameraFacing {
            yaw: yaw_deg.to_radians(),
            pitch: 0.3,
        };

        // 镜头绕到角色右侧 90°：视线指向 +X → 头向左拧 90°（不是回头对着机位）
        let (yaw, pitch) = camera_look_angles(&camera(90.0), 0.0);
        assert!(close(yaw, 90.0), "{yaw}");
        // 机位在上、视线朝下 → 跟着低头
        assert!(near(pitch, -17.19, 0.01), "{pitch}");

        // 镜头绕到正前方：视线朝 -Z → 头要拧 180°（再被 120° 上限夹住）
        let (yaw, _) = camera_look_angles(&camera(180.0), 0.0);
        assert!(yaw.abs() > 179.0, "{yaw}");
    }

    #[test]
    fn world_dir_roundtrip() {
        // 角色朝 +Z：正前方 → 0/0
        let front = LookAt::from_world_dir(Vec3::Z, 0.0);
        assert!(close(front.yaw, 0.0) && close(front.pitch, 0.0));
        // 左侧（+X）→ +90 偏航
        let left = LookAt::from_world_dir(Vec3::X, 0.0);
        assert!(close(left.yaw, 90.0), "{}", left.yaw);
        // 抬头
        let up = LookAt::from_world_dir(Vec3::new(0.0, 1.0, 1.0), 0.0);
        assert!(close(up.pitch, 45.0), "{}", up.pitch);
    }

    /// 在最小 App 里搭一条 Mixamo 形状的链，返回每根骨骼的局部偏航 / 俯仰
    fn look_world(request: LookAt, frames: usize) -> Vec<(LookBoneKind, f32, f32)> {
        look_world_with(request, frames, LookSettings::default())
            .into_iter()
            .map(|(kind, rotation)| {
                // 纯偏航是 (0, sin θ/2, 0, cos θ/2)，纯俯仰是 (sin -θ/2, 0, 0, cos θ/2)
                let yaw = 2.0 * rotation.y.atan2(rotation.w).to_degrees();
                let pitch = -2.0 * rotation.x.atan2(rotation.w).to_degrees();
                (kind, yaw, pitch)
            })
            .collect()
    }

    /// 和 [`look_world`] 同一套装置，但返回每根骨骼的**局部**旋转四元数，
    /// 方便在世界空间里验证「头到底朝哪」（局部角度相加在两个轴同时转时并不等价）。
    fn look_world_with(
        request: LookAt,
        frames: usize,
        settings: LookSettings,
    ) -> Vec<(LookBoneKind, Quat)> {
        use std::time::Duration;

        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .insert_resource(settings)
            .add_systems(Update, apply_look);

        let world = app.world_mut();
        let root = world
            .spawn((Name::new("model"), request, LookState::default()))
            .id();
        // Armature（Mixamo 的 Z-up 转换节点）→ Hips → 脊椎链
        let armature = world
            .spawn((Name::new("Armature"), Transform::default(), ChildOf(root)))
            .id();
        let hips = world
            .spawn((
                Name::new("mixamorig:Hips"),
                Transform::default(),
                ChildOf(armature),
            ))
            .id();
        let mut parent = hips;
        let mut bones = Vec::new();
        for (kind, name) in [
            (LookBoneKind::Spine, "mixamorig:Spine"),
            (LookBoneKind::Spine1, "mixamorig:Spine1"),
            (LookBoneKind::Spine2, "mixamorig:Spine2"),
            (LookBoneKind::Neck, "mixamorig:Neck"),
            (LookBoneKind::Head, "mixamorig:Head"),
        ] {
            parent = world
                .spawn((Name::new(name), Transform::default(), ChildOf(parent)))
                .id();
            bones.push((kind, parent));
        }

        let step = Duration::from_secs_f32(1.0 / 60.0);
        for _ in 0..frames {
            // 真实管线里 `drive_pose` 每帧都会把骨骼重写成动画姿态，
            // 看向层只叠加；这里要模拟这一步，否则偏航会一帧一帧累上去
            for (_, entity) in &bones {
                *app.world_mut().get_mut::<Transform>(*entity).unwrap() = Transform::default();
            }
            app.world_mut().resource_mut::<Time>().advance_by(step);
            app.update();
        }

        bones
            .into_iter()
            .map(|(kind, entity)| {
                (kind, app.world().get::<Transform>(entity).unwrap().rotation)
            })
            .collect()
    }

    #[test]
    fn chain_yaw_sums_to_input_and_never_exceeds_caps() {
        // 1 秒足够两条通道都收敛
        let bones = look_world(LookAt::new(65.0, 0.0), 240);
        let total: f32 = bones.iter().map(|(_, yaw, _)| *yaw).sum();
        assert!(near(total, 65.0, 0.5), "总偏航 {total}，明细 {bones:?}");

        for (kind, yaw, _) in &bones {
            let cap = LOOK_BONES[kind.index()].yaw_cap;
            assert!(
                yaw.abs() <= cap + 0.2,
                "{kind:?} 转了 {yaw}，超过上限 {cap}"
            );
        }
        // 每一节都要真的参与，不能只有脖子在动
        for (kind, yaw, _) in &bones {
            assert!(
                yaw.abs() > 0.5,
                "{kind:?} 只转了 {yaw}°，脊柱链没参与"
            );
        }
        // 腰椎几乎不能拧，胸椎是转身主力，颈椎补剩下的一半
        let lumbar = bones
            .iter()
            .find(|(kind, _, _)| *kind == LookBoneKind::Spine)
            .unwrap()
            .1;
        assert!(lumbar.abs() < 8.5, "腰椎转了 {lumbar}");
        let spine: f32 = bones
            .iter()
            .filter(|(kind, _, _)| {
                matches!(
                    *kind,
                    LookBoneKind::Spine | LookBoneKind::Spine1 | LookBoneKind::Spine2
                )
            })
            .map(|(_, yaw, _)| *yaw)
            .sum();
        assert!(
            spine.abs() > 20.0,
            "躯干只拧了 {spine}°，看起来会像只有脖子在扭"
        );
        let cervical: f32 = bones
            .iter()
            .filter(|(kind, _, _)| matches!(*kind, LookBoneKind::Neck | LookBoneKind::Head))
            .map(|(_, yaw, _)| *yaw)
            .sum();
        assert!(near(cervical, 65.0 * 0.50, 1.0), "颈椎合计 {cervical}，明细 {bones:?}");
    }

    /// 镜头绕到正后方时，肩膀要真的跟着拧过去，不能停在 90°——
    /// 这是「链的总容量必须罩住配置上限」的回归测试。
    #[test]
    fn yaw_reaches_behind_with_the_whole_chain() {
        let bones = look_world(LookAt::new(180.0, 0.0), 240);
        let total: f32 = bones.iter().map(|(_, yaw, _)| *yaw).sum();
        assert!(
            near(total, 120.0, 1.0),
            "镜头在正后方时只拧到 {total}°，明细 {bones:?}"
        );

        for (kind, yaw, _) in &bones {
            let cap = LOOK_BONES[kind.index()].yaw_cap;
            assert!(
                yaw.abs() <= cap + 0.2,
                "{kind:?} 转了 {yaw}，超过解剖上限 {cap}"
            );
        }

        // 胸口相对髋至少要拧过 40°：真人回头看是这个量级，全靠脖子会被看出假
        let torso: f32 = bones
            .iter()
            .filter(|(kind, _, _)| {
                matches!(
                    *kind,
                    LookBoneKind::Spine | LookBoneKind::Spine1 | LookBoneKind::Spine2
                )
            })
            .map(|(_, yaw, _)| *yaw)
            .sum();
        assert!(torso.abs() >= 40.0, "躯干只拧了 {torso}°");
        assert!(torso.abs() <= 60.5, "躯干拧了 {torso}°，超过胸椎合计上限");

        // 链的容量一定 >= 默认限幅，否则 distribute 会静态截断
        let capacity: f32 = LOOK_BONES.iter().map(|p| p.yaw_cap).sum();
        assert!(
            capacity >= LookSettings::default().yaw_left,
            "总容量 {capacity}° 罩不住默认上限"
        );
    }

    /// 大角度回头要带一点低头；小角度必须干净，否则平时侧看也会一直低着头。
    /// 局部角度在「偏航 + 俯仰」同时存在时不能简单相加，所以这里验的是
    /// **头在世界空间里的朝向**（局部四元数沿链累乘）。
    #[test]
    fn large_yaw_dips_the_view_a_little() {
        let head_forward = |yaw: f32, per_deg: f32| -> Vec3 {
            let mut settings = LookSettings::default();
            settings.yaw_dip_per_deg = per_deg;
            let chain = look_world_with(LookAt::new(yaw, 0.0), 240, settings);
            let world_rotation = chain
                .iter()
                .fold(Quat::IDENTITY, |acc, (_, rotation)| acc * *rotation);
            world_rotation * Vec3::Z
        };

        // 阈值内：纯偏航，视线水平
        assert!(head_forward(30.0, 0.15).y.abs() < 0.01);
        assert!(head_forward(50.0, 0.15).y.abs() < 0.01);

        // 65°：只带一点点（2.7° → y ≈ -0.047）
        let small = head_forward(65.0, 0.18).y;
        assert!(near(small, -0.047, 0.01), "65° 的低头量 {small}");

        // 120°：明显低头（12.6° → y ≈ -0.22），视线落到肩后偏低处
        let big = head_forward(120.0, 0.18).y;
        assert!(near(big, -0.218, 0.02), "120° 的低头量 {big}");
        assert!(big < small, "角度越大低头越多");

        // 关掉耦合：同一姿态下视线必须回到水平——证明低头是这个耦合造成的
        let flat = head_forward(120.0, 0.0).y;
        assert!(flat.abs() < 0.01, "关掉耦合后仍有 {flat} 的俯仰");
    }

    #[test]
    fn pitch_up_is_positive_and_down_uses_its_own_cap() {
        let up = look_world(LookAt::new(0.0, 45.0), 240);
        let total: f32 = up.iter().map(|(_, _, pitch)| *pitch).sum();
        assert!(
            near(total, 40.0, 0.5),
            "抬头上限应压到 40°，实际 {total}，{up:?}"
        );
        assert!(total > 0.0, "抬头必须朝上");

        let down = look_world(LookAt::new(0.0, -45.0), 240);
        let total: f32 = down.iter().map(|(_, _, pitch)| *pitch).sum();
        assert!(near(total, -35.0, 0.5), "低头上限应压到 35°，实际 {total}");

        // 屈伸主要发生在颈段：抬头 40° 时脊柱组最多贡献 ~9°（否则就是整个人往后折）
        let spine: f32 = up
            .iter()
            .filter(|(kind, _, _)| {
                matches!(
                    *kind,
                    LookBoneKind::Spine | LookBoneKind::Spine1 | LookBoneKind::Spine2
                )
            })
            .map(|(_, _, pitch)| *pitch)
            .sum();
        assert!(spine < 12.0, "抬头时脊柱贡献了 {spine}°，躯干折太多");
        assert!(spine > 0.0, "脊柱完全不参与也不自然：{spine}");
    }
}
