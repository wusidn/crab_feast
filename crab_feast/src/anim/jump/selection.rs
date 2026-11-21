//! 起跳分流：地面高差探测、支撑脚判定、按资源表选剪辑。
//!
//! 所有候选与回退顺序都来自 `resources.rs`，这里不写死剪辑名。

use avian3d::prelude::{SpatialQuery, SpatialQueryFilter};
use bevy::prelude::{Transform, Vec3};

use crate::anim::character::ClipGait;
use crate::anim::core::ClipSet;
use crate::ground::{PROBE_RISE, ground_height};
use crate::input::LocomotionIntent;

use super::gait::phase_in_span;
use super::resources::{JumpPose, RULES, SupportFoot};
use super::tuning::{JumpKind, JumpProfile};

/// 判定「哪只脚在支撑」的容差：两只脚的支撑权重差不到这个值就当作双脚起跳
const CONTACT_EPSILON: f32 = 0.05;

/// 沿移动 / 朝向方向前瞻探测落点，比较脚下与落点的地面高差。
pub(super) fn probe_kind(
    spatial: &SpatialQuery,
    filter: &SpatialQueryFilter,
    transform: &Transform,
    intent: &LocomotionIntent,
) -> JumpKind {
    let pos = transform.translation;
    let Some(current) = ground_height(spatial, filter, pos + Vec3::Y * PROBE_RISE) else {
        return JumpKind::Flat;
    };
    let forward = if intent.magnitude > 1e-3 {
        intent.direction.normalize_or_zero()
    } else {
        transform.rotation * Vec3::Z
    };
    let Some(landing) = ground_height(
        spatial,
        filter,
        pos + forward * RULES.land_probe_dist + Vec3::Y * PROBE_RISE,
    ) else {
        return JumpKind::Flat;
    };

    let delta = landing - current;
    if delta > RULES.step_height {
        JumpKind::Up
    } else if delta < -RULES.step_height {
        JumpKind::Down
    } else {
        JumpKind::Flat
    }
}

/// 支撑脚：左脚支撑权重明显大于右脚用左派生，反之用右派生，分不出来用对称版本。
pub(super) fn support_from_contacts(contacts: [f32; 2]) -> SupportFoot {
    let [left, right] = contacts;
    if left > right + CONTACT_EPSILON {
        SupportFoot::Left
    } else if right > left + CONTACT_EPSILON {
        SupportFoot::Right
    } else {
        SupportFoot::Any
    }
}

/// 远端用相位推支撑脚：此刻落在哪只脚的支撑窗里就用哪边的派生版本。
pub(super) fn support_from_phase(phase: Option<f32>, gait: Option<&ClipGait>) -> SupportFoot {
    let (Some(phase), Some(gait)) = (phase, gait) else {
        return SupportFoot::Any;
    };
    let left = phase_in_span(phase, gait.contact_left, gait.lift_left);
    let right = phase_in_span(phase, gait.contact_right, gait.lift_right);
    match (left, right) {
        (true, false) => SupportFoot::Left,
        (false, true) => SupportFoot::Right,
        _ => SupportFoot::Any,
    }
}

/// 按大类 / 地速 / 支撑脚 / 套别解析本次跳跃用的剪辑名。
///
/// 候选姿态由资源表（[`JumpPose::select`]）给出，随后沿每行的回退链找第一条
/// **剪辑表里真的有句柄**的资源；一条都没有时返回原地跳名字（`begin` 会报缺件）。
pub(super) fn resolve_clip(
    kind: JumpKind,
    speed: f32,
    support: SupportFoot,
    variant: u8,
    clips: &ClipSet,
) -> &'static str {
    let profile = JumpProfile::for_jump(kind, speed);
    let pose = JumpPose::select(kind, profile, support, variant);
    clip_with_fallback(pose, clips)
}

/// 沿资源表的回退链找第一条真正加载到的剪辑。
fn clip_with_fallback(pose: JumpPose, clips: &ClipSet) -> &'static str {
    let mut current = Some(pose);
    for _ in 0..=JumpPose::COUNT {
        let Some(pose) = current else {
            break;
        };
        if clips.handle(pose.clip()).is_some() {
            return pose.clip();
        }
        current = pose.fallback();
    }
    // 连原地跳都没有：仍返回名字，交给 `begin` 报缺件，不在选择阶段假装成功
    JumpPose::Stand.clip()
}

/// 远端跳跃的剪辑选择：台阶类型 / 地速 / 相位 / 套别，规则与本地一致。
///
/// 与本地共用同一张资源表和同一套回退链；差别只在支撑脚不是从接触权重读，
/// 而是从复制过来的步态相位推。套别由远端自己数观察到的起跳次数轮换
/// （见 [`RemoteJump`](super::remote::RemoteJump)）——这一层是纯表现，两端各自
/// 轮换即可，不需要再为它加一条网络状态。
pub fn pick_clip(
    kind: JumpKind,
    speed: f32,
    phase: Option<f32>,
    gait: Option<&ClipGait>,
    variant: u8,
    clips: &ClipSet,
) -> &'static str {
    resolve_clip(kind, speed, support_from_phase(phase, gait), variant, clips)
}
