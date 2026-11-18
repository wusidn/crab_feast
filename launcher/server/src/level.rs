//! 服务端的关卡内容：**每一块关卡物体都是这里的 ECS 实体**。
//!
//! 客户端不再自己按常量摆方块——它只渲染服务端复制下来的
//! [`LevelBlock`]。所以改了这里的方块，画面自动跟着变，不会再出现
//! 「两边各写一份、改一边忘一边」。
//!
//! 服务端没有渲染层，`Mesh3d` 之类一个都不挂；实体上只有
//! `RigidBody::Static + Collider + Transform + LevelBlock + Replicate`。

use avian3d::prelude::*;
use bevy::prelude::*;
use crab_feast_library::level::{LevelBlock, LevelLook};
use lightyear::prelude::Replicate;

/// 地面半边长（米）：地面是 `2 * GROUND_HALF` 见方的板子。
const GROUND_HALF: f32 = 30.0;
/// 地面厚度（米，完整厚度）。顶面必须落在 `y = 0`——那是角色的落脚高度。
const GROUND_THICKNESS: f32 = 0.2;
/// 方块边长（米）。
const CUBE_SIZE: f32 = 1.0;
/// 方块中心位置。y 取边长一半，底面正好压在地面 `y = 0` 上。
///
/// 别摆在出生列上：出生点是 `x = player_id * 1.5, z = 0`，胶囊半径 0.2，
/// 方块半边长 0.5，所以 `x ≈ 0` 且 `|z| < 0.7` 的位置玩家一出生就插在里面，
/// 现在服务端有碰撞，会被直接弹飞。
const CUBE_CENTERS: [Vec3; 2] = [Vec3::new(0.0, 0.5, 3.0), Vec3::new(1.0, 0.5, 1.0)];

/// 生成全部关卡实体。由 `setup` 调用一次。
pub(super) fn spawn_level(commands: &mut Commands) {
    spawn_block(
        commands,
        Vec3::new(0.0, -GROUND_THICKNESS / 2.0, 0.0),
        Vec3::new(GROUND_HALF * 2.0, GROUND_THICKNESS, GROUND_HALF * 2.0),
        LevelLook::Ground,
    );
    for center in CUBE_CENTERS {
        spawn_block(commands, center, Vec3::splat(CUBE_SIZE), LevelLook::Cube);
    }
}

/// 一块关卡：静态刚体 + 碰撞体 + 复制描述。
fn spawn_block(commands: &mut Commands, center: Vec3, size: Vec3, look: LevelLook) {
    commands.spawn((
        RigidBody::Static,
        Collider::cuboid(size.x, size.y, size.z),
        Transform::from_translation(center),
        LevelBlock {
            center: center.to_array(),
            size: size.to_array(),
            look,
        },
        // 关卡是静态的：`replicate_once` 只在实体生成时发一次
        Replicate::default(),
        Name::new(format!("level-{look:?}")),
    ));
}
