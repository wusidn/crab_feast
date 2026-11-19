use bevy::{
    dev_tools::fps_overlay::{FpsOverlayConfig, FpsOverlayPlugin, FrameTimeGraphConfig},
    prelude::*,
    text::FontSmoothing,
};
use crab_feast_library::platform::PlatformInfo;

mod anim;
mod assets;
mod camera;
#[cfg(feature = "net")]
mod discovery;
mod input;
#[cfg(feature = "net")]
mod net;
mod scene;
mod session;
mod settings;
mod state;
mod ui;

pub use assets::GameAssets;
#[cfg(feature = "net")]
pub use discovery::{DiscoveryPlugin, ServerBrowser};
#[cfg(feature = "net")]
pub use net::{AutoWalk, LocalPlayer, NetConfig, NetPlugin};
pub use state::{AutoJoin, GameState, LobbyNotice, SessionMode, SessionReady};

pub fn build_app(app: &mut App) {
    app.insert_resource(PlatformInfo::current())
        .insert_resource(input::MovementSettings::default()) // 移动设置
        .add_plugins((
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
                        target_fps: 75.0,
                    },
                    ..Default::default()
                },
            },
            anim::AnimPlugin,
            assets::AssetLoadingPlugin,
            session::SessionPlugin,
            settings::SettingsPlugin,
            ui::UiPlugin,
            scene::ScenePlugin,
        ));

    // 大厅的服务器列表靠这条后台监听线程喂数据；没有网络特性的构建没有列表
    #[cfg(feature = "net")]
    app.add_plugins(discovery::DiscoveryPlugin);
}
