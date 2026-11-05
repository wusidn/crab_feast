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
                    .continue_to_state(GameState::Game),
            );
    }
}
