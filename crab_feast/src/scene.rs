use std::{ops::DerefMut, time::Duration};

use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

use crate::camera::GameCamera;
use crate::input::{ControlInputPlugin, LookAxis, LookController, MovementController};
use crate::{GameAssets, GameState};

pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ControlInputPlugin)
            .add_plugins((
                RapierPhysicsPlugin::<NoUserData>::default(),
                RapierDebugRenderPlugin::default(),
            ))
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
        mut graphs: ResMut<Assets<AnimationGraph>>,
        game_camera: Res<GameCamera>,
    ) {
        let (graph, node_indices) = AnimationGraph::from_clips([
            game_assets.idle.clone(),
        ]);

        let graph_handle = graphs.add(graph);
        commands.insert_resource(Animations {
            animations: node_indices,
            graph_handle,
        });

        let ground_size = 30.0;
        let ground_height = 0.1;

        // 用可复用的 scene 函数组合整个场景（纯视觉 + 带物理均可进 BSN）
        commands.spawn_scene_list(bsn_list! {
            Mesh3d(asset_value(Circle::new(4.0)))
            MeshMaterial3d<StandardMaterial>(asset_value(Color::WHITE))
            Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
            --
            @cube(Vec3::new(0.0, 0.5, 0.0))
            --
            @cube(Vec3::new(1.0, 0.5, 1.0))
            --
            PointLight {
                shadow_maps_enabled: true,
            }
            Transform::from_xyz(4.0, 8.0, 4.0)
            --
            @ground(ground_size, ground_height)
        });

        let player_entity = commands
            .spawn((
                Transform::from_xyz(0.0, 2.0, 0.0),
                RigidBody::Dynamic,
                Collider::capsule_y(0.5, 0.2),
                ColliderDebugColor(Hsla::hsl(0.0, 0.0, 1.0)),
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
                    WorldAssetRoot(game_assets.amy_model.clone()),
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

/// 一个带物理的立方体：视觉 + 碰撞体 + 固定刚体
fn cube(translation: Vec3) -> impl Scene {
    let debug_color = ColliderDebugColor(Hsla::hsl(0.0, 0.0, 0.6));
    bsn! {
        Mesh3d(asset_value(Cuboid::new(1.0, 1.0, 1.0)))
        MeshMaterial3d<StandardMaterial>(asset_value(Color::srgb_u8(124, 144, 255)))
        Transform::from_translation(translation)
        ~Collider::cuboid(0.5, 0.5, 0.5)
        debug_color
        ~RigidBody::Fixed
    }
}

/// 地面：只有碰撞体，无渲染 mesh
fn ground(size: f32, height: f32) -> impl Scene {
    let debug_color = ColliderDebugColor(Hsla::hsl(0.0, 0.0, 0.0));
    bsn! {
        Transform::from_xyz(0.0, -height / 2.0, 0.0)
        ~Collider::cuboid(size, height, size)
        debug_color
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
        info!("Switching to animation 1: RIG-rainAction");
        switch_animation(&mut animation_players, animations.animations[0]);
    }
    // else if keyboard_input.just_pressed(KeyCode::Digit2) {
    //     info!("Switching to animation 2: Runba Dancing");
    //     switch_animation(&mut animation_players, animations.animations[1]);
    // } else if keyboard_input.just_pressed(KeyCode::Digit3) {
    //     info!("Switching to animation 3: Idle");
    //     switch_animation(&mut animation_players, animations.animations[2]);
    // } else if keyboard_input.just_pressed(KeyCode::Digit4) {
    //     info!("Switching to animation 4: Jogging");
    //     switch_animation(&mut animation_players, animations.animations[3]);
    // } else if keyboard_input.just_pressed(KeyCode::Digit5) {
    //     info!("Switching to animation 5: Jogging");
    //     switch_animation(&mut animation_players, animations.animatons[4]);
    // }
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