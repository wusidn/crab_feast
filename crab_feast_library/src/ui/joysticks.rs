use bevy::{
    platform::collections::HashMap, prelude::*
};


pub struct JoysticksPlugin;


#[derive(Component, Clone, Debug, Default)]
pub struct JoystickIntput(pub Vec2);

#[derive(Component, Clone, Copy, Debug, Default)]
pub enum JoystickState {
    #[default]
    Idle,
    Running(Vec2, f32, Option<u64>),
}

#[derive(Component)]
pub struct JoystickRing;

#[derive(Component)]
pub struct JoystickDot;

#[derive(Debug, Clone, Copy)]
struct PressedInfo {
    entity: Entity,
    start: Vec2,
    stack_index: u32,
}

#[derive(Bundle, Clone, Debug)]
pub struct JoystickBundle {
    pub node: Node,
    pub input: JoystickIntput,
    pub status: JoystickState,
    pub interaction: Interaction,
}

impl Default for JoystickBundle {
    fn default() -> Self {
        Self {
            node: Node {
                width: Val::Vw(50.0),
                height: Val::Vh(100.0),
                ..Default::default()
            },
            input: Default::default(),
            status: Default::default(),
            interaction: Default::default(),
        }
    }
}
#[derive(Resource, Default)]
struct CursorPosition(Vec2);


impl Plugin for JoysticksPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, Self::joystick_system).init_resource::<CursorPosition>();
    }
}

macro_rules! update_joystick {
    ($dot_query_iter:expr, $node_query:expr, $joystick_input:expr, $radius:expr) => {
        $dot_query_iter.for_each(|dot_entity| {
            let _ = $node_query.get_mut(dot_entity).map(|mut node| {
                node.left = Val::Px($joystick_input.x * $radius);
                node.top = Val::Px($joystick_input.y * $radius);
            });
        })   
    }
}

impl JoysticksPlugin {
    fn joystick_system(
        interaction_query: Query<(Entity, &Interaction, &ComputedNode, &GlobalTransform)>,
        mut joystick_query: Query<(Entity, &mut JoystickState, &mut JoystickIntput)>,
        ring_query: Query<(Entity, &ComputedNode), With<JoystickRing>>,
        dot_query: Query<Entity, With<JoystickDot>>,
        children_query: Query<&Children>,
        mut node_query: Query<&mut Node>,
        touch_inputs: Res<Touches>,
        mut cursor_moved_events: MessageReader<CursorMoved>,
        mut cursor_pos: ResMut<CursorPosition>,
    ) {
        for event in cursor_moved_events.read() {
            cursor_pos.0 = event.position;
        }
        let mut touchid_entities_map: HashMap<Option<u64>, PressedInfo> = Default::default();
        // moust input
        (!touch_inputs.is_changed()).then(|| {
            interaction_query.iter()
            .filter(|(_, interaction, ..)| *interaction == &Interaction::Pressed)
            .for_each(|(entity, _, computed_node, ..)| {
                touchid_entities_map.insert(None, PressedInfo {
                    entity,
                    start: cursor_pos.0,
                    stack_index: computed_node.stack_index(),
                });
            });
        });

        // touch input
        touch_inputs.iter_just_pressed()
        .for_each(|touch| 
            interaction_query.iter()
            .filter(|(_, _, cn, transform)| Rect::from_center_size(transform.translation().truncate()* cn.inverse_scale_factor(), cn.size() * cn.inverse_scale_factor()).contains(touch.position()))
            .for_each(|(entity, _, computed_node, ..)| {
                touchid_entities_map.get(&Some(touch.id())).filter(|info| info.stack_index > computed_node.stack_index()).is_none().then(|| {
                    touchid_entities_map.insert(Some(touch.id()), PressedInfo{
                        entity,
                        start: touch.position(),
                        stack_index: computed_node.stack_index(),
                    });
                });
            })
        );

        joystick_query.iter_mut().for_each(|(joystick, mut status, mut input)| {
            let interaction_data = interaction_query.get(joystick);
            if let Ok((_, interaction, cn, transform)) = interaction_data {
                match *status {
                    JoystickState::Idle => {
                        touchid_entities_map.iter().find(|(_, pressed_info)| pressed_info.entity == joystick)
                        .map(|(touch_id, pressed_info)| {
                            let node_rect = Rect::from_center_size(transform.translation().truncate() * cn.inverse_scale_factor(), cn.size() * cn.inverse_scale_factor());
                            let mut radius = 1.0;
                            children_query.get(joystick).iter()
                            .for_each(|children|
                                children.iter().filter_map(|child| ring_query.get(child).ok())
                                .for_each(|(ring, ring_node)|{
                                    let ring_size = ring_node.size() * ring_node.inverse_scale_factor();
                                    let ring_half_size = (ring_size.length() == 0.0).then(|| Vec2::ZERO).unwrap_or(ring_size / 2.0);
                                    let left_top = pressed_info.start - node_rect.min - ring_half_size;
                                    let _ = node_query.get_mut(ring).map(|mut node| {
                                        node.left = Val::Px(left_top.x);
                                        node.top = Val::Px(left_top.y);
                                    });
                                    radius = ring_half_size.x.min(ring_half_size.y);
                                    children_query.get(ring).iter().for_each(|children|{
                                        update_joystick!(children.iter().filter_map(|child| dot_query.get(child).ok()), node_query, input.0, radius);
                                    });
                                })
                            );
                            input.0 = Vec2::ZERO;
                            *status = JoystickState::Running(pressed_info.start, radius, *touch_id);
                        });
                    }

                    JoystickState::Running(start, radius, touch_id) => {
                        input.0 = touch_id.is_some()
                        .then(||touch_inputs.get_pressed(touch_id.unwrap()).map(|touch| touch.position() - start))
                        .or_else(||(interaction == &Interaction::Pressed).then(|| Some(cursor_pos.0 - start))).flatten()
                        .map(|distance|if distance.length_squared() >= 0.0001 {distance.normalize() * (distance.length() / radius).min(1.0)} else {Vec2::ZERO})
                        .or_else(||{*status = JoystickState::Idle; Some(Vec2::ZERO)})
                        .unwrap();

                        children_query.get(joystick).iter()
                        .for_each(|children|
                            children.iter().filter_map(|child| ring_query.get(child).ok())
                            .for_each(|(ring, ..)|{
                                children_query.get(ring).iter().for_each(|children|{
                                    update_joystick!(children.iter().filter_map(|child| dot_query.get(child).ok()), node_query, input.0, radius);
                                });
                            })
                        );
                    }
                }
            } else {
                return;
            }
        });
    }

}
