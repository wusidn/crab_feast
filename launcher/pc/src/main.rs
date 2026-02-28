use bevy::{prelude::*, winit::WinitSettings};
use bevy_inspector_egui::{bevy_egui::EguiPlugin, quick::WorldInspectorPlugin};

fn main() {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "carab_feast".to_string(),
            focused: true,
            resizable: true,
            present_mode: bevy::window::PresentMode::AutoVsync,
            ..Default::default()
        }),
        ..Default::default()
    }))
    .insert_resource(WinitSettings {
        focused_mode: bevy::winit::UpdateMode::Continuous,
        ..Default::default()
    })
    .add_plugins(EguiPlugin::default())
    .add_plugins(WorldInspectorPlugin::new());

    crab_feast::build_app(&mut app);

    app.run();
}
