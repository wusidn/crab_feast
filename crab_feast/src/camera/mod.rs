//! 第三人称轨道相机：机位几何、缩放、朝向，以及**跟随策略的挂载点**。
//!
//! ```text
//! camera/
//! ├─ mod.rs           机位几何（轨道偏移 / 缩放 / 朝向）+ 系统集编排
//! ├─ follow.rs        策略枚举、设置、共享焦点状态、策略激活条件
//! └─ strategies/
//!    ├─ mod.rs        策略插件的汇总注册
//!    ├─ rigid.rs      刚性绑定
//!    ├─ spring.rs     临界阻尼弹簧
//!    └─ ground_anchor.rs 地面锚定
//! ```
//!
//! 一帧内的顺序是固定的三段（都在 [`ControlSet::Camera`] 里，先于移动输入）：
//!
//! ```text
//! CameraSet::Gather  采集角色位置 → FollowInput
//! CameraSet::Follow  当前策略算焦点 → CameraFollowState
//! CameraSet::Place   焦点 + 轨道偏移 = 机位，并公布 CameraFacing
//! ```
//!
//! 每个策略是一个独立插件，自己往 `CameraSet::Follow` 挂系统，并带
//! `run_if(strategy_is(自己))`：面板里选哪个，就只有哪个策略的系统在跑。
//! 机位计算和输入层完全不认识策略，加策略只需要加一个文件 + 一行注册。

use bevy::{ecs::message::MessageReader, input::mouse::MouseWheel, prelude::*};

use crate::GameState;
use crate::input::{ControlSet, LookController};
use crate::scene::Avatar;

mod follow;
mod strategies;

pub use follow::{
    CameraFollowSettings, CameraFollowState, CameraFollowStrategy, CameraSet, FollowInput,
    MAX_SPEED_RANGE, SMOOTH_TIME_RANGE, SNAP_DISTANCE_RANGE, strategy_is,
};
pub use strategies::{ANCHOR_SPEED_RANGE, ANCHOR_TIME_RANGE, DEAD_ZONE_RANGE};

/// 集中管理所有相机的插件
pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraFacing>()
            .init_resource::<ZoomTarget>()
            .init_resource::<CameraFollowState>()
            .init_resource::<CameraFollowSettings>()
            .init_resource::<FollowInput>()
            // 双指捏合的倍率走消息：触摸输入在 pinch 模块里单独成系统，只在触屏设备上跑
            .add_message::<PinchZoom>()
            // 三段串行：先采角色、再按策略算焦点、最后摆机位。
            // 整组挂在 ControlSet::Camera 上，保证移动系统读到的是本帧的新朝向
            .configure_sets(
                Update,
                (CameraSet::Gather, CameraSet::Follow, CameraSet::Place)
                    .chain()
                    .in_set(ControlSet::Camera)
                    .run_if(in_state(GameState::Game)),
            )
            // 各策略自己注册系统，互不引用
            .add_plugins(strategies::FollowStrategiesPlugin)
            .add_systems(Startup, setup_ui_camera)
            .add_systems(Startup, setup_game_camera)
            // 清掉上一局的跟随状态：新角色出生点可能和上一局很远，但同一局的传送
            // 又不该硬切，所以按状态切换来重置，而不是只靠距离阈值兜底
            .add_systems(OnExit(GameState::Game), reset_follow)
            // 滚轮先改距离，同一帧的机位就用上新距离
            .add_systems(
                Update,
                (zoom_camera, smooth_zoom).chain().in_set(CameraSet::Gather),
            )
            .add_systems(Update, gather_follow_input.in_set(CameraSet::Gather))
            .add_systems(Update, orbit_camera.in_set(CameraSet::Place));
    }
}

/// 轨道相机：绕着焦点转，**与角色朝向无关**。
///
/// 偏航 / 俯仰存在它自己的 [`LookController`] 里（右半屏拖动积分得到），
/// 机位每帧按这两个角重算。角色不再带着相机走，转身由 `input::facing_system`
/// 限速追赶，于是「转相机 → 角色过一会儿才跟上来」。
#[derive(Component)]
pub struct CameraRig {
    /// 到焦点的水平距离（米）
    pub distance: f32,
    /// 视线落点相对**锚点**的高度（米）
    pub focus_height: f32,
}

impl Default for CameraRig {
    fn default() -> Self {
        Self {
            distance: 10.0,
            focus_height: 1.3,
        }
    }
}

/// 目标相机距离（用于缓动过渡）
#[derive(Resource)]
pub struct ZoomTarget {
    pub target_distance: f32,
}

impl Default for ZoomTarget {
    fn default() -> Self {
        Self {
            target_distance: 10.0, // 默认距离
        }
    }
}

/// 双指捏合给出的相机缩放**倍率**：> 1 两指捏拢、相机拉远，< 1 两指张开、相机拉近。
///
/// 只传比例、不传米数：手指张开一倍，距离就该减半，和当前离得多远无关。
/// 米数区间与缓动都在本模块收口，输入层没法把相机直接缩进角色身体里。
#[derive(Message, Debug, Clone, Copy)]
pub struct PinchZoom(pub f32);

/// 滚轮可调的相机距离区间（米），以及每格滚轮的增量。
///
/// 滚轮和移动端的双指捏合共用这一段：滚轮是加减固定米数，捏合是乘倍率，
/// 两条路最后都落在 [`ZoomTarget`] 上，所以区间只写一处。
const DISTANCE_RANGE: (f32, f32) = (2.0, 12.0);
const ZOOM_PER_NOTCH: f32 = 0.4 / 3.0;

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

/// 采集本帧的跟随输入：角色在哪、焦点该抬多高。
///
/// 所有策略都只读这一份快照，避免每个策略各查一遍 `Transform`（也避免
/// 各策略的查询过滤器不一致，出现「弹簧看到角色、锚定看不到」这种怪事）。
fn gather_follow_input(
    rigs: Query<&CameraRig>,
    avatars: Query<(Entity, &Transform), With<Avatar>>,
    mut input: ResMut<FollowInput>,
) {
    let (Some(rig), Some((avatar_entity, avatar))) = (rigs.iter().next(), avatars.iter().next())
    else {
        input.valid = false;
        return;
    };

    input.valid = true;
    input.avatar = avatar_entity;
    input.translation = avatar.translation;
    input.focus_height = rig.focus_height;
}

/// 滚轮拉近 / 推远相机，并把双指捏合的倍率并进来，更新目标距离。
///
/// 两条输入共用 [`ZoomTarget`] 与同一段 [`DISTANCE_RANGE`]：滚轮是「每格固定米数」，
/// 捏合是「每帧乘一个倍率」，所以手感不同、收口却只有一处。
fn zoom_camera(
    mut rigs: Query<&mut CameraRig>,
    mut wheels: MessageReader<MouseWheel>,
    mut pinches: MessageReader<PinchZoom>,
    mut zoom_target: ResMut<ZoomTarget>,
) {
    // 触控板一帧可能发好几条事件，先累加再一次性改距离
    let notches: f32 = wheels.read().map(|wheel| wheel.y).sum();
    // 捏合一帧里两根手指各拖一次就是两条消息，乘起来才是本帧的总倍率
    let ratio: f32 = pinches.read().map(|pinch| pinch.0).product();
    if notches == 0.0 && ratio == 1.0 {
        return;
    }

    for rig in rigs.iter_mut() {
        let new_distance = if notches != 0.0 {
            // 滚轮从「当前实际距离」加减：一格就是一格，不受缓动追赶到哪儿影响
            rig.distance - notches * ZOOM_PER_NOTCH
        } else {
            // 捏合从「目标距离」乘：跟的是手指，不跟被缓动拖慢的实际距离，
            // 否则一次长捏合会被平滑反馈成越捏越慢
            zoom_target.target_distance * ratio
        };
        zoom_target.target_distance = new_distance.clamp(DISTANCE_RANGE.0, DISTANCE_RANGE.1);
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

/// 按轨道参数把游戏相机摆到焦点周围，并公布本帧朝向。
///
/// 焦点由「跟随策略」在 `CameraSet::Follow` 里算好，这里只做几何：
/// 机位 = 焦点 + 轨道偏移。换策略不会动机位几何。
fn orbit_camera(
    rigs: Query<(&CameraRig, &LookController)>,
    input: Res<FollowInput>,
    mut camera_transforms: Query<&mut Transform, (With<CameraRig>, Without<Avatar>)>,
    mut facing: ResMut<CameraFacing>,
    follow: Res<CameraFollowState>,
) {
    if !input.valid || !follow.is_ready() {
        return;
    }
    let Some((rig, look)) = rigs.iter().next() else {
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
    // 相机在水平面的偏移：向后（-Z）投影，再绕 Y 轴旋转 yaw 角度
    let offset_3d = Vec3::new(0.0, height, -horizontal);
    let offset = Quat::from_rotation_y(facing.yaw) * offset_3d;

    // 焦点就是角色本帧的位置：位置由服务端快照直接写（没有外推偏移），
    // 机位跟同一个点走，背景和角色才不在同一条时间轴上各抖各的
    let focus = follow.focus();
    if let Some(mut transform) = camera_transforms.iter_mut().next() {
        *transform = Transform::from_translation(focus + offset).looking_at(focus, Vec3::Y);
    }
}

/// 离开对局时清掉跟随状态：下一局第一帧直接落在新角色上
fn reset_follow(mut state: ResMut<CameraFollowState>) {
    state.reset();
}
