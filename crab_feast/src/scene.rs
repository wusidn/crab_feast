use std::time::Duration;

use bevy::app::AnimationSystems;
use bevy::animation::RepeatAnimation;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

use crate::camera::GameCamera;
use crate::input::{
    CharacterBodyYaw, ControlInputPlugin, LookAxis, LookController, MovementController, MovementInput,
    PlayerCharacterModelRoot,
};
use crate::locomotion::{locomotion_anim_from_speed_and_force, LocomotionAnim, LocomotionInput};
use crate::root_motion::{
    process_root_motion_rebase_requests, wire_mixamo_hips_for_root_compensation,
    CharacterRootMotionLink, RootMotionPlugin, RootMotionRebaseRequest,
};
use crate::{GameAssets, GameState};

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
                try_wire_amy_hips_root_motion.run_if(in_state(GameState::Game)),
            )
            .add_systems(
                Update,
                debug_animation_hotkeys.run_if(in_state(GameState::Game)),
            )
            .add_systems(
                PostUpdate,
                (
                    update_amy_locomotion_animation,
                    process_root_motion_rebase_requests,
                )
                    .chain()
                    .before(AnimationSystems)
                    .run_if(in_state(GameState::Game)),
            );
    }
}

/// Order: idle, walk, run, jog, silly, runba — used for locomotion (first four).
#[derive(Resource)]
struct AmyAnimationGraph {
    graph_handle: Handle<AnimationGraph>,
    idle: AnimationNodeIndex,
    walk: AnimationNodeIndex,
    run: AnimationNodeIndex,
    jog: AnimationNodeIndex,
    silly: AnimationNodeIndex,
    runba: AnimationNodeIndex,
}

#[derive(Resource)]
struct AmyPlayerBinding {
    body: Entity,
    anim_player: Option<Entity>,
    hips_wired: bool,
}

#[derive(Component, Clone, Copy, PartialEq, Eq)]
struct LastLocomotion(LocomotionAnim);

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
            game_assets.idle.clone(),
            game_assets.walking.clone(),
            game_assets.running.clone(),
            game_assets.jogging.clone(),
            game_assets.silly_dancing.clone(),
            game_assets.runba_dancing.clone(),
        ]);

        let graph_handle = graphs.add(graph);
        commands.insert_resource(AmyAnimationGraph {
            graph_handle,
            idle: node_indices[0],
            walk: node_indices[1],
            run: node_indices[2],
            jog: node_indices[3],
            silly: node_indices[4],
            runba: node_indices[5],
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
                CharacterBodyYaw::default(),
                RigidBody::Dynamic,
                Collider::capsule_y(0.5, 0.2),
                ColliderDebugColor(Hsla::WHITE),
                Restitution::coefficient(0.3),
                Damping {
                    linear_damping: 0.3,
                    angular_damping: 0.3,
                },
                LockedAxes::ROTATION_LOCKED,
                Velocity::zero(),
                MovementController::default(),
                InheritedVisibility::default(),
                Visibility::Visible,
            ))
            .with_children(|parent| {
                parent
                    .spawn((
                        Name::new("PlayerCharacterModelRoot"),
                        PlayerCharacterModelRoot,
                        Transform::from_translation(Vec3::new(0.0, -0.7, 0.0)),
                    ))
                    .with_children(|p2| {
                        p2.spawn((SceneRoot(game_assets.amy_model.clone()), Transform::default()));
                    });
            })
            .id();

        commands.insert_resource(AmyPlayerBinding {
            body: player_entity,
            anim_player: None,
            hips_wired: false,
        });

        commands.entity(game_camera.0).insert((
            Transform::from_xyz(0.0, 1.3, 5.0).looking_at(Vec3::new(0.0, 2.0, 0.0), Vec3::Y),
            LookController {
                axis: LookAxis::YawAndPitch,
                ..Default::default()
            },
        ));
    }
}

const HIPS_BONE: &str = "mixamorig:Hips";

fn is_descendant_of(
    mut current: Entity,
    child_of: &Query<&ChildOf>,
    ancestor: Entity,
) -> bool {
    for _ in 0..256 {
        if current == ancestor {
            return true;
        }
        if let Ok(c) = child_of.get(current) {
            current = c.0;
        } else {
            return false;
        }
    }
    false
}

fn find_descendant_by_name(
    start: Entity,
    children: &Query<&Children>,
    names: &Query<&Name>,
    target: &str,
) -> Option<Entity> {
    let mut stack = vec![start];
    while let Some(e) = stack.pop() {
        if let Ok(n) = names.get(e) {
            if n.as_str() == target {
                return Some(e);
            }
        }
        if let Ok(kids) = children.get(e) {
            for c in kids.iter() {
                stack.push(c);
            }
        }
    }
    None
}

fn setup_scene_once_loaded(
    mut commands: Commands,
    anims: Res<AmyAnimationGraph>,
    mut binding: ResMut<AmyPlayerBinding>,
    child_of: Query<&ChildOf>,
    mut players: Query<(Entity, &mut AnimationPlayer), Added<AnimationPlayer>>,
) {
    for (entity, mut player) in &mut players {
        if !is_descendant_of(entity, &child_of, binding.body) {
            continue;
        }
        if binding.anim_player.is_some() {
            continue;
        }
        let mut transitions = AnimationTransitions::new();
        transitions
            .play(&mut player, anims.idle, Duration::ZERO)
            .set_repeat(RepeatAnimation::Forever);

        commands
            .entity(entity)
            .insert(AnimationGraphHandle(anims.graph_handle.clone()))
            .insert(transitions)
            .insert(LastLocomotion(LocomotionAnim::Idle));

        binding.anim_player = Some(entity);
    }
}

fn try_wire_amy_hips_root_motion(
    mut commands: Commands,
    _anims: Res<AmyAnimationGraph>,
    mut binding: ResMut<AmyPlayerBinding>,
    children: Query<&Children>,
    child_of: Query<&ChildOf>,
    names: Query<&Name>,
) {
    if binding.hips_wired {
        return;
    }
    let Some(anim_e) = binding.anim_player else {
        return;
    };

    let Some(hips) = find_descendant_by_name(binding.body, &children, &names, HIPS_BONE) else {
        return;
    };
    let Ok(parent) = child_of.get(hips) else {
        return;
    };
    let parent = parent.0;

    wire_mixamo_hips_for_root_compensation(&mut commands, parent, hips);

    commands
        .entity(anim_e)
        .insert(CharacterRootMotionLink { hips_entity: hips });
    binding.hips_wired = true;
}

const FADE: Duration = Duration::from_millis(200);
const LOCO_IDLE_MAX: f32 = 0.2;
const LOCO_RUN_MIN: f32 = 4.0;
const LOCO_RUN_FORCE: f32 = 0.9;

/// Pauses automatic walk/run/idle for a few seconds after number-key debug switches.
#[derive(Component)]
struct LocomotionDebugSuppress {
    end_secs: f32,
}

fn update_amy_locomotion_animation(
    mut commands: Commands,
    anims: Res<AmyAnimationGraph>,
    binding: Res<AmyPlayerBinding>,
    input: Res<MovementInput>,
    time: Res<Time>,
    bodies: Query<&Velocity>,
    mut anim_state: Query<(
        Entity,
        &mut AnimationPlayer,
        &mut AnimationTransitions,
        &mut LastLocomotion,
        Option<&LocomotionDebugSuppress>,
    )>,
) {
    let Some(anim_e) = binding.anim_player else {
        return;
    };
    let Ok(vel) = bodies.get(binding.body) else {
        return;
    };
    let Ok((anim_entity, mut ap, mut tr, mut last, debug_hold)) = anim_state.get_mut(anim_e) else {
        return;
    };
    if let Some(s) = debug_hold {
        if time.elapsed_secs() < s.end_secs {
            return;
        }
    }

    let horizontal = (vel.linvel * Vec3::new(1.0, 0.0, 1.0)).length();
    let loco_in = match input.as_ref() {
        MovementInput::Idle => LocomotionInput {
            speed_horizontal: horizontal,
            move_force: None,
        },
        MovementInput::Activated { direction, force } if direction.length() > 0.01 => {
            LocomotionInput {
                speed_horizontal: horizontal,
                move_force: Some(*force),
            }
        }
        _ => LocomotionInput {
            speed_horizontal: horizontal,
            move_force: None,
        },
    };

    let desired = locomotion_anim_from_speed_and_force(
        loco_in,
        LOCO_IDLE_MAX,
        LOCO_RUN_MIN,
        LOCO_RUN_FORCE,
    );

    if last.0 == desired {
        return;
    }
    last.0 = desired;

    let node = match desired {
        LocomotionAnim::Idle => anims.idle,
        LocomotionAnim::Walk => anims.walk,
        LocomotionAnim::Run => anims.run,
    };
    tr.play(&mut ap, node, FADE)
        .set_repeat(RepeatAnimation::Forever);
    commands
        .entity(anim_entity)
        .insert(RootMotionRebaseRequest);
}

fn debug_animation_hotkeys(
    time: Res<Time>,
    keyboard: Res<ButtonInput<KeyCode>>,
    anims: Res<AmyAnimationGraph>,
    mut q: Query<(
        Entity,
        &mut AnimationPlayer,
        &mut AnimationTransitions,
    ), With<LastLocomotion>>,
    mut commands: Commands,
) {
    let any = [
        (KeyCode::Digit1, anims.silly),
        (KeyCode::Digit2, anims.runba),
        (KeyCode::Digit3, anims.jog),
        (KeyCode::Digit4, anims.run),
    ]
    .into_iter()
    .find(|(k, _)| keyboard.just_pressed(*k));
    if let Some((_, node)) = any {
        for (e, mut p, mut t) in &mut q {
            t.play(&mut p, node, FADE)
                .set_repeat(RepeatAnimation::Forever);
            commands.entity(e).insert((
                RootMotionRebaseRequest,
                LocomotionDebugSuppress {
                    end_secs: time.elapsed_secs() + 2.0,
                },
            ));
        }
    }
}
