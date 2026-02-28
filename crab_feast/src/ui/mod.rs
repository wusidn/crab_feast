use bevy::app::{App, Plugin};

mod input_layer;
mod loading;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(input_layer::InputPlugin)
            .add_plugins(loading::LoadingUiPlugin);
    }
}
