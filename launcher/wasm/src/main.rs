use bevy::{prelude::*, window::WindowResized};
use crab_feast_launcher::{LaunchConfig, build_app};
use wasm_bindgen::prelude::wasm_bindgen;

fn main() {
    let mut app = build_app(LaunchConfig::default(), |app| {
        app.add_systems(Startup, update_window_size)
            .add_systems(PreUpdate, listen_window_size);
        crab_feast::build_app(app);
    });

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
