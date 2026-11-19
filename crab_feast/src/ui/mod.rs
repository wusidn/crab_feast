use bevy::app::{App, Plugin};

mod blendspace;
mod debug;
mod font;
mod game_settings;
mod input_layer;
mod loading;
mod lobby;
pub(crate) mod menu;
mod movement;
pub(crate) mod preview;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(font::FontFamilyPlugin)
            .add_plugins(input_layer::InputPlugin)
            .add_plugins(loading::LoadingUiPlugin)
            .add_plugins(lobby::LobbyUiPlugin)
            .add_plugins(menu::GameMenuPlugin)
            .add_plugins(preview::AnimationPreviewPlugin)
            .add_plugins(movement::MovementSettingsPlugin)
            .add_plugins(game_settings::GameSettingsPlugin)
            .add_plugins(blendspace::BlendSpaceEditorPlugin)
            .add_plugins(debug::DebugSettingsPlugin);
    }
}
