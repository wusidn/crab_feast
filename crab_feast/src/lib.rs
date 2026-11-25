use bevy::{
    dev_tools::fps_overlay::{FpsOverlayConfig, FpsOverlayPlugin, FrameTimeGraphConfig},
    prelude::*,
    text::FontSmoothing,
};
use crab_feast_library::platform::PlatformInfo;

/// 动画层：跳跃档位与调参表要暴露给 dedicated server（蓄力/离地速度两端同源）。
pub mod anim;
mod assetpack;
mod assets;
mod camera;
/// 系统健康诊断：逐条移动/表现链路的实时指标与状态灯。
mod diagnose;
#[cfg(feature = "net")]
mod discovery;
/// 脚下接地探测：客户端动画层与服务端物理共用（任意台面高度都成立）。
pub mod ground;
pub mod input;
/// 关卡：静态场景的设计数据（地面 / 方块尺寸与位置）与本地构建入口。
///
/// dedicated server 也会引用这里的数据建自己的权威碰撞几何。
pub mod level;
/// 全局光照：场景主平行光的亮度设置。
mod lighting;
#[cfg(feature = "net")]
mod net;
/// 复制快照的落地：位置直接对齐 + 本地动画参数。
#[cfg(feature = "net")]
mod net_pose;
/// 本地预测核：60 Hz 自己推位移与朝向，快照只做校正。
#[cfg(feature = "net")]
mod predict;
/// 移动端双指缩放：独立的触摸输入系统，只在触屏设备上跑。
mod pinch;
mod scene;
mod session;
mod settings;
mod state;
mod ui;

pub use assets::GameAssets;
// 真机上看不到 stdout：把日志函数暴露出去，启动时装的 panic hook 也写这份文件
pub use assetpack::log_line;
#[cfg(feature = "net")]
pub use discovery::{DiscoveryPlugin, RegistryConfig, ServerBrowser};
#[cfg(feature = "net")]
pub use net::{AutoJump, AutoWalk, LocalPlayer, NetConfig, NetPlugin};
pub use state::{AutoJoin, GameState, LobbyNotice, NetDiagnostics, SessionMode, SessionReady};

/// 出包时由 `launcher/mobile/ios/build_rust_deps.sh` 从 Xcode 的
/// `CURRENT_PROJECT_VERSION` 注入；PC 的 cargo 构建没有这个变量，显示 `dev`。
pub const BUILD_TAG: &str = match option_env!("CRAB_BUILD_NUMBER") {
    Some(value) => value,
    None => "dev",
};

pub fn build_app(app: &mut App) {
    // 装过热更包就把资源根指过去，角色清单扫描与 asset_server 加载必须是同一个目录
    if let Some(dir) = assetpack::ready_pack_dir() {
        // SAFETY: 单线程启动阶段设置，之后只读
        unsafe { std::env::set_var("CRAB_ASSET_ROOT", &dir) };
        info!("资源根目录：{}（热更包）", dir.display());
    }
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
            assetpack::AssetPackPlugin,
            assets::AssetLoadingPlugin,
            pinch::PinchPlugin,
            session::SessionPlugin,
            lighting::LightingPlugin,
            settings::SettingsPlugin,
            ui::UiPlugin,
            scene::ScenePlugin,
            diagnose::DiagnosticsPlugin,
        ));

    // 大厅的服务器列表靠这条后台监听线程喂数据；没有网络特性的构建没有列表
    #[cfg(feature = "net")]
    app.add_plugins(discovery::DiscoveryPlugin);
}
