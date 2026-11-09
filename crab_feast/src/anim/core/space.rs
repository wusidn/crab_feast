//! Blend Space 2D 文档与求值。
//!
//! 一个混合空间 = 两个输入参数 + 一组「参数坐标 → 剪辑」的采样点；
//! 任意参数点用三角剖分 + 重心坐标插值出权重，所有剪辑共享一个归一化相位。

use std::collections::HashMap;

use bevy::{
    animation::{AnimationClip, AnimationTargetId},
    prelude::*,
};
use serde::{Deserialize, Serialize};

use super::{
    blendspace::{self, Sample2D},
    clip::{ClipMeta, ClipSet},
    pose::{BonePose, Pose},
};

fn speed_one() -> f32 {
    1.0
}

/// 某一轴上采样点的实际跨度 `(min, max)`；没有采样点时返回 `None`
fn sample_span(samples: &[Sample2D], axis: usize) -> Option<(f32, f32)> {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for sample in samples {
        let value = sample.at[axis.min(1)];
        if value.is_finite() {
            min = min.min(value);
            max = max.max(value);
        }
    }
    if min.is_finite() && max.is_finite() {
        Some((min, max))
    } else {
        None
    }
}

/// 文档没有写轴范围时，按采样点跨度推导：两侧各留 10% 余量，单点轴取该点 ±0.5。
/// 完全没有采样点时退回归一化的 `[-1, 1]`。
///
/// 用「推导」而不是「猜一个默认单位」：同一个 `BlendSpaceDoc` 既放角度轴也放
/// 归一化轴，任何固定默认值都会在另一类轴上把参数挡在滑杆范围之外。
fn derived_range(samples: &[Sample2D], axis: usize) -> [f32; 2] {
    let Some((min, max)) = sample_span(samples, axis) else {
        return [-1.0, 1.0];
    };
    let span = max - min;
    if span <= 1e-4 {
        return [min - 0.5, max + 0.5];
    }
    let pad = span * 0.1;
    [min - pad, max + pad]
}

/// 一个 Blend Space 2D（文档即真源，可直接序列化进 assets）
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Resource)]
pub struct BlendSpaceDoc {
    pub version: u32,
    /// 两个输入轴的参数名，顺序为 [横轴, 纵轴]
    pub params: [String; 2],
    /// 两个输入轴的取值区间，顺序与 `params` 对齐。
    ///
    /// 省略时按采样点跨度推导（见 [`BlendSpaceDoc::range`]），**不再**用一个
    /// 写死的默认区间——那会在「角度轴 vs 归一化轴」之间把一侧参数挡在滑杆外。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranges: Option<[[f32; 2]; 2]>,
    /// 采样点：参数坐标 + 剪辑名
    pub samples: Vec<Sample2D>,
    #[serde(default = "speed_one")]
    pub speed: f32,
}

impl Default for BlendSpaceDoc {
    fn default() -> Self {
        Self {
            version: 1,
            params: ["yaw".to_string(), "speed".to_string()],
            ranges: None,
            samples: Vec::new(),
            speed: 1.0,
        }
    }
}

impl BlendSpaceDoc {
    pub fn axis(&self, index: usize) -> &str {
        &self.params[index.min(1)]
    }

    /// 某一轴的 `[最小值, 最大值]`：文档里写了就用，没写就按采样点跨度推导
    pub fn range(&self, index: usize) -> [f32; 2] {
        let index = index.min(1);
        match self.ranges {
            Some(ranges) => ranges[index],
            None => derived_range(&self.samples, index),
        }
    }

    /// 两轴区间的一次性快照：UI 每帧要拿同一个值同时换算像素与参数增量，
    /// 分别调用 `range()` 在推导模式下没有差别，但集中取更省事也更好读。
    pub fn axis_ranges(&self) -> [[f32; 2]; 2] {
        [self.range(0), self.range(1)]
    }

    /// 把显式范围扩张到至少包住全部采样点，返回是否改动过。
    ///
    /// 老文档把 speed 写成 `0..1`、yaw 写成 `±180` 时，滑杆会把另一半参数
    /// （负速度）直接截掉；这里只做「扩张」，从不缩小，不会动编辑者刻意留大的区间。
    pub fn fit_ranges(&mut self) -> bool {
        let Some(mut ranges) = self.ranges else {
            return false;
        };
        let mut changed = false;
        for (axis, range) in ranges.iter_mut().enumerate() {
            let Some((min, max)) = sample_span(&self.samples, axis) else {
                continue;
            };
            let span = (range[1] - range[0]).abs().max(1e-6);
            if min < range[0] {
                range[0] = min - span * 0.02;
                changed = true;
            }
            if max > range[1] {
                range[1] = max + span * 0.02;
                changed = true;
            }
        }
        if changed {
            self.ranges = Some(ranges);
        }
        changed
    }

    pub fn weights(&self, point: [f32; 2]) -> Vec<(String, f32)> {
        let mut weights = blendspace::weights_2d(&self.samples, point);
        // 双线性四角里必然有「几乎为 0」的那几个：它们仍然会触发一次完整的
        // 姿态采样（几十根骨骼 × 三条通道），是每帧最贵的一步，先剪掉再归一化。
        weights.retain(|(_, weight)| *weight > 1e-3);
        let total: f32 = weights.iter().map(|(_, weight)| *weight).sum();
        if total > f32::EPSILON {
            for (_, weight) in weights.iter_mut() {
                *weight /= total;
            }
        }
        weights
    }

    pub fn add_sample(&mut self, at: [f32; 2], clip: impl Into<String>) -> usize {
        let ranges = self.axis_ranges();
        self.samples.push(Sample2D {
            at: [
                at[0].clamp(ranges[0][0], ranges[0][1]),
                at[1].clamp(ranges[1][0], ranges[1][1]),
            ],
            clip: clip.into(),
            reverse: false,
        });
        self.samples.len() - 1
    }

    pub fn remove_sample(&mut self, index: usize) -> bool {
        if index < self.samples.len() {
            self.samples.remove(index);
            true
        } else {
            false
        }
    }

    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if self.samples.len() < 3 {
            issues.push(format!(
                "采样点只有 {} 个，少于 3 个会退化成反距离权重",
                self.samples.len()
            ));
        }
        if self.samples.iter().any(|sample| sample.clip.is_empty()) {
            issues.push("有采样点没有绑定剪辑".to_string());
        }
        let mut seen: Vec<([i32; 2], &str)> = Vec::new();
        for sample in &self.samples {
            let key = [
                (sample.at[0] * 100.0).round() as i32,
                (sample.at[1] * 100.0).round() as i32,
            ];
            if seen
                .iter()
                .any(|(other, clip)| *other == key && *clip == sample.clip)
            {
                issues.push(format!(
                    "{} 坐标上有重复的 {} 采样点",
                    format_point(sample.at),
                    sample.clip
                ));
            }
            seen.push((key, &sample.clip));
        }
        if let Some(ranges) = self.ranges {
            for (axis, range) in ranges.iter().enumerate() {
                let Some((min, max)) = sample_span(&self.samples, axis) else {
                    continue;
                };
                if min < range[0] - 1e-4 || max > range[1] + 1e-4 {
                    issues.push(format!(
                        "{} 轴范围 [{:.2}, {:.2}] 没包住采样点 [{:.2}, {:.2}]，滑杆会有一侧调不到",
                        self.axis(axis),
                        range[0],
                        range[1],
                        min,
                        max
                    ));
                }
            }
        }
        issues
    }
}

fn format_point(at: [f32; 2]) -> String {
    format!("({:.2}, {:.2})", at[0], at[1])
}

/// 求值所需的外部输入
pub struct SpaceEvalContext<'a> {
    pub params: &'a HashMap<String, f32>,
    pub clips: &'a ClipSet,
    pub clip_assets: &'a Assets<AnimationClip>,
    pub targets: &'a [AnimationTargetId],
    pub rest: &'a HashMap<AnimationTargetId, BonePose>,
    /// 跨帧保留的归一化相位
    pub phase: &'a mut f32,
    pub dt: f32,
    pub speed_scale: f32,
    pub hips: Option<AnimationTargetId>,
}

pub struct SpaceEvalOutput {
    pub pose: Pose,
    pub root_delta: Vec3,
    pub clip_weights: Vec<(String, f32)>,
    /// 混合输出的等效地速（米/秒，只给大小），见 [`blended_ground_speed`]
    pub ground_speed: f32,
    pub issues: Vec<String>,
    pub point: [f32; 2],
}

/// 混合输出的等效循环时长（秒）。
///
/// 输入是每个采样剪辑的 `(净步幅, 时长, 权重)`。返回 `Σw·S / Σw·(S/T)`，
/// 即让混合输出的地速严格等于 `Σw·v` 的周期；所有剪辑都没有可用步幅
/// （例如纯 Idle）时返回 `None`，由调用方回退到加权时长。
pub fn effective_period(weighted: &[(f32, f32, f32)]) -> Option<f32> {
    let mut stride_total = 0.0f32;
    let mut speed_total = 0.0f32;
    for (stride, duration, weight) in weighted {
        let s = stride.abs();
        if *duration > f32::EPSILON && s > f32::EPSILON && weight.is_finite() && *weight > 0.0 {
            stride_total += s * weight;
            speed_total += (s / *duration) * weight;
        }
    }
    if stride_total > 1e-4 && speed_total > 1e-4 {
        Some((stride_total / speed_total).clamp(0.05, 30.0))
    } else {
        None
    }
}

/// 按权重对各剪辑的事件相位做**环形加权平均**，作为混合区间的统一时间轴。
///
/// 独占时（只有一个权重非零）它就是该剪辑自己的事件，映射退化为恒等，
/// 单段动画的节奏完全不被改动；混合时它落在两者之间，双方各改一半，
/// 事件精确对齐，支撑期长度也落在中间而不是偏向某一方。
pub fn average_events(sources: &[(f32, [Option<f32>; 4])]) -> [Option<f32>; 4] {
    let mut out = [None; 4];
    for slot in 0..4 {
        let mut items: Vec<(f32, f32)> = Vec::new();
        for (weight, events) in sources {
            if let Some(phase) = events[slot]
                && weight.is_finite()
                && *weight > 0.0
            {
                items.push((*weight, phase.rem_euclid(1.0)));
            }
        }
        if items.is_empty() {
            continue;
        }
        let base = items[0].1;
        let mut sum = 0.0;
        let mut total = 0.0;
        for (weight, phase) in &items {
            // 取与基准最近的等价相位，避免 0/1 边界把平均值拉到中间
            let mut p = *phase;
            let mut delta = p - base;
            while delta > 0.5 {
                p -= 1.0;
                delta -= 1.0;
            }
            while delta < -0.5 {
                p += 1.0;
                delta += 1.0;
            }
            sum += *weight * p;
            total += *weight;
        }
        if total > 0.0 {
            out[slot] = Some((sum / total).rem_euclid(1.0));
        }
    }
    out
}

/// 由「参考事件」和「剪辑自己的事件」构造分段线性映射表。
///
/// 事件顺序固定为 `[左脚触地, 左脚离地, 右脚触地, 右脚离地]`。
/// 首末事件的缺口由最后一段环绕补上，因此映射天然闭合、不会逐周期漂移。
/// 事件不足两个、或参考相位挤在一起时返回空表（调用方退回恒等映射）。
pub fn gait_segments(events: &[Option<f32>; 4], reference: &[Option<f32>; 4]) -> Vec<(f32, f32)> {
    let mut pairs: Vec<(f32, f32)> = (0..4)
        .filter_map(|index| match (reference[index], events[index]) {
            (Some(u), Some(l)) => Some((u.rem_euclid(1.0), l.rem_euclid(1.0))),
            _ => None,
        })
        .collect();
    if pairs.len() < 2 {
        return Vec::new();
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    if pairs
        .windows(2)
        .any(|window| (window[1].0 - window[0].0).abs() < 1e-3)
    {
        return Vec::new();
    }
    // 解缠绕：相邻差归一到 [0,1)，让本地相位随参考相位单调
    for index in 1..pairs.len() {
        let mut delta = pairs[index].1 - pairs[index - 1].1;
        while delta < 0.0 {
            delta += 1.0;
        }
        while delta >= 1.0 {
            delta -= 1.0;
        }
        pairs[index].1 = pairs[index - 1].1 + delta;
    }
    let span = pairs[pairs.len() - 1].1 - pairs[0].1;
    // 首末差必须小于一个周期，剩下的缺口由「最后事件 → 首事件」的环绕段补上
    if !(0.3..=1.0).contains(&span) {
        return Vec::new();
    }
    pairs
}

/// 把统一相位按分段表映射到剪辑本地相位（表为空时恒等）。
pub fn warp_phase(phase: f32, points: &[(f32, f32)]) -> f32 {
    if points.len() < 2 {
        return phase;
    }
    let count = points.len();
    let base = points[0].0;
    let u = (phase - base).rem_euclid(1.0) + base;
    // 取最后一个起点 ≤ u 的分段；整表起点都在 u 之后时退回最后一段（环绕段）
    let index = points
        .iter()
        .rposition(|&(start, _)| start <= u)
        .unwrap_or(count - 1);
    let (u0, l0) = points[index];
    let (u1, l1) = if index + 1 < count {
        points[index + 1]
    } else {
        (points[0].0 + 1.0, points[0].1 + 1.0)
    };
    let span = (u1 - u0).max(1e-6);
    let t = ((u - u0) / span).clamp(0.0, 1.0);
    (l0 + t * (l1 - l0)).rem_euclid(1.0)
}

/// 混合输出的等效地速（米/秒）：`Σ w·|净步幅| / 时长`。
///
/// 与 [`effective_period`] 是同一个模型的两面：相位按 `1 / T_ref` 推进，
/// 一个周期里各剪辑贡献的位移按权重相加，得到的地速正好就是这个加权和，
/// 所以拿它去驱动角色位移不会滑步。
///
/// 取绝对值是因为倒放（后退）只改变方向、不改变脚底走过的距离；
/// 净步幅未知的剪辑（Idle / 一次性动作）按 0 计入，会自然把档位稀释下来。
pub fn blended_ground_speed(weighted: &[(f32, f32, f32)]) -> f32 {
    let mut total = 0.0f32;
    for (stride, duration, weight) in weighted {
        if *duration > f32::EPSILON
            && stride.abs() > f32::EPSILON
            && weight.is_finite()
            && *weight > 0.0
        {
            total += stride.abs() / *duration * weight;
        }
    }
    total
}

pub fn evaluate_space(doc: &BlendSpaceDoc, ctx: &mut SpaceEvalContext) -> SpaceEvalOutput {
    let point = [
        ctx.params.get(doc.axis(0)).copied().unwrap_or(0.0),
        ctx.params.get(doc.axis(1)).copied().unwrap_or(0.0),
    ];
    // 文档层面的校验（重复坐标/空剪辑）由编辑器在重建时调 `validate` 一次即可；
    // 这里的 issues 只承载求值期的运行时问题，避免每帧做一次 O(n²) 扫描。
    let mut issues: Vec<String> = Vec::new();

    let weights = doc.weights(point);
    if weights.is_empty() {
        issues.push("没有可用的采样点".to_string());
        return SpaceEvalOutput {
            pose: Pose::default(),
            root_delta: Vec3::ZERO,
            clip_weights: Vec::new(),
            ground_speed: 0.0,
            issues,
            point,
        };
    }

    // 权重键先解析成采样项（剪辑名 + 播放方向 + 元数据），下面两趟求值共用，
    // 每帧只查一次 ClipSet。
    let plans = build_plans(ctx, &weights);

    // 参考时长：让输出地速严格等于「权重加权的各剪辑地速」。
    //
    // 相位推进速度是 1/T_ref，混合输出的地速 = Σw·S / T_ref。
    // 要把这个值做到 Σw·v（v = S/T），T_ref 必须取 Σw·S / Σw·v；
    // 直接用 Σw·T 只是近似，走跑档之间会偏 8%~9%。
    let weighted: Vec<(f32, f32, f32)> = plans.iter().map(SamplePlan::stride_speed).collect();
    let duration_sum: f32 = plans
        .iter()
        .map(|plan| plan.meta.duration.max(0.0) * plan.weight)
        .sum();
    let ground_speed = blended_ground_speed(&weighted);
    let reference_duration = effective_period(&weighted)
        .or_else(|| (duration_sum > f32::EPSILON).then_some(duration_sum))
        .unwrap_or(1.0);
    let dt = ctx.dt * ctx.speed_scale * doc.speed;
    *ctx.phase = (*ctx.phase + dt / reference_duration).rem_euclid(1.0);
    let phase = *ctx.phase;

    // 统一时间轴：当前权重下的事件加权平均。独占剪辑时它等于该剪辑自己的
    // 事件，重映射退化为恒等（不改变单段动画的节奏）；混合时它落在两者之间，
    // 双方各改一半，四个事件（含支撑期两端）精确对齐。
    let event_sources: Vec<(f32, [Option<f32>; 4])> = plans
        .iter()
        .map(|plan| (plan.weight, plan.events()))
        .collect();
    let reference = average_events(&event_sources);

    let parts: Vec<(f32, Pose)> = plans
        .iter()
        .map(|plan| {
            let time = plan.sample_time(phase, &reference);
            (plan.weight, sample_clip(ctx, &plan.clip, time))
        })
        .collect();

    let pose = Pose::combine(&parts);
    let root_delta = ctx
        .hips
        .and_then(|hips| {
            let bone = pose.get(hips)?;
            let rest = ctx.rest.get(&hips)?;
            Some(bone.translation - rest.translation)
        })
        .unwrap_or(Vec3::ZERO);

    SpaceEvalOutput {
        pose,
        root_delta,
        // 对外显示用剪辑名，不带反转后缀
        clip_weights: plans
            .iter()
            .map(|plan| (plan.clip.clone(), plan.weight))
            .collect(),
        ground_speed,
        issues,
        point,
    }
}

/// 一个参与本次混合的采样项：剪辑 + 播放方向 + 权重 + 已解析的元数据。
struct SamplePlan {
    clip: String,
    reverse: bool,
    weight: f32,
    meta: ClipMeta,
}

impl SamplePlan {
    /// 步态事件按播放方向对齐后的结果：倒放时时间轴整个掉头，相位要跟着镜像。
    fn events(&self) -> [Option<f32>; 4] {
        if self.reverse {
            self.meta
                .gait_events
                .map(|event| event.map(|phase| (1.0 - phase).rem_euclid(1.0)))
        } else {
            self.meta.gait_events
        }
    }

    /// `(净步幅, 时长, 权重)`：等效周期与混合地速都由这三元组算出。
    fn stride_speed(&self) -> (f32, f32, f32) {
        (self.meta.stride, self.meta.duration, self.weight)
    }

    /// 统一相位 → 该剪辑的采样时刻。先把四个步态事件对齐到 `reference`，
    /// 倒放时再从剪辑尾部往头走（后退一类动作）。
    fn sample_time(&self, phase: f32, reference: &[Option<f32>; 4]) -> f32 {
        let duration = self.meta.duration;
        let points = gait_segments(&self.events(), reference);
        let local_phase = warp_phase(phase, &points);
        if self.reverse {
            (duration - local_phase * duration).rem_euclid(duration.max(f32::EPSILON))
        } else {
            local_phase * duration
        }
    }
}

/// 把权重键（可能带 `#rev` 后缀）解析成采样项，顺带把剪辑元数据查出来。
fn build_plans(ctx: &SpaceEvalContext, weights: &[(String, f32)]) -> Vec<SamplePlan> {
    weights
        .iter()
        .map(|(key, weight)| {
            let (clip, reverse) = blendspace::split_key(key);
            SamplePlan {
                clip: clip.to_string(),
                reverse,
                weight: *weight,
                meta: ctx.clips.meta(ctx.clip_assets, clip),
            }
        })
        .collect()
}

fn sample_clip(ctx: &SpaceEvalContext, clip: &str, time: f32) -> Pose {
    let Some(handle) = ctx.clips.handle(clip) else {
        return Pose::default();
    };
    let Some(asset) = ctx.clip_assets.get(handle) else {
        return Pose::default();
    };
    Pose::sample(asset, ctx.targets, ctx.rest, time)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Amy 实测：Walk 1.190m/1.033s，Run 2.114m/0.700s。
    /// 旧做法（加权时长 0.8665s）会把中间档地速算成 1.906，
    /// 新做法必须严格等于线性插值速度 2.086。
    #[test]
    fn equivalent_period_matches_linear_speed() {
        let weighted = [(1.190, 1.033, 0.5), (2.114, 0.700, 0.5)];
        let period = effective_period(&weighted).expect("有步幅时应给出周期");
        let speed = (0.5 * 1.190 + 0.5 * 2.114) / period;
        let expected = 0.5 * (1.190 / 1.033) + 0.5 * (2.114 / 0.700);
        assert!(
            (speed - expected).abs() < 1e-3,
            "输出地速 {speed} 应等于线性插值 {expected}"
        );
        assert!(
            (0.70..0.87).contains(&period),
            "等效周期应落在两段剪辑之间，实际 {period}"
        );
    }

    /// 实测事件：Walking [0.312, 0.781, 0.823, 0.281]，
    /// Running [0.406, 0.625, 0.927, 0.135]。
    /// 重映射后触地与离地都必须精确对齐，且跨周期闭合（+1 相位对应 +1）。
    #[test]
    fn warp_aligns_contact_and_lift() {
        let walk = [Some(0.312), Some(0.781), Some(0.823), Some(0.281)];
        let run = [Some(0.406), Some(0.625), Some(0.927), Some(0.135)];
        let points = gait_segments(&run, &walk);
        assert!(!points.is_empty(), "四个事件应能构造映射");
        assert!((warp_phase(0.312, &points) - 0.406).abs() < 1e-3);
        assert!((warp_phase(0.781, &points) - 0.625).abs() < 1e-3);
        // 闭合：相位多一个周期，本地相位也只多一个周期
        let shifted = warp_phase(0.312 + 1.0, &points);
        assert!((shifted - (0.406 + 1.0)).abs() < 1e-3 || (shifted - 0.406).abs() < 1e-3);
    }

    /// 独占剪辑时统一轴就是它自己，重映射必须退化为恒等，不改原节奏。
    #[test]
    fn average_single_source_is_identity() {
        let walk = [Some(0.312), Some(0.781), Some(0.823), Some(0.281)];
        let reference = average_events(&[(1.0, walk)]);
        let points = gait_segments(&walk, &reference);
        for phase in [0.0, 0.2, 0.5, 0.77, 0.99] {
            assert!((warp_phase(phase, &points) - phase).abs() < 1e-3);
        }
    }

    /// 50/50 混合时统一轴取两者中点，两边各改一半，四个事件都对齐。
    #[test]
    fn average_events_aligns_both_sides() {
        let walk = [Some(0.312), Some(0.781), Some(0.823), Some(0.281)];
        let run = [Some(0.406), Some(0.625), Some(0.927), Some(0.135)];
        let reference = average_events(&[(0.5, walk), (0.5, run)]);
        // 左脚离地：0.781 与 0.625 的中点
        assert!((reference[1].unwrap() - 0.703).abs() < 1e-3);
        // 在统一轴的中点上，两个剪辑都处在各自的事件相位
        let walk_points = gait_segments(&walk, &reference);
        let run_points = gait_segments(&run, &reference);
        assert!((warp_phase(0.359, &walk_points) - 0.312).abs() < 1e-3);
        assert!((warp_phase(0.359, &run_points) - 0.406).abs() < 1e-3);
        assert!((warp_phase(0.703, &walk_points) - 0.781).abs() < 1e-3);
        assert!((warp_phase(0.703, &run_points) - 0.625).abs() < 1e-3);
    }

    /// 环形平均不能被 0/1 边界拉偏：0.95 与 0.05 应该平均到 0.0 附近。
    #[test]
    fn average_events_handles_wrap() {
        let a = [Some(0.95), None, None, None];
        let b = [Some(0.05), None, None, None];
        let reference = average_events(&[(0.5, a), (0.5, b)]);
        let value = reference[0].unwrap();
        assert!(
            value < 1e-3 || value > 0.999,
            "环绕平均应落在 0 附近，实际 {value}"
        );
    }

    /// 事件不足两个时退回恒等映射，不能炸。
    #[test]
    fn warp_without_events_is_identity() {
        let points: Vec<(f32, f32)> = Vec::new();
        assert!((warp_phase(0.42, &points) - 0.42).abs() < 1e-6);
        let sparse = gait_segments(
            &[Some(0.4), None, None, None],
            &[Some(0.3), None, None, None],
        );
        assert!(sparse.is_empty());
    }

    /// 改坐标成「yaw × 带符号 speed」后，规则网格上的插值必须落在预期剪辑上。
    #[test]
    fn yaw_speed_grid_interpolates() {
        use crate::anim::core::blendspace::Sample2D;
        let mut doc = BlendSpaceDoc {
            ranges: Some([[-90.0, 90.0], [-1.0, 1.0]]),
            ..BlendSpaceDoc::default()
        };
        let walk = [
            (-90.0, "LeftStrafeWalking", false),
            (0.0, "Walking", false),
            (90.0, "RightStrafeWalking", false),
        ];
        let run = [
            (-90.0, "LeftStrafe", false),
            (0.0, "Running", false),
            (90.0, "RightStrafe", false),
        ];
        for yaw in [-90.0, 0.0, 90.0] {
            doc.samples.push(Sample2D {
                at: [yaw, 0.0],
                clip: "Idle".to_string(),
                reverse: false,
            });
        }
        // 负速度层＝把同一档剪辑的播放方向翻转（倒退，不是转身）
        for (yaw, clip, reverse) in walk {
            doc.samples.push(Sample2D {
                at: [yaw, 0.5],
                clip: clip.to_string(),
                reverse,
            });
            doc.samples.push(Sample2D {
                at: [yaw, -0.5],
                clip: clip.to_string(),
                reverse: !reverse,
            });
        }
        for (yaw, clip, reverse) in run {
            doc.samples.push(Sample2D {
                at: [yaw, 1.0],
                clip: clip.to_string(),
                reverse,
            });
            doc.samples.push(Sample2D {
                at: [yaw, -1.0],
                clip: clip.to_string(),
                reverse: !reverse,
            });
        }

        let top = |point: [f32; 2]| {
            let mut weights = doc.weights(point);
            weights.sort_by(|a, b| b.1.total_cmp(&a.1));
            weights.into_iter().next().unwrap_or_default()
        };

        // 采样点本身必须独占
        let (key, weight) = top([0.0, 1.0]);
        assert_eq!(key, "Running");
        assert!((weight - 1.0).abs() < 1e-3);
        let (key, weight) = top([0.0, 0.5]);
        assert_eq!(key, "Walking");
        assert!((weight - 1.0).abs() < 1e-3);
        // 正前方向后走 = 倒放走路，不能变成转身
        let (key, _) = top([0.0, -0.5]);
        assert_eq!(key, "Walking#rev");
        let (key, _) = top([0.0, -1.0]);
        assert_eq!(key, "Running#rev");
        // 朝左却倒退 = 实际向右移动，仍然是侧的剪辑被倒放
        let (key, _) = top([-90.0, -1.0]);
        assert_eq!(key, "LeftStrafe#rev");
        // 静止时无论朝向都是 Idle
        for yaw in [-80.0, -45.0, 0.0, 60.0] {
            let (key, _) = top([yaw, 0.0]);
            assert_eq!(key, "Idle", "yaw={yaw} 静止时不该是 {key}");
        }
        // 侧向半速落在左/右横移
        let (key, _) = top([-90.0, 0.5]);
        assert_eq!(key, "LeftStrafeWalking");
        let (key, _) = top([90.0, 0.5]);
        assert_eq!(key, "RightStrafeWalking");
        // 斜向没有素材点，必须由相邻两列合成
        let (key, weight) = top([-45.0, 0.5]);
        assert!(
            key == "Walking" || key == "LeftStrafeWalking",
            "斜向应为两列混合，实际 {key}"
        );
        assert!((weight - 0.5).abs() < 1e-3, "斜向应各半，实际 {weight}");
        // 走↔跑之间是两档同向剪辑的线性混合
        let (key, weight) = top([0.0, 0.75]);
        assert!(
            key == "Walking" || key == "Running",
            "中间档应为走跑混合，实际 {key}"
        );
        assert!((weight - 0.5).abs() < 1e-3, "走跑应各半，实际 {weight}");
    }

    /// 等效地速与等效周期是同一个模型的两面：混合输出的地速必须严格等于
    /// `Σ w·|步幅| / T_ref`，否则按这个值驱动角色位移就会滑步。
    #[test]
    fn blended_speed_matches_period_and_dilutes_with_idle() {
        let walk = (1.190, 1.033);
        let run = (2.114, 0.700);
        let weighted = [(walk.0, walk.1, 0.5), (run.0, run.1, 0.5)];
        let period = effective_period(&weighted).expect("有步幅时应给出周期");
        let by_phase = (0.5 * walk.0 + 0.5 * run.0) / period;
        assert!(
            (blended_ground_speed(&weighted) - by_phase).abs() < 1e-3,
            "地速应与相位推进模型一致"
        );
        // Idle（净步幅 0）占一半权重时，地速减半：档位被稀释是正确行为
        let half = blended_ground_speed(&[(0.0, 8.333, 0.5), (walk.0, walk.1, 0.5)]);
        assert!((half - 0.5 * walk.0 / walk.1).abs() < 1e-4, "实际 {half}");
        // 倒放只改方向，不该让地速变号或变小
        let back = blended_ground_speed(&[(-run.0, run.1, 1.0)]);
        assert!((back - run.0 / run.1).abs() < 1e-4, "实际 {back}");
        assert!(blended_ground_speed(&[(0.0, 8.333, 1.0)]) <= f32::EPSILON);
    }

    /// 纯 Idle（净位移为 0）不能参与速度换算，必须走时长回退。
    #[test]
    fn no_stride_falls_back_to_none() {
        assert!(effective_period(&[(0.0, 8.333, 1.0)]).is_none());
        // 半步幅也要被忽略，否则低速档会把周期算爆
        assert!((effective_period(&[(1.190, 1.033, 1.0)]).unwrap() - 1.033).abs() < 1e-4);
    }

    /// 文档不写 ranges 时按采样点跨度推导，角度轴不会再被当成归一化轴。
    #[test]
    fn missing_ranges_derive_from_samples() {
        let mut doc = BlendSpaceDoc {
            params: ["yaw".to_string(), "speed".to_string()],
            ..BlendSpaceDoc::default()
        };
        doc.samples.push(Sample2D {
            at: [-90.0, -1.0],
            clip: "LeftStrafe".into(),
            reverse: true,
        });
        doc.samples.push(Sample2D {
            at: [90.0, 1.0],
            clip: "RightStrafe".into(),
            reverse: false,
        });

        let yaw = doc.range(0);
        let speed = doc.range(1);
        assert!(
            yaw[0] < -90.0 && yaw[1] > 90.0,
            "yaw 轴应覆盖 ±90，实际 {yaw:?}"
        );
        assert!(
            speed[0] < -1.0 && speed[1] > 1.0,
            "speed 轴应覆盖 ±1，实际 {speed:?}"
        );
        // 归一化余量不该把角度轴压回 ±1
        assert!(yaw[1] > 45.0);
    }

    /// 显式范围太窄时 fit_ranges 会扩张到包住采样点，validate 也能提前报出来。
    #[test]
    fn narrow_ranges_are_fitted_and_reported() {
        let mut doc = BlendSpaceDoc {
            ranges: Some([[-90.0, 90.0], [0.0, 1.0]]),
            ..BlendSpaceDoc::default()
        };
        doc.samples.push(Sample2D {
            at: [0.0, -1.0],
            clip: "Running".into(),
            reverse: true,
        });
        let before = doc.validate();
        assert!(
            before
                .iter()
                .any(|issue| issue.contains("speed") && issue.contains("没包住")),
            "validate 应报出 speed 轴调不到负值，实际 {before:?}"
        );
        assert!(doc.fit_ranges(), "负向采样点越界时应触发扩张");
        assert!(doc.range(1)[0] < -1.0, "扩张后下限应小于 -1");
        assert!(!doc.fit_ranges(), "已包住后不应再触发改动");
        assert!(doc.validate().iter().all(|issue| !issue.contains("没包住")));
    }

    /// ranges 字段可省略、也可原样存取：老文档缺字段不会静默换成别的单位。
    #[test]
    fn ranges_round_trip() {
        let mut doc = BlendSpaceDoc::default();
        doc.samples.push(Sample2D {
            at: [-90.0, 0.0],
            clip: "Idle".into(),
            reverse: false,
        });
        let json = serde_json::to_string(&doc).unwrap();
        assert!(
            !json.contains("ranges"),
            "未指定范围时不该写入 ranges：{json}"
        );
        let back: BlendSpaceDoc = serde_json::from_str(&json).unwrap();
        assert!(back.range(0)[0] < -90.0, "反序列化后应按采样点推导");

        doc.ranges = Some([[-90.0, 90.0], [-1.0, 1.0]]);
        let json = serde_json::to_string(&doc).unwrap();
        let back: BlendSpaceDoc = serde_json::from_str(&json).unwrap();
        assert_eq!(back.range(0), [-90.0, 90.0]);
        assert_eq!(back.range(1), [-1.0, 1.0]);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use bevy::animation::{AnimationClip, AnimationTargetId, animated_field};

    /// 诊断用：按真实骨架规模（169 个 target、3 通道、30 关键帧）量一次
    /// 4 剪辑混合的单帧求值成本。`cargo test -p crab_feast bench_pose -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_pose_sampling() {
        let names: Vec<Name> = (0..169).map(|i| Name::new(format!("Bone{i}"))).collect();
        let ids: Vec<AnimationTargetId> = names.iter().map(AnimationTargetId::from_name).collect();

        let mut clip = AnimationClip::default();
        for (index, id) in ids.iter().enumerate() {
            let base = index as f32 * 0.01;
            let keys = (0..30).map(|k| {
                let t = k as f32 / 29.0;
                (
                    t,
                    Vec3::new(
                        base + (t * std::f32::consts::TAU).sin() * 0.01,
                        base + (t * std::f32::consts::TAU).cos() * 0.01,
                        base,
                    ),
                )
            });
            clip.add_curve_to_target(
                *id,
                AnimatableCurve::new(
                    animated_field!(Transform::translation),
                    AnimatableKeyframeCurve::new(keys).unwrap(),
                ),
            );
            let keys = (0..30).map(|k| {
                let t = k as f32 / 29.0;
                (t, Quat::from_rotation_y(t * std::f32::consts::TAU))
            });
            clip.add_curve_to_target(
                *id,
                AnimatableCurve::new(
                    animated_field!(Transform::rotation),
                    AnimatableKeyframeCurve::new(keys).unwrap(),
                ),
            );
            let keys = (0..30).map(|k| (k as f32 / 29.0, Vec3::ONE * (1.0 + base)));
            clip.add_curve_to_target(
                *id,
                AnimatableCurve::new(
                    animated_field!(Transform::scale),
                    AnimatableKeyframeCurve::new(keys).unwrap(),
                ),
            );
        }

        let rest: HashMap<AnimationTargetId, BonePose> =
            ids.iter().map(|id| (*id, BonePose::IDENTITY)).collect();

        const FRAMES: usize = 600;
        let start = std::time::Instant::now();
        for frame in 0..FRAMES {
            let time = (frame as f32 / FRAMES as f32) * clip.duration();
            let mut poses = Vec::with_capacity(4);
            for _ in 0..4 {
                poses.push((0.25f32, Pose::sample(&clip, &ids, &rest, time)));
            }
            let _ = Pose::combine(&poses);
        }
        let per_frame = start.elapsed().as_secs_f64() * 1e3 / FRAMES as f64;
        println!(
            "4 剪辑 × {} target × 3 通道：{:.3} ms/帧（含 combine），单剪辑约 {:.3} ms",
            ids.len(),
            per_frame,
            per_frame / 4.0
        );
    }
}
