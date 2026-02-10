use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

use crate::movement::{MovementInputPlugin, Movement};
pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app
        .add_plugins(MovementInputPlugin)
        .add_plugins((
            RapierPhysicsPlugin::<NoUserData>::default(),
            RapierDebugRenderPlugin::default(),
        ))
        .add_systems(Startup, Self::setup);
    }
}

impl ScenePlugin {
    fn setup(
        mut commands: Commands,
        mut meshes: ResMut<Assets<Mesh>>,
        mut materials: ResMut<Assets<StandardMaterial>>,
    ) {

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
        // camera
        commands.spawn((
            Camera3d::default(),
            Transform::from_xyz(0.0, 6.0, 3.0).looking_at(Vec3::ZERO, Vec3::Y),
            Movement,
        ));

        // Base
        let ground_size = 3.1;
        let ground_height = 0.1;
        commands.spawn((
            Transform::from_xyz(0.0, -ground_height / 2.0, 0.0),
            Collider::cuboid(ground_size, ground_height, ground_size),
            ColliderDebugColor(Hsla::BLACK),
        ));
    }
}

