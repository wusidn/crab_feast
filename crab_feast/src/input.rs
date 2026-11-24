use avian3d::prelude::{LinearVelocity, Position};
use bevy::picking::pointer::PointerId;
use bevy::platform::collections::HashSet;
use bevy::prelude::*;
use crab_feast_library::air::{air_steer_active, air_steer_velocity};
use serde::{Deserialize, Serialize};

use crate::anim::{JumpAction, JumpSettings, PoseDriver};
use crate::camera::CameraFacing;

/// 控制链的执行顺序。
///
/// 相机先定机位，移动系统才知道「摇杆往上推」对应世界里的哪个方向；
/// 角色朝向在移动意图之后算，动画系统再拿本帧的朝向误差去选转向姿态。
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ControlSet {
    /// 轨道相机：把 look 输入摆成机位，并公布 [`CameraFacing`]
    Camera,
    /// 摇杆输入（相对相机）→ 世界空间线速度
    Move,
    /// 角色朝向限速追目标（延迟跟随）
    Facing,
}

#[derive(Resource, Debug, Clone, Copy, Reflect, Default)]
pub enum MovementInput {
    #[default]
    Idle,
    Activated {
        direction: Vec2,
        force: f32,
    },
}

#[derive(Event)]
pub struct LookInput(pub Vec2);

/// 这几根指头**不转视角**：摇杆按住的那根、左半屏按下的那根。
///
/// 由 UI 输入层写（摇杆激活 / 左半屏落点），双指缩放也读它——
/// 「左手走路 + 右手看路」时相机距离不能跟着变。
#[derive(Resource, Debug, Default)]
pub struct LookInputIgnorePointers(pub(crate) HashSet<PointerId>);

impl LookInputIgnorePointers {
    /// 这根指头是否归摇杆 / 左半屏管
    pub(crate) fn contains(&self, pointer: PointerId) -> bool {
        self.0.contains(&pointer)
    }
}

#[derive(Resource, Debug, Clone, Copy, Reflect, Default)]
pub enum JumpInput {
    #[default]
    Idle,
    Activated,
}

/// 角色移动控制器。
///
/// `speed` 是**目标地速（米/秒）**，由动画系统按当前混合姿态写入
/// （见 anim/locomotion.rs 里的 sync_move_speed）：剪辑的净步幅与时长已经算出
/// 脚底真实走过的距离，物理端照抄这个值才不会滑步。
#[derive(Component)]
pub struct MovementController {
    pub speed: f32,
}

/// 角色朝向控制器：相机转走后角色不瞬移，按最大角速度追向目标朝向。
///
/// 三个量分工明确：
/// - `turn_speed` 是匀速段的上限，决定“转得最快能多快”；
/// - `turn_accel` 是角加速度上限，决定“从静止到最快要用多久”——没有它，
///   180° 反向的第一帧角速度就从 0 跳到上限，看着像原地瞬移朝向；
/// - 比例收尾（[`FOLLOW_RESPONSE`]）决定最后几十度收得多干脆。
///
/// 现在这组（90°/s、350°/s²）是「身体有重量、但不会拖到失控」的节奏：
/// 180° 反向约 2.2 秒，90° 约 1.2 秒，全程边走边转。角加速度按 180/700 的原始比例跟着走，
/// 起步段恒定 0.26 秒走 11.6°，不管顶速调快调慢，起步形状都不变。
/// 往上加就是更快更"机械"，往下减就是更慢更沉。历史值：180°/s、700°/s² → 1.26 秒；
/// 30°/s、120°/s² → 6.1 秒（太沉，已否决）；110°/s/无加速度 → 1.75 秒（拖沓）；
/// 450°/s/3000°/s² → 0.63 秒（节奏对但观感偏假，已被否决）。
#[derive(Component)]
pub struct FacingController {
    /// 最大转向角速度（度/秒）
    pub turn_speed: f32,
    /// 最大转向角加速度（度/秒²）
    pub turn_accel: f32,
    /// 角度差小于它就认为已经对准，避免在阈值附近抖
    pub tolerance: f32,
}

impl Default for FacingController {
    fn default() -> Self {
        Self {
            // 数值只写在 crab_feast_library::tuning 一处：服务器做朝向复制要用同一组，
            // 两边分家就会「本地转 2.2 秒、远端转 0.5 秒」
            turn_speed: crab_feast_library::tuning::TURN_SPEED_DEG_PER_SEC,
            turn_accel: crab_feast_library::tuning::TURN_ACCEL_DEG_PER_SEC2,
            tolerance: crab_feast_library::tuning::TURN_TOLERANCE_DEG,
        }
    }
}

impl FacingController {
    /// 按「脚在地上」权重缩放后的顶速：腾空期打 [`AIR_TURN`] 折。
    ///
    /// `support` = 0 表示双脚腾空（跑步 57% 的时间），1 表示至少一只脚踩实。
    pub fn turn_speed_at(&self, support: f32) -> f32 {
        let support = support.clamp(0.0, 1.0);
        self.turn_speed * (AIR_TURN + (1.0 - AIR_TURN) * support)
    }

    /// 按「脚在地上」权重缩放后的角加速度：腾空期几乎不许再加速（空中没有地面反力）
    pub fn turn_accel_at(&self, support: f32) -> f32 {
        let support = support.clamp(0.0, 1.0);
        self.turn_accel * (AIR_ACCEL + (1.0 - AIR_ACCEL) * support)
    }
}

/// 角色本帧的朝向状态，由 [`facing_system`] 写、动画系统读。
#[derive(Component, Clone, Copy)]
pub struct FacingState {
    /// 当前朝向（世界空间弧度，见 [`yaw_of`]）
    pub yaw: f32,
    /// 当前角速度（度/秒，带符号）。跨帧记住它才能做角加速度限制
    pub yaw_rate: f32,
    /// 目标与当前的带符号夹角（度）。正 = 需要绕 +Y 转，即向左转
    pub error_deg: f32,
    /// 低通过的「脚在地上」权重（0 = 双脚腾空，1 = 至少一只脚踩实）。
    /// 转向的顶速和角加速度按它缩放：腾空期没有地面反力，不该越转越快
    pub support: f32,
}

impl Default for FacingState {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            yaw_rate: 0.0,
            error_deg: 0.0,
            // 一上来按踩实算：别让刚生成的角色头 80ms 转不动
            support: 1.0,
        }
    }
}

/// 键盘的走路 / 跑步档位。
///
/// WASD 默认就是跑；按一下 Shift 切到走，再按一下切回跑——不是按住才跑。
/// 档位值直接对应混合空间的 `speed` 轴采样点：走 0.5、跑 1.0，
/// 别用中间值（0.75 那种会把 Walking 和 Running 混在一起，脚会飘）。
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct MoveGait {
    pub running: bool,
}

impl MoveGait {
    /// 走档对应的 `speed` 轴值（Walking 的采样点）
    pub const WALK: f32 = 0.5;
    /// 跑档对应的 `speed` 轴值（Running 的采样点）
    pub const RUN: f32 = 1.0;

    pub fn speed(&self) -> f32 {
        if self.running { Self::RUN } else { Self::WALK }
    }

    pub fn toggle(&mut self) {
        self.running = !self.running;
    }
}

impl Default for MoveGait {
    fn default() -> Self {
        // 默认跑：玩家一推杆就该跑起来，不用一直按着 Shift
        Self { running: true }
    }
}

/// 本帧的移动意图：世界空间单位方向 + 档位量级（0..1）+ 转身降速系数（0..1）。
///
/// 摇杆给的是「相对相机」的输入，这里已经换算成世界方向；
/// 朝向系统与动画系统都读它，不再各自解释一遍摇杆坐标。
///
/// `speed_scale` 是「转身降速」：朝向误差大时按
/// [`crab_feast_library::tuning::turn_speed_scale`] 压到 0。它**不**进
/// `magnitude`——`magnitude` 是玩家推了多少杆，朝向系统靠它判断「还要不要转」，
/// 压成 0 会让身体停止追方向，180° 就永远转不过来了。地速与动画档位用的都是
/// `magnitude * speed_scale`。
#[derive(Component, Clone, Copy, Debug)]
pub struct LocomotionIntent {
    pub direction: Vec3,
    pub magnitude: f32,
    pub speed_scale: f32,
}

impl Default for LocomotionIntent {
    fn default() -> Self {
        Self {
            direction: Vec3::ZERO,
            magnitude: 0.0,
            // 没有输入时地速本来就是 0，降速系数保持中性
            speed_scale: 1.0,
        }
    }
}

/// 本帧**实测**的平面位移速度（世界系，米/秒）。
///
/// 混合空间 `yaw` 轴要的是「人实际往哪挪」而不是「玩家想往哪推」，单机角色从物理
/// [`LinearVelocity`] 就能读到。联机角色的位置由服务端快照直接写进 `Transform`，
/// 又是 `Kinematic`（Avian 不碰非动态刚体的组件），物理速度恒为 0——不补这一份，
/// 混合空间永远停在 `yaw = 0` 的正前方那一列：身体在转、腿还在往前跑，支撑脚只能
/// 靠脚掌偏航锁硬扛，看起来就是「转向时一只脚往外拐」。
#[derive(Component, Default, Clone, Copy, Debug)]
pub struct PlanarMotion(pub Vec2);

/// 角色朝向三件套：挂上就进入「相机先转、角色限速追」这套控制。
///
/// 打成一个 bundle 是因为 Bevy 的元组 Bundle 实现到 15 个为止，
/// 玩家实体上的组件已经快把那个名额用完了。
#[derive(Bundle, Default)]
#[cfg_attr(feature = "net", allow(dead_code))]
pub struct Facing {
    pub controller: FacingController,
    pub state: FacingState,
    pub intent: LocomotionIntent,
}

/// 「转身降速」的斜坡状态（比例/秒限幅）。
///
/// 夹角是**瞬时**的：摇杆一甩，误差当场就是 180°，直接拿曲线乘速度会一帧掉到
/// 下限，观感是被拉住而不是被惯性带走。这里按
/// [`crab_feast_library::tuning::ramp_turn_brake`] 把它滑下去、再滑回来，
/// 单机 / 预测端 / 服务端每 tick 推同一格，落点才不会各走一条。
///
/// 放资源而不是组件：客户端只有本机一个受控角色（单机一份、联机一份预测体）。
#[derive(Resource)]
pub struct TurnBrakeRamp(pub f32);

impl Default for TurnBrakeRamp {
    fn default() -> Self {
        Self(1.0)
    }
}

/// 输入平滑的跨帧状态：对**移动输入的朝向**（键盘 WASD / 摇杆）做一阶滞后。
///
/// 系数本身来自 [`MovementSettings`]，这里只存「上一次平滑到哪儿了」，
/// 所以开关与系数改动立刻生效，不需要清状态。
#[derive(Resource, Default)]
pub struct InputSmoothing {
    /// 上一帧平滑后的输入方向（弧度，相对相机）。`None` = 还没历史，第一次直接取目标
    pub local_angle: Option<f32>,
    /// 摇杆扫过中心时要沿用的上一段输入（见 [`StickHold`]）
    pub hold: Option<StickHold>,
}

/// 平滑系数下限：0 时 alpha=0，输入会冻在上一帧的值上（推杆后像卡住不动）
pub const SMOOTHING_MIN: f32 = 0.05;

/// 摇杆量级低于这个，就当「手还按着、但正从一边扫到另一边」。
///
/// 手机上是虚拟摇杆：想掉头只能把拇指从一边拖到另一边，中间必然经过圆心。
/// 那几帧的量级会掉到 0（甚至正好落在圆心），如果照「没输入」处理，人会先
/// 站一拍、再瞬间切到反方向跑，脚步就全乱。所以这个区间沿用上一段的朝向与
/// 量级，最多 [`STICK_SWEEP_SECS`]。
const STICK_SWEEP_MAG: f32 = 0.35;

/// 扫过中心的保持窗口（秒）。正常一次拖过圆心是 60~120ms，给到 0.25s 足够；
/// 停这么久的「停在中间」仍然会被当成没在走。
const STICK_SWEEP_SECS: f32 = 0.25;

/// 到底算不算「在走」的量级下限：低于它且不在保持窗口里 → 停下
const STICK_REST_MAG: f32 = 0.05;

/// 扫过摇杆中心时沿用的上一段输入（朝向 + 摇杆偏转 + force + 已经保持了多久）
#[derive(Clone, Copy, Debug, Default)]
pub struct StickHold {
    angle: f32,
    /// 摇杆偏转量（`direction.length()`，0~1）
    deflection: f32,
    force: f32,
    age: f32,
}

/// 这一帧**生效**的移动输入（摇杆扫过中心的保持已经算完了）。
///
/// `movement_system` 与上行发包都读它，不能各自再读原始摇杆：
/// 拇指扫过圆心那几帧客户端还在跑，服务端若按「原始摇杆 ≈ 0」处理就会当成没输入，
/// 两端立刻错开一整段减速——这正是要避免的抖。
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct EffectiveMove {
    /// 屏幕空间单位方向；零 = 没在走
    pub direction: Vec2,
    /// 摇杆偏转量（0~1）
    pub deflection: f32,
    /// 摇杆 force（和偏转一起决定动画量级与上行语义）
    pub force: f32,
}

impl EffectiveMove {
    /// 这一帧到底在不在走
    pub fn is_active(&self) -> bool {
        self.magnitude() > STICK_REST_MAG
    }

    /// 动画量级（混合空间的 speed 轴）= 偏转 × force
    pub fn magnitude(&self) -> f32 {
        (self.deflection * self.force).clamp(0.0, 1.0)
    }

    /// 上行用的屏幕空间摇杆向量（长度 = 偏转量），与服务端 `MoveInput.dir/force` 同义
    pub fn local_stick(&self) -> Vec2 {
        self.direction * self.deflection
    }
}

/// 移动设置：控制输入平滑的开启/关闭及参数
///
/// 这两个值来自「移动设置」菜单页，并落在 `<assets>/settings/movement.json`。
#[derive(Resource, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MovementSettings {
    /// 是否开启输入平滑
    pub smoothing_enabled: bool,
    /// 平滑系数（0.05~1），越小越平滑，响应越慢
    pub smoothing_factor: f32,
    /// 转身降速起点（度）：朝向误差小于它时地速不受影响
    pub turn_brake_start_deg: f32,
    /// 转身降速下限（0~1）：误差到 180° 时保留的地速比例，1.0 = 完全不降速
    pub turn_brake_min_scale: f32,
}

impl MovementSettings {
    /// 把系数夹回可用区间：0 会让看向彻底冻住（alpha=0），所以下限留一点
    pub fn sanitized(mut self) -> Self {
        self.smoothing_factor = self.smoothing_factor.clamp(SMOOTHING_MIN, 1.0);
        self.turn_brake_start_deg = self.turn_brake_start_deg.clamp(
            crab_feast_library::tuning::TURN_BRAKE_START_RANGE.0,
            crab_feast_library::tuning::TURN_BRAKE_START_RANGE.1,
        );
        self.turn_brake_min_scale = self.turn_brake_min_scale.clamp(
            crab_feast_library::tuning::TURN_BRAKE_MIN_RANGE.0,
            crab_feast_library::tuning::TURN_BRAKE_MIN_RANGE.1,
        );
        self
    }
}

impl Default for MovementSettings {
    fn default() -> Self {
        Self {
            smoothing_enabled: false,
            smoothing_factor: 0.5,
            turn_brake_start_deg: crab_feast_library::tuning::TURN_BRAKE_START_DEG_DEFAULT,
            turn_brake_min_scale: crab_feast_library::tuning::TURN_BRAKE_MIN_SCALE_DEFAULT,
        }
    }
}

/// 角色正前方在实体局部空间里的轴。
///
/// 不是 Bevy 默认的 -Z：本作的 Mixamo 模型面向 +Z。实测依据（`assets/characters`）：
/// Walking / Running 的髛部净位移沿 +Z，LeftToe_End 落在 +X（即右侧是 -X），
/// RightStrafe 往 -X 走 —— 三者自洽。
pub(crate) const FORWARD_LOCAL: Vec3 = Vec3::Z;

/// 世界方向 → 绕 Y 的偏航角，满足 `Quat::from_rotation_y(yaw) * FORWARD_LOCAL == direction`。
pub(crate) fn yaw_of(direction: Vec3) -> f32 {
    // v' = v·cosθ + (ŷ×v)·sinθ（水平向量），把 FORWARD_LOCAL 和它的右手垂线当正交基
    let forward = FORWARD_LOCAL;
    let right = Vec3::Y.cross(forward);
    direction.dot(right).atan2(direction.dot(forward))
}

/// 相机视线朝向换算成角色旋转约定的偏航角。
///
/// 移动和转向都用它，两个地方对「屏幕深处是哪个世界方向」的理解才不会分叉。
pub fn camera_view_yaw(camera: &CameraFacing) -> f32 {
    yaw_of(camera.view_forward())
}

/// 把角度折到 (-π, π]，用于求「最短转向」的带符号误差。
pub fn wrap_pi(angle: f32) -> f32 {
    (angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

#[allow(dead_code)]
pub enum LookAxis {
    Yaw,
    Pitch,
    YawAndPitch,
}

#[derive(Component)]
pub struct LookController {
    pub axis: LookAxis,
    pub accumulated_yaw: f32,
    pub accumulated_pitch: f32,
}

impl Default for MovementController {
    fn default() -> Self {
        // 动画系统接管前的默认档位：取跑的量级，避免模型还在加载时一推摇杆就飞出去
        Self { speed: 3.0 }
    }
}

impl Default for LookController {
    fn default() -> Self {
        Self {
            axis: LookAxis::YawAndPitch,
            accumulated_yaw: 0.0,   // 0 = 相机在角色背后（-Z 方向）
            accumulated_pitch: 0.0, // 水平视线
        }
    }
}

pub struct ControlInputPlugin;

impl Plugin for ControlInputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MovementInput>()
            .init_resource::<JumpInput>()
            // 空中操控的力度/窗口是移动系统的入参，所以在这里也要保证资源存在
            // （正常流程里 `JumpPlugin` 也会 init，重复初始化是幂等的）
            .init_resource::<JumpSettings>()
            .init_resource::<InputSmoothing>()
            .init_resource::<EffectiveMove>()
            .init_resource::<TurnBrakeRamp>()
            // 哪些指头不许转视角：UI 输入层写，触摸缩放读
            .init_resource::<LookInputIgnorePointers>()
            // 三个集合的先后顺序在 ScenePlugin 里配：相机系统属于 Camera 段，
            // 整条链还要接着排到动画驱动之前，放在接线处比在这里声明一半清楚
            // 先把原始摇杆折算成「这一帧生效的输入」（扫过中心的保持在这里做），
            // 移动系统和上行发包都读它
            .add_systems(Update, steady_stick.before(ControlSet::Move))
            .add_systems(Update, movement_system.in_set(ControlSet::Move))
            .add_systems(Update, facing_system.in_set(ControlSet::Facing))
            .add_systems(Update, jump_keyboard.in_set(ControlSet::Move))
            .add_observer(look_system);

        // TEMP 诊断：`CRAB_AUTO_TURN=<秒>` 自己每 N 秒把摇杆方向翻 180°，
        // 用来在 PC / 手机上复现「连续甩头 → 两端位置漂移」。不设环境变量就不装。
        if std::env::var("CRAB_AUTO_TURN").is_ok() {
            app.init_resource::<AutoTurnDiag>()
                .add_systems(Update, auto_turn_diag.before(steady_stick));
        }
        if std::env::var("CRAB_AUTO_WALK").is_ok() {
            app.init_resource::<AutoWalkDiag>()
                .add_systems(Update, auto_walk_diag.before(steady_stick));
        }
        // 帧时间注入抖动，单独一个开关：不依赖自动直线跑，也能配合手搓输入用。
        if std::env::var("CRAB_FRAME_JITTER").is_ok() {
            app.init_resource::<FrameJitterDiag>()
                .add_systems(Last, frame_jitter_diag);
        }
    }
}

/// 把「目标地速 + 输入方向」写成线速度。
///
/// 摇杆方向现在是**相对相机**的：上 = 屏幕深处。绕世界 Y 旋到相机偏航就是世界方向，
/// 于是转完相机再推摇杆，角色朝画面里的那个方向走，而不是沿自己刚才的脸向。
///
/// 大小不再乘摇杆量级：`force` 已经作为档位写进混合空间，混合出来的姿态本身就
/// 带了这个快慢，再乘一次会变成「半推摇杆、脚却按走路频率跑两倍远」的滑步。
///
/// 「输入平滑」调的是**转向手感**，所以只对输入的**方向**做一阶低通（走最短弧），
/// 再换算成世界方向；档位量级原样透传（那是起步加速手感，另一个旋钮）。
/// 滤在输入端而不是转向端，是为了只滤一次——下游 [`facing_system`] 追的就是这份
/// 已经平滑过的方向，不会再叠一层滞后。
fn movement_system(
    mut movers: Query<(
        &mut LinearVelocity,
        &MovementController,
        &mut LocomotionIntent,
        // 转身降速要看「身体还差多少没转过来」。没有朝向状态的实体（编辑器预览、
        // 单测最小 App）按 1.0 放行，不强行拦住人
        Option<&FacingState>,
        Option<&Position>,
    )>,
    camera: Res<CameraFacing>,
    settings: Res<MovementSettings>,
    time: Res<Time>,
    jump: Option<Res<JumpAction>>,
    jump_settings: Res<JumpSettings>,
    mut smoothing: ResMut<InputSmoothing>,
    // 转身降速的斜坡：刹得住惯性、也要能自己爬回来
    mut turn_brake: ResMut<TurnBrakeRamp>,
    // 这一帧生效的摇杆输入（扫过中心的保持已经在 `steady_stick` 里算过）
    effective: Res<EffectiveMove>,
) {
    // 滞空期不再「只改朝向」：完全锁死落点太苛刻（起跳角差 10° 就差出半个方块），
    // 但也不能给满。折中成**保动量、限转向**：
    //
    // - 松手：不刹车，起跳那一帧的水平动量原样留到落地（绝不把落点往前拽）；
    // - 推杆：以有限加速度把水平速度**转**向输入方向，速度大小不变（不加速、不减速），
    //   落点只是被有限地挪动；
    // - 起跳后 `air_steer_secs` 一过就彻底不接管，最后 0.2 秒落点定死
    //   （否则触地判定的位置会被输入抖动带偏）。
    //
    // 这条规则单机、预测端、服务端三处必须完全一致（服务端见
    // `launcher/server/src/main.rs` 的 `drive_avatars`，两边共用
    // `crab_feast_library::net::air_steer_velocity`）：任何一端多给一点空中控制，
    // 预测和服务端就会各落一处，和解系统只好一直往回拽人。
    let airborne = jump.as_ref().is_some_and(|jump| jump.is_airborne());
    let air_elapsed = jump.as_ref().map_or(0.0, |jump| jump.air_elapsed());
    let dt = time.delta_secs();
    let effective = *effective;
    if std::env::var("CRAB_LOG_SPEED").is_ok() {
        info!("speed/frame: active={} airborne={} dt={:.3}",
            effective.is_active(), airborne, dt);
    }
    let Some((target_angle, magnitude)) = effective
        .is_active()
        .then(|| (effective.direction.to_angle(), effective.magnitude()))
    else {
        // 没有有效输入就停下：不能只靠线性阻尼慢慢漂，否则动画已经切回 Idle 了人还在滑。
        // 方向状态一并清掉，免得下次推杆还从上一次的角度慢慢摆过来
        smoothing.local_angle = None;
        // 松手：降速斜坡自己回满，下次起步不必从「还在刹车」的格子里开始
        turn_brake.0 = crab_feast_library::tuning::ramp_turn_brake(turn_brake.0, 1.0, 0.0, dt);
        for (mut velocity, _, mut intent, _, _) in movers.iter_mut() {
            // 空中松手：意向归零（身体不再追任何方向），水平动量留到落地。
            // 这里绝不能顺手把速度写成 0：那等于在空中把落点往回拽
            *intent = LocomotionIntent::default();
            if !airborne {
                velocity.0.x = 0.0;
                velocity.0.z = 0.0;
            }
        }
        return;
    };

    // 系数越小，输入越慢跟上按键；关闭平滑时 alpha = 1，等价于直接采用本帧输入
    let alpha = smoothing_alpha(
        settings.smoothing_enabled,
        settings.smoothing_factor,
        time.delta_secs(),
    );

    // 方向低通走最短弧：从「前」摆到「后」不会绕一圈
    let angle = match smoothing.local_angle {
        Some(prev) => approach_yaw(prev, target_angle, alpha),
        // 第一次有输入时没有历史，直接对齐，免得一推杆角色先向某个固定方向偏一下
        None => target_angle,
    };
    smoothing.local_angle = Some(angle);

    let smoothed_local = Vec2::from_angle(angle);

    // 摇杆的 y 是屏幕上下，不是世界的竖直轴：直接 extend 会把前后量丢到 Y 上，
    // 于是只剩左右能动。这里拆成「屏幕深处 + 屏幕右」两个世界方向重组
    let forward = camera.view_forward();
    let right = forward.cross(Vec3::Y);
    let direction = (right * smoothed_local.x + forward * -smoothed_local.y).normalize_or_zero();
    if std::env::var("CRAB_LOG_YAW").is_ok() {
        info!(
            "dir-input: cam_yaw={:.1} fwd=({:.2},{:.2},{:.2}) stick=({:.2},{:.2}) world=({:.2},{:.2},{:.2})",
            camera.yaw.to_degrees(), forward.x, forward.y, forward.z,
            smoothed_local.x, smoothed_local.y, direction.x, direction.y, direction.z
        );
    }
    // 档位量级**不过滤波器**：它是「起步加速」的手感，和转向是两个旋钮，
    // 混在一起会让调系数的人分不清自己改的是转身还是加速。
    // 它与混合空间的 speed 轴同义：推满为 1。量级在上面已经算好（含扫中心的保持）

    // 转身降速：读**上一帧**的朝向误差（`facing_system` 在 ControlSet::Facing 跑，
    // 排在移动之后）。16ms 的滞后在 90°/s 的转速下是 1.5°，对曲线没有影响。
    // 单机、预测端、服务端用同一个函数（服务端见 `launcher/server/src/main.rs`），
    // 一端降速另一端不降，和解系统会一直把预测位置往回拽。
    for (mut velocity, movement_controller, mut intent, facing, pos) in movers.iter_mut() {
        // 意向先写：空中也照样写，身体朝向由 `facing_system` 按它转
        // （空中转速已经被 `AIR_TURN` 压过，能转但转不快）
        intent.direction = direction;
        intent.magnitude = magnitude;
        // 目标比例只看夹角，实际比例按时间斜坡逼近：甩头时先被惯性带走、慢慢刹到
        // 下限，转回来再慢慢提速。斜坡是纯时间的函数（每 tick 推一格），帧率不影响落点。
        let (target, turn_rate) = facing.map_or((1.0, 0.0), |f| {
            (
                crab_feast_library::tuning::turn_speed_scale_with(
                    f.error_deg,
                    settings.turn_brake_start_deg,
                    settings.turn_brake_min_scale,
                ),
                // 刹车力按本帧的转速给：身体还没转起来的那几帧不许刹
                f.yaw_rate,
            )
        });
        turn_brake.0 = crab_feast_library::tuning::ramp_turn_brake(
            turn_brake.0,
            target,
            turn_rate,
            time.delta_secs(),
        );
        intent.speed_scale = turn_brake.0;
        if airborne {
            // 空中：不写目标速度，只把**当前**水平速度转向输入方向——速度大小原样保留，
            // 所以「保动量」；转多快由空中操控加速度卡住，所以「限转向」。
            // 加速度按摇杆量级缩放：轻推摇杆转得慢，推满才是设置的满力度。
            if air_steer_active(air_elapsed, jump_settings.air_steer_secs) {
                let steered = air_steer_velocity(
                    [velocity.0.x, velocity.0.z],
                    [direction.x, direction.z],
                    jump_settings.air_accel * magnitude,
                    time.delta_secs(),
                );
                velocity.0.x = steered[0];
                velocity.0.z = steered[1];
            }
            continue;
        }
        // 地速方向取**这一帧的输入方向**，不是身体朝向：甩头 180° 时人是
        // 「刹住 → 倒退走 → 再转身」，不是沿一条弧把身体带过去。朝向和位移是
        // 两条独立的时间轴（朝向 90°/s 追，位移另有各套剪辑负责）。
        //
        // 反向那一下交给 `ground_velocity_step` 按有限加速度掰：先沿原方向
        // 刹到 0、再过零反向加速，所以看到的是惯性减速，不是一帧把速度翻过去。
        #[allow(unused_mut)]
        let mut target = direction * movement_controller.speed;
        if std::env::var("CRAB_LOG_SPEED").is_ok() {
            let err = facing.map_or(0.0, |f| f.error_deg);
            let yaw = facing.map_or(0.0, |f| f.yaw.to_degrees());
            let py = pos.map_or(0.0, |p| p.0.y);
            info!("speed: yaw={:.1}° ctrl={:.2} ramp={:.2} err={:.1} y={:.3} vy={:.2} target=({:.2},{:.2}) vel=({:.2},{:.2})",
                yaw, movement_controller.speed, turn_brake.0, err, py, velocity.0.y, target.x, target.z, velocity.0.x, velocity.0.z);
        }
        let stepped = crab_feast_library::tuning::ground_velocity_step(
            [velocity.0.x, velocity.0.z],
            [target.x, target.z],
            crab_feast_library::tuning::REVERSE_ACCEL_MPS2,
            time.delta_secs(),
        );
        velocity.0.x = stepped[0];
        velocity.0.z = stepped[1];
    }
}

/// TEMP 诊断：连续甩头用的摇杆替身
#[derive(Resource)]
struct AutoTurnDiag {
    period: f32,
    elapsed: f32,
    flip: bool,
}

impl Default for AutoTurnDiag {
    fn default() -> Self {
        let period = std::env::var("CRAB_AUTO_TURN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.6);
        Self {
            period,
            elapsed: 0.0,
            flip: true,
        }
    }
}

fn auto_turn_diag(
    time: Res<Time>,
    mut state: ResMut<AutoTurnDiag>,
    mut input: ResMut<MovementInput>,
) {
    // `CRAB_AUTO_TURN_MODE=pulse`：推 → 松手 → 反向推 → 松手，
    // 复现「频繁停下再起步、每次起步都掉头」这一种输入节奏
    let pulse = std::env::var("CRAB_AUTO_TURN_MODE").is_ok_and(|v| v == "pulse");
    state.elapsed += time.delta_secs();
    if state.elapsed >= state.period {
        state.elapsed = 0.0;
        state.flip = !state.flip;
        info!(
            "auto-turn: flip={} t={:.1}s",
            state.flip,
            time.elapsed_secs()
        );
    }
    if pulse && state.flip {
        *input = MovementInput::Idle;
        return;
    }
    let direction = if state.flip { Vec2::Y } else { Vec2::NEG_Y };
    *input = MovementInput::Activated {
        direction,
        force: 1.0,
    };
}

/// TEMP 诊断：`CRAB_AUTO_WALK=<秒>` 一直往前推摇杆，用来在手机上复现
/// 「直线跑动时的抖动」——不发这个变量就不装。
#[derive(Resource, Default)]
struct AutoWalkDiag {
    left: f32,
    armed: bool,
}

fn auto_walk_diag(
    time: Res<Time>,
    mut state: ResMut<AutoWalkDiag>,
    mut input: ResMut<MovementInput>,
) {
    if !state.armed {
        state.armed = true;
        state.left = std::env::var("CRAB_AUTO_WALK")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(6.0);
    }
    if state.left <= 0.0 {
        return;
    }
    state.left -= time.delta_secs();
    // TEMP 抓帧：朝 -Z 开阔地走（相机默认朝 +Z，摇杆 -Y 反向），避开 +Z 出生挡墙。
    *input = MovementInput::Activated {
        direction: Vec2::new(0.0, -1.0),
        force: 1.0,
    };
}

/// 临时诊断：给每帧注入随机延时，用来验证「画面位置与帧时间解耦」。
/// `CRAB_FRAME_JITTER=<毫秒上限>` 开了才装，用来在桌面上复现「帧时间忽长忽短」。
#[derive(Resource)]
struct FrameJitterDiag {
    max_us: u64,
    armed: bool,
    seed: u64,
}

impl Default for FrameJitterDiag {
    fn default() -> Self {
        Self {
            max_us: 0,
            armed: false,
            // xorshift 的种子不能是 0，否则一直吐 0
            seed: 0x9E37_79B9_7F4A_7C15,
        }
    }
}

fn frame_jitter_diag(mut state: ResMut<FrameJitterDiag>, mut frames: Local<u64>) {
    if !state.armed {
        state.armed = true;
        state.max_us = std::env::var("CRAB_FRAME_JITTER")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .map(|ms| (ms.max(0.0) * 1000.0) as u64)
            .unwrap_or(0);
    }
    if state.max_us == 0 {
        return;
    }
    *frames += 1;
    // xorshift：够随机，不引入依赖
    state.seed ^= state.seed << 13;
    state.seed ^= state.seed >> 7;
    state.seed ^= state.seed << 17;
    let extra = state.seed % state.max_us.max(1);
    std::thread::sleep(std::time::Duration::from_micros(extra));
}

/// 原始摇杆 → 这一帧**生效**的输入（[`EffectiveMove`]）。
///
/// 只处理一件事：拇指从一边扫到另一边时会经过圆心，那几帧量级 ≈ 0。
/// 这不是「没在走」，而是「正在换方向」——所以低量级时沿用上一段的朝向/偏转/force，
/// 最多 [`STICK_SWEEP_SECS`]；真松手（`MovementInput::Idle`）不在此列，立刻停。
///
/// 这个结果同时给移动系统和上行发包（`net::send_input`）用：两端必须看到同一条
/// 输入，否则客户端还在跑、服务端已经当成没输入停住，和解只能把人往回拽。
fn steady_stick(
    time: Res<Time>,
    input: Res<MovementInput>,
    mut smoothing: ResMut<InputSmoothing>,
    mut effective: ResMut<EffectiveMove>,
) {
    let dt = time.delta_secs();
    let (local, force) = match input.as_ref() {
        MovementInput::Idle => {
            smoothing.hold = None;
            *effective = EffectiveMove::default();
            return;
        }
        MovementInput::Activated { direction, force } => (*direction, *force),
    };
    let deflection = local.length().clamp(0.0, 1.0);
    // 只看「摇杆 + force」合成的量级：轻推摇杆（偏转小、force 也给得小）不该被判成扫中心
    let magnitude = (deflection * force).clamp(0.0, 1.0);
    if magnitude >= STICK_SWEEP_MAG {
        // 正常推杆：记下这一段，供扫过中心时沿用
        smoothing.hold = Some(StickHold {
            angle: local.to_angle(),
            deflection,
            force,
            age: 0.0,
        });
    } else if smoothing
        .hold
        .as_ref()
        .is_some_and(|hold| hold.age + dt <= STICK_SWEEP_SECS)
    {
        // 手还按着、量级掉了：沿用上一段，别让人在中间站一下
        let hold = smoothing.hold.as_mut().expect("上一行刚检查过");
        hold.age += dt;
        *effective = EffectiveMove {
            direction: Vec2::from_angle(hold.angle),
            deflection: hold.deflection,
            force: hold.force,
        };
        return;
    } else {
        smoothing.hold = None;
    }
    *effective = if magnitude > STICK_REST_MAG {
        EffectiveMove {
            direction: local.normalize_or_zero(),
            deflection,
            force,
        }
    } else {
        // 在圆心停太久：确实没在走
        EffectiveMove::default()
    };
}

/// 角色朝向：追着目标偏航转过去，角速度受限，所以看起来比相机慢半拍。
///
/// 有移动意图就追走位方向（脚上的侧向剪辑负责这段时间），松手后保持最后朝向。
/// 目标方向已经在 [`movement_system`] 里被「输入平滑」低通过了，这里只管限速追，
/// 不再二次滤波——否则系数小的时候两层滞后叠在一起，转身会慢得反常。
/// 误差同时写进 [`FacingState`]，动画系统拿它选转向姿态。
///
/// 顶速和角加速度还要乘一个「脚在地上」权重：跑步 57% 的时间是双脚腾空
/// （Running 剪辑 0.70s 周期里支撑只有 0.153s + 0.146s），腾空期没有任何地面反力，
/// 身体却照样按 90°/s 转，看起来就是「人在空中被拧了一下」。真实跑步的转身力矩
/// 全来自支撑期，所以这里把转速压到 [`AIR_TURN`] 的份上，落地那个支撑段再补回来。
fn facing_system(
    mut avatars: Query<(
        Entity,
        &FacingController,
        &LocomotionIntent,
        &mut FacingState,
    )>,
    yaw_nodes: Query<(Entity, &ChildOf), With<crate::scene::AvatarYaw>>,
    mut yaw_transforms: Query<&mut Transform, With<crate::scene::AvatarYaw>>,
    mut body_transforms: Query<&mut Transform, Without<crate::scene::AvatarYaw>>,
    time: Res<Time>,
    driver: Option<Res<PoseDriver>>,
    jump: Option<Res<JumpAction>>,
) {
    // 滞空段没有任何地面反力：转向一律按腾空算。不看 `contacts` 是因为动作层
    // 接管期间它已经被清零，而「松手时意向量为 0」那条捷径会让 `foot_support`
    // 误判成踩实，空中就按满速拧身子。蓄力 / 落地恢复段人还在地上，照常算踩实。
    let airborne = jump.as_ref().is_some_and(|jump| jump.is_airborne());
    for (entity, controller, intent, mut state) in avatars.iter_mut() {
        // 朝向节点只挂在本地角色身上；预测体（联机）没有，回退写本体。
        let yaw_entity = yaw_nodes
            .iter()
            .find(|(_, child_of)| child_of.parent() == entity)
            .map(|(yaw, _)| yaw);

        // 当前朝向直接取状态里的 yaw：本体旋转不再被 facing 写入（本地角色），
        // 也不能再从本体 Transform 反推。
        let current = state.yaw;
        let target = if intent.magnitude > 1e-3 {
            yaw_of(intent.direction)
        } else {
            // 松手后保持当前朝向，不自动转向相机
            state.yaw
        };

        let error = wrap_pi(target - current);
        let error_deg = error.to_degrees();

        // 「脚在地上」权重：读的是**上一帧**的值——`drive_pose` 在 `PostUpdate` 跑，
        // 我们在 `Update`，混合输出的 contacts 只能晚一帧拿到。跑步的腾空段有
        // 0.19~0.21s，16ms 的滞后不影响门控落在哪一段；这里也不能挪到 `PostUpdate`，
        // 因为 `solve_foot_ik` 是直接拿 Transform 自己乘世界矩阵的，必须看到本帧的
        // root 旋转，而 root 旋转就是这里写进去的
        let support_now = foot_support(driver.as_deref(), intent, airborne);
        let dt = time.delta_secs();
        state.support += (support_now - state.support) * support_smoothing(dt);
        let support = state.support;
        // 腾空期：顶速打 [`AIR_TURN`] 折，角加速度几乎清零（空中没有力矩能加速）
        let speed_eff = controller.turn_speed_at(support);
        let accel_eff = controller.turn_accel_at(support);

        // 比例控制：误差大时跑满顶速，接近目标自动收速，不会在终点上来回抖
        let desired = if error_deg.abs() <= controller.tolerance {
            0.0
        } else {
            error.signum() * (error_deg.abs() * FOLLOW_RESPONSE).min(speed_eff)
        };
        // 角加速度限速：转速是「逼近」而不是「赋值」，起步和收尾才有形状
        let rate = approach(state.yaw_rate, desired, accel_eff * dt).clamp(-speed_eff, speed_eff);
        state.yaw_rate = rate;

        // 单帧转角不许超过剩余误差，避免高转速下冲过头再往回摆
        let step = error.signum() * (rate.abs().to_radians() * dt).min(error.abs());
        state.yaw = current + step;
        // 误差取本帧转完之后剩下的那一份：动画的转向权重按它算，
        // 才不会在角色已经对准时还压满转向剪辑
        state.error_deg = wrap_pi(target - state.yaw).to_degrees();

        if std::env::var("CRAB_LOG_YAW").is_ok() {
            info!(
                "yaw-step: cur={:.1}° tgt={:.1}° err={:.1}° rate={:.1}°/s",
                current.to_degrees(),
                target.to_degrees(),
                state.error_deg,
                state.yaw_rate.to_degrees()
            );
        }

        // 朝向写到朝向子节点（本地角色）；本体只承担物理与平移插值，绝不写它的
        // rotation，否则插值库会把这次改动判成「手动传送」，连平移两格 easing 一起清空。
        let facing_quat = Quat::from_rotation_y(state.yaw);
        if let Some(yaw_entity) = yaw_entity {
            if let Ok(mut yaw_transform) = yaw_transforms.get_mut(yaw_entity) {
                yaw_transform.rotation = facing_quat;
            }
        } else if let Ok(mut body_transform) = body_transforms.get_mut(entity) {
            body_transform.rotation = facing_quat;
        }
    }
}

/// 转向的比例增益（度/秒 每 度误差）：90° 误差时立刻跑满上限，最后十几度平滑收到 0。
///
/// 这个值只影响**收尾**：前半段的快慢由 [`FacingController::turn_speed`] 决定，
/// 觉得“转得拖沓”但增大会显得假时，先调它，别去抬最大角速度。
const FOLLOW_RESPONSE: f32 = crab_feast_library::tuning::TURN_RESPONSE;

/// 腾空期还保留多少转向：0 = 空中完全不转，1 = 完全按顶速转（等于关掉门控）。
///
/// 跑步一个 0.70s 周期里腾空占 0.40s（Running 剪辑实测 0.211s + 0.190s）。取 0.5：
/// 腾空 45°/s、每段腾空转 9.5°；落地支撑段 0.15s 转 13.5°，一步 23°（原来是 36°）。
/// 平均转速 `(0.57 × 0.5 + 0.43) × 90 ≈ 64°/s`，180° 反向约 2.8s（原来 2.2s）。
const AIR_TURN: f32 = 0.5;

/// 腾空期的角加速度保留比例（350°/s² × 0.1 = 35°/s²）。
/// 空中没有地面反力，只有角动量守恒：转速可以保持，但不该再加速
const AIR_ACCEL: f32 = 0.1;

/// 支撑权重低通的时间常数（秒）。跑步的支撑段只有 0.15s、腾空段 0.19~0.21s，
/// 硬切会让转速每一步抖一个折角；0.08s 能削掉折角又不把腾空段抹平
const SUPPORT_SMOOTH_SECS: f32 = 0.08;

/// 本帧「至少一只脚踩实」的权重（0 = 双脚腾空，1 = 踩实）。
///
/// 取 [`PoseDriver::contacts`] 的较大者：它由剪辑的步态事件插值出来，
/// 走/跑/跳天然不同（Walking 支撑占 93%、Running 43%、Jump 20%），混合时按剪辑权重
/// 加权，所以走跑过渡也不用单独处理。
///
/// 三种情况一律按踩实算，否则转身会被门控卡死：
/// - 没挂混合空间（预览、没加载模型、单测这条系统）时 `contacts` 是 0；
/// - Idle / LeftTurn / RightTurn 这些剪辑只有触地事件、没有离地事件，
///   `support_window` 需要成对事件才给值，于是恒为 0；
/// - 刚起步（地速 < 0.5 m/s）还谈不上步态，加速段不该被压住。
///
/// 例外是跳跃动作接管期间（`airborne`）：那时上面三条都不成立，人是真的离地。
fn foot_support(driver: Option<&PoseDriver>, intent: &LocomotionIntent, airborne: bool) -> f32 {
    if airborne {
        return 0.0;
    }
    let Some(driver) = driver else {
        return 1.0;
    };
    if intent.magnitude < 0.2 || driver.ground_speed < 0.5 {
        return 1.0;
    }
    let [left, right] = driver.contacts;
    left.max(right).clamp(0.0, 1.0)
}

/// 一帧的指数低通系数（时间常数 [`SUPPORT_SMOOTH_SECS`]）
fn support_smoothing(dt: f32) -> f32 {
    if SUPPORT_SMOOTH_SECS <= f32::MIN_POSITIVE {
        return 1.0;
    }
    (1.0 - (-dt.max(0.0) / SUPPORT_SMOOTH_SECS).exp()).clamp(0.0, 1.0)
}

/// 带限速的趋近：每帧最多走 `max_delta`，到不了就停在目标上。
fn approach(current: f32, target: f32, max_delta: f32) -> f32 {
    let delta = target - current;
    if delta.abs() <= max_delta {
        target
    } else {
        current + delta.signum() * max_delta
    }
}

/// 平滑系数 → 与帧率无关的单帧跟随权重。
///
/// 60fps 时 alpha 就等于系数本身；帧率变了衰减速度不变。关闭平滑时返回 1.0（直接到位）。
fn smoothing_alpha(enabled: bool, factor: f32, dt: f32) -> f32 {
    if !enabled {
        return 1.0;
    }
    let factor = factor.clamp(SMOOTHING_MIN, 1.0);
    1.0 - (1.0 - factor).powf((dt * 60.0).clamp(0.0, 4.0))
}

/// 角度的一阶低通：走最短弧，跨 ±π 不会绕远路。
fn approach_yaw(current: f32, target: f32, alpha: f32) -> f32 {
    current + wrap_pi(target - current) * alpha
}

/// 鼠标拖动 → 相机轨道角。
///
/// 这里**故意不做平滑**：视角是玩家手里的「方向盘」，滤一下就变成拖影；
/// 「输入平滑」只作用于移动输入（见 [`movement_system`]），两者互不干扰。
fn look_system(
    trigger: On<LookInput>,
    mut look_controllers: Query<(&mut Transform, &mut LookController)>,
) {
    // 每单位（视口宽度的比例）拖动转多少弧度
    let rotate_speed = 10.0;
    let max_pitch = std::f32::consts::FRAC_PI_2 - 0.1; // 防止翻转，限制在±85度

    look_controllers
        .iter_mut()
        .for_each(|(mut transform, mut look_controller)| {
            let input = trigger.event().0;

            match look_controller.axis {
                LookAxis::Yaw => {
                    look_controller.accumulated_yaw -= input.x * rotate_speed;
                }
                LookAxis::Pitch => {
                    look_controller.accumulated_pitch = (look_controller.accumulated_pitch
                        + input.y * rotate_speed)
                        .clamp(-max_pitch, max_pitch);
                }
                LookAxis::YawAndPitch => {
                    look_controller.accumulated_yaw -= input.x * rotate_speed;
                    look_controller.accumulated_pitch = (look_controller.accumulated_pitch
                        + input.y * rotate_speed)
                        .clamp(-max_pitch, max_pitch);
                }
            }

            // 基于累积的角度创建新的旋转
            transform.rotation = Quat::from_euler(
                EulerRot::ZYX,
                0.0, // 滚转
                look_controller.accumulated_yaw,
                look_controller.accumulated_pitch,
            );
        });
}

/// 空格 → 跳跃请求。这里只维护「按下/松开」的状态，边沿锁存在动作层。
fn jump_keyboard(keys: Res<ButtonInput<KeyCode>>, mut jump: ResMut<JumpInput>) {
    if keys.just_pressed(KeyCode::Space) {
        eprintln!("client: 空格键按下（JumpInput → Activated）");
        *jump = JumpInput::Activated;
    } else if keys.just_released(KeyCode::Space) {
        *jump = JumpInput::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 系数越小，目标偏航追得越慢：10 帧后低系数的角度明显落在后面
    #[test]
    fn lower_factor_makes_target_lag() {
        let dt = 1.0 / 60.0;
        let target = 1.5_f32; // 约 86°
        let slow_alpha = smoothing_alpha(true, SMOOTHING_MIN, dt);
        let fast_alpha = smoothing_alpha(true, 1.0, dt);

        let mut slow = 0.0_f32;
        let mut fast = 0.0_f32;
        for _ in 0..10 {
            slow = approach_yaw(slow, target, slow_alpha);
            fast = approach_yaw(fast, target, fast_alpha);
        }

        // 系数拉满时一帧就跟到目标
        assert!((fast - target).abs() < 1e-4);
        // 系数压到底时十帧后还在半路上
        assert!(slow < target * 0.75);
        assert!(slow > 0.0);
    }

    /// 帧率不同但经过同样的时间，追到的角度应当相近（指数平滑的要点）
    #[test]
    fn smoothing_is_frame_rate_independent() {
        let target = 1.0_f32;
        let factor = 0.3;

        let mut at_60 = 0.0_f32;
        for _ in 0..60 {
            at_60 = approach_yaw(at_60, target, smoothing_alpha(true, factor, 1.0 / 60.0));
        }
        let mut at_30 = 0.0_f32;
        for _ in 0..30 {
            at_30 = approach_yaw(at_30, target, smoothing_alpha(true, factor, 1.0 / 30.0));
        }

        // 一秒后两条曲线的落点应当在半度以内
        assert!((at_60 - at_30).abs() < 0.01, "60fps={at_60}, 30fps={at_30}");
    }

    /// 关闭平滑时权重为 1，行为与改造前完全一致
    #[test]
    fn disabled_smoothing_snaps_immediately() {
        assert_eq!(smoothing_alpha(false, 0.1, 1.0 / 60.0), 1.0);
        assert!((approach_yaw(0.0, 2.0, 1.0) - 2.0).abs() < 1e-5);
    }

    /// 最短弧：从 +170° 转到 -170° 只需要转 20°，不能绕 340°
    #[test]
    fn approach_wraps_the_short_way() {
        let start = 170.0_f32.to_radians();
        let target = (-170.0_f32).to_radians();
        let next = approach_yaw(start, target, 1.0);
        let delta = wrap_pi(next - start).to_degrees();
        assert!((delta.abs() - 20.0).abs() < 0.01, "delta={delta}");
    }

    // ---- 系统层面：平滑必须作用在 WASD / 摇杆输入上，且不碰相机视角 ----

    /// 只挂 movement_system 的最小 App，带一个角色实体
    fn movement_app(enabled: bool, factor: f32, direction: Vec2) -> App {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .init_resource::<InputSmoothing>()
            .init_resource::<EffectiveMove>()
            .init_resource::<TurnBrakeRamp>()
            .insert_resource(MovementSettings {
                smoothing_enabled: enabled,
                smoothing_factor: factor,
                ..MovementSettings::default()
            })
            .init_resource::<crate::anim::JumpSettings>()
            .insert_resource(CameraFacing::default())
            .insert_resource(MovementInput::Activated {
                direction,
                force: 1.0,
            })
            .add_systems(Update, (steady_stick, movement_system).chain());
        app.world_mut().spawn((
            LinearVelocity(Vec3::ZERO),
            MovementController::default(),
            LocomotionIntent::default(),
            FacingState::default(),
        ));
        app
    }

    /// 从静止跑到满速。起步走的是有限加速度（20 m/s²，约 0.15 秒），
    /// 所以测试里不能再「一帧就满速」，先跑够这一小段。
    fn run_to_cruise(app: &mut App) {
        for _ in 0..14 {
            step(app, 1.0 / 60.0);
        }
    }

    /// 把朝向误差摆到某个角度（降速读的就是这个值）
    fn set_facing_error(app: &mut App, error_deg: f32) {
        let world = app.world_mut();
        let mut query = world.query::<&mut FacingState>();
        let mut state = query.iter_mut(world).next().expect("角色实体");
        state.error_deg = error_deg;
        // 夹角这么大，身体必然正在按顶速转——刹车力按转速给，这里必须一起摆上
        state.yaw_rate = -error_deg.signum() * 90.0;
    }

    /// 把身体朝向掰到某个世界角度（地速方向取的就是它）
    fn set_facing_yaw(app: &mut App, yaw: f32) {
        let world = app.world_mut();
        let mut query = world.query::<&mut FacingState>();
        let mut state = query.iter_mut(world).next().expect("角色实体");
        state.yaw = yaw;
    }

    fn step(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(seconds));
        app.update();
    }

    fn intent_of(app: &mut App) -> LocomotionIntent {
        let world = app.world_mut();
        let mut query = world.query::<&LocomotionIntent>();
        let intents: Vec<_> = query.iter(world).copied().collect();
        assert_eq!(intents.len(), 1);
        intents[0]
    }

    fn velocity_of(app: &mut App) -> Vec3 {
        let world = app.world_mut();
        let mut query = world.query::<&LinearVelocity>();
        let mut velocities: Vec<_> = query.iter(world).copied().collect();
        assert_eq!(velocities.len(), 1);
        velocities.remove(0).0
    }

    /// 把动作层摆成「已经在滞空」
    fn take_off(app: &mut App) {
        app.insert_resource(JumpAction::in_air_for_test());
    }

    /// 180° 甩头：地速该被压到下限（不是 0），档位本身必须留着——
    /// 朝向系统靠 `magnitude > 0` 判断「还要不要追方向」，压成 0 就永远转不过来
    #[test]
    fn reversal_brakes_speed_but_keeps_turning() {
        let mut app = movement_app(false, 1.0, -Vec2::Y);
        step(&mut app, 1.0 / 60.0);
        let floor = crab_feast_library::tuning::TURN_BRAKE_MIN_SCALE_DEFAULT;

        set_facing_error(&mut app, 180.0);
        step(&mut app, 1.0 / 60.0);
        let first = intent_of(&mut app).speed_scale;
        assert!(first > 0.9, "第一帧就掉到 {first}，没有惯性刹车的过程");

        // 约 0.35 秒刹到下限，然后一直稳在下限（整段转弯都在低速折线里）
        for _ in 0..30 {
            step(&mut app, 1.0 / 60.0);
        }
        let intent = intent_of(&mut app);
        assert!(
            (intent.speed_scale - floor).abs() < 1e-4,
            "180° 应稳在下限 {floor}，实测 {}",
            intent.speed_scale
        );
        assert!(
            (intent.magnitude - 1.0).abs() < 1e-6,
            "档位被一起压掉了，身体会停止转身：{}",
            intent.magnitude
        );

        // 转身完成后（夹角收敛到 0）斜坡把速度提回满速，且比刹车慢
        set_facing_error(&mut app, 0.0);
        let mut ticks = 0;
        while intent_of(&mut app).speed_scale < 1.0 - 1e-4 && ticks < 200 {
            step(&mut app, 1.0 / 60.0);
            ticks += 1;
        }
        assert!(ticks > 30, "提速不该比刹车快：{ticks} tick");
        assert!(ticks < 60, "提速拖了 {ticks} tick，转完半天还上不来");

        // 小角度：30° 以内完全不碰
        set_facing_error(&mut app, 25.0);
        step(&mut app, 1.0 / 60.0);
        assert!(
            (intent_of(&mut app).speed_scale - 1.0).abs() < 1e-6,
            "小角度不该降速"
        );
    }

    /// 拇指扫过圆心（摇杆回中那几帧）：绝不能当成「没输入」，
    /// 否则人会先站一拍、速度被写成 0，再瞬间切到反方向跑，脚步全乱
    #[test]
    fn sweeping_through_the_center_keeps_running() {
        let mut app = movement_app(false, 1.0, -Vec2::Y);
        run_to_cruise(&mut app);
        let cruise = velocity_of(&mut app);
        assert!(cruise.z > 2.5, "先要跑起来：{cruise:?}");

        // 正从一边拖到另一边：手还按着，量级掉到 0（正好落在圆心）
        *app.world_mut().resource_mut::<MovementInput>() = MovementInput::Activated {
            direction: Vec2::ZERO,
            force: 0.0,
        };
        for _ in 0..8 {
            step(&mut app, 1.0 / 60.0);
            let v = velocity_of(&mut app);
            assert!(
                (v - cruise).length() < 0.2,
                "扫过中心时速度被抽走了：{cruise:?} → {v:?}"
            );
            assert_eq!(intent_of(&mut app).magnitude, 1.0, "量级不该掉到 0");
        }

        // 落到另一边：一条直线反向，不横着偏
        *app.world_mut().resource_mut::<MovementInput>() = MovementInput::Activated {
            direction: Vec2::Y,
            force: 1.0,
        };
        for _ in 0..12 {
            step(&mut app, 1.0 / 60.0);
            let v = velocity_of(&mut app);
            assert!(v.x.abs() < 1e-3, "反向过程横向偏出去了：{v:?}");
        }
        let braking = velocity_of(&mut app);
        assert!(
            braking.z < cruise.z - 0.5,
            "落到另一边之后应该开始减速：{braking:?}"
        );
    }

    /// 真的松手（手抬起来）仍然是立刻停，不给「扫过中心」的保持
    #[test]
    fn release_stops_without_the_sweep_hold() {
        let mut app = movement_app(false, 1.0, -Vec2::Y);
        run_to_cruise(&mut app);
        assert!(velocity_of(&mut app).z > 2.5, "先要跑起来");
        *app.world_mut().resource_mut::<MovementInput>() = MovementInput::Idle;
        step(&mut app, 1.0 / 60.0);
        let v = velocity_of(&mut app);
        assert!(v.x.abs() < 1e-6 && v.z.abs() < 1e-6, "松手该立刻停：{v:?}");
    }

    /// 拇指停在圆心不动超过保持窗口：确实不走，允许停下
    #[test]
    fn resting_at_the_center_stops_after_the_window() {
        let mut app = movement_app(false, 1.0, -Vec2::Y);
        run_to_cruise(&mut app);
        *app.world_mut().resource_mut::<MovementInput>() = MovementInput::Activated {
            direction: Vec2::ZERO,
            force: 0.0,
        };
        // 0.25s 窗口：前 0.2s 保持，之后才允许掉到 0
        for _ in 0..30 {
            step(&mut app, 1.0 / 60.0);
        }
        let v = velocity_of(&mut app);
        assert!(v.length() < 0.1, "停在圆心这么久应该停下：{v:?}");
        assert_eq!(intent_of(&mut app).magnitude, 0.0, "量级该归零");
    }

    /// 90° 以内的变向是贴地控制：几 tick 就跟上，不做成拖泥带水的惯性
    #[test]
    fn shallow_changes_apply_instantly() {
        let mut app = movement_app(false, 1.0, -Vec2::Y);
        // 脸朝 +X（yaw = 90°），摇杆推向世界 +Z：夹角 90°，不算「反向」
        set_facing_yaw(&mut app, std::f32::consts::FRAC_PI_2);
        for _ in 0..14 {
            step(&mut app, 1.0 / 60.0);
        }

        let velocity = velocity_of(&mut app);
        // 90° 夹角已经吃满转身降速（下限 0.4×），只验方向不再沿旧轴、纵向已成主分量
        assert!(velocity.z > 1.0, "90° 变向该跟上：{velocity:?}");
        assert!(velocity.x.abs() < 1e-3, "不该还沿旧方向跑：{velocity:?}");
    }

    /// 180° 甩头：先在原方向上刹到 0、再反向加速，全程**一条直线**。
    ///
    /// 之前速度方向取身体朝向，位移会被身体拖着划出一道圆弧（实测横向偏出 1.6 m），
    /// 观感就是「绕一个圈转回来」。现在方向取输入、惯性在速度层给。
    #[test]
    fn reversal_brakes_on_a_straight_line() {
        let mut app = movement_app(false, 1.0, -Vec2::Y);
        run_to_cruise(&mut app);
        let cruise = velocity_of(&mut app);
        assert!(cruise.z > 2.5, "先要跑起来：{cruise:?}");

        // 摇杆推向后方（local +Y → 世界 -Z）
        *app.world_mut().resource_mut::<MovementInput>() = MovementInput::Activated {
            direction: Vec2::Y,
            force: 1.0,
        };
        // 0.2 秒：还没刹完，速度仍朝原方向、但已经在掉
        for _ in 0..12 {
            step(&mut app, 1.0 / 60.0);
            let v = velocity_of(&mut app);
            assert!(v.x.abs() < 1e-3, "反向过程横向偏出去了：{v:?}");
        }
        let braking = velocity_of(&mut app);
        assert!(
            braking.z > 0.5 && braking.z < cruise.z,
            "应该正在沿原方向减速：{braking:?}"
        );

        // 0.8 秒：已经反向，仍然没有横向
        for _ in 0..36 {
            step(&mut app, 1.0 / 60.0);
        }
        let back = velocity_of(&mut app);
        assert!(back.z < -0.2, "该倒着往回走了：{back:?}");
        assert!(back.x.abs() < 1e-3, "反向之后横向偏出去了：{back:?}");
    }

    /// 空中松手：水平动量必须原样留到落地，落点不能被输入抽走
    #[test]
    fn airborne_release_keeps_horizontal_momentum() {
        let mut app = movement_app(true, 1.0, -Vec2::Y);
        run_to_cruise(&mut app);
        let cruise = velocity_of(&mut app);
        assert!(cruise.x.abs() + cruise.z.abs() > 1.0, "先要跑起来");

        take_off(&mut app);
        *app.world_mut().resource_mut::<MovementInput>() = MovementInput::Idle;
        step(&mut app, 1.0 / 60.0);

        let after = velocity_of(&mut app);
        assert!(
            (after - cruise).length() < 1e-6,
            "空中松手改掉了水平速度：{cruise:?} → {after:?}"
        );
        assert_eq!(intent_of(&mut app).magnitude, 0.0, "松手后身体不该再追方向");
    }

    /// 空中推杆：速度**只能**限速转向，一 tick 挪不到一个加速度步长
    ///
    /// 这条同时守住两件事：一是转向不能瞬移（否则推杆当场改落点），
    /// 二是转向不能改速度大小（否则空中等于多给一格推进）。
    #[test]
    fn airborne_input_steers_slowly_without_changing_speed() {
        let mut app = movement_app(true, 1.0, -Vec2::Y);
        run_to_cruise(&mut app);
        let cruise = velocity_of(&mut app);

        take_off(&mut app);
        // 90° 换向：往 +X 推
        *app.world_mut().resource_mut::<MovementInput>() = MovementInput::Activated {
            direction: Vec2::X,
            force: 1.0,
        };
        let settings = *app.world().resource::<JumpSettings>();
        step(&mut app, 1.0 / 60.0);
        let after = velocity_of(&mut app);

        let moved = (after - cruise).length();
        let step_len = settings.air_accel / 60.0;
        assert!(
            (moved - step_len).abs() < 1e-3,
            "空中一 tick 应该正好挪一个加速度步长 {step_len}，实际 {moved}"
        );
        let before_speed = cruise.x.hypot(cruise.z);
        let after_speed = after.x.hypot(after.z);
        assert!(
            after_speed <= before_speed + 1e-4,
            "空中转向不该加速：{before_speed} → {after_speed}"
        );
        let intent = intent_of(&mut app);
        assert!(
            intent.direction.x.abs() > 0.5,
            "意向应当已经转向新方向（朝向不受限速影响）：{intent:?}"
        );
    }

    /// 起跳后过了 `air_steer_secs`：水平速度彻底锁死，推杆只转朝向
    #[test]
    fn airborne_steering_locks_after_the_window() {
        let mut app = movement_app(true, 1.0, -Vec2::Y);
        step(&mut app, 1.0 / 60.0);
        let cruise = velocity_of(&mut app);

        let settings = *app.world().resource::<JumpSettings>();
        let mut action = JumpAction::in_air_for_test();
        action.set_air_elapsed_for_test(settings.air_steer_secs + 0.01);
        app.insert_resource(action);
        *app.world_mut().resource_mut::<MovementInput>() = MovementInput::Activated {
            direction: Vec2::X,
            force: 1.0,
        };
        step(&mut app, 1.0 / 60.0);

        let after = velocity_of(&mut app);
        assert!(
            (after - cruise).length() < 1e-6,
            "接管窗口过了还在改落点：{cruise:?} → {after:?}"
        );
        assert!(
            intent_of(&mut app).direction.x.abs() > 0.5,
            "朝向仍然该跟着输入转"
        );
    }

    /// 空中操控力度滑到 0：退回「完全锁死落点」，推杆一个字都不动速度
    #[test]
    fn zero_air_accel_locks_the_trajectory() {
        let mut app = movement_app(true, 1.0, -Vec2::Y);
        step(&mut app, 1.0 / 60.0);
        let cruise = velocity_of(&mut app);

        app.insert_resource(JumpSettings {
            air_accel: 0.0,
            ..*app.world().resource::<JumpSettings>()
        });
        take_off(&mut app);
        *app.world_mut().resource_mut::<MovementInput>() = MovementInput::Activated {
            direction: Vec2::X,
            force: 1.0,
        };
        step(&mut app, 1.0 / 60.0);

        let after = velocity_of(&mut app);
        assert!(
            (after - cruise).length() < 1e-6,
            "力度为 0 还改了落点：{cruise:?} → {after:?}"
        );
    }

    /// 起跳接管期间一律按腾空算转向权重，松手（意向量 0）也不例外
    #[test]
    fn air_facing_is_gated_even_without_intent() {
        assert_eq!(
            foot_support(Some(&empty_driver(3.0)), &idle_intent(), true),
            0.0
        );
    }

    /// 档位量级不过滤波器：系数压到底也是同一帧满档，起步加速手感不受这个旋钮影响
    #[test]
    fn magnitude_passes_through_unsmoothed() {
        let mut app = movement_app(true, SMOOTHING_MIN, -Vec2::Y);
        step(&mut app, 1.0 / 60.0);

        let intent = intent_of(&mut app);
        assert!(
            (intent.magnitude - 1.0).abs() < 1e-6,
            "magnitude={}",
            intent.magnitude
        );
        // 方向第一次有输入时直接对齐，不会先偏一下
        assert!(
            (intent.direction.length() - 1.0).abs() < 1e-3,
            "direction={:?}",
            intent.direction
        );
    }

    /// 关平滑：切向后同一帧就到位，和改造前一致
    #[test]
    fn smoothing_off_turns_immediately() {
        let mut app = movement_app(false, SMOOTHING_MIN, -Vec2::Y);
        step(&mut app, 1.0 / 60.0);
        let start = intent_of(&mut app).direction;

        *app.world_mut().resource_mut::<MovementInput>() = MovementInput::Activated {
            direction: Vec2::X,
            force: 1.0,
        };
        step(&mut app, 1.0 / 60.0);

        let turned = intent_of(&mut app).direction;
        let angle = start.dot(turned).clamp(-1.0, 1.0).acos();
        assert!(
            (angle - 90.0_f32.to_radians()).abs() < 0.01,
            "关平滑时应当一帧转满 90°，实际 {}°",
            angle.to_degrees()
        );
    }

    /// 从 W 切到 D（相对相机转了 -90°）：低系数下一帧方向只挪一小步，
    /// 这正是「键盘八方向离散跳变」被抹平的地方
    #[test]
    fn direction_change_is_smoothed() {
        // 先用满系数把方向对齐到「前」
        let mut app = movement_app(true, 1.0, -Vec2::Y);
        step(&mut app, 1.0 / 60.0);
        let start = intent_of(&mut app).direction;
        assert!(start.z > 0.9, "初态应当朝 +Z（屏幕深处），实际 {start:?}");

        {
            let world = app.world_mut();
            *world.resource_mut::<MovementInput>() = MovementInput::Activated {
                direction: Vec2::X,
                force: 1.0,
            };
            *world.resource_mut::<MovementSettings>() = MovementSettings {
                smoothing_enabled: true,
                smoothing_factor: SMOOTHING_MIN,
                ..MovementSettings::default()
            };
        }
        step(&mut app, 1.0 / 60.0);

        let turned = intent_of(&mut app).direction;
        let angle = start.dot(turned).clamp(-1.0, 1.0).acos();
        assert!(angle > 0.0, "方向应当开始转");
        assert!(
            angle < 90.0_f32.to_radians() * 0.25,
            "低系数下一帧不该转过 1/4，实际转了 {}°",
            angle.to_degrees()
        );
    }

    /// 松手要立刻停，并把平滑状态清零（否则下次推杆会继承上次的残余方向）
    #[test]
    fn releasing_input_resets_smoothing_state() {
        let mut app = movement_app(true, SMOOTHING_MIN, -Vec2::Y);
        step(&mut app, 1.0 / 60.0);
        assert!(
            app.world()
                .resource::<InputSmoothing>()
                .local_angle
                .is_some()
        );

        *app.world_mut().resource_mut::<MovementInput>() = MovementInput::Idle;
        step(&mut app, 1.0 / 60.0);

        assert!(
            app.world()
                .resource::<InputSmoothing>()
                .local_angle
                .is_none()
        );
        assert_eq!(intent_of(&mut app).magnitude, 0.0);
    }

    /// 相机视角不吃「输入平滑」：同样的拖动量，开/关、不同系数转过的角度必须一模一样
    #[test]
    fn camera_look_is_not_affected_by_smoothing() {
        fn yaw_after_drag(enabled: bool, factor: f32) -> f32 {
            let mut app = App::new();
            app.insert_resource(MovementSettings {
                smoothing_enabled: enabled,
                smoothing_factor: factor,
                ..MovementSettings::default()
            })
            .add_observer(look_system);
            app.world_mut()
                .spawn((Transform::default(), LookController::default()));

            app.world_mut().trigger(LookInput(Vec2::new(0.2, 0.0)));

            let world = app.world_mut();
            let mut query = world.query::<&LookController>();
            query.iter(world).next().unwrap().accumulated_yaw
        }

        let fast = yaw_after_drag(true, 1.0);
        let slow = yaw_after_drag(true, SMOOTHING_MIN);
        let off = yaw_after_drag(false, SMOOTHING_MIN);

        assert!(fast.abs() > 0.0, "拖动应当改变视角");
        assert!(
            (fast - slow).abs() < 1e-6,
            "系数不该影响相机：{fast} vs {slow}"
        );
        assert!(
            (fast - off).abs() < 1e-6,
            "开关不该影响相机：{fast} vs {off}"
        );
    }

    // ---- 系统层面：转身必须起步有加速、过程不超顶速、终点能收住 ----

    /// 只挂 facing_system 的最小 App；角色初始朝 +Z，意图指向 `error_deg` 偏角处
    fn facing_app(error_deg: f32) -> App {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .add_systems(Update, facing_system);
        let target = error_deg.to_radians();
        app.world_mut().spawn((
            Transform::default(),
            FacingController::default(),
            LocomotionIntent {
                direction: Quat::from_rotation_y(target) * FORWARD_LOCAL,
                magnitude: 1.0,
                ..LocomotionIntent::default()
            },
            FacingState::default(),
        ));
        app
    }

    fn facing_state(app: &mut App) -> FacingState {
        let world = app.world_mut();
        let mut query = world.query::<&FacingState>();
        *query.iter(world).next().expect("角色实体")
    }

    #[test]
    fn turn_ramps_up_instead_of_snapping() {
        let mut app = facing_app(180.0);
        let controller = FacingController::default();
        step(&mut app, 1.0 / 60.0);
        let rate = facing_state(&mut app).yaw_rate;
        // 第一帧只有角加速度给的那一份，不能直接等于顶速
        let expected = controller.turn_accel / 60.0;
        assert!(
            (rate.abs() - expected).abs() < 1.0,
            "第一帧角速度 {rate}，期望 {expected}"
        );
        assert!(
            rate.abs() < controller.turn_speed * 0.2,
            "第一帧不该接近顶速：{rate}"
        );
    }

    #[test]
    fn turn_never_exceeds_cap_and_settles() {
        let mut app = facing_app(180.0);
        let controller = FacingController::default();
        let dt = 1.0 / 60.0;
        let mut max_rate: f32 = 0.0;
        let mut elapsed = 0.0;
        // 帧数按「180° 走满顶速」的时长给 1.6 倍余量：顶速越低，这圈转得越久，
        // 写死 180 帧只够 180°/s 那档，30°/s 会永远转不到位
        let ideal = 180.0 / controller.turn_speed;
        let frames = ((ideal * 1.6) / dt).ceil() as usize;
        for _ in 0..frames {
            step(&mut app, dt);
            elapsed += dt;
            let state = facing_state(&mut app);
            max_rate = max_rate.max(state.yaw_rate.abs());
            if state.error_deg.abs() <= controller.tolerance {
                break;
            }
        }
        let state = facing_state(&mut app);
        assert!(
            max_rate <= controller.turn_speed + 1.0,
            "角速度超了上限：{max_rate}"
        );
        assert!(
            state.error_deg.abs() <= controller.tolerance,
            "转不到位：剩余 {}°",
            state.error_deg
        );
        // 起步加速与末段比例收尾都会比「匀速走完 180°」多一点，所以比值落在 1.0 以上；
        // 用相对值而不是写死秒数，以后调 turn_speed 不会把这个测试变成噪音
        let ratio = elapsed / ideal;
        assert!(
            (0.85..=1.35).contains(&ratio),
            "180° 用时 {elapsed}s（{ratio:.2} 倍理想时长）不在预期区间"
        );
    }

    #[test]
    fn gait_defaults_to_run_and_toggles_to_walk() {
        // 默认跑：一推杆就是跑，不用一直按 Shift
        let mut gait = MoveGait::default();
        assert!(gait.running);
        assert!((gait.speed() - MoveGait::RUN).abs() < 1e-6);
        // 按一下切走档，档位值必须正好落在 Walking 的采样点上
        gait.toggle();
        assert!(!gait.running);
        assert!((gait.speed() - 0.5).abs() < 1e-6);
        gait.toggle();
        assert!(gait.running);
    }

    // ---- 系统层面：脚在不在空中决定转向顶速 ----

    /// 把角色的 `FacingState` 摆到某个起点（测试里不想等加速段）
    fn set_facing_state(app: &mut App, f: impl FnOnce(&mut FacingState)) {
        let world = app.world_mut();
        let mut query = world.query::<&mut FacingState>();
        let mut state = query.iter_mut(world).next().expect("角色实体");
        f(&mut state);
    }

    /// 双脚腾空（跑步 57% 的时间）：转速必须压在 `AIR_TURN` 那一折上
    #[test]
    fn airborne_caps_the_turn_rate() {
        let mut app = facing_app(180.0);
        // 跑动中、两只脚都在空中
        app.insert_resource(PoseDriver {
            contacts: [0.0, 0.0],
            ground_speed: 3.0,
            ..PoseDriver::default()
        });
        // 省掉 0.4s 的低通过渡：直接摆成「已到顶速、support 已经放空」
        // （180° 的误差取最短弧是 -180°，所以顶速是负向的）
        set_facing_state(&mut app, |state| {
            state.yaw_rate = -90.0;
            state.support = 0.0;
        });
        step(&mut app, 1.0 / 60.0);
        let want = FacingController::default().turn_speed * AIR_TURN;
        let rate = facing_state(&mut app).yaw_rate.abs();
        assert!(
            (rate - want).abs() < 2.0,
            "腾空期角速度 {rate:.1}°/s，期望压在 {want:.1}°/s"
        );
    }

    /// 抬脚那一帧不能把转速直接砍掉：支撑权重低通后是渐变，否则每一步都顿一下
    #[test]
    fn losing_ground_fades_the_rate_instead_of_snapping() {
        let mut app = facing_app(180.0);
        app.insert_resource(PoseDriver {
            contacts: [1.0, 1.0],
            ground_speed: 3.0,
            ..PoseDriver::default()
        });
        for _ in 0..40 {
            step(&mut app, 1.0 / 60.0);
        }
        let cruise = facing_state(&mut app).yaw_rate.abs();
        assert!(cruise > 85.0, "地上没跑到顶速：{cruise:.1}");

        // 两只脚离地
        app.world_mut().resource_mut::<PoseDriver>().contacts = [0.0, 0.0];
        let mut previous = cruise;
        let mut worst = 0.0_f32;
        for _ in 0..12 {
            step(&mut app, 1.0 / 60.0);
            let now = facing_state(&mut app).yaw_rate.abs();
            worst = worst.max((previous - now).abs());
            previous = now;
        }
        assert!(
            worst < 20.0,
            "抬脚那一帧转速掉了 {worst:.1}°/s，会看到顿一下"
        );
        let want = FacingController::default().turn_speed * AIR_TURN;
        assert!(
            (previous - want).abs() < want * 0.25,
            "腾空 0.2s 后应贴近 {want:.1}°/s，实际 {previous:.1}°/s"
        );

        // 落地后要能把欠的转速补回来：比的是「腾空时被压下去的那一档」，
        // 不是绝对顶速——顶速调快以后人可能已经转到目标、转速本来该收尾
        let air_rate = previous;
        app.world_mut().resource_mut::<PoseDriver>().contacts = [1.0, 1.0];
        let mut best = 0.0_f32;
        for _ in 0..30 {
            step(&mut app, 1.0 / 60.0);
            best = best.max(facing_state(&mut app).yaw_rate.abs());
        }
        assert!(
            best > air_rate * 1.2,
            "落地没能把转速补回来：{best:.1}°/s（腾空时 {air_rate:.1}°/s）"
        );
    }

    /// Idle / LeftTurn 这些剪辑只有触地事件、没有离地事件，`support_window` 恒为 0；
    /// 站着原地转身不能被当成「全程腾空」，刚起步的低地速同理
    #[test]
    fn gait_without_lift_events_counts_as_grounded() {
        let mut app = facing_app(180.0);
        app.insert_resource(PoseDriver {
            contacts: [0.0, 0.0],
            ground_speed: 0.0,
            ..PoseDriver::default()
        });
        step(&mut app, 1.0 / 60.0);
        assert!(
            facing_state(&mut app).support > 0.99,
            "地速为 0 时应按踩实算：{}",
            facing_state(&mut app).support
        );
        assert_eq!(
            foot_support(Some(&empty_driver(3.0)), &idle_intent(), false),
            1.0
        );
        assert_eq!(
            foot_support(Some(&empty_driver(0.0)), &moving_intent(), false),
            1.0
        );
        // 真有步态时取两只脚里更强的那只
        let mut driver = empty_driver(3.0);
        driver.contacts = [0.2, 0.7];
        assert!((foot_support(Some(&driver), &moving_intent(), false) - 0.7).abs() < 1e-6);
    }

    fn empty_driver(ground_speed: f32) -> PoseDriver {
        PoseDriver {
            ground_speed,
            ..PoseDriver::default()
        }
    }

    fn moving_intent() -> LocomotionIntent {
        LocomotionIntent {
            direction: FORWARD_LOCAL,
            magnitude: 1.0,
            ..LocomotionIntent::default()
        }
    }

    fn idle_intent() -> LocomotionIntent {
        LocomotionIntent::default()
    }
}
