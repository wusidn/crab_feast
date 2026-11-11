use avian3d::prelude::LinearVelocity;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

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
/// 现在这组（180°/s、700°/s²）就是「倒退着走、身体慢慢转过来」的节奏：180° 反向
/// 落地约 1 秒，转身过程中一直是边走边转，不是急停急转。往上加就是更快更"机械"，
/// 往下减就是更慢更沉。历史值：110°/s/无加速度 → 1.75 秒（拖沓），
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
            turn_speed: 180.0,
            turn_accel: 700.0,
            tolerance: 2.0,
        }
    }
}

/// 角色本帧的朝向状态，由 [`facing_system`] 写、动画系统读。
#[derive(Component, Default, Clone, Copy)]
pub struct FacingState {
    /// 当前朝向（世界空间弧度，见 [`yaw_of`]）
    pub yaw: f32,
    /// 当前角速度（度/秒，带符号）。跨帧记住它才能做角加速度限制
    pub yaw_rate: f32,
    /// 目标与当前的带符号夹角（度）。正 = 需要绕 +Y 转，即向左转
    pub error_deg: f32,
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

/// 本帧的移动意图：世界空间单位方向 + 档位量级（0..1）。
///
/// 摇杆给的是「相对相机」的输入，这里已经换算成世界方向；
/// 朝向系统与动画系统都读它，不再各自解释一遍摇杆坐标。
#[derive(Component, Default, Clone, Copy)]
pub struct LocomotionIntent {
    pub direction: Vec3,
    pub magnitude: f32,
}

/// 角色朝向三件套：挂上就进入「相机先转、角色限速追」这套控制。
///
/// 打成一个 bundle 是因为 Bevy 的元组 Bundle 实现到 15 个为止，
/// 玩家实体上的组件已经快把那个名额用完了。
#[derive(Bundle, Default)]
pub struct Facing {
    pub controller: FacingController,
    pub state: FacingState,
    pub intent: LocomotionIntent,
}

/// 输入平滑的跨帧状态：对**移动输入的朝向**（键盘 WASD / 摇杆）做一阶滞后。
///
/// 系数本身来自 [`MovementSettings`]，这里只存「上一次平滑到哪儿了」，
/// 所以开关与系数改动立刻生效，不需要清状态。
#[derive(Resource, Default)]
pub struct InputSmoothing {
    /// 上一帧平滑后的输入方向（弧度，相对相机）。`None` = 还没历史，第一次直接取目标
    pub local_angle: Option<f32>,
}

/// 平滑系数下限：0 时 alpha=0，输入会冻在上一帧的值上（推杆后像卡住不动）
pub const SMOOTHING_MIN: f32 = 0.05;

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
}

impl MovementSettings {
    /// 把系数夹回可用区间：0 会让看向彻底冻住（alpha=0），所以下限留一点
    pub fn sanitized(mut self) -> Self {
        self.smoothing_factor = self.smoothing_factor.clamp(SMOOTHING_MIN, 1.0);
        self
    }
}

impl Default for MovementSettings {
    fn default() -> Self {
        Self {
            smoothing_enabled: true,
            smoothing_factor: 0.5,
        }
    }
}

/// 角色正前方在实体局部空间里的轴。
///
/// 不是 Bevy 默认的 -Z：本作的 Mixamo 模型面向 +Z。实测依据（`assets/characters`）：
/// Walking / Running 的髛部净位移沿 +Z，LeftToe_End 落在 +X（即右侧是 -X），
/// RightStrafe 往 -X 走 —— 三者自洽。
const FORWARD_LOCAL: Vec3 = Vec3::Z;

/// 世界方向 → 绕 Y 的偏航角，满足 `Quat::from_rotation_y(yaw) * FORWARD_LOCAL == direction`。
fn yaw_of(direction: Vec3) -> f32 {
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
            .init_resource::<InputSmoothing>()
            // 三个集合的先后顺序在 ScenePlugin 里配：相机系统属于 Camera 段，
            // 整条链还要接着排到动画驱动之前，放在接线处比在这里声明一半清楚
            .add_systems(Update, movement_system.in_set(ControlSet::Move))
            .add_systems(Update, facing_system.in_set(ControlSet::Facing))
            .add_systems(
                Update,
                jump_system.run_if(resource_changed::<MovementInput>),
            )
            .add_observer(look_system);
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
    )>,
    input: Res<MovementInput>,
    camera: Res<CameraFacing>,
    settings: Res<MovementSettings>,
    time: Res<Time>,
    mut smoothing: ResMut<InputSmoothing>,
) {
    let local = match input.as_ref() {
        MovementInput::Activated { direction, force } => {
            Some((*direction, *force)).filter(|(local, _)| local.length_squared() > 1e-6)
        }
        // 松手
        MovementInput::Idle => None,
    };
    let Some((local, force)) = local else {
        // 没有有效输入就停下：不能只靠线性阻尼慢慢漂，否则动画已经切回 Idle 了人还在滑。
        // 方向状态一并清掉，免得下次推杆还从上一次的角度慢慢摆过来
        smoothing.local_angle = None;
        for (_, _, mut intent) in movers.iter_mut() {
            *intent = LocomotionIntent::default();
        }
        for (mut velocity, _, _) in movers.iter_mut() {
            velocity.0.x = 0.0;
            velocity.0.z = 0.0;
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
    let target_angle = local.to_angle();
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
    // 档位量级**不过滤波器**：它是「起步加速」的手感，和转向是两个旋钮，
    // 混在一起会让调系数的人分不清自己改的是转身还是加速。
    // 它与混合空间的 speed 轴同义：推满为 1
    let magnitude = (local.length() * force).clamp(0.0, 1.0);

    for (mut velocity, movement_controller, mut intent) in movers.iter_mut() {
        intent.direction = direction;
        intent.magnitude = magnitude;
        let target = direction * movement_controller.speed;
        // LinearVelocity 是 newtype，内部直接是 Vec3
        velocity.0.x = target.x;
        velocity.0.z = target.z;
    }
}

/// 角色朝向：追着目标偏航转过去，角速度受限，所以看起来比相机慢半拍。
///
/// 有移动意图就追走位方向（脚上的侧向剪辑负责这段时间），松手后保持最后朝向。
/// 目标方向已经在 [`movement_system`] 里被「输入平滑」低通过了，这里只管限速追，
/// 不再二次滤波——否则系数小的时候两层滞后叠在一起，转身会慢得反常。
/// 误差同时写进 [`FacingState`]，动画系统拿它选转向姿态。
fn facing_system(
    mut avatars: Query<(
        &mut Transform,
        &FacingController,
        &LocomotionIntent,
        &mut FacingState,
    )>,
    time: Res<Time>,
) {
    for (mut transform, controller, intent, mut state) in avatars.iter_mut() {
        // 朝向由 Avatar 根节点控制
        let current = yaw_of(transform.rotation * FORWARD_LOCAL);
        let target = if intent.magnitude > 1e-3 {
            yaw_of(intent.direction)
        } else {
            // 松手后保持当前朝向，不自动转向相机
            state.yaw
        };

        let error = wrap_pi(target - current);
        let error_deg = error.to_degrees();
        // 比例控制：误差大时跑满 turn_speed，接近目标自动收速，不会在终点上来回抖
        let desired = if error_deg.abs() <= controller.tolerance {
            0.0
        } else {
            error.signum() * (error_deg.abs() * FOLLOW_RESPONSE).min(controller.turn_speed)
        };
        // 角加速度限速：转速是「逼近」而不是「赋值」，起步和收尾才有形状
        let dt = time.delta_secs();
        let rate = approach(state.yaw_rate, desired, controller.turn_accel * dt);
        state.yaw_rate = rate;

        // 单帧转角不许超过剩余误差，避免高转速下冲过头再往回摆
        let step = error.signum() * (rate.abs().to_radians() * dt).min(error.abs());
        state.yaw = current + step;
        // 误差取本帧转完之后剩下的那一份：动画的转向权重按它算，
        // 才不会在角色已经对准时还压满转向剪辑
        state.error_deg = wrap_pi(target - state.yaw).to_degrees();

        // Avatar 的 rotation 由物理锁定，这里直接修改它（会覆盖但不生效于物理）
        // 模型是 Avatar 的子节点，继承 Avatar 的旋转
        transform.rotation = Quat::from_rotation_y(state.yaw);
    }
}

/// 转向的比例增益（度/秒 每 度误差）：90° 误差时立刻跑满上限，最后十几度平滑收到 0。
///
/// 这个值只影响**收尾**：前半段的快慢由 [`FacingController::turn_speed`] 决定，
/// 觉得“转得拖沓”但增大会显得假时，先调它，别去抬最大角速度。
const FOLLOW_RESPONSE: f32 = 10.0;

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

fn jump_system(input: Res<JumpInput>) {
    if let JumpInput::Activated = input.as_ref() {
        // 跳跃逻辑
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
            .insert_resource(MovementSettings {
                smoothing_enabled: enabled,
                smoothing_factor: factor,
            })
            .insert_resource(CameraFacing::default())
            .insert_resource(MovementInput::Activated {
                direction,
                force: 1.0,
            })
            .add_systems(Update, movement_system);
        app.world_mut().spawn((
            LinearVelocity(Vec3::ZERO),
            MovementController::default(),
            LocomotionIntent::default(),
        ));
        app
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
        let mut max_rate: f32 = 0.0;
        let mut elapsed = 0.0;
        for _ in 0..180 {
            step(&mut app, 1.0 / 60.0);
            elapsed += 1.0 / 60.0;
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
        // 180°：180°/s + 700°/s² 大概 1.1s（起步 0.26s 加速、中段匀速、末段比例收尾）。
        // 给个宽区间，防止以后调参把「起步加速」悄悄退化成瞬转
        assert!(
            (0.9..=1.4).contains(&elapsed),
            "180° 用时 {elapsed}s 不在预期区间"
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
}
