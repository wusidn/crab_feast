use crate::anim::character::ClipGait;

use super::super::landing::{landing_phase, landing_phase_at, stance_fraction, stance_progress};

/// 交接相位补到支撑相后段：动作末帧的着地脚已经撑到哪，移动剪辑就走到哪。
///
/// Amy 实测：`RunningJump1`（右脚蹬地）右脚 0.875→0.062（末帧相位 1.0 已走完
/// (1.0-0.875)/0.187 = 0.67 的支撑相）；`Running` 右脚 0.927→0.135（支撑相
/// 长 0.208）。补 0.67×0.208 = 0.139 → 相位 1.066 ≈ 0.066。
/// 直接对到触地点（0.927）时，着地脚在交接瞬间会从体后 −0.23m 被拽到体前
/// +0.08m（0.31m / 0.15s），这就是「落地后又换了一次脚」。
#[test]
fn landing_phase_reaches_past_the_contact_frame() {
    let action = ClipGait {
        contact_left: Some(0.708),
        lift_left: Some(0.781),
        contact_right: Some(0.875),
        lift_right: Some(0.062),
        ..Default::default()
    };
    let run = ClipGait {
        contact_left: Some(0.406),
        lift_left: Some(0.625),
        contact_right: Some(0.927),
        lift_right: Some(0.135),
        ..Default::default()
    };

    let phase = landing_phase(Some(&action), Some(&run), true);
    assert!((phase - 0.066).abs() < 0.002, "phase = {phase}");
    // 交接后支撑脚只剩 0.135 - 0.066 = 0.07 个周期就要抬脚，
    // 正好接上动作末帧「右脚已经在起跟」的姿态，不再多钉一段
    assert!((0.135 - phase).abs() < 0.07);

    // 镜像：着地脚换成左脚，同样要补
    let mirrored = ClipGait {
        contact_left: Some(0.875),
        lift_left: Some(0.052),
        contact_right: Some(0.708),
        lift_right: Some(0.781),
        ..Default::default()
    };
    let phase = landing_phase(Some(&mirrored), Some(&run), false);
    // 左脚支撑相 0.406→0.625 长 0.219，末帧走完 0.125/0.177 = 0.71
    let expected = 0.406 + 0.71 * 0.219;
    assert!((phase - expected).abs() < 0.01, "phase = {phase}");

    // 缺离地帧（原地跳 / 站立落地）补不了，退回触地点本身
    let no_lift = ClipGait {
        contact_left: Some(0.969),
        contact_right: Some(0.51),
        ..Default::default()
    };
    assert!((landing_phase(None, Some(&no_lift), true) - 0.51).abs() < 1e-6);
    // 全缺数据：跑跳实测默认
    assert!((landing_phase(None, None, true) - 0.927).abs() < 1e-6);
    assert!((landing_phase(None, None, false) - 0.406).abs() < 1e-6);
}

/// 恢复段里相位按移动剪辑的原生速度往前走：起点退回一个恢复段，终点正好到 target。
///
/// 跑跳：recover 0.24s / Running 0.70s = 0.343 个周期。所以落地那一帧写
/// `1.066 − 0.343 = 0.723`，之后每帧往前推，恢复段播完正好 1.066（= 0.066 相位），
/// 交回移动时两套姿态对齐。冻在 0.927 那版是「一步也不走」，着地脚 0.24s 里
/// 被身体带着滑 0.7m。
#[test]
fn landing_phase_walks_back_one_recover_segment() {
    let target = 1.066;
    let advance: f32 = 0.24 / 0.70;
    assert!((advance - 0.343).abs() < 5e-4);

    let start = landing_phase_at(target, advance, 0.0);
    assert!((start - 0.723).abs() < 5e-3, "start = {start}");
    let end = landing_phase_at(target, advance, 1.0);
    assert!((end - 0.066).abs() < 5e-3, "end = {end}");
    let mid = landing_phase_at(target, advance, 0.5);
    assert!((mid - 0.895).abs() < 5e-3, "mid = {mid}");

    // 交回那一刻右脚只剩 0.135 − 0.066 = 0.07 个周期就要抬脚，正好接上动作
    // 末帧「右脚已经在起跟」的姿态，不会再多钉一段也不会立刻被换掉
    assert!(0.135 - end > 0.05);
}

/// 支撑相进度 / 长度：跨相位 0/1 也要算对
#[test]
fn stance_window_helpers_wrap() {
    // 右脚 0.927→0.135：跨 0，长 0.208
    assert!((stance_fraction(0.927, Some(0.135)).unwrap() - 0.208).abs() < 1e-5);
    // 支撑窗口外的相位（右脚摆动相）→ None
    assert_eq!(stance_progress(0.6, Some(0.927), Some(0.135)), None);
    // 刚触地 / 刚要抬脚
    assert!(stance_progress(0.927, Some(0.927), Some(0.135)).unwrap() < 1e-4);
    assert!(stance_progress(0.135, Some(0.927), Some(0.135)).unwrap() > 0.999);
    // 窗口长度为零（原地跳那类没测出步态）→ None
    assert_eq!(stance_fraction(0.5, Some(0.5)), None);
}
