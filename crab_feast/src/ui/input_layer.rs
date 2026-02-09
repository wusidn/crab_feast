use std::any::TypeId;

use bevy::{
    animation::{AnimatedBy, AnimationEntityMut, AnimationEvaluationError, AnimationTargetId},
    input::keyboard::Key,
    picking::pointer::PointerId,
    platform::collections::HashSet,
    prelude::*,
};
use crab_feast_ui_joysticks::{
    Joystick, JoystickEvent, JoystickInteraction, JoystickMarionette, JoystickMarionettePlugin,
    JoystickPlugin,
};

use crate::{
    event::{MoveInputState, RotateInput},
    utils::is_non_mobile,
};

pub struct InputPlugin;

#[derive(Resource, Debug, Default)]
pub struct InputState {
    rotate_ignore_pointers: HashSet<PointerId>,
}

#[derive(Clone)]
struct BackgroundColorProperty;

#[derive(Component)]
struct JoystickFadeAnimatePlayer {
    fade_in_index: AnimationNodeIndex,
    fade_out_index: AnimationNodeIndex,
}

#[derive(Component)]
struct MoveInputJoystick;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(JoystickPlugin)
            .init_resource::<InputState>()
            .init_resource::<MoveInputState>()
            .add_systems(Startup, Self::setup)
            .add_systems(PreUpdate, on_keyboard_event.run_if(is_non_mobile));

        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        app.add_plugins(JoystickMarionettePlugin);
    }
}

impl InputPlugin {
    fn setup(
        mut commands: Commands,
        mut animation_graphs: ResMut<Assets<AnimationGraph>>,
        mut animation_clips: ResMut<Assets<AnimationClip>>,
    ) {
        let joystick_idle_color = Color::hsla(160.0, 0.5, 0.6, 0.03);
        let joystick_active_color = Color::hsla(160.0, 0.5, 0.7, 0.08);

        let animation_target_name = Name::new("joystick_background_fade");
        let animation_target_id = AnimationTargetId::from_name(&animation_target_name);

        // 创建淡出动画
        let mut fade_out_animation_clip = AnimationClip::default();
        fade_out_animation_clip.add_curve_to_target(
            animation_target_id,
            AnimatableCurve::new(
                BackgroundColorProperty,
                AnimatableKeyframeCurve::new([0.0, 1.0, 1.5].into_iter().zip([
                    Srgba::from(joystick_active_color),
                    Srgba::from(joystick_active_color),
                    Srgba::from(joystick_idle_color),
                ]))
                .unwrap(),
            ),
        );

        // 创建淡入动画
        let mut fade_in_animation_clip = AnimationClip::default();
        fade_in_animation_clip.add_curve_to_target(
            animation_target_id,
            AnimatableCurve::new(
                BackgroundColorProperty,
                AnimatableKeyframeCurve::new([0.0, 0.2].into_iter().zip([
                    Srgba::from(joystick_idle_color),
                    Srgba::from(joystick_active_color),
                ]))
                .unwrap(),
            ),
        );

        // 存储动画句柄
        let fade_out_animation_clip_handle = animation_clips.add(fade_out_animation_clip);
        let fade_in_animation_clip_handle = animation_clips.add(fade_in_animation_clip);

        // 创建包含两个动画的动画图
        let mut animation_graph = AnimationGraph::default();
        let fade_in_index =
            animation_graph.add_clip(fade_in_animation_clip_handle, 1.0, animation_graph.root);
        let fade_out_index =
            animation_graph.add_clip(fade_out_animation_clip_handle, 1.0, animation_graph.root);
        let animation_graph_handle = animation_graphs.add(animation_graph);

        let animation_player = AnimationPlayer::default();
        // 初始不播放任何动画（或根据需要选择默认动画）

        let animation_player_entity = commands
            .spawn((
                animation_player,
                AnimationGraphHandle(animation_graph_handle),
                JoystickFadeAnimatePlayer {
                    fade_in_index,
                    fade_out_index,
                },
            ))
            .id();

        let input_layer_entity = commands
            .spawn(Node {
                width: Val::Vw(100.0),
                height: Val::Vh(100.0),
                padding: UiRect::all(Val::Vw(10.0)),
                display: Display::Flex,
                justify_content: JustifyContent::FlexStart,
                align_items: AlignItems::FlexEnd,
                ..Default::default()
            })
            .observe(on_rotate_plane_drag)
            .id();

        commands
            .spawn((
                Node {
                    width: Val::Vw(10.0),
                    height: Val::Vw(10.0),
                    display: Display::Flex,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border: UiRect::all(Val::Percent(0.05)),
                    border_radius: BorderRadius::all(Val::Percent(50.0)),
                    ..Default::default()
                },
                BackgroundColor(joystick_idle_color),
                BorderColor::all(Color::hsla(0.0, 1.0, 1.0, 0.2)),
                Joystick {
                    ..Default::default()
                },
                AnimatedBy(animation_player_entity),
                animation_target_id,
                animation_target_name,
                ChildOf(input_layer_entity),
                MoveInputJoystick,
            ))
            .observe(on_joystick_event);
    }
}

fn on_joystick_event(
    joystick_event: On<JoystickEvent>,
    mut joystick_fade_animate_player_query: Query<(
        &mut AnimationPlayer,
        &JoystickFadeAnimatePlayer,
    )>,
    mut move_input_state: ResMut<MoveInputState>,
    mut input_state: ResMut<InputState>,
) {
    match joystick_event.event {
        JoystickInteraction::Activated(pointer_id) => {
            // println!("Joystick activated: {:?}", joystick_event.entity);
            input_state.rotate_ignore_pointers.insert(pointer_id);
            *move_input_state = MoveInputState::Activated {
                direction: Vec2::ZERO,
                force: 0.0,
            };

            joystick_fade_animate_player_query.iter_mut().for_each(
                |(mut animation_player, joystick_fade_animate_player)| {
                    animation_player.stop_all();
                    // 播放淡入动画
                    animation_player.play(joystick_fade_animate_player.fade_in_index);
                },
            );
        }
        JoystickInteraction::Moved(new_direction, new_force) => {
            // println!("Joystick moved: {:?}", joystick_event.entity);
            if let MoveInputState::Activated { direction, force } = move_input_state.as_mut() {
                *direction = new_direction;
                *force = new_force;
            }
        }
        JoystickInteraction::Deactivated(pointer_id) => {
            // println!("Joystick deactivated: {:?}", joystick_event.entity);
            *move_input_state = MoveInputState::Idle;
            input_state.rotate_ignore_pointers.remove(&pointer_id);
        }
        JoystickInteraction::Rebound => {
            // println!("Joystick rebound: {:?}", joystick_event.entity);

            joystick_fade_animate_player_query.iter_mut().for_each(
                |(mut animation_player, joystick_fade_animate_player)| {
                    animation_player.stop_all();
                    // 播放淡出动画
                    animation_player.play(joystick_fade_animate_player.fade_out_index);
                },
            );
        }
    }
}

impl AnimatableProperty for BackgroundColorProperty {
    type Property = Srgba;

    fn evaluator_id(&self) -> EvaluatorId<'_> {
        EvaluatorId::Type(TypeId::of::<Self>())
    }

    fn get_mut<'a>(
        &self,
        entity: &'a mut AnimationEntityMut,
    ) -> Result<&'a mut Self::Property, AnimationEvaluationError> {
        let background_color = entity
            .get_mut::<BackgroundColor>()
            .ok_or(AnimationEvaluationError::ComponentNotPresent(TypeId::of::<
                BackgroundColor,
            >(
            )))?
            .into_inner();

        if let Color::Srgba(_) = background_color.0 {
        } else {
            background_color.0 = Color::from(background_color.0.to_srgba());
        }

        match background_color.0 {
            Color::Srgba(ref mut color) => Ok(color),
            _ => Err(AnimationEvaluationError::PropertyNotPresent(TypeId::of::<
                Srgba,
            >(
            ))),
        }
    }
}

fn on_rotate_plane_drag(
    event: On<Pointer<Drag>>,
    mut commands: Commands,
    input_state: ResMut<InputState>,
    target_camera_query: Query<&ComputedUiTargetCamera>,
    camera_query: Query<&Camera>,
) {
    if input_state
        .rotate_ignore_pointers
        .contains(&event.pointer_id)
    {
        return;
    }

    let scaled_delta = target_camera_query
        .get(event.entity)
        .ok()
        .and_then(|target| target.get())
        .and_then(|camera_entity| camera_query.get(camera_entity).ok())
        .map(|camera| {
            camera
                .physical_viewport_size()
                .map(|viewport_size| event.delta / viewport_size.x as f32)
                .unwrap_or(event.delta)
        });
    if let Some(delta) = scaled_delta {
        commands.trigger(RotateInput(delta));
    }
}

fn on_keyboard_event(
    mut commands: Commands,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    key_input: Res<ButtonInput<Key>>,
    move_input_joystick_query: Query<Entity, With<MoveInputJoystick>>,
    mut joystick_marionette_query: Query<(Entity, &mut JoystickMarionette)>,
) {
    let walk_speed = 0.65;
    let run_speed = 1.0;

    let mut direction = Vec2::ZERO;
    let mut speed = walk_speed;

    let is_pressed = [KeyCode::KeyW, KeyCode::KeyA, KeyCode::KeyS, KeyCode::KeyD]
        .iter()
        .any(|&key| keyboard_input.pressed(key));
    if !is_pressed {
        joystick_marionette_query.iter().for_each(|(entity, ..)| {
            commands.entity(entity).remove::<JoystickMarionette>();
        });
        return;
    }

    [((KeyCode::KeyW, KeyCode::ArrowUp), -Vec2::Y),
        ((KeyCode::KeyS, KeyCode::ArrowDown), Vec2::Y),
        ((KeyCode::KeyA, KeyCode::ArrowLeft), -Vec2::X),
        ((KeyCode::KeyD, KeyCode::ArrowRight), Vec2::X)]
    .iter()
    .for_each(|&(keys, offset)| {
        if keyboard_input.pressed(keys.0) || keyboard_input.pressed(keys.1) {
            direction += offset;
        }
    });

    if key_input.pressed(Key::Shift) {
        speed = run_speed;
    }

    direction = direction.normalize_or_zero();

    move_input_joystick_query.iter().for_each(|entity| {
        match joystick_marionette_query.get_mut(entity) {
            Ok((_, mut joystick_marionette)) => {
                joystick_marionette.direction = direction;
                joystick_marionette.force = speed;
            },
            Err(_) => {
                commands.entity(entity).insert(JoystickMarionette {
                            direction: direction,
                            force: speed,
                            ..Default::default()
                        });
            }
        };
    });
}
