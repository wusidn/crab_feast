use bevy_inspector_egui::{bevy_egui::EguiPlugin, quick::WorldInspectorPlugin};
use crab_feast_launcher::{LaunchConfig, build_app};

fn main() {
    let mut app = build_app(LaunchConfig::default(), |app| {
        app.add_plugins(EguiPlugin::default())
            .add_plugins(WorldInspectorPlugin::new());
        crab_feast::build_app(app);
    });

    app.run();
}
