use bevy::app::AnimationSystems;
use bevy::prelude::*;
use bevy::transform::TransformSystems;
use bevy_rapier3d::prelude::PhysicsSet;

/// Visual-only root offset for the character bone: parent applies `lock * inverse(current)`.
#[derive(Component)]
pub struct RootMotion {
    /// Chain product `MotionIntermediate * Hips` from the previous frame (`lock` in `int = lock * inv(hip)`).
    pub initial_hips_local: Option<Transform>,
    /// End-of-previous-frame Hips local pose; used to seam `MotionIntermediate` when rebase or init runs.
    pub prev_hip_local: Option<Transform>,
}

impl Default for RootMotion {
    fn default() -> Self {
        Self {
            initial_hips_local: None,
            prev_hip_local: None,
        }
    }
}

/// Marks the extra transform inserted between a bone and its parent to cancel in-place root motion.
#[derive(Component)]
pub struct MotionIntermediateLayer {
    pub root_bone_entity: Entity,
}

#[derive(Component)]
pub struct RootBone;

/// Links the main character [`AnimationPlayer`] to the root bone for compensation + rebase.
#[derive(Component)]
pub struct CharacterRootMotionLink {
    pub hips_entity: Entity,
}

/// Request a single [`RootMotion::initial_hips_local`] clear before the next
/// `AnimationSystems` run (e.g. locomotion graph node change). Insert on the
/// entity that has [`CharacterRootMotionLink`].
#[derive(Component)]
pub struct RootMotionRebaseRequest;

pub struct RootMotionPlugin;

impl Plugin for RootMotionPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            apply_root_compensation_to_intermediate
                .after(AnimationSystems)
                // Physics writes the capsule [`Transform`]; run compensation after
                // so the skeleton/layer chain is consistent for this frame.
                .after(PhysicsSet::Writeback)
                .before(TransformSystems::Propagate),
        );
    }
}

/// Inserts a [`MotionIntermediateLayer`] between the current parent and the hip bone.
pub fn wire_mixamo_hips_for_root_compensation(
    commands: &mut Commands,
    parent_of_hips: Entity,
    hips: Entity,
) {
    commands
        .entity(hips)
        .insert((
            RootBone,
            RootMotion::default(),
        ));

    let intermediate = commands
        .spawn((
            Name::new("MotionIntermediateLayer"),
            MotionIntermediateLayer {
                root_bone_entity: hips,
            },
            Transform::IDENTITY,
            GlobalTransform::IDENTITY,
            ChildOf(parent_of_hips),
        ))
        .id();

    commands.entity(hips).insert(ChildOf(intermediate));
}

/// Clears the locked Hips product so the next `apply_root_compensation_to_intermediate`
/// run will seam from [`RootMotion::prev_hip_local`]. Removes the request component.
pub fn process_root_motion_rebase_requests(
    mut commands: Commands,
    q: Query<(Entity, &CharacterRootMotionLink), With<RootMotionRebaseRequest>>,
    mut root_motions: Query<&mut RootMotion, With<RootBone>>,
) {
    for (e, link) in &q {
        if let Ok(mut rm) = root_motions.get_mut(link.hips_entity) {
            rm.initial_hips_local = None;
        }
        commands.entity(e).remove::<RootMotionRebaseRequest>();
    }
}

fn apply_root_compensation_to_intermediate(
    mut layers: Query<
        (&MotionIntermediateLayer, &mut Transform),
        Without<RootBone>,
    >,
    mut q_hips: Query<
        (&mut RootMotion, &Transform),
        (With<RootBone>, Without<MotionIntermediateLayer>),
    >,
) {
    for (layer, mut int_xform) in &mut layers {
        let Ok((mut rm, hip_xform)) = q_hips.get_mut(layer.root_bone_entity) else {
            continue;
        };
        if rm.initial_hips_local.is_none() {
            // Rebase / first init: do not throw away previous int — seam so world(hips) is continuous
            // (otherwise cross-fade to idle makes the model snap onto the physics root).
            if let Some(prev) = rm.prev_hip_local {
                let m = int_xform
                    .to_matrix()
                    * prev.to_matrix()
                    * (*hip_xform).to_matrix().inverse();
                *int_xform = Transform::from_matrix(m);
            } else {
                *int_xform = Transform::IDENTITY;
            }
        } else if let Some(t0) = rm.initial_hips_local {
            // Lock is the chain product (intermediate * hip) from the previous frame.
            let m = t0.to_matrix() * (*hip_xform).to_matrix().inverse();
            *int_xform = Transform::from_matrix(m);
        }
        // Store int * hip = lock so `int = lock * inv(hip)` stays valid across blends and
        // re-seams (the previous `Some(hip)`-only value broke seam: lock must be the product).
        rm.initial_hips_local = Some(int_xform.mul_transform(*hip_xform));
        rm.prev_hip_local = Some(*hip_xform);
    }
}
