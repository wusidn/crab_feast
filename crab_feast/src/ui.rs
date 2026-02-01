use std::any::TypeId;

use bevy::{animation::{AnimatedBy, AnimationEntityMut, AnimationEvaluationError, AnimationTargetId}, prelude::*};
use crab_feast_ui_joysticks::{Joystick, JoystickEvent, JoystickPlugin};

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

impl UiPlugin {
    fn setup(
        mut commands: Commands,
        mut animation_graphs: ResMut<Assets<AnimationGraph>>,
        mut animation_clips: ResMut<Assets<AnimationClip>>,
    ) {

        let animation_target_name = Name::new("joystick_background");
        let animation_target_id = AnimationTargetId::from_name(&animation_target_name);
        // 创建淡入动画
        let mut animation_clip = AnimationClip::default();
        animation_clip.add_curve_to_target(
            animation_target_id,
            AnimatableCurve::new(
                BackgroundColorProperty,
                AnimatableKeyframeCurve::new(
                    [0.0, 0.3].into_iter().zip([
                        Srgba::from(Color::hsla(160.0, 0.5, 0.8, 0.0)),
                        Srgba::from(Color::hsla(160.0, 0.5, 0.8, 0.5)),
                    ])
                )
                .unwrap(),
            ),
        );

        // 存储动画句柄
        let animation_clip_handle = animation_clips.add(animation_clip);

              // Create an animation graph with that clip.
        let (animation_graph, animation_node_index) =
            AnimationGraph::from_clip(animation_clip_handle);
        let animation_graph_handle = animation_graphs.add(animation_graph);

        let mut animation_player = AnimationPlayer::default();
        animation_player.play(animation_node_index).repeat();

        let animation_player_entity = commands.spawn((
            Node{
                width: Val::Vw(100.0),
                height: Val::Vh(100.0),
                padding: UiRect::all(Val::Vw(10.0)),
                display: Display::Flex,
                justify_content: JustifyContent::FlexStart,
                align_items: AlignItems::FlexEnd,
                ..Default::default()
            },
            animation_player,
            AnimationGraphHandle(animation_graph_handle),
            children![
                (
                    Node{
                        width: Val::Vw(10.0),
                        height: Val::Vw(10.0),
                        display: Display::Flex,
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        border_radius: BorderRadius::all(Val::Percent(50.0)),
                        ..Default::default()
                    },
                    BackgroundColor(Color::srgba(0.6, 0.5, 0.8, 0.)),
                    Joystick {
                        ..Default::default()
                    },
                    
                )
            ]
        )).id();

        commands.spawn((
            Node{
                width: Val::Vw(10.0),
                height: Val::Vw(10.0),
                display: Display::Flex,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border_radius: BorderRadius::all(Val::Percent(50.0)),
                ..Default::default()
            },
            BackgroundColor(Color::srgba(0.6, 0.5, 0.8, 0.)),
            AnimatedBy(animation_player_entity),
            animation_target_id,
            animation_target_name
        ));

    }
}

fn on_joystick_event(
    mut event_reader: MessageReader<JoystickEvent>,
    mut background_color_query: Query<&mut BackgroundColor>,
) {
    for event in event_reader.read() {
        match event {
            JoystickEvent::Activate(entity) => {
                println!("Joystick activated: {:?}", entity);
                if let Ok(mut background_color) = background_color_query.get_mut(*entity) {
                    background_color.0.set_alpha(0.5);
                }
            }
            JoystickEvent::Changed(state) => {
                println!("Joystick state changed: {:?}", state.direction);
            }
            JoystickEvent::ThumbReset(entity) => {
                println!("Joystick thumb reset: {:?}", entity);
                if let Ok(mut background_color) = background_color_query.get_mut(*entity) {
                    background_color.0.set_alpha(0.0);
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
        let text_color = entity
            .get_mut::<BackgroundColor>()
            .ok_or(AnimationEvaluationError::ComponentNotPresent(TypeId::of::<
                BackgroundColor,
            >(
            )))?
            .into_inner();
        match text_color.0 {
            Color::Srgba(ref mut color) => Ok(color),
            _ => Err(AnimationEvaluationError::PropertyNotPresent(TypeId::of::<
                Srgba,
            >(
            ))),
        }
    }
}
