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

/// 原地转向的姿态权重映射：误差小于 `TURN_MIN_DEG` 不碰转向剪辑，
/// 大于 `TURN_FULL_DEG` 就完全交给 LeftTurn / RightTurn。
const TURN_MIN_DEG: f32 = 8.0;
const TURN_FULL_DEG: f32 = 70.0;

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
/// 两个分支都读 [`FacingState::error_deg`]，因为混合空间的 yaw 轴本来就是
/// 「相对角色正前方的偏角」：
/// - 在走：这个角就是走位方向与脸向的夹角，侧向 / 倒退剪辑负责把脚带对方向；
/// - 站着但在追相机：同一个位置放的是 LeftTurn / RightTurn（见 spaces.rs），
///   误差越大权重越高，于是转相机时角色用转向动画跟过来。
fn drive_locomotion(
    focus: Res<AnimFocus>,
    avatars: Query<(&FacingState, &LocomotionIntent)>,
    mut mixer: ResMut<LocomotionMixer>,
) {
    if focus.is_focused(LocomotionMixer::ID) || !mixer.core().enabled {
        return;
    }

    let (mut yaw, mut speed) = match avatars.iter().next() {
        Some((state, intent)) => {
            // 混合空间的偏角右正左负，error_deg 是「需要绕 +Y 转」即左转为正，符号相反
            let right_deg = -state.error_deg;
            if intent.magnitude > 1e-3 {
                (right_deg, intent.magnitude)
            } else {
                // 原地转向：把误差映射到 0..±90，端点正好落在纯转向剪辑上
                let amount = ((right_deg.abs() - TURN_MIN_DEG) / (TURN_FULL_DEG - TURN_MIN_DEG))
                    .clamp(0.0, 1.0);
                (right_deg.signum() * amount * 90.0, 0.0)
            }
        }
        None => (0.0, 0.0),
    };

    // 空间只覆盖 ±90°：更大的偏角折回半球，倒退由速度符号承担（见空间定义）
    if yaw.abs() > 90.0 {
        yaw = yaw.signum() * (180.0 - yaw.abs());
        speed = -speed;
    }

    write_params(mixer.core_mut(), [yaw, speed]);
}

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
