use bevy::{
    dev_tools::fps_overlay::{FpsOverlayConfig, FpsOverlayPlugin, FrameTimeGraphConfig},
    prelude::*,
    text::FontSmoothing,
};
use crab_feast_library::platform::PlatformInfo;

mod assets;
mod camera;
mod input;
mod scene;
mod state;
mod ui;

pub use assets::GameAssets;
pub use state::GameState;

pub fn build_app(app: &mut App) {
    app.insert_resource(PlatformInfo::current()).add_plugins((
        camera::CameraPlugin,
        FpsOverlayPlugin {
            config: FpsOverlayConfig {
                text_config: TextFont {
                    // Here we define size of our overlay
                    font_size: FontSize::Rem(1.0),
                    // If we want, we can use a custom font
                    font: default(),
                    // We could also disable font smoothing,
                    font_smoothing: FontSmoothing::default(),
                    ..default()
                },
                // We can also set the refresh interval for the FPS counter
                refresh_interval: core::time::Duration::from_millis(100),
                enabled: true,
                frame_time_graph_config: FrameTimeGraphConfig {
                    enabled: true,
                    // The minimum acceptable fps
                    min_fps: 30.0,
                    // The target fps
                    target_fps: 60.0,
                },
                ..Default::default()
            },
        },
        assets::AssetLoadingPlugin,
        ui::UiPlugin,
        scene::ScenePlugin,
    ));

}
