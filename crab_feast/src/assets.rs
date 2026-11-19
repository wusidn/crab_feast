use bevy::prelude::*;
use bevy_asset_loader::prelude::*;
use iyes_progress::ProgressPlugin;

use crate::GameState;

/// 角色模型与动画现在由 `preview` 模块按需异步加载，
/// 这里保留资源位以便后续放入真正需要预加载的资产。
#[derive(AssetCollection, Resource)]
pub struct GameAssets {}

pub struct AssetLoadingPlugin;

impl Plugin for AssetLoadingPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<GameState>()
            .add_plugins(ProgressPlugin::<GameState>::default())
            .add_loading_state(
                LoadingState::new(GameState::AssetLoading)
                    .load_collection::<GameAssets>()
                    // 启动资源加载完先进大厅：选服务器或本地游戏，登录成功才建场景
                    .continue_to_state(GameState::Lobby),
            );
    }
}
