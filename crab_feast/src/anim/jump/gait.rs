//! 起跳窗口：步态相位、单脚支撑、排队超时与联网上行电平。
//!
//! 窗口只推迟起跳，不吞请求；等满一个步幅周期就强制放行。

use crate::anim::character::{AnimCatalog, AnimSelection, ClipGait};
use crate::anim::driver::PoseDriver;
use crate::anim::mixers::{LocomotionMixer, RegisteredMixer};

use super::state::{JumpState, JumpTiming};

/// 停止移动的速度阈值（m/s）：低于它按站定处理，直接选站定跳，不再等步态相位窗口。
pub(super) const STOP_SPEED: f32 = 0.6;
/// 排队最长等待（秒）。相位窗口只是把起跳**推迟**到步态合适的那一帧，
/// 不是把请求吞掉：等满一个步幅周期还没等到窗口，就照常起跳，宁可不完美
/// 也不让玩家觉得按键失灵。
pub(super) const QUEUE_MAX_WAIT: f32 = 0.55;

/// 起跳窗口：起跳腿进入支撑后的这段时间。
const CONTACT_WINDOW: f32 = 0.14;

/// 跑步跳单脚着地判定：踩实脚支撑权重 ≥ 此值，另一脚 ≤ 此值。
/// 双脚同时踩实、双脚腾空、或权重模糊（过渡相）都不放行。
const SUPPORT_ON: f32 = 0.50;
const SUPPORT_OFF: f32 = 0.35;
/// 起跳窗口在支撑相里的进度区间（0=触地，1=离地）。
///
/// 早于此值的「触地初期」不放行：跑跳剪辑的入场帧是「支撑腿已蹬直在后、
/// 摆动腿已前摆到大步」，而跑步在触地初期摆动腿还在体后（实测前后错位差
/// 0.24m），在那一刻切剪辑，摆动腿会"抢半步"——看上去就是在空中走了一步。
/// 拖到支撑相后段（蹬离前）再起跳，两段的前后错位差降到 0.04m，
/// 摆动腿高度差 9cm（0.05s 淡入可以吃下）。
///
/// 晚于此值不放行：脚已经离地，没有再蹬的支撑点。
const TAKEOFF_EARLY: f32 = 0.55;
const TAKEOFF_LATE: f32 = 0.95;

/// 窗口没开时是否也该放行：等满一个步幅周期就强制起跳。
pub(super) fn takeoff_allowed(window_open: bool, queued_for: f32) -> bool {
    window_open || queued_for >= QUEUE_MAX_WAIT
}

/// 起跳窗口。按策略决定此刻能不能起跳。
pub(super) fn ready_to_jump(
    timing: JumpTiming,
    driver: &PoseDriver,
    catalog: &AnimCatalog,
    selection: &AnimSelection,
) -> bool {
    if timing == JumpTiming::Immediate {
        return true;
    }
    let Some(phase) = driver.phases.get(LocomotionMixer::ID).copied() else {
        return true;
    };
    let Some(clip) = driver.weights.first().map(|(name, _)| name.as_str()) else {
        return true;
    };
    let Some(gait) = catalog
        .character(selection.character)
        .and_then(|def| def.gaits.get(clip))
    else {
        return true;
    };

    match timing {
        JumpTiming::Immediate => true,
        // 任意一只脚在地上：跑 61% 的周期都在这个区间里，最坏 0.30s 必到
        JumpTiming::AnySupport => {
            phase_in_span(phase, gait.contact_left, gait.lift_left)
                || phase_in_span(phase, gait.contact_right, gait.lift_right)
        }
        // 起跳腿要在支撑期的**前段**：0.16s 的蓄力得塞进剩下的支撑时间里
        JumpTiming::PushOffFoot => match gait.contact_right {
            Some(contact) => (phase - contact).rem_euclid(1.0) < CONTACT_WINDOW,
            None => true,
        },
    }
}

/// 奔跑档起跳窗口：必须单脚着地，且踩实脚处于支撑相前段（重心尚在支撑脚后方）。
///
/// - 双脚同时踩实、双脚腾空、支撑权重模糊 → 不放行；
/// - 踩实脚已进入支撑后段（髋追过支撑脚）→ 不放行，避免「跨出去才蹬」；
/// - 步态相位元数据缺失时，退回单脚着地这一条硬条件，别把起跳彻底卡死。
pub(super) fn running_takeoff_ready(
    driver: &PoseDriver,
    catalog: &AnimCatalog,
    selection: &AnimSelection,
) -> bool {
    let phase = driver.phases.get(LocomotionMixer::ID).copied();
    let clip = driver.weights.first().map(|(name, _)| name.as_str());
    let gait = match clip {
        Some(clip) => catalog
            .character(selection.character)
            .and_then(|def| def.gaits.get(clip)),
        None => None,
    };
    running_window(driver.contacts, phase, gait)
}

/// 奔跑起跳窗口的纯判定：单脚着地且踩实脚走到支撑相后段（蹬离前）。
///
/// - 双脚同时踩实、双脚腾空、支撑权重模糊 → 不放行；
/// - 踩实脚还在支撑相前段（[`TAKEOFF_EARLY`] 之前）→ 不放行，此时摆动腿仍在
///   体后，切到跑跳剪辑会让摆动腿"抢半步"；
/// - 相位 / 步态元数据缺失时，退回单脚着地这一条硬条件，别把起跳彻底卡死。
pub(super) fn running_window(
    contacts: [f32; 2],
    phase: Option<f32>,
    gait: Option<&ClipGait>,
) -> bool {
    let [left, right] = contacts;
    let support = if left >= SUPPORT_ON && right <= SUPPORT_OFF {
        0usize
    } else if right >= SUPPORT_ON && left <= SUPPORT_OFF {
        1
    } else {
        return false;
    };

    let (Some(phase), Some(gait)) = (phase, gait) else {
        return true;
    };
    let (contact, lift) = if support == 0 {
        (gait.contact_left, gait.lift_left)
    } else {
        (gait.contact_right, gait.lift_right)
    };
    let (Some(contact), Some(lift)) = (contact, lift) else {
        return true;
    };
    if !phase_in_span(phase, Some(contact), Some(lift)) {
        return false;
    }
    let span = (lift - contact).rem_euclid(1.0);
    if span <= f32::EPSILON {
        return true;
    }
    let progress = (phase - contact).rem_euclid(1.0) / span;
    (TAKEOFF_EARLY..=TAKEOFF_LATE).contains(&progress)
}

/// 联网上行里 `jump` 电平该发什么。
///
/// 上行是「空口上的按键电平 + 服务端查上升沿」，所以只推迟边沿、不吞请求：
///
/// - 动作执行中（Takeoff / Air / Land）：**永远发 `true`**，按键抬起也不改。
///   电平跟的是动作状态、不是按键：一次跳跃一旦触发就要演完，提前回落会让
///   服务端以为这一跳已经结束；
/// - 已锁存但还没起跳（Queued / 地面等待）：发 `false`，让出电平，起跳那一帧
///   再发上升沿（相位锁定只推迟边沿、不吞请求）；
/// - 回到地面：跟随按键，但**挂着的下一次请求**要先让出电平——否则连跳时
///   两跳共用同一个上升沿，服务端第二跳不会被触发。
///
/// 等窗口超时后动作层会强制起跳，这里自然就把边沿发出去了——**相位锁定
/// 延后跳跃，不禁止跳跃**。
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub fn uplink_jump_level(state: JumpState, queued: bool, jump_pressed: bool) -> bool {
    match state {
        // 跳跃执行中：锁电平，直到 Land / Abort 演完回到地面才允许变化。
        // Abort 也锁住：这一跳的收尾还挂在客户端，提前回落会让服务端把
        // 「上一跳的余波」和「下一次按键」混成同一个电平。
        JumpState::Takeoff | JumpState::Air | JumpState::Land | JumpState::Abort => true,
        // 还没起跳：让出电平，边沿留到真正起跳那一帧
        JumpState::Queued => false,
        // 地面：跟随按键；锁存着的请求要先让出电平，下一跳才有干净的上升沿
        JumpState::Ground => jump_pressed && !queued,
    }
}

/// 相位是否落在 `[contact, lift]` 这段支撑里（跨 0 也算）
pub(super) fn phase_in_span(phase: f32, contact: Option<f32>, lift: Option<f32>) -> bool {
    let (Some(contact), Some(lift)) = (contact, lift) else {
        return false;
    };
    let span = (lift - contact).rem_euclid(1.0);
    (phase - contact).rem_euclid(1.0) <= span
}
