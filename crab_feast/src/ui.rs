use bevy::prelude::*;
use crab_feast_ui_joysticks::{Joystick, JoystickPlugin};

pub struct UiPlugin;



impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(JoystickPlugin)
            .add_systems(Startup, Self::setup)
            .add_systems(Update, update);
    }
}

impl UiPlugin {
    fn setup(
        mut commands: Commands,
    ) {
        commands.spawn((
            Node{
                width: Val::Vw(100.0),
                height: Val::Vh(100.0),
                padding: UiRect::all(Val::Vw(10.0)),
                display: Display::Flex,
                justify_content: JustifyContent::FlexStart,
                align_items: AlignItems::FlexEnd,
                ..Default::default()
            },
            children![
                (
                    Node{
                        width: Val::Vw(20.0),
                        height: Val::Vw(20.0),
                        display: Display::Flex,
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        border_radius: BorderRadius::all(Val::Percent(50.0)),
                        ..Default::default()
                    },
                    BackgroundColor(Color::hsl(160.0, 0.6, 0.8)),
                    Joystick {
                        ..Default::default()
                    }
                )
            ]
        ));

    }
}

fn update(
    mut commands: Commands,
    joystick_query: Query<(Entity, &Joystick)>,
    keyboard_input: Res<ButtonInput<KeyCode>>,
) {
    if keyboard_input.just_pressed(KeyCode::KeyA) {
        joystick_query.iter().for_each(|(entity, _)| {
            commands.entity(entity).remove::<Joystick>();
        });
    }
}
