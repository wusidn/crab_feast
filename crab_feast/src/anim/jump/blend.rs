//! 姿态混合：把跳跃剪辑按权重混进移动姿态，本地与远端共用。

use bevy::animation::{AnimationClip, AnimationTargetId};
use bevy::prelude::Assets;

use crate::anim::core::ClipSet;
use crate::anim::core::pose::{BonePose, Pose};

use super::state::JumpAction;

/// 动作层求值：给驱动端调用，按权重混进移动姿态。
pub(crate) fn apply_action(
    action: &JumpAction,
    clips: &ClipSet,
    assets: &Assets<AnimationClip>,
    targets: &[AnimationTargetId],
    rest: &std::collections::HashMap<AnimationTargetId, BonePose>,
    pose: &mut Pose,
) {
    blend_clip_pose(
        action.clip_name(),
        action.clip_time,
        action.weight,
        clips,
        assets,
        targets,
        rest,
        pose,
    );
}

/// 把某条剪辑在 `clip_time` 的姿态按 `weight` 混进 `pose`。本地动作层与远端
/// 观察层共用：两边采样的必须是同一套骨骼映射与静息姿态。
#[allow(clippy::too_many_arguments)]
pub(crate) fn blend_clip_pose(
    clip: &str,
    clip_time: f32,
    weight: f32,
    clips: &ClipSet,
    assets: &Assets<AnimationClip>,
    targets: &[AnimationTargetId],
    rest: &std::collections::HashMap<AnimationTargetId, BonePose>,
    pose: &mut Pose,
) {
    if weight <= 0.0 {
        return;
    }
    let Some(handle) = clips.handle(clip) else {
        return;
    };
    let Some(asset) = assets.get(handle) else {
        return;
    };
    let action_pose = Pose::sample(asset, targets, rest, clip_time);
    pose.blend_in_place(&action_pose, weight);
}
