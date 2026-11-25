//! 游戏侧参数驱动：把输入与控制状态换算成混合器参数，再把混合出来的地速送回移动。
//!
//! UI 不写这些值——编辑器打开时 [`AnimFocus`] 会指向被聚焦的混合器，这里的系统
//! 就让位（见每个系统开头的 `is_focused` 判断），所以预览拖滑杆不会被游戏状态顶掉。
//! 反向的那一半是 [`sync_move_speed`]：角色的实际移动速度取当前姿态的等效地速。

use avian3d::prelude::LinearVelocity;
use bevy::prelude::*;

use crate::camera::{CAMERA_PITCH_RANGE, CameraFacing};
use crate::input::{
    FacingState, LocomotionIntent, MovementController, PlanarMotion, camera_view_yaw, wrap_pi,
};
use crate::state::GameState;

use super::AnimationSet;
use super::character::AnimModelRoot;
use super::core::{BlendMixer, MixerCore};
use super::driver::{AnimFocus, PoseDriver, drive_pose};
use super::jump::JumpAction;
use super::mixers::{LocomotionMixer, LookMixer, RegisteredMixer};

/// 模型根节点上的「视觉朝向」（世界，弧度，与 `FacingState.yaw` 同口径）。
///
/// 混合空间算 `yaw` 轴要的不是**逻辑**朝向，而是「这具身体看起来朝哪」：
/// 联机预测端由 [`super::turn_visual`] 按支撑期一步步追上来写它；
/// 单机根本没有这个组件（`AvatarYaw` 节点本身就是视觉朝向，见 `facing_system`）。
/// 放在这里而不是 `turn_visual` 里，是因为读取方是本文件、而写入方只在联机有。
// 后面几个字段全是联机跟随器的内部状态（`turn_visual` 独享）：单机编译时
// 它们只会被读写于构造函数里，`dead_code` 会报一堆，这里显式豁免
#[derive(Component, Clone, Copy, Debug)]
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub struct VisualYaw {
    /// 视觉世界朝向（弧度）
    pub(crate) yaw: f32,
    /// 本帧角速度（度/秒），用于加/减速
    pub(crate) rate: f32,
    /// 是否已经和本体对齐过：第一帧必须直接取本体朝向，不能从 0 追
    pub(crate) init: bool,
    /// 距离上次打日志过了多久
    pub(crate) since_log: f32,
    /// 上一帧的可见世界朝向 / 本体朝向（诊断用：抓「单帧跳一大下」是谁跳的）
    pub(crate) last_world: f32,
    pub(crate) last_body: f32,
}

impl Default for VisualYaw {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            rate: 0.0,
            init: false,
            since_log: 0.0,
            last_world: 0.0,
            last_body: 0.0,
        }
    }
}

impl VisualYaw {
    /// 当前的视觉朝向（世界，弧度）。见类型文档。
    pub fn yaw(&self) -> f32 {
        self.yaw
    }
}

/// 四元数里绕 Y 的偏航角。算式只有 `input::yaw_of` 那一份，
/// 这里只是把「取前向量」这一步包一层，免得两个模块各抄一遍 `atan2(x, z)`。
pub(crate) fn yaw_of(rotation: Quat) -> f32 {
    crate::input::yaw_of(rotation * Vec3::Z)
}

/// 转身这条链路的诊断日志开关（`CRAB_LOG_TURN=1`）。
///
/// 之前排查「转一下」时默认常开，日志里每帧级别的东西不该在正式包里刷屏；
/// 和 `CRAB_LOG_YAW` / `CRAB_LOG_JUMP` 同一套约定。
pub(crate) fn log_turn() -> bool {
    std::env::var("CRAB_LOG_TURN").is_ok()
}

pub struct LocomotionPlugin;

impl Plugin for LocomotionPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (drive_locomotion, drive_look)
                .in_set(AnimationSet::Drive)
                // 菜单里不跑角色，参数保持编辑器留下的值
                .run_if(in_state(GameState::Game)),
        )
        .add_systems(
            PostUpdate,
            // 地速来自本帧的姿态求值，所以排在驱动之后；下一帧的移动系统读它
            sync_move_speed
                .after(drive_pose)
                .run_if(in_state(GameState::Game)),
        );
    }
}

/// 混合输出的等效地速 → [`MovementController`] 的目标地速。
///
/// 这一步是「预览里看到的档位」与「游戏里跑出来的速度」对齐的关键：
/// 走 / 跑 / 侧向的快慢全由剪辑自己的净步幅与时长决定，改采样点位置就等于改移速，
/// 不需要在代码里再维护一套走路速度、跑步速度。
fn sync_move_speed(
    driver: Res<PoseDriver>,
    mut controllers: Query<&mut MovementController>,
) {
    // 还没求值过（模型未就绪）就保留默认值；预览单条剪辑时混合空间仍然在求值，
    // 地速依旧新鲜，角色不会停在最后一个档位上滑走
    if !driver.evaluated {
        return;
    }
    // 大转角时姿态档位被抬到 [`PIVOT_POSE_FLOOR`]，这里按补偿系数折算回真实档位：
    // 腿摆得像样一点，但走多远一点不变。
    //
    // 联机角色不需要这个：它没有 [`MovementController`]（位置由服务端快照直接写），
    // 这里有控制器的一定是单机角色，速度就是它的本地物理目标速度。
    let speed = driver.ground_speed * driver.pose_gain;
    for mut controller in &mut controllers {
        if (controller.speed - speed).abs() > 1e-3 {
            controller.speed = speed;
        }
    }
}

/// 移动意图 + 朝向误差 → `yaw` / `speed`。
///
/// 走起来时只动 `speed` 轴：位移方向就是脸向，混合空间永远停在正前方那一列。
///
/// 站着的时候一律回 `(0, 0)` 也就是 Idle：原地转身靠 [`FacingState`] 驱动的
/// 身体旋转本身，不再往混合空间里送偏角。采样点布局里没有「原地转向」这一档
/// （见 spaces.rs 的默认定义），硬送偏角只会混出侧移走的姿态——脚在原地踏步，
/// 人却在转，看着更假。
///
/// 不额外降速：之前试过按误差收速度，结果是「转身期间被拽慢」，观感更像拖沓
/// 而不是转身。
fn drive_locomotion(
    focus: Res<AnimFocus>,
    jump: Res<JumpAction>,
    time: Res<Time>,
    avatars: Query<(
        Entity,
        &FacingState,
        &LocomotionIntent,
        Option<&LinearVelocity>,
        Option<&PlanarMotion>,
    )>,
    // 「脸朝哪」的视觉参考：单机在 `AvatarYaw` 节点上，联机预测端在模型根节点上。
    // 混合空间的 yaw 轴必须相对**视觉**朝向算，不能相对逻辑朝向
    // （见下面 `yaw_deg` 的注释）
    yaw_nodes: Query<(Entity, &ChildOf), With<crate::scene::AvatarYaw>>,
    yaw_transforms: Query<&Transform, With<crate::scene::AvatarYaw>>,
    looks: Query<(&ChildOf, &VisualYaw), With<AnimModelRoot>>,
    mut mixer: ResMut<LocomotionMixer>,
    mut driver: ResMut<PoseDriver>,
) {
    if focus.is_focused(LocomotionMixer::ID) || !mixer.core().enabled {
        return;
    }
    // 滞空期参数冻结在起跳那一刻：松手也不该在空中把地速抽走。
    // 相位照常推进（混合空间每帧都在求值），落地时再把相位写成着地脚。
    if jump.is_airborne() {
        return;
    }

    // 大转角时给姿态档位留一个下限：人还在以 0.75 m/s 慢慢平移，腿却已经塌成
    // 「Idle + 半个走档」的半站姿，看起来就是那一下僵硬。抬档只改观感，
    // 物理速度按 [`PoseDriver::pose_gain`] 折算回去。
    let Some((avatar, facing, intent, velocity, measured)) = avatars.iter().next() else {
        return;
    };
    let planar = planar_displacement(velocity, measured);
    let honest = blend_speed(intent);
    // 「脸朝哪」取**视觉**朝向：单机是 `AvatarYaw` 节点，联机预测端是模型根节点上的
    // `VisualYaw`。这一条是「突然转一大下」的根治点——之前拿逻辑朝向当参考，
    // 甩杆的瞬间位移方向相对脸向直接甩到 180°，混合空间切去放后腿/横移的剪辑，
    // 姿态在 0.25 秒里翻过去一大块，然后等逻辑朝向慢慢追上来才「转正」。
    // 用视觉朝向当参考，转身这段里这具身体看到的偏角就是「移动方向偏了几度」，
    // 不会因为逻辑朝向已经跑到前面去而把整圈的偏角塞进混合空间。
    let visual_yaw = looks
        .iter()
        .find(|(child_of, _)| child_of.parent() == avatar)
        .map(|(_, look)| look.yaw())
        .or_else(|| {
            yaw_nodes
                .iter()
                .find(|(_, child_of)| child_of.parent() == avatar)
                .and_then(|(node, _)| yaw_transforms.get(node).ok())
                .map(|transform| yaw_of(transform.rotation))
        })
        .unwrap_or(facing.yaw);
    // 偏角取**实测位移**，不是输入方向：刹车那半秒速度还朝原方向，姿态就该
    // 还是正前方的跑，不能提前切成倒退剪辑。速度太小没有方向可言，回正前方。
    //
    // 还要按**位移大小**再压一道斜坡：掉头时刹车会把速度穿过 0 再反向，
    // 跨过零点的前后两帧方向差 180°，而那时人几乎没在动（实测 0.27 m/s）。
    // 不压它，混合空间就会在那一帧从"正前跑"切到"放后腿"，观感是姿态
    // **突然翻一大块**（实机日志 `pose/yaw: +0.0° → +166.6°` 就是这一下）。
    // 慢到没有方向可言时，姿态就回正前方，让脚去转，别让剪辑去翻。
    let yaw_raw = if planar.length() > 0.05 {
        wrap_pi(planar.x.atan2(planar.y) - visual_yaw).to_degrees()
    } else {
        0.0
    };
    let dir_gate = ((planar.length() - DIR_GATE_LOW_MPS) / (DIR_GATE_HIGH_MPS - DIR_GATE_LOW_MPS))
        .clamp(0.0, 1.0);
    let error_deg = yaw_raw * dir_gate;
    let speed = pose_axis(honest, error_deg);
    driver.pose_gain = if speed > 1e-4 { honest / speed } else { 1.0 };
    let yaw_deg = error_deg;
    // 诊断：混合空间 yaw 轴单帧跳多少。姿态「突然翻一大块」就是这条轴在跳
    if log_turn() && (yaw_deg - driver.last_pose_yaw).abs() > 30.0 && driver.pose_log_secs >= 0.5 {
        driver.pose_log_secs = 0.0;
        info!(
            "pose/yaw: {:+.1}° → {:+.1}°（位移 {:.2} m/s，视觉参考）",
            driver.last_pose_yaw,
            yaw_deg,
            planar.length()
        );
    }
    driver.last_pose_yaw = yaw_deg;

    let [yaw, speed] = params_for(Some(speed), yaw_deg);

    // 参数轴限速：参数一跳，权重就跟着跳，姿态就是一帧台阶。yaw 现在覆盖整圈，
    // 所以限速要按**最短弧**走，否则 180° 附近抖一下会让参数绕一整圈。
    driver.pose_log_secs += time.delta_secs();
    let dt = time.delta_secs();
    let axes = mixer.core().space.params.clone();
    let core = mixer.core_mut();
    let mut values = [yaw, speed];
    for (index, axis) in axes.iter().enumerate() {
        let Some(current) = core.params.get(axis).copied() else {
            continue;
        };
        // 轴名对不上（编辑器里改过轴序）就不限速，宁可跳也别限错轴
        values[index] = match axis.as_str() {
            "yaw" => slew_deg(current, values[index], PARAM_SLEW[0] * dt),
            "speed" => slew(current, values[index], PARAM_SLEW[1] * dt),
            _ => values[index],
        };
    }
    write_params(core, values);
}

/// 本帧「实测平面位移」：位移方向与大小的唯一口径。
///
/// 联机角色由服务端快照搬位置，物理线速度恒为 0（`Kinematic` 的组件 Avian 不写），
/// 所以有 [`PlanarMotion`] 就以它为准；单机角色没有这个组件，照旧读物理速度。
fn planar_displacement(
    velocity: Option<&LinearVelocity>,
    measured: Option<&PlanarMotion>,
) -> Vec2 {
    match measured {
        Some(measured) => measured.0,
        None => velocity
            .map(|velocity| Vec2::new(velocity.0.x, velocity.0.z))
            .unwrap_or(Vec2::ZERO),
    }
}

/// 角度归一到 (-180, 180]
fn wrap_deg(deg: f32) -> f32 {    (deg + 180.0).rem_euclid(360.0) - 180.0
}

/// 按最短弧限速：跨 ±180 时不绕远路
fn slew_deg(current: f32, target: f32, max_step: f32) -> f32 {
    let delta = wrap_deg(target - current);
    wrap_deg(current + delta.clamp(-max_step, max_step))
}

fn slew(current: f32, target: f32, max_step: f32) -> f32 {
    let delta = target - current;
    if delta.abs() > max_step {
        current + delta.signum() * max_step
    } else {
        target
    }
}

/// 混合空间的 `speed` 轴值：玩家推了多少杆 × 转身降速。
///
/// 转身降速乘在这里而不是 `magnitude` 上：`magnitude` 还要给 `facing_system`
/// 判断「该不该继续追方向」，压成 0 会让身体停在原地干等，180° 永远转不过来。
/// 一路乘到 `speed` 轴，误差到 150° 时整行落到 Idle，两套腿不再同时有权重。
fn blend_speed(intent: &LocomotionIntent) -> f32 {
    if intent.magnitude > 1e-3 {
        intent.magnitude * intent.speed_scale
    } else {
        0.0
    }
}

/// 大转角期间姿态档位的下限（1.0 = 跑档）。
///
/// 转身降速把档位压到 0.25 时，混合空间落在 Idle 与走档中间：腿几乎不摆，
/// 人却在以 0.75 m/s 平移，观感是「僵住然后被拧着转」。抬到 0.35 让腿一直在摆。
const PIVOT_POSE_FLOOR: f32 = 0.35;

/// 下限的启用区间（度）：`START` 以下完全不抬，`END` 以上吃满，中间线性过渡，
/// 免得夹角跨过阈值时档位跳一格。
const PIVOT_FLOOR_RAMP: (f32, f32) = (45.0, 75.0);

/// 真实档位 + 朝向误差 → 送进混合空间的档位。
fn pose_axis(honest: f32, error_deg: f32) -> f32 {
    if honest >= PIVOT_POSE_FLOOR {
        // 本来就跑得比下限快（正常跑、小减速）：一个字都不动
        return honest;
    }
    if honest <= 1e-3 {
        // 站住不动：就是 Idle，别硬抬成走路
        return honest;
    }
    // 从「真实档位」往 0.35 抬，而不是从 0 抬：夹角跨过 45° 时档位是连续的
    let t = ((error_deg.abs() - PIVOT_FLOOR_RAMP.0) / (PIVOT_FLOOR_RAMP.1 - PIVOT_FLOOR_RAMP.0))
        .clamp(0.0, 1.0);
    honest + (PIVOT_POSE_FLOOR - honest) * t
}

/// 位移方向相对脸向的偏角 + 档位 → `yaw` / `speed`（未经限速的原始目标）。
///
/// `yaw_deg` 是**位移方向**相对脸向的偏角（左转为正）；混合空间的 yaw 是右正
/// 左负，所以送进去取反。位移和朝向是两条独立的时间轴：甩头时人先减速、再倒着
/// 往回走，身体慢慢转过去，这期间偏角从 180° 沿圆周滑到 0°，由反放 / 横移剪辑
/// 接力，不需要折回、也不需要给速度翻符号。
fn params_for(magnitude: Option<f32>, yaw_deg: f32) -> [f32; 2] {
    match magnitude {
        Some(value) if value > 1e-3 => [-yaw_deg, value],
        // 站住不动：没有移动方向可言，直接站姿
        _ => [0.0, 0.0],
    }
}

/// 「位移方向可信」的起点速度（米/秒）：低于它，姿态一律按正前方。
///
/// 掉头时刹车把速度压到 0.4×满速以下，还会穿过 0 再反向，那一瞬间方向本身
/// 没有意义；不压这道斜坡，混合空间就会在那一帧从「正前跑」切到「放后腿」，
/// 姿态一帧翻一百多度——实机日志 `pose/yaw: +0.0° → +166.6°`（位移 0.27 m/s）
/// 就是这一下。取 0.7 是照着实测踩的：掉头跨零点那几帧速度就在 0.5~0.6 m/s，
/// 0.35 的起点留下的 32° 残留（`+0.0° → +32.4°`）还在肉眼里
const DIR_GATE_LOW_MPS: f32 = 0.7;
/// 到这个速度，位移方向全额参与混合（米/秒）
const DIR_GATE_HIGH_MPS: f32 = 1.2;

/// 参数轴每帧最大变化量（`yaw` 度/秒，`speed` 单位/秒），顺序同空间定义的轴。
/// 只用来消台阶，别当成滤波：正常操作下这一项几乎不生效。
///
/// `yaw` 从 700 收到 400：掉头时这条轴还是有可能一次性变一百多度（速度穿过 0
/// 那一帧的方向翻转，见 `DIR_GATE_*`），700°/s 只需要 0.24s 扫完，看着就是
/// 「一下翻过去」；400°/s 摊到 0.4s 以上，中间还有位移大小那道斜坡压着
const PARAM_SLEW: [f32; 2] = [400.0, 20.0];

/// 相机与角色的朝向差 → `yaw` / `pitch`。
///
/// 相机朝向已经不再等于角色朝向，所以这两个轴现在都有真实来源了：
/// yaw 是镜头视线相对角色正前方的偏角（上半身该拧多少），pitch 是俯仰。
fn drive_look(
    focus: Res<AnimFocus>,
    camera: Res<CameraFacing>,
    avatars: Query<&FacingState>,
    mut mixer: ResMut<LookMixer>,
) {
    if focus.is_focused(LookMixer::ID) || !mixer.core().enabled {
        return;
    }
    // 相机视线朝向（换算到角色旋转约定）减去角色当前朝向 = 上半身要拧的角度，
    // ±90° 归一化到 ±1
    let yaw = avatars
        .iter()
        .next()
        .map(|state| {
            let offset = wrap_pi(camera_view_yaw(&camera) - state.yaw).to_degrees();
            (offset / 90.0).clamp(-1.0, 1.0)
        })
        .unwrap_or(0.0);
    let pitch = (camera.pitch / CAMERA_PITCH_RANGE.1).clamp(-1.0, 1.0);

    write_params(mixer.core_mut(), [yaw, pitch]);
}

/// 按空间定义的轴顺序写入两个参数，避免这里再抄一遍轴名
fn write_params(core: &mut MixerCore, values: [f32; 2]) {
    let axes = core.space.params.clone();
    for (axis, value) in axes.iter().zip(values) {
        core.params.insert(axis.clone(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_sends_no_direction() {
        assert_eq!(params_for(None, 120.0), [0.0, 0.0]);
        assert_eq!(params_for(Some(0.0), 120.0), [0.0, 0.0]);
    }

    /// 联机角色的实测位移走 `PlanarMotion`，不走物理速度（Kinematic 上恒为 0）。
    #[test]
    fn planar_displacement_prefers_measured_motion() {
        // 单机：没有 `PlanarMotion`，读物理速度
        let velocity = LinearVelocity(Vec3::new(0.6, 0.0, 0.8));
        assert_eq!(
            planar_displacement(Some(&velocity), None),
            Vec2::new(0.6, 0.8)
        );

        // 联机：物理速度是 0，但有 `PlanarMotion` 就听它的
        let still = LinearVelocity(Vec3::ZERO);
        let measured = PlanarMotion(Vec2::new(0.0, -1.2));
        assert_eq!(
            planar_displacement(Some(&still), Some(&measured)),
            Vec2::new(0.0, -1.2)
        );

        // 什么都没挂：没有位移
        assert_eq!(planar_displacement(None, None), Vec2::ZERO);
    }

    #[test]
    fn forward_keeps_full_speed() {
        let [yaw, speed] = params_for(Some(1.0), 0.0);
        assert_eq!(yaw, 0.0);
        assert!((speed - 1.0).abs() < 1e-6);
    }

    /// 位移方向相对脸向的偏角要真的送进混合空间：倒着走（180°）选中反放剪辑，
    /// 侧向走（±90°）选中横移剪辑。右正左负和空间约定一致，所以要取反。
    #[test]
    fn yaw_axis_follows_the_movement_direction() {
        // 正后方：±180 那一列（反放）
        assert!((params_for(Some(1.0), 180.0)[0] + 180.0).abs() < 1e-6);
        // 往左走（位移偏在左，左转正）：空间里是 -90
        assert!((params_for(Some(1.0), 90.0)[0] + 90.0).abs() < 1e-6);
        // 往右走：空间里是 +90
        assert!((params_for(Some(1.0), -90.0)[0] - 90.0).abs() < 1e-6);
    }

    /// 大转角抬档只抬「腿摆得起来」，不抬站立、也不动小角度变向
    #[test]
    fn pivot_floor_only_lifts_the_pose_on_big_turns() {
        // 站住不动：Idle 就是 Idle
        assert_eq!(pose_axis(0.0, 170.0), 0.0);
        // 甩头压到 0.25 时抬到 0.35（否则 Idle 占一半权重，腿不摆）
        assert!((pose_axis(0.25, 170.0) - PIVOT_POSE_FLOOR).abs() < 1e-6);
        // 满速本来就高于下限，一列不动
        assert!((pose_axis(1.0, 170.0) - 1.0).abs() < 1e-6);
        // 小角度变向不碰
        assert!((pose_axis(0.25, 20.0) - 0.25).abs() < 1e-6);
        // 阈值区间内线性过渡，跨过 45° 时档位不跳
        let mid = pose_axis(0.25, 60.0);
        assert!(mid > 0.25 && mid < PIVOT_POSE_FLOOR, "{mid}");
        // 抬档后物理速度要按 gain 折回真实档位
        let gain = 0.25 / pose_axis(0.25, 170.0);
        assert!(
            (PIVOT_POSE_FLOOR * gain - 0.25).abs() < 1e-6,
            "补偿后速度变了"
        );
    }

    #[test]
    fn turn_brake_scales_the_speed_axis() {
        // 满速、没降速：档位原样进混合空间
        let cruising = LocomotionIntent {
            magnitude: 1.0,
            speed_scale: 1.0,
            ..LocomotionIntent::default()
        };
        assert!((blend_speed(&cruising) - 1.0).abs() < 1e-6);
        // 180° 甩头时服务端/客户端都把 scale 压到 0：整行落到 Idle，脚不再交叉
        let pivoting = LocomotionIntent {
            speed_scale: 0.0,
            ..cruising
        };
        assert_eq!(blend_speed(&pivoting), 0.0);
        // 半档 + 0.5：档位和降速是相乘，不是相加
        let half = LocomotionIntent {
            magnitude: 0.5,
            speed_scale: 0.5,
            ..LocomotionIntent::default()
        };
        assert!((blend_speed(&half) - 0.25).abs() < 1e-6);
        // 松手（magnitude 0）不论 scale 多少都该是 0
        assert_eq!(blend_speed(&LocomotionIntent::default()), 0.0);
    }

    #[test]
    fn yaw_slew_takes_the_short_way_round() {
        // 179 → -179 是同一处，最短弧只差 2°，不该绕 358°
        let step = slew_deg(179.0, -181.0, 700.0 * 0.016);
        assert!((step.abs() - 180.0).abs() < 1e-3 || step > 175.0, "{step}");
        let near = slew_deg(170.0, 180.0, 5.0);
        assert!((near - 175.0).abs() < 1e-6, "{near}");
    }
}
