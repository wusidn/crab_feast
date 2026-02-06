use bevy::{camera::Viewport, prelude::*};

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

fn auto_camera_rotate(
    trigger: On<RotateInput>,
    mut camera: Query<(&mut Transform, &mut Camera3d), With<AutoCamera>>,
) {
    let rotate_speed = 0.002;
    camera.iter_mut().for_each(|(mut transform, _)| {
        // 计算绕世界Y轴的旋转（yaw）
        let yaw = Quat::from_rotation_y(-trigger.event().0.x * rotate_speed);
        // 计算绕局部X轴的旋转（pitch）
        let pitch = Quat::from_rotation_x(-trigger.event().0.y * rotate_speed);
        
        // 应用旋转到当前旋转上
        let new_rotation = yaw * transform.rotation * pitch;
        
        // 四元数投影法锁定z轴（Swing-Twist分解）
        // 将四元数分解为：q = swing * twist
        // 其中twist是绕局部z轴的旋转，swing是垂直于z轴的旋转
        // 我们移除twist分量来锁定z轴
        
        // 获取旋转后的局部z轴方向（即相机的前向向量的反方向）
        let twist_axis = Vec3::Z;
        
        // 计算twist分量：将四元数的向量部分投影到twist轴上
        let q_vec = new_rotation.xyz();
        let projection = q_vec.dot(twist_axis);
        
        // 构造twist四元数：(projection * twist_axis, w)
        let twist = Quat::from_xyzw(
            twist_axis.x * projection,
            twist_axis.y * projection,
            twist_axis.z * projection,
            new_rotation.w,
        );
        
        // 归一化twist四元数
        let twist_normalized = twist.normalize();
        
        // 计算swing分量：swing = q * twist^(-1)
        // swing保留了所有非z轴的旋转
        let swing = new_rotation * twist_normalized.inverse();
        
        // 应用swing旋转（已移除z轴旋转）
        transform.rotation = swing.normalize();
    });
}
