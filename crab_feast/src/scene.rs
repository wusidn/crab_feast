use bevy::{camera::Viewport, prelude::*};

use crate::event::MoveInputState;
pub struct ScenePlugin;

#[derive(Component)]
struct AutoCamera;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, Self::setup)
        .add_systems(Update, auto_camera_react_to_input.run_if(resource_exists::<MoveInputState>));
    }
}

impl ScenePlugin {
    fn setup(
        mut commands: Commands,
        mut meshes: ResMut<Assets<Mesh>>,
        mut materials: ResMut<Assets<StandardMaterial>>,
        window: Single<&Window>,
    ) {

        let window_size = window.resolution.physical_size().as_vec2();

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
            AutoCamera,
            Camera {
                viewport: Some(Viewport {
                    physical_position: (window_size * 0.125).as_uvec2(),
                    physical_size: (window_size * 0.75).as_uvec2(),
                ..default()
            }),
            ..Default::default()
            }
        ));
    }
}

fn auto_camera_react_to_input(
    mut camera: Query<(&mut Transform, &mut Camera3d), With<AutoCamera>>,
    input: Res<MoveInputState>,
    time: Res<Time>,
) {

    let move_speed = 3.;
    camera.iter_mut().for_each(|(mut transform, _)| {
        let local_move_direction = Vec3::new(input.0.x, 0.0, input.0.y);
        let world_move_direction = transform.rotation.mul_vec3(local_move_direction);
        let move_distance = world_move_direction * input.1 * move_speed * time.delta_secs();
        transform.translation += move_distance;
    });
}
