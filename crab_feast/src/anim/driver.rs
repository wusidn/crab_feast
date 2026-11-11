//! 姿态驱动：把混合器快照求值成骨骼姿态，写进当前角色的骨架。
//!
//! 这是全项目唯一用混合空间写骨骼的地方。菜单预览和游戏运行时跑的是同一个系统、
//! 同一份共享相位，所以「预览里看到的」和「游戏里跑出来的」必然一致。
//!
//! 多个混合器同时生效时按注册顺序叠加：后面的覆盖前面同名骨骼的旋转/位移。

use std::collections::HashMap;

use super::character::{AnimCatalog, AnimRuntime, AnimSelection};
use super::core::pose::BonePose;
use super::core::{ClipSet, SpaceEvalContext, evaluate_space};
use super::mixers::MixerCache;
use bevy::{
    animation::{AnimatedBy, AnimationClip, AnimationTargetId},
    prelude::*,
};

/// 骨骼与参数此刻归谁。优先级：单条剪辑预览 > 被聚焦的混合器 > 「启用」的全部混合器。
#[derive(Resource, Default)]
pub struct AnimFocus {
    pub mixer: Option<String>,
    /// 动画预览页停在页面上：骨骼让给 AnimationPlayer 的单条剪辑
    pub preview: bool,
}

impl AnimFocus {
    /// UI 打开某个混合器时调用：该混合器的参数归编辑器所有
    pub fn focus(&mut self, id: &str) {
        if self.mixer.as_deref() != Some(id) {
            self.mixer = Some(id.to_string());
        }
    }

    /// 混合空间编辑器关掉：参数所有权交回游戏代码
    pub fn clear(&mut self) {
        if self.mixer.take().is_some() {
            info!("混合空间编辑器关闭，参数交还游戏代码");
        }
    }

    /// 某个混合器的参数此刻归谁：编辑器打开时游戏代码不要写它
    pub fn is_focused(&self, id: &str) -> bool {
        self.mixer.as_deref() == Some(id)
    }

    /// 动画预览页进出时调用：为真时混合空间不再写骨骼，
    /// 求值照旧跑（权重与地速保持新鲜），但姿态归单条剪辑播放。
    pub fn set_preview(&mut self, on: bool) {
        if self.preview != on {
            self.preview = on;
            let owner = if on {
                "单条剪辑播放"
            } else {
                "混合空间"
            };
            info!(
                "动画预览{}，骨骼交给{owner}",
                if on { "开启" } else { "关闭" }
            );
        }
    }
}

/// 当前角色的骨架与最近一次求值结果（供 UI 显示）
#[derive(Resource, Default)]
pub struct PoseDriver {
    pub skeleton: Option<Skeleton>,
    /// 每个混合器各自的归一化相位
    pub phases: HashMap<String, f32>,
    pub weights: Vec<(String, f32)>,
    pub issues: Vec<String>,
    pub point: [f32; 2],
    /// 当前混合输出的等效地速（米/秒），角色实际移动速度按它走
    pub ground_speed: f32,
    /// 本帧是否求值过混合空间（权重与地速这些数值可用）
    pub evaluated: bool,
    /// 上一帧是否真的由混合空间写了姿态（单条剪辑的根运动要让位）
    pub active: bool,
}

pub struct Skeleton {
    model: Entity,
    by_id: HashMap<AnimationTargetId, Entity>,
    rest: HashMap<AnimationTargetId, BonePose>,
    targets: Vec<AnimationTargetId>,
    hips: Option<AnimationTargetId>,
    base_translation: Vec3,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn drive_pose(
    mut driver: ResMut<PoseDriver>,
    focus: Res<AnimFocus>,
    cache: Res<MixerCache>,
    catalog: Res<AnimCatalog>,
    selection: Res<AnimSelection>,
    runtime: Res<AnimRuntime>,
    clips: Res<ClipSet>,
    clip_assets: Res<Assets<AnimationClip>>,
    time: Res<Time>,
    children: Query<&Children>,
    targets: Query<(&AnimationTargetId, Option<&AnimatedBy>)>,
    names: Query<&Name>,
    mut transforms: Query<&mut Transform>,
) {
    let Some(model) = runtime.model else {
        return;
    };

    // 模型换了就重建骨架：实体 id 全变了，缓存的映射没有意义
    if driver.skeleton.as_ref().map(|skeleton| skeleton.model) != Some(model) {
        driver.skeleton = build_skeleton(
            model,
            &children,
            &targets,
            &names,
            &mut transforms,
            &clips,
            &clip_assets,
        );
        driver.phases.clear();
    }

    let PoseDriver {
        skeleton,
        phases,
        weights,
        issues,
        point,
        ground_speed,
        evaluated,
        active,
    } = &mut *driver;

    // 预览单条剪辑时只求值不写骨骼：地速与权重照旧新鲜，姿态归 AnimationPlayer
    let preview = focus.preview;

    let Some(skeleton) = skeleton.as_ref() else {
        return;
    };
    let character = catalog
        .character(selection.character)
        .map(|def| def.name.clone())
        .unwrap_or_default();

    let mut last_weights: Vec<(String, f32)> = Vec::new();
    let mut last_issues: Vec<String> = Vec::new();
    let mut last_point = [0.0, 0.0];
    let mut last_root_delta = Vec3::ZERO;
    let mut last_ground_speed = 0.0;
    let mut any = false;

    // 预览聚焦时只驱动那一个；关闭编辑器后按「启用」顺序驱动当前角色的混合器
    let selected: Vec<&_> = match focus.mixer.as_deref() {
        Some(id) => cache
            .mixers
            .iter()
            .filter(|mixer| mixer.key == id)
            .collect(),
        None => cache
            .mixers
            .iter()
            .filter(|mixer| mixer.enabled && mixer.character == character)
            .collect(),
    };

    for mixer in selected {
        any = true;
        let phase = phases.entry(mixer.key.clone()).or_insert(0.0);
        let mut context = SpaceEvalContext {
            params: &mixer.params,
            clips: &clips,
            clip_assets: &clip_assets,
            targets: &skeleton.targets,
            rest: &skeleton.rest,
            phase,
            dt: time.delta_secs(),
            speed_scale: 1.0,
            hips: skeleton.hips,
        };

        let output = evaluate_space(&mixer.space, &mut context);

        if !preview {
            for (id, bone) in output.pose.iter() {
                if let Some(entity) = skeleton.by_id.get(id)
                    && let Ok(mut transform) = transforms.get_mut(*entity)
                {
                    bone.apply_to(&mut transform);
                }
            }
        }
        last_weights = output.clip_weights.clone();
        last_issues = output.issues.clone();
        last_point = output.point;
        last_root_delta = output.root_delta;
        // 多个混合器叠加时取最后一个求值者的地速：真正带动位移的总是它
        last_ground_speed = output.ground_speed;
    }

    *weights = last_weights;
    *issues = last_issues;
    *point = last_point;
    *ground_speed = if any { last_ground_speed } else { 0.0 };
    *evaluated = any;
    // 没写骨骼就不能声称自己拥有姿态，否则单条剪辑的根运动补偿会一直让位
    *active = any && !preview;

    if !any || preview {
        return;
    }
    if !selection.root_motion {
        // 关闭根运动时把 hips 的水平位移锁回静息位置，避免人物离开展台
        if let Some(hips) = skeleton.hips
            && let (Some(rest), Some(entity)) =
                (skeleton.rest.get(&hips), skeleton.by_id.get(&hips))
            && let Ok(mut transform) = transforms.get_mut(*entity)
        {
            transform.translation.x = rest.translation.x;
            transform.translation.z = rest.translation.z;
        }
    } else if let Ok(mut transform) = transforms.get_mut(model) {
        // 绝对定位而不是累加：root_delta 已经是相对静息位置的位移
        transform.translation =
            skeleton.base_translation + Vec3::new(last_root_delta.x, 0.0, last_root_delta.z);
    }
}

#[allow(clippy::too_many_arguments)]
fn build_skeleton(
    model: Entity,
    children: &Query<&Children>,
    targets: &Query<(&AnimationTargetId, Option<&AnimatedBy>)>,
    names: &Query<&Name>,
    transforms: &mut Query<&mut Transform>,
    clips: &ClipSet,
    clip_assets: &Assets<AnimationClip>,
) -> Option<Skeleton> {
    let mut by_id: HashMap<AnimationTargetId, Entity> = HashMap::new();
    let mut name_map: HashMap<AnimationTargetId, String> = HashMap::new();
    let mut stack = vec![model];

    while let Some(entity) = stack.pop() {
        if let Ok(list) = children.get(entity) {
            stack.extend(list.iter());
        }
        if let Ok((target_id, _)) = targets.get(entity) {
            by_id.insert(*target_id, entity);
            if let Ok(name) = names.get(entity) {
                name_map.insert(*target_id, name.as_str().to_string());
            }
        }
    }

    // 老模型可能没有 AnimationTarget（未挂 AnimatedBy），退回按名字建 target
    if by_id.is_empty() {
        let mut stack = vec![model];
        while let Some(entity) = stack.pop() {
            if let Ok(list) = children.get(entity) {
                stack.extend(list.iter());
            }
            if let Ok(name) = names.get(entity) {
                let id = AnimationTargetId::from_name(name);
                by_id.insert(id, entity);
                name_map.insert(id, name.as_str().to_string());
            }
        }
    }

    if by_id.is_empty() {
        warn!("模型 {model:?} 下没有找到动画 target，无法驱动姿态");
        return None;
    }

    let mut targets_vec = Vec::with_capacity(by_id.len());
    let mut rest = HashMap::with_capacity(by_id.len());
    for (id, entity) in &by_id {
        targets_vec.push(*id);
        if let Ok(transform) = transforms.get(*entity) {
            rest.insert(*id, BonePose::from_transform(transform));
        }
    }

    let mut hips = None;
    for (id, name) in &name_map {
        if name.contains("Hips") && !name.contains("Ctrl") && !name.contains("Helper") {
            hips = Some(*id);
            break;
        }
    }

    // 覆盖率日志：剪辑只覆盖部分骨骼时，剩下的会保持 rest，肉眼能看出断层
    let coverage = clips
        .ids()
        .next()
        .and_then(|clip| clips.handle(clip))
        .and_then(|handle| clip_assets.get(handle))
        .map(|clip| {
            targets_vec
                .iter()
                .filter(|id| clip.curves_for_target(**id).is_some())
                .count()
        })
        .unwrap_or(0);
    info!(
        "混合空间骨架：{} 个 target，首个剪辑覆盖 {} 个，hips={:?}",
        targets_vec.len(),
        coverage,
        hips.is_some()
    );

    let base_translation = transforms
        .get(model)
        .map(|transform| transform.translation)
        .unwrap_or(Vec3::ZERO);

    Some(Skeleton {
        model,
        by_id,
        rest,
        targets: targets_vec,
        hips,
        base_translation,
    })
}
