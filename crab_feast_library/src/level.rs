//! 「关卡方块」的数据形状 —— 只是组件类型，**不含任何关卡内容**。
//!
//! 哪几块、多大、摆在哪，由**权威侧**（联网时是 `launcher/server`，单机时是本地
//! 客户端自己）spawn 成实体决定；客户端在联网模式下不参与定义，只渲染服务端
//! 复制下来的 `LevelBlock`。所以这里若写成一份共享的方块列表，等于让客户端
//! 在自己编造关卡内容，服务端改了客户端也照旧——那是错的。
//!
//! 字段故意用数组而不是 `Vec3`：这个类型会进复制协议，不希望序列化格式
//! 随 glam 版本变化（同 [`crate::net::NetPos`]）。

use bevy::prelude::*;

/// 关卡里一块长方体（地面 / 方块都走它）。
///
/// 尺寸是**完整边长**：`center + size / 2` 是顶面，直接用 Avian 的
/// `Collider::cuboid(size.x, size.y, size.z)`；客户端再用同一个 size 造 mesh，
/// 画面与碰撞体就不会差出一截。
#[derive(Component, Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "net", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "net", derive(serde::Serialize, serde::Deserialize))]
pub enum LevelLook {
    /// 地面：贴黑白格，一格一米（走滑步一眼能看出来）
    Ground,
    /// 普通方块：纯色
    Cube,
}
