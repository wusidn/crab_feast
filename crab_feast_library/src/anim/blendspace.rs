//! 混合空间采样：把参数空间里的一个点换算成「若干剪辑 + 权重」。
//!
//! 1D 用相邻两点线性插值；2D 对齐 UE 的做法，先三角剖分、
//! 再用重心坐标求权重，落在凸包外时投影到最近的三角形。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 一维采样点
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Sample1D {
    pub value: f32,
    pub clip: String,
}

/// 二维采样点：参数坐标 + 剪辑名
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Sample2D {
    pub at: [f32; 2],
    pub clip: String,
    /// 反转播放（倒放剪辑，用来做后退一类的动作）
    #[serde(default)]
    pub reverse: bool,
}

/// 反转播放的键后缀：同一剪辑的正放与倒放各占一条权重，合并时不会互相吃掉
const REVERSE_SUFFIX: &str = "#rev";

/// 采样点在权重里使用的键：反转播放时带上后缀
pub fn playback_key(clip: &str, reverse: bool) -> String {
    if reverse {
        format!("{clip}{REVERSE_SUFFIX}")
    } else {
        clip.to_string()
    }
}

/// 把权重键拆回「剪辑名 + 是否反转」
pub fn split_key(key: &str) -> (&str, bool) {
    match key.strip_suffix(REVERSE_SUFFIX) {
        Some(clip) => (clip, true),
        None => (key, false),
    }
}

/// 合并同名剪辑的权重并归一化
fn merge(weights: Vec<(String, f32)>) -> Vec<(String, f32)> {
    let mut merged: HashMap<String, f32> = HashMap::new();
    let mut total = 0.0;
    for (clip, weight) in weights {
        if weight <= 0.0 {
            continue;
        }
        *merged.entry(clip).or_insert(0.0) += weight;
        total += weight;
    }
    if total <= f32::EPSILON {
        return Vec::new();
    }
    let mut out: Vec<(String, f32)> = merged
        .into_iter()
        .map(|(clip, weight)| (clip, weight / total))
        .collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// 一维采样：返回 (clip, weight)，权重和为 1
pub fn weights_1d(samples: &[Sample1D], value: f32) -> Vec<(String, f32)> {
    if samples.is_empty() {
        return Vec::new();
    }

    let mut sorted: Vec<&Sample1D> = samples.iter().collect();
    sorted.sort_by(|a, b| a.value.partial_cmp(&b.value).unwrap_or(std::cmp::Ordering::Equal));

    if value <= sorted[0].value {
        return merge(vec![(sorted[0].clip.clone(), 1.0)]);
    }
    let last = sorted[sorted.len() - 1];
    if value >= last.value {
        return merge(vec![(last.clip.clone(), 1.0)]);
    }

    for pair in sorted.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if value >= a.value && value <= b.value {
            let span = b.value - a.value;
            let t = if span <= f32::EPSILON {
                0.0
            } else {
                (value - a.value) / span
            };
            return merge(vec![(a.clip.clone(), 1.0 - t), (b.clip.clone(), t)]);
        }
    }

    merge(vec![(last.clip.clone(), 1.0)])
}

fn barycentric(a: [f32; 2], b: [f32; 2], c: [f32; 2], p: [f32; 2]) -> Option<[f32; 3]> {
    let denominator = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
    if denominator.abs() <= f32::EPSILON {
        return None;
    }
    let w0 = ((b[1] - c[1]) * (p[0] - c[0]) + (c[0] - b[0]) * (p[1] - c[1])) / denominator;
    let w1 = ((c[1] - a[1]) * (p[0] - c[0]) + (a[0] - c[0]) * (p[1] - c[1])) / denominator;
    let w2 = 1.0 - w0 - w1;
    Some([w0, w1, w2])
}

fn clamped(weights: [f32; 3]) -> [f32; 3] {
    let w = [
        weights[0].max(0.0),
        weights[1].max(0.0),
        weights[2].max(0.0),
    ];
    let total = w[0] + w[1] + w[2];
    if total <= f32::EPSILON {
        [1.0, 0.0, 0.0]
    } else {
        [w[0] / total, w[1] / total, w[2] / total]
    }
}

/// 2D 采样：三角剖分 + 重心坐标；采样点不足或退化时退回最近邻反距离权重
pub fn weights_2d(samples: &[Sample2D], point: [f32; 2]) -> Vec<(String, f32)> {
    if samples.is_empty() {
        return Vec::new();
    }
    if samples.len() < 3 {
        return inverse_distance(samples, point);
    }

    // 采样点排成规则网格时（典型如「yaw × speed」），按格子做双线性插值。
    // 扇形三角剖分假设点围绕中心分布，对矩形网格会把相邻列错连成斜三角，
    // 使侧向采样点被投影到别的行上。
    if let Some((xs, ys)) = grid_axes(samples) {
        return weights_grid(samples, &xs, &ys, point);
    }

    // 以离中心最近的采样点为扇心，按角度排序后扇形三角剖分。
    // 位移混合空间的采样点通常是「一圈方向 + 可选中心」，这一步等价于三角剖分。
    let centroid = {
        let (mut x, mut y) = (0.0f32, 0.0f32);
        for sample in samples {
            x += sample.at[0];
            y += sample.at[1];
        }
        [x / samples.len() as f32, y / samples.len() as f32]
    };
    let hub = samples
        .iter()
        .enumerate()
        .min_by(|a, b| {
            let da = (a.1.at[0] - centroid[0]).powi(2) + (a.1.at[1] - centroid[1]).powi(2);
            let db = (b.1.at[0] - centroid[0]).powi(2) + (b.1.at[1] - centroid[1]).powi(2);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, _)| index)
        .unwrap_or(0);

    let origin = samples[hub].at;
    let mut ring: Vec<usize> = (0..samples.len()).filter(|index| *index != hub).collect();
    ring.sort_by(|a, b| {
        let angle_a = (samples[*a].at[1] - origin[1]).atan2(samples[*a].at[0] - origin[0]);
        let angle_b = (samples[*b].at[1] - origin[1]).atan2(samples[*b].at[0] - origin[0]);
        angle_a
            .partial_cmp(&angle_b)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut triangles: Vec<(usize, usize, usize)> = Vec::new();
    for index in 0..ring.len() {
        let a = ring[index];
        let b = ring[(index + 1) % ring.len()];
        triangles.push((hub, a, b));
    }

    let mut best: Option<(f32, [f32; 3], usize, usize, usize)> = None;
    for (i0, i1, i2) in triangles {
        let (a, b, c) = (samples[i0].at, samples[i1].at, samples[i2].at);
        let Some(raw) = barycentric(a, b, c, point) else {
            continue;
        };
        if raw[0] >= -1e-5 && raw[1] >= -1e-5 && raw[2] >= -1e-5 {
            let weights = clamped(raw);
            return merge(vec![
                (playback_key(&samples[i0].clip, samples[i0].reverse), weights[0]),
                (playback_key(&samples[i1].clip, samples[i1].reverse), weights[1]),
                (playback_key(&samples[i2].clip, samples[i2].reverse), weights[2]),
            ]);
        }

        let weights = clamped(raw);
        let projected = [
            a[0] * weights[0] + b[0] * weights[1] + c[0] * weights[2],
            a[1] * weights[0] + b[1] * weights[1] + c[1] * weights[2],
        ];
        let distance = (projected[0] - point[0]).powi(2) + (projected[1] - point[1]).powi(2);
        if best
            .as_ref()
            .map(|entry| distance < entry.0)
            .unwrap_or(true)
        {
            best = Some((distance, weights, i0, i1, i2));
        }
    }

    if let Some((_, weights, i0, i1, i2)) = best {
        return merge(vec![
            (playback_key(&samples[i0].clip, samples[i0].reverse), weights[0]),
            (playback_key(&samples[i1].clip, samples[i1].reverse), weights[1]),
            (playback_key(&samples[i2].clip, samples[i2].reverse), weights[2]),
        ]);
    }

    inverse_distance(samples, point)
}

/// 采样点是否排成规则网格：两轴各自去重后，组合数正好等于采样数
fn grid_axes(samples: &[Sample2D]) -> Option<(Vec<f32>, Vec<f32>)> {
    let mut xs: Vec<f32> = samples.iter().map(|sample| sample.at[0]).collect();
    xs.sort_by(|a, b| a.total_cmp(b));
    xs.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
    let mut ys: Vec<f32> = samples.iter().map(|sample| sample.at[1]).collect();
    ys.sort_by(|a, b| a.total_cmp(b));
    ys.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
    if xs.len() * ys.len() != samples.len() || (xs.len() < 2 && ys.len() < 2) {
        return None;
    }
    Some((xs, ys))
}

/// 在排好序的轴上定位：返回 `(左索引, 右索引, 左侧占比)`，超出范围时夹到端点
fn axis_cell(axis: &[f32], value: f32) -> (usize, usize, f32) {
    if axis.len() < 2 {
        return (0, 0, 0.0);
    }
    let mut left = 0usize;
    for index in 0..axis.len() - 1 {
        if value >= axis[index] {
            left = index;
        }
    }
    let span = axis[left + 1] - axis[left];
    let t = if span.abs() <= 1e-6 {
        0.0
    } else {
        ((value - axis[left]) / span).clamp(0.0, 1.0)
    };
    (left, left + 1, t)
}

/// 规则网格上的双线性插值：四角缺哪个就跳过哪个，剩余权重重新归一化
fn weights_grid(
    samples: &[Sample2D],
    xs: &[f32],
    ys: &[f32],
    point: [f32; 2],
) -> Vec<(String, f32)> {
    let find = |x: f32, y: f32| -> Option<&Sample2D> {
        samples.iter().find(|sample| {
            (sample.at[0] - x).abs() < 1e-4 && (sample.at[1] - y).abs() < 1e-4
        })
    };
    let (x0, x1, tx) = axis_cell(xs, point[0]);
    let (y0, y1, ty) = axis_cell(ys, point[1]);
    let corners = [
        (x0, y0, (1.0 - tx) * (1.0 - ty)),
        (x1, y0, tx * (1.0 - ty)),
        (x0, y1, (1.0 - tx) * ty),
        (x1, y1, tx * ty),
    ];
    let mut out: Vec<(String, f32)> = Vec::new();
    for (xi, yi, weight) in corners {
        if weight <= 0.0 {
            continue;
        }
        if let Some(sample) = find(xs[xi], ys[yi]) {
            out.push((playback_key(&sample.clip, sample.reverse), weight));
        }
    }
    merge(out)
}

fn inverse_distance(samples: &[Sample2D], point: [f32; 2]) -> Vec<(String, f32)> {
    let mut nearest: Vec<(f32, String)> = samples
        .iter()
        .map(|sample| {
            let distance =
                (sample.at[0] - point[0]).powi(2) + (sample.at[1] - point[1]).powi(2);
            (distance, playback_key(&sample.clip, sample.reverse))
        })
        .collect();
    nearest.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    nearest.truncate(3);

    let weights: Vec<(String, f32)> = nearest
        .into_iter()
        .map(|(distance, clip)| (clip, 1.0 / (distance + 1e-4)))
        .collect();
    merge(weights)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_dimensional_interpolates() {
        let samples = vec![
            Sample1D {
                value: 0.0,
                clip: "idle".into(),
            },
            Sample1D {
                value: 2.0,
                clip: "walk".into(),
            },
            Sample1D {
                value: 6.0,
                clip: "run".into(),
            },
        ];
        let weights = weights_1d(&samples, 1.0);
        assert_eq!(weights.len(), 2);
        assert!((weights.iter().map(|w| w.1).sum::<f32>() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn two_dimensional_weights_sum_to_one() {
        let samples = vec![
            Sample2D {
                at: [0.0, 1.0],
                clip: "fwd".into(),
                reverse: false,
            },
            Sample2D {
                at: [1.0, 0.0],
                clip: "right".into(),
                reverse: false,
            },
            Sample2D {
                at: [0.0, -1.0],
                clip: "back".into(),
                reverse: false,
            },
            Sample2D {
                at: [-1.0, 0.0],
                clip: "left".into(),
                reverse: false,
            },
            Sample2D {
                at: [0.0, 0.0],
                clip: "idle".into(),
                reverse: false,
            },
        ];
        let weights = weights_2d(&samples, [0.5, 0.5]);
        assert!((weights.iter().map(|w| w.1).sum::<f32>() - 1.0).abs() < 1e-5);
    }
}
