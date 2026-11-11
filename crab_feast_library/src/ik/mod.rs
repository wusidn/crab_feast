//! 与作品无关的骨骼数学层：IK 求解、锁脚状态。
//!
//! 这两块都只依赖 `bevy::math`（向量 / 四元数），不碰 ECS、实体、资源，
//! 所以既能在单人游戏里用，也能原样复用到联机时每个可见角色各自的骨架实例上。

pub mod foot_lock;
pub mod two_bone;

pub use foot_lock::{FootContact, FootLock, FootLockConfig};
pub use two_bone::{aim_world_rotation, solve_two_bone, TwoBoneSolution};
