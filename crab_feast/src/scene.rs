use bevy::prelude::*;

use crate::event::{MoveInputState, RotateInput};
pub struct ScenePlugin;

#[derive(Component)]
struct AutoCamera;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, Self::setup)
        .add_systems(Update, auto_camera_react_to_input.run_if(resource_exists::<MoveInputState>))
        .add_observer(auto_camera_rotate);
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
            AutoCamera,
        ));
    }
}

fn auto_camera_react_to_input(
    mut camera: Query<(&mut Transform, &mut Camera3d), With<AutoCamera>>,
    input: Res<MoveInputState>,
    time: Res<Time>,
) {
    if !input.active {
        return;
    }

    let move_speed = 0.3;
    camera.iter_mut().for_each(|(mut transform, _)| {
        let local_move_direction = Vec3::new(input.direction.x, 0.0, input.direction.y);
        let world_move_direction = transform.rotation.mul_vec3(local_move_direction);
        let move_distance = world_move_direction * input.force * move_speed * time.delta_secs();
        transform.translation += move_distance;
    });

}

fn auto_camera_rotate(
    trigger: On<RotateInput>,
    mut camera: Query<(&mut Transform, &mut Camera3d), With<AutoCamera>>,
) {
    let rotate_speed = 0.02;
    camera.iter_mut().for_each(|(mut transform, _)| {
        // 计算绕世界Y轴的旋转（yaw）
        let yaw = Quat::from_rotation_y(-trigger.event().0.x * rotate_speed);
        // 计算绕局部X轴的旋转（pitch）
        let pitch = Quat::from_rotation_x(-trigger.event().0.y * rotate_speed);
        
        // 应用旋转到当前旋转上
        transform.rotation = (transform.rotation * yaw * pitch).normalize();
    });
}
