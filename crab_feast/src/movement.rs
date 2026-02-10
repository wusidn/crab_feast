use bevy::prelude::*;


#[derive(Resource, Debug, Clone, Copy, Reflect, Default)]
pub enum MovementInput {
    #[default]
    Idle,
    Activated {
        direction: Vec2,
        force: f32,
    }
}


#[derive(Event)]
pub struct LookInput(pub Vec2);

#[derive(Component)]
pub struct Movement;

pub struct MovementInputPlugin;

impl Plugin for MovementInputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MovementInput>()
        .add_systems(Update, move_input_system.run_if(|input: Res<MovementInput>| {
            matches!(input.as_ref(), MovementInput::Activated { .. })
        }))
        .add_observer(look_input_system);
    }
}

fn move_input_system(
    mut camera: Query<(&mut Transform, &mut Camera3d), With<Movement>>,
    input: Res<MovementInput>,
    time: Res<Time>,
) {
    let move_speed = 30.0;

    if let MovementInput::Activated { direction, force } = input.as_ref() {
        camera.iter_mut().for_each(|(mut transform, _)| {
                let local_move_direction = Vec3::new(direction.x, 0.0, direction.y);
                let world_move_direction = transform.rotation.mul_vec3(local_move_direction);
                let move_distance = world_move_direction * *force * move_speed * time.delta_secs();
                transform.translation += move_distance;
            });
    }
}

fn look_input_system(
    trigger: On<LookInput>,
    mut rotation_angles: Local<Option<Vec2>>,
    mut camera: Query<(&mut Transform, &mut Camera3d), With<Movement>>,
) {
    let rotate_speed = 10.0;
    let max_pitch = std::f32::consts::FRAC_PI_2 - 0.1; // 防止翻转，限制在±85度
    
    if let Ok((mut transform, _)) = camera.single_mut() {
            // 初始化角度
        let angles = rotation_angles.get_or_insert_with(|| {
            let (_roll, yaw, pitch) = transform.rotation.to_euler(EulerRot::ZYX);
            Vec2::new(yaw, pitch)
        });

        // 更新偏航(yaw)和俯仰(pitch)角度
        angles.x -= trigger.event().0.x * rotate_speed; // Yaw: 左右转动
        angles.y -= trigger.event().0.y * rotate_speed; // Pitch: 上下转动
        
        // 限制俯仰角度，防止翻转
        angles.y = angles.y.clamp(-max_pitch, max_pitch);
        
        // 应用旋转
        transform.rotation = Quat::from_euler(EulerRot::ZYX, 0.0, angles.x, angles.y);
    }
}