use bevy::{input::mouse::MouseButtonInput, prelude::*, ui::UiStack, window::PrimaryWindow};

pub struct JoystickPlugin;

#[derive(Component)]
pub struct Joystick {
    pub thumb_radius_percent: f32,
    pub thumb_max_distance_percent: f32,
    pub enable_elastic_rebound: bool,
    pub elastic_rebound_duration: f32,
}
#[derive(Component, Default)]
pub struct JoystickState {
    pub direction: Vec2,
    pub force: f32,
}

#[derive(Component)]
pub struct JoystickThumb;

#[derive(Component, Default)]
pub struct JoystickDisabled;

#[derive(Message)]
pub struct JoystickActivate {
    pub entity: Entity,
}

 pub struct JoystickDeactivate {
    pub entity: Entity,
}

#[derive(Message)]
pub enum JoystickEvent {
    Activate(Entity),
    Changed(Entity, Vec2, f32),
    Deactivate(Entity),
    ThumbReset(Entity),
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
            thumb_radius_percent: 50.0,
            thumb_max_distance_percent: 100.0,
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
        .add_message::<JoystickEvent>();
    }
}

fn joystick_on_add(
    on_add: On<Add, Joystick>,
    mut commands: Commands,
    children_query: Query<&Children>,
    joystick_query: Query<&Joystick>,
    joystick_thumb_query: Query<&JoystickThumb>,
) {
    info!("Joystick added");

    let joystick_entity = on_add.event_target();
    let children = children_query.get(joystick_entity);

    if children.map_or(true, |children| {
        children
            .iter()
            .find(|child| joystick_thumb_query.get(*child).is_ok())
            .is_none()
    }) {

        let joystick_component = joystick_query.get(joystick_entity).unwrap();

        commands.spawn((
            Node {
                width: Val::Percent(joystick_component.thumb_radius_percent),
                height: Val::Percent(joystick_component.thumb_radius_percent),
                border_radius: BorderRadius::all(Val::Percent(50.0)),
                ..Default::default()
            },
            BackgroundColor(Color::hsl(30.0, 0.3, 0.7)),
            JoystickThumb,
            ChildOf(joystick_entity),
        ));

        commands.entity(joystick_entity).insert(JoystickState::default());
    }
}

fn joystick_on_remove(
    on_remove: On<Remove, Joystick>,
    mut commands: Commands,
    children_query: Query<&Children>,
    joystick_thumb_query: Query<&JoystickThumb>,
) {
    info!("Joystick removed");
    if let Ok(children) = children_query.get(on_remove.event_target()) {
        children.iter().for_each(|child| {
            if joystick_thumb_query.get(child).is_ok() {
                commands.entity(child).despawn();
            }
        });
    }
}

fn joystick_idle_system(
    mut commands: Commands,
    joystick_query: Query<
        (Entity, &ComputedNode, &UiGlobalTransform, &Joystick, &ComputedUiTargetCamera),
        (
            With<Joystick>,
            Without<JoystickDisabled>,
            Without<Activated>,
        ),
    >,
    camera_query: Query<&Camera>,
    mut max_distance_query: Query<&mut MaxDistance>,
    touches: Res<Touches>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut mouse_button_input_reader: MessageReader<MouseButtonInput>,
    mut joystick_event_writer: MessageWriter<JoystickEvent>,
    ui_stack: Res<UiStack>, //todo: 检查是否被遮挡
) {

    if joystick_query.is_empty() {
        return;
    }

    let window = windows.single().unwrap();
    let cursor_pos = window.cursor_position();
    let physical_cursor_pos = window.physical_cursor_position();
    let physical_scaler = window.scale_factor();
    joystick_query.iter()
        .for_each(|(joystick_entity, computed_node, global_transform, joystick_component, target_camera)| {
            
            let viewport_offset: Vec2 = target_camera.get().map_or(None, |camera| {
                camera_query.get(camera).map_or(None, |camera| camera.physical_viewport_rect().map(|rect| rect.min.as_vec2()))
            }).map_or(Vec2::ZERO, |offset| offset);

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
                            if computed_node.contains_point(*global_transform, physical_cursor_pos - viewport_offset) {
                                start_pos = Some(cursor_pos);
                            }
                        }
                    });
            }

            // Check touches
            for touch in touches.iter_just_pressed() {
                let touch_pos = touch.start_position();
                if computed_node.contains_point(*global_transform, touch_pos * physical_scaler - viewport_offset) {
                    start_pos = Some(touch_pos);
                    touch_id = Some(touch.id());
                    info!("window physical_size: {:?} size: {:?}", windows.single().unwrap().physical_size(), windows.single().unwrap().size());
                }
            }

            start_pos.map(|start_pos|{

                // check z-index todo iterator ui task

                commands.entity(joystick_entity).insert(Activated{
                    center: global_transform.translation.xy() / physical_scaler,
                    touch_id,
                    offset: Vec2::ZERO,
                });
                let max_distance = computed_node.size().xy().length() * joystick_component.thumb_max_distance_percent / (100.0 + joystick_component.thumb_radius_percent) / 2.0 / physical_scaler;
                match max_distance_query.get_mut(joystick_entity) {
                    Ok(mut max_distance_component) => {
                        max_distance_component.0 = max_distance;
                    },
                    Err(_) => {
                        commands.entity(joystick_entity).insert(MaxDistance(max_distance));
                    },
                }
                joystick_event_writer.write( JoystickEvent::Activate(joystick_entity));
                info!("Joystick activated start: {:?} center: {:?}", start_pos, global_transform.affine().translation.xy());
            });
        });
}

fn joystick_activate_system(
    mut commands: Commands,
    mut joystick_activate_query: Query<(Entity, &mut Activated, &UiGlobalTransform, &MaxDistance, &Children, &ComputedUiTargetCamera)>,
    mut joystick_state_query: Query<&mut JoystickState>,
    mut ui_transform_query: Query<&mut UiTransform, With<JoystickThumb>>,
    mut mouse_button_input_reader: MessageReader<MouseButtonInput>,
    touches: Res<Touches>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut joystick_event_writer: MessageWriter<JoystickEvent>,
    camera_query: Query<&Camera>,
) {

    if joystick_activate_query.is_empty() {
        return;
    }

    let window = windows.single().unwrap();
    let cursor_pos = window.cursor_position();

    for (joystick_entity, mut active_info, .., max_distance, children, target_camera) in joystick_activate_query.iter_mut() {
        let mut pointer_pos: Option<Vec2> = None;
        let mut deactivated = false;

        let logical_viewport_offset: Vec2 = target_camera.get().map_or(None, |camera| {
                    camera_query.get(camera).map_or(None, |camera| camera.logical_viewport_rect().map(|rect| rect.min))
                }).map_or(Vec2::ZERO, |offset| offset);

        match active_info.touch_id {
            Some(touch_id) => {
                match touches.iter().find(|touch| touch.id() == touch_id) {
                    Some(touch) => {
                        pointer_pos = Some(touch.position() - logical_viewport_offset);
                    },
                    None => {deactivated = true;},
                }
            },
            None => {
                pointer_pos = cursor_pos.map(|pos| pos - logical_viewport_offset);
                // 鼠标释放检查
                deactivated = mouse_button_input_reader.read()
                    .any(|input| input.button == MouseButton::Left && !input.state.is_pressed());
            }
        }
        if deactivated {
            commands.entity(joystick_entity).remove::<Activated>().insert(ElasticRebound{
                offset: active_info.offset,
                duration: 0.1,
                ..Default::default()
            });
            if let Ok(mut joystick_state) = joystick_state_query.get_mut(joystick_entity) {
                joystick_state.direction = Vec2::ZERO;
                joystick_state.force = 0.0;
            }
            joystick_event_writer.write(JoystickEvent::Changed(joystick_entity, Vec2::ZERO, 0.0));
            joystick_event_writer.write( JoystickEvent::Deactivate(joystick_entity));
        }
        else if let Some(pointer_pos) = pointer_pos {
            let distance = pointer_pos - active_info.center;
            let direction = distance.normalize_or_zero();
            let capped_distance = distance.length().min(max_distance.0);
            let offset = direction * capped_distance;
            let force = match capped_distance {
                0.0 => 0.0,
                _ => capped_distance / max_distance.0,
            };
            active_info.offset = offset;
            children.iter().for_each(|child| {
                if let Ok(mut transform) = ui_transform_query.get_mut(child) {
                    transform.translation = Val2::new(Val::Px(offset.x), Val::Px(offset.y));
                }
            });
            if let Ok(mut joystick_state) = joystick_state_query.get_mut(joystick_entity) {
                if (joystick_state.direction - direction).length() < 0.01 && (joystick_state.force - force).abs() < 0.01 {
                    continue;
                }
                joystick_state.direction = direction;
                joystick_state.force = force;
                joystick_event_writer.write( JoystickEvent::Changed(joystick_entity, direction, force));
            }
        }

    }
}

fn joystick_thumb_elastic_rebound_system(
    mut commands: Commands,
    mut joystick_thumb_elastic_rebound_query: Query<(Entity, &mut ElasticRebound, &Children)>,
    mut ui_transform_query: Query<&mut UiTransform>,
    mut joystick_event_writer: MessageWriter<JoystickEvent>,
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
            joystick_event_writer.write(JoystickEvent::ThumbReset(entity));
        }
    }
}