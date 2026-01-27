use bevy::{
    prelude::*, 
    window::WindowMode, 
    winit::WinitSettings,
    log::{Level, LogPlugin},
};

use cfg_if::cfg_if;


// the `bevy_main` proc_macro generates the required boilerplate for iOS and Android
#[bevy_main]
pub fn main() {

    // 初始化日志系统（仅在Android平台）
    cfg_if! {
        if #[cfg(target_os = "android")] {
            android_logger::init_once(
                android_logger::Config::default()
                    .with_max_level(log::LevelFilter::Info)
                    .with_tag("BevyAndroidApp"),  // Logcat中的标签，方便过滤
            );
        }
    }

    let mut app = crab_feast::build_app();

    app.add_plugins(
        DefaultPlugins
            .set(LogPlugin {
                // This will show some log events from Bevy to the native logger.
                level: Level::DEBUG,
                filter: "wgpu=error,bevy_render=info,bevy_ecs=trace".to_string(),
                ..Default::default()
            })
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "crab_feast".to_string(),
                    resizable: false,
                    mode: WindowMode::BorderlessFullscreen(MonitorSelection::Primary),
                    // on iOS, gestures must be enabled.
                    // This doesn't work on Android
                    recognize_rotation_gesture: true,
                    ..default()
                }),
                ..default()
            }),
    )

    // Make the winit loop wait more aggressively when no user input is received
    // This can help reduce cpu usage on mobile devices
    .insert_resource(WinitSettings::mobile())
    // 测试日志输出的系统
    .add_systems(Startup, print_info_log);

    app.run();
}


// 测试用的系统，输出info日志
fn print_info_log() {
    // 输出不同级别的日志，验证Info级别是否生效
    info!("=== Bevy Android Info Log ===");
    info!("应用启动成功，当前平台：Android");
    info!("自定义数据：{}", 12345);
    warn!("这是警告日志（会同时输出）");
    error!("这是错误日志（会同时输出）");
}