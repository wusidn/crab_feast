use bevy::prelude::*;
use std::env;
use std::path::Path;
use std::path::PathBuf;

macro_rules! workspace_dir {
    () => {{
        let current_dir: PathBuf = env::var("CARGO_MANIFEST_DIR").expect("Failed to get CARGO_MANIFEST_DIR").into();
        let mut current_path = current_dir;

        loop {
            let cargo_lock_path = current_path.join("Cargo.lock");
            if cargo_lock_path.exists() {
                break current_path.to_str().expect("Failed to convert path to string").to_string();
            }

            let cargo_toml_path = current_path.join("Cargo.toml");
            if let Ok(cargo_toml_content) = std::fs::read_to_string(cargo_toml_path) {
                if cargo_toml_content.contains("[workspace]") {
                    break current_path.to_str().expect("Failed to convert path to string").to_string();
                }
            }
            // 如果没有找到Cargo.lock或Cargo.toml，则向上一级目录移动
            if !current_path.pop() {
                break "Failed to find workspace directory".to_string();
            }
        }
    }};
}

fn main() {
    let mut app = App::new();
    let assetss_path = Path::new(&workspace_dir!()).join("assets");
    let assets_dir = assetss_path.to_str().expect("Failed to convert path to string");
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "carab_feast".to_string(),
            // resolution: WindowResolution::new(720.0, 480.0),
            // present_mode: bevy::window::PresentMode::AutoNoVsync,
            ..Default::default()
        }),
        ..Default::default()
    }).set(AssetPlugin {
        file_path: assets_dir.to_string(),
        ..Default::default()
    }));
    crab_feast::entry(&mut app);
    app.run();
}
