use avian3d::prelude::LinearVelocity;
use bevy::prelude::*;

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
/// `turn_speed` 取接近转向剪辑自身的转速（LeftTurn 实测 180°/1.63s ≈ 110°/s），
/// 脚上播的转身和实体真实转过的角度才对得上。
#[derive(Component)]
pub struct FacingController {
    /// 最大转向角速度（度/秒）
    pub turn_speed: f32,
    /// 角度差小于它就认为已经对准，避免在阈值附近抖
    pub tolerance: f32,
}

impl Default for FacingController {
    fn default() -> Self {
        Self {
            turn_speed: 110.0,
            tolerance: 2.0,
        }
    }
}

/// 角色本帧的朝向状态，由 [`facing_system`] 写、动画系统读。
#[derive(Component, Default, Clone, Copy)]
pub struct FacingState {
    /// 当前朝向（世界空间弧度，见 [`yaw_of`]）
    pub yaw: f32,
    /// 目标与当前的带符号夹角（度）。正 = 需要绕 +Y 转，即向左转
    pub error_deg: f32,
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

/// 输入平滑：对摇杆/鼠标输入做一阶滞后（低通滤波）
#[derive(Resource, Default)]
pub struct InputSmoothing {
    /// 上一帧的输入（保留以便跨帧插值）
    pub prev_input: Option<Vec2>,
    /// 平滑系数（0~1），越小越平滑，响应越慢
    pub smoothing_factor: f32,
}

impl InputSmoothing {
    pub fn new(smoothing_factor: f32) -> Self {
        Self {
            prev_input: None,
            smoothing_factor,
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
            accumulated_yaw: 0.0,
            accumulated_pitch: 0.0,
        }
    }
}

pub struct ControlInputPlugin;

impl Plugin for ControlInputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MovementInput>()
            .init_resource::<JumpInput>()
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
fn movement_system(
    mut movers: Query<(
        &mut LinearVelocity,
        &MovementController,
        &mut LocomotionIntent,
    )>,
    input: Res<MovementInput>,
    camera: Res<CameraFacing>,
) {
    let local = match input.as_ref() {
        MovementInput::Activated { direction, force } => {
            Some((*direction, *force)).filter(|(local, _)| local.length_squared() > 1e-6)
        }
        // 松手
        MovementInput::Idle => None,
    };
    let Some((local, force)) = local else {
        // 没有有效输入就停下：不能只靠线性阻尼慢慢漂，否则动画已经切回 Idle 了人还在滑
        for (_, _, mut intent) in movers.iter_mut() {
            *intent = LocomotionIntent::default();
        }
        for (mut velocity, _, _) in movers.iter_mut() {
            velocity.0.x = 0.0;
            velocity.0.z = 0.0;
        }
        return;
    };

    // 摇杆的 y 是屏幕上下，不是世界的竖直轴：直接 extend 会把前后量丢到 Y 上，
    // 于是只剩左右能动。这里拆成「屏幕深处 + 屏幕右」两个世界方向重组
    let forward = camera.view_forward();
    let right = forward.cross(Vec3::Y);
    let direction = (right * local.x + forward * -local.y).normalize_or_zero();
    // 档位量级与混合空间的 speed 轴同义：推满为 1
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
/// 误差同时写进 [`FacingState`]，动画系统拿它选转向姿态。
fn facing_system(
    mut avatars: Query<(
        &mut Transform,
        &FacingController,
        &LocomotionIntent,
        &mut FacingState,
    )>,
    _camera: Res<CameraFacing>,
    time: Res<Time>,
) {
    for (mut transform, controller, intent, mut state) in avatars.iter_mut() {
        // 朝向由 Avatar 根节点控制
        let current = yaw_of(transform.rotation * FORWARD_LOCAL);
        let target_yaw = if intent.magnitude > 1e-3 {
            yaw_of(intent.direction)
        } else {
            // 松手后保持当前朝向，不自动转向相机
            state.yaw
        };

        let error = wrap_pi(target_yaw - current);
        let error_deg = error.to_degrees();
        // 比例控制：误差大时跑满 turn_speed，接近目标自动收速，不会在终点上来回抖
        let rate = (error_deg.abs() * FOLLOW_RESPONSE).min(controller.turn_speed);
        let step = if error_deg.abs() <= controller.tolerance {
            0.0
        } else {
            error.signum() * rate.to_radians() * time.delta_secs()
        };
        state.yaw = current + step;
        // 误差取本帧转完之后剩下的那一份：动画的转向权重按它算，
        // 才不会在角色已经对准时还压满转向剪辑
        state.error_deg = wrap_pi(target_yaw - state.yaw).to_degrees();

        // Avatar 的 rotation 由物理锁定，这里直接修改它（会覆盖但不生效于物理）
        // 模型是 Avatar 的子节点，继承 Avatar 的旋转
        transform.rotation = Quat::from_rotation_y(state.yaw);
    }
}

/// 转向的比例增益（度/秒 每 度误差）：90° 误差时立刻跑满上限，最后十几度平滑收到 0。
const FOLLOW_RESPONSE: f32 = 8.0;

fn look_system(
    trigger: On<LookInput>,
    mut look_controllers: Query<(&mut Transform, &mut LookController)>,
    mut smoothing: ResMut<InputSmoothing>,
    time: Res<Time>,
) {
    let rotate_speed = 10.0;
    let max_pitch = std::f32::consts::FRAC_PI_2 - 0.1; // 防止翻转，限制在±85度
    let _dt = time.delta_secs();

    look_controllers
        .iter_mut()
        .for_each(|(mut transform, mut look_controller)| {
            let input = trigger.event().0;

            // 输入平滑：一阶滞后滤波（低通）
            let smoothed_input = if let Some(prev) = smoothing.prev_input {
                // 当前输入和上一帧输入的插值
                let smoothed_x = prev.x + (input.x - prev.x) * smoothing.smoothing_factor;
                let smoothed_y = prev.y + (input.y - prev.y) * smoothing.smoothing_factor;
                Vec2::new(smoothed_x, smoothed_y)
            } else {
                // 第一次输入，直接使用
                input
            };

            // 更新上一帧输入（用于下一帧插值）
            smoothing.prev_input = Some(input);

            match look_controller.axis {
                LookAxis::Yaw => {
                    look_controller.accumulated_yaw -= smoothed_input.x * rotate_speed;
                }
                LookAxis::Pitch => {
                    look_controller.accumulated_pitch = (look_controller.accumulated_pitch
                        - smoothed_input.y * rotate_speed)
                        .clamp(-max_pitch, max_pitch);
                }
                LookAxis::YawAndPitch => {
                    look_controller.accumulated_yaw -= smoothed_input.x * rotate_speed;
                    look_controller.accumulated_pitch = (look_controller.accumulated_pitch
                        - smoothed_input.y * rotate_speed)
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
