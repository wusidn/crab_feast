use bevy::{prelude::*, window::WindowResized};
use wasm_bindgen::prelude::wasm_bindgen;

fn main() {
    console_error_panic_hook::set_once();
    wasm_logger::init(wasm_logger::Config::new(log::Level::Info));

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "carab_feast111".to_string(),
                    focused: true,
                    resizable: false,
                    fit_canvas_to_parent: true,
                    canvas: Some("#bevy".to_string()),
                    prevent_default_event_handling: false,
                    present_mode: bevy::window::PresentMode::Fifo,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .set(bevy::log::LogPlugin {
                level: bevy::log::Level::INFO,
                filter: "wgpu=error,bevy_render=info,bevy_ecs=trace".to_string(),
                ..Default::default()
            }),
    )
    .add_systems(Startup, update_window_size)
    .add_systems(PreUpdate, listen_window_size)
    .add_systems(Startup, test_logs);

    crab_feast::build_app(&mut app);
    app.run();
}

static mut WINDOW_SIZE_BUFFER: [f32; 5] = [0.0; 5];

fn update_window_size(windows: Query<&Window>) {
    let window = windows.single().unwrap();
    let window_physical_size = window.physical_size();
    unsafe {
        WINDOW_SIZE_BUFFER[0] = window.width();
        WINDOW_SIZE_BUFFER[1] = window.height();
        WINDOW_SIZE_BUFFER[2] = window_physical_size.x as f32;
        WINDOW_SIZE_BUFFER[3] = window_physical_size.y as f32;
        WINDOW_SIZE_BUFFER[4] = window.resolution.scale_factor();
        info!(
            "window size: {}, {}, {}, {}, {}",
            WINDOW_SIZE_BUFFER[0],
            WINDOW_SIZE_BUFFER[1],
            WINDOW_SIZE_BUFFER[2],
            WINDOW_SIZE_BUFFER[3],
            WINDOW_SIZE_BUFFER[4]
        );
    }
}

fn listen_window_size(windows: Query<&Window>, resize_events: MessageReader<WindowResized>) {
    if resize_events.is_empty() {
        return;
    }
    update_window_size(windows);
}

#[wasm_bindgen]
#[allow(static_mut_refs)]
pub fn get_window_size_buffer_ptr() -> *const f32 {
    unsafe { WINDOW_SIZE_BUFFER.as_ptr() }
}

#[wasm_bindgen]
#[allow(static_mut_refs)]
pub fn get_window_size_buffer_len() -> usize {
    unsafe { WINDOW_SIZE_BUFFER.len() }
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
