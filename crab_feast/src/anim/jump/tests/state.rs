use super::super::*;

use super::super::air::launch_speed;
use super::super::gait::STOP_SPEED;

/// 台阶类型 + 地速 → 档位：高差优先，平地再按速度分站 / 跑
#[test]
fn profile_resolves_from_kind_and_speed() {
    assert_eq!(
        JumpProfile::for_jump(JumpKind::Flat, 0.0),
        JumpProfile::Stand
    );
    assert_eq!(
        JumpProfile::for_jump(JumpKind::Flat, STOP_SPEED),
        JumpProfile::Stand
    );
    assert_eq!(
        JumpProfile::for_jump(JumpKind::Flat, RULES.run_jump_speed),
        JumpProfile::Run
    );
    // 高差压过速度：跑着跳上高台仍然是「上跳」档，不会串成跑跳
    assert_eq!(JumpProfile::for_jump(JumpKind::Up, 3.0), JumpProfile::Up);
    assert_eq!(
        JumpProfile::for_jump(JumpKind::Down, 3.0),
        JumpProfile::Down
    );
}

/// 四档调参互不影响：改一档，另外三档原样
#[test]
fn tuning_profiles_do_not_leak_into_each_other() {
    let mut tunings = JumpTunings::default();
    let before = [
        tunings.get(JumpProfile::Stand),
        tunings.get(JumpProfile::Run),
        tunings.get(JumpProfile::Up),
        tunings.get(JumpProfile::Down),
    ];

    // 只调跑跳：抬高一点、慢放下限抬到 0.8
    tunings.tune(JumpProfile::Run).launch_vy = 4.2;
    tunings.tune(JumpProfile::Run).net_launch_vy = 4.8;
    tunings.tune(JumpProfile::Run).slow_min = 0.8;

    assert_eq!(tunings.get(JumpProfile::Run).launch_vy, 4.2);
    assert_eq!(tunings.get(JumpProfile::Run).net_launch_vy, 4.8);
    assert_eq!(tunings.get(JumpProfile::Run).slow_min, 0.8);

    assert_eq!(tunings.get(JumpProfile::Stand), before[0]);
    assert_eq!(tunings.get(JumpProfile::Up), before[2]);
    assert_eq!(tunings.get(JumpProfile::Down), before[3]);

    // 站 / 跑之外的档位不借用平地的蓄力时长
    assert_eq!(
        before[2].take_secs,
        Some(JumpPose::Up.spec().take_secs()),
        "上跳应按剪辑自己的离地帧"
    );
    assert_eq!(
        before[3].take_secs,
        Some(JumpPose::Down.spec().take_secs()),
        "下跳的准备段本来就是压缩段"
    );
    assert_eq!(before[0].take_secs, JumpPose::Stand.tuning().take_secs);
    assert_eq!(before[1].take_secs, JumpPose::Run1.tuning().take_secs);
}

/// 起跳初速取的是**这一档**的标定值：同一份实测速度在不同档里得到不同高度
#[test]
fn launch_speed_uses_the_profile_tuning() {
    let mut run = JumpPose::Run1.tuning();
    run.launch_vy = 4.4;
    assert!((launch_speed(0.19, run) - 4.4).abs() < 1e-5);
    let stand = JumpPose::Stand.tuning();
    assert!((launch_speed(0.19, stand) - stand.launch_vy).abs() < 1e-5);
}

/// 起跳那一帧把参数快照进动作层：之后改表不影响这一跳
#[test]
fn action_keeps_its_tuning_snapshot() {
    let mut action = JumpAction::default();
    let mut tunings = JumpTunings::default();
    action.profile = JumpProfile::Up;
    action.tuning = tunings.get(JumpProfile::Up);

    tunings.tune(JumpProfile::Up).slow_min = 0.9;
    tunings.tune(JumpProfile::Up).max_air_hold = 0.6;

    assert_eq!(action.tuning.slow_min, JumpPose::Up.tuning().slow_min);
    assert_eq!(
        action.tuning.max_air_hold,
        JumpPose::Up.tuning().max_air_hold
    );
    assert_eq!(tunings.get(JumpProfile::Up).slow_min, 0.9);
}

/// 动作层接管判定：地面与排队都不算接管，避免排队期间接触权重被清零卡死窗口
#[test]
fn active_excludes_ground_and_queued() {
    let mut action = JumpAction {
        state: JumpState::Ground,
        ..Default::default()
    };
    assert!(!action.is_active());
    action.state = JumpState::Queued;
    assert!(!action.is_active());
    for state in [JumpState::Takeoff, JumpState::Air, JumpState::Land] {
        action.state = state;
        assert!(action.is_active(), "{:?} 应算接管", state);
    }
    // 起跳失败的淡出段也算接管：姿态还挂在动作层上，这时放开脚 IK 会把
    // 半蹬出去的腿当场钉回地面，看着就是一顿抽。
    action.state = JumpState::Abort;
    assert!(action.is_active());
}
