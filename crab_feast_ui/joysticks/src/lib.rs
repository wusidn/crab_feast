

use bevy::prelude::*;


#[derive(Component)]
pub struct Joystick;

#[derive(Component)]
pub struct JoystickBase;

#[derive(Component)]
pub struct JoystickStick;


pub struct JoystickPlugin;

impl Plugin for JoystickPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(joystick_on_add)
        .add_observer(joystick_on_remove)
        .add_observer(joystick_base_on_add);
    }
}


fn joystick_on_add(
    trigger: Trigger<OnAdd, Joystick>, 
    mut commands: Commands,
    children_query: Query<&Children>,
    joystick_base_query: Query<&JoystickBase>,
) {
    println!("Joystick added");

    let joystick_entity = trigger.target();
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
        ));
    }
}


fn joystick_on_remove(
    trigger: Trigger<OnRemove, Joystick>, 
    mut commands: Commands,
    children_query: Query<&Children>,
    joystick_base_query: Query<&JoystickBase>,
) {
    println!("Joystick removed");
    if let Ok(children) = children_query.get(trigger.target()) {
        children.iter().for_each(|child| {
            if joystick_base_query.get(child).is_ok() {
                commands.entity(child).despawn();
            }
        });
    }
}


fn joystick_base_on_add(
    trigger: Trigger<OnAdd, JoystickBase>, 
    mut commands: Commands,
    children_query: Query<&Children>,
    joystick_stick_query: Query<&JoystickStick>,
) {
    println!("Joystick base added");
    let base_entity = trigger.target();
    let children = children_query.get(base_entity);

    if children.map_or(true, |children| {
        children.iter().find(|child| {
            joystick_stick_query.get(*child).is_ok()
        }).is_none()
    }) {
        commands.spawn((
            Node {
                width: Val::Px(50.0),
                height: Val::Px(50.0),
                ..Default::default()
            },
            BackgroundColor(Color::hsl(160.0, 0.6, 0.8)),
            JoystickStick,
            ChildOf(base_entity),
        ));
    }
}

