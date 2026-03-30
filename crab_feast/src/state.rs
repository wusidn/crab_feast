use bevy::prelude::*;

#[derive(States, Default, Clone, Eq, PartialEq, Debug, Hash)]
pub enum GameState {
    #[default]
    AssetLoading,
    SceneLoading,
    Menu,
    Game,
    Pause,
}
