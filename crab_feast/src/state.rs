//! 顶层状态与会话资源。
//!
//! 进游戏的路径固定为：
//! `AssetLoading`（加载启动资源）→ `Lobby`（服务器列表 / 本地游戏）→ `Login`
//! （统一登录：连服务器或本地登录）→ `Game`（建场景 / 角色 / 相机）→ 世界就绪
//! 后才开输入 UI。
//!
//! `SessionReady` 是「世界真的能操作了」的唯一判据：角色在场上、模型和动画图
//! 都建好了、相机有目标可跟。输入 UI 与键盘绑定都等它。

use std::net::SocketAddr;

use bevy::prelude::*;

#[derive(States, Default, Clone, Eq, PartialEq, Debug, Hash)]
pub enum GameState {
    #[default]
    AssetLoading,
    /// 首页：局域网服务器列表 + 本地游戏
    Lobby,
    /// 统一登录流程：连接服务器 / 本地登录
    Login,
    /// 登录成功：建场景、角色、相机
    Game,
    Pause,
}

/// 这一局从哪儿来。
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMode {
    /// 本地单机：客户端自己就是权威，不发包
    Local,
    /// 联机：连指定服务器，登录成功后由服务器分配角色
    Online { server: SocketAddr },
}

impl Default for SessionMode {
    fn default() -> Self {
        Self::Local
    }
}

/// 世界是否准备就绪（角色 + 模型 + 动画图 + 相机目标都在）。
///
/// 为真之前：输入 UI 不生成、键盘移动不采样、`Preparation` 遮罩盖着屏幕。
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct SessionReady(pub bool);

/// 从命令行带进来的「进大厅后直接登录」（冒烟测试 / 开发用）。
///
/// 进 `Lobby` 时消费一次；正常启动时是 `None`，停在服务器列表。
#[derive(Resource, Default, Debug)]
pub struct AutoJoin(pub Option<SessionMode>);

/// 回到大厅时要显示的一句话（登录失败、掉线等）。
#[derive(Resource, Default, Debug, Clone)]
pub struct LobbyNotice(pub Option<String>);

/// 取消登录：网络侧收到后断开正在握手的连接。
#[derive(Message, Clone, Copy, Debug, Default)]
pub struct AbortSession;
