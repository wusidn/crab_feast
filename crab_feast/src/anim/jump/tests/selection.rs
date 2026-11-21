use super::super::*;

use bevy::animation::AnimationClip;
use bevy::prelude::Handle;

use crate::anim::character::ClipGait;
use crate::anim::core::ClipSet;

use super::super::gait::phase_in_span;
use super::super::landing::{default_landing_foot_right, landing_foot_right};
use super::super::selection::resolve_clip;
use super::super::timeline::spec_for;

/// 支撑区间判定：右脚 0.927→1.135 是跨 0 的，也要算对
#[test]
fn stance_span_handles_wrap() {
    // Running 右脚：0.927 触地，0.135 离地
    assert!(phase_in_span(0.95, Some(0.927), Some(0.135)));
    assert!(phase_in_span(0.02, Some(0.927), Some(0.135)));
    assert!(!phase_in_span(0.2, Some(0.927), Some(0.135)));

    // Running 左脚：0.406 → 0.625
    assert!(phase_in_span(0.5, Some(0.406), Some(0.625)));
    assert!(!phase_in_span(0.7, Some(0.406), Some(0.625)));

    // 没测出来就当不在支撑
    assert!(!phase_in_span(0.5, None, None));
}

/// 支撑脚 → 姿态 → 剪辑名：左 / 右派生跟着支撑脚走，分不出来用基础跑跳；
/// 缺件时沿资源表的回退链退回原地跳。
#[test]
fn run_clip_follows_support_foot() {
    let mut clips = ClipSet::default();
    for pose in JumpPose::ALL {
        clips.insert(pose.clip(), Handle::<AnimationClip>::default());
    }
    let speed = RULES.run_jump_speed;
    // 第 1 套：源片本来就是右脚蹬地，左支撑走镜像片
    assert_eq!(
        resolve_clip(JumpKind::Flat, speed, SupportFoot::Left, 0, &clips),
        JumpPose::Run1Left.clip()
    );
    assert_eq!(
        resolve_clip(JumpKind::Flat, speed, SupportFoot::Right, 0, &clips),
        JumpPose::Run1.clip()
    );
    assert_eq!(
        resolve_clip(JumpKind::Flat, speed, SupportFoot::Any, 0, &clips),
        JumpPose::Run1.clip()
    );
    // 连跳轮换：第 2 跳走第 2 套（源片左脚、镜像右脚），第 3 跳走第 3 套（源片右脚）
    assert_eq!(
        resolve_clip(JumpKind::Flat, speed, SupportFoot::Left, 1, &clips),
        JumpPose::Run2.clip()
    );
    assert_eq!(
        resolve_clip(JumpKind::Flat, speed, SupportFoot::Right, 1, &clips),
        JumpPose::Run2Right.clip()
    );
    assert_eq!(
        resolve_clip(JumpKind::Flat, speed, SupportFoot::Right, 2, &clips),
        JumpPose::Run3.clip()
    );
    assert_eq!(
        resolve_clip(JumpKind::Flat, speed, SupportFoot::Left, 2, &clips),
        JumpPose::Run3Left.clip()
    );
    // 慢速走 / 站定：原地跳
    assert_eq!(
        resolve_clip(JumpKind::Flat, 0.0, SupportFoot::Left, 0, &clips),
        JumpPose::Stand.clip()
    );

    // 派生片缺件 → 基础跑跳 → 原地跳
    let mut sparse = ClipSet::default();
    sparse.insert(JumpPose::Run1.clip(), Handle::<AnimationClip>::default());
    sparse.insert(JumpPose::Stand.clip(), Handle::<AnimationClip>::default());
    assert_eq!(
        resolve_clip(JumpKind::Flat, speed, SupportFoot::Left, 0, &sparse),
        JumpPose::Run1.clip()
    );
    // 那一套整行都缺件：退回第一套（而不是直接掉到原地跳）
    assert_eq!(
        resolve_clip(JumpKind::Flat, speed, SupportFoot::Any, 2, &sparse),
        JumpPose::Run1.clip()
    );
    // 只有原地跳：所有档都回退到它
    let mut minimal = ClipSet::default();
    minimal.insert(JumpPose::Stand.clip(), Handle::<AnimationClip>::default());
    assert_eq!(
        resolve_clip(JumpKind::Up, speed, SupportFoot::Any, 0, &minimal),
        JumpPose::Stand.clip()
    );

    // 没走过 begin 的默认状态退回原地跳
    assert_eq!(JumpAction::default().clip_name(), JumpPose::Stand.clip());
}

/// 落地脚按剪辑末帧的支撑判，不按剪辑名猜。
///
/// 实测 `RunningJump1`（右腿蹬地）左踝 0.700 就触地、0.781 抬走；
/// 右踝 0.875 才踩实（离地 0.062，撑到剪辑末尾）。所以交接后该继续
/// 钉地的是**右脚**——名字里没有 `Right` 也一样。
#[test]
fn landing_foot_follows_the_clip_not_its_name() {
    let base = ClipGait {
        contact_left: Some(0.708),
        lift_left: Some(0.781),
        contact_right: Some(0.875),
        lift_right: Some(0.062),
        ..Default::default()
    };
    assert_eq!(landing_foot_right(&base), Some(true));
    assert_ne!(JumpPose::Run1.clip(), JumpPose::Run1Left.clip());
    // 缺步态数据时按资源表登记的着地脚约定兜底
    assert!(default_landing_foot_right(JumpPose::Run1.clip()));
    assert!(!default_landing_foot_right(JumpPose::Run1Left.clip()));

    // 镜像派生片：左手撑到末帧
    let mirrored = ClipGait {
        contact_left: Some(0.875),
        lift_left: Some(0.052),
        contact_right: Some(0.708),
        lift_right: Some(0.781),
        ..Default::default()
    };
    assert_eq!(landing_foot_right(&mirrored), Some(false));

    // 原地双脚跳：两只脚都在末帧前就「抬」过（检测的是髋弹跳），分不出来
    let static_jump = ClipGait {
        contact_left: Some(0.167),
        lift_left: Some(0.26),
        contact_right: Some(0.146),
        lift_right: Some(0.281),
        ..Default::default()
    };
    assert_eq!(landing_foot_right(&static_jump), None);
    // 缺步态数据也分不出来，交给名字兜底
    assert_eq!(landing_foot_right(&ClipGait::default()), None);
}

/// 剪辑名 → 资源表里的时间轴参数；表外名字按原地跳兜底
#[test]
fn spec_lookup_follows_the_resource_table() {
    for pose in JumpPose::ALL {
        assert!(std::ptr::eq(spec_for(pose.clip()), pose.spec()));
    }
    assert!(std::ptr::eq(
        spec_for("SomeUnregisteredClip"),
        JumpPose::Stand.spec()
    ));
}

/// 远端挑剪辑：慢速原地跳、快速按支撑脚选左右派生、缺剪辑时逐级回退
#[test]
fn pick_clip_matches_local_rules() {
    let mut clips = ClipSet::default();
    clips.insert(JumpPose::Stand.clip(), Handle::<AnimationClip>::default());
    // 只有原地跳：快速也回退到原地跳
    assert_eq!(
        pick_clip(JumpKind::Flat, 3.0, None, None, 0, &clips),
        JumpPose::Stand.clip()
    );

    clips.insert(JumpPose::Run1.clip(), Handle::<AnimationClip>::default());
    // 无相位可用 → 该套的源片
    assert_eq!(
        pick_clip(JumpKind::Flat, 3.0, None, None, 0, &clips),
        JumpPose::Run1.clip()
    );
    // 慢速 → 原地跳
    assert_eq!(
        pick_clip(JumpKind::Flat, 0.4, Some(0.6), None, 0, &clips),
        JumpPose::Stand.clip()
    );

    clips.insert(JumpPose::Run1Left.clip(), Handle::<AnimationClip>::default());
    let gait = ClipGait {
        stride: 1.6,
        contact_left: Some(0.50),
        lift_left: Some(0.85),
        contact_right: Some(0.00),
        lift_right: Some(0.35),
    };
    // 相位落在右脚支撑窗里 → 第 1 套源片（它本来就是右脚蹬地）
    assert_eq!(
        pick_clip(JumpKind::Flat, 3.0, Some(0.2), Some(&gait), 0, &clips),
        JumpPose::Run1.clip()
    );
    // 左脚支撑窗
    assert_eq!(
        pick_clip(JumpKind::Flat, 3.0, Some(0.6), Some(&gait), 0, &clips),
        JumpPose::Run1Left.clip()
    );

    // 第 2 套是左脚源片，右脚支撑时要用它的镜像；没装镜像就沿回退链退回第 2 套源片
    clips.insert(JumpPose::Run2.clip(), Handle::<AnimationClip>::default());
    assert_eq!(
        pick_clip(JumpKind::Flat, 3.0, Some(0.6), Some(&gait), 1, &clips),
        JumpPose::Run2.clip()
    );
    assert_eq!(
        pick_clip(JumpKind::Flat, 3.0, Some(0.2), Some(&gait), 1, &clips),
        JumpPose::Run2.clip()
    );
    clips.insert(
        JumpPose::Run2Right.clip(),
        Handle::<AnimationClip>::default(),
    );
    assert_eq!(
        pick_clip(JumpKind::Flat, 3.0, Some(0.2), Some(&gait), 1, &clips),
        JumpPose::Run2Right.clip()
    );
    // 台阶类型优先于速度分档；这一份剪辑集里没有上跳资源 → 回退原地跳
    assert_eq!(
        pick_clip(JumpKind::Up, 3.0, Some(0.6), Some(&gait), 0, &clips),
        JumpPose::Stand.clip()
    );
}
