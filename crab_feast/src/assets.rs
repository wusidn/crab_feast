use bevy::prelude::*;
use bevy_asset_loader::prelude::*;
use iyes_progress::ProgressPlugin;

use crate::GameState;

/// Character clips: for best results with [`crate::root_motion`] compensation, export
/// Mixamo animations **in place** (or strip root translation in a DCC) so Hips carry
/// minimal unwanted world drift in the source data.
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

    #[asset(path = "characters/animations/Amy/Walking.glb#Animation0")]
    pub walking: Handle<AnimationClip>,

    #[asset(path = "characters/animations/Amy/Running.glb#Animation0")]
    pub running: Handle<AnimationClip>,

    #[asset(path = "characters/animations/Amy/Jogging.glb#Animation0")]
    pub jogging: Handle<AnimationClip>,
}

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