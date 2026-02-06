use bevy::prelude::*;

use crate::event::RotateInput;
pub struct DragRotateInputPlugin;

impl Plugin for DragRotateInputPlugin {
    fn build(&self, app: &mut App) {
        app
            .add_systems(Startup, Self::setup);
    }
}
impl DragRotateInputPlugin {
    fn setup(
        mut commands: Commands
    ) {
        commands.spawn((
            Node{
                width: Val::Vw(100.0),
                height: Val::Vh(100.0),
                ..Default::default()
            },
            ZIndex(1),
        )).observe(on_drag);
    }
}

fn on_drag(
    event: On<Pointer<Drag>>,
    mut commands: Commands
) {
    commands.trigger(RotateInput(event.delta));
    // println!("Drag: {:?}", event.delta);
}