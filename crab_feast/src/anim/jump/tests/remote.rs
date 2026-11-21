use super::super::*;

use super::super::air::GRAVITY;
use super::super::timeline::{BLEND_IN_SECS, Timeline};
fn test_remote_jump(timeline: Timeline) -> RemoteJump {
    RemoteJump {
        timeline,
        ..RemoteJump::default()
    }
}

/// 远端观察：起跳剪辑从离地帧起播、滞空钉在落地帧、触地即进落地段
#[test]
fn remote_jump_aligns_to_observed_trajectory() {
    let timeline = Timeline {
        entry: 0.0,
        takeoff: 0.30,
        land: 0.90,
        end: 1.20,
        take_secs: 0.30,
        air_secs: 0.60,
        recover_secs: 0.30,
        blend_secs: BLEND_IN_SECS,
    };
    let mut jump = test_remote_jump(timeline);
    let setup = |_| {
        Some(RemoteJumpSetup {
            clip: JumpPose::Stand.clip(),
            timeline,
            takeoff_hips: 1.05,
            tuning: JumpPose::Stand.tuning(),
        })
    };

    const GROUND_Y: f32 = 0.70;
    const LAUNCH: f32 = 5.0;
    let dt = 1.0 / 60.0;
    // 先站 0.5s：平地上不该被当成起跳
    let mut t = 0.0;
    for _ in 0..30 {
        jump.observe(dt, GROUND_Y, Some(0.0), setup);
        t += dt;
        assert!(!jump.is_airborne(), "平地被误判起跳");
        assert_eq!(jump.weight(), 0.0);
    }

    // 起跳：y 走抛物线，脚底高度跟着 y 走（时间从离地那一刻重新起算）
    let t0 = t;
    let mut first_air = None;
    let mut last_clip = jump.clip_time();
    let mut landed = None;
    let mut monotone = true;
    for _ in 0..120 {
        t += dt;
        // 抛物线从「离地那一刻」起算
        let air = t - t0;
        let y = GROUND_Y + LAUNCH * air - 0.5 * GRAVITY * air * air;
        let height = Some((y - GROUND_Y).max(0.0));
        jump.observe(dt, y, height, setup);
        if jump.is_airborne() {
            assert_eq!(jump.clip(), JumpPose::Stand.clip());
            let ct = jump.clip_time();
            if ct + 1e-4 < last_clip {
                monotone = false;
            }
            last_clip = ct;
            assert!(ct <= timeline.land + 1e-4, "空中越过落地帧：{ct}");
            first_air.get_or_insert((t - t0, ct));
        } else if first_air.is_some() && landed.is_none() {
            // 触地那一帧必须已经切到落地帧
            assert!(
                (jump.clip_time() - timeline.land).abs() < 1e-3,
                "落地点没对齐：{}",
                jump.clip_time()
            );
            landed = Some(t - t0);
            break;
        }
        if y < GROUND_Y - 0.05 {
            break;
        }
    }
    let (takeoff_t, takeoff_clip) = first_air.expect("应该识别到起跳");
    assert!(monotone, "空中剪辑时间回绕");
    // 动作从离地帧起播（倒推量不超过 0.2s），不能落到蓄力段里
    assert!(
        takeoff_clip >= timeline.takeoff - 1e-4 && takeoff_clip <= timeline.takeoff + 0.2,
        "起跳帧对齐失败：{takeoff_clip}"
    );
    // 识别起跳只比真正离地晚两帧以内（60Hz 下 ≤ 0.05s）
    assert!(takeoff_t <= 0.06, "起跳识别太晚：{takeoff_t}");
    // 落地时刻（相对真正离地）应当就是物理滞空 2v/g ≈ 1.02s，误差 ≲ 两帧
    let landed = landed.expect("应该落回地面");
    let physical_air = 2.0 * LAUNCH / GRAVITY;
    assert!(
        (landed - physical_air).abs() < 0.05,
        "落地识别偏差：{landed} vs {physical_air}"
    );
    // 识别起跳那一刻动作已经倒推了 `h/vy`，起跳帧仍然对得上
    let _ = takeoff_t;

    // 落地恢复段：权重单调淡出到 0
    let mut weight = jump.weight();
    for _ in 0..40 {
        jump.observe(dt, GROUND_Y, Some(0.0), setup);
        let now = jump.weight();
        assert!(now <= weight + 1e-5, "落地权重回升：{weight}→{now}");
        weight = now;
    }
    assert!(weight < 1e-5, "落地恢复没有淡出：{weight}");
}

/// 下坠（竖直速度为负、脚下没有地面）不能播成起跳
#[test]
fn remote_jump_ignores_falling() {
    let mut jump = RemoteJump::default();
    let mut y = 3.0f32;
    for _ in 0..40 {
        let y0 = y;
        y -= 0.08;
        jump.observe(1.0 / 60.0, y, None, |_| {
            unreachable!("下坠不该请求起跳剪辑");
        });
        let _ = y0;
        assert!(!jump.is_airborne());
        assert_eq!(jump.weight(), 0.0);
    }
}
