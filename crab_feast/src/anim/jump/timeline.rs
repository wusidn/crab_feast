//! 剪辑时间轴：锚点校正、蓄力 / 滞空 / 恢复段时长、髋高曲线采样。
//!
//! 本地起跳与远端观察共用同一套事件检测，离地帧才能和物理对得上。

use bevy::animation::{AnimationClip, AnimationTargetId};
use bevy::prelude::*;
use bevy_animation::animated_field;

use super::resources::{Anticipation, JumpClipSpec, JumpPose};
use super::state::JumpState;

/// 进入时从移动混合融进来的时长。
///
/// 起跳前腿要从「跑步跨步的一条腿在前一条在后」收到「双脚并拢下蹲」，
/// 0.08s 只有 5 帧，腿的角速度接近 3 rad/s，看着就是绊一下。0.14s 才是可读的"收腿"。
///
/// 但这只是上限：淡入**必须在离地帧之前跑完**（见 [`blend_for`]）。否则物理已经把
/// 人抬起来了，姿态还停在跑步循环里——跑跳的蓄力段只有 0.05s，慢 0.09s 就是
/// 「人已经飞起来了还在跑」。
pub(super) const BLEND_IN_SECS: f32 = 0.14;

/// 淡入最短时长（一帧半）：压得再狠也不能在一帧里把姿态硬切过去。
pub(super) const MIN_BLEND_SECS: f32 = 0.02;

/// 起手淡入时长：不超过本档的蓄力段（`take_secs`），保证离地那一帧姿态已经换完。
///
/// 蓄力段比最短淡入还短时（`RunningJump2` 离地帧只有 0.017s，不到一帧），
/// 让「离地帧满权重」优先：宁可把淡入压进那半帧，也不能让人已经离地、
/// 姿态还停在跑步循环上。
fn blend_for(take_secs: f32) -> f32 {
    BLEND_IN_SECS.min(take_secs.max(MIN_BLEND_SECS)).min(take_secs)
}

/// 髋曲线的采样频率（只采一个通道，一次性开销）
pub(super) const CURVE_HZ: f32 = 120.0;

/// 髋高基线事件检测阈值（米）：髋高抬离首帧基线超过这个值判为离地。
const EVENT_BASELINE: f32 = 0.03;

/// 一次跳跃解析后的时间轴：默认锚点经曲线事件检测校正，段时长按策略折算。
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Timeline {
    pub(super) entry: f32,
    /// 离地事件帧（剪辑本地秒）
    pub(super) takeoff: f32,
    /// 落地事件帧（剪辑本地秒）
    pub(super) land: f32,
    pub(super) end: f32,
    /// 蓄力段动作时长（秒）：下蹲→蹬伸
    pub(super) take_secs: f32,
    /// 滞空段动作时长（秒）：1:1 保留剪辑
    pub(super) air_secs: f32,
    /// 落地恢复段动作时长（秒）：压缩后
    pub(super) recover_secs: f32,
    /// 起手淡入时长（秒）：永远不超过 `take_secs`，离地帧姿态已就位
    pub(super) blend_secs: f32,
}

impl Timeline {
    /// 用资源表里的锚点 + 实测髋高曲线解析出本次动作的时间轴。
    pub(super) fn build(spec: &JumpClipSpec, curve: &[f32], step: f32, duration: f32) -> Self {
        let (takeoff, land) = match spec.anticipation {
            Anticipation::Preserve => detect_events(spec, curve, step, duration),
            // 高台准备段不展示深蹲：锚点固定，事件检测会把它当成离地提前退出
            Anticipation::CompressTo(_) => (spec.takeoff, spec.land),
        };
        // 事件顺序兜底：落地不能早于离地
        let land = land.max(takeoff);
        let take_secs = match spec.anticipation {
            Anticipation::Preserve => (takeoff - spec.entry).max(0.0),
            Anticipation::CompressTo(secs) => secs.max(0.0),
        };
        let air_secs = (land - takeoff).max(0.0);
        Self {
            entry: spec.entry,
            takeoff,
            land,
            end: spec.end,
            take_secs,
            air_secs,
            recover_secs: spec.recover_secs,
            blend_secs: blend_for(take_secs),
        }
    }

    /// 把蓄力段对齐到外部时长（联网跟服务端蓄力走）。
    ///
    /// 淡入要同步压进新的蓄力段：服务端按 0.05s 就把人蹬起来了，
    /// 姿态却还要 0.14s 才换完，那 0.09s 就是「脚离地了还在跑」。
    pub(super) fn set_take_secs(&mut self, secs: f32) {
        self.take_secs = secs.max(0.0);
        self.blend_secs = blend_for(self.take_secs);
    }

    /// 剪辑时间轴上的理论总时长（秒）。实际滞空段由物理接地信号决定，
    /// 不再按这个时长提前退出；仅测试锚点时参照。
    #[allow(dead_code)]
    pub(super) fn total_secs(self) -> f32 {
        self.take_secs + self.air_secs + self.recover_secs
    }

    /// 按状态与段进度求剪辑本地时间。
    ///
    /// - `Takeoff`：蓄力段 `entry → takeoff`；
    /// - `Air`：整段腾空弧线 `takeoff → land` 按 `slow` 慢放拉伸；滞空比剪辑
    ///   长时钉在落地帧，等接地信号，不回绕、不循环；
    /// - `Land`：接地后恢复段 `land → end`；
    /// - `Abort`：起跳失败，定格在离地帧 `takeoff`（**不**取 `land`），权重由动作层
    ///   在 [`ABORT_BLEND_SECS`] 内淡出，姿态不回弹、也不假装落地。
    pub(super) fn clip_time(
        self,
        state: JumpState,
        elapsed: f32,
        air_time: f32,
        land_time: f32,
        slow: f32,
    ) -> f32 {
        match state {
            JumpState::Takeoff => {
                if self.take_secs > f32::EPSILON {
                    let k = (elapsed / self.take_secs).clamp(0.0, 1.0);
                    self.entry + k * (self.takeoff - self.entry)
                } else {
                    self.takeoff
                }
            }
            JumpState::Air => {
                if self.air_secs > f32::EPSILON {
                    let k = (air_time * slow) / self.air_secs;
                    self.takeoff + k.clamp(0.0, 1.0) * (self.land - self.takeoff)
                } else {
                    self.land
                }
            }
            JumpState::Land | JumpState::Ground => {
                if self.recover_secs > f32::EPSILON {
                    let k = (land_time / self.recover_secs).clamp(0.0, 1.0);
                    self.land + k * (self.end - self.land)
                } else {
                    self.end
                }
            }
            // 起跳失败：定格在离地那一帧的姿态。取 `land` 会让人在半空摆出踩地
            // 的恢复姿势，再在这上面淡出——玩家看到的是一次凭空的「落地回弹」。
            JumpState::Abort => self.takeoff,
            JumpState::Queued => self.entry,
        }
    }

    /// 按状态求动作层权重：蓄力 / 滞空保持淡入后的权重，落地恢复段淡出交回移动混合。
    ///
    /// `Abort` 的权重不在这里算：失败那一帧要从**上一帧**的权重开始往下淡，这里返回
    /// 0 会把淡出起点直接抹掉，所以由动作层用 [`ABORT_BLEND_SECS`] 逐帧递减。
    pub(super) fn weight(self, state: JumpState, elapsed: f32, land_time: f32) -> f32 {
        let fade_in = (elapsed / self.blend_secs.max(f32::EPSILON)).clamp(0.0, 1.0);
        match state {
            JumpState::Takeoff | JumpState::Air => fade_in,
            JumpState::Land => {
                let k = (land_time / self.recover_secs.max(f32::EPSILON)).clamp(0.0, 1.0);
                (fade_in * (1.0 - k)).clamp(0.0, 1.0)
            }
            JumpState::Abort | JumpState::Ground | JumpState::Queued => 0.0,
        }
    }
}

/// 髋高基线法检测离地 / 落地事件（剪辑本地秒）；检测不到回退 `spec` 默认锚点。
///
/// 离地：髋高第一次抬离首帧基线超过 [`EVENT_BASELINE`]；落地：顶点之后
/// 髋高回到基线附近。登上高台这类末帧仍高于基线的剪辑找不到落地帧，
/// 由调用方回退默认值。
pub(super) fn detect_events(
    spec: &JumpClipSpec,
    curve: &[f32],
    step: f32,
    duration: f32,
) -> (f32, f32) {
    if curve.len() < 4 {
        return (spec.takeoff, spec.land);
    }
    let base = curve[0];
    let takeoff = curve
        .iter()
        .position(|&y| y > base + EVENT_BASELINE)
        .map(|i| (i as f32 * step).clamp(spec.entry, duration))
        .unwrap_or(spec.takeoff);

    let apex = curve
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let land = (apex + 1..curve.len())
        .find(|&i| curve[i] <= base + EVENT_BASELINE)
        .map(|i| (i as f32 * step).clamp(takeoff, duration))
        .unwrap_or(spec.land);

    (takeoff, land)
}

/// 剪辑名 → 资源表里的时间轴参数；表里没有的名字（缺件回退出来的）按原地跳处理。
pub(super) fn spec_for(clip: &str) -> &'static JumpClipSpec {
    JumpPose::from_clip(clip).map_or_else(|| JumpPose::Stand.spec(), JumpPose::spec)
}

/// 解析剪辑的髋高曲线与时间轴（离地 / 落地帧）。本地起跳与远端观察共用同一份
/// 事件锚点，否则动画的离地帧对不上物理的离地时刻。
///
/// 曲线按 [`CURVE_HZ`] 采样，一次跳跃开始时采下来，省得每帧再解一条剪辑通道。
pub(super) fn analyze_clip(
    clip: &'static str,
    asset: &AnimationClip,
    hips: AnimationTargetId,
) -> (Timeline, Vec<f32>, f32) {
    let spec = spec_for(clip);
    let duration = asset.duration().max(spec.end);
    let step = 1.0 / CURVE_HZ;
    let count = (duration / step).ceil() as usize + 1;
    let mut curve = Vec::with_capacity(count);
    for index in 0..count {
        let time = (index as f32 * step).min(duration);
        let y = asset
            .sample_clamped(animated_field!(Transform::translation), hips, time)
            .map(|translation| translation.y)
            .unwrap_or(0.0);
        curve.push(y);
    }
    let timeline = Timeline::build(spec, &curve, step, duration);
    (timeline, curve, step)
}

/// 在髋高曲线上按 `step` 线性插值，取 `time` 处的值。
pub(super) fn sample_curve(curve: &[f32], step: f32, time: f32) -> f32 {
    if curve.is_empty() || step <= f32::EPSILON {
        return 0.0;
    }
    let x = (time / step).max(0.0);
    let index = x.floor() as usize;
    let frac = x - index as f32;
    let a = curve[index.min(curve.len() - 1)];
    let b = curve[(index + 1).min(curve.len() - 1)];
    a + (b - a) * frac
}
