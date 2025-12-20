

use bevy::{prelude::*, window::PrimaryWindow};


#[derive(Component)]
pub struct Joystick;

#[derive(Component, Default)]
struct JoystickBase {
    start: Vec2,
    touch_id: Option<u64>,
}

#[derive(Component)]
struct JoystickThumb;

pub struct JoystickPlugin;

impl Plugin for JoystickPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(joystick_on_add)
        .add_observer(joystick_on_remove)
        // .add_observer(joystick_on_touch)
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
            JoystickBase::default(),
            Interaction::default(),
            ChildOf(joystick_entity),
            children![
                (
                    Node {
                        width: Val::Px(50.0),
                        height: Val::Px(50.0),
                        ..Default::default()
                    },
                    BackgroundColor(Color::hsl(160.0, 0.6, 0.8)),
                    JoystickThumb
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


// fn joystick_on_touch(interaction: On<Changed<Interaction>, JoystickBase>) {
//     println!("Pointer event on interaction: {:?}", interaction.event_target());
// }

fn joystick_system(
    mut interaction_query: Query<(Entity, &mut JoystickBase, &Interaction, &ComputedNode, &GlobalTransform), Changed<Interaction>>,
    touches: Res<Touches>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    
    for (entity, mut joystick_base, interaction, computed_node, global_transform) in &mut interaction_query {
        match interaction {
            Interaction::Pressed => {
                println!("JoystickBase pressed: {:?}", entity);
                if touches.any_just_pressed() {
                    let physical_pos = global_transform.translation().truncate() * computed_node.inverse_scale_factor();
                    let physical_dimensions = computed_node.size() * computed_node.inverse_scale_factor();
                    let physical_bounds = Rect::from_center_size(physical_pos,physical_dimensions);

                    for touch in touches.iter_just_pressed() {
                        println!("Touch just pressed: {:?}", touch);
                        if physical_bounds.contains(touch.position()) {
                            println!("Touch is inside joystick: {:?}", touch);
                            joystick_base.touch_id = Some(touch.id());
                            joystick_base.start = touch.position();
                        }
                    }
                } else {
                    let window = windows.single().unwrap();
                    window.cursor_position().map(|pos| {
                        println!("Cursor position: {:?}", pos);
                    });
                }
            }
            Interaction::Hovered => {
                println!("JoystickBase hovered: {:?}", entity);
            }
            Interaction::None => {
                println!("JoystickBase released: {:?}", entity);
            }
        }
    }
}