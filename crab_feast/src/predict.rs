//! 本地预测核：客户端自己按 60 Hz 推进角色的位移与朝向，服务端快照只用来校正。
//!
//! 分工：
//!
//! - [`PredClock`]：本地 tick 时间轴，锚定在服务端 `NetTick` 上，每个定步长 +1；
//! - [`Prediction`]（挂在本地角色上）：转身降速斜坡 + 视觉软偏移；
//! - 轨迹历史**不另存一份**，直接用 [`PoseSmoothing`] 的 tick 环——那个环本来就在做
//!   渲染插值，比分存两份更不容易错位；
//! - 每个定步长：先按输入推速度（算式与服务端**逐 tick 同源**），物理积分，
//!   再把结果记进环，`clock.tick += 1`；
//! - 每收到一份快照：拿「服务端在 tick T 的位置」对「我记在 tick T 的位置」，
//!   差多少就把**当前状态和整段历史**一起平移过去（预测轨迹被拉回权威轨迹），
//!   同时把差值塞进视觉软偏移里，让画面在 ~0.1 s 内慢慢收敛，而不是跳一下。
//!
//! 为什么不是「重放」：重放要求客户端有一份能逐 tick 复现的仿真，并且把没被确认的
//! 输入全部再跑一遍。这里的物理是世界统一的（墙、方块、别的玩家都在），重放会把
//! 整段物理再推一次，代价和风险都大；直接平移轨迹 + 视觉软收敛在同样能消掉误差的
//! 前提下，不会因为一次抖动就把人拽回去（这正是之前那版预测抖动的原因）。

use avian3d::prelude::{LinearVelocity, Position, Rotation};
use bevy::prelude::*;
use crab_feast_library::air::{air_steer_active, air_steer_velocity};
use crab_feast_library::net::{NetPos, NetTick, NetYaw};
use crab_feast_library::tuning;

use crate::anim::{JumpAction, JumpSettings};
use crate::camera::{CameraAnchor, CameraFacing};
use crate::input::{EffectiveMove, FacingState, LocomotionIntent, MovementSettings, wrap_pi};
use crate::net_pose::PoseSmoothing;

/// 本地预测的时间轴。
///
/// `anchored = false` 时还没和服务端对上：不上锚就不发 tick（`send_input` 发 0，
/// 服务端退回「到达即生效」），本地也不推速度，等第一份快照把人放到位再说。
#[derive(Resource, Default, Debug)]
pub struct PredClock {
    /// **下一个**要跑的 tick（每记完一 tick 就 +1）。
    pub tick: u32,
    pub anchored: bool,
}

/// 本地预测的跨 tick 状态。
#[derive(Component, Clone, Copy, Debug)]
pub struct Prediction {
    /// 转身降速斜坡（0~1），和服务端 `TurnBrake` 同一个 [`tuning::ramp_turn_brake`]。
    brake: f32,
    /// 视觉软偏移（世界空间）：校正时把差值记在这里，让它慢慢收敛到 0。
    vis: Vec3,
    vis_yaw: f32,
    /// 上一次消费过的快照 tick（同一份快照只和解一次）。
    last_tick: u32,
    seen: bool,
}

impl Default for Prediction {
    fn default() -> Self {
        Self {
            brake: 1.0,
            vis: Vec3::ZERO,
            vis_yaw: 0.0,
            last_tick: 0,
            seen: false,
        }
    }
}

/// 视觉软偏移的时间常数（秒）：差值按指数收敛，和帧率无关。
const VIS_TAU: f32 = 0.10;
/// 软偏移上限（米 / 弧度）：避免一次大误差把画面糊掉。
const VIS_MAX: f32 = 0.5;
const VIS_YAW_MAX: f32 = 0.6;
/// 超过这个距离就不做软收敛，直接落位（视作传送 / 被别的玩家挤开）。
const SNAP_DIST: f32 = 1.5;

impl Prediction {
    /// 本帧的视觉偏移（已经过衰减）。
    fn offset(&self) -> (Vec3, f32) {
        (self.vis, self.vis_yaw)
    }
}

/// 6.1 上锚：第一份快照告诉我们服务端现在是第几 tick，本地从它的下一 tick 开始推。
pub(crate) fn anchor_clock(
    mut clock: ResMut<PredClock>,
    locals: Query<&NetTick, With<Prediction>>,
) {
    if clock.anchored {
        return;
    }
    let Some(tick) = locals.iter().next() else {
        return;
    };
    clock.tick = tick.0.wrapping_add(1);
    clock.anchored = true;
    info!("predict: 上锚到服务端 tick {}，本地从 {} 开始", tick.0, clock.tick);
}

/// 每 tick 按输入推速度与朝向。
///
/// 算式全部来自 `crab_feast_library`，和服务端 `drive_avatars` / `face_movement`
/// 是同一套：地速方向取输入方向、转身降速走同一个斜坡、反向走
/// [`tuning::ground_velocity_step`]、滞空走 `air_steer_velocity` 且只在前
/// `air_steer_secs` 内接管。任何一处两端不一致，预测轨迹就会每个 tick 慢慢分叉。
#[allow(clippy::too_many_arguments)]
pub(crate) fn pred_velocity(
    clock: Res<PredClock>,
    // 这几个资源在冒烟测试 / 最小 App 里可能不存在：缺席就让预测停手，
    // 不要为了一个可选输入把整个 App panic 掉
    effective: Option<Res<EffectiveMove>>,
    facing: Option<Res<CameraFacing>>,
    settings: Option<Res<MovementSettings>>,
    jump_settings: Option<Res<JumpSettings>>,
    jump: Option<Res<JumpAction>>,
    time: Res<Time<Fixed>>,
    mut locals: Query<(
        &mut LinearVelocity,
        &mut Rotation,
        &mut Prediction,
        &mut LocomotionIntent,
        &mut FacingState,
    )>,
) {
    if !clock.anchored {
        return;
    }
    let dt = time.delta_secs();
    let airborne = jump.as_ref().is_some_and(|jump| jump.is_airborne());
    let air_elapsed = jump.as_ref().map_or(0.0, |jump| jump.air_elapsed());
    let air_accel = jump_settings.as_ref().map_or(0.0, |s| s.air_accel);
    let air_steer_secs = jump_settings.as_ref().map_or(0.0, |s| s.air_steer_secs);

    // 摇杆是相对相机的：上 = 屏幕深处。与 `send_input` 用同一条换算。
    let (Some(effective), Some(facing), Some(settings)) = (effective, facing, settings) else {
        return;
    };
    let effective = *effective;
    let direction = if effective.is_active() {
        let local = effective.local_stick();
        let forward = facing.view_forward();
        let right = Vec3::new(-forward.z, 0.0, forward.x);
        let world = right * local.x + forward * (-local.y);
        Vec2::new(world.x, world.z).normalize_or_zero()
    } else {
        Vec2::ZERO
    };
    let push = if effective.is_active() {
        // 档位 = 摇杆偏转 × force，和单机的 `EffectiveMove::magnitude()`
        // 以及混合空间 `speed` 轴是同一个量。只用 `force` 会漏掉偏转量：
        // 轻推摇杆（触屏）就按满档算，永远走不起来
        effective.magnitude()
    } else {
        0.0
    };

    for (mut velocity, mut rotation, mut pred, mut intent, mut state) in &mut locals {
        // 意向（表现层用）：方向取输入方向，量级取档位（和 `send_input` 上行同一份）
        intent.direction = Vec3::new(direction.x, 0.0, direction.y);
        intent.magnitude = if direction.length_squared() > 1e-6 {
            push
        } else {
            0.0
        };

        // 朝向：和服务端 `face_movement` 完全一致（同一个 `step_yaw`）。
        // 服务端不按触地权重折速（它拿不到动画层），预测端也不能折。
        let (yaw, rate) = if intent.magnitude > 1e-3 && direction.length_squared() > 1e-6 {
            let goal = direction.x.atan2(direction.y);
            tuning::step_yaw(state.yaw, state.yaw_rate, wrap_pi(goal - state.yaw), dt)
        } else {
            (state.yaw, 0.0)
        };
        state.yaw = yaw;
        state.yaw_rate = rate;
        state.error_deg = if direction.length_squared() > 1e-6 {
            wrap_pi(direction.x.atan2(direction.y) - yaw).to_degrees()
        } else {
            0.0
        };
        rotation.0 = Quat::from_rotation_y(yaw);

        // 转身降速斜坡：每个 tick 都推，空中也推（两端不能停在不同格子）
        let target = tuning::ground_brake_target(
            [direction.x, direction.y],
            yaw,
            settings.turn_brake_start_deg,
            settings.turn_brake_min_scale,
        );
        pred.brake = tuning::ramp_turn_brake(pred.brake, target, state.yaw_rate, dt);
        // 降速比例也要进动画档位（和单机的 `intent.speed_scale` 同一个语义）：
        // 只压速度不压档位，甩头时腿还在跑满档、身子已经慢下来了，就是滑步
        intent.speed_scale = pred.brake;

        if !airborne {
            let ground = if direction.length_squared() > 1e-6 {
                // 档位同时进速度：只进动画不进速度，走档就是「腿在走、身子在冲刺」
                tuning::ground_velocity_push([direction.x, direction.y], pred.brake, push)
            } else {
                [0.0, 0.0]
            };
            let stepped = tuning::ground_velocity_step(
                [velocity.0.x, velocity.0.z],
                ground,
                tuning::REVERSE_ACCEL_MPS2,
                dt,
            );
            velocity.0.x = stepped[0];
            velocity.0.z = stepped[1];
        } else if air_steer_active(air_elapsed, air_steer_secs) && direction.length_squared() > 1e-6 {
            // 滞空：保动量、限转向（同一份 `air_steer_velocity`，同一组旋钮）。
            // 加速度按档位缩放：单机用 `magnitude`、服务端用「dir 长度 × force」，
            // 这里是同一个量，轻推不会拿到满力度
            let steered = air_steer_velocity(
                [velocity.0.x, velocity.0.z],
                [direction.x, direction.y],
                air_accel * push,
                dt,
            );
            velocity.0.x = steered[0];
            velocity.0.z = steered[1];
        }
    }
}

/// 每个定步长把物理结果记进轨迹环，然后推进 tick。
///
/// 必须排在 `PhysicsSystems::Writeback` 之后：记的是这一步**积分完**的位置，
/// 和解时才能在同一个 tick 上和服务端比。
pub(crate) fn pred_record(
    mut clock: ResMut<PredClock>,
    locals: Query<(&Position, &FacingState, &mut PoseSmoothing), With<Prediction>>,
) {
    if !clock.anchored {
        return;
    }
    for (position, state, mut smoothing) in locals {
        smoothing.push(clock.tick, position.0, state.yaw);
    }
    clock.tick = clock.tick.wrapping_add(1);
}

/// 和解：快照到了就比一次，把轨迹拉回权威位置。
///
/// `delta = 服务端在 tick T 的位置 − 我在 tick T 的位置`：
/// 1. `Position` 与历史整体 `+= delta`（当前轨迹被拉回，后续 tick 从校正后的状态继续）；
/// 2. 视觉偏移 `-= delta`（画面留在原处，不跳）；
/// 3. 偏移按 [`VIS_TAU`] 指数收敛 → 画面在 ~0.1 s 内平滑滑到权威轨迹上。
///
/// 差值超过 [`SNAP_DIST`] 时不清历史、不走偏移，直接让画面跟着落位。
pub(crate) fn pred_reconcile(
    // 起跳比服务端早半个 RTT：在空中时服务端还在蓄力，它报的 y 是地面高度。
    // 这段时间绝不能按竖直方向拉回，否则每一份快照都把刚跳起来的人按回地面。
    jump: Option<Res<JumpAction>>,
    mut locals: Query<(
        &NetPos,
        &NetTick,
        &NetYaw,
        &mut Position,
        &mut Prediction,
        &mut PoseSmoothing,
        &mut FacingState,
    )>,
) {
    let airborne = jump.as_ref().is_some_and(|jump| jump.is_airborne());
    for (net_pos, net_tick, net_yaw, mut position, mut pred, mut smoothing, mut state) in &mut locals {
        if pred.seen && pred.last_tick == net_tick.0 {
            continue;
        }
        pred.seen = true;
        pred.last_tick = net_tick.0;
        let Some((mine, my_yaw)) = smoothing.sample_at(net_tick.0) else {
            // 历史里没有这一 tick（刚上锚 / 被清过）：这一份只用来对齐，不校正
            continue;
        };
        let target = Vec3::from_array(net_pos.0);
        let mut delta = target - mine;
        if airborne {
            // 只管水平：竖直方向等落地后再收敛
            delta.y = 0.0;
        }
        let dyaw = wrap_pi(net_yaw.0 - my_yaw);
        if !delta.is_finite() || !dyaw.is_finite() {
            continue;
        }
        let hard = delta.length() > SNAP_DIST;

        position.0 += delta;
        state.yaw = wrap_pi(state.yaw + dyaw);
        smoothing.shift(delta, dyaw);

        if hard {
            pred.vis = Vec3::ZERO;
            pred.vis_yaw = 0.0;
        } else {
            pred.vis = (pred.vis - delta).clamp_length_max(VIS_MAX);
            pred.vis_yaw = (pred.vis_yaw - dyaw).clamp(-VIS_YAW_MAX, VIS_YAW_MAX);
        }
    }
}

/// 本帧渲染：插值（`PoseSmoothing`）+ 视觉软偏移。
///
/// 同时写一份**不含**软偏移的 [`CameraAnchor`]：机位读那一份，`vis` 这层收敛
/// 只留给眼睛看角色自己（否则每次和解都会带着整幅画面晃）。
pub(crate) fn pred_render(
    time: Res<Time>,
    mut commands: Commands,
    mut locals: Query<(
        Entity,
        &mut Prediction,
        &mut PoseSmoothing,
        Option<&mut CameraAnchor>,
        &mut Transform,
    )>,
) {
    let dt = time.delta_secs();
    let decay = (-dt / VIS_TAU).exp();
    for (entity, mut pred, mut smoothing, mut anchor, mut transform) in &mut locals {
        pred.vis *= decay;
        pred.vis_yaw *= decay;
        let Some((pos, yaw)) = smoothing.sample(dt) else {
            continue;
        };
        let (vis, vis_yaw) = pred.offset();
        transform.translation = pos + vis;
        transform.rotation = Quat::from_rotation_y(yaw + vis_yaw);
        match anchor.as_mut() {
            Some(anchor) => anchor.0 = pos,
            // 认领那一帧可能还没挂：下一帧之前相机先读 `Transform`，这里补上
            None => {
                commands.entity(entity).insert(CameraAnchor(pos));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 校正后画面不许跳：视觉偏移恰好抵消轨迹平移。
    #[test]
    fn correction_keeps_the_visual_still() {
        let before = Vec3::new(1.0, 0.0, 0.0);
        let delta = Vec3::new(0.05, 0.0, 0.02);
        let mut pred = Prediction::default();
        // 模拟 pred_reconcile 的软分支
        pred.vis = (pred.vis - delta).clamp_length_max(VIS_MAX);
        let shifted = before + delta;
        assert!((shifted + pred.vis - before).length() < 1e-5, "校正那一帧画面被推动了");
    }

    /// 预测端必须把「走档」原样带进动画轴，并且地速也跟着档位走。
    ///
    /// 单机的档位是「摇杆偏转 × force」合成 `speed` 轴，物理速度再由混合输出的
    /// 等效地速决定——走档 1.5 m/s、跑档 3.0 m/s。预测端如果只拿 `force` 当档位、
    /// 地速却始终按满速算，画面就是「走档的腿、跑档的身子」，看着永远在跑。
    #[test]
    fn walk_gait_survives_the_prediction_step() {
        use bevy::ecs::system::RunSystemOnce;
        use std::time::Duration;

        /// 按 `deflection` / `force` 跑 40 个 tick，返回（动画轴量级，稳态地速）
        fn step(deflection: f32, force: f32) -> (f32, f32) {
            let mut app = App::new();
            app.init_resource::<Time<Fixed>>()
                .init_resource::<PredClock>()
                .insert_resource(EffectiveMove {
                    // 屏幕上方（-Y）= 相机视线正前方 = 世界 +Z：和初始朝向一致，
                    // 免得测试自己被转身降速压住档位
                    direction: Vec2::NEG_Y,
                    deflection,
                    force,
                })
                .insert_resource(CameraFacing {
                    yaw: 0.0,
                    pitch: 0.0,
                })
                .insert_resource(MovementSettings::default())
                .insert_resource(JumpSettings::default());
            app.world_mut().resource_mut::<PredClock>().anchored = true;

            let body = app
                .world_mut()
                .spawn((
                    LinearVelocity::ZERO,
                    Rotation::IDENTITY,
                    Prediction::default(),
                    LocomotionIntent::default(),
                    FacingState::default(),
                ))
                .id();

            let mut velocity = Vec3::ZERO;
            for _ in 0..120 {
                app.world_mut()
                    .resource_mut::<Time<Fixed>>()
                    .advance_by(Duration::from_secs_f32(1.0 / 60.0));
                app.world_mut()
                    .run_system_once(pred_velocity)
                    .expect("预测端跑不起来");
                velocity = app.world().get::<LinearVelocity>(body).expect("速度").0;
            }
            let intent = *app.world().get::<LocomotionIntent>(body).expect("意图");
            (intent.magnitude, Vec2::new(velocity.x, velocity.z).length())
        }

        // 键盘走档：偏转满、force 0.5 → 档位 0.5，地速半速
        let (walk_axis, walk_speed) = step(1.0, 0.5);
        assert!(
            (walk_axis - 0.5).abs() < 1e-3,
            "走档被当成跑档送进混合空间：magnitude = {walk_axis}"
        );
        assert!(
            (walk_speed - tuning::MOVE_SPEED_MPS * 0.5).abs() < 0.05,
            "走档的地速不是半速：{walk_speed} m/s"
        );
        // 跑档：满档、满速
        let (run_axis, run_speed) = step(1.0, 1.0);
        assert!((run_axis - 1.0).abs() < 1e-3 && (run_speed - 3.0).abs() < 0.05);
        // 触屏轻推：偏转和 force 都要算进去（单机是 `偏转 × force`），不能按满档跑
        let (half_axis, half_speed) = step(0.5, 0.5);
        assert!(
            (half_axis - 0.25).abs() < 1e-3 && (half_speed - 0.75).abs() < 0.05,
            "轻推摇杆被当成满档：轴 {half_axis}、地速 {half_speed}"
        );
    }

    /// 视觉偏移按指数收敛，帧率不同、同样的真实时间长度收敛到同样的量。
    #[test]
    fn visual_offset_decays_frame_rate_independently() {
        let start = 0.1_f32;
        let mut a = start;
        for _ in 0..75 {
            a *= (-(1.0f32 / 75.0) / VIS_TAU).exp();
        }
        let mut b = start;
        for _ in 0..60 {
            b *= (-(1.0f32 / 60.0) / VIS_TAU).exp();
        }
        assert!((a - b).abs() < 0.002, "75 Hz 剩 {a}，60 Hz 剩 {b}");
        assert!(a < 0.01, "1 秒之后还剩 {a}，收敛太慢");
    }
}
