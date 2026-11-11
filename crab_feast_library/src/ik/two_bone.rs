//! 两骨解析 IK（髋-膝-踝、肩-肘-腕这类三关节链）。
//!
//! 纯函数，不碰 ECS、不碰骨骼命名与局部轴向：输入三个关节的**世界位置**和目标点，
//! 输出「中间关节该落在哪、两段骨骼该指向哪」。把方向换算成局部旋转是调用方的事，
//! 所以同一套求解器可以直接用在任意角色身上，包括联机时每个可见角色自己的骨架。
//!
//! 求解顺序（余弦定理的常规做法）：
//! 1. 先把目标距离夹进 `[|a-b|, a+b]`，够不着就落在最大伸展处，而不是让腿断开；
//! 2. 用当前膝盖相对「髋→踝」轴的偏移当极点，保证膝盖仍然朝动画原本的方向弯；
//! 3. 由余弦定理算出上段骨骼与轴的夹角，中间关节 = 髋 + 上段方向 × 上段长度。

use bevy::math::{Quat, Vec3};

/// 骨长小于它就认为退化（米）
const MIN_LENGTH: f32 = 1e-5;
/// 可达范围两端留出的保护量，避免目标正好落在完全伸直/完全折叠的奇点上
const REACH_EPSILON: f32 = 1e-3;

/// 一次两骨求解的结果。全部是**世界空间**的量，调用方负责转成局部旋转。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TwoBoneSolution {
    /// 求解后的中间关节（膝盖/手肘）世界位置
    pub joint: Vec3,
    /// 实际到达的末端位置：目标超出可达范围时会被夹在最大伸展处
    pub end: Vec3,
    /// 上段骨骼应当指向的世界方向（单位向量）
    pub upper_dir: Vec3,
    /// 下段骨骼应当指向的世界方向（单位向量）
    pub lower_dir: Vec3,
    /// 目标是否因为够不着而被夹住
    pub clamped: bool,
    /// 伸展比例：0 = 完全折叠，1 = 完全伸直
    pub extension: f32,
}

/// 求解一条两骨链。
///
/// - `root` / `joint` / `end`：当前姿态下三个关节的世界位置（顺序即父子关系）；
/// - `target`：末端希望到达的世界位置；
/// - `pole_hint`：中间关节应当偏向的方向（世界空间，无需归一化、无需垂直于轴）。
///   传「当前中间关节 - root」就能保住动画原本的弯曲方向。
///
/// 骨长为零、输入含 NaN、或目标与根重合时返回 `None`：调用方应当原样跳过本帧，
/// 不要把 `None` 当成「解在原点」。
pub fn solve_two_bone(
    root: Vec3,
    joint: Vec3,
    end: Vec3,
    target: Vec3,
    pole_hint: Vec3,
) -> Option<TwoBoneSolution> {
    if !root.is_finite() || !joint.is_finite() || !end.is_finite() || !target.is_finite() {
        return None;
    }

    let upper_len = (joint - root).length();
    let lower_len = (end - joint).length();
    if upper_len <= MIN_LENGTH || lower_len <= MIN_LENGTH {
        return None;
    }

    let to_target = target - root;
    let raw_distance = to_target.length();
    if raw_distance <= MIN_LENGTH {
        return None;
    }

    // 可达范围：完全折叠 / 完全伸直，两端都留一点余量，避免余弦定理在边界上除零
    let min_reach = ((upper_len - lower_len).abs() + REACH_EPSILON).max(REACH_EPSILON);
    let max_reach = (upper_len + lower_len - REACH_EPSILON).max(min_reach);
    let distance = raw_distance.clamp(min_reach, max_reach);
    let clamped = (distance - raw_distance).abs() > 1e-6;

    let axis = to_target / raw_distance;
    let pole = stable_pole(axis, pole_hint, joint - root);

    // 余弦定理：上段骨骼与「髋→目标」方向的夹角
    let cos_upper = ((upper_len * upper_len + distance * distance - lower_len * lower_len)
        / (2.0 * upper_len * distance))
        .clamp(-1.0, 1.0);
    let sin_upper = (1.0 - cos_upper * cos_upper).max(0.0).sqrt();

    // 极点在垂直于轴的平面内，所以这两个分量正交，加起来天然是单位向量
    let upper_dir = (axis * cos_upper + pole * sin_upper).normalize_or_zero();
    if upper_dir == Vec3::ZERO {
        return None;
    }
    let solved_joint = root + upper_dir * upper_len;
    let solved_end = root + axis * distance;
    let lower_dir = (solved_end - solved_joint).normalize_or_zero();
    if lower_dir == Vec3::ZERO {
        return None;
    }

    Some(TwoBoneSolution {
        joint: solved_joint,
        end: solved_end,
        upper_dir,
        lower_dir,
        clamped,
        extension: (distance / (upper_len + lower_len)).clamp(0.0, 1.0),
    })
}

/// 极点要垂直于「根→目标」轴；极点退化时依次退回当前关节偏移、任意正交向量。
fn stable_pole(axis: Vec3, pole_hint: Vec3, current_joint: Vec3) -> Vec3 {
    let candidate = pole_hint - axis * pole_hint.dot(axis);
    if candidate.length_squared() > 1e-6 {
        return candidate.normalize();
    }
    let current = current_joint - axis * current_joint.dot(axis);
    if current.length_squared() > 1e-6 {
        return current.normalize();
    }
    // 膝盖正好压在轴上（完全伸直）：任意挑一个垂直方向，至少不会出 NaN
    axis.any_orthonormal_vector()
}

/// 把骨骼的世界旋转「瞄准」到目标方向：只加一个最小弧旋转，保留原有扭转（roll）。
///
/// `current_dir` / `desired_dir` 都是**骨骼原点 → 子骨骼原点**的世界向量，
/// 不假设骨骼局部 +Y 是骨轴：换一套命名或轴向约定也能用。
pub fn aim_world_rotation(current: Quat, current_dir: Vec3, desired_dir: Vec3) -> Quat {
    if current_dir.length_squared() <= MIN_LENGTH * MIN_LENGTH
        || desired_dir.length_squared() <= MIN_LENGTH * MIN_LENGTH
    {
        return current;
    }
    let from = current_dir.normalize();
    let to = desired_dir.normalize();
    (Quat::from_rotation_arc(from, to) * current).normalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 居中不动的链：膝在正前方，目标是原位置时解必须等于输入
    fn sample_chain() -> (Vec3, Vec3, Vec3) {
        let root = Vec3::new(0.0, 1.0, 0.0);
        let joint = Vec3::new(0.0, 0.45, 0.2);
        let end = Vec3::new(0.0, 0.0, 0.0);
        (root, joint, end)
    }

    #[test]
    fn identity_target_keeps_the_pose() {
        let (root, joint, end) = sample_chain();
        let solution = solve_two_bone(root, joint, end, end, joint - root).expect("应当有解");
        assert!((solution.joint - joint).length() < 1e-4, "{:?}", solution.joint);
        assert!((solution.end - end).length() < 1e-4, "{:?}", solution.end);
        assert!(!solution.clamped);
    }

    #[test]
    fn target_is_reached_within_leg_reach() {
        let (root, joint, end) = sample_chain();
        let target = end + Vec3::new(0.12, 0.0, 0.0);
        let solution = solve_two_bone(root, joint, end, target, joint - root).expect("应当有解");
        // 三段必须首尾相接，且末端落在目标上
        assert!(
            ((solution.joint - root).length() - (joint - root).length()).abs() < 1e-4,
            "上段长度被改了"
        );
        assert!(
            ((solution.joint - solution.end).length() - (end - joint).length()).abs() < 1e-4,
            "下段长度被改了"
        );
        assert!((solution.end - target).length() < 1e-4, "{:?}", solution.end);
    }

    #[test]
    fn unreachable_target_is_clamped_not_exploded() {
        let (root, joint, end) = sample_chain();
        let upper = (joint - root).length();
        let lower = (end - joint).length();
        let far = end + Vec3::new(0.0, 0.0, 5.0);
        let solution = solve_two_bone(root, joint, end, far, joint - root).expect("应当有解");
        let reach = (solution.end - root).length();
        assert!(solution.clamped);
        assert!(reach <= upper + lower + 1e-3, "伸过头了：{reach}");
        assert!(reach >= upper + lower - 2e-3, "应落在最大伸展处：{reach}");
    }

    #[test]
    fn degenerate_chain_returns_none() {
        let p = Vec3::ZERO;
        assert!(solve_two_bone(p, p, p, Vec3::X, Vec3::Z).is_none());
        let nan = Vec3::new(f32::NAN, 0.0, 0.0);
        assert!(solve_two_bone(p, Vec3::Y, Vec3::ZERO, nan, Vec3::Z).is_none());
    }

    #[test]
    fn knee_keeps_bending_towards_the_pole() {
        let (root, joint, end) = sample_chain();
        let target = end + Vec3::new(0.05, 0.0, 0.0);
        let solution = solve_two_bone(root, joint, end, target, joint - root).expect("应当有解");
        // 原来的膝盖朝 +Z，解出来的仍然要朝 +Z
        assert!(solution.joint.z > 0.0, "{:?}", solution.joint);
    }

    #[test]
    fn aim_rotation_preserves_reach_and_twist() {
        // 上段骨骼原方向是 -Y，绕 X 轴转 30° 后应精确指到新方向
        let current = Quat::IDENTITY;
        let from = Vec3::NEG_Y;
        let to = Quat::from_rotation_x(0.5) * from;
        let aimed = aim_world_rotation(current, from, to);
        let mapped = aimed * from;
        assert!((mapped - to).length() < 1e-5, "{mapped:?} vs {to:?}");
        assert!((aimed.length() - 1.0).abs() < 1e-5);
    }
}
