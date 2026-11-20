//! 角色脚下接地探测：客户端动画层与服务端权威物理共用同一套几何口径。
//!
//! 以前两端各写一份：客户端用「脚底距地 ≤ 0.10m + 残余上升速度」判接地，
//! 服务端却用「`y <= 1.15`」的绝对高度阈值。后者只在 `y = 0` 的平地上成立，
//! 一走到 1m 方块顶上（胶囊中心 `y = 1.7`）就永远判「不在地上」，蓄力起跳
//! 直接被取消——站在方块上怎么按都不跳。统一成向下射线后，任意台面都成立。

use avian3d::prelude::{SpatialQuery, SpatialQueryFilter};
use bevy::math::Dir3;
use bevy::prelude::*;

/// 胶囊半高：`Collider::capsule(0.2, 1.0)` 柱长 1.0、半径 0.2，总高 1.4，
/// 脚底相对胶囊中心向下偏移 0.7m。
pub const CAPSULE_HALF_HEIGHT: f32 = 0.7;
/// 射线起点相对胶囊中心的抬升（米），避免起点埋进地面。
pub const PROBE_RISE: f32 = 0.3;
/// 射线最大探测深度（米）。
pub const PROBE_DEPTH: f32 = 2.0;
/// 接地判定：脚底距地面小于等于这个距离视为落地（米）。
pub const GROUND_CONTACT_DIST: f32 = 0.10;

/// 向下打一条射线，返回命中点的世界高度；未命中返回 `None`。
pub fn ground_height(
    spatial: &SpatialQuery,
    filter: &SpatialQueryFilter,
    origin: Vec3,
) -> Option<f32> {
    let hit = spatial.cast_ray(origin, Dir3::NEG_Y, PROBE_DEPTH, true, filter)?;
    Some(origin.y - hit.distance)
}

/// 脚下射线返回脚底距地面的高度（米）；射线打不中返回 `None`。
pub fn foot_ground_dist(
    spatial: &SpatialQuery,
    filter: &SpatialQueryFilter,
    transform: &Transform,
) -> Option<f32> {
    let origin = transform.translation + Vec3::Y * PROBE_RISE;
    let ground_y = ground_height(spatial, filter, origin)?;
    let foot_y = transform.translation.y - CAPSULE_HALF_HEIGHT;
    Some(foot_y - ground_y)
}

/// 脚底是否踩在地上。`upward_speed` 是当前竖直速度，`noise` 是贴地时接触
/// 求解允许的残余速度上限：高于它才算「真在蹬升」，不把接触噪声当离地。
pub fn grounded(
    spatial: &SpatialQuery,
    avatar: Entity,
    transform: &Transform,
    upward_speed: f32,
    noise: f32,
) -> bool {
    let filter = SpatialQueryFilter::from_excluded_entities([avatar]);
    foot_ground_dist(spatial, &filter, transform)
        .is_some_and(|dist| dist <= GROUND_CONTACT_DIST && upward_speed <= noise)
}
