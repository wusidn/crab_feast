//! 姿态驱动：把混合器快照求值成骨骼姿态，写进当前角色的骨架。
//!
//! 这是全项目唯一用混合空间写骨骼的地方。菜单预览和游戏运行时跑的是同一个系统、
//! 同一份共享相位，所以「预览里看到的」和「游戏里跑出来的」必然一致。
//!
//! 多个混合器同时生效时按注册顺序叠加：后面的覆盖前面同名骨骼的旋转/位移。

use std::collections::HashMap;

use super::character::{AnimCatalog, AnimRuntime, AnimSelection};
use super::core::pose::{BonePose, Pose};
use super::core::{ClipSet, SpaceEvalContext, evaluate_space};
use super::jump::JumpAction;
use super::mixers::{LocomotionMixer, MixerCache, RegisteredMixer};
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
#[derive(Resource)]
pub struct PoseDriver {
    pub(crate) skeleton: Option<Skeleton>,
    /// 每个混合器各自的归一化相位
    pub phases: HashMap<String, f32>,
    pub weights: Vec<(String, f32)>,
    pub issues: Vec<String>,
    pub point: [f32; 2],
    /// 当前混合输出的等效地速（米/秒），角色实际移动速度按它走
    pub ground_speed: f32,
    /// 当前混合输出的两只脚支撑权重 `[左, 右]`（0 = 空中，1 = 踩实），
    /// 脚部 IK 用它决定什么时候把脚钉在地上
    pub contacts: [f32; 2],
    /// 姿态档位被「大转角下限」抬起来后的补偿系数：`真实档位 / 显示档位`。
    /// 物理速度按它折算回去，抬档只改观感、不改走多远。
    pub pose_gain: f32,
    /// 本帧是否求值过混合空间（权重与地速这些数值可用）
    pub evaluated: bool,
    /// 上一帧是否真的由混合空间写了姿态（单条剪辑的根运动要让位）
    pub active: bool,
    /// 诊断：上一帧送进混合空间的 yaw 轴值（抓"姿态突然翻一大块"）
    pub last_pose_yaw: f32,
    /// 诊断：距离上次打姿态日志过了多久
    pub pose_log_secs: f32,
}

impl Default for PoseDriver {
    fn default() -> Self {
        Self {
            skeleton: None,
            phases: HashMap::default(),
            weights: Vec::new(),
            issues: Vec::new(),
            point: [0.0; 2],
            ground_speed: 0.0,
            contacts: [0.0; 2],
            pose_gain: 1.0,
            evaluated: false,
            active: false,
            last_pose_yaw: 0.0,
            pose_log_secs: 0.0,
        }
    }
}

pub(crate) struct Skeleton {
    pub(crate) model: Entity,
    pub(crate) by_id: HashMap<AnimationTargetId, Entity>,
    pub(crate) rest: HashMap<AnimationTargetId, BonePose>,
    pub(crate) targets: Vec<AnimationTargetId>,
    pub(crate) hips: Option<AnimationTargetId>,
    pub(crate) base_translation: Vec3,
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
    jump: Res<JumpAction>,
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
        contacts,
        // 只读：补偿系数由 `drive_locomotion` 写，姿态求值本身不动它
        pose_gain: _,
        evaluated,
        active,
        last_pose_yaw: _,
        pose_log_secs: _,
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
    let mut last_contacts = [0.0; 2];
    // 最后一个生效者的姿态：动作层要在它之上叠加，最后统一写一次骨骼
    let mut last_pose: Option<Pose> = None;
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
        // 动作层接管期间移动相位必须由动作层说话，不能让混合器自己走：蓄力 /
        // 滞空段一脚都没着地，相位爱怎么走都无所谓，但落地恢复段要是放它自走
        // 0.24s 就走掉 0.343 个周期（比整段支撑期 0.208 还长），交接时支撑会换到
        // 另一只脚，看上去就是「步频少了一拍」。
        //
        // 恢复段的相位由 jump/landing.rs 每帧写（`follow_landing_phase`）：起点退到
        // 动作末帧那只着地脚的支撑位置之前一个恢复段，再按移动剪辑的原生速度
        // 往前推，交回那一刻正好落在着地脚的支撑位置上。这里只负责别让它偷跑。
        let hold = jump.is_active() && mixer.key == LocomotionMixer::ID;
        let mut context = SpaceEvalContext {
            params: &mixer.params,
            clips: &clips,
            clip_assets: &clip_assets,
            targets: &skeleton.targets,
            rest: &skeleton.rest,
            phase,
            dt: time.delta_secs(),
            speed_scale: if hold { 0.0 } else { 1.0 },
            hips: skeleton.hips,
        };

        let output = evaluate_space(&mixer.space, &mut context);

        last_weights = output.clip_weights;
        last_issues = output.issues;
        last_point = output.point;
        last_root_delta = output.root_delta;
        last_contacts = output.contacts;
        // 多个混合器叠加时取最后一个求值者的地速：真正带动位移的总是它
        last_ground_speed = output.ground_speed;
        last_pose = Some(output.pose);
    }

    *weights = last_weights;
    *issues = last_issues;
    *point = last_point;
    *ground_speed = if any { last_ground_speed } else { 0.0 };
    // 动作层接管期间两只脚都不该钉地：脚 IK 的落锁判据来自这里，
    // 否则蓄力和落地时会「钉住一只、放开另一只」
    *contacts = if any && !jump.is_active() {
        last_contacts
    } else {
        [0.0; 2]
    };
    *evaluated = any;
    // 没写骨骼就不能声称自己拥有姿态，否则单条剪辑的根运动补偿会一直让位
    *active = any && !preview;

    if !any || preview {
        return;
    }

    if let Some(mut pose) = last_pose {
        super::jump::apply_action(
            &jump,
            &clips,
            &clip_assets,
            &skeleton.targets,
            &skeleton.rest,
            &mut pose,
        );
        for (id, bone) in pose.iter() {
            if let Some(entity) = skeleton.by_id.get(id)
                && let Ok(mut transform) = transforms.get_mut(*entity)
            {
                bone.apply_to(&mut transform);
            }
        }
        // 空中把髋的竖直平移钉在起跳高度：角色升高由胶囊根承担（见 jump/drive.rs）
        if jump.is_airborne()
            && let Some(hips) = skeleton.hips
            && let Some(entity) = skeleton.by_id.get(&hips)
            && let Ok(mut transform) = transforms.get_mut(*entity)
        {
            transform.translation.y = jump.hips_freeze;
        }
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
        // 模型根节点的局部旋转恒为单位：偏航由父节点 Avatar 承担。混合空间接管
        // 姿态时也要在这里清一次，免得预览/单条剪辑路径留下的偏航继续挂着
        transform.rotation = Quat::IDENTITY;
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_skeleton(
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
