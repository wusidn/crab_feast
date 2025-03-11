use bevy::prelude::*;

fn main() {
    let mut app = App::new();
    // let assetss_path = Path::new(&workspace_dir!()).join("assets");
    // let assets_dir = assetss_path.to_str().expect("Failed to convert path to string");
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "carab_feast".to_string(),
            // resolution: WindowResolution::new(720.0, 480.0),
            // present_mode: bevy::window::PresentMode::AutoNoVsync,
            ..Default::default()
        }),
        ..Default::default()
    }));
    crab_feast::entry(&mut app);
    app.run();
}
