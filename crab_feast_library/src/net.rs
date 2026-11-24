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

/// 服务器注册中心（Web 服务）的默认地址。
///
/// 默认端口用 8090 而不是 8080：仓库 README 里的 wasm 预览服务已经占了 8080，
/// 本地同时开「资源/网页服务 + 注册中心」时不会撞端口。
pub const DEFAULT_REGISTRY_URL: &str = "http://127.0.0.1:8090";

// 空中操控与起跳速度的取值规则放在 `crate::air`（不带联网特性也要用），
// 这里再导出一次：协议字段和它们的默认值本来就该挨着看。
pub use crate::air::{
    AIR_ACCEL_DEFAULT, AIR_ACCEL_RANGE, AIR_STEER_SECS_DEFAULT, AIR_STEER_SECS_RANGE,
    JUMP_LAUNCH_VY_DEFAULT, JUMP_LAUNCH_VY_RANGE, air_steer_active, air_steer_velocity,
};

/// 顶速在 `crate::tuning` 里（单机也要用），这里再导出给协议侧代码。
pub use crate::tuning::MOVE_SPEED_MPS;

/// netcode 协议号。两端必须一致，否则握手包会被丢弃（表现为"连不上也不报错"）。
///
/// 上线前改成你自己的随机 u64。
pub const NETCODE_PROTOCOL_ID: u64 = 0x2_0C2B_FEAD;

/// netcode 私钥（32 字节）。**生产环境绝对不能放在客户端**：
/// 正式做法是由后端签发 `ConnectToken`（`lightyear::netcode::prelude::Authentication::Token`），
/// 客户端只拿到 token。这里的明文私钥只用于本地开发 / 自建测试服。
pub const NETCODE_DEV_KEY: [u8; 32] = [0x42; 32];

/// 服务器发现广播端口。
///
/// 服务端每两秒往这个端口广播一条 [`ServerAnnounce`]；客户端在大厅页监听它，
/// 把「现在能连的服务器」直接列出来，不需要玩家手输地址。
/// 同机多开客户端时多个进程会绑同一个端口，所以监听端必须开 `SO_REUSEPORT`。
pub const DISCOVERY_PORT: u16 = 5098;

/// 广播包的协议标记：只认自己人发的包，避免把同网段的其它 UDP 流量当服务器。
pub const DISCOVERY_MAGIC: &str = "crab_feast/announce";

/// 服务端广播的自我介绍。
///
/// `addr` 是服务端实际监听地址的字符串形式：
/// - 绑到具体网卡 / `127.0.0.1` 时客户端直接用这个地址；
/// - 绑到 `0.0.0.0` 时这里是未指定地址，客户端改用「广播包的源 IP + `port`」——
///   局域网里服务端不知道自己该对外报哪个网卡地址，但收包方能从 IP 头里拿到。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerAnnounce {
    pub magic: String,
    /// 显示名（默认取机器名）
    pub name: String,
    /// 监听地址字符串，可能含未指定 IP
    pub addr: String,
    /// 监听端口（`addr` 解析失败时用它 + 源 IP 兜底）
    pub port: u16,
    /// 当前房间里的玩家数
    pub players: u8,
    /// 房间上限
    pub max_players: u8,
    /// 服务器 tick 频率，客户端用来核对协议/手感是否匹配
    pub tick_hz: f32,
}

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

/// 这条快照对应的服务器模拟 tick（每个 tick 自增 1）。
///
/// 客户端不能拿「收包时刻」当时间轴：包到达会被网络抖动拉长压短，直接按到达时间
/// 插值，角色会一会儿快一会儿慢。带上 tick 之后，快照在时间轴上的间距恒为
/// 1/TICK_HZ，渲染层按 tick 查找前后两帧做插值，网络抖动只影响「还能不能及时收到
/// 下一帧」，不影响运动速度。
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetTick(pub u32);

/// 服务端跳跃阶段：远端动画不再靠位置差猜「什么时候起跳」。
///
/// 编码进协议的是 `u8`（见 [`NetJump::phase`]），这里只给两端共用的常量，
/// 避免一边写 `2`、另一边写成 `3`。阶段顺序与 `JumpState` 的推进一致：
/// 蓄力（贴地蹲）→ 滞空 → 落地恢复 → 地面。
pub mod jump_phase {
    /// 站在地面，没有跳跃
    pub const GROUND: u8 = 0;
    /// 已接受起跳请求，贴地蓄力中
    pub const CHARGE: u8 = 1;
    /// 已写离地速度，滞空中
    pub const AIR: u8 = 2;
    /// 已触地，落地恢复段
    pub const LAND: u8 = 3;
}

/// 复制到客户端的跳跃状态（服务端权威）。
///
/// 光有 `NetPos` 时，远端只能靠「位置差 + 竖直速度」推测起跳和触地：
/// 60Hz 台阶会让动作晚 1~2 tick，落地判定也会迟；`seq` 对应客户端的起跳序号，
/// `ticks` 是当前阶段已经过的服务器 tick，远端据此把剪辑时间轴直接对到
/// 「真正的离地/落地时刻」，而不是从「被看见离地」开始算。
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetJump {
    /// 起跳请求序号（= 上行 `MoveInput.jump_seq`）；变化即新的一跳
    pub seq: u8,
    /// [`jump_phase`] 里的阶段编码
    pub phase: u8,
    /// 当前阶段已过的 tick 数（`u16` 够覆盖 18 分钟的滞空兜底）
    pub ticks: u16,
    /// 起跳大类：0 = 平地，1 = 跳上，2 = 跳下
    pub kind: u8,
}

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
    /// 这条输入该在**哪个 tick** 生效（客户端本地预测的 tick 编号，与服务端
    /// `NetTick` 同一条时间轴）。
    ///
    /// 掉头/松手那一下两端差得最多的不是曲线，而是**相位**：客户端在自己那一帧
    /// 就反向了，服务端却在「收到包那一 tick」才执行，两条轨迹整体错开
    /// ≈ 速度 × 单程延迟（180° 甩头实测 20~35cm），和解只能把人往回拽。
    /// 带上 tick，服务端就能把输入排进它该生效的那一 tick，两端逐 tick 同源。
    ///
    /// 0 = 客户端还没和服务端对上 tick（没上锚），服务端退回「到达即生效」。
    pub tick: u32,
    pub dir: [f32; 2],
    pub force: f32,
    pub jump: bool,
    /// 起跳请求序号：客户端每 `begin` 一次自增。
    ///
    /// 线上是「状态快照」不是事件流，只靠 `jump` 电平推上升沿会丢：
    /// 两帧之间的电平回落可能被合包压平，落地恢复段里的第二跳更会跟上一跳
    /// 共用一个电平。序号放在每一条快照里，服务端比较 `prev_seq != seq` 就能
    /// 认出新的一跳，不依赖任何一次回落被观察到。
    pub jump_seq: u8,
    /// 客户端判出来的起跳大类：0 = 平地，1 = 跳上，2 = 跳下。
    ///
    /// 服务端不自己再探一遍：它的 `Transform` 朝向恒等，站定起跳时探不出「前方
    /// 有没有高台」。客户端在起跳那一帧把结果带上来，两端就取同一档调参
    /// （蓄力时长 / 离地速度），不会出现「客户端按上跳编排、服务端按平地抬升」。
    pub jump_kind: u8,
    /// 起跳竖直初速度（米/秒）。0 = 让服务端用它自己的默认值。
    ///
    /// 手感参数在客户端设置面板上调，服务端按 [`JUMP_LAUNCH_VY_RANGE`] 夹取。
    /// 放在每条快照里而不是握手时握一次：玩家可以在对局中改设置，服务端下一 tick
    /// 就用新值，不会出现「改了设置这一跳没生效」。
    pub jump_vy: f32,
    /// 空中操控加速度（米/秒²），0 = 不接管。服务端按 [`AIR_ACCEL_RANGE`] 夹取。
    pub air_accel: f32,
    /// 起跳后多久停止接管水平速度（秒）。服务端按 [`AIR_STEER_SECS_RANGE`] 夹取。
    pub air_steer_secs: f32,
    /// 转身降速的起点（度）：朝向误差小于它时地速不受影响。0 = 用服务端默认值。
    ///
    /// 这两个旋钮在「移动设置」面板上，服务端必须拿到同一组数——否则同一 tick
    /// 两侧算出的地速不同，预测端会被和解系统一直往回拽。
    pub turn_brake_start_deg: f32,
    /// 转身降速的下限（0~1）：误差到 180° 时保留的地速比例。0 = 用服务端默认值。
    pub turn_brake_min_scale: f32,
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

/// **服务端权威**的世界时间速度，复制给所有客户端。
///
/// 挂在服务器上的一个旁观实体（世界时钟）上，客户端只读不写：任意一端拖动调试面板的
/// 滑杆都会变成一条 [`TimeScaleRequest`]，由服务器改写这里的值再复制下来，
/// 于是「谁拖的」不重要，所有端看到的世界速度永远是同一个。
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetTimeScale(pub f32);

impl Default for NetTimeScale {
    fn default() -> Self {
        Self(1.0)
    }
}

impl NetTimeScale {
    /// 把任意输入夹到合法区间（规则见 [`crate::world_time::clamp_scale`]）。
    pub fn clamp_speed(value: f32) -> f32 {
        crate::world_time::clamp_scale(value)
    }
}

/// client → server：请求把世界时间速度改成 `value`。
///
/// 只是「请求」：服务端会夹到合法区间，改完再复制给所有人。拖动时客户端会节流上行，
/// 所以服务端收到的永远是最新那一条的近似值，不需要序号。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default)]
pub struct TimeScaleRequest {
    pub value: f32,
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
        app.component::<NetTick>().replicate();
        app.component::<NetJump>().replicate();
        app.component::<NetMotion>().replicate();
        app.component::<NetPlayerId>().replicate_once();
        // 世界时间速度：整局只有一个值，挂在服务器的「世界时钟」实体上复制下来。
        // 用 `replicate()`（不是 `replicate_once`）：滑杆每动一次都要同步下去。
        app.component::<NetTimeScale>().replicate();
        // 关卡几何**不进协议**：静态场景是设计数据，两端各自从同一份关卡
        // 定义建实体（`crab_feast::level::spawn_static_level`）。
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
        // 世界时间速度：客户端请求、服务端改权威值，结果走 `NetTimeScale` 复制回来
        app.register_message::<TimeScaleRequest>()
            .add_direction(NetworkDirection::ClientToServer);
    }
}
