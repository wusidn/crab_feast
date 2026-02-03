use std::any::TypeId;

use bevy::{animation::{AnimatedBy, AnimationEntityMut, AnimationEvaluationError, AnimationTargetId}, prelude::*};
use crab_feast_ui_joysticks::{Joystick, JoystickEvent, JoystickPlugin};

use crate::event::MoveInputState;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app
            .add_plugins(JoystickPlugin)
            .add_systems(Startup, Self::setup)
            .add_systems(Update, on_joystick_event);
    }
}

#[derive(Clone)]
struct BackgroundColorProperty;

#[derive(Component)]
struct JoystickFadeAnimatePlayer {
    fade_in_index: AnimationNodeIndex,
    fade_out_index: AnimationNodeIndex,
}

#[derive(Component)]
struct MoveControl;

#[derive(Component)]
struct RotateControl;

impl UiPlugin {
    fn setup(
        mut commands: Commands,
        mut animation_graphs: ResMut<Assets<AnimationGraph>>,
        mut animation_clips: ResMut<Assets<AnimationClip>>,
    ) {

        commands.spawn((
            Node{
                width: Val::Vw(100.0),
                height: Val::Vh(100.0),
                left: Val::Vw(0.0),
                ..Default::default()
            },
            // BackgroundColor(Color::hsla(0.0, 0.6, 0.8, 0.5)),
            Interaction::default(),
            RotateControl,
        ));

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
                AnimatableKeyframeCurve::new(
                    [0.0, 1.0, 1.5].into_iter().zip([
                        Srgba::from(joystick_active_color),
                        Srgba::from(joystick_active_color),
                        Srgba::from(joystick_idle_color),
                    ])
                )
                .unwrap(),
            ),
        );
        
        // 创建淡入动画
        let mut fade_in_animation_clip = AnimationClip::default();
        fade_in_animation_clip.add_curve_to_target(
            animation_target_id,
            AnimatableCurve::new(
                BackgroundColorProperty,
                AnimatableKeyframeCurve::new(
                    [0.0, 0.2].into_iter().zip([
                        Srgba::from(joystick_idle_color),
                        Srgba::from(joystick_active_color),
                    ])
                )
                .unwrap(),
            ),
        );

        // 存储动画句柄
        let fade_out_animation_clip_handle = animation_clips.add(fade_out_animation_clip);
        let fade_in_animation_clip_handle = animation_clips.add(fade_in_animation_clip);

        // 创建包含两个动画的动画图
        let mut animation_graph = AnimationGraph::default();
        let fade_in_index = animation_graph.add_clip(fade_in_animation_clip_handle, 1.0, animation_graph.root);
        let fade_out_index = animation_graph.add_clip(fade_out_animation_clip_handle, 1.0, animation_graph.root);
        let animation_graph_handle = animation_graphs.add(animation_graph);

        let animation_player = AnimationPlayer::default();
        // 初始不播放任何动画（或根据需要选择默认动画）

        let animation_player_entity = commands.spawn((
            animation_player,
            AnimationGraphHandle(animation_graph_handle),
            JoystickFadeAnimatePlayer {
                fade_in_index,
                fade_out_index,
            },
        )).id();

        commands.spawn((
            Node{
                width: Val::Vw(100.0),
                height: Val::Vh(100.0),
                padding: UiRect::all(Val::Vw(10.0)),
                display: Display::Flex,
                justify_content: JustifyContent::FlexStart,
                align_items: AlignItems::FlexEnd,
                ..Default::default()
            },
            children![
                (
                    Node{
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
                    MoveControl,
                )
            ]
        ));

    }
}

fn move_control_activate(
    commands: &mut Commands,
    joystick_fade_animate_player_components: Option<(Mut<'_, AnimationPlayer>, &JoystickFadeAnimatePlayer)>,
) {
    if let Some((mut animation_player, joystick_fade_animate_player)) = joystick_fade_animate_player_components {
        animation_player.stop_all();
        // 播放淡入动画
        animation_player.play(joystick_fade_animate_player.fade_in_index);
    }

    commands.insert_resource(MoveInputState(Vec2::ZERO, 0.0));
}

fn move_control_deactivate(
    commands: &mut Commands,
    joystick_fade_animate_player_components: Option<(Mut<'_, AnimationPlayer>, &JoystickFadeAnimatePlayer)>,
) {
    if let Some((mut animation_player, joystick_fade_animate_player)) = joystick_fade_animate_player_components {
        animation_player.stop_all();
        // 播放淡出动画
        animation_player.play(joystick_fade_animate_player.fade_out_index);
    }
    commands.remove_resource::<MoveInputState>();
}

fn move_control_update(
    commands: &mut Commands,
    mut input_state: Option<&mut ResMut<MoveInputState>>,
    direction: &Vec2,
    force: &f32,
) {
    if let Some(input_state) = input_state.as_mut() {
        input_state.0 = *direction;
        input_state.1 = *force;
    } else {
        commands.insert_resource(MoveInputState(*direction, *force));
    }
}

fn on_joystick_event(
    mut commands: Commands,
    mut event_reader: MessageReader<JoystickEvent>,
    mut joystick_fade_animate_player_query: Query<(&mut AnimationPlayer, &JoystickFadeAnimatePlayer)>,
    move_control_query: Query<Entity, With<MoveControl>>,
    mut input_state: Option<ResMut<MoveInputState>>,
) {
    for event in event_reader.read() {
        match event {
            JoystickEvent::Activate(entity) => {
                println!("Joystick activated: {:?}", entity);
                if let Ok(_) = move_control_query.get(*entity) {
                    move_control_activate(&mut commands, joystick_fade_animate_player_query.iter_mut().next());
                }
            }
            JoystickEvent::Changed(entity, direction, force) => {
                println!("Joystick state changed: {:?}", direction);

                if let Ok(_) = move_control_query.get(*entity) {
                    move_control_update(&mut commands, input_state.as_mut(), &direction, &force);
                }
            }
            JoystickEvent::ThumbReset(entity) => {
                println!("Joystick thumb reset: {:?}", entity);
                if let Ok(_) = move_control_query.get(*entity) {
                    move_control_deactivate(&mut commands, joystick_fade_animate_player_query.iter_mut().next());
                }
            }
            _ => {}
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

        if let Color::Srgba(_) = background_color.0 {} else {
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