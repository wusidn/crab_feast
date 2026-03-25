use bevy::prelude::*;

mod assets;
mod camera;
mod input;
mod scene;
mod state;
mod ui;
mod utils;
mod root_motion;

pub use assets::GameAssets;
pub use state::GameState;

pub fn build_app(app: &mut App) {
    app.add_plugins(camera::CameraPlugin)
        .add_plugins(assets::AssetLoadingPlugin)
        .add_plugins(scene::ScenePlugin)
        .add_plugins((ui::UiPlugin, crab_feast_ui_fps::FPSPlugin));
}