use std::{ops::DerefMut, time::Duration};

use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

use crate::camera::GameCamera;
use crate::input::{ControlInputPlugin, LookAxis, LookController, MovementController};
use crate::{GameAssets, GameState};
use crate::root_motion::{RootMotionPlugin, setup_root_motion_for_character, RootBone};

pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ControlInputPlugin)
            .add_plugins((
                RapierPhysicsPlugin::<NoUserData>::default(),
                RapierDebugRenderPlugin::default(),
            ))
            .add_plugins(RootMotionPlugin)
            .add_systems(OnEnter(GameState::Game), Self::setup)
            .add_systems(
                Update,
                setup_scene_once_loaded.run_if(in_state(GameState::Game)),
            )
            .add_systems(
                Update,
                animation_controller.run_if(in_state(GameState::Game)),
            );
    }
}
#[derive(Resource)]
struct Animations {
    animations: Vec<AnimationNodeIndex>,
    graph_handle: Handle<AnimationGraph>,
}

impl ScenePlugin {
    fn setup(
        mut commands: Commands,
        game_assets: Res<GameAssets>,
        mut meshes: ResMut<Assets<Mesh>>,
        mut materials: ResMut<Assets<StandardMaterial>>,
        mut graphs: ResMut<Assets<AnimationGraph>>,
        game_camera: Res<GameCamera>,
    ) {
        let (graph, node_indices) = AnimationGraph::from_clips([
            game_assets.silly_dancing.clone(),
            game_assets.runba_dancing.clone(),
            game_assets.idle.clone(),
            game_assets.jogging.clone(),
        ]);

        let graph_handle = graphs.add(graph);
        commands.insert_resource(Animations {
            animations: node_indices,
            graph_handle,
        });

        commands.spawn((
            Mesh3d(meshes.add(Circle::new(4.0))),
            MeshMaterial3d(materials.add(Color::WHITE)),
            Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
        ));

        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
            MeshMaterial3d(materials.add(Color::srgb_u8(124, 144, 255))),
            Transform::from_xyz(0.0, 0.5, 0.0),
            Collider::cuboid(0.5, 0.5, 0.5),
            ColliderDebugColor(Hsla::gray(0.6)),
            RigidBody::Fixed,
        ));

        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
            MeshMaterial3d(materials.add(Color::srgb_u8(124, 144, 255))),
            Transform::from_xyz(1.0, 0.5, 1.0),
            Collider::cuboid(0.5, 0.5, 0.5),
            ColliderDebugColor(Hsla::gray(0.6)),
            RigidBody::Fixed,
        ));

        commands.spawn((
            PointLight {
                shadows_enabled: true,
                ..default()
            },
            Transform::from_xyz(4.0, 8.0, 4.0),
        ));

        let ground_size = 30.0;
        let ground_height = 0.1;
        commands.spawn((
            Transform::from_xyz(0.0, -ground_height / 2.0, 0.0),
            Collider::cuboid(ground_size, ground_height, ground_size),
            ColliderDebugColor(Hsla::BLACK),
        ));

        let player_entity = commands
            .spawn((
                Transform::from_xyz(0.0, 2.0, 0.0),
                RigidBody::Dynamic,
                Collider::capsule_y(0.5, 0.2),
                ColliderDebugColor(Hsla::WHITE),
                Restitution::coefficient(0.3),
                Damping {
                    linear_damping: 0.3,
                    angular_damping: 0.3,
                },
                // Ccd::enabled(),  
                LockedAxes::ROTATION_LOCKED,
                Velocity::zero(),
                MovementController::default(),
                LookController {
                    axis: LookAxis::Yaw,
                    ..Default::default()
                },
                InheritedVisibility::default(),
                Visibility::Visible,
            ))
            .with_children(|parent| {
                parent.spawn((
                    SceneRoot(game_assets.amy_model.clone()),
                    Transform {
                        translation: Vec3::new(0.0, -0.7, 0.0),
                        ..Default::default()
                    },
                ));

            }).id();

        commands.entity(game_camera.0).insert((
            ChildOf(player_entity),
            Transform::from_xyz(0.0, 1.3, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
            LookController {
                axis: LookAxis::Pitch,
                ..Default::default()
            },
        ));
    }
}

fn setup_scene_once_loaded(
    mut commands: Commands,
    animations: Res<Animations>,
    mut players: Query<(Entity, &mut AnimationPlayer), Added<AnimationPlayer>>,
) {
    for (entity, mut player) in &mut players {
        let mut transitions = AnimationTransitions::new();

        transitions
            .play(&mut player, animations.animations[0], Duration::ZERO)
            .repeat();

        commands
            .entity(entity)
            .insert(AnimationGraphHandle(animations.graph_handle.clone()))
            .insert(transitions);

        info!("Added animation graph to entity {:?}", entity);
    }
}

fn animation_controller(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    animations: Res<Animations>,
    mut animation_players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
) {
    if keyboard_input.just_pressed(KeyCode::Digit1) {
        info!("Switching to animation 1: Silly Dancing");
        switch_animation(&mut animation_players, animations.animations[0]);
    } else if keyboard_input.just_pressed(KeyCode::Digit2) {
        info!("Switching to animation 2: Runba Dancing");
        switch_animation(&mut animation_players, animations.animations[1]);
    } else if keyboard_input.just_pressed(KeyCode::Digit3) {
        info!("Switching to animation 3: Idle");
        switch_animation(&mut animation_players, animations.animations[2]);
    } else if keyboard_input.just_pressed(KeyCode::Digit4) {
        info!("Switching to animation 4: Jogging");
        switch_animation(&mut animation_players, animations.animations[3]);
    }
}

fn switch_animation(
    animation_players: &mut Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
    animation_index: AnimationNodeIndex,
) {
    for (mut animation_player, mut transitions) in animation_players.iter_mut() {
        transitions
            .play(
                animation_player.deref_mut(),
                animation_index,
                Duration::ZERO,
            )
            .repeat();
    }
}