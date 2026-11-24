//! 空中操控与起跳速度：客户端预测、服务端权威、设置面板三方共用的唯一一份规则。
//!
//! 不放在 `net` 模块里：单机 / 移动端不开联网特性也要有空中操控，而这些数值和
//! 判据是**同一个物理规则**——谁复制一份，联机时就会在同一跳上分出两个落点。

/// 起跳竖直初速度默认值（米/秒）。
///
/// 客户端设置面板可以调，**每次上行都快照带上**，服务端按 [`JUMP_LAUNCH_VY_RANGE`]
/// 夹取后使用：跳跃高度是玩法参数，两端必须用同一个数，否则同一跳的高度和滞空
/// 长度都不一样，预测端会被和解一路拽回来。
pub const JUMP_LAUNCH_VY_DEFAULT: f32 = 5.0;
/// 起跳速度的合法区间（米/秒）
pub const JUMP_LAUNCH_VY_RANGE: (f32, f32) = (3.0, 8.0);

/// 空中操控的默认加速度（米/秒²）。
///
/// 空中**不覆盖速度**，只把水平速度转向输入方向：侧向力 `a` 在速度 `v` 上
/// 对应角速度 `a / v`，所以 3 m/s 上 4 m/s² 约 76°/s，一次 0.95s 的滞空最多
/// 转 70° 出头。速度大小严格守恒（松手不刹车、推杆也不加速）。
pub const AIR_ACCEL_DEFAULT: f32 = 4.0;
/// 空中操控加速度的合法区间（米/秒²）。0 = 完全不接管（轨迹锁死）
pub const AIR_ACCEL_RANGE: (f32, f32) = (0.0, 8.0);

/// 起跳后多久停止接管水平速度（秒）。
///
/// 默认 0.75s：按 5.0 m/s 起跳、0.95s 滞空算，等于落地前 0.2s 锁死落点——
/// 最后那一下不许改主意，否则触地判定的位置会被输入抖动带偏。
pub const AIR_STEER_SECS_DEFAULT: f32 = 0.75;
/// 空中接管时长的合法区间（秒）
pub const AIR_STEER_SECS_RANGE: (f32, f32) = (0.0, 1.5);

/// 空中操控：把水平速度**转向**输入方向，角速度受限，速度大小一字不改。
///
/// 这是客户端预测和服务端**共用的唯一一份实现**：两端各写一遍必然会在
/// 角速度 / 归一化上分叉，分叉的后果就是同一跳落点差一截，和解系统一直拉人。
///
/// 为什么是「纯旋转」而不是「把速度向量直线拉向目标方向」：后者走的是弦，
/// 在 90° 换向时每一 tick 都会把速度大小磨掉约 1.6%，0.2 秒就掉两成——空中
/// 转个向等于把动量白送出去，落点反而更难预判。这里只转方向，模长严格守恒。
///
/// `accel` 是「空中操控加速度」（米/秒²，玩家可调）：侧向力 `a` 在速度 `v` 上的
/// 角速度就是 `a / v`，所以 3 m/s 上 4 m/s² ≈ 76°/s。速度越小转得越快（力一样，
/// 圆半径更小），这符合直觉，也不会出现「慢速时转向被卡死」。
///
/// - `accel <= 0`、`dt <= 0`、速度为 0、输入方向为 0 时原样返回；
/// - 转角一次最多转 `accel / speed * dt` 弧度，绝不瞬移。
pub fn air_steer_velocity(
    velocity: [f32; 2],
    direction: [f32; 2],
    accel: f32,
    dt: f32,
) -> [f32; 2] {
    let speed = (velocity[0] * velocity[0] + velocity[1] * velocity[1]).sqrt();
    let dir_len = (direction[0] * direction[0] + direction[1] * direction[1]).sqrt();
    if accel <= 0.0 || dt <= 0.0 || speed <= 1e-4 || dir_len <= 1e-6 {
        return velocity;
    }
    // 有符号夹角：从当前速度转到输入方向要走的最短弧
    let angle = (velocity[0] * direction[1] - velocity[1] * direction[0])
        .atan2(velocity[0] * direction[0] + velocity[1] * direction[1]);
    // 这一帧最多能转多少弧度。上限夹到 π，避免速度极小时一步转半圈以上
    let max_step = (accel / speed * dt).min(core::f32::consts::PI);
    let step = angle.clamp(-max_step, max_step);
    let (sin, cos) = step.sin_cos();
    [
        velocity[0] * cos - velocity[1] * sin,
        velocity[0] * sin + velocity[1] * cos,
    ]
}

/// 空中是否还该接管水平速度：起跳后 `steer_secs` 之内才有操控。
///
/// 叫「落地前锁死」也行：默认参数下滞空约 0.95s，`steer_secs = 0.75`，
/// 剩下 0.2s 落点已经定死。用「起跳后经过多久」而不是「距落地还有多久」，
/// 是因为服务端只能数 tick，算不出未来的接地时刻。
pub fn air_steer_active(air_elapsed: f32, steer_secs: f32) -> bool {
    air_elapsed < steer_secs
}
#[cfg(test)]
mod tests {
    use super::*;

    fn speed_of(v: [f32; 2]) -> f32 {
        (v[0] * v[0] + v[1] * v[1]).sqrt()
    }

    /// 只转方向，不改大小：连续转 90° 也不许掉速
    ///
    /// 这条专门守住「保动量」：旧实现是直线逼近目标速度（走弦），90° 换向时
    /// 每 tick 会把速度磨掉 1.6%，实测 0.2 秒丢了 23%——空中转个向就没速度了。
    #[test]
    fn rotation_preserves_the_momentum() {
        let dt = 1.0 / 60.0;
        let mut v = [3.0, 0.0];
        for _ in 0..(60 * 2) {
            v = air_steer_velocity(v, [0.0, 1.0], 4.0, dt);
            assert!((speed_of(v) - 3.0).abs() < 1e-4, "转向过程掉速了：{:?}", v);
        }
    }

    /// 一 tick 的角度上限：3 m/s 上 4 m/s² 约 1.27°（≈76°/s）
    #[test]
    fn turn_rate_is_capped_by_accel_over_speed() {
        let dt = 1.0 / 60.0;
        let v = air_steer_velocity([3.0, 0.0], [0.0, 1.0], 4.0, dt);
        let angle = v[1].atan2(v[0]).to_degrees();
        assert!(
            (angle - 1.27).abs() < 0.05,
            "一 tick 该转 4/3/60 弧度（1.27°），实际 {angle}"
        );
    }

    /// 推着同一个方向不放，最终会严丝合缝地转到输入方向上
    #[test]
    fn keeps_steering_until_aligned() {
        let dt = 1.0 / 60.0;
        let mut v = [3.0, 0.0];
        for _ in 0..(60 * 3) {
            v = air_steer_velocity(v, [0.0, 1.0], 4.0, dt);
        }
        assert!(v[0].abs() < 1e-3, "横向分量该收敛到 0：{}", v[0]);
        assert!((v[1] - 3.0).abs() < 1e-3, "纵向该等于原速：{}", v[1]);
    }

    /// 已经对准输入方向：再推也不动（不加速、不抖动）
    #[test]
    fn aligned_input_is_a_no_op() {
        let v = air_steer_velocity([0.0, 3.0], [0.0, 1.0], 4.0, 1.0 / 60.0);
        assert_eq!(v, [0.0, 3.0]);
    }

    /// accel = 0 = 完全不接管：轨迹原样保留（设置面板滑到 0 就是「锁死落点」）
    #[test]
    fn zero_accel_keeps_the_trajectory() {
        assert_eq!(
            air_steer_velocity([3.0, 0.0], [0.0, 1.0], 0.0, 0.016),
            [3.0, 0.0]
        );
    }

    /// 速度为 0（原地跳）：没有动量可转，推杆也不能凭空给速度
    #[test]
    fn zero_speed_cannot_steer_into_motion() {
        assert_eq!(
            air_steer_velocity([0.0, 0.0], [0.0, 1.0], 4.0, 0.016),
            [0.0, 0.0]
        );
    }

    /// 接管窗口是按「起跳后经过多久」算的，过了就锁死
    #[test]
    fn steer_window_closes_after_the_deadline() {
        assert!(air_steer_active(0.5, 0.75));
        assert!(!air_steer_active(0.75, 0.75));
        assert!(!air_steer_active(0.9, 0.75));
    }
}
