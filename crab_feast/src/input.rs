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

#[derive(Resource, Debug, Clone, Copy, Reflect, Default)]
pub enum JumpInput {
    #[default]
    Idle,
    Activated,
}

#[derive(Component)]
pub struct MovementController{
    pub speed: f32,
}

#[allow(dead_code)]
pub enum LookAxis {
    Yaw,
    Pitch,
    YawAndPitch,
}

#[derive(Component)]
pub struct LookController {
    pub axis: LookAxis,
    pub accumulated_yaw: f32,
    pub accumulated_pitch: f32,
}

impl Default for MovementController {
    fn default() -> Self {
        Self {
            speed: 10.0,
        }
    }
}

impl Default for LookController {
    fn default() -> Self {
        Self {
            axis: LookAxis::YawAndPitch,
            accumulated_yaw: 0.0,
            accumulated_pitch: 0.0,
        }
    }
}

pub struct ControlInputPlugin;

impl Plugin for ControlInputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MovementInput>()
        .init_resource::<JumpInput>()
        .add_systems(Update, movement_system.run_if(|input: Res<MovementInput>| {
            matches!(input.as_ref(), MovementInput::Activated { .. })
        }))
        .add_systems(Update, jump_system.run_if(resource_changed::<MovementInput>))
        .add_observer(look_system);
    }
}

fn movement_system(
    mut movement_controllers: Query<(&mut Transform, &MovementController)>,
    input: Res<MovementInput>,
    time: Res<Time>,
) {
    if let MovementInput::Activated { direction, force } = input.as_ref() {
        movement_controllers.iter_mut().for_each(|(mut transform, movement_controller)| {
                let local_move_direction = Vec3::new(direction.x, 0.0, direction.y);
                let world_move_direction = transform.rotation.mul_vec3(local_move_direction);
                let move_distance = world_move_direction * *force * movement_controller.speed * time.delta_secs();
                transform.translation += move_distance;
            });
    }
}

fn look_system(
    trigger: On<LookInput>,
    mut look_controllers: Query<(&mut Transform, &mut LookController)>,
) {
    let rotate_speed = 10.0;
    let max_pitch = std::f32::consts::FRAC_PI_2 - 0.1; // 防止翻转，限制在±85度
   
    look_controllers.iter_mut().for_each(|(mut transform, mut look_controller)| {
        match look_controller.axis {
            LookAxis::Yaw => {
                look_controller.accumulated_yaw -= trigger.event().0.x * rotate_speed;
            }
            LookAxis::Pitch => {
                look_controller.accumulated_pitch = (look_controller.accumulated_pitch - trigger.event().0.y * rotate_speed)
                    .clamp(-max_pitch, max_pitch);
            }
            LookAxis::YawAndPitch => {
                look_controller.accumulated_yaw -= trigger.event().0.x * rotate_speed;
                look_controller.accumulated_pitch = (look_controller.accumulated_pitch - trigger.event().0.y * rotate_speed)
                    .clamp(-max_pitch, max_pitch);
            }
        }

        // 基于累积的角度创建新的旋转
        transform.rotation = Quat::from_euler(
            EulerRot::ZYX, 
            0.0, // 滚转
            look_controller.accumulated_yaw, 
            look_controller.accumulated_pitch
        );
    });
}

fn jump_system(
    input: Res<JumpInput>,
) {
    if let JumpInput::Activated = input.as_ref() {
        // 跳跃逻辑
    }
}