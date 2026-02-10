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
pub struct Controllable;

pub struct ControlInputPlugin;

impl Plugin for ControlInputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MovementInput>()
        .add_systems(Update, controlled_move_system.run_if(|input: Res<MovementInput>| {
            matches!(input.as_ref(), MovementInput::Activated { .. })
        }))
        .add_observer(controlled_look_system);
    }
}

fn controlled_move_system(
    mut controlled_objects: Query<&mut Transform, With<Controllable>>,
    input: Res<MovementInput>,
    time: Res<Time>,
) {
    let move_speed = 30.0;

    if let MovementInput::Activated { direction, force } = input.as_ref() {
        controlled_objects.iter_mut().for_each(|mut controlled_transform| {
                let local_move_direction = Vec3::new(direction.x, 0.0, direction.y);
                let world_move_direction = controlled_transform.rotation.mul_vec3(local_move_direction);
                let move_distance = world_move_direction * *force * move_speed * time.delta_secs();
                controlled_transform.translation += move_distance;
            });
    }
}

fn controlled_look_system(
    trigger: On<LookInput>,
    mut rotation_angles: Local<Option<(Entity, Vec2)>>,
    mut controlled_objects: Query<(Entity, &mut Transform), With<Controllable>>,
) {
    let rotate_speed = 10.0;
    let max_pitch = std::f32::consts::FRAC_PI_2 - 0.1; // 防止翻转，限制在±85度
    
    if let Ok((current_entity, mut transform)) = controlled_objects.single_mut() {
        
        // 检查实体是否发生了变化
        if rotation_angles.as_ref().map(|(e, _)| *e) != Some(current_entity) {
            // 实体变化了，重新初始化角度
            let (_roll, yaw, pitch) = transform.rotation.to_euler(EulerRot::ZYX);
            *rotation_angles = Some((current_entity, Vec2::new(yaw, pitch)));
        }

        // 获取角度
        if let Some((_, ref mut angles)) = *rotation_angles {
            // 更新偏航(yaw)和俯仰(pitch)角度
            angles.x -= trigger.event().0.x * rotate_speed; // Yaw: 左右转动
            angles.y -= trigger.event().0.y * rotate_speed; // Pitch: 上下转动
            
            // 限制俯仰角度，防止翻转
            angles.y = angles.y.clamp(-max_pitch, max_pitch);
            
            // 应用旋转
            transform.rotation = Quat::from_euler(EulerRot::ZYX, 0.0, angles.x, angles.y);
        }
    }
}