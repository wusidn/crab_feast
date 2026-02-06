use bevy::app::{App, Plugin};


mod move_input_layer;
mod rotate_input_layer;


pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app
        .add_plugins(rotate_input_layer::DragRotateInputPlugin)
        .add_plugins(move_input_layer::JoystickMoveInputPlugin);
    }
}
