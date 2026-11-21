//! 插件注册：锁存系统与驱动系统的调度。

use bevy::prelude::*;

use crate::anim::AnimationSet;

use super::drive::{drive_jump, latch_jump_request};
use super::state::{JumpAction, JumpPolicy};
use super::tuning::JumpTunings;

pub struct JumpPlugin;

impl Plugin for JumpPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<JumpAction>()
            .init_resource::<JumpPolicy>()
            .init_resource::<JumpTunings>()
            // 锁存按键要排在动作层之前：同一帧按下就在这一帧参与窗口判定
            .add_systems(
                Update,
                latch_jump_request
                    .in_set(AnimationSet::Publish)
                    .run_if(in_state(crate::GameState::Game)),
            )
            .add_systems(
                Update,
                drive_jump
                    .in_set(AnimationSet::Drive)
                    .run_if(in_state(crate::GameState::Game)),
            );
    }
}
