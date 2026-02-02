use bevy::{
    log::{Level, LogPlugin}, prelude::*, window::{PresentMode, WindowMode}, winit::WinitSettings
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

    app
    .insert_resource(WinitSettings{
        // focused_mode: bevy::winit::UpdateMode::Reactive { 
        //     wait: Duration::from_secs_f32(1.0/120.0), 
        //     react_to_device_events: true, 
        //     react_to_user_events: true, 
        //     react_to_window_events: true 
        // },
        focused_mode: bevy::winit::UpdateMode::Continuous,
        ..Default::default()
    })
    .add_plugins(
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
                    present_mode: PresentMode::Immediate,
                    ..default()
                }),
                ..default()
            }),
    )
    // 测试日志输出的系统
    .add_systems(Startup, print_info_log);

    app.run();
}


// 测试用的系统，输出info日志
fn print_info_log() {
    // 输出不同级别的日志，验证Info级别是否生效
    info!("=== Bevy Info Log ===");
    info!("自定义数据：{}", 12345);
    warn!("这是警告日志（会同时输出）");
    error!("这是错误日志（会同时输出）");
}