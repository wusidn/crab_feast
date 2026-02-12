use std::{ops::DerefMut, time::Duration};

use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

use crate::input::{ControlInputPlugin, LookAxis, LookController, MovementController};
pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app
        .add_plugins(ControlInputPlugin)
        .add_plugins((
            RapierPhysicsPlugin::<NoUserData>::default(),
            RapierDebugRenderPlugin::default(),
        ))
        .add_systems(Update, setup_scene_once_loaded)
        .add_systems(Update, animation_controller)
        .add_systems(Startup, Self::setup);
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
        asset_server: Res<AssetServer>,
        mut meshes: ResMut<Assets<Mesh>>,
        mut materials: ResMut<Assets<StandardMaterial>>,
        mut graphs: ResMut<Assets<AnimationGraph>>,
    ) {

        // Build the animation graph
        let (graph, node_indices) = AnimationGraph::from_clips([
            asset_server.load(GltfAssetLabel::Animation(0).from_asset("characters/animations/Amy/SillyDancing.glb")),
            asset_server.load(GltfAssetLabel::Animation(0).from_asset("characters/animations/Amy/RunbaDancing.glb")),
            asset_server.load(GltfAssetLabel::Animation(0).from_asset("characters/animations/Amy/Idle.glb")),
            asset_server.load(GltfAssetLabel::Animation(0).from_asset("characters/animations/Amy/Jogging.glb")),
        ]);

        // Keep our animation graph in a Resource so that it can be inserted onto
        // the correct entity once the scene actually loads.
        let graph_handle = graphs.add(graph);
        commands.insert_resource(Animations {
            animations: node_indices,
            graph_handle,
        });

        // circular base
        commands.spawn((
            Mesh3d(meshes.add(Circle::new(4.0))),
            MeshMaterial3d(materials.add(Color::WHITE)),
            Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
        ));
        // cube
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
            MeshMaterial3d(materials.add(Color::srgb_u8(124, 144, 255))),
            Transform::from_xyz(0.0, 0.5, 0.0),
        ));
        // light
        commands.spawn((
            PointLight {
                shadows_enabled: true,
                ..default()
            },
            Transform::from_xyz(4.0, 8.0, 4.0),
        ));

        // Base
        let ground_size = 30.0;
        let ground_height = 0.1;
        commands.spawn((
            Transform::from_xyz(0.0, -ground_height / 2.0, 0.0),
            Collider::cuboid(ground_size, ground_height, ground_size),
            ColliderDebugColor(Hsla::BLACK),
        ));

        // Player
        commands.spawn((
            Transform::from_xyz(0.0, 1.0, 0.0),
            RigidBody::Dynamic, // 添加动态刚体组件使对象受重力影响
            Collider::cuboid(0.5, 1.0, 0.5),
            ColliderDebugColor(Hsla::WHITE),
            MovementController::default(),
            LookController{
                axis: LookAxis::Yaw,
                ..Default::default()
            },
        )).with_children(|parent| {

            parent.spawn((
                SceneRoot(
                    asset_server.load(GltfAssetLabel::Scene(0).from_asset("characters/rigs/Amy.glb")),
                ),
                Transform{
                    translation: Vec3::new(0.0, -1.0, 0.0),
                    // rotation: Quat::from_rotation_y(std::f32::consts::PI),
                    ..Default::default()
                },
            ));

            // camera
            parent.spawn((
                Camera3d::default(),
                Transform::from_xyz(0.0, 1.3, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
                LookController {
                    axis: LookAxis::Pitch,
                    ..Default::default()
                },
            ));
        });
    }
}

fn setup_scene_once_loaded(
    mut commands: Commands,
    animations: Res<Animations>,
    mut players: Query<(Entity, &mut AnimationPlayer), Added<AnimationPlayer>>,
) {
    for (entity, mut player) in &mut players {
        let mut transitions = AnimationTransitions::new();

        // Make sure to start the animation via the `AnimationTransitions`
        // component. The `AnimationTransitions` component wants to manage all
        // the animations and will get confused if the animations are started
        // directly via the `AnimationPlayer`.
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
    // 检查数字键1、2、3是否被按下
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
        transitions.play(animation_player.deref_mut(), animation_index, Duration::ZERO).repeat();
    }
}