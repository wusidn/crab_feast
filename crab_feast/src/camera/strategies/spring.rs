//! 临界阻尼弹簧：焦点带速度状态追角色，到点不过冲、速度自适应。
//!
//! 实现是 Unity `SmoothDamp` 的等价式：`ω = 2 / smooth_time` 标定，
//! `exp(-x)` 用三阶倒数近似，越过目标就钉住并清速度。
//! 比一阶低通好在两处：参数是「特征时间」而不是每帧比例（换帧率手感不变），
//! 以及追远快、收尾稳的自动调速。

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::camera::{
    CameraFollowSettings, CameraFollowState, CameraFollowStrategy, CameraSet, FollowInput,
    strategy_is,
};

pub struct SmoothDampFollowPlugin;

impl Plugin for SmoothDampFollowPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            smooth_damp_follow
                .in_set(CameraSet::Follow)
                .run_if(strategy_is(CameraFollowStrategy::SmoothDamp)),
        );
    }
}

/// 弹簧策略的参数（临界阻尼下只有两个可调量）
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SpringParams {
    /// 特征时间（秒）：追上目标所需的量级。越小越跟手，越大越飘。
    pub smooth_time: f32,
    /// 焦点最大速度（米/秒）。`0` = 不限；它是安全阀，不是常规手感旋钮。
    pub max_speed: f32,
}

impl Default for SpringParams {
    fn default() -> Self {
        Self {
            // 0.2s：跑动时几乎察觉不到滞后，跳跃的整帧抖动会被抹平
            smooth_time: 0.2,
            // 远高于角色跑速与跳跃竖直速度，只在传送被弹簧硬拽时才起作用
            max_speed: 12.0,
        }
    }
}

/// 三轴各弹一次，竖直与水平互不串速
fn smooth_damp_follow(
    input: Res<FollowInput>,
    settings: Res<CameraFollowSettings>,
    mut follow: ResMut<CameraFollowState>,
    time: Res<Time>,
) {
    if !input.valid {
        return;
    }
    let target = input.ideal_focus();
    if follow.snap_if_needed(target, settings.snap_distance) {
        return;
    }

    follow.target = target;
    // 暂停或帧间隔为 0 时不推进弹簧：`omega * dt` 在 dt 抖到 0 附近会算爆
    let dt = time.delta_secs();
    if dt > 0.0 {
        follow.focus = smooth_damp_vec(
            follow.focus,
            target,
            &mut follow.velocity,
            settings.spring,
            dt,
        );
    }
}

/// 三轴各自做一次 [`smooth_damp_axis`]。
///
/// 分轴而不是整体缩放：竖直方向的冲击不会顺着速度传到水平偏移上。
fn smooth_damp_vec(
    current: Vec3,
    target: Vec3,
    velocity: &mut Vec3,
    params: SpringParams,
    dt: f32,
) -> Vec3 {
    let mut v = *velocity;
    let out = Vec3::new(
        smooth_damp_axis(
            current.x,
            target.x,
            &mut v.x,
            params.smooth_time,
            params.max_speed,
            dt,
        ),
        smooth_damp_axis(
            current.y,
            target.y,
            &mut v.y,
            params.smooth_time,
            params.max_speed,
            dt,
        ),
        smooth_damp_axis(
            current.z,
            target.z,
            &mut v.z,
            params.smooth_time,
            params.max_speed,
            dt,
        ),
    );
    *velocity = v;
    out
}

/// 一维临界阻尼弹簧（Unity `SmoothDamp` 的等价式）。
///
/// 相比一阶低通 `lerp(cur, target, k * dt)`：
/// - 参数是「特征时间」而不是每帧比例，改帧率手感不变；
/// - 速度自适应——追得远就快、快到点就收，`max_speed` 只当安全阀；
/// - 到点不过冲，不用另写回弹抑制。
///
/// 地面锚定只弹竖直一个轴，所以这个函数在 crate 内可见。
pub(crate) fn smooth_damp_axis(
    current: f32,
    target: f32,
    velocity: &mut f32,
    smooth_time: f32,
    max_speed: f32,
    dt: f32,
) -> f32 {
    let smooth_time = smooth_time.max(1e-4);
    // 临界阻尼的解析解是 exp(-ωt)；这里用 ω = 2 / T 的常用标定，
    // 让 `smooth_time` 大致等于「走到目标附近所需的时间」
    let omega = 2.0 / smooth_time;
    let x = omega * dt;
    // exp(-x) 的三阶倒数近似：1 / (1 + x + 0.48x² + 0.235x³)，比直接用 1 - x 稳
    let decay = 1.0 / (1.0 + x + 0.48 * x * x + 0.235 * x * x * x);

    let gap = current - target;
    if gap.abs() < 1e-6 {
        *velocity = 0.0;
        return target;
    }

    // 这一步允许的偏差按最大速度压住（`max_speed * smooth_time` 是这一步的长度量级）
    let mut change = gap;
    if max_speed > 0.0 {
        let limit = max_speed * smooth_time;
        change = change.clamp(-limit, limit);
    }

    let wanted = target;
    // 压缩后的「起步点」：弹簧按 current → target 的压缩版走，避免一步冲过
    let target = current - change;
    let temp = (*velocity + omega * change) * dt;
    *velocity = (*velocity - omega * temp) * decay;
    let output = target + (change + temp) * decay;

    // 越过目标就钉住并把残余速度清零：否则下一帧会从目标另一侧往回弹
    if (wanted - current).signum() * (output - wanted).signum() > 0.0 {
        *velocity = 0.0;
        return wanted;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::CameraFollowSettings;

    fn spring_settings(smooth_time: f32, max_speed: f32) -> CameraFollowSettings {
        CameraFollowSettings {
            strategy: CameraFollowStrategy::SmoothDamp,
            spring: SpringParams {
                smooth_time,
                max_speed,
            },
            ..default()
        }
        .sanitized()
    }

    fn app_with(input: FollowInput) -> App {
        let mut app = App::new();
        app.insert_resource(input)
            .insert_resource(spring_settings(0.3, 12.0))
            .init_resource::<CameraFollowState>()
            .insert_resource(Time::<()>::default())
            .add_systems(Update, smooth_damp_follow);
        app
    }

    /// 弹簧的两条硬性要求：不许过冲、必须单调收敛
    #[test]
    fn spring_never_overshoots_and_converges() {
        let mut app = app_with(FollowInput {
            translation: Vec3::ZERO,
            focus_height: 1.3,
            valid: true,
            ..default()
        });
        // 第一帧先落到「地面上的焦点」
        app.update();
        let before = app.world().resource::<CameraFollowState>().focus().y;

        // 角色跳起 1.5m：焦点要平滑上追，而不是一帧到位
        let target = before + 1.5;
        app.world_mut().resource_mut::<FollowInput>().translation = Vec3::new(0.0, 1.5, 0.0);

        let mut previous = before;
        for frame in 0..240 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(1.0 / 120.0));
            app.update();
            let focus = app.world().resource::<CameraFollowState>().focus().y;
            assert!(
                focus <= target + 1e-3,
                "第 {frame} 帧过冲：{focus} > {target}"
            );
            assert!(
                focus >= previous - 1e-5,
                "第 {frame} 帧回弹：{previous} -> {focus}"
            );
            previous = focus;
        }
        assert!((previous - target).abs() < 1e-2, "两秒后该到位：{previous}");
    }

    /// 起手一帧只走一点点：平滑的观感来自「追不上」，而不是一步到位
    #[test]
    fn spring_lags_on_the_first_frame() {
        let mut app = app_with(FollowInput {
            translation: Vec3::ZERO,
            focus_height: 1.3,
            valid: true,
            ..default()
        });
        app.update();
        let before = app.world().resource::<CameraFollowState>().focus().y;

        app.world_mut().resource_mut::<FollowInput>().translation = Vec3::new(0.0, 1.0, 0.0);
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
        app.update();

        let state = app.world().resource::<CameraFollowState>();
        let moved = state.focus().y - before;
        assert!(moved > 0.0, "弹簧必须动起来");
        assert!(moved < 0.2, "一帧不该走完 1m，实际 {moved}");
        assert!(state.lag() > 0.8);
    }

    /// 0 帧间隔不能把速度算成 NaN / 无穷（暂停、掉帧时会发生）
    #[test]
    fn zero_delta_time_is_harmless() {
        let mut app = app_with(FollowInput {
            translation: Vec3::ZERO,
            focus_height: 1.3,
            valid: true,
            ..default()
        });
        app.update();
        app.world_mut().resource_mut::<FollowInput>().translation = Vec3::new(0.0, 1.0, 0.0);
        app.update(); // Time 没推进，dt = 0

        let state = app.world().resource::<CameraFollowState>();
        assert!(state.focus().is_finite());
        assert!(state.speed().is_finite());
    }

    /// 最大速度只当安全阀：压住之后追得更慢，但不改变收敛方向
    #[test]
    fn max_speed_slows_the_chase() {
        let capped = SpringParams {
            smooth_time: 0.3,
            max_speed: 1.0,
        };
        let uncapped = SpringParams {
            smooth_time: 0.3,
            max_speed: 0.0,
        };

        let target = 6.0;
        let mut capped_pos = 0.0;
        let mut capped_vel = 0.0;
        let mut free_pos = 0.0;
        let mut free_vel = 0.0;
        for _ in 0..6 {
            capped_pos = smooth_damp_axis(
                capped_pos,
                target,
                &mut capped_vel,
                capped.smooth_time,
                capped.max_speed,
                1.0 / 60.0,
            );
            free_pos = smooth_damp_axis(
                free_pos,
                target,
                &mut free_vel,
                uncapped.smooth_time,
                uncapped.max_speed,
                1.0 / 60.0,
            );
        }
        assert!(
            target - capped_pos > target - free_pos,
            "限速后应该落后更多：{} vs {}",
            target - capped_pos,
            target - free_pos
        );
    }
}
