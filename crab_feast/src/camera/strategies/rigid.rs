//! 刚性绑定：焦点每帧钉在角色身上。
//!
//! 这是最初的行为，保留下来有两个用途：一是当「零延迟」的对照样本，
//! 二是排查「画面抖到底是策略的锅还是物理/动画的锅」——切回它，
//! 如果还在抖，那抖动就不来自跟随。

use bevy::prelude::*;

use crate::camera::{
    CameraFollowSettings, CameraFollowState, CameraFollowStrategy, CameraSet, FollowInput,
    strategy_is,
};

pub struct RigidFollowPlugin;

impl Plugin for RigidFollowPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            rigid_follow
                .in_set(CameraSet::Follow)
                .run_if(strategy_is(CameraFollowStrategy::Rigid)),
        );
    }
}

/// 焦点 = 理想焦点，速度清零。除了瞬移硬切，没有第三种情况。
fn rigid_follow(
    input: Res<FollowInput>,
    settings: Res<CameraFollowSettings>,
    mut follow: ResMut<CameraFollowState>,
) {
    if !input.valid {
        return;
    }
    let target = input.ideal_focus();
    if follow.snap_if_needed(target, settings.snap_distance) {
        return;
    }
    follow.target = target;
    follow.focus = target;
    follow.velocity = Vec3::ZERO;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(input: FollowInput) -> App {
        let mut app = App::new();
        app.insert_resource(input)
            .init_resource::<CameraFollowSettings>()
            .init_resource::<CameraFollowState>()
            .add_systems(Update, rigid_follow);
        app
    }

    /// 刚性绑定：焦点正好落在「角色 + 焦点高度」，没有滞后
    #[test]
    fn focus_lands_exactly_on_the_character() {
        let mut app = app_with(FollowInput {
            translation: Vec3::new(1.0, 0.5, 2.0),
            focus_height: 1.3,
            valid: true,
            ..default()
        });
        app.update();

        let state = app.world().resource::<CameraFollowState>();
        assert_eq!(state.focus(), Vec3::new(1.0, 1.8, 2.0));
        assert!(state.lag() < 1e-6);
        assert_eq!(state.speed(), 0.0);
    }

    /// 角色跳起 1m：刚性绑定必须跟着上 1m（这正是它的问题所在，也是对照基准）
    #[test]
    fn jump_moves_the_focus_one_to_one() {
        let mut app = app_with(FollowInput {
            translation: Vec3::ZERO,
            focus_height: 1.3,
            valid: true,
            ..default()
        });
        app.update();

        app.world_mut().resource_mut::<FollowInput>().translation = Vec3::new(0.0, 1.0, 0.0);
        app.update();

        let state = app.world().resource::<CameraFollowState>();
        assert!((state.focus().y - 2.3).abs() < 1e-6);
    }

    /// 没有角色时什么都不做，焦点保持原样
    #[test]
    fn invalid_input_is_ignored() {
        let mut app = app_with(FollowInput::default());
        app.update();
        let state = app.world().resource::<CameraFollowState>();
        assert!(!state.is_ready());
        assert_eq!(state.focus(), Vec3::ZERO);
    }
}
