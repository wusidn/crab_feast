use bevy::{input::mouse::MouseButtonInput, prelude::*, ui::FocusPolicy, window::PrimaryWindow};

#[derive(Component)]
pub struct Joystick;

#[derive(Component, Default)]
#[require(Node, FocusPolicy::Block, Interaction)]
struct JoystickBase;

#[derive(Component)]
struct JoystickThumb;

#[derive(Component, Default)]
struct JoystickDisabled;

#[derive(Component, Default)]
struct JustifyActived {
    start: Option<Vec2>,
    touch_id: Option<u64>,
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
    info!("Joystick added");

    let joystick_entity = on_add.event_target();
    let children = children_query.get(joystick_entity);

    if children.map_or(true, |children| {
        children
            .iter()
            .find(|child| joystick_base_query.get(*child).is_ok())
            .is_none()
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
            Transform::default(),
            ChildOf(joystick_entity),
            children![(
                Node {
                    width: Val::Px(50.0),
                    height: Val::Px(50.0),
                    ..Default::default()
                },
                BackgroundColor(Color::hsl(160.0, 0.6, 0.8)),
                JoystickThumb
            )],
        ));
    }
}

fn joystick_on_remove(
    on_remove: On<Remove, Joystick>,
    mut commands: Commands,
    children_query: Query<&Children>,
    joystick_base_query: Query<&JoystickBase>,
) {
    info!("Joystick removed");
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
    joystick_query: Query<
        (Entity, &ComputedNode, &UiGlobalTransform),
        (
            With<JoystickBase>,
            Without<JoystickDisabled>,
            Without<JustifyActived>,
        ),
    >,
    touches: Res<Touches>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut mouse_button_input_reader: MessageReader<MouseButtonInput>,
) {
    let window = windows.single().unwrap();
    let window_size = window.physical_size().as_vec2();
    let logic_size = window.size();
    let scale_factor = window.scale_factor();
    joystick_query
        .iter()
        .for_each(|(entity, cn, global_transform)| {
            let mut joystick_actived = JustifyActived::default();
            

            // Check mouse cursor
            if let Some(cursor_pos) = window.physical_cursor_position() {
                mouse_button_input_reader
                    .read()
                    .for_each(|mouse_button_input| {
                        if mouse_button_input.button == MouseButton::Left
                            && mouse_button_input.state.is_pressed()
                        {
                            if cn.contains_point(*global_transform, cursor_pos) {
                                joystick_actived.start = Some(cursor_pos);
                            }
                        }
                    });
            }

            // Check touches
            for touch in touches.iter_just_pressed() {
                info!("window_size: {:?} logic_size: {:?} scale_factor: {:?}", window_size, logic_size, scale_factor);
                let touch_pos = touch.start_position();
                info!("Touch at position: {:?} Correction: {:?}", touch.start_position(), touch_pos);
                if cn.contains_point(*global_transform, touch_pos) {
                    joystick_actived.start = Some(touch_pos);
                    joystick_actived.touch_id = Some(touch.id());
                }
            }
            if joystick_actived.start.is_some() {
                commands.entity(entity).insert(joystick_actived);
            }
        });
}

fn joystick_setup_system(
    mut commands: Commands,
    mut joystick_ready_query: Query<(Entity, &JustifyActived, &ComputedNode, &UiGlobalTransform)>,
) {
    for (entity, ready, _cn, _gt) in joystick_ready_query.iter_mut() {
        info!(
            "Joystick actived at position: {:?}, touch_id: {:?}",
            ready.start, ready.touch_id
        );
        commands.entity(entity).remove::<JustifyActived>();
    }
}
