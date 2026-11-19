pub mod ik;
pub mod platform;
/// 角色运动调参常量（转身限速等），客户端与服务器共用。
pub mod tuning;

/// 联网协议（组件 / 消息 / 通道）。
///
/// 关掉 `net` 就不编译，wasm / mobile / 单机构建不会把 lightyear 带进来。
#[cfg(feature = "net")]
pub mod net;
