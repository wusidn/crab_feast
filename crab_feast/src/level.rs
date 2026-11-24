//! 关卡：**游戏侧的静态场景设计数据**，两端各自在自己的 ECS 里建实体。
//!
//! 这是游戏内容，不是可复用的通用库：布局、尺寸、外观都属于本作的数据管理，
//! 所以放在 `crab_feast` 而不是 `crab_feast_library`。dedicated server 直接
//! 依赖 game crate，复用同一份 [`LEVEL`]，不另抄一份几何。
//!
//! 联网模式下不复制 `LevelBlock`：地面/方块的尺寸与位置是**设计数据**，
//! 不是运行时状态，客户端和服务端都从同一份 [`LEVEL`] 建静态刚体 + 碰撞体。
//! 这样客户端的物理世界是完整的 —— 角色碰撞、脚部 IK 射线、将来的本地预测
//! 都有真实几何可解，而不是靠客户端补一份"只给射线用"的探针碰撞体。
//!
//! 边界：**只有静态几何走这条路径**。运行时会变的关卡内容（开关门、可破坏
//! 方块、服务端脚本临时摆的障碍）不属于静态场景，必须走复制，否则两端分叉。
//!
//! 字段故意用数组而不是 `Vec3`：这个类型既进场景构建，又可能被将来基于
//! 地图文件的加载器读取，不希望数据形状依赖 glam 的版本（同一形状约定也用在
//! 联网协议的 `NetPos` 上）。

use avian3d::prelude::*;
use bevy::prelude::*;

/// 地面半边长（米）：地面是 `2 * GROUND_HALF` 见方的板子。
pub const GROUND_HALF: f32 = 30.0;
/// 地面厚度（米，完整厚度）。顶面必须落在 `y = 0`——那是角色的落脚高度。
pub const GROUND_THICKNESS: f32 = 0.2;
/// 方块边长（米）。
pub const CUBE_SIZE: f32 = 1.0;
/// 角色胶囊的半高（含两端半球）：`Collider::capsule(0.2, 1.0)` = 1.0 / 2 + 0.2。
pub const PLAYER_CAPSULE_HALF_HEIGHT: f32 = 0.7;
/// 角色出生高度：胶囊底面离地 2cm，落地没有可见下坠，也不会穿进地面。
///
/// 客户端本地局和服务端联机局都用它——以前本地写 2.0、服务端写 1.0，
/// 一个掉 1.3m 一个掉 0.3m，看起来都像「一出生就在下坠」。
pub const PLAYER_SPAWN_Y: f32 = PLAYER_CAPSULE_HALF_HEIGHT + 0.02;
/// 方块中心位置。y 取边长一半，底面正好压在地面 `y = 0` 上。
///
/// 别摆在出生列上：出生点是 `x = player_id * 1.5, z = 0`，胶囊半径 0.2，
/// 方块半边长 0.5，所以 `x ≈ 0` 且 `|z| < 0.7` 的位置玩家一出生就插在里面，
/// 会被物理直接弹飞。
pub const CUBE_CENTERS: [Vec3; 2] = [Vec3::new(0.0, 0.5, 3.0), Vec3::new(1.0, 0.5, 1.0)];

/// 关卡里的全部静态长方体。两端都按这个顺序、这些参数建实体。
pub const LEVEL: [LevelBlock; 3] = [
    LevelBlock {
        center: [0.0, -GROUND_THICKNESS / 2.0, 0.0],
        size: [GROUND_HALF * 2.0, GROUND_THICKNESS, GROUND_HALF * 2.0],
        look: LevelLook::Ground,
    },
    LevelBlock {
        center: [CUBE_CENTERS[0].x, CUBE_CENTERS[0].y, CUBE_CENTERS[0].z],
        size: [CUBE_SIZE, CUBE_SIZE, CUBE_SIZE],
        look: LevelLook::Cube,
    },
    LevelBlock {
        center: [CUBE_CENTERS[1].x, CUBE_CENTERS[1].y, CUBE_CENTERS[1].z],
        size: [CUBE_SIZE, CUBE_SIZE, CUBE_SIZE],
        look: LevelLook::Cube,
    },
];

/// 关卡里一块长方体（地面 / 方块都走它）。
///
/// 尺寸是**完整边长**：`center + size / 2` 是顶面，直接用 Avian 的
/// `Collider::cuboid(size.x, size.y, size.z)`；客户端再用同一个 size 造 mesh，
/// 画面与碰撞体就不会差出一截。
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct LevelBlock {
    /// 几何中心（世界坐标，米）
    pub center: [f32; 3],
    /// 完整尺寸（世界轴对齐，米）
    pub size: [f32; 3],
    /// 客户端该画成什么（纯外观，不参与物理）
    pub look: LevelLook,
}

/// `LevelBlock` 的外观？只给客户端选材质，服务端不看它。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelLook {
    /// 地面：贴黑白格，一格一米（走滑步一眼能看出来）
    Ground,
    /// 普通方块：纯色
    Cube,
}

/// 在本地 ECS 里构建整张静态场景。
///
/// 服务端和客户端都调这个：一边一份实体，各自跑自己的物理。
/// - 服务端：这些实体是权威碰撞几何；
/// - 客户端：同一份几何进本地物理世界，供角色碰撞与射线查询。
///
/// 实体上只挂 `RigidBody::Static + Collider + Transform + LevelBlock`，
/// 不带 `Replicate`：静态几何不复制，靠两端同一份 [`LEVEL`] 保持一致。
pub fn spawn_static_level(commands: &mut Commands) {
    for block in LEVEL {
        let center = Vec3::from_array(block.center);
        commands.spawn((
            RigidBody::Static,
            Collider::cuboid(block.size[0], block.size[1], block.size[2]),
            Transform::from_translation(center),
            // 物理 `Position` 必须显式挂：`transform_to_position` 在场景里被关掉
            // （见 scene.rs），静态体不再由 Transform 回读同步。漏挂时 Position
            // 停在原点，碰撞体被抬到地面 y=0 之上，角色一出生就深度穿透，被接触
            // 求解器的穿透补正速度反复上推成悬浮，再被和解当成误差吸附拉回。
            Position::from_xyz(center.x, center.y, center.z),
            block,
            Name::new(format!("level-{:?}", block.look)),
        ));
    }
}
