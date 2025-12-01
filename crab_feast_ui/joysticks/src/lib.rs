

use bevy::prelude::*;


#[derive(Component)]
pub struct Joystick;

#[derive(Component)]
struct JoystickBase;

#[derive(Component)]
struct JoystickThumb;


pub struct JoystickPlugin;

impl Plugin for JoystickPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(joystick_on_add)
        .add_observer(joystick_on_remove)
        .add_systems(Update, joystick_system);
    }
}


fn joystick_on_add(
    on_add: On<Add, Joystick>, 
    mut commands: Commands,
    children_query: Query<&Children>,
    joystick_base_query: Query<&JoystickBase>,
) {
    println!("Joystick added");

    let joystick_entity = on_add.event_target();
    let children = children_query.get(joystick_entity);

    if children.map_or(true, |children| {
        children.iter().find(|child| {
            joystick_base_query.get(*child).is_ok()
        }).is_none()
    }) {
        commands.spawn((
            Node {
                width: Val::Px(100.0),
                height: Val::Px(100.0),
                display: Display::Flex,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..Default::default()
            },
            BackgroundColor(Color::hsl(310.0, 0.6, 0.8)),
            JoystickBase,
            ChildOf(joystick_entity),
            children![
                (
                    Node {
                        width: Val::Px(50.0),
                        height: Val::Px(50.0),
                        ..Default::default()
                    },
                    BackgroundColor(Color::hsl(160.0, 0.6, 0.8)),
                    JoystickThumb,
                )
            ]
        ));
    }
}


fn joystick_on_remove(
    on_remove: On<Remove, Joystick>, 
    mut commands: Commands,
    children_query: Query<&Children>,
    joystick_base_query: Query<&JoystickBase>,
) {
    println!("Joystick removed");
    if let Ok(children) = children_query.get(on_remove.event_target()) {
        children.iter().for_each(|child| {
            if joystick_base_query.get(child).is_ok() {
                commands.entity(child).despawn();
            }
        });
    }
}

fn joystick_system(
    // joystick_base_query: Query<&JoystickBase>,
    // joystick_thumb_query: Query<&JoystickThumb>,
) {
    

}