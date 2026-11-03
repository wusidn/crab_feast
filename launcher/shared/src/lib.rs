use bevy::{
    asset::AssetPlugin,
    log::{Level, LogPlugin},
    prelude::*,
    window::PresentMode,
    winit::{UpdateMode, WinitSettings},
};

const LOG_FILTER: &str = "wgpu=error,bevy_render=info,bevy_ecs=trace";

#[derive(Clone, Copy, Debug)]
pub struct LaunchConfig {
    pub title: &'static str,
    pub log_level: Level,
    pub log_filter: &'static str,
}

impl Default for LaunchConfig {
    fn default() -> Self {
        Self {
            title: "crab_feast",
            log_level: default_log_level(),
            log_filter: LOG_FILTER,
        }
    }
}

const fn default_log_level() -> Level {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        Level::DEBUG
    } else {
        Level::INFO
    }
}

/// Builds an [`App`] with the platform-independent plugin stack and window/log
/// configuration. Launcher-specific plugins and the game plugins are added in
/// `setup`, which runs after the base plugins are installed.
pub fn build_app<F>(config: LaunchConfig, setup: F) -> App
where
    F: FnOnce(&mut App),
{
    init_platform_logging();

    let mut app = App::new();

    if !cfg!(target_arch = "wasm32") {
        app.insert_resource(WinitSettings {
            focused_mode: UpdateMode::Continuous,
            ..default()
        });
    }

    app.add_plugins(
        DefaultPlugins
            .set(LogPlugin {
                level: config.log_level,
                filter: config.log_filter.to_string(),
                ..default()
            })
            .set(AssetPlugin {
                file_path: asset_file_path(),
                ..default()
            })
            .set(window_plugin(&config)),
    );

    setup(&mut app);
    app
}

/// Resolves the asset root for each platform.
///
/// On desktop debug builds the workspace `assets/` directory is used directly,
/// so no symlink is needed next to the launcher crate. On wasm, `file_path` is
/// an HTTP path relative to the served page; the web build is served from the
/// repository root (see `scripts/build.sh`), hence `../../assets`.
fn asset_file_path() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        "../../assets".to_string()
    }

    #[cfg(all(
        not(target_arch = "wasm32"),
        not(target_os = "android"),
        not(target_os = "ios")
    ))]
    {
        if cfg!(debug_assertions) {
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets").to_string()
        } else {
            "assets".to_string()
        }
    }

    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        "assets".to_string()
    }
}

fn window_plugin(config: &LaunchConfig) -> WindowPlugin {
    let mut window = Window {
        title: config.title.to_string(),
        focused: true,
        ..default()
    };

    #[cfg(target_arch = "wasm32")]
    {
        window.resizable = false;
        window.fit_canvas_to_parent = true;
        window.canvas = Some("#bevy".to_string());
        window.prevent_default_event_handling = false;
        window.present_mode = PresentMode::Fifo;
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        window.present_mode = PresentMode::AutoVsync;
    }

    #[cfg(all(
        not(target_arch = "wasm32"),
        any(target_os = "android", target_os = "ios")
    ))]
    {
        window.resizable = false;
        window.mode =
            bevy::window::WindowMode::BorderlessFullscreen(bevy::window::MonitorSelection::Primary);
        window.recognize_rotation_gesture = false;
        window.prevent_default_event_handling = true;
        window.present_mode = PresentMode::Immediate;
    }

    WindowPlugin {
        primary_window: Some(window),
        ..default()
    }
}

fn init_platform_logging() {
    #[cfg(target_arch = "wasm32")]
    {
        console_error_panic_hook::set_once();
        wasm_logger::init(wasm_logger::Config::new(log::Level::Info));
    }

    #[cfg(target_os = "android")]
    {
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Info)
                .with_tag("BevyAndroidApp"),
        );
    }
}
