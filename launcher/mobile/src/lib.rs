use std::net::ToSocketAddrs;
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::*;
use crab_feast::{
    AutoJoin, NetConfig, NetPlugin, RegistryConfig, SessionMode, build_app as build_game,
};
use crab_feast_launcher::{LaunchConfig, build_app};

/// iOS 包没有 `launcher.json`，`RegistryConfig::default()` 会退回
/// `http://127.0.0.1:8090`——在手机上这是手机自己，永远拉不到 Mac 上的注册中心。
///
/// 所以移动端显式给一个外部可达的地址：默认走和 OTA 同一个域名的 8443
/// （TLS + 注册 API 同进程），需要临时改地址时用 `CRAB_REGISTRY_URL` 覆盖，
/// 也可以在出包时用 `CRAB_MOBILE_REGISTRY_URL` 编译期覆盖。
fn mobile_registry_url() -> Option<String> {
    std::env::var("CRAB_REGISTRY_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| option_env!("CRAB_MOBILE_REGISTRY_URL").map(str::to_string))
        .or_else(|| Some("https://ota.qiaqia.cyou:8443".to_string()))
}

/// 移动端没有 `--client-id` 参数，用纳秒时间戳 + pid 生成一个进程内唯一值。
/// 两台客户端都用默认的 1 时，netcode 会把它们当成同一个客户端，后连的会被踢。
fn mobile_client_id() -> u64 {
    if let Ok(value) = std::env::var("CRAB_CLIENT_ID") {
        if let Ok(id) = value.trim().parse::<u64>() {
            return id;
        }
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(1);
    (nanos ^ ((std::process::id() as u64) << 32)).max(1)
}

/// 模拟器/自动冒烟测试用：`CRAB_AUTO_SERVER=game.qiaqia.cyou:5099` 时跳过大厅。
/// 真机正常启动没有这个变量，仍然从大厅手动点「加入」。
fn auto_join_target() -> Option<SessionMode> {
    let value = std::env::var("CRAB_AUTO_SERVER").ok()?;
    let addrs = value.trim().to_socket_addrs().ok()?;
    let server = addrs
        .into_iter()
        .find(|addr| addr.is_ipv6())
        .or_else(|| value.trim().to_socket_addrs().ok()?.next())?;
    Some(SessionMode::Online { server })
}

// the `bevy_main` proc_macro generates the required boilerplate for iOS and Android
#[bevy_main]
pub fn main() {
    // 真机崩溃只会留一份 SIGABRT 的 .ips，里面没有 panic 文本（Bevy 的事件循环是
    // `extern "C"`，panic 直接 abort）。这里把 panic 消息和位置写进容器的 sync.log，
    // 出问题捞一份日志就能看到具体是哪一行炸的。
    std::panic::set_hook(Box::new(|info| {
        crab_feast::log_line(&format!("PANIC: {info}"));
        eprintln!("PANIC: {info}");
        let backtrace = std::backtrace::Backtrace::force_capture();
        crab_feast::log_line(&format!("PANIC 栈:\n{backtrace}"));
    }));

    let registry = mobile_registry_url();
    let mut app = build_app(LaunchConfig::default(), move |app| {
        // DiscoveryPlugin 在 build 阶段只读一次，必须在游戏插件之前插入。
        app.insert_resource(RegistryConfig::from_option(registry))
            .insert_resource(NetConfig {
                client_id: mobile_client_id(),
                ..Default::default()
            })
            // 只给模拟器冒烟用；真机没有这个环境变量，仍走大厅手动加入。
            .insert_resource(AutoJoin(auto_join_target()))
            // 移动端以前漏加了 NetPlugin：大厅能显示服务器，但 `connect`
            // 系统、UDP socket 和 netcode 都不存在，点「加入」只会停在
            // 「正在连接」，服务端连一个包都收不到。PC launcher 里有这行，
            // 所以只有手机会复现。
            .add_plugins(NetPlugin);
        build_game(app);
    });
    app.run();
}
