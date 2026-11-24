pub mod ik;
/// 空中操控 / 起跳速度的共享规则（客户端、服务端、设置面板同源）。
pub mod air;
pub mod platform;
/// 角色运动调参常量（转身限速等），客户端与服务器共用。
pub mod tuning;
/// 世界时间速度（慢放/快放）的取值区间，客户端滑杆与服务端夹取共用。
pub mod world_time;

/// 联网协议（组件 / 消息 / 通道）。
///
/// 关掉 `net` 就不编译，wasm / mobile / 单机构建不会把 lightyear 带进来。
#[cfg(feature = "net")]
pub mod net;
