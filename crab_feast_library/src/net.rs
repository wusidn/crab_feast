//! client / server 共用的网络协议定义。
//!
//! 这个模块只放「两边必须完全一致」的东西：组件、消息、通道。
//! 它不碰任何 IO（UDP / WebTransport / Steam），谁需要哪种传输就在自己的
//! Cargo feature 里打开对应的 lightyear feature。
//!
//! 整个模块挂在 `crab_feast_library` 的 `net` feature 后面：不开 net 的构建
//! （wasm / mobile / 单机）连 lightyear 都不会编译进来。
//!
//! 注册顺序就是协议哈希的一部分，client 与 server 必须走同一个
//! [`CrabProtocolPlugin`]，不要各自手写一遍。

use bevy::prelude::*;
use lightyear::prelude::*;
use serde::{Deserialize, Serialize};

/// 固定 tick 频率。server 的模拟与复制、client 的输入采样都以它为准。
///
/// 60Hz 对应 16.67ms/tick，是「手感够用」与「带宽/CPU 成本」的折中；
/// 后面做 100 人时这个值要降到 30，不要在这里写死成常量再散落各处。
pub const TICK_HZ: f64 = 60.0;

/// 默认监听 / 连接地址。
pub const DEFAULT_ADDR: &str = "127.0.0.1:5000";

/// 服务器推动角色的顶速（米/秒）。
///
/// 两端都要用：服务器拿它写 `LinearVelocity`，客户端拿它把复制下来的
/// `NetMotion.speed` 换算成动画混合空间的 `speed` 轴值（1.0 = 跑档）。
/// 分开写两份必然对不上：真实地速和步频一错位就是滑步。
pub const MOVE_SPEED_MPS: f32 = 3.0;

/// netcode 协议号。两端必须一致，否则握手包会被丢弃（表现为"连不上也不报错"）。
///
/// 上线前改成你自己的随机 u64。
pub const NETCODE_PROTOCOL_ID: u64 = 0x1_0C2B_FEA5;

/// netcode 私钥（32 字节）。**生产环境绝对不能放在客户端**：
/// 正式做法是由后端签发 `ConnectToken`（`lightyear::netcode::prelude::Authentication::Token`），
/// 客户端只拿到 token。这里的明文私钥只用于本地开发 / 自建测试服。
pub const NETCODE_DEV_KEY: [u8; 32] = [0x42; 32];

/// 复制到客户端的角色位置（米）。
///
/// 用数组而不是 `Vec3`：协议层不希望依赖 glam 的 serde 实现，
/// 也不希望序列化格式随 glam 版本变化。
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetPos(pub [f32; 3]);

/// 复制到客户端的角色朝向（绕 Y 的偏航角，弧度）。
///
/// 只同步 yaw 不同步完整 quaternion：本作角色只绕 Y 转（`LockedAxes::ROTATION_LOCKED`），
/// 一个 f32 足够，横滚/俯仰由动画层负责。
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetYaw(pub f32);

/// 复制到客户端的运动状态，给（远端与本地的）动画层用。
///
/// 三件事必须分开，混成一个值就会出三种毛病：
/// - `dir`：**想走的方向**（世界空间 XZ 单位向量，松手时长度 0）。只给转向用，
///   拿实测位移方向当它，顶墙时人就不转身。
/// - `speed`：**实测水平速度**（米/秒，位置差低通）。动画档位用它，拿意向当它，
///   顶墙时人脚在原地跑。
/// - `intent`：**主动度**（本 tick 自己的输入力度 0~1）。0 表示这一帧的位移跟
///   自己的输入无关——被人推、被斜坡带、被平台拖，这时候人是「被搬走的」，
///   不该播走/跑剪辑，否则动画和位移是两套原因，看着就是漂。
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetMotion {
    pub dir: [f32; 2],
    pub speed: f32,
    pub intent: f32,
}

/// 每个玩家在房间里的编号，由 server 分配。
///
/// client 拿它认领「哪个远端实体是我」，不要再依赖连接顺序。
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetPlayerId(pub u8);

/// client → server 的移动输入。
///
/// `dir` 已经是世界空间方向（client 用相机偏航把摇杆方向转过去），
/// 所以 server 不需要知道相机状态；`force` 是档位（0~1）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default)]
pub struct MoveInput {
    pub dir: [f32; 2],
    pub force: f32,
    pub jump: bool,
}

/// client → server：请求加入房间（登录的第一步）。
///
/// 空结构体是有意的：现在没有账号体系，将来要带昵称/背包/皮肤时往这里加字段，
/// 服务端在收到它的时候才创建角色实体，没 Join 的连接不会占一个玩家位。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default)]
pub struct JoinRequest {}

/// server → client：告知它被分配到哪个 `player_id`（落地为一个角色实体）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct Welcome {
    pub player_id: u8,
}

/// 输入通道：不可靠 + 只接受最新序号。
///
/// 输入丢一帧没关系（下一帧会带更新的方向），但**不能被旧包覆盖新包**，
/// 所以用 `SequencedUnreliable` 而不是裸 `UnorderedUnreliable`。
pub struct InputChannel;
/// 控制通道：可靠有序，只走低频关键消息（上行 JoinRequest、下行 Welcome、后续的 RPC）。
///
/// 是双向的：客户端登录要能说话，服务端回执也走这条，没必要为两个方向各开一条。
pub struct ControlChannel;

pub struct CrabProtocolPlugin;

impl Plugin for CrabProtocolPlugin {
    fn build(&self, app: &mut App) {
        // 组件：寄存器决定「哪些字段会进协议哈希」，顺序必须两边一致。
        app.component::<NetPos>().replicate();
        app.component::<NetYaw>().replicate();
        app.component::<NetMotion>().replicate();
        app.component::<NetPlayerId>().replicate_once();
        // 关卡几何**不进协议**：静态场景是设计数据，两端各自从
        // `crab_feast_library::level::LEVEL` 建实体（见 `spawn_static_level`）。
        // 复制一份只读几何除了占握手带宽，还会让客户端出现「复制体 + 本地体」
        // 两份碰撞体，物理里同一个方块被解算两次。

        // 通道：输入要低延迟，控制要可靠。
        //
        // add_direction 不能省：`Transport` 上的收发通道是"插入 (Transport, Client) 时"
        // 由观察者按方向补上的，只调 add_channel 不去注册方向，发送时会报
        // ChannelNotFound 而消息静默丢失。
        app.add_channel::<InputChannel>(ChannelSettings {
            mode: ChannelMode::SequencedUnreliable,
            send_frequency: core::time::Duration::default(),
            priority: 1.0,
            ..default()
        })
        .add_direction(NetworkDirection::ClientToServer);
        app.add_channel::<ControlChannel>(ChannelSettings {
            mode: ChannelMode::OrderedReliable(ReliableSettings::default()),
            send_frequency: core::time::Duration::default(),
            priority: 0.5,
            ..default()
        })
        .add_direction(NetworkDirection::Bidirectional);

        // 消息：JoinRequest / MoveInput 只允许 client→server，Welcome 只允许 server→client。
        app.register_message::<JoinRequest>()
            .add_direction(NetworkDirection::ClientToServer);
        app.register_message::<MoveInput>()
            .add_direction(NetworkDirection::ClientToServer);
        app.register_message::<Welcome>()
            .add_direction(NetworkDirection::ServerToClient);
    }
}
