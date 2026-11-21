//! 落地交接：恢复段把移动相位对齐到动作剪辑末帧的着地脚位置。
//!
//! 相位基准只在进入 Land 那一帧算一次，之后按移动剪辑原生速度推进。

use bevy::animation::AnimationClip;
use bevy::prelude::{Assets, info};

use crate::anim::character::{AnimCatalog, AnimSelection, ClipGait};
use crate::anim::core::ClipSet;
use crate::anim::driver::PoseDriver;
use crate::anim::mixers::{LocomotionMixer, RegisteredMixer};

use super::resources::{JumpPose, SupportFoot};
use super::timeline::Timeline;

/// 落地恢复段的交接相位：每帧把移动相位推到「动作剪辑末帧那只着地脚的支撑位置」。
///
/// 不写这一步，融回移动的第一帧可能落在「半支撑」姿态上，脚会在地上滑半步。
///
/// **着地脚不能按剪辑名猜**：跑跳是「一条腿蹬地、另一条腿先着地」，蹬地腿和
/// 最后踩实的腿不是同一只。实测 `RunningJump1`（右腿蹬地的原片）左踝 0.700 就
/// 触地，右踝到 0.875 才踩实、一直撑到末帧；`RunningJump1Left` 正好镜像。
/// 名字里带不带 `Left` 只在派生片上成立，源片 `RunningJump1` 也落在右脚——
/// 按名字取脚会整整差半个周期：交接那一刻「另一只脚」正处在支撑相里往后蹭，
/// 看着就是落地后步频少了一拍。`RunningJump2` 反过来是左脚蹬地，所以资源表里
/// 每行都单独登记了着地脚，不按套别推算。
///
/// 所以按**剪辑末帧还踩在地上的那只脚**判（它的支撑窗口跨过相位 0/1），
/// 现场没有步态数据时才退回名字约定。
///
/// **只对到触地点还不够。** 动作剪辑末帧的着地脚早不在触地位置，而是已经撑到
/// 支撑相后段；`Running` 的触地点却是「脚掌刚落下、还在体前」。实测（Amy，
/// 踝相对髋的前后位置）：`RunningJump1` 末帧右脚 z −0.234（体后），
/// `Running` 相位 0.927 右脚 z +0.075（体前）。差 0.31 m，全摊在淡出的 0.15s 上
/// 就是 2.1 m/s —— 脚在地上被硬拽回体前，正常速度下看着就是落地后又「换了一次脚」
/// 加一层残影（慢放拆得开，所以慢放看着正常）。
///
/// 补法：动作剪辑末帧这只脚在支撑相里已经走完的比例（`stance_progress`，跑跳实测
/// 0.67），乘到移动剪辑这只脚的支撑相长度上，得到 `target`。
///
/// **相位也不能冻着。** 恢复段里移动相位按它自己的原生速度推进
/// `recover_secs / 剪辑时长` 个周期（跑跳 = 0.24/0.70 = 0.347），所以起点要退到
/// `target − 0.343`，正好在落地那一帧开始走、交回那一刻走到 `target`。
/// 这样淡出期间着地脚相对身体是往后走的（和真正跑步一样），不会被身体带着
/// 一路往前滑。起点 0.723 落在移动剪辑这只脚的摆动相（0.135 → 0.927）——那一刻
/// 动作剪辑权重还是 1.0，摆动相看不见；等权重降下来，相位已经走到 0.923 → 0.066
/// 的支撑段，交回时着地脚正好踩实。
/// 落地恢复段的相位基准：`(交接目标相位, 恢复段该走的周期数)`。
///
/// 只在**进入 Land 那一帧**算一次（`drive_jump` 的 `previous == Air` 分支），之后整段
/// 复用。每帧现算会让基准跟着权重漂——跑着落地时人一停，混合从 Running 滑向
/// Walking / Idle，`target` 从 0.066 跳到 0.51、`advance` 从 0.343 掉到 0.03，
/// 写出来的相位当场瞬移半圈，脚就是「落地后猛换一下」。
#[allow(clippy::too_many_arguments)]
pub(super) fn landing_alignment(
    driver: &PoseDriver,
    catalog: &AnimCatalog,
    selection: &AnimSelection,
    action_clip: &str,
    timeline: Timeline,
    clips: &ClipSet,
    clip_assets: &Assets<AnimationClip>,
) -> (f32, f32) {
    let def = catalog.character(selection.character);
    // 着地脚：优先看这一步剪辑末帧的实测支撑；缺步态数据时用资源表登记的默认约定
    let right = def
        .and_then(|def| def.gaits.get(action_clip))
        .and_then(landing_foot_right)
        .unwrap_or_else(|| default_landing_foot_right(action_clip));
    // 混合里第一个不一定是最主要的剪辑：取权重最大的那条拿它的触地 / 离地事件
    let dominant = driver
        .weights
        .iter()
        .filter(|(_, weight)| *weight > f32::EPSILON)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(clip, _)| clip.as_str());
    let locomotion = dominant.and_then(|clip| def.and_then(|def| def.gaits.get(clip)));
    let target = landing_phase(
        def.and_then(|def| def.gaits.get(action_clip)),
        locomotion,
        right,
    );
    // 恢复段该推进多少周期：按移动剪辑的原生播放速度（相位的推进速度就是 1/时长）
    let advance = dominant
        .and_then(|clip| clips.duration(clip_assets, clip))
        .filter(|duration| *duration > f32::EPSILON)
        .map(|duration| timeline.recover_secs / duration)
        .unwrap_or(0.0);
    (target, advance)
}

/// 恢复段每帧把移动相位写到「基准相位 + 按原生速度走出来的那一段」。
///
/// 现场调参用：`CRAB_LOG_LAND=1` 启动客户端，就能看到恢复段每帧写了多少相位。
pub(super) fn follow_landing_phase(
    driver: &mut PoseDriver,
    target: f32,
    advance: f32,
    land_time: f32,
    recover_secs: f32,
) {
    let progress = (land_time / recover_secs.max(f32::EPSILON)).clamp(0.0, 1.0);
    let phase = landing_phase_at(target, advance, progress);
    if std::env::var("CRAB_LOG_LAND").is_ok() {
        info!(
            "land t={:.3} prog={:.3} phase={:.3} target={:.3} adv={:.3}",
            land_time, progress, phase, target, advance
        );
    }
    driver.phases.insert(LocomotionMixer::ID.to_string(), phase);
}

/// 恢复段进度 `progress` 时该写的移动相位：从 `target − advance` 线性推到 `target`。
pub(super) fn landing_phase_at(target: f32, advance: f32, progress: f32) -> f32 {
    (target - (1.0 - progress.clamp(0.0, 1.0)) * advance).rem_euclid(1.0)
}

/// 交接相位：移动剪辑着地脚的触地点 + 动作剪辑末帧已经走完的那段支撑相。
pub(super) fn landing_phase(
    action: Option<&ClipGait>,
    locomotion: Option<&ClipGait>,
    right: bool,
) -> f32 {
    let (action_contact, action_lift) = action.map_or((None, None), |gait| foot_span(gait, right));
    let (contact, lift) = locomotion.map_or((None, None), |gait| foot_span(gait, right));
    // 触地点缺数据时退回实测默认（跑跳：右脚 0.927 / 左脚 0.406）
    let contact = contact.unwrap_or(if right { 0.927 } else { 0.406 });
    let lag = match (
        stance_progress(1.0, action_contact, action_lift),
        stance_fraction(contact, lift),
    ) {
        (Some(progress), Some(span)) => progress * span,
        _ => 0.0,
    };
    (contact + lag).rem_euclid(1.0)
}

/// 某只脚的 `[触地, 离地)` 事件对。
pub(super) fn foot_span(gait: &ClipGait, right: bool) -> (Option<f32>, Option<f32>) {
    if right {
        (gait.contact_right, gait.lift_right)
    } else {
        (gait.contact_left, gait.lift_left)
    }
}

/// 支撑窗口 `[触地, 离地)` 的长度（周比），跨相位 0/1 时按回绕算。
pub(super) fn stance_fraction(contact: f32, lift: Option<f32>) -> Option<f32> {
    let span = (lift? - contact).rem_euclid(1.0);
    (span > f32::EPSILON).then_some(span)
}

/// 支撑窗口里 `phase` 已经走完的比例（0 = 刚触地，1 = 正要抬脚）。
pub(super) fn stance_progress(phase: f32, contact: Option<f32>, lift: Option<f32>) -> Option<f32> {
    let contact = contact?;
    let span = stance_fraction(contact, lift)?;
    let progress = (phase - contact).rem_euclid(1.0) / span;
    (progress <= 1.0 + 1e-4).then_some(progress.clamp(0.0, 1.0))
}

/// 剪辑末帧还踩在地上的是不是右脚。
///
/// 判据是支撑窗口 `[触地, 离地)` 跨过相位 0/1——跑跳收尾正是「一只脚撑住、
/// 另一只脚已经抬起来前摆」，所以这一只就是交接后该继续钉地的那只。
/// 两只脚都在窗口里或都不在（原地双脚跳那种）时分不出来，返回 `None`。
pub(super) fn landing_foot_right(gait: &ClipGait) -> Option<bool> {
    let supports_at_wrap = |contact: Option<f32>, lift: Option<f32>| {
        let (Some(contact), Some(lift)) = (contact, lift) else {
            return false;
        };
        let span = (lift - contact).rem_euclid(1.0);
        span > f32::EPSILON && (0.0 - contact).rem_euclid(1.0) < span
    };
    match (
        supports_at_wrap(gait.contact_left, gait.lift_left),
        supports_at_wrap(gait.contact_right, gait.lift_right),
    ) {
        (true, false) => Some(false),
        (false, true) => Some(true),
        _ => None,
    }
}

/// 缺步态数据时的着地脚约定，来自资源表的 [`JumpPoseDef::landing_foot`]。
///
/// `SupportFoot::Any`（原片没登记方向 / 原地双脚跳）沿用旧约定按右脚对齐。
pub(super) fn default_landing_foot_right(clip: &str) -> bool {
    let def = JumpPose::from_clip(clip).map(JumpPose::def);
    !matches!(def.map(|def| def.landing_foot), Some(SupportFoot::Left))
}
