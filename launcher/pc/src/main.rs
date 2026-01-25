use bevy::{prelude::*, winit::WinitSettings};

fn main() {
    let mut app = crab_feast::build_app();
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "carab_feast".to_string(),
                    focused: true,
                    resizable: true,
                    // 启用垂直同步（和显示器刷新率同步，通常60/120FPS）
                    present_mode: bevy::window::PresentMode::AutoVsync,
                    ..Default::default()
                }),
                ..Default::default()
            })
    ) // 配置WinitSettings，禁用后台帧率限制
    .insert_resource(WinitSettings {
        focused_mode: bevy::winit::UpdateMode::Continuous,
        ..Default::default()
    });
    app.run();
}