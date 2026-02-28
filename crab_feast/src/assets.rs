use bevy::prelude::*;
use bevy_asset_loader::prelude::*;

use crate::GameState;

#[derive(AssetCollection, Resource)]
pub struct GameAssets {
    #[asset(path = "characters/rigs/Amy.glb#Scene0")]
    pub amy_model: Handle<Scene>,

    #[asset(path = "characters/animations/Amy/SillyDancing.glb#Animation0")]
    pub silly_dancing: Handle<AnimationClip>,

    #[asset(path = "characters/animations/Amy/RunbaDancing.glb#Animation0")]
    pub runba_dancing: Handle<AnimationClip>,

    #[asset(path = "characters/animations/Amy/Idle.glb#Animation0")]
    pub idle: Handle<AnimationClip>,

    #[asset(path = "characters/animations/Amy/Jogging.glb#Animation0")]
    pub jogging: Handle<AnimationClip>,
}

pub struct AssetLoadingPlugin;

impl Plugin for AssetLoadingPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<GameState>().add_loading_state(
            LoadingState::new(GameState::AssetLoading)
                .load_collection::<GameAssets>()
                .continue_to_state(GameState::Game),
        );
    }
}
