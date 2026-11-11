//! 混合空间采样：把参数空间里的一个点换算成「若干剪辑 + 权重」。
//!
//! 对齐 Unreal 的 Blend Space 2D：先三角剖分、再用重心坐标求权重，
//! 落在凸包外时投影到最近的三角形；规则网格走双线性插值。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

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

/// 2D 采样：布局与算法解耦——怎么摆点都可以。
///
/// - **（可能缺格的）规则网格** → 双线性插值，单元格缺角就跳过再归一化；
/// - **任意其它布局** → 反距离权重（见 [`local_distance_weights`](#fn-local_distance_weights)）。
///
/// 这里不做三角剖分。以前用「以离质心最近的采样点为扇心的扇形剖分」，它隐含假设
/// 采样点绕中心呈星形分布：网格缺角、或有点与扇心共线时，三角形会退化被跳过，
/// 于是同一份数据在 yaw 为正的一侧走一条路径、为负的一侧走另一条，速度和档位
/// 都静默跑偏。反距离权重对采样点的位置和数量没有任何要求——共线、重叠、
/// 缺角、单点都能算，镜像坐标得到镜像权重。
pub fn weights_2d(samples: &[Sample2D], point: [f32; 2]) -> Vec<(String, f32)> {
    if samples.is_empty() {
        return Vec::new();
    }

    // 规则网格（典型如「yaw × speed」）按格子做双线性插值：局部、精确、可预测，
    // 也是「四个角各占多少」这种直觉唯一成立的情形
    if let Some((xs, ys)) = grid_axes(samples) {
        let weights = weights_grid(samples, &xs, &ys, point);
        // 整个单元格四角全缺（编辑器把一行/一列整排删了）时双线性给不出任何权重，
        // 再退回距离权重——否则这一片参数区域会「没有可用的采样点」，姿态直接空掉
        if !weights.is_empty() {
            return weights;
        }
    }

    local_distance_weights(samples, point)
}

/// 不规则布局：按 `1/距离²` 加权，只纳入**两倍最近距离**以内的采样点。
///
/// 半径只取最近点的两倍，是为了让权重保持局部：一圈方向点 + 中心点时，
/// 22.5° 那种落在两个方向中间的位置只会吃到相邻两个方向的权重（邻居到它的
/// 距离是 0.39、中心的 1.0 已经出了半径），和三角剖分的直觉一致；
/// 而真正相隔很远的采样点不会隔着大半个参数空间来掺一脚。
///
/// 恰好落在某个采样点上时该点独占（距离为 0，权重发散），否则按距离衰减。
fn local_distance_weights(samples: &[Sample2D], point: [f32; 2]) -> Vec<(String, f32)> {
    let distance_sq = |sample: &Sample2D| {
        (sample.at[0] - point[0]).powi(2) + (sample.at[1] - point[1]).powi(2)
    };
    let nearest = samples
        .iter()
        .map(distance_sq)
        .fold(f32::INFINITY, f32::min);

    // 正好压在采样点上：独占，避免 1/0
    if nearest <= 1e-12 {
        return merge(
            samples
                .iter()
                .filter(|sample| distance_sq(sample) <= 1e-12)
                .map(|sample| (playback_key(&sample.clip, sample.reverse), 1.0))
                .collect(),
        );
    }

    // 半径按平方保存，省一次开方：nearest 已经是距离平方，两倍距离即 4 倍平方
    let radius_sq = nearest * 4.0;
    let weighted: Vec<(String, f32)> = samples
        .iter()
        .filter_map(|sample| {
            let d2 = distance_sq(sample);
            (d2 <= radius_sq).then(|| {
                (
                    playback_key(&sample.clip, sample.reverse),
                    1.0 / d2.max(f32::MIN_POSITIVE),
                )
            })
        })
        .collect();
    merge(weighted)
}

/// 采样点是否排成规则网格：两轴各自去重后，组合数正好等于采样数。
///
/// 允许**缺格**：编辑器里删掉几个采样点（典型如把 speed=0 行的转向点删了）之后，
/// 网格仍然成立，`weights_grid` 会把缺掉的角跳过再归一化。以前这里要求
/// `组合数 == 采样数`，缺一个点就退回扇形三角剖分，结果整片区域被投影到
/// 错误的三角形上——yaw 为正的那半边会混进 Idle / Walking，速度莫名其妙掉一半，
/// 而 yaw 为负的那半边一切正常（因为扇心在 Idle，剖分天然不对称）。
///
/// 覆盖率下限 0.6 用来把「环形/扇形布局」挡在外面：一圈 8 个方向点去重后
/// 组合数远大于采样数，覆盖率很低，仍然走反距离权重。
fn grid_axes(samples: &[Sample2D]) -> Option<(Vec<f32>, Vec<f32>)> {
    let mut xs: Vec<f32> = samples.iter().map(|sample| sample.at[0]).collect();
    xs.sort_by(|a, b| a.total_cmp(b));
    xs.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
    let mut ys: Vec<f32> = samples.iter().map(|sample| sample.at[1]).collect();
    ys.sort_by(|a, b| a.total_cmp(b));
    ys.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
    if xs.len() < 2 || ys.len() < 2 {
        return None;
    }
    let cells = xs.len() * ys.len();
    if cells < samples.len() {
        return None;
    }
    let coverage = samples.len() as f32 / cells as f32;
    if cells == samples.len() || coverage >= 0.6 {
        Some((xs, ys))
    } else {
        None
    }
}

/// 在排好序的轴上定位：返回 `(左索引, 右索引, 左侧占比)`，超出范围时夹到端点
fn axis_cell(axis: &[f32], value: f32) -> (usize, usize, f32) {
    if axis.len() < 2 {
        return (0, 0, 0.0);
    }
    // 取最后一个起点 ≤ value 的格子；整条轴都在 value 之后时退回第一格
    let left = axis
        .iter()
        .take(axis.len() - 1)
        .rposition(|&start| value >= start)
        .unwrap_or(0);
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
        samples
            .iter()
            .find(|sample| (sample.at[0] - x).abs() < 1e-4 && (sample.at[1] - y).abs() < 1e-4)
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// 采样点随便摆也行：布局不构成网格时走反距离权重，镜像坐标必须得到镜像权重。
    ///
    /// 这是「任意位置、任意数量」的底线要求：以前扇形剖分对这种散乱布局会
    /// 一侧落在三角形内、另一侧落在三角形外，权重根本不对称。
    #[test]
    fn scattered_layout_is_mirror_symmetric() {
        let layout: [(f32, f32, &str); 7] = [
            (-90.0, 1.0, "left"),
            (90.0, 1.0, "right"),
            (-90.0, -1.0, "left_back"),
            (90.0, -1.0, "right_back"),
            (-30.0, 0.4, "left_in"),
            (30.0, 0.4, "right_in"),
            (0.0, 0.0, "idle"),
        ];
        fn mirror(clip: &str) -> &str {
            match clip {
                "left" => "right",
                "left_in" => "right_in",
                "left_back" => "right_back",
                "right" => "left",
                "right_in" => "left_in",
                "right_back" => "left_back",
                other => other,
            }
        }
        let samples: Vec<Sample2D> = layout
            .iter()
            .map(|(x, y, clip)| Sample2D {
                at: [*x, *y],
                clip: clip.to_string(),
                reverse: false,
            })
            .collect();

        for point in [[12.0, 0.6], [60.0, 0.3], [-12.0, 0.8], [0.0, -0.6]] {
            let mut a = weights_2d(&samples, point);
            let mut b = weights_2d(&samples, [-point[0], point[1]]);
            a.sort_by(|x, y| x.0.cmp(&y.0));
            b.sort_by(|x, y| x.0.cmp(&y.0));
            assert_eq!(a.len(), b.len(), "{point:?}: {a:?} vs {b:?}");
            for ((clip_a, weight_a), (clip_b, weight_b)) in a.iter().zip(&b) {
                assert_eq!(mirror(clip_a), clip_b, "{point:?}: {a:?} vs {b:?}");
                assert!(
                    (weight_a - weight_b).abs() < 1e-5,
                    "{point:?}: {a:?} vs {b:?}"
                );
            }
        }
    }

    /// 一圈方向点 + 中心点：方向之间只该吃到相邻两个方向，中心点不该隔着半径掺进来。
    #[test]
    fn ring_layout_blends_two_neighbours() {
        let samples: Vec<Sample2D> = (0..8)
            .map(|step| {
                let angle = step as f32 * std::f32::consts::TAU / 8.0;
                Sample2D {
                    at: [angle.cos(), angle.sin()],
                    clip: format!("dir{step}"),
                    reverse: false,
                }
            })
            .chain(std::iter::once(Sample2D {
                at: [0.0, 0.0],
                clip: "idle".to_string(),
                reverse: false,
            }))
            .collect();

        // 22.5° 落在 dir0 与 dir1 正中间
        let angle = std::f32::consts::TAU / 16.0;
        let weights = weights_2d(&samples, [angle.cos(), angle.sin()]);
        assert_eq!(weights.len(), 2, "实际 {weights:?}");
        assert!(weights.iter().all(|(clip, _)| clip != "idle"));
        assert!((weights[0].1 - weights[1].1).abs() < 1e-4);
    }

    /// 网格里整格缺角：双线性拿不到四角，必须退回距离权重，不能整片空掉。
    #[test]
    fn empty_grid_cell_falls_back_to_distance_weights() {
        let mut samples = Vec::new();
        for x in 0..4 {
            for y in 0..4 {
                // 挖掉中间那一格 (1..2, 1..2) 的两个整数角
                if (x, y) == (1, 1) || (x, y) == (2, 1) || (x, y) == (1, 2) || (x, y) == (2, 2) {
                    continue;
                }
                samples.push(Sample2D {
                    at: [x as f32, y as f32],
                    clip: format!("c{x}_{y}"),
                    reverse: false,
                });
            }
        }
        let weights = weights_2d(&samples, [1.5, 1.5]);
        assert!(!weights.is_empty(), "空格子不该返回空权重");
        assert!((weights.iter().map(|w| w.1).sum::<f32>() - 1.0).abs() < 1e-5);
    }

    /// 缺格不算破格：删掉 speed=0 行的两个转向点后，仍要按网格插值。
    ///
    /// 回归用例——Amy 的 locomotion.json 曾经只有 13 个点（缺 (±90, 0)），
    /// 网格检测失败退回扇形剖分，正 yaw 一侧整片混进 Walking/Idle，速度掉一半。
    #[test]
    fn grid_with_missing_cells_still_interpolates() {
        let rows: [f32; 5] = [-1.0, -0.5, 0.0, 0.5, 1.0];
        let cols: [f32; 3] = [-90.0, 0.0, 90.0];
        let mut samples = Vec::new();
        for y in rows {
            for x in cols {
                // 只缺 (±90, 0) 这两个角，其余照旧
                if (x.abs() - 90.0).abs() < 1e-6 && y.abs() < 1e-6 {
                    continue;
                }
                // 正前方这一列按档位分开命名，才能看出插值有没有跨行跳档
                let clip = if x < 0.0 {
                    format!("left_{y}")
                } else if x > 0.0 {
                    format!("right_{y}")
                } else {
                    // 正前方这一列按档位各自命名，才能看出插值有没有跨行跳档
                    format!("front_{y}")
                };
                samples.push(Sample2D {
                    at: [x, y],
                    clip,
                    reverse: y < 0.0,
                });
            }
        }
        assert_eq!(samples.len(), 13);

        // yaw 正负两侧必须对称：+2° 与 -2° 都应当几乎完全是正前方的跑档剪辑，
        // 而不是一侧 Running、另一侧 Walking
        let positive = weights_2d(&samples, [2.0, 1.0]);
        let negative = weights_2d(&samples, [-2.0, 1.0]);
        let top = |list: &[(String, f32)]| {
            list.iter()
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                .map(|(clip, _)| clip.clone())
                .unwrap()
        };
        assert_eq!(top(&positive), "front_1", "实际 {positive:?}");
        assert_eq!(top(&negative), "front_1", "实际 {negative:?}");

        // 网格中央的走档点不能被跨行跳过：0.75 必须是 走档 / 跑档 各一半
        let mid = weights_2d(&samples, [0.0, 0.75]);
        let mut sorted = mid.clone();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        assert_eq!(sorted.len(), 2, "实际 {mid:?}");
        assert!((sorted[0].1 - 0.5).abs() < 1e-4, "实际 {mid:?}");
    }
}
