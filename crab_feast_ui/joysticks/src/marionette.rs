use bevy::{asset::uuid::Uuid, picking::pointer::PointerId, prelude::*};

use crate::{Joystick, JoystickEvent, JoystickInteraction, joystick::{Activated, ElasticRebound, JoystickDisabled, JoystickState}};

pub struct JoystickMarionettePlugin;

#[derive(Component, Debug, Clone, Copy, Reflect)]
pub struct JoystickMarionette {
    pub direction: Vec2,
    pub force: f32,
    pub pointer_id: PointerId,
}

impl Default for JoystickMarionette {
    fn default() -> Self {
        Self {
            direction: Vec2::ZERO,
            force: 0.0,
            pointer_id: PointerId::Custom(Uuid::new_v4()),
        }
    }
}

impl Plugin for JoystickMarionettePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (
            update_joystick_marionette,
        ))
        .add_observer(on_joystick_marionette_added)
        .add_observer(on_joystick_marionette_removed);
    }
}


fn on_joystick_marionette_added(
    on_add: On<Add, JoystickMarionette>,
    mut commands: Commands,
    camera_query: Query<&Camera>,
    mut query: Query<(&Joystick, &mut JoystickState, &JoystickMarionette, &ComputedNode, &ComputedUiTargetCamera), Without<JoystickDisabled>>,
    elastic_rebound_query: Query<&ElasticRebound>,
) {
    let joystick_entity = on_add.event_target();
    if let Ok((joystick, mut joystick_state, joystick_marionette, computed_node, computed_ui_target_camera)) = query.get_mut(joystick_entity) {

        joystick_state.direction = joystick_marionette.direction;
        joystick_state.force = joystick_marionette.force;

        let camera = computed_ui_target_camera.get().and_then(|entity| camera_query.get(entity).ok()).unwrap();
        let scale_factor = camera.computed.target_info.as_ref().map(|info| info.scale_factor).unwrap_or(1.0);
        let max_distance = (computed_node.size / scale_factor).length() * joystick.thumb_max_distance_percent / 100.0 / 2.0;
        commands.entity(joystick_entity).insert(Activated {
            pointer: joystick_marionette.pointer_id,
            center_position: Vec2::ZERO,
            max_distance,
        });

        if elastic_rebound_query.get(joystick_entity).is_ok() {
            commands.entity(joystick_entity).remove::<ElasticRebound>();
        }

        commands.trigger(JoystickEvent {
            entity: joystick_entity,
            event: JoystickInteraction::Activated(joystick_marionette.pointer_id),
        });

        commands.trigger(JoystickEvent {
            entity: joystick_entity,
            event: JoystickInteraction::Moved(joystick_state.direction, joystick_state.force),
        });
    }
}

fn on_joystick_marionette_removed(
    on_remove: On<Remove, JoystickMarionette>,
    mut commands: Commands,
    mut query: Query<(&mut JoystickState, &Activated), Without<JoystickDisabled>>,
) {
    let joystick_entity = on_remove.event_target();
    if let Ok((mut joystick_state, activated)) = query.get_mut(joystick_entity) {

        commands.entity(joystick_entity)
            .remove::<Activated>()
            .insert(ElasticRebound{
                offset: joystick_state.direction * joystick_state.force * activated.max_distance,
                duration: 0.1,
                ..Default::default()
            });

        joystick_state.direction = Vec2::ZERO;
        joystick_state.force = 0.0;

        commands.trigger(JoystickEvent {
            entity: joystick_entity,
            event: JoystickInteraction::Moved(joystick_state.direction, joystick_state.force),
        });
    }
}

fn update_joystick_marionette(
    mut commands: Commands,
    mut joystick_marionettes: Query<(Entity, &JoystickMarionette, &mut JoystickState)>,
) {
    for ( joystick_entity, joystick_marionette, mut joystick_state) in joystick_marionettes.iter_mut() {

        if (joystick_state.direction.dot(joystick_marionette.direction) - 1.0).abs() <= 0.01 && (joystick_state.force - joystick_marionette.force).abs() < 0.01 {
            continue;
        }

        joystick_state.direction = joystick_marionette.direction;
        joystick_state.force = joystick_marionette.force;

        commands.trigger(JoystickEvent {
            entity: joystick_entity,
            event: JoystickInteraction::Moved(joystick_state.direction, joystick_state.force),
        });
    }
}