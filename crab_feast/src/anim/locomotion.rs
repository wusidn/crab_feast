//! 游戏侧参数驱动：把输入与控制状态换算成混合器参数，再把混合出来的地速送回移动。
//!
//! UI 不写这些值——编辑器打开时 [`AnimFocus`] 会指向被聚焦的混合器，这里的系统
//! 就让位（见每个系统开头的 `is_focused` 判断），所以预览拖滑杆不会被游戏状态顶掉。
//! 反向的那一半是 [`sync_move_speed`]：角色的实际移动速度取当前姿态的等效地速。

use bevy::prelude::*;

use crate::camera::{CAMERA_PITCH_RANGE, CameraFacing};
use crate::input::{FacingState, LocomotionIntent, MovementController, camera_view_yaw, wrap_pi};
use crate::state::GameState;

use super::AnimationSet;
use super::core::{BlendMixer, MixerCore};
use super::driver::{AnimFocus, PoseDriver, drive_pose};
use super::mixers::{LocomotionMixer, LookMixer, RegisteredMixer};

pub struct LocomotionPlugin;

impl Plugin for LocomotionPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (drive_locomotion, drive_look)
                .in_set(AnimationSet::Drive)
                // 菜单里不跑角色，参数保持编辑器留下的值
                .run_if(in_state(GameState::Game)),
        )
        .add_systems(
            PostUpdate,
            // 地速来自本帧的姿态求值，所以排在驱动之后；下一帧的移动系统读它
            sync_move_speed
                .after(drive_pose)
                .run_if(in_state(GameState::Game)),
        );
    }
}

/// 混合输出的等效地速 → [`MovementController`] 的目标地速。
///
/// 这一步是「预览里看到的档位」与「游戏里跑出来的速度」对齐的关键：
/// 走 / 跑 / 侧向的快慢全由剪辑自己的净步幅与时长决定，改采样点位置就等于改移速，
/// 不需要在代码里再维护一套走路速度、跑步速度。
fn sync_move_speed(driver: Res<PoseDriver>, mut controllers: Query<&mut MovementController>) {
    // 还没求值过（模型未就绪）就保留默认值；预览单条剪辑时混合空间仍然在求值，
    // 地速依旧新鲜，角色不会停在最后一个档位上滑走
    if !driver.evaluated {
        return;
    }
    for mut controller in &mut controllers {
        if (controller.speed - driver.ground_speed).abs() > 1e-3 {
            controller.speed = driver.ground_speed;
        }
    }
}

/// 移动意图 + 朝向误差 → `yaw` / `speed`。
///
/// 走起来时 `yaw` 就是走位方向与脸向的夹角（混合空间的 yaw 轴本来就是
/// 「相对角色正前方的偏角」，现在覆盖整圈 ±180°），侧向 / 反放的走跑剪辑
/// 负责把脚带对方向。
///
/// 站着的时候一律回 `(0, 0)` 也就是 Idle：原地转身靠 [`FacingState`] 驱动的
/// 身体旋转本身，不再往混合空间里送偏角。采样点布局里没有「原地转向」这一档
/// （见 spaces.rs 的默认定义），硬送偏角只会混出侧移走的姿态——脚在原地踏步，
/// 人却在转，看着更假。
///
/// 不额外降速：之前试过按误差收速度，结果是「转身期间被拽慢」，观感更像拖沓
/// 而不是转身。
fn drive_locomotion(
    focus: Res<AnimFocus>,
    time: Res<Time>,
    avatars: Query<(&FacingState, &LocomotionIntent)>,
    mut mixer: ResMut<LocomotionMixer>,
) {
    if focus.is_focused(LocomotionMixer::ID) || !mixer.core().enabled {
        return;
    }

    let [yaw, speed] = params_for(
        avatars.iter().next().map(|(state, intent)| {
            (
                state.error_deg,
                if intent.magnitude > 1e-3 {
                    intent.magnitude
                } else {
                    0.0
                },
            )
        }),
    );

    // 参数轴限速：参数一跳，权重就跟着跳，姿态就是一帧台阶。yaw 现在覆盖整圈，
    // 所以限速要按**最短弧**走，否则 180° 附近抖一下会让参数绕一整圈。
    let dt = time.delta_secs();
    let axes = mixer.core().space.params.clone();
    let core = mixer.core_mut();
    let mut values = [yaw, speed];
    for (index, axis) in axes.iter().enumerate() {
        let Some(current) = core.params.get(axis).copied() else {
            continue;
        };
        // 轴名对不上（编辑器里改过轴序）就不限速，宁可跳也别限错轴
        values[index] = match axis.as_str() {
            "yaw" => slew_deg(current, values[index], PARAM_SLEW[0] * dt),
            "speed" => slew(current, values[index], PARAM_SLEW[1] * dt),
            _ => values[index],
        };
    }
    write_params(core, values);
}

/// 角度归一到 (-180, 180]
fn wrap_deg(deg: f32) -> f32 {
    (deg + 180.0).rem_euclid(360.0) - 180.0
}

/// 按最短弧限速：跨 ±180 时不绕远路
fn slew_deg(current: f32, target: f32, max_step: f32) -> f32 {
    let delta = wrap_deg(target - current);
    wrap_deg(current + delta.clamp(-max_step, max_step))
}

fn slew(current: f32, target: f32, max_step: f32) -> f32 {
    let delta = target - current;
    if delta.abs() > max_step {
        current + delta.signum() * max_step
    } else {
        target
    }
}

/// 朝向误差 + 移动幅度 → `yaw` / `speed`（未经限速的原始目标）。
///
/// `error_deg` 是「还需绕 +Y 转多少」，左转为正；混合空间的 yaw 右正左负，所以取反。
/// 就这么直给：yaw 覆盖整圈 ±180°，速度恒为正，转身时参数沿圆周连续滑动，
/// 不需要折回、不需要给速度翻符号。
fn params_for(avatar: Option<(f32, f32)>) -> [f32; 2] {
    let Some((error_deg, magnitude)) = avatar else {
        return [0.0, 0.0];
    };
    if magnitude <= 1e-3 {
        // 站住不动：没有移动方向可言，偏角也别送，直接站姿
        return [0.0, 0.0];
    }
    [-error_deg, magnitude]
}

/// 参数轴每帧最大变化量（`yaw` 度/秒，`speed` 单位/秒），顺序同空间定义的轴。
/// 只用来消台阶，别当成滤波：正常操作下这一项几乎不生效。
const PARAM_SLEW: [f32; 2] = [700.0, 20.0];

/// 相机与角色的朝向差 → `yaw` / `pitch`。
///
/// 相机朝向已经不再等于角色朝向，所以这两个轴现在都有真实来源了：
/// yaw 是镜头视线相对角色正前方的偏角（上半身该拧多少），pitch 是俯仰。
fn drive_look(
    focus: Res<AnimFocus>,
    camera: Res<CameraFacing>,
    avatars: Query<&FacingState>,
    mut mixer: ResMut<LookMixer>,
) {
    if focus.is_focused(LookMixer::ID) || !mixer.core().enabled {
        return;
    }
    // 相机视线朝向（换算到角色旋转约定）减去角色当前朝向 = 上半身要拧的角度，
    // ±90° 归一化到 ±1
    let yaw = avatars
        .iter()
        .next()
        .map(|state| {
            let offset = wrap_pi(camera_view_yaw(&camera) - state.yaw).to_degrees();
            (offset / 90.0).clamp(-1.0, 1.0)
        })
        .unwrap_or(0.0);
    let pitch = (camera.pitch / CAMERA_PITCH_RANGE.1).clamp(-1.0, 1.0);

    write_params(mixer.core_mut(), [yaw, pitch]);
}

/// 按空间定义的轴顺序写入两个参数，避免这里再抄一遍轴名
fn write_params(core: &mut MixerCore, values: [f32; 2]) {
    let axes = core.space.params.clone();
    for (axis, value) in axes.iter().zip(values) {
        core.params.insert(axis.clone(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_sends_no_direction() {
        assert_eq!(params_for(None), [0.0, 0.0]);
        assert_eq!(params_for(Some((120.0, 0.0))), [0.0, 0.0]);
    }

    #[test]
    fn forward_keeps_full_speed() {
        let [yaw, speed] = params_for(Some((0.0, 1.0)));
        assert_eq!(yaw, 0.0);
        assert!((speed - 1.0).abs() < 1e-6);
    }

    #[test]
    fn sideways_keeps_full_speed() {
        // 侧向走和正走同速：转身是身体的事，不该把地速一起拽下来
        let [yaw, speed] = params_for(Some((-90.0, 0.5)));
        assert!((yaw - 90.0).abs() < 1e-6);
        assert!((speed - 0.5).abs() < 1e-6, "{speed}");
    }

    #[test]
    fn any_direction_keeps_speed_positive() {
        // 整圈都不给速度翻符号：翻符号必然经过 speed=0，而那里只有 Idle，
        // 转身时腿就会闪一下。这是 A 方案要守住的不变量。
        for error in [0.0, 45.0, 90.0, 91.0, 120.0, 150.0, 179.0, 180.0,
                      -45.0, -91.0, -150.0, -180.0] {
            let [yaw, speed] = params_for(Some((error, 0.8)));
            assert!((speed - 0.8).abs() < 1e-6, "error {error} speed {speed}");
            assert!((yaw + error).abs() < 1e-6, "error {error} yaw {yaw}");
        }
    }

    #[test]
    fn yaw_sweeps_the_whole_circle_monotonically() {
        // 身体从正后方转到正前方：yaw 一路 -180 → 0，中途没有回折、没有翻符号
        let mut last = params_for(Some((180.0, 1.0)))[0];
        assert!((last + 180.0).abs() < 1e-6, "{last}");
        for deg in (0..=179).rev() {
            let yaw = params_for(Some((deg as f32, 1.0)))[0];
            assert!(yaw >= last - 1e-6, "deg {deg}: {yaw} < {last}");
            last = yaw;
        }
        assert!(last.abs() < 1e-6, "{last}");
    }

    #[test]
    fn yaw_slew_takes_the_short_way_round() {
        // 179 → -179 是同一处，最短弧只差 2°，不该绕 358°
        let step = slew_deg(179.0, -181.0, 700.0 * 0.016);
        assert!((step.abs() - 180.0).abs() < 1e-3 || step > 175.0, "{step}");
        let near = slew_deg(170.0, 180.0, 5.0);
        assert!((near - 175.0).abs() < 1e-6, "{near}");
    }
}
