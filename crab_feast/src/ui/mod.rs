use bevy::app::{App, Plugin};

mod blendspace;
mod camera_settings;
pub(crate) mod debug;
mod diagnose;
mod font;
mod game_settings;
mod input_layer;
mod jump_settings;
mod lighting_settings;
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
            .add_plugins(jump_settings::JumpSettingsPlugin)
            .add_plugins(camera_settings::CameraSettingsPlugin)
            .add_plugins(lighting_settings::LightingSettingsPlugin)
            .add_plugins(game_settings::GameSettingsPlugin)
            .add_plugins(blendspace::BlendSpaceEditorPlugin)
            .add_plugins(debug::DebugSettingsPlugin)
            .add_plugins(diagnose::DiagnoseUiPlugin);
    }
}
