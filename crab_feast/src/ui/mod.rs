use bevy::app::{App, Plugin};

mod blendspace;
mod font;
mod input_layer;
mod loading;
pub(crate) mod menu;
mod movement;
pub(crate) mod preview;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(font::FontFamilyPlugin)
            .add_plugins(input_layer::InputPlugin)
            .add_plugins(loading::LoadingUiPlugin)
            .add_plugins(menu::GameMenuPlugin)
            .add_plugins(preview::AnimationPreviewPlugin)
            .add_plugins(movement::MovementSettingsPlugin)
            .add_plugins(blendspace::BlendSpaceEditorPlugin);
    }
}
