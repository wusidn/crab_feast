use super::super::*;

use super::super::air::{GRAVITY, TAKEOFF_FAIL_SECS, launch_speed, takeoff_failed};

/// 剪辑腾空段要跟真实滞空同步：落地那一刻剪辑的落地帧已经播到
///
/// 实测联网跑跳（5.0 m/s + LinearDamping 0.3）真正滞空 0.95s，
/// RunningJump1 的腾空段（takeoff 0.025 → land 0.658）只有 0.63s，
/// 所以必须慢放；慢放系数由资源表中 `air_predict` 定，预测得比真实长
/// 就会在落地时只播到 82%，画面变成"人落地了腿还在空中迈一步"。
#[test]
fn air_segment_ends_with_the_real_touchdown() {
    let tuning = JumpPose::Run1.tuning();
    let measured_flight = 0.95_f32;
    let air_secs = 0.658_f32 - 0.029_f32;
    let predicted =
        (2.0 * tuning.net_launch_vy / GRAVITY * tuning.air_predict).clamp(air_secs, 60.0);
    let slow = (air_secs / predicted).clamp(tuning.slow_min, 1.0);
    let k = (measured_flight * slow / air_secs).clamp(0.0, 1.0);
    assert!(k >= 0.95, "触地时剪辑腾空段应已播到 95% 以上，实际 {k}");
}

/// 起跳初速不会被贴地接触噪声拉低（噪声 v²/2g 只有毫米级）
#[test]
fn launch_speed_ignores_contact_noise() {
    let stand = JumpPose::Stand.tuning();
    for noise in [-1.0, -0.135, 0.0, 0.028, 0.192, 0.45] {
        assert!(
            (launch_speed(noise, stand) - stand.launch_vy).abs() < 1e-5,
            "噪声 {noise} 不该决定起跳初速"
        );
    }
    // 真的被别的力弹起来（明显高于标定值）才采用实测
    let boosted = stand.launch_vy + 1.5;
    assert!((launch_speed(boosted, stand) - boosted).abs() < 1e-5);
}

#[test]
fn failed_takeoff_only_when_blocked() {
    // 窗口到、根没升、脚没离地、人还贴地 → 这一跳被物理吃掉了
    assert!(takeoff_failed(
        TAKEOFF_FAIL_SECS,
        TAKEOFF_FAIL_SECS,
        false,
        false,
        true
    ));
    // 窗口没到：先别认输，可能只是物理还没把它抬起来
    assert!(!takeoff_failed(0.02, TAKEOFF_FAIL_SECS, false, false, true));
    // 根升起来了 → 真起跳，绝不能提前收尾
    assert!(!takeoff_failed(
        JumpPose::Stand.tuning().max_air_hold,
        TAKEOFF_FAIL_SECS,
        true,
        false,
        true
    ));
    // 脚底抬离过地面 → 真起跳
    assert!(!takeoff_failed(
        JumpPose::Stand.tuning().max_air_hold,
        TAKEOFF_FAIL_SECS,
        false,
        true,
        true
    ));
    // 人已经不在接地状态（掉下高台 / 还在上升）→ 不算失败
    assert!(!takeoff_failed(
        JumpPose::Stand.tuning().max_air_hold,
        TAKEOFF_FAIL_SECS,
        false,
        false,
        false
    ));
}
