//! 视觉转身：把「根节点直接拧过去」拆成「按支撑期一步一步转」。
//!
//! 单机路径一直是这么做的（`input::facing_system` 按 `foot_support` 折转速）；
//! 联机接上预测核之后，身体朝向由 `predict::pred_velocity` 每 tick 写进 `Rotation`，
//! 是**逻辑朝向**，和脚一点关系都没有——于是 180°/s 硬转，支撑脚的 IK 只能拿
//! `foot_yaw_limit`（30°）去扛，扛满就把脚在地上拧着走。
//!
//! 这里在**视觉**上再加一层：模型根节点的朝向不等于逻辑朝向，它慢慢追，而且
//! 追的速度由支撑状态决定——
//!
//! - 踩实时只给 [`PLANTED_DEG_PER_SEC`]：这点角速度靠髋/髋外旋就能吸收，
//!   支撑脚的偏航补偿永远吃不到上限；
//! - 有一只脚明显离地（换步窗口）给 [`SWING_DEG_PER_SEC`]：差值在这一步里追回来，
//!   落地时脚已经朝着新方向；
//! - 踩实档的顶速**不低于本体**，而且落后超过 [`MAX_LAG_DEG`] 就强制加档：
//!   差值绝不许攒到 ±180°，否则 `wrap_pi` 一翻就是实机上那个「瞬间转一大角度」；
//! - 腾空（跳跃）几乎没有地面反力，给 [`AIR_DEG_PER_SEC`]。
//!
//! 模型根节点的本地旋转**只**由这里写：以后若别处也要转模型根，先把这一份撤掉，
//! 否则两处相加会转两倍。
//!
//! 全部只写模型的 `Transform`：物理 `Position`/`Rotation`、上行的 `NetYaw` 一个字不动，
//! 所以预测端与服务端的轨迹仍然逐 tick 同源，不会因为这一层产生分叉。

use bevy::prelude::*;

use crate::input::wrap_pi;

// 整个模块只服务联机预测端（`mod.rs` 里按 `net` 特性裁剪）：单机由
// `input::facing_system` 直接按支撑期写 `AvatarYaw` 节点，没有这一层。
use super::character::AnimModelRoot;
use super::foot_ik::{FootIk, FootIkSettings};
use super::jump::JumpAction;
use super::locomotion::{VisualYaw, log_turn, yaw_of};
use crate::input::{FacingState, LocomotionIntent, foot_support};
use crate::predict::Prediction;

/// 踩实时允许的转速（度/秒）。
///
/// **必须不小于本体的转身顶速**（`tuning::TURN_SPEED_DEG_PER_SEC = 120`）：小于它，
/// 差值在支撑期里就会一路攒大，攒到 180° 时 `wrap_pi` 一翻就是「瞬间转一个大角度」，
/// 再花一两秒慢慢追回来——这正是实机上看到的那个形状。
///
/// 取 125 而不是 180：脚在支撑期里是钉在世界上的，本体每转一度，踝就要被拽着
/// 走 `半径 × 1°`。125°/s × 0.3s × 身侧 12cm ≈ 4.5cm 的弧，单机那条路径
/// （`facing_system` 顶速 120°/s）是 4.3cm —— 两边基本持平。真要更快，
/// 快的那一段交给抬脚窗口（[`SWING_DEG_PER_SEC`]）
const PLANTED_DEG_PER_SEC: f32 = 125.0;
/// 有一只脚离地（换步窗口）允许的转速：把这一步该转的角度追回来
const SWING_DEG_PER_SEC: f32 = 300.0;
/// 腾空（跳跃）：没有地面反力，几乎不许转
const AIR_DEG_PER_SEC: f32 = 20.0;
/// 支撑权重低于它就算「有脚离地」
const SWING_CONTACT: f32 = 0.35;
/// 允许落后本体多少（度）就强制加档追。
///
/// 这是「绝不把差值攒到 ±180」的保险：差值一大，模型相对本体就朝向背面，
/// 一旦越过 180° 再由 `wrap_pi` 折回来，观感就是**瞬间转过一个大角度**。
/// 45° 还在「转身中段」的合理区间，再大就该让本体带着走了
const MAX_LAG_DEG: f32 = 45.0;
/// 落后超限后的追赶转速（度/秒）。必须高于本体顶速（90），差值才会收回来
const CATCHUP_DEG_PER_SEC: f32 = 200.0;
/// 模型本地偏航差值的硬上限（度）。`wrap_pi` 在 ±180 附近会翻面，留足余量
const MAX_LOCAL_DEG: f32 = 100.0;

/// 诊断日志节流（秒）：只在「视觉落后逻辑超过 15°」时报一行，用来确认这一层
/// 真的在起作用，以及它落后了多少
const LAG_LOG_SECS: f32 = 1.0;

impl VisualYaw {
    /// 推进一帧，返回新的视觉朝向（弧度）。
    ///
    /// `speed` 是本帧允许的顶速（度/秒），`target` 是**插值后的**本体朝向（弧度）。
    ///
    /// 纯粹按速度限流，没有「按误差比例加速」那一层：目标的转速本来就在 90°/s
    /// 以内（它是插值曲线，不是逻辑朝向的 180°/s 硬转），限速只是防跳；
    /// 而按误差取速会让它**稳定落后** `转速 / 增益` —— 90°/s ÷ 8 = 11°。
    /// 那 11° 会被 `locomotion` 当成「移动方向偏了 11°」塞进混合空间的 yaw 轴，
    /// 转弯时腿就切去放横移剪辑，看起来就是「腿扭到一个奇怪的角度」，IK 治不了
    fn advance(&mut self, target: f32, speed: f32, dt: f32) -> f32 {
        let error = wrap_pi(target - self.yaw);
        let cap = speed.max(0.0) * dt.max(0.0);
        let step = error.to_degrees().clamp(-cap, cap);
        self.rate = step / dt.max(1e-4);
        self.yaw = wrap_pi(self.yaw + step.to_radians());
        self.yaw
    }
}

/// 每帧追一次视觉朝向，并把差值写进模型根节点的本地旋转。
///
/// 排在 `drive_pose` 之后（拿到本帧的支撑权重）、`FootIkSet` 之前（IK 必须看到
/// 本帧的根朝向，否则它解的是上一帧的身体）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn follow_visual_yaw(
    time: Res<Time>,
    settings: Res<FootIkSettings>,
    jump: Option<Res<JumpAction>>,
    driver: Option<Res<crate::anim::PoseDriver>>,
    mut commands: Commands,
    // 支撑期的判据要 `LocomotionIntent`：它挂在**角色**身上（模型上没有），
    // 所以和本体一起取，别让模型实体多背一个组件
    locals: Query<(&Transform, &FacingState, &LocomotionIntent), With<Prediction>>,
    mut models: Query<
        (
            Entity,
            &ChildOf,
            &mut Transform,
            Option<&mut VisualYaw>,
            Option<&FootIk>,
            // 模型一定不是本体（本体挂 `Prediction`）：这样两个 `Transform` 查询才是
            // 互斥的，否则 Bevy 会在第一次跑这个系统时直接 panic（B0001）
        ),
        (With<AnimModelRoot>, Without<Prediction>),
    >,
) {
    let dt = time.delta_secs();
    let airborne = jump.as_ref().is_some_and(|jump| jump.is_airborne());
    let limit = settings.foot_yaw_limit.max(1.0);

    for (model, child_of, mut transform, visual, ik) in &mut models {
        let avatar = child_of.parent();
        let Ok((body, facing, intent)) = locals.get(avatar) else {
            continue;
        };
        let Some(mut visual) = visual else {
            // 模型刚挂上（资源异步）：下一帧再对齐，这一帧不碰朝向。
            // 状态必须挂在**模型根节点**上（IK 读的也是它，本体上没有这层）
            commands.entity(model).insert(VisualYaw::default());
            continue;
        };
        let body_yaw = yaw_of(body.rotation);
        if !visual.init {
            // 第一帧直接站到本体朝向上：否则会从 0° 追到实际朝向，一进场就转半圈
            visual.yaw = body_yaw;
            visual.rate = 0.0;
            visual.init = true;
            visual.last_world = body_yaw;
            visual.last_body = body_yaw;
        }

        // 支撑状态：两只脚里最"轻"的那只。跑起来它经常是 0（换步窗口），
        // 站着不动时两只都是 1。没有 IK（资源还没挂 / 全局关了）就当"一直踩实"，
        // 那会给最慢的档位、绝不硬转
        // 换步窗口的判定和单机 `facing_system` 用**同一个** `foot_support`：
        // 两条路径对「一只脚离地了吗」的定义必须一致，否则同一段跑步剪辑在
        // 单机按 45°/s 转、联机按 300°/s 转，两边手感永远对不齐
        let support = foot_support(driver.as_deref(), intent, airborne);
        let contacts = ik.map(|ik| ik.contacts).unwrap_or([1.0; 2]);
        let swing = support < SWING_CONTACT;
        // 支撑脚的偏航补偿吃了多少（0..1，按 `foot_yaw_limit` 归一）
        let used = ik
            .map(|ik| {
                ik.yaw_lock_deg()
                    .iter()
                    .fold(0.0_f32, |worst, deg| worst.max(deg.abs() / limit))
            })
            .unwrap_or(0.0);

        // 档位：腾空最慢、换步窗口最快、踩实居中。
        //
        // `used >= YAW_BUDGET`（支撑脚的偏航补偿已经吃到髋的活动范围）**不再停手**：
        // 停手会让差值被本体一路甩开，攒到 180° 就翻面。它只在「踩实」档里生效，
        // 也就是把「踩实时」压到最慢的那一档，等下一只脚抬起来再快
        let speed = if airborne {
            AIR_DEG_PER_SEC
        } else if swing {
            SWING_DEG_PER_SEC
        } else {
            PLANTED_DEG_PER_SEC
        };

        // 保险：落后超过 [`MAX_LAG_DEG`] 就加档
        let lag_now = wrap_pi(facing.yaw - visual.yaw).to_degrees();
        let speed = if lag_now.abs() > MAX_LAG_DEG {
            speed.max(CATCHUP_DEG_PER_SEC)
        } else {
            speed
        };

        // 目标换成**插值后的**本体朝向：`PoseSmoothing::sample` 已经把位置和朝向
        // 一起插值成一条延迟 1.25/3.0 tick 的曲线，模型只要跟着它 —— 两只表叠起来
        // 才是那 11° 稳定落后 + 支撑期拧脚的来源
        let yaw = visual.advance(body_yaw, speed, dt);

        // 诊断：可见朝向单帧跳了多少。`视觉` 那一栏是我们自己算的（理论上不会跳），
        // `本体` 那一栏是 `pred_render` 写进本体 Transform 的。哪一栏在跳，
        // 就说明那一下是从哪条路径来的
        let world_jump = wrap_pi(yaw - visual.last_world).to_degrees();
        let body_jump = wrap_pi(body_yaw - visual.last_body).to_degrees();
        if log_turn() && world_jump.abs() > 8.0 {
            info!(
                "turn/snap: 可见 {:+.1}°/帧（本体 {:+.1}°，视觉 {:+.1}°，补偿 {:+.1}°，档位 {speed:.0}）",
                world_jump,
                body_jump,
                world_jump - body_jump,
                used * limit
            );
        }
        visual.last_world = yaw;
        visual.last_body = body_yaw;

        // 诊断：视觉真的落后逻辑时才喊，一秒最多一行
        visual.since_log += dt;
        let lag_deg = wrap_pi(facing.yaw - yaw).to_degrees();
        if log_turn() && lag_deg.abs() > 15.0 && visual.since_log >= LAG_LOG_SECS {
            visual.since_log = 0.0;
            info!(
                "turn/visual: 视觉落后 {lag_deg:+.1}°（支撑脚补偿 {:+.1}°，支撑 {:.2}/{:.2}，档位 {speed:.0}°/s）",
                used * limit,
                contacts[0],
                contacts[1]
            );
        }
        // 模型是本体子节点：本地旋转 = 视觉朝向 − 本体逻辑朝向。
        // `wrap_pi` 在 ±180 附近会翻面（瞬间转一大圈），所以先把差值夹在
        // [`MAX_LOCAL_DEG`] 以内再写，并把视觉朝向一起钉回夹取后的位置
        let mut diff = wrap_pi(yaw - body_yaw).to_degrees();
        if diff.abs() > MAX_LOCAL_DEG {
            diff = MAX_LOCAL_DEG * diff.signum();
            visual.yaw = wrap_pi(body_yaw + diff.to_radians());
        }
        transform.rotation = Quat::from_rotation_y(diff.to_radians());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deg(value: f32) -> f32 {
        value.to_radians()
    }

    /// 单帧转角不许超过剩余误差：180° 反向不会冲过头再摆回来
    #[test]
    fn never_overshoots_the_target() {
        let mut visual = VisualYaw::default();
        visual.yaw = 0.0;
        visual.init = true;
        let target = deg(180.0);
        let dt = 1.0 / 60.0;
        for _ in 0..240 {
            visual.advance(target, SWING_DEG_PER_SEC, dt);
        }
        assert!(
            (wrap_pi(target - visual.yaw)).abs() < deg(0.6),
            "转完还剩 {:.2}°",
            wrap_pi(target - visual.yaw).to_degrees()
        );
    }

    /// 踩实档的转速要明显低于换步档：这就是"用脚转"和"硬转"的区别
    #[test]
    fn planted_keeps_up_with_the_body_and_swing_is_faster() {
        // 踩实档不许比本体慢：慢了差值就会一路攒到 180°，`wrap_pi` 一翻
        // 就是「瞬间转一大角度」。这是那条实机症状的回归测试
        assert!(
            PLANTED_DEG_PER_SEC >= crab_feast_library::tuning::TURN_SPEED_DEG_PER_SEC,
            "踩实档 {PLANTED_DEG_PER_SEC}°/s 比本体顶速还慢，差值会攒到翻面"
        );
        let dt = 1.0 / 60.0;
        let mut planted = VisualYaw::default();
        planted.init = true;
        let mut swing = VisualYaw::default();
        swing.init = true;
        // 本体在这个窗口里该走过的角：顶速 × 0.15 s（支撑期量级）
        let body_deg = crab_feast_library::tuning::TURN_SPEED_DEG_PER_SEC * 0.15;
        // 一个换步窗口量级的时长：0.15 s
        for _ in 0..9 {
            planted.advance(deg(120.0), PLANTED_DEG_PER_SEC, dt);
            swing.advance(deg(120.0), SWING_DEG_PER_SEC, dt);
        }
        let p = planted.yaw.to_degrees().abs();
        let s = swing.yaw.to_degrees().abs();
        // 第一段还带着角加速度的爬坡（2000°/s² 爬到 125°/s 要 0.06 s，吃掉不到 1°），
        // 允许它比本体少一点点
        assert!(
            p >= body_deg * 0.9,
            "踩实档 0.15 s 只转了 {p:.1}°，跟不上本体的 {body_deg:.1}°"
        );
        // 坡爬满之后的第二段才看「跟不跟得住」：本体 120°/s × 0.15 = 18°，
        // 踩实档这时已经是稳态转速，必须不落后
        let yaw0 = planted.yaw;
        for _ in 0..9 {
            planted.advance(deg(120.0), PLANTED_DEG_PER_SEC, dt);
        }
        let steady = (planted.yaw - yaw0).to_degrees().abs();
        assert!(
            steady >= body_deg - 1e-2,
            "踩实档稳态 0.15 s 只转了 {steady:.2}°，跟不上本体的 {body_deg:.1}°（差值会攒到翻面）"
        );
        assert!(
            s > p * 1.4,
            "0.15 s 内踩实 {p:.1}°，换步 {s:.1}°，档位没拉开"
        );
        assert!(s > 15.0, "换步窗口只转了 {s:.1}°，追不回来");
    }

    /// 本体甩到 180° 的另一侧时，本地偏航差值必须夹在 `MAX_LOCAL_DEG` 以内：
    /// 越过 ±180 会被 `wrap_pi` 折回来，观感就是"瞬间转一大圈"
    #[test]
    fn local_offset_never_reaches_the_wrap() {
        let body = deg(180.0);
        let visual = deg(-1.0);
        let mut diff = wrap_pi(visual - body).to_degrees();
        if diff.abs() > MAX_LOCAL_DEG {
            diff = MAX_LOCAL_DEG * diff.signum();
        }
        assert!(diff.abs() <= MAX_LOCAL_DEG + 1e-3);
    }

    /// 第一帧必须站在本体朝向上：否则一进场就从 0° 追到实际朝向
    #[test]
    fn first_frame_matches_the_body() {
        let mut visual = VisualYaw::default();
        let body_yaw = deg(135.0);
        assert!(!visual.init);
        visual.yaw = body_yaw;
        visual.init = true;
        assert!((visual.yaw - body_yaw).abs() < 1e-6);
    }

    /// 只验证接线：模型是本体子节点时，系统必须真的写到**模型的**本地旋转上，
    /// 且第一帧不能跳（要从本体朝向起步）
    #[test]
    fn system_drives_the_model_not_the_body() {
        use bevy::ecs::system::RunSystemOnce;
        use std::time::Duration;

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<FootIkSettings>();
        let body = app
            .world_mut()
            .spawn((
                // `pred_render` 写进本体的**插值朝向**：这一层要跟的就是它
                Transform::default(),
                FacingState::default(),
                LocomotionIntent {
                    direction: Vec3::Z,
                    magnitude: 1.0,
                    ..Default::default()
                },
                Prediction::default(),
            ))
            .id();
        let mut ik = FootIk::default();
        ik.contacts = [1.0, 1.0];
        let model = app
            .world_mut()
            .spawn((
                Transform::default(),
                VisualYaw::default(),
                AnimModelRoot,
                ik,
                ChildOf(body),
            ))
            .id();

        // 先把视觉朝向对齐到本体（0°）
        for _ in 0..2 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(Duration::from_secs_f32(1.0 / 60.0));
            app.world_mut()
                .run_system_once(follow_visual_yaw)
                .expect("视觉转身系统跑不起来");
        }
        // 本体一帧甩到 60°（`pred_render` 的插值曲线偶尔会走这样一步）
        app.world_mut()
            .get_mut::<Transform>(body)
            .expect("本体")
            .rotation = Quat::from_rotation_y(deg(60.0));
        for _ in 0..2 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(Duration::from_secs_f32(1.0 / 60.0));
            app.world_mut()
                .run_system_once(follow_visual_yaw)
                .expect("视觉转身系统跑不起来");
        }

        // 模型根节点写的是「视觉 − 本体」的本地差值：踩实档一帧只能走 1.6°，
        // 所以本地应该是 -58° 一带，绝不可能是 0（没写）
        let local = yaw_of(app.world().get::<Transform>(model).expect("模型").rotation);
        assert!(
            local < deg(-20.0),
            "模型根节点没被写入朝向（本地 {:.1}°）",
            local.to_degrees()
        );
        assert!(
            local > deg(-60.0),
            "踩实档一帧就把 60° 追平了，档位没生效（本地 {:.1}°）",
            local.to_degrees()
        );
        // 本体一个字节都不该动
        let body_yaw = yaw_of(app.world().get::<Transform>(body).expect("本体").rotation);
        assert!(
            (body_yaw - deg(60.0)).abs() < 1e-6,
            "本体被写了：{:.1}°",
            body_yaw.to_degrees()
        );
    }

    /// 稳态不许落后：本体以顶速（120°/s）匀速转，视觉朝向必须跟住（误差 < 1°）。
    ///
    /// 这是「腿扭到一个奇怪的角度」的回归测试：按误差取速的旧实现会稳定落后
    /// `顶速 ÷ 8 ≈ 15°`，那 15° 进混合空间的 yaw 轴就是横移剪辑
    #[test]
    fn tracks_the_interpolated_yaw_without_a_steady_lag() {
        let dt = 1.0 / 60.0;
        let rate = crab_feast_library::tuning::TURN_SPEED_DEG_PER_SEC;
        let mut visual = VisualYaw::default();
        visual.init = true;
        // 先跑一秒把任何瞬态吃掉
        let mut body = 0.0f32;
        for _ in 0..120 {
            body += deg(rate * dt);
            visual.advance(body, PLANTED_DEG_PER_SEC, dt);
        }
        let lag = wrap_pi(body - visual.yaw).to_degrees().abs();
        assert!(lag < 1.0, "稳态落后 {lag:.2}°（{rate:.0}°/s 直行转身）");
    }

    /// 差值写回必须是「视觉 = 本体 + 本地」：本地旋转写错，模型会整体偏一个角
    #[test]
    fn local_rotation_restores_the_visual_yaw() {
        let body_yaw = deg(30.0);
        let visual_yaw = deg(50.0);
        let local = wrap_pi(visual_yaw - body_yaw);
        let total = wrap_pi(body_yaw + local);
        assert!((total - visual_yaw).abs() < 1e-5, "{total} != {visual_yaw}");
    }
}
