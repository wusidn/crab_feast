use bevy::{ecs::message::MessageReader, input::mouse::MouseWheel, prelude::*};

use crate::GameState;
use crate::input::{ControlSet, LookController};
use crate::scene::Avatar;

/// 集中管理所有相机的插件
pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraFacing>()
            .init_resource::<ZoomTarget>()
            .add_systems(Startup, setup_ui_camera)
            .add_systems(Startup, setup_game_camera)
            // 相机先定机位，移动系统才知道「摇杆往上推」是世界里的哪个方向
            // 滚轮先改距离，同一帧的机位就用上新距离
            .add_systems(
                Update,
                (zoom_camera, smooth_zoom, orbit_camera)
                    .chain()
                    .in_set(ControlSet::Camera)
                    .run_if(in_state(GameState::Game)),
            );
    }
}

/// 轨道相机：绕着角色转，**与角色朝向无关**。
///
/// 偏航 / 俯仰存在它自己的 [`LookController`] 里（右半屏拖动积分得到），
/// 机位每帧按这两个角重算。角色不再带着相机走，转身由 `input::facing_system`
/// 限速追赶，于是「转相机 → 角色过一会儿才跟上来」。
#[derive(Component)]
pub struct CameraRig {
    /// 到角色的水平距离（米）
    pub distance: f32,
    /// 视线落点相对角色原点的高度（米）
    pub focus_height: f32,
}

impl Default for CameraRig {
    fn default() -> Self {
        Self {
            distance: 5.0,
            focus_height: 1.3,
        }
    }
}

/// 目标相机距离（用于缓动过渡）
#[derive(Resource, Default)]
pub struct ZoomTarget {
    pub target_distance: f32,
}

/// 滚轮可调的相机距离区间（米），以及每格滚轮的增量
const DISTANCE_RANGE: (f32, f32) = (2.0, 12.0);
const ZOOM_PER_NOTCH: f32 = 0.4;

/// 第三人称可用的俯仰区间（弧度）：再低就钻地，再高就只剩头顶。
pub const CAMERA_PITCH_RANGE: (f32, f32) = (-0.12, 1.0);

/// 相机此刻的朝向（世界空间弧度）。
///
/// 移动系统按 `yaw` 把摇杆输入换算成世界方向，动画系统按它决定角色该追向哪里。
#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct CameraFacing {
    /// 绕世界 Y 的轨道偏航，0 = 相机在角色正后方（角色面向 +Z，所以机位在 -Z）
    pub yaw: f32,
    /// 俯仰，正值在上
    pub pitch: f32,
}

impl CameraFacing {
    /// 视线在水平面上的朝向（世界空间单位向量），即「屏幕深处」。
    ///
    /// 摇杆往上推走这个方向，角色松手后也面向它。只给几何量，
    /// 不掺角约定，免得和角色的正前方轴定义拧来拧去。
    pub fn view_forward(&self) -> Vec3 {
        Quat::from_rotation_y(self.yaw) * Vec3::Z
    }
}

/// 游戏场景相机资源标记
#[derive(Resource)]
pub struct GameCamera(pub Entity);

/// 设置UI相机（用于加载界面）
fn setup_ui_camera(mut commands: Commands) {
    commands.spawn((
        Camera2d,
        Camera {
            order: 1, // UI相机渲染在游戏场景前面
            ..default()
        },
    ));
}

/// 设置游戏场景相机
fn setup_game_camera(mut commands: Commands) {
    let game_camera = commands
        .spawn((
            Camera3d::default(),
            Camera {
                order: 0, // 游戏场景相机渲染在底层
                ..default()
            },
        ))
        .id();

    commands.insert_resource(GameCamera(game_camera));
}

/// 滚轮拉近 / 推远相机，更新目标距离。
fn zoom_camera(
    mut rigs: Query<&mut CameraRig>,
    mut wheels: MessageReader<MouseWheel>,
    mut zoom_target: ResMut<ZoomTarget>,
) {
    // 触控板一帧可能发好几条事件，先累加再一次性改距离
    let notches: f32 = wheels.read().map(|wheel| wheel.y).sum();
    if notches == 0.0 {
        return;
    }

    for rig in rigs.iter_mut() {
        let new_distance =
            (rig.distance - notches * ZOOM_PER_NOTCH).clamp(DISTANCE_RANGE.0, DISTANCE_RANGE.1);
        zoom_target.target_distance = new_distance;
    }
}

/// 平滑缓动相机距离到目标值
fn smooth_zoom(mut rigs: Query<&mut CameraRig>, zoom_target: Res<ZoomTarget>, time: Res<Time>) {
    let Some(mut rig) = rigs.iter_mut().next() else {
        return;
    };

    // 缓动速度（每秒接近目标距离的 10 倍）
    const SMOOTH_SPEED: f32 = 10.0;
    let dt = time.delta_secs();

    // 线性插值：start + (end - start) * t
    rig.distance = rig
        .distance
        .lerp(zoom_target.target_distance, dt * SMOOTH_SPEED);

    // 确保不会超出范围（防止浮点误差）
    rig.distance = rig.distance.clamp(DISTANCE_RANGE.0, DISTANCE_RANGE.1);
}

/// 按轨道参数把游戏相机摆到角色周围，并公布本帧朝向。
fn orbit_camera(
    rigs: Query<(&CameraRig, &LookController)>,
    avatars: Query<&Transform, With<Avatar>>,
    mut camera_transforms: Query<&mut Transform, (With<CameraRig>, Without<Avatar>)>,
    mut facing: ResMut<CameraFacing>,
) {
    let Some((rig, look)) = rigs.iter().next() else {
        return;
    };
    let Some(avatar) = avatars.iter().next() else {
        return;
    };

    // 本作的模型面向局部 +Z（实测 Walking 净位移沿 +Z），所以偏航 0、俯仰 0 时
    // 相机落在角色背后的 -Z：一进场看到的是后背，不是正脸
    facing.yaw = look.accumulated_yaw;
    // 俯仰夹在第三人称可用的区间里：look_system 只保证不翻面（±85°），
    // 但绕到头顶或地下就看不见角色了
    facing.pitch = look
        .accumulated_pitch
        .clamp(CAMERA_PITCH_RANGE.0, CAMERA_PITCH_RANGE.1);

    let horizontal = rig.distance * facing.pitch.cos();
    let height = rig.distance * facing.pitch.sin();
    let offset = Quat::from_rotation_y(facing.yaw) * Vec3::new(0.0, height, -horizontal);
    let target = avatar.translation + Vec3::Y * rig.focus_height;

    if let Some(mut transform) = camera_transforms.iter_mut().next() {
        *transform = Transform::from_translation(target + offset).looking_at(target, Vec3::Y);
    }
}
