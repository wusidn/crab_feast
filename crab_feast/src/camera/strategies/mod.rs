//! 跟随策略的实现层：**一个策略一个文件、一个插件**。
//!
//! 每个策略插件只做两件事：往 [`CameraSet::Follow`](crate::camera::CameraSet)
//! 挂自己的系统，并 `run_if(strategy_is(自己))`。选择面板里换策略时，
//! 其余策略的系统不会执行、查询也不会建立。

mod ground_anchor;
mod rigid;
mod spring;

use bevy::prelude::*;

pub use ground_anchor::{
    ANCHOR_SPEED_RANGE, ANCHOR_TIME_RANGE, AnchorParams, DEAD_ZONE_RANGE, GroundAnchorPlugin,
};
pub use rigid::RigidFollowPlugin;
pub use spring::{SmoothDampFollowPlugin, SpringParams};

/// 把所有策略插件收在一个入口，`CameraPlugin` 只认这一个
pub struct FollowStrategiesPlugin;

impl Plugin for FollowStrategiesPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            RigidFollowPlugin,
            SmoothDampFollowPlugin,
            GroundAnchorPlugin,
        ));
    }
}
