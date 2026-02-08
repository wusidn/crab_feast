
use bevy::{input::{ButtonState, mouse::MouseButtonInput, touch::TouchPhase}, picking::pointer::PointerId, prelude::*};

pub struct JoystickPlugin;

#[derive(Component)]
pub struct Joystick {
    pub hit_area_percent: f32,
    pub thumb_percent: f32,
    pub thumb_max_distance_percent: f32,
    pub elastic_rebound_enabled: bool,
    pub elastic_rebound_duration_secs: f32,
}
#[derive(Component, Default)]
pub struct JoystickState {
    pub direction: Vec2,
    pub force: f32,
}

#[derive(Component, Debug, Clone, Reflect)]
pub struct Activated {
    pub pointer: PointerId,
    pub center_position: Vec2,
    pub max_distance: f32,
}

#[derive(Component)]
pub struct JoystickThumb;

#[derive(Component, Default)]
pub struct JoystickDisabled;

#[derive(Component)]
struct ElasticRebound {
    offset: Vec2,
    duration: f32,
    time: f32,
}

#[derive(Reflect, Clone, Debug, PartialEq)]
pub enum JoystickInteraction {
    Activated(PointerId),
    Moved(Vec2, f32),
    Deactivated(PointerId),
    Rebound,
}

#[derive(Message, EntityEvent, Clone, PartialEq, Debug, Reflect)]
pub struct JoystickEvent {
    pub entity: Entity,
    pub event: JoystickInteraction,
}

impl Default for Joystick {
    fn default() -> Self {
        Self {
            hit_area_percent: 100.0,
            thumb_percent: 50.0,
            thumb_max_distance_percent: 75.0,
            elastic_rebound_enabled: true,
            elastic_rebound_duration_secs: 0.2,
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
        app.add_observer(joystick_on_add)
            .add_observer(joystick_on_remove)
            .add_systems(Update, joystick_on_release)
            .add_systems(Update, joystick_thumb_elastic_rebound_system);
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

    let mut joystick_thumb_entity = children.map_or(None, |children| {
        children
            .iter()
            .find(|child| joystick_thumb_query.get(*child).is_ok())
    });

    if joystick_thumb_entity.is_none() {
        let joystick_component = joystick_query.get(joystick_entity).unwrap();

        joystick_thumb_entity = Some(
            commands
                .spawn((
                    Node {
                        width: Val::Percent(joystick_component.thumb_percent),
                        height: Val::Percent(joystick_component.thumb_percent),
                        border_radius: BorderRadius::all(Val::Percent(50.0)),
                        ..Default::default()
                    },
                    BackgroundColor(Color::hsl(30.0, 0.3, 0.7)),
                    JoystickThumb,
                    ChildOf(joystick_entity),
                ))
                .id(),
        );
    }

    if let Some(thumb_entity) = joystick_thumb_entity {
        commands.entity(thumb_entity).with_children(|parent| {
            parent.spawn((Observer::new(joystick_on_press).with_entity(joystick_entity),));
            parent.spawn((Observer::new(joystick_on_drag).with_entity(joystick_entity),));
        });

        commands
            .entity(joystick_entity)
            .insert(JoystickState::default());
    }
}

fn joystick_on_remove(
    on_remove: On<Remove, Joystick>,
    mut commands: Commands,
    children_query: Query<&Children>,
    joystick_thumb_query: Query<&JoystickThumb>,
) {
    info!("Joystick removed");
    let joystick_entity = on_remove.event_target();
    if let Ok(children) = children_query.get(joystick_entity) {
        children.iter().for_each(|child| {
            if joystick_thumb_query.get(child).is_ok() {
                commands.entity(child).despawn();
            }
        });
    }
}

fn joystick_on_press(
    event: On<Pointer<Press>>,
    mut commands: Commands,
    camera_query: Query<&Camera>,
    mut joystick_state_query: Query<(&mut JoystickState, &ComputedNode, &Joystick, &UiGlobalTransform, &Children), (Without<JoystickDisabled>, Without<Activated>)>,
    mut transform_query: Query<&mut UiTransform, With<JoystickThumb>>,
) {
    let joystick_entity = event.event_target();
    
    let camera = camera_query.get(event.hit.camera).unwrap();
    let viewport_rect_min = camera.logical_viewport_rect().map_or(Vec2::ZERO, |rect| rect.min);
    let scale_factor = camera.computed.target_info.as_ref().map(|info| info.scale_factor).unwrap_or(1.0);

    if let Ok((mut joystick_state, computed_node, joystick, ui_global_transform, children)) = joystick_state_query.get_mut(joystick_entity)
    {
        // 获取点击位置 logic （相对于窗口左上角的逻辑坐标系）
        let pointer_position = event.pointer_location.position;

        // 获取 Joystick UI (相对于视口左上角的物理坐标系)
        let joystick_position = ui_global_transform.translation;


        let activated = Activated { 
            pointer: event.pointer_id,
            center_position: joystick_position / scale_factor + viewport_rect_min,
            max_distance: (computed_node.size / scale_factor).length() * joystick.thumb_max_distance_percent / 100.0 / 2.0,
        };

        let pointer_center_sub = pointer_position - activated.center_position;
        joystick_state.direction = pointer_center_sub.normalize();
        joystick_state.force = pointer_center_sub.length().min(activated.max_distance);

        commands.entity(joystick_entity).insert(activated);

        // 更新 Thumb 位置
        joystick_thumb_update(joystick_state.as_ref(), children, &mut transform_query);
    }

    commands.trigger(JoystickEvent {
        entity: joystick_entity,
        event: JoystickInteraction::Activated(event.pointer_id),
    });
}

fn joystick_on_drag(
    event: On<Pointer<Drag>>,
    mut commands: Commands,
    mut joystick_state_query: Query<(&mut JoystickState, &Activated, &Children)>,
    mut transform_query: Query<&mut UiTransform, With<JoystickThumb>>,
) {
    let joystick_entity = event.event_target();

    if let Ok((mut joystick_state, activated, children)) = joystick_state_query.get_mut(joystick_entity) {
        let pointer_position = event.pointer_location.position;

        let thumb_position = pointer_position - activated.center_position;
        joystick_state.direction = thumb_position.normalize();
        joystick_state.force = thumb_position.length().min(activated.max_distance);

        // 更新 Thumb 位置
        joystick_thumb_update(joystick_state.as_ref(), children, &mut transform_query);

        commands.trigger(JoystickEvent {
            entity: joystick_entity,
            event: JoystickInteraction::Moved(joystick_state.direction, joystick_state.force),
        });
    }
}

fn joystick_on_release(
    mut commands: Commands,
    joystick_activated_query: Query<(Entity, &Activated)>,
    mut mouse_button_input_reader: MessageReader<MouseButtonInput>,
    mut touch_input_reader: MessageReader<TouchInput>,
    mut deactivated_entities: Local<Vec<Entity>>,
    mut joystick_state_query: Query<&mut JoystickState>,
) {

    if joystick_activated_query.iter().count() == 0 {
        return;
    }
    
    mouse_button_input_reader.read().for_each(|event| {
        if event.button != MouseButton::Left || event.state != ButtonState::Released {
            return;
        }
        joystick_activated_query.iter().for_each(|(entity, activated)| {
            match activated.pointer {
                PointerId::Mouse => {
                    deactivated_entities.push(entity);
                },
                _ => {},
            }

        });
    });

    touch_input_reader.read().for_each(|event| {
        if event.phase != TouchPhase::Ended && event.phase != TouchPhase::Canceled {
            return;
        }
        joystick_activated_query.iter().for_each(|(entity, activated)| {
            match activated.pointer {
                PointerId::Touch(id) if id == event.id => {
                    deactivated_entities.push(entity);
                },
                _ => {},
            }
        });
    });

    deactivated_entities.iter().for_each(|entity| {
        if let Ok((_, activated)) = joystick_activated_query.get(*entity) {
            commands.entity(*entity).remove::<Activated>();
            if let Ok(mut joystick_state) = joystick_state_query.get_mut(*entity) {
                commands.entity(*entity)
                .remove::<Activated>()
                .insert(ElasticRebound{
                    offset: joystick_state.direction * joystick_state.force,
                    duration: 0.1,
                    ..Default::default()
                });
                joystick_state.direction = Vec2::ZERO;
                joystick_state.force = 0.0;
            }
            commands.trigger(JoystickEvent {
                entity: *entity,
                event: JoystickInteraction::Deactivated(activated.pointer),
            });
        }
    });
    deactivated_entities.clear();

}

fn joystick_thumb_update(
    joystick_state: &JoystickState,
    joystick_children: &Children,
    transform_query: &mut Query<&mut UiTransform, With<JoystickThumb>>,
) {
    // 更新 Thumb 位置
    joystick_children.iter().for_each(|child| {
        if let Ok(mut thumb_transform) = transform_query.get_mut(child) {
            let thumb_position = joystick_state.direction * joystick_state.force;
            thumb_transform.translation.x = Val::Px(thumb_position.x);
            thumb_transform.translation.y = Val::Px(thumb_position.y);
        }
    });
}

fn joystick_thumb_elastic_rebound_system(
    mut commands: Commands,
    mut joystick_thumb_elastic_rebound_query: Query<(Entity, &mut ElasticRebound, &Children)>,
    mut ui_transform_query: Query<&mut UiTransform, With<JoystickThumb>>,
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
            commands.trigger(JoystickEvent {
                entity: entity,
                event: JoystickInteraction::Rebound,
            });
        }
    }
}