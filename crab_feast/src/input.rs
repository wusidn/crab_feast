use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

use crate::camera::GameCamera;

#[derive(Resource, Debug, Clone, Copy, Reflect, Default)]
pub enum MovementInput {
    #[default]
    Idle,
    Activated {
        direction: Vec2,
        force: f32,
    },
}

#[derive(Event)]
pub struct LookInput(pub Vec2);

#[derive(Resource, Debug, Clone, Copy, Reflect, Default)]
pub enum JumpInput {
    #[default]
    Idle,
    Activated,
}

/// 仅用于移动方向与蒙皮前向；刚体根保持 **identity** 旋转，相机在世界里独立轨道。
#[derive(Component, Clone, Copy, Default)]
pub struct CharacterBodyYaw(pub f32);

/// 蒙皮/场景根父节点（胶囊子级），只同步 [`CharacterBodyYaw`] 的 yaw。
#[derive(Component)]
pub struct PlayerCharacterModelRoot;

#[derive(Component)]
pub struct MovementController {
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
        Self { speed: 10.0 }
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
            .add_systems(
                Update,
                (
                    face_body_toward_local_movement,
                    // 每帧运行：无输入时清零水平速度，避免仅靠阻尼滑行导致与切 idle/根骨 存在长时间错位感
                    movement_system,
                    sync_player_character_model_rotation,
                    sync_third_person_game_camera,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                jump_system.run_if(resource_changed::<MovementInput>),
            )
            .add_observer(look_system);
    }
}

const FACE_TURN_RADIANS_PER_SEC: f32 = 10.0;
/// 与目标角差小于此则直接对齐，避免围绕「移动目标角」做无尽闭环修正
const FACE_ARRIVAL_RAD: f32 = 0.05;
const CAM_OFFSET: Vec3 = Vec3::new(0.0, 1.3, 5.0);
const CAM_LOOK_AT_OFFSET: Vec3 = Vec3::new(0.0, 0.6, 0.0);

fn body_rotation(yaw: f32) -> Quat {
    Quat::from_axis_angle(Vec3::Y, yaw)
}

/// 以相机**轨道水平偏航**为基准的地面移动方向（不随 `CharacterBodyYaw` 而变，避免自激旋转）。
/// `stick.x` 右 / `stick.y` 上：上推为前（`-direction.y`）。
fn intent_horizontal_xz_on_ground(
    direction: Vec2,
    camera_orbit_yaw: f32,
) -> Option<Vec3> {
    if direction.length() < 1e-4 {
        return None;
    }
    let q = Quat::from_euler(EulerRot::ZYX, 0.0, camera_orbit_yaw, 0.0);
    let mut f = q * -Vec3::Z;
    f.y = 0.0;
    if f.length_squared() < 1e-6 {
        f = Vec3::Z;
    } else {
        f = f.normalize();
    }
    let mut r = q * Vec3::X;
    r.y = 0.0;
    if r.length_squared() < 1e-6 {
        r = Vec3::X;
    } else {
        r = r.normalize();
    }
    let mut w = f * -direction.y + r * direction.x;
    w.y = 0.0;
    let len = w.length();
    if len < 1e-5 {
        return None;
    }
    Some(w / len)
}

fn face_body_toward_local_movement(
    time: Res<Time>,
    input: Res<MovementInput>,
    game: Res<GameCamera>,
    camera: Query<&LookController, With<Camera3d>>,
    mut q: Query<&mut CharacterBodyYaw, With<MovementController>>,
) {
    if let MovementInput::Activated { direction, force } = input.as_ref() {
        if *force < 0.01 || direction.length() < 0.01 {
            return;
        }
        let Ok(cam) = camera.get(game.0) else {
            return;
        };
        let Some(w) = intent_horizontal_xz_on_ground(*direction, cam.accumulated_yaw) else {
            return;
        };
        let target_yaw = w.x.atan2(w.z);
        let dt = time.delta_secs();
        for mut yaw in &mut q {
            let from = yaw.0;
            let mut d = target_yaw - from;
            d = (d + std::f32::consts::PI).rem_euclid(2.0 * std::f32::consts::PI)
                - std::f32::consts::PI;
            if d.abs() <= FACE_ARRIVAL_RAD {
                yaw.0 = target_yaw;
                continue;
            }
            let max_step = FACE_TURN_RADIANS_PER_SEC * dt;
            yaw.0 = from + d.clamp(-max_step, max_step);
        }
    }
}

fn clear_horizontal_move_velocity(vel: &mut Velocity) {
    vel.linvel.x = 0.0;
    vel.linvel.z = 0.0;
}

fn movement_system(
    game: Res<GameCamera>,
    camera: Query<&LookController, With<Camera3d>>,
    mut movement_controllers: Query<(&mut Velocity, &MovementController)>,
    input: Res<MovementInput>,
) {
    match input.as_ref() {
        MovementInput::Idle => {
            for (mut vel, _) in &mut movement_controllers {
                clear_horizontal_move_velocity(&mut vel);
            }
        }
        MovementInput::Activated { direction, force } => {
            if *force < 0.01 || direction.length() < 0.01 {
                for (mut vel, _) in &mut movement_controllers {
                    clear_horizontal_move_velocity(&mut vel);
                }
                return;
            }
            let Ok(cam) = camera.get(game.0) else {
                for (mut vel, _) in &mut movement_controllers {
                    clear_horizontal_move_velocity(&mut vel);
                }
                return;
            };
            let Some(w) = intent_horizontal_xz_on_ground(*direction, cam.accumulated_yaw) else {
                for (mut vel, _) in &mut movement_controllers {
                    clear_horizontal_move_velocity(&mut vel);
                }
                return;
            };
            for (mut vel, movement_controller) in &mut movement_controllers {
                let v = w * *force * movement_controller.speed;
                vel.linvel.x = v.x;
                vel.linvel.z = v.z;
            }
        }
    }
}

fn sync_player_character_model_rotation(
    mut model: Query<(&mut Transform, &ChildOf), With<PlayerCharacterModelRoot>>,
    parents: Query<&CharacterBodyYaw, With<MovementController>>,
) {
    for (mut t, child_of) in &mut model {
        if let Ok(yaw) = parents.get(child_of.0) {
            t.rotation = body_rotation(yaw.0);
        }
    }
}

fn sync_third_person_game_camera(
    game_camera: Res<GameCamera>,
    player: Query<&GlobalTransform, (With<MovementController>, With<CharacterBodyYaw>)>,
    mut camera: Query<(&mut Transform, &LookController), With<Camera3d>>,
) {
    let Ok(player_gt) = player.single() else {
        return;
    };
    let Ok((mut cam_tf, look)) = camera.get_mut(game_camera.0) else {
        return;
    };
    let p = player_gt.translation();
    let orbit = Quat::from_euler(
        EulerRot::ZYX,
        0.0,
        look.accumulated_yaw,
        look.accumulated_pitch,
    );
    cam_tf.translation = p + orbit * CAM_OFFSET;
    let look_at = p + CAM_LOOK_AT_OFFSET;
    cam_tf.look_at(look_at, Vec3::Y);
}

fn look_system(
    trigger: On<LookInput>,
    mut look_controllers: Query<&mut LookController, With<Camera3d>>,
) {
    let rotate_speed = 10.0;
    let max_pitch = std::f32::consts::FRAC_PI_2 - 0.1;

    for mut look_controller in &mut look_controllers {
        if !matches!(look_controller.axis, LookAxis::YawAndPitch) {
            continue;
        }
        look_controller.accumulated_yaw -= trigger.event().0.x * rotate_speed;
        look_controller.accumulated_pitch = (look_controller.accumulated_pitch
            - trigger.event().0.y * rotate_speed)
        .clamp(-max_pitch, max_pitch);
    }
}

fn jump_system(input: Res<JumpInput>) {
    if let JumpInput::Activated = input.as_ref() {
        // 跳跃逻辑
    }
}
