use bevy::{input::mouse::MouseButtonInput, prelude::*, window::PrimaryWindow};

pub struct JoystickPlugin;

#[derive(Component)]
pub struct Joystick {
    pub radius: f32,
    pub thumb_radius: f32,
    pub thumb_max_distance: f32,
    pub enable_elastic_rebound: bool,
    pub elastic_rebound_duration: f32,
}

#[derive(Component, Default)]
pub struct JoystickBase;

#[derive(Component)]
pub struct JoystickThumb;

#[derive(Component, Default)]
pub struct JoystickDisabled;

#[derive(Message)]
pub struct JoystickActivate {
    pub entity: Entity,
    pub start_pos: Vec2,
}

#[derive(Message)]
pub struct JoystickMove {
    pub entity: Entity,
    pub pos: Vec2,
}

 #[derive(Message)]
 pub struct JoystickDeactivate {
    pub entity: Entity,
}

#[derive(Component, Default)]
struct Activated  {
    center: Vec2,
    offset: Vec2,
    touch_id: Option<u64>,
}

#[derive(Component)]
struct MaxDistance(f32);

#[derive(Component)]
struct ElasticRebound {
    offset: Vec2,
    duration: f32,
    time: f32,
}


impl Default for Joystick {
    fn default() -> Self {
        Self {
            radius: 100.0,
            thumb_radius: 50.0,
            thumb_max_distance: 90.0,
            enable_elastic_rebound: true,
            elastic_rebound_duration: 0.2,
        }
    }
}

impl Default for ElasticRebound {
    fn default() -> Self {
        Self {
            offset: Vec2::ZERO,
            duration: 0.0,
            time: 0.0,
        }
    }
}


impl Plugin for JoystickPlugin {
    fn build(&self, app: &mut App) {
        app
        .add_observer(joystick_on_add)
        .add_observer(joystick_on_remove)
        .add_systems(First, joystick_idle_system)
        .add_systems(PostUpdate, (joystick_activate_system, joystick_thumb_elastic_rebound_system))
        .add_message::<JoystickActivate>()
        .add_message::<JoystickMove>()
        .add_message::<JoystickDeactivate>();
    }
}

fn joystick_on_add(
    on_add: On<Add, Joystick>,
    mut commands: Commands,
    children_query: Query<&Children>,
    joystick_query: Query<&Joystick>,
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

        let joystick_component = joystick_query.get(joystick_entity).unwrap();
        let joystick_base_radius = joystick_component.radius;
        let joystick_thumb_radius = joystick_component.thumb_radius;

        commands.spawn((
            Node {
                width: Val::Px(joystick_base_radius*2.0),
                height: Val::Px(joystick_base_radius*2.0),
                display: Display::Flex,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border_radius: BorderRadius::all(Val::Percent(50.0)),
                ..Default::default()
            },
            MaxDistance(joystick_component.thumb_max_distance),
            BackgroundColor(Color::hsl(310.0, 0.6, 0.8)),
            JoystickBase::default(),
            ChildOf(joystick_entity),
            children![(
                Node {
                    width: Val::Px(joystick_thumb_radius*2.0),
                    height: Val::Px(joystick_thumb_radius*2.0),
                    border_radius: BorderRadius::all(Val::Percent(50.0)),
                    ..Default::default()
                },
                BackgroundColor(Color::hsl(30.0, 0.6, 0.8)),
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
            Without<Activated>,
        ),
    >,
    touches: Res<Touches>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut mouse_button_input_reader: MessageReader<MouseButtonInput>,
    mut joystick_activate_writer: MessageWriter<JoystickActivate>,
) {

    if joystick_query.is_empty() {
        return;
    }

    let window = windows.single().unwrap();
    let cursor_pos = window.cursor_position();
    let physical_cursor_pos = window.physical_cursor_position();
    let physical_scaler = window.scale_factor();
    joystick_query.iter()
        .for_each(|(entity, computed_node, global_transform)| {

            let mut start_pos: Option<Vec2> = None;
            let mut touch_id: Option<u64> = None;
            // Check mouse cursor
            if let (Some(cursor_pos), Some(physical_cursor_pos)) = (cursor_pos, physical_cursor_pos) {
                mouse_button_input_reader
                    .read()
                    .for_each(|mouse_button_input| {
                        if mouse_button_input.button == MouseButton::Left
                            && mouse_button_input.state.is_pressed()
                        {
                            if computed_node.contains_point(*global_transform, physical_cursor_pos) {
                                start_pos = Some(cursor_pos);
                            }
                        }
                    });
            }

            // Check touches
            for touch in touches.iter_just_pressed() {
                let touch_pos = touch.start_position();
                if computed_node.contains_point(*global_transform, touch_pos * physical_scaler) {
                    start_pos = Some(touch_pos);
                    touch_id = Some(touch.id());
                    info!("window physical_size: {:?} size: {:?}", windows.single().unwrap().physical_size(), windows.single().unwrap().size());
                }
            }

            start_pos.map(|start_pos|{
                commands.entity(entity).insert(Activated{
                    center: global_transform.translation.xy() / physical_scaler,
                    touch_id,
                    offset: Vec2::ZERO,
                });
                joystick_activate_writer.write(JoystickActivate{
                    entity,
                    start_pos,
                });
                info!("Joystick activated start: {:?} center: {:?}", start_pos, global_transform.affine().translation.xy());
            });
        });
}

fn joystick_activate_system(
    mut commands: Commands,
    mut joystick_activate_query: Query<(Entity, &mut Activated, &UiGlobalTransform, &MaxDistance, &Children)>,
    mut ui_transform_query: Query<&mut UiTransform>,
    mut mouse_button_input_reader: MessageReader<MouseButtonInput>,
    touches: Res<Touches>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut joystick_move_writer: MessageWriter<JoystickMove>,
    mut joystick_deactivate_writer: MessageWriter<JoystickDeactivate>,
) {

    if joystick_activate_query.is_empty() {
        return;
    }

    let window = windows.single().unwrap();
    let cursor_pos = window.cursor_position();

    for (entity, mut active_info, .., max_distance, children) in joystick_activate_query.iter_mut() {
        let mut pointer_pos: Option<Vec2> = None;
        let mut deactivated = false;
        match active_info.touch_id {
            Some(touch_id) => {
                match touches.iter().find(|touch| touch.id() == touch_id) {
                    Some(touch) => {
                        pointer_pos = Some(touch.position());
                        joystick_move_writer.write(JoystickMove{
                            entity,
                            pos: touch.position() - active_info.center,
                        });
                        info!("Touch {:?} pos: {:?} offset: {:?}", touch_id, touch.position(), active_info.offset);
                    },
                    None => {deactivated = true;},
                }
            },
            None => {
                pointer_pos = cursor_pos;
                if let Some(cursor_pos) = cursor_pos {
                    joystick_move_writer.write(JoystickMove{
                        entity,
                        pos: cursor_pos - active_info.center,
                    });
                    // info!("output: {:?}", cursor_pos - active_info.center);

                    // 鼠标释放检查
                    deactivated = mouse_button_input_reader.read()
                        .any(|input| input.button == MouseButton::Left && !input.state.is_pressed());
                }
            }
        }
        if deactivated {
            commands.entity(entity).remove::<Activated>().insert(ElasticRebound{
                offset: active_info.offset,
                duration: 0.1,
                ..Default::default()
            });
            joystick_deactivate_writer.write(JoystickDeactivate { entity });
        }
        else if let Some(pointer_pos) = pointer_pos {
            let direction = (pointer_pos - active_info.center).normalize_or_zero();
            let distance = (pointer_pos - active_info.center).length().min(max_distance.0);
            let offset = direction * distance;
            active_info.offset = offset;
            children.iter().for_each(|child| {
                if let Ok(mut transform) = ui_transform_query.get_mut(child) {
                    transform.translation = Val2::new(Val::Px(offset.x), Val::Px(offset.y));
                }
            });
        }

    }
}

fn joystick_thumb_elastic_rebound_system(
    mut commands: Commands,
    mut joystick_thumb_elastic_rebound_query: Query<(Entity, &mut ElasticRebound, &Children)>,
    mut ui_transform_query: Query<&mut UiTransform>,
    time: Res<Time>,
) {
    for (entity, mut elastic_rebound, children) in joystick_thumb_elastic_rebound_query.iter_mut() { 
        elastic_rebound.time += time.delta_secs();
        let t = (elastic_rebound.time / elastic_rebound.duration).min(1.0);
        // 弹性插值公式：使用指数衰减和余弦函数模拟弹性效果
        // 参数调整：k控制衰减速度，w控制振动频率
        let k = 5.0;
        let w = 6.0;
        let elastic_factor = (-k * t).exp() * (w * t).cos();
        // 计算当前位置：起始位置 * 弹性因子
        let pos = elastic_rebound.offset * elastic_factor;
        children.iter().for_each(|child| {
            if let Ok(mut transform) = ui_transform_query.get_mut(child) {
                transform.translation = Val2::new(Val::Px(pos.x), Val::Px(pos.y));
            }
        });
        if t >= 1.0 {
            commands.entity(entity).remove::<ElasticRebound>();
        }
    }
}