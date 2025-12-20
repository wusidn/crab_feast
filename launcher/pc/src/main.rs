use bevy::prelude::*;

fn main() {
    let mut app = crab_feast::build_app();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "carab_feast".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }));
    app.run();
}
