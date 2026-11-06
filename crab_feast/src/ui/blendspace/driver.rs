//! 运行时：把每个「启用的混合系统」的参数求值成姿态，依次写到预览角色骨骼上。
//!
//! 多个系统同时启用时按注册顺序叠加，后注册的覆盖前面同名骨骼的旋转/位移。

use std::collections::HashMap;

use bevy::{
    animation::{AnimatedBy, AnimationClip, AnimationTargetId},
    prelude::*,
};

use crab_feast_library::anim::{
    evaluate_space, pose::BonePose, ClipMeta, ClipSet, SpaceEvalContext,
};

use crate::ui::preview::{PreviewCatalog, PreviewRuntime, PreviewState};

use super::{registry::MixerCache, SpaceEditor};

#[derive(Resource, Default)]
pub struct PoseDriver {
    pub skeleton: Option<Skeleton>,
    /// 每个混合器各自的归一化相位
    pub phases: HashMap<String, f32>,
    pub weights: Vec<(String, f32)>,
    pub issues: Vec<String>,
    pub root_motion: bool,
    pub point: [f32; 2],
}

pub struct Skeleton {
    model: Entity,
    by_id: HashMap<AnimationTargetId, Entity>,
    rest: HashMap<AnimationTargetId, BonePose>,
    targets: Vec<AnimationTargetId>,
    hips: Option<AnimationTargetId>,
    base_translation: Vec3,
}

/// 预览角色切换后重建按名字索引的剪辑集合，并把 manifest 里的
/// 步幅 / 步态事件写进 `ClipSet`。
///
/// 相位对齐不再是固定的常数偏移，而是给每个剪辑存一组事件
/// `[左脚触地, 左脚离地, 右脚触地, 右脚离地]`；运行时按当前权重对这些事件
/// 做环形加权平均，得到统一时间轴，再分段重映射每个剪辑的相位。
pub(super) fn sync_clip_set(
    runtime: Res<PreviewRuntime>,
    catalog: Res<PreviewCatalog>,
    preview: Res<PreviewState>,
    mut clips: ResMut<ClipSet>,
) {
    if runtime.clips.is_empty() {
        return;
    }
    let character = catalog.character(preview.character);
    let name = character.map(|def| def.name.as_str()).unwrap_or("");
    let fingerprint = runtime.clips.keys().cloned().collect::<Vec<_>>().join("|");
    let key = format!("{name}|{fingerprint}");
    if clips.rig.as_deref() == Some(key.as_str()) {
        return;
    }
    clips.rebuild(key);
    for (clip, handle) in &runtime.clips {
        clips.insert(clip.clone(), handle.clone());
    }

    let Some(def) = character else {
        return;
    };

    let mut with_events = 0usize;
    for clip in runtime.clips.keys() {
        let Some(gait) = def.gaits.get(clip) else {
            continue;
        };
        let events = gait.events();
        if events[0].is_some() && events[2].is_some() {
            with_events += 1;
        }
        clips.set_meta(
            clip.clone(),
            ClipMeta {
                // 0 表示由 ClipSet::meta 回落到 AnimationClip 自身时长
                duration: 0.0,
                looped: true,
                stride: gait.stride,
                gait_events: events,
            },
        );
    }
    info!(
        "{name}: {} 个剪辑写入步态元数据，其中 {} 个有完整的触地事件",
        def.gaits.len(),
        with_events
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn drive_pose(
    mut driver: ResMut<PoseDriver>,
    editor: Res<SpaceEditor>,
    cache: Res<MixerCache>,
    catalog: Res<PreviewCatalog>,
    preview: Res<PreviewState>,
    runtime: Res<PreviewRuntime>,
    clips: Res<ClipSet>,
    clip_assets: Res<Assets<AnimationClip>>,
    time: Res<Time>,
    children: Query<&Children>,
    targets: Query<(&AnimationTargetId, Option<&AnimatedBy>)>,
    names: Query<&Name>,
    mut transforms: Query<&mut Transform>,
    mut last_log: Local<usize>,
    // (evaluate_space 累计微秒, 写骨骼累计微秒, 帧数)
    mut stats: Local<(f64, f64, u32)>,
) {
    let Some(model) = runtime.model else {
        return;
    };

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
        root_motion,
        point,
    } = &mut *driver;

    let Some(skeleton) = skeleton.as_ref() else {
        return;
    };
    let character = catalog
        .character(preview.character)
        .map(|def| def.name.clone())
        .unwrap_or_default();

    let mut active: Vec<String> = Vec::new();
    let mut last_weights: Vec<(String, f32)> = Vec::new();
    let mut last_issues: Vec<String> = Vec::new();
    let mut last_point = [0.0, 0.0];
    let mut last_root_delta = Vec3::ZERO;
    let mut any = false;
    let mut apply_micros = 0.0f64;

    // 预览打开时只驱动当前选中的混合系统；关闭编辑器后按「启用」顺序驱动
    let selected: Vec<&super::registry::MixerSnapshot> = if editor.open {
        cache.mixers.get(editor.mixer).into_iter().collect()
    } else {
        cache
            .mixers
            .iter()
            .filter(|mixer| mixer.enabled && mixer.character == character)
            .collect()
    };

    for mixer in selected {
        any = true;
        active.push(mixer.title.clone());
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

        let eval_start = std::time::Instant::now();
        let output = evaluate_space(&mixer.space, &mut context);
        stats.0 += eval_start.elapsed().as_secs_f64() * 1e6;

        let apply_start = std::time::Instant::now();
        for (id, bone) in output.pose.iter() {
            if let Some(entity) = skeleton.by_id.get(id) {
                if let Ok(mut transform) = transforms.get_mut(*entity) {
                    bone.apply_to(&mut transform);
                }
            }
        }
        apply_micros += apply_start.elapsed().as_secs_f64() * 1e6;

        last_weights = output.clip_weights.clone();
        last_issues = output.issues.clone();
        last_point = output.point;
        last_root_delta = output.root_delta;
    }

    *weights = last_weights;
    *issues = last_issues;
    *point = last_point;

    if any {
        if !*root_motion {
            // 关闭根运动时把 hips 的水平位移锁回静息位置，避免人物离开展台
            if let Some(hips) = skeleton.hips {
                if let (Some(rest), Some(entity)) = (skeleton.rest.get(&hips), skeleton.by_id.get(&hips)) {
                    if let Ok(mut transform) = transforms.get_mut(*entity) {
                        transform.translation.x = rest.translation.x;
                        transform.translation.z = rest.translation.z;
                    }
                }
            }
        } else if let Ok(mut transform) = transforms.get_mut(model) {
            transform.translation =
                skeleton.base_translation + Vec3::new(last_root_delta.x, 0.0, last_root_delta.z);
        }
    }

    if any {
        stats.2 += 1;
    }
    stats.1 += apply_micros;

    if debug_enabled() {
        let second = time.elapsed_secs() as usize;
        if second != *last_log {
            *last_log = second;
            let frames = stats.2.max(1) as f64;
            let eval_us = stats.0 / frames;
            let apply_us = stats.1 / frames;
            info!(
                "[blendspace] t={}s point={:?} weights={:?} eval={:.0}us apply={:.0}us 上限{:.0}fps",
                second,
                last_point,
                weights,
                eval_us,
                apply_us,
                if eval_us > 0.0 { 1e6 / eval_us } else { 0.0 }
            );
            *stats = (0.0, 0.0, 0);
        }
    }
}

fn debug_enabled() -> bool {
    std::env::var("CRAB_ANIM_DEBUG").is_ok()
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
            rest.insert(*id, BonePose::from_transform(&transform));
        }
    }

    let mut hips = None;
    for (id, name) in &name_map {
        if name.contains("Hips") && !name.contains("Ctrl") && !name.contains("Helper") {
            hips = Some(*id);
            break;
        }
    }

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
