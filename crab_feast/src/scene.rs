use bevy::prelude::*;

use crate::event::InputState;

pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, Self::setup)
        .add_systems(Update, camera_react_to_input.run_if(resource_exists::<InputState>));
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
            Transform::from_xyz(-2.5, 4.5, 3.0).looking_at(Vec3::ZERO, Vec3::Y),
        ));
    }
}

fn camera_react_to_input(
    mut camera: Query<(&mut Transform, &mut Camera3d)>,
    input: Res<InputState>,
    time: Res<Time>,
) {

    let move_speed = 3.;
    if let Some((dir, force)) = input.move_direction {
        camera.iter_mut().for_each(|(mut transform, _)| {
            let local_move_direction = Vec3::new(dir.x, 0.0, dir.y);
            let world_move_direction = transform.rotation.mul_vec3(local_move_direction);
            let move_distance = world_move_direction * force * move_speed * time.delta_secs();
            transform.translation += move_distance;
        });
    }

    let rotate_speed_angle = 0.1;

    if let Some(rot) = input.rotate_direction {
        camera.iter_mut().for_each(|(mut transform, _)| {
            transform.rotate_local_x(rot.x * rotate_speed_angle);
            transform.rotate_local_y(rot.y * rotate_speed_angle);
        });
    }
}
