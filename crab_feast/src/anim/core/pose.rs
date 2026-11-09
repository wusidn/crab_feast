//! 姿态（Pose）：按骨骼 target 组织的局部变换集合，以及采样/混合运算。
//!
//! 这是 UE `AnimBP` 里 pose 栈的最小对应物：节点产出 Pose，节点消费 Pose，
//! 合成结果最后一次性写回骨骼。

use std::collections::HashMap;

use bevy::{animation::AnimationClip, animation::AnimationTargetId, prelude::*};
use bevy_animation::animated_field;

/// 单根骨骼的局部变换
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BonePose {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl BonePose {
    pub const IDENTITY: Self = Self {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    pub fn from_transform(transform: &Transform) -> Self {
        Self {
            translation: transform.translation,
            rotation: transform.rotation,
            scale: transform.scale,
        }
    }

    pub fn apply_to(&self, transform: &mut Transform) {
        transform.translation = self.translation;
        transform.rotation = self.rotation;
        transform.scale = self.scale;
    }

    /// 线性插值：平移/缩放 lerp，旋转 slerp
    pub fn lerp(a: Self, b: Self, weight: f32) -> Self {
        let weight = weight.clamp(0.0, 1.0);
        Self {
            translation: a.translation.lerp(b.translation, weight),
            rotation: a.rotation.slerp(b.rotation, weight),
            scale: a.scale.lerp(b.scale, weight),
        }
    }
}

/// 一整套骨骼姿态
#[derive(Clone, Debug, Default)]
pub struct Pose {
    bones: HashMap<AnimationTargetId, BonePose>,
}

impl Pose {
    pub fn is_empty(&self) -> bool {
        self.bones.is_empty()
    }

    pub fn get(&self, id: AnimationTargetId) -> Option<&BonePose> {
        self.bones.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&AnimationTargetId, &BonePose)> {
        self.bones.iter()
    }

    /// 从剪辑按时间采样；未被打关键帧的骨骼退回 rest 姿态
    pub fn sample(
        clip: &AnimationClip,
        targets: &[AnimationTargetId],
        rest: &HashMap<AnimationTargetId, BonePose>,
        time: f32,
    ) -> Self {
        let mut bones = HashMap::with_capacity(targets.len());
        for target in targets {
            let base = rest.get(target).copied().unwrap_or(BonePose::IDENTITY);
            let translation = clip
                .sample_clamped(animated_field!(Transform::translation), *target, time)
                .unwrap_or(base.translation);
            let rotation = clip
                .sample_clamped(animated_field!(Transform::rotation), *target, time)
                .unwrap_or(base.rotation);
            let scale = clip
                .sample_clamped(animated_field!(Transform::scale), *target, time)
                .unwrap_or(base.scale);
            bones.insert(
                *target,
                BonePose {
                    translation,
                    rotation,
                    scale,
                },
            );
        }
        Self { bones }
    }

    /// 以 `weight` 的比例把 `other` 混进自己（不分配新表）
    pub fn blend_in_place(&mut self, other: &Pose, weight: f32) {
        if weight <= 0.0 {
            return;
        }
        for (id, bone) in &other.bones {
            match self.bones.get_mut(id) {
                Some(existing) => *existing = BonePose::lerp(*existing, *bone, weight),
                None => {
                    self.bones.insert(*id, *bone);
                }
            }
        }
    }

    /// 按 (clip 权重, 姿态) 列表合成；权重会被重新归一到总和
    ///
    /// 注意这里不能写成「先克隆一份 out，再按比例合进去」：那样每混入一个
    /// 剪辑都要克隆一次整张骨骼表（几十项 HashMap），4 个剪辑就是 4 次全量分配。
    pub fn combine(parts: &[(f32, Pose)]) -> Pose {
        let mut out = Pose::default();
        let mut accumulated = 0.0;
        for (weight, pose) in parts {
            if *weight <= 0.0 {
                continue;
            }
            let next = accumulated + *weight;
            if out.is_empty() && accumulated <= 0.0 {
                out = pose.clone();
            } else {
                let local_weight = *weight / next;
                out.blend_in_place(pose, local_weight);
            }
            accumulated = next;
        }
        out
    }
}
