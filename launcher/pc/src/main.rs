use bevy::prelude::*;

fn main() {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "crab_feast".to_string(),
            // resolution: WindowResolution::new(720.0, 480.0),
            // present_mode: bevy::window::PresentMode::AutoNoVsync,
            ..Default::default()
        }),
        ..Default::default()
    }));
    crab_feast::entry(&mut app);
    app.run();
}
