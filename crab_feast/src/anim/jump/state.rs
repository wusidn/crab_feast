//! 运行时状态：`JumpState` / `JumpAction` / `JumpPolicy`。
//!
//! 这里只放数据与查询方法，状态推进在 `drive.rs`。

use bevy::prelude::*;

use super::resources::{JumpPose, RUN_VARIANTS};
use super::timeline::{CURVE_HZ, Timeline};
use super::tuning::{JumpKind, JumpProfile, JumpTuning};

/// 起跳时机策略。等窗口换来的是"动作精确"，付出的是跟手。
// 三档由 `JumpPolicy` 资源在运行时切换，另外两档暂时没有调用方
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JumpTiming {
    /// 按下就起跳，不等任何相位。脚可能从摆动相起跳，靠融合时长盖过去。
    #[default]
    Immediate,
    /// 等任意一只脚进入支撑：跑最坏 0.30s，走最坏 0.18s。
    AnySupport,
    /// 等起跳腿进入支撑的前 0.14s：跑最坏 0.70s。
    PushOffFoot,
}

/// 跳跃的策略开关。默认立即起跳；换这个资源就能在三种策略间切换，
/// 不用改 `window_open` 的实现。
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct JumpPolicy {
    pub timing: JumpTiming,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JumpState {
    /// 脚在地面上，移动混合完全接管
    #[default]
    Ground,
    /// 已收到请求，等起跳窗口（`Immediate` 下最多停留一帧）
    Queued,
    /// 蓄力 + 蹬伸：根仍在地面，姿态从移动融进蹲姿
    Takeoff,
    /// 滞空：垂直由髋曲线驱动，水平保持起跳速度
    Air,
    /// 落地冲击 + 融回移动
    Land,
    /// 起跳被物理吃掉：姿态冻在离地帧，快速淡出交回移动混合。
    ///
    /// 这一跳没有落地冲击可演，所以**不走** [`JumpState::Land`] 的恢复段——那一段是
    /// 按真实触地设计的，硬播一次就是凭空多出来的一个落地缓冲。
    Abort,
}

/// 跳跃动作层的全部状态。资源是单实例的：本作的动画栈本来就只有一个本地角色。
#[derive(Resource)]
pub struct JumpAction {
    pub state: JumpState,
    /// 动作层对最终姿态的权重（0 = 纯移动混合）
    pub weight: f32,
    /// 当前在剪辑本地时间轴上的位置
    pub clip_time: f32,
    /// 空中 hips 骨骼要钉住的竖直平移（= 起跳时的髋高）
    pub hips_freeze: f32,
    /// 相对起跳高度已经升了多少（米）；下跳为负值
    pub root_y: f32,
    /// 本次跳跃的大类（平地 / 上 / 下）。
    pub kind: JumpKind,
    /// 本次跳跃走的调参档（站 / 跑 / 上 / 下）。
    pub profile: JumpProfile,
    /// 起跳那一帧快照下来的整档参数：空中改表不会影响这一跳。
    pub tuning: JumpTuning,

    /// 动作层已播放的总时间（用于淡入；滞空段时长由物理接地决定，不封顶剪辑时长）
    pub(super) elapsed: f32,
    /// 请求已锁存，等窗口
    pub(super) queued: bool,
    pub(super) queued_for: f32,
    /// 这一次按键已经被消费，必须先松开才能再请求
    pub(super) hold_latched: bool,

    /// 剪辑髋高曲线（米，按 CURVE_HZ 采样）
    pub(super) curve: Vec<f32>,
    pub(super) curve_step: f32,
    /// 这一次用的是哪条剪辑
    pub(super) clip: &'static str,
    /// 本次动作解析后的时间轴（事件锚点 + 段时长）
    pub(super) timeline: Timeline,
    /// 下蹲低点帧（剪辑本地秒），离地前髋高极小值；仅用于观测
    pub(super) crouch_low: f32,
    /// 起跳时的髋高，作为根位移的基线
    pub(super) takeoff_hips: f32,
    /// 上一帧的根位移，用来求竖直速度
    pub(super) last_root: f32,

    /// 滞空段累计时长：空中姿态按慢放系数推进到接地。
    pub(super) air_time: f32,
    /// 落地恢复段进度（0..=recover_secs）：接地后才开始播缓冲、淡出交回移动混合。
    pub(super) land_time: f32,
    /// 滞空段累计时长（含姿态保持），用于宽限判定和兜底超时。
    pub(super) air_hold: f32,
    /// 起跳那一帧的根高度（世界 Y）：判断这一跳有没有真的把根抬起来。
    pub(super) takeoff_y: f32,
    /// 根是否已经明显升离 [`JumpAction::takeoff_y`]：起跳成功的直接证据。
    pub(super) climbed: bool,
    /// 滞空期间是否已经确认离开地面（脚底距地 > [`AIR_LIFT_DIST`]）。
    /// 没抬起来过就不判落地：小速度起跳时人还贴着地面，射线会立刻判「接地」。
    pub(super) air_lifted: bool,
    /// 进入滞空帧记录的起跳上升速度（m/s）；用于预测滞空时长。
    pub(super) launch_vy: f32,
    /// 空中姿态慢放系数（≤1）：预测滞空比剪辑空中段越长，放得越慢。
    pub(super) slow: f32,
    /// 起跳请求序号：每 `begin` 一次自增，随上行快照发给服务端。
    ///
    /// 线上传的是状态快照，只靠 `jump` 电平推上升沿会丢（两帧之间的回落可能被
    /// 合包压平，落地恢复段里的第二跳还会跟上一跳共用一个电平）。把序号放进每条
    /// 快照，服务端比较 `prev_seq != seq` 就认得出新的一跳。
    pub(super) jump_seq: u8,
    /// 落地恢复段的相位对齐基准，**进入 Land 那一帧算一次**，之后整段不变。
    ///
    /// 每帧按「当前权重最大的移动剪辑」现算会让目标相位跟着权重跳：跑着落地
    /// 时人一停，混合从 Running 滑到 Walking / Idle，目标相位从跑姿的触点（0.066）
    /// 跳到 Idle 的 0.51 —— 恢复段中间那一帧相位会瞬移半圈，脚当场换一只。
    pub(super) land_phase_target: f32,
    pub(super) land_phase_advance: f32,
    /// 对齐基准算过没有（`begin` 时清掉，进 Land 时置真）。
    pub(super) land_aligned: bool,
}

impl Default for JumpAction {
    fn default() -> Self {
        Self {
            state: JumpState::default(),
            weight: 0.0,
            clip_time: 0.0,
            hips_freeze: 0.0,
            root_y: 0.0,
            kind: JumpKind::default(),
            profile: JumpProfile::default(),
            tuning: JumpTuning::default(),
            elapsed: 0.0,
            queued: false,
            queued_for: 0.0,
            hold_latched: false,
            curve: Vec::new(),
            curve_step: 1.0 / CURVE_HZ,
            clip: JumpPose::Stand.clip(),
            timeline: Timeline::build(
                JumpPose::Stand.spec(),
                &[],
                1.0 / CURVE_HZ,
                JumpPose::Stand.spec().end,
            ),
            crouch_low: 0.0,
            takeoff_hips: 0.0,
            last_root: 0.0,
            air_time: 0.0,
            land_time: 0.0,
            air_hold: 0.0,
            takeoff_y: 0.0,
            climbed: false,
            air_lifted: false,
            launch_vy: 0.0,
            slow: 1.0,
            jump_seq: 0,
            land_phase_target: 0.0,
            land_phase_advance: 0.0,
            land_aligned: false,
        }
    }
}

impl JumpAction {
    /// 双脚是否离地。空中水平速度只能限速转向（见 `crab_feast_library::air`），
    /// 也不让脚 IK 钉脚。
    pub fn is_airborne(&self) -> bool {
        self.state == JumpState::Air
    }

    /// 进入滞空后已经过了多久（秒）。
    ///
    /// 空中操控只在起跳后的头一段时间里生效（见
    /// `crab_feast_library::net::air_steer_active`），这个值就是那把尺。
    /// 用「起跳后经过多久」而不是「距落地还有多久」：落地时刻要算未来，
    /// 而服务端只能数 tick，两端得用同一个判据才谈得上逐 tick 一致。
    pub fn air_elapsed(&self) -> f32 {
        if self.is_airborne() {
            self.air_time
        } else {
            0.0
        }
    }

    /// 单测用：造一个「已经在滞空」的动作层，不必真跑一遍起跳流程。
    #[cfg(test)]
    pub(crate) fn in_air_for_test() -> Self {
        Self {
            state: JumpState::Air,
            ..Default::default()
        }
    }

    /// 单测用：把滞空计时拨到某一刻，验证「起跳多久之后锁死落点」。
    #[cfg(test)]
    pub(crate) fn set_air_elapsed_for_test(&mut self, secs: f32) {
        self.state = JumpState::Air;
        self.air_time = secs;
    }

    /// 动作层是否在接管姿态（蓄力 / 滞空 / 落地都算）。
    ///
    /// 这三个阶段脚 IK 都不能按移动循环的支撑事件去钉：剪辑的双脚此时有自己的
    /// 轨迹，钉住一只、放开另一只，正好把「两脚并拢起跳」变成「一脚踩地一脚抬」。
    ///
    /// 排队态 [`JumpState::Queued`] 不算接管：角色还在地面正常跑，接触权重和脚 IK
    /// 必须按移动循环继续算；否则排队期间接触权重被清零，跑步起跳窗口读到全零
    /// 接触就一直判「双腾空」拒绝，形成卡死循环。
    pub fn is_active(&self) -> bool {
        !matches!(self.state, JumpState::Ground | JumpState::Queued)
    }

    /// 当前跳跃用的剪辑名。没走过 `begin` 时（比如单测里直接构造）退回原地跳。
    pub fn clip_name(&self) -> &'static str {
        if self.clip.is_empty() {
            JumpPose::Stand.clip()
        } else {
            self.clip
        }
    }

    /// 起跳请求序号：客户端每起跳一次自增，服务端用它认「新的一跳」。
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn jump_seq(&self) -> u8 {
        self.jump_seq
    }

    /// 这一跳用奔跑跳的第几套剪辑（0 起，见 [`RUN_VARIANTS`]）。
    ///
    /// 直接由起跳序号轮换：`begin` 里才自增，所以第一跳是第 1 套，之后 2 → 3 → 1 …
    /// 不另起一个计数器，是因为序号本来就要随上行快照上网（服务端靠它认新的一跳），
    /// 复用同一个数，重连 / 重生成之后编号仍然对得上，也不会多出一份要同步的状态。
    pub fn run_variant(&self) -> u8 {
        self.jump_seq % RUN_VARIANTS
    }

    /// 请求是否还挂着（已锁存、尚未起跳）。联网上行用它决定是否先让出电平。
    ///
    /// 必须看锁存位，不能只看状态：落地恢复段（[`JumpState::Land`]）里按下时状态仍是
    /// `Land`，上行电平本来就是 true；等回到 `Ground` 窗口已经开着，状态一路
    /// `Ground → Takeoff` 都不回落——服务端看不到上升沿，这一跳只有客户端在演。
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn is_queued(&self) -> bool {
        self.queued || self.state == JumpState::Queued
    }
}
