use super::super::*;

use crate::anim::character::ClipGait;

use super::super::gait::{QUEUE_MAX_WAIT, running_window, takeoff_allowed};

/// 奔跑跳窗口：单脚着地且踩实脚在支撑前段（重心在后）才放行
#[test]
fn running_window_requires_single_foot_rear_support() {
    // Running 实测相位：右脚 0.927→0.135（跨 0），左脚 0.406→0.625
    let gait = ClipGait {
        contact_left: Some(0.406),
        lift_left: Some(0.625),
        contact_right: Some(0.927),
        lift_right: Some(0.135),
        ..Default::default()
    };

    // 右脚着地、支撑相后段（进度 ~0.83，蹬离前）→ 放行
    assert!(running_window([0.05, 0.95], Some(0.10), Some(&gait)));
    // 左脚着地、支撑相后段（进度 ~0.79）→ 放行
    assert!(running_window([0.95, 0.05], Some(0.58), Some(&gait)));

    // 双脚同时踩实 → 拒
    assert!(!running_window([0.9, 0.9], Some(0.10), Some(&gait)));
    // 双脚腾空 → 拒
    assert!(!running_window([0.05, 0.05], Some(0.10), Some(&gait)));
    // 右脚着地但还在触地初期（进度 ~0.11，摆动腿仍在体后）→ 拒
    assert!(!running_window([0.05, 0.95], Some(0.95), Some(&gait)));
    // 右脚着地、支撑进度 ~0.45（门槛 0.55 之前）→ 拒
    assert!(!running_window([0.05, 0.95], Some(0.02), Some(&gait)));
    // 右脚着地但相位在摆动相（不在右脚支撑窗）→ 拒
    assert!(!running_window([0.05, 0.95], Some(0.50), Some(&gait)));

    // 相位 / 步态元数据缺失：退回单脚着地硬条件，不卡死
    assert!(running_window([0.05, 0.95], None, Some(&gait)));
    assert!(running_window([0.05, 0.95], Some(0.95), None));
}

/// 相位锁定只推迟不吞请求：等满一个步幅周期窗口不开也照跳
#[test]
fn queue_never_swallows_the_request() {
    assert!(takeoff_allowed(true, 0.0));
    assert!(!takeoff_allowed(false, 0.0));
    assert!(!takeoff_allowed(false, QUEUE_MAX_WAIT - 0.01));
    assert!(takeoff_allowed(false, QUEUE_MAX_WAIT));
    assert!(takeoff_allowed(false, QUEUE_MAX_WAIT + 1.0));
}

/// 上行电平：等窗口压住、起跳与执行期间保持、落地请求先让出电平
#[test]
fn uplink_level_defers_but_never_swallows() {
    assert!(!uplink_jump_level(JumpState::Queued, true, true));
    // 窗口打开后玩家早已松手，上升沿也要发出去
    assert!(uplink_jump_level(JumpState::Takeoff, false, false));
    assert!(uplink_jump_level(JumpState::Air, false, false));
    assert!(uplink_jump_level(JumpState::Land, false, false));
    assert!(uplink_jump_level(JumpState::Abort, false, false));
    // 地面跟随按键
    assert!(uplink_jump_level(JumpState::Ground, false, true));
    assert!(!uplink_jump_level(JumpState::Ground, false, false));
    // 跳跃执行中锁电平：落地恢复段里按下的下一跳不能把电平提前拉下来，
    // 否则服务端会认为这一跳已经结束
    assert!(uplink_jump_level(JumpState::Land, true, true));
    // 但回到地面时，锁存中的请求必须先让出电平，下一跳才有干净的上升沿
    assert!(!uplink_jump_level(JumpState::Ground, true, true));
}
