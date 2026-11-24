//! 插件注册：锁存系统与驱动系统的调度。

use bevy::prelude::*;

use crate::anim::AnimationSet;

use super::drive::{drive_jump, latch_jump_request};
use super::settings::JumpSettings;
use super::state::{JumpAction, JumpPolicy};
use super::tuning::JumpProfile;
use super::tuning::JumpTunings;

pub struct JumpPlugin;

impl Plugin for JumpPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<JumpAction>()
            .init_resource::<JumpSettings>()
            .init_resource::<JumpPolicy>()
            .init_resource::<JumpTunings>()
            // 锁存按键要排在动作层之前：同一帧按下就在这一帧参与窗口判定
            // 设置面板的起跳速度灌进档位表，必须排在所有读档位表的系统之前
            // （本地起跳 / 远端预测 / 上行日志都在 Update 里读）
            .add_systems(PreUpdate, sync_jump_settings)
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

/// 把设置面板的起跳速度灌进四档档位表。
///
/// 走「改表」而不是「每个读表的地方各自叠一层设置」：`drive_jump` 的系统参数
/// 已经顶到 Bevy 的 16 个上限，再加一个 `Res<JumpSettings>` 直接编不过；而读表
/// 的地方有十几处（本地起跳、远端预测、日志、服务端下发），逐处叠必然漏一处。
///
/// 四档共用同一个起跳速度旋钮：玩家调的是「我这一跳跳多高」，不是「站跳和跑跳
/// 要分别调多高」。档位表里四行的 `launch_vy` 本来就是同一个常量，所以默认值下
/// 灌进去不会改变任何现有手感。
fn sync_jump_settings(settings: Res<JumpSettings>, mut tunings: ResMut<JumpTunings>) {
    // 资源刚插入那一帧 `is_changed()` 也是真，不必额外判断 `is_added()`
    if !settings.is_changed() {
        return;
    }
    for profile in [
        JumpProfile::Stand,
        JumpProfile::Run,
        JumpProfile::Up,
        JumpProfile::Down,
    ] {
        let tuning = tunings.tune(profile);
        *tuning = settings.apply_to(*tuning);
    }
}
