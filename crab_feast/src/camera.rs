use bevy::prelude::*;

/// 集中管理所有相机的插件
pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_ui_camera)
            .add_systems(Startup, setup_game_camera);
    }
}

/// UI相机资源标记
#[derive(Resource)]
pub struct UiCamera(pub Entity);

/// 游戏场景相机资源标记
#[derive(Resource)]
pub struct GameCamera(pub Entity);

/// 设置UI相机（用于加载界面）
fn setup_ui_camera(mut commands: Commands) {
    let ui_camera = commands.spawn((
        Camera2d,
        Camera {
            order: 1,  // UI相机渲染在游戏场景前面
            ..default()
        },
    )).id();
    
    commands.insert_resource(UiCamera(ui_camera));
}

/// 设置游戏场景相机
fn setup_game_camera(mut commands: Commands) {
    let game_camera = commands.spawn((
        Camera3d::default(),
        Camera {
            order: 0,  // 游戏场景相机渲染在底层
            ..default()
        },
    )).id();
    
    commands.insert_resource(GameCamera(game_camera));
}
