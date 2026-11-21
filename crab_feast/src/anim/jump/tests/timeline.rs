use super::super::*;

use super::super::drive::root_y_at;
use super::super::timeline::{BLEND_IN_SECS, CURVE_HZ, MIN_BLEND_SECS, Timeline, detect_events};

fn build_timeline(spec: &JumpClipSpec) -> Timeline {
    Timeline::build(spec, &[], 1.0 / CURVE_HZ, spec.end)
}

/// 各档锚点必须精确落在实测时间上
#[test]
fn timeline_hits_measured_anchors() {
    for pose in JumpPose::ALL {
        let spec = *pose.spec();
        let tl = build_timeline(&spec);
        assert!((tl.clip_time(JumpState::Takeoff, 0.0, 0.0, 0.0, 1.0) - tl.entry).abs() < 1e-4);
        assert!(
            (tl.clip_time(JumpState::Takeoff, tl.take_secs, 0.0, 0.0, 1.0) - tl.takeoff).abs()
                < 1e-4
        );
        assert!((tl.clip_time(JumpState::Air, 0.0, 0.0, 0.0, 1.0) - tl.takeoff).abs() < 1e-4);
        assert!(
            (tl.clip_time(JumpState::Land, tl.total_secs(), 0.0, tl.recover_secs, 1.0) - tl.end)
                .abs()
                < 1e-3
        );
    }
}

/// 蓄力段与落地恢复段剪辑时间单调，否则姿态会倒着走
#[test]
fn takeoff_and_land_segments_monotonic() {
    for pose in JumpPose::ALL {
        let spec = *pose.spec();
        let tl = build_timeline(&spec);
        let mut last = tl.clip_time(JumpState::Takeoff, 0.0, 0.0, 0.0, 1.0);
        for i in 1..=50 {
            let e = tl.take_secs * i as f32 / 50.0;
            let now = tl.clip_time(JumpState::Takeoff, e, 0.0, 0.0, 1.0);
            assert!(now >= last - 1e-5, "takeoff {e}: {now} < {last}");
            last = now;
        }
        for i in 1..=50 {
            let l = tl.recover_secs * i as f32 / 50.0;
            let now = tl.clip_time(JumpState::Land, tl.total_secs(), 0.0, l, 1.0);
            assert!(now >= last - 1e-5, "land {l}: {now} < {last}");
            last = now;
        }
    }
}

/// 空中：整段 takeoff→land 单调推进、不回绕；慢放系数越小推进越慢；
/// 滞空超过剪辑空中段后钉在落地帧，等接地信号。
#[test]
fn air_plays_full_clip_slowed_without_wrap() {
    for pose in JumpPose::ALL {
        let spec = *pose.spec();
        let tl = build_timeline(&spec);
        let mut last = tl.clip_time(JumpState::Air, 0.0, 0.0, 0.0, 1.0);
        for i in 1..=50 {
            let a = tl.air_secs * i as f32 / 50.0;
            let now = tl.clip_time(JumpState::Air, 0.0, a, 0.0, 1.0);
            assert!(now >= last - 1e-5, "air {a}: {now} < {last}");
            assert!(now <= tl.land + 1e-4, "air {a}: {now} > land");
            last = now;
        }
        // 超长滞空钉在落地帧，不回绕
        let parked = tl.clip_time(JumpState::Air, 0.0, 5.0, 0.0, 1.0);
        assert!((parked - tl.land).abs() < 1e-4, "parked: {parked}");
        // 慢放 0.5：同样 air_time 只走一半剪辑进度
        let half = tl.clip_time(JumpState::Air, 0.0, tl.air_secs, 0.0, 0.5);
        let mid = tl.takeoff + 0.5 * (tl.land - tl.takeoff);
        assert!((half - mid).abs() < 1e-4, "slow half: {half} vs {mid}");
    }
}

/// 权重：起跳淡入、滞空保持满权重、落地恢复段淡出归零
#[test]
fn weight_fades_in_and_out() {
    for pose in JumpPose::ALL {
        let spec = *pose.spec();
        let tl = build_timeline(&spec);
        assert_eq!(tl.weight(JumpState::Ground, 0.0, 0.0), 0.0);
        assert_eq!(tl.weight(JumpState::Takeoff, 0.0, 0.0), 0.0);
        assert!((tl.weight(JumpState::Takeoff, BLEND_IN_SECS, 0.0) - 1.0).abs() < 1e-5);
        assert!((tl.weight(JumpState::Air, BLEND_IN_SECS, 0.0) - 1.0).abs() < 1e-5);
        assert!(tl.weight(JumpState::Land, tl.total_secs(), tl.recover_secs) < 1e-5);
    }
}

/// 淡入必须在离地帧之前完成：否则物理已经把人抬起来，姿态还在跑
#[test]
fn blend_completes_before_takeoff() {
    for pose in JumpPose::ALL {
        let spec = *pose.spec();
        let tl = build_timeline(&spec);
        assert!(
            tl.blend_secs <= tl.take_secs.max(MIN_BLEND_SECS) + 1e-6,
            "淡入 {:.3}s 超过蓄力段 {:.3}s",
            tl.blend_secs,
            tl.take_secs
        );
        // 离地那一帧就是满权重：起飞瞬间姿态已经换到跳跃剪辑
        assert!(
            (tl.weight(JumpState::Air, tl.take_secs, 0.0) - 1.0).abs() < 1e-5,
            "离地帧权重不足 1（take {:.3}s, blend {:.3}s）",
            tl.take_secs,
            tl.blend_secs
        );
    }
    // 跑跳蓄力段 0.05s：淡入跟着压到 0.05s，不再吃满 0.14s
    let run_take = JumpPose::Run1.tuning().take_secs.unwrap_or_default();
    let mut run = build_timeline(JumpPose::Run1.spec());
    run.set_take_secs(run_take);
    assert!((run.blend_secs - run_take).abs() < 1e-6);
}

/// 蓄力段：地面深蹲 1:1 保留剪辑时长；高台准备段压缩到固定时长
#[test]
fn anticipation_preserved_for_jumps_not_drop() {
    let flat_spec = JumpPose::Stand.spec();
    let flat = build_timeline(flat_spec);
    assert!((flat.take_secs - (flat_spec.takeoff - flat_spec.entry)).abs() < 1e-4);
    let drop_spec = JumpPose::Down.spec();
    let drop = build_timeline(drop_spec);
    assert!(
        matches!(drop_spec.anticipation, Anticipation::CompressTo(_)),
        "下跳的高台准备段应当是压缩段"
    );
    assert!((drop.take_secs - drop_spec.take_secs()).abs() < 1e-4);
}

/// 事件检测：先蹲后升再落的曲线能检出离地 / 落地；末帧仍高于基线（登上高台）回退默认落地帧
#[test]
fn detect_events_finds_takeoff_and_land() {
    let step = 1.0 / CURVE_HZ;
    // 0.70 基线：先蹲到 0.55，抬到 0.95 顶点，再落回 0.70
    let curve = vec![
        0.70, 0.66, 0.60, 0.55, 0.58, 0.64, 0.74, 0.85, 0.95, 0.88, 0.78, 0.71, 0.70,
    ];
    let (takeoff, land) = detect_events(
        JumpPose::Stand.spec(),
        &curve,
        step,
        curve.len() as f32 * step,
    );
    // 越过基线+0.03 的第一帧是索引 6（0.74）；落地是索引 11（0.71）
    assert!((takeoff - 6.0 * step).abs() < 1e-4);
    assert!((land - 11.0 * step).abs() < 1e-4);

    // 登上高台：末帧 1.05 仍高于基线，找不到回落帧 → 回退默认 land
    let up = vec![0.70, 0.66, 0.60, 0.62, 0.75, 0.90, 1.02, 1.05];
    let up_spec = JumpPose::Up.spec();
    let (_, land_up) = detect_events(up_spec, &up, step, up.len() as f32 * step);
    assert!((land_up - up_spec.land).abs() < 1e-4);
}

/// 平地 / 上跳根位移只取起跳基线以上的部分
#[test]
fn flat_root_offset_is_non_negative() {
    let curve = vec![0.71, 0.54, 0.63, 0.91, 0.61, 0.72];
    let action = JumpAction {
        curve,
        curve_step: 0.1,
        takeoff_hips: 0.6339,
        kind: JumpKind::Flat,
        ..Default::default()
    };
    for step in 0..40 {
        let y = root_y_at(&action, step as f32 * 0.02);
        assert!(y >= 0.0, "t={step}: {y}");
    }
    // 0.3s 处正好是曲线里的 0.914（最高点）
    assert!(root_y_at(&action, 0.3) > 0.27);
}

/// 下跳根位移取基线以下的有符号部分
#[test]
fn down_root_offset_is_non_positive() {
    let curve = vec![1.43, 1.40, 1.00, 0.66, 0.66];
    let action = JumpAction {
        curve,
        curve_step: 0.1,
        takeoff_hips: 1.43,
        kind: JumpKind::Down,
        ..Default::default()
    };
    for step in 0..40 {
        let y = root_y_at(&action, step as f32 * 0.02);
        assert!(y <= 0.0, "t={step}: {y}");
    }
}

/// 起跳失败：姿态定格在离地帧，而不是跳到落地恢复段。
///
/// 失败时人还站在原处，没有落地冲击可演。若按 `Land` 取剪辑时间，动作层会在
/// 权重还是 1.0 的那一帧摆出「落地踩实」的恢复姿势，再在这上面淡出——玩家看到
/// 的是一次凭空的落地回弹。定格在 `takeoff` 则只是把蹬伸姿态收回移动混合。
#[test]
fn failed_jump_freezes_the_takeoff_frame() {
    for pose in JumpPose::ALL {
        let spec = *pose.spec();
        let tl = build_timeline(&spec);
        assert!((tl.clip_time(JumpState::Abort, 0.0, 0.0, 0.0, 1.0) - tl.takeoff).abs() < 1e-4);
        // 滞空段无论已经飞了多久，失败后都定格在离地帧，不再往前推
        for air in [0.0, 0.05, 0.3, tl.air_secs] {
            let frozen = tl.clip_time(JumpState::Abort, 0.0, air, 0.0, 0.5);
            assert!((frozen - tl.takeoff).abs() < 1e-4, "{air}s: {frozen}");
        }
        // 落地恢复段的起点是另一帧，绝不能被拿来冒充格
        assert!((tl.takeoff - tl.land).abs() > 1e-3);
    }
}
