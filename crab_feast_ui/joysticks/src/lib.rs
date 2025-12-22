

use bevy::{prelude::*, ui::FocusPolicy, window::PrimaryWindow};


#[derive(Component)]
pub struct Joystick;

#[derive(Component, Default)]
#[require(Node, FocusPolicy::Block, Interaction)]
struct JoystickBase;

#[derive(Component)]
struct JoystickThumb;

#[derive(Debug)]
struct TouchInfo {
    touch_id: u64,
    start: Vec2,
}

#[derive(Component, Default, Debug)]
struct JoystickReady {
    cursor_pos: Option<Vec2>,
    touches: Option<Vec<TouchInfo>>
}

#[derive(Component, Default)]
struct JustifyActived {
    start: Option<Vec2>,
    touch_id:  Option<u64>
}

pub struct JoystickPlugin;

impl Plugin for JoystickPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(joystick_on_add)
        .add_observer(joystick_on_remove)
        // .add_observer(joystick_on_touch)
        .add_systems(Update, (joystick_idle_system, joystick_setup_system));
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
            Transform::default(),
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

fn joystick_idle_system(
    mut commands: Commands,
    interaction_query: Query<
                            (Entity, &Interaction), 
                            (Changed<Interaction>, With<JoystickBase>, Without<JoystickReady>, Without<JustifyActived>)
                            > ,
    touches: Res<Touches>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    
    for (entity, interaction) in interaction_query {
        match interaction {
            Interaction::Pressed => {
                println!("JoystickBase pressed: {:?}", entity);

                commands.entity(entity).insert(JoystickReady{
                    cursor_pos: windows.single().map_or(None, |window| window.physical_cursor_position()),
                    touches: touches.any_just_pressed().then(|| touches.iter_just_pressed().map(|touch| TouchInfo{
                        touch_id: touch.id(),
                        start: touch.position(),
                    }).collect::<Vec<_>>())
                });
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

fn joystick_setup_system(
    mut commands: Commands,
    mut joystick_ready_query: Query<(Entity, &JoystickReady, &ComputedNode, &UiGlobalTransform)>,
) {
    for (entity, ready, cn, gt) in joystick_ready_query.iter_mut() { 
        
        let mut touch_id: Option<u64> = None;
        let start_pos = if ready.cursor_pos.is_some() {
            ready.cursor_pos
        } else {
            ready.touches.as_ref().unwrap().iter().find(|touch| {
                cn.contains_point(*gt, touch.start)
            }).map(|touch| {
                touch_id = Some(touch.touch_id);
                touch.start
            })
        };
        commands.entity(entity).remove::<JoystickReady>();

        if start_pos.is_none() {
            return;
        }

        println!("click in joystick: {:?}", start_pos);
        commands.entity(entity).insert(JustifyActived{
            start: start_pos,
            touch_id: touch_id
        });

    }
}
