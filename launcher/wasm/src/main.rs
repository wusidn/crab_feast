
use bevy::prelude::*;

fn main() {
    console_error_panic_hook::set_once();
    wasm_logger::init(wasm_logger::Config::new(log::Level::Info));
    
    let mut app = crab_feast::build_app();
    app.add_plugins(DefaultPlugins
        .set(WindowPlugin {
            primary_window: Some(Window {
                title: "carab_feast111".to_string(),
                focused: true,
                resizable: false,
                fit_canvas_to_parent: true,
                // 指定canvas元素ID
                canvas: Some("#bevy".to_string()),
                prevent_default_event_handling: false,
                // resolution: WindowResolution::new(1280, 720),  // 明确指定分辨率
                // 添加这个设置来控制缩放
                present_mode: bevy::window::PresentMode::Fifo,  // 或其他模式
                ..Default::default()
            }),
            ..Default::default()
        })
        .set(bevy::log::LogPlugin {
            level: bevy::log::Level::INFO,
            filter: "wgpu=error,bevy_render=info,bevy_ecs=trace".to_string(),
            ..Default::default()
        })
    )
    // 添加测试系统，输出不同级别的日志
    .add_systems(Startup, test_logs);
    app.run();
}

// 测试日志输出的系统
fn test_logs() {
    // 不同级别的日志，都会输出到浏览器控制台
    error!("这是错误日志 ❌");
    warn!("这是警告日志 ⚠️");
    info!("这是信息日志 ℹ️");
    debug!("这是调试日志 🐛"); // 需要日志级别设为Debug才能看到
    trace!("这是追踪日志 🕵️"); // 需要级别设为Trace
}
