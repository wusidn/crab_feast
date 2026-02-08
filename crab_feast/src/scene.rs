use bevy::prelude::*;

use crate::event::{MoveInputState, RotateInput};
pub struct ScenePlugin;

#[derive(Component)]
struct AutoCamera;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, Self::setup)
        .add_systems(Update, camera_move_system.run_if(|input: Res<MoveInputState>| {
            matches!(input.as_ref(), MoveInputState::Activated { .. })
        }))
        .add_observer(camera_rotate_system);
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

fn camera_move_system(
    mut camera: Query<(&mut Transform, &mut Camera3d), With<AutoCamera>>,
    input: Res<MoveInputState>,
    time: Res<Time>,
) {
    let move_speed = 0.3;

    if let MoveInputState::Activated { direction, force } = input.as_ref() {
        camera.iter_mut().for_each(|(mut transform, _)| {
                let local_move_direction = Vec3::new(direction.x, 0.0, direction.y);
                let world_move_direction = transform.rotation.mul_vec3(local_move_direction);
                let move_distance = world_move_direction * *force * move_speed * time.delta_secs();
                transform.translation += move_distance;
            });
    }
}

fn camera_rotate_system(
    trigger: On<RotateInput>,
    mut camera: Query<(&mut Transform, &mut Camera3d), With<AutoCamera>>,
) {
    let rotate_speed = 10.0;
    camera.iter_mut().for_each(|(mut transform, _)| {
        // 获取当前向上向量（通常是Y轴）
        let current_up = transform.rotation.mul_vec3(Vec3::Y);
        
        // 计算绕世界Y轴的旋转（yaw），保持向上向量不变
        let yaw = Quat::from_axis_angle(current_up, -trigger.event().0.x * rotate_speed);
        
        // 计算绕本地X轴的旋转（pitch）
        let current_right = transform.rotation.mul_vec3(Vec3::X);
        let pitch = Quat::from_axis_angle(current_right, -trigger.event().0.y * rotate_speed);
        
        // 组合旋转：先应用yaw，再应用pitch
        let rotation_change = yaw * pitch;
        
        // 更新相机旋转
        transform.rotation = rotation_change * transform.rotation;
        
        // 确保四元数标准化
        transform.rotation = transform.rotation.normalize();
    });
}