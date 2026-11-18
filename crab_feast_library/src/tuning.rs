//! 角色运动的调参常量：客户端本地面向逻辑与服务器朝向复制共用同一份。
//!
//! 这些值决定「转身看起来有多重」：单机时由客户端的 `facing_system` 用它做
//! 比例控制 + 角加速度限速；联网后位移和朝向归服务器，服务器必须用同一组值，
//! 否则同一个动作在本地和远端会转出两种节奏。

/// 最大转向角速度（度/秒）。
///
/// 90 是「身体有重量、但不会拖到失控」的节奏：180° 反向约 2.2 秒，90° 约 1.2 秒。
/// 历史值：180°/s、700°/s² → 1.26 秒；30°/s、120°/s² → 6.1 秒（太沉，已否决）；
/// 110°/s、无加速度 → 1.75 秒（拖沓）；450°/s、3000°/s² → 0.63 秒（节奏对但观感偏假）。
pub const TURN_SPEED_DEG_PER_SEC: f32 = 90.0;

/// 最大转向角加速度（度/秒²）。
///
/// 没有它，180° 反向的第一帧角速度就从 0 跳到上限，看着像原地瞬移朝向。
/// 按 180/700 的原始比例跟着 [`TURN_SPEED_DEG_PER_SEC`] 走：起步段恒定
/// 0.26 秒走 11.6°，顶速调快调慢都不改起步形状。
pub const TURN_ACCEL_DEG_PER_SEC2: f32 = 350.0;

/// 误差小于它就算已经对准，避免在阈值附近抖。
pub const TURN_TOLERANCE_DEG: f32 = 2.0;

/// 比例增益（度/秒 每 度误差）：90° 误差时立刻跑满上限，最后十几度平滑收到 0。
///
/// 这个值只影响**收尾**：前半段的快慢由 [`TURN_SPEED_DEG_PER_SEC`] 决定，
/// 觉得「转得拖沓」但增大会显得假时，先调它，别去抬最大角速度。
pub const TURN_RESPONSE: f32 = 10.0;

/// 把 `current` 以 `max_step` 的步长逼近 `target`（不越过）。
pub fn approach(current: f32, target: f32, max_step: f32) -> f32 {
    let delta = target - current;
    if delta.abs() <= max_step {
        target
    } else {
        current + delta.signum() * max_step
    }
}

/// 把角度折到 (-π, π]，用于求「最短转向」的带符号误差。
pub fn wrap_pi(angle: f32) -> f32 {
    (angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

/// 一次比例 + 角加速度限速的转向步进，单机与服务器共用同一套算式。
///
/// 返回 `(新的 yaw, 新的 yaw_rate)`。`error` 是「还需绕 +Y 转多少」，
/// 由调用方按 [`wrap_pi`] 算好。
pub fn step_yaw(yaw: f32, yaw_rate: f32, error: f32, dt: f32) -> (f32, f32) {
    let error_deg = error.to_degrees();
    let desired = if error_deg.abs() <= TURN_TOLERANCE_DEG {
        0.0
    } else {
        error.signum() * (error_deg.abs() * TURN_RESPONSE).min(TURN_SPEED_DEG_PER_SEC)
    };
    let rate = approach(yaw_rate, desired, TURN_ACCEL_DEG_PER_SEC2 * dt)
        .clamp(-TURN_SPEED_DEG_PER_SEC, TURN_SPEED_DEG_PER_SEC);
    // 单帧转角不许超过剩余误差，避免高转速下冲过头再往回摆
    let step = error.signum() * (rate.abs().to_radians() * dt).min(error.abs());
    (wrap_pi(yaw + step), rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定步长把一次转身跑到「进入容差」，返回到位用时（秒）与第一帧转角（度）。
    fn turn_seconds(error_deg: f32, dt: f32) -> (f32, f32) {
        let target = error_deg.to_radians();
        let mut yaw = 0.0;
        let mut rate = 0.0;
        let mut secs = 0.0;
        let mut first = 0.0f32;
        for tick in 0..2000 {
            let error = wrap_pi(target - yaw);
            if error.to_degrees().abs() <= TURN_TOLERANCE_DEG {
                break;
            }
            let (next, next_rate) = step_yaw(yaw, rate, error, dt);
            let delta = wrap_pi(next - yaw).to_degrees().abs();
            assert!(delta <= TURN_SPEED_DEG_PER_SEC * dt + 1e-3, "单帧转了 {delta}°");
            if tick == 0 {
                first = delta;
            }
            yaw = next;
            rate = next_rate;
            secs += dt;
        }
        (secs, first)
    }

    /// 「身体有重量」的手感就是这个数：180° 反向约 2.2 秒、90° 约 1.2 秒。
    /// 顶速没被吃掉（一下子转过去）或加速度丢了（第一帧就满速）都在这里露出来。
    #[test]
    fn reversal_takes_the_designed_time() {
        let (reversal, first) = turn_seconds(180.0, 1.0 / 60.0);
        assert!((2.0..2.5).contains(&reversal), "180° 用了 {reversal}s");
        assert!((1.0..1.5).contains(&turn_seconds(90.0, 1.0 / 60.0).0), "90° 用得太久/太快");
        // 起步段受加速度限制：60 Hz 第一帧只该走 350°/s² × dt² ≈ 0.1°，不是 1.5°
        assert!(first < TURN_SPEED_DEG_PER_SEC / 60.0 / 3.0, "第一帧就到顶速了：{first}°");
    }

    /// 容差内不动：站在目标朝向上不该每帧抖 0.0x 度。
    #[test]
    fn inside_tolerance_holds_still() {
        let (yaw, rate) = step_yaw(0.0, 0.0, 0.5f32.to_radians(), 1.0 / 60.0);
        assert_eq!(yaw, 0.0);
        assert_eq!(rate, 0.0);
    }

    /// 反向转身时角速度要穿过 0 再反向加速，不能一帧翻符号。
    #[test]
    fn reversal_accelerates_through_zero() {
        let mut rate = TURN_SPEED_DEG_PER_SEC;
        let dt = 1.0 / 60.0;
        let (_, first) = step_yaw(1.5, rate, -0.2, dt);
        assert!(first < rate, "角速度没有开始刹车：{rate} → {first}");
        assert!(first > 0.0, "一帧就翻成反向转了：{first}");
        rate = first;
        for _ in 0..30 {
            let (_, next) = step_yaw(1.5, rate, -0.2, dt);
            assert!(next < rate + 1e-3);
            rate = next;
        }
        assert!(rate < 0.0, "刹车太慢，最后没转过来");
    }

    /// 不越过目标：转向途中不许冲过头再用容差抖回来。
    #[test]
    fn never_overshoots_the_target() {
        let target = 2.0f32.to_radians();
        let mut yaw = 0.0;
        let mut rate = TURN_SPEED_DEG_PER_SEC;
        let dt = 1.0 / 60.0;
        for _ in 0..200 {
            let error = wrap_pi(target - yaw);
            let (next, next_rate) = step_yaw(yaw, rate, error, dt);
            assert!(next <= target + 1e-6, "冲过头：{next} > {target}");
            yaw = next;
            rate = next_rate;
        }
        assert!((yaw - target).abs() < 1e-3);
    }
}
