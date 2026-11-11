//! 脚部 IK：支撑期把脚踝钉在世界地面上，转身 / 侧移 / 起停的残差全由腿吸收。
//!
//! 为什么需要它：混合空间描述的是**相对身体**的脚部轨迹，而地面是世界的。
//! 身体朝向与真实速度方向不一致时（转身最明显），支撑脚在地面上会多走一小段，
//! 观感就是「轻微滑步」。纯数学的求解器在 `crab_feast_library::ik`，
//! 这里只负责把它接进 Bevy 的姿态管线。
//!
//! 执行位置：`PostUpdate`，在 `drive_pose`（动画写骨骼）之后、
//! `TransformSystems::Propagate`（算世界矩阵）之前。所以本系统读到的还是**本帧**
//! 的局部变换；世界矩阵自己按父子链乘出来，不依赖上一帧的 `GlobalTransform`——
//! 转身时那一帧的滞后就会变成肉眼可见的脚滑。
//!
//! 为什么是逐实体而不是 Resource：
//! - 锚点、淡入权重、骨架缓存全部挂在模型实体上，一个人一份；
//! - 联机时「所有可见角色」= 所有带 [`FootIk`] 的实体，系统按查询逐个求解，
//!   不需要知道谁是本地玩家，也不会互相踩状态；
//! - 关闭一个角色只是把 `enabled` 置 false：`blend` 会在 `blend_secs` 内淡出，
//!   淡到 0 后整段求解被跳过（每帧只剩一次分支）。重新打开不需要重建任何东西，
//!   从下一次支撑重新贴地即可，所以可以任意高频地开关；
//! - 抬脚解除锁定时**不是**立刻回到动画姿态：`FootLock` 把还没还回去的修正量
//!   按 `FootIkSettings::release_secs` 衰减到 0，脚踝是平滑追上动画，
//!   否则支撑期攒下的几厘米修正量会在当帧消失，看起来就是「卡一下」。

use bevy::math::Affine3A;
use bevy::prelude::*;
use bevy::transform::TransformSystems;
use crab_feast_library::ik::{
    aim_world_rotation, solve_two_bone, FootContact, FootLock, FootLockConfig,
};

use crate::GameState;

use super::character::{AnimModelRoot, AnimRuntime};
use super::driver::{AnimFocus, PoseDriver, drive_pose};

pub struct FootIkPlugin;

/// 全局参数。逐帧读取，所以改这里的值当帧生效；联机时每个角色可以再各自
/// 用 [`FootIk::enabled`] 做二级开关。
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct FootIkSettings {
    /// 总开关。关掉时所有角色的 `blend` 都会平滑降到 0，求解整段跳过
    pub enabled: bool,
    /// 目标权重 0→1 的过渡时长（秒）。0 表示硬切（不推荐：会看到膝盖瞬移）
    pub blend_secs: f32,
    /// 锚点最多允许偏离脚踝多少米，超过就让锚点跟着脚走
    pub max_correction: f32,
    /// 支撑权重高于它才算踩实
    pub plant_enter: f32,
    /// 支撑权重低于它才算抬脚
    pub plant_exit: f32,
    /// 抬脚解除锁定后，修正量衰减回动画姿态的时间常数（秒）。
    /// 0 = 当帧硬切（会看到脚「卡」一下），默认 0.06
    pub release_secs: f32,
    /// 刚踩实时锁脚强度从 0 升到 1 的时间（秒）。
    /// 0 = 一踩实就钉死（脚的速度一帧内被掐断，同样会「卡」），默认 0.04
    pub engage_secs: f32,
    /// 支撑期里动画把脚跟抬起多少米之后，承重点从踝滑到前掌（绕前掌转）。
    /// 0 = 从踩实第一帧就钉前掌（脚跟会过早抬起），默认 0.03
    pub toe_pivot_rise: f32,
}

impl Default for FootIkSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            // 80ms 大约是 5 帧：足够盖住膝盖的一次跳变，又快到跟手
            blend_secs: 0.08,
            max_correction: 0.22,
            plant_enter: 0.55,
            plant_exit: 0.25,
            release_secs: 0.06,
            engage_secs: 0.04,
            toe_pivot_rise: 0.03,
        }
    }
}

impl FootIkSettings {
    fn lock_config(&self) -> FootLockConfig {
        FootLockConfig {
            max_correction: self.max_correction.max(0.0),
            plant_enter: self.plant_enter.clamp(0.0, 1.0),
            plant_exit: self.plant_exit.clamp(0.0, self.plant_enter),
            release_secs: self.release_secs.max(0.0),
            engage_secs: self.engage_secs.max(0.0),
            toe_pivot_rise: self.toe_pivot_rise.max(0.0),
        }
    }
}

/// 每个角色模型一份的脚部 IK 状态。
#[derive(Component)]
pub struct FootIk {
    /// 本角色是否参与 IK。随时可改；关掉后 `blend_secs` 内平滑退出
    pub enabled: bool,
    /// 动画层每帧写入的两只脚支撑权重 `[左, 右]`，0 = 空中，1 = 踩实。
    ///
    /// 当前单人动画栈由 [`feed_contacts`] 从 [`PoseDriver`] 灌进来；
    /// 联机时每个角色各自的动画层往自己的组件里写就行，求解端不用改。
    pub contacts: [f32; 2],
    /// 骨架缓存：模型换掉时实体 id 全变，跟着模型实体一起销毁
    rig: Option<FootRig>,
    /// 当前生效权重（0..1），用于开关与模式切换的平滑过渡
    blend: f32,
    /// 两只脚各自的锁脚状态
    locks: [FootLock; 2],
    /// 最近一帧的锚点与修正量，调试面板 / 测试读取
    anchors: [Option<Vec3>; 2],
    correction: [f32; 2],
}

impl Default for FootIk {
    fn default() -> Self {
        Self {
            enabled: true,
            contacts: [0.0; 2],
            rig: None,
            blend: 0.0,
            locks: [FootLock::default(); 2],
            anchors: [None; 2],
            correction: [0.0; 2],
        }
    }
}

impl FootIk {
    /// 骨架缓存是否就绪（模型还在异步加载时为 false）
    pub fn is_ready(&self) -> bool {
        self.rig.is_some()
    }

    /// 当前生效权重
    pub fn blend(&self) -> f32 {
        self.blend
    }

    /// 两只脚最近一帧的世界锚点
    #[allow(dead_code)] // 调试/测试读数用，UI 面板暂未展示
    pub fn anchors(&self) -> [Option<Vec3>; 2] {
        self.anchors
    }

    /// 两只脚最近一帧被拉动的距离（米）
    pub fn correction(&self) -> [f32; 2] {
        self.correction
    }

    /// 两只脚当前承重点沿脚掌滑到哪了（0 = 脚踝，1 = 前掌）
    pub fn roll(&self) -> [f32; 2] {
        [self.locks[0].roll(), self.locks[1].roll()]
    }

    /// 两只脚是否处于支撑期
    pub fn planted(&self) -> [bool; 2] {
        [self.locks[0].is_planted(), self.locks[1].is_planted()]
    }

}

/// 模型实体下的骨架缓存。实体 id 与模型同生共死，不需要额外失效判断。
struct FootRig {
    /// 从场景最顶层到 hips（含）的链：用局部变换自己乘出世界矩阵，
    /// 不依赖尚未传播的 `GlobalTransform`
    spine: Vec<Entity>,
    /// 两条腿 `[左, 右]`，顺序与 [`FootIk::contacts`] 一致
    legs: [Leg; 2],
}

#[derive(Clone, Copy)]
struct Leg {
    upper: Entity,
    lower: Entity,
    foot: Entity,
    /// 前掌关节（`*ToeBase`）。有它承重点才能在支撑末期滑到前掌
    toe: Option<Entity>,
}

impl Plugin for FootIkPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FootIkSettings>()
            .add_systems(
                Update,
                attach_rigs.run_if(in_state(GameState::Game)),
            )
            .add_systems(
                PostUpdate,
                (feed_contacts, solve_foot_ik)
                    .chain()
                    .after(drive_pose)
                    .before(TransformSystems::Propagate)
                    .run_if(in_state(GameState::Game)),
            );
    }
}

/// 给每个角色模型挂上 [`FootIk`]，并趁骨骼已加载后缓存腿链。
///
/// 骨骼比场景本体晚一点出现，所以这里每帧重试一次；缓存成功后
/// `rig.is_some()` 直接短路，正常开销只有一次判断。
fn attach_rigs(
    mut commands: Commands,
    models: Query<Entity, (With<AnimModelRoot>, Without<FootIk>)>,
    mut iks: Query<(Entity, &mut FootIk)>,
    children: Query<&Children>,
    names: Query<&Name>,
    parents: Query<&ChildOf>,
) {
    for model in &models {
        commands.entity(model).insert(FootIk::default());
    }

    for (model, mut ik) in &mut iks {
        if ik.rig.is_some() {
            continue;
        }
        if let Some(rig) = build_rig(model, &children, &names, &parents) {
            info!("脚部 IK 骨架就绪：{model:?}，{} 节脊椎链", rig.spine.len());
            ik.rig = Some(rig);
        }
    }
}

/// 在当前动画栈里把两脚支撑权重灌给正在被驱动的那个角色。
///
/// 这是单人动画栈（`AnimRuntime` / `PoseDriver` 都是 Resource）的桥；
/// 联机版本里每个角色各有一套动画求值，直接写自己的 [`FootIk::contacts`]，
/// 求解系统不需要任何改动。
fn feed_contacts(
    focus: Res<AnimFocus>,
    driver: Res<PoseDriver>,
    runtime: Res<AnimRuntime>,
    mut iks: Query<&mut FootIk>,
) {
    let Some(model) = runtime.model else {
        return;
    };
    let Ok(mut ik) = iks.get_mut(model) else {
        return;
    };
    // 单条剪辑预览时骨骼归 AnimationPlayer，混合空间的支撑权重不代表当前姿态
    ik.contacts = if focus.preview {
        [0.0; 2]
    } else {
        driver.contacts
    };
}

/// 逐角色求解：读本帧姿态 → 锁脚目标 → 两骨 IK → 写回腿部旋转。
fn solve_foot_ik(
    time: Res<Time>,
    settings: Res<FootIkSettings>,
    mut iks: Query<&mut FootIk>,
    mut transforms: Query<&mut Transform>,
) {
    let dt = time.delta_secs();
    let config = settings.lock_config();

    'character: for mut ik in &mut iks {
        // 目标权重：全局开 + 本角色开。关掉时平滑淡出而不是直接跳没
        let want = if settings.enabled && ik.enabled { 1.0 } else { 0.0 };
        let step = if settings.blend_secs > f32::MIN_POSITIVE {
            dt / settings.blend_secs
        } else {
            1.0
        };
        ik.blend = approach(ik.blend, want, step.max(0.0));
        let blend = ik.blend;
        let on = want > 0.0;

        let FootIk {
            rig,
            locks,
            anchors,
            correction,
            contacts,
            ..
        } = &mut *ik;

        // 骨架还没就绪：这一帧什么都不做（模型加载完会自己重试）
        let Some(rig) = rig.as_ref() else {
            continue;
        };

        // 彻底关掉：清锚点，重新打开时从下一次支撑重新贴地，
        // 不会拿几帧前的旧位置把人拽回去
        if !on && blend <= 0.0 {
            locks[0].reset();
            locks[1].reset();
            *anchors = [None; 2];
            *correction = [0.0; 2];
            continue;
        }

        // ---- 读：自己把局部变换乘成世界矩阵（本帧的骨骼姿态）----
        // 脊椎链上偶尔会有纯组织节点（没有 Transform），按单位矩阵处理；
        // 腿骨则在下面严格校验：缺一节就整段跳过，不拿错的骨骼去解 IK。
        let mut spine_world = Affine3A::IDENTITY;
        let mut spine_rot = Quat::IDENTITY;
        for &entity in &rig.spine {
            let local = transforms
                .get(entity)
                .map(|transform| *transform)
                .unwrap_or(Transform::IDENTITY);
            spine_world *= local.compute_affine();
            spine_rot *= local.rotation;
        }

        let mut poses: [Option<LegPose>; 2] = [None; 2];
        for (index, leg) in rig.legs.iter().enumerate() {
            let Some(upper) = transforms.get(leg.upper).ok().map(|transform| *transform) else {
                continue 'character;
            };
            let Some(lower) = transforms.get(leg.lower).ok().map(|transform| *transform) else {
                continue 'character;
            };
            let Some(foot) = transforms.get(leg.foot).ok().map(|transform| *transform) else {
                continue 'character;
            };

            let upper_world = spine_world * upper.compute_affine();
            let lower_world = upper_world * lower.compute_affine();
            let foot_world = lower_world * foot.compute_affine();
            // 前掌：脚骨的世界矩阵 × 前掌骨局部变换
            let toe = leg
                .toe
                .and_then(|entity| transforms.get(entity).ok().map(|transform| *transform))
                .map(|transform| (foot_world * transform.compute_affine()).translation.into());

            poses[index] = Some(LegPose {
                root: upper_world.translation.into(),
                joint: lower_world.translation.into(),
                end: foot_world.translation.into(),
                toe,
                parent_rot: spine_rot,
                upper_rot: spine_rot * upper.rotation,
                lower_rot: spine_rot * upper.rotation * lower.rotation,
                foot_rot: spine_rot * upper.rotation * lower.rotation * foot.rotation,
            });
        }

        // ---- 解：逐腿算目标、求解、写回旋转 ----
        for (index, pose) in poses.into_iter().enumerate() {
            let Some(mut pose) = pose else {
                continue;
            };

            let support = contacts[index];
            // 目标位置里已经带着「解除锁定后还没还完的残差」，
            // 所以抬脚是平滑追上动画，不会从锚点瞬移回去
            let (lock_target, lock_weight) = locks[index].update(
                FootContact {
                    ankle: pose.end,
                    toe: pose.toe,
                },
                support,
                on,
                dt,
                &config,
            );
            anchors[index] = locks[index].anchor();
            // 锁脚权重再乘全局淡入：任何开关/切换都在这里平滑掉
            let weight = (lock_weight * blend).clamp(0.0, 1.0);
            let target = pose.end.lerp(lock_target, weight);
            correction[index] = (target - pose.end).length();
            if weight <= 0.0 {
                continue;
            }

            // 前掌不穿地：钉着前掌时，动画如果把脚尖压到踩实时的高度以下，
            // 就绕踝在竖直平面里把脚抬起来（只改朝向，不动锚点）
            if let (Some(floor_y), Some(toe)) = (locks[index].toe_floor(), pose.toe) {
                // 前掌相对踝的偏移要换成「脚局部空间」：夹朝向时是绕踝转，不是再乘一次朝向
                let offset = pose.foot_rot.inverse() * (toe - pose.end);
                let clamped = clamp_toe_above_floor(pose.foot_rot, offset, target, floor_y);
                if clamped != pose.foot_rot {
                    // 夹紧强度跟着锁脚一起淡出：抬脚那一帧不能把脚掌朝向一把翻回去
                    let clamp_weight = weight * locks[index].presence();
                    pose.foot_rot = pose.foot_rot.lerp(clamped, clamp_weight);
                }
            }

            let Some(solved) = solve_leg(pose, target) else {
                continue;
            };
            let Some(leg) = rig.legs.get(index) else {
                continue;
            };
            if let Ok(mut transform) = transforms.get_mut(leg.upper) {
                transform.rotation = solved.upper_local;
            }
            if let Ok(mut transform) = transforms.get_mut(leg.lower) {
                transform.rotation = solved.lower_local;
            }
            if let Ok(mut transform) = transforms.get_mut(leg.foot) {
                transform.rotation = solved.foot_local;
            }
        }
    }
}

/// 一条腿求解前的姿态快照（全部是世界空间 / 世界旋转）。
#[derive(Clone, Copy)]
struct LegPose {
    root: Vec3,
    joint: Vec3,
    end: Vec3,
    /// 前掌关节（`*ToeBase`）的世界位置；骨架里没有就是 `None`
    toe: Option<Vec3>,
    /// 上段骨骼父节点的世界旋转（hips）
    parent_rot: Quat,
    upper_rot: Quat,
    lower_rot: Quat,
    foot_rot: Quat,
}

/// 一条腿求解后的局部旋转。
struct LegSolution {
    upper_local: Quat,
    lower_local: Quat,
    foot_local: Quat,
}

/// 纯函数：两骨求解 + 换算回局部旋转，方便单测直接验证「脚有没有落到目标上」。
///
/// 脚踝额外做一件事：**世界朝向保持原样**。IK 只挪位置，如果踝关节跟着小腿一起转，
/// 脚尖就会被转进地里；所以这里让脚的局部旋转反向补偿小腿的旋转。
fn solve_leg(pose: LegPose, target: Vec3) -> Option<LegSolution> {
    let solution = solve_two_bone(
        pose.root,
        pose.joint,
        pose.end,
        target,
        // 当前膝盖的偏移方向 = 动画原本的弯曲方向
        pose.joint - pose.root,
    )?;

    let upper_world = aim_world_rotation(
        pose.upper_rot,
        pose.joint - pose.root,
        solution.upper_dir,
    );
    let lower_world = aim_world_rotation(
        pose.lower_rot,
        pose.end - pose.joint,
        solution.lower_dir,
    );

    Some(LegSolution {
        upper_local: (pose.parent_rot.inverse() * upper_world).normalize(),
        // 下段的父节点是已经转过的上段：只能用新的世界旋转换算
        lower_local: (upper_world.inverse() * lower_world).normalize(),
        foot_local: (lower_world.inverse() * pose.foot_rot).normalize(),
    })
}

/// 把脚的世界朝向夹到「前掌不低于 `floor_y`」。
///
/// 只在绕踝的竖直平面里转：轴 = 水平面内与脚掌方向垂直的方向，所以前掌的水平方位
/// 不变，只有俯仰被抬起。已经在地面之上、脚掌几乎竖直（`offset` 水平分量太小）或者
/// 抬到顶也够不着 `floor_y` 时原样返回。
fn clamp_toe_above_floor(foot_rot: Quat, toe_offset: Vec3, ankle: Vec3, floor_y: f32) -> Quat {
    let v = foot_rot * toe_offset;
    let toe_y = ankle.y + v.y;
    if toe_y >= floor_y {
        return foot_rot;
    }
    let horizontal = Vec2::new(v.x, v.z).length();
    let length = v.length();
    if horizontal < 1e-4 || length < 1e-4 {
        return foot_rot;
    }
    let wanted = floor_y - ankle.y;
    if wanted >= length {
        // 脚掌再抬也够不着地面：这不是朝向能修的（交给位置 IK）
        return foot_rot;
    }
    // 在 (水平, 竖直) 平面里把角度从当前值转到「竖直分量 = wanted」
    let from = v.y.atan2(horizontal);
    let to = (wanted / length).clamp(-1.0, 1.0).asin();
    let axis = Vec3::new(-v.z, 0.0, v.x).normalize();
    Quat::from_axis_angle(axis, to - from) * foot_rot
}

/// 带限速的趋近：每帧最多走 `max_delta`，到位就停
fn approach(current: f32, target: f32, max_delta: f32) -> f32 {
    let delta = target - current;
    if delta.abs() <= max_delta {
        target
    } else {
        current + delta.signum() * max_delta
    }
}

/// 从模型往下找名字以 `suffix` 结尾的骨骼（跳过 Ctrl / Helper / IK 控制骨）
fn find_bone(
    model: Entity,
    children: &Query<&Children>,
    names: &Query<&Name>,
    suffix: &str,
) -> Option<Entity> {
    let mut stack = vec![model];
    while let Some(entity) = stack.pop() {
        if entity != model
            && let Ok(name) = names.get(entity)
        {
            let name = name.as_str();
            if name.ends_with(suffix)
                && !name.contains("Ctrl")
                && !name.contains("Helper")
                && !name.contains("_IK")
            {
                return Some(entity);
            }
        }
        if let Ok(list) = children.get(entity) {
            stack.extend(list.iter());
        }
    }
    None
}

/// 缓存骨架：hips + 到根的脊椎链 + 两条腿。
///
/// 任何一节缺失都返回 `None`（下一帧再试）：宁可整段不跑，也不要用错的骨骼去解 IK。
fn build_rig(
    model: Entity,
    children: &Query<&Children>,
    names: &Query<&Name>,
    parents: &Query<&ChildOf>,
) -> Option<FootRig> {
    let hips = find_bone(model, children, names, "Hips")?;

    let mut spine = vec![hips];
    let mut cursor = hips;
    while let Ok(child_of) = parents.get(cursor) {
        cursor = child_of.parent();
        spine.push(cursor);
    }
    // 从场景最顶层往 hips 排，乘矩阵时才是父 → 子
    spine.reverse();

    let mut legs = [Leg {
        upper: hips,
        lower: hips,
        foot: hips,
        toe: None,
    }; 2];
    for (index, side) in ["Left", "Right"].into_iter().enumerate() {
        let upper = find_bone(model, children, names, &format!("{side}UpLeg"))?;
        let lower = find_bone(model, children, names, &format!("{side}Leg"))?;
        let foot = find_bone(model, children, names, &format!("{side}Foot"))?;
        // 前掌缺失不影响踝部锁脚（承重点就滑不到前掌而已），所以是 Option
        let toe = find_bone(model, children, names, &format!("{side}ToeBase"));
        legs[index] = Leg {
            upper,
            lower,
            foot,
            toe,
        };
    }

    Some(FootRig { spine, legs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    fn straight_pose() -> LegPose {
        LegPose {
            toe: None,
            root: Vec3::new(0.0, 1.0, 0.0),
            joint: Vec3::new(0.0, 0.45, 0.2),
            end: Vec3::ZERO,
            parent_rot: Quat::IDENTITY,
            upper_rot: Quat::IDENTITY,
            lower_rot: Quat::IDENTITY,
            foot_rot: Quat::IDENTITY,
        }
    }

    /// 把解出来的局部旋转重新乘回世界，确认脚落到目标上、骨长不变
    #[test]
    fn solved_leg_lands_on_target() {
        let pose = straight_pose();
        let target = Vec3::new(0.12, 0.0, 0.0);
        let solved = solve_leg(pose, target).expect("应当有解");

        let upper_world = pose.parent_rot * solved.upper_local;
        let lower_world = upper_world * solved.lower_local;
        let foot = pose.root
            + upper_world * (pose.joint - pose.root)
            + lower_world * (pose.end - pose.joint);

        assert!((foot - target).length() < 1e-4, "脚落在 {foot:?}");
        // 脚的世界朝向没被篡改：局部旋转把下段的转动补了回来
        let foot_rot_world = lower_world * solved.foot_local;
        assert!(
            foot_rot_world.angle_between(Quat::IDENTITY).abs() < 1e-4,
            "脚被转歪了：{foot_rot_world:?}"
        );
    }

    /// 目标就是当前脚踝时，解应当退化成恒等（不该凭空给姿态加旋转）
    #[test]
    fn unchanged_target_is_a_noop() {
        let pose = straight_pose();
        let solved = solve_leg(pose, pose.end).expect("应当有解");
        assert!(solved.upper_local.angle_between(Quat::IDENTITY) < 1e-4);
        assert!(solved.lower_local.angle_between(Quat::IDENTITY) < 1e-4);
        assert!(solved.foot_local.angle_between(Quat::IDENTITY) < 1e-4);
    }

    /// 够不着的目标不会把腿拉断，脚停在最大伸展处
    #[test]
    fn unreachable_target_keeps_bone_lengths() {
        let pose = straight_pose();
        let target = Vec3::new(0.0, 0.0, 0.6);
        let solved = solve_leg(pose, target).expect("应当有解");

        let upper_world = pose.parent_rot * solved.upper_local;
        let lower_world = upper_world * solved.lower_local;
        let joint = pose.root + upper_world * (pose.joint - pose.root);
        let foot = joint + lower_world * (pose.end - pose.joint);

        assert!(((joint - pose.root).length() - (pose.joint - pose.root).length()).abs() < 1e-4);
        assert!(((foot - joint).length() - (pose.end - pose.joint).length()).abs() < 1e-4);
        // 目标在 +Z 更远处，脚只会落在可达边界，不会跟过去
        assert!(foot.z < 0.6);
    }

    /// 系统级验证：身体持续转动时，支撑脚应当钉在原地不动。
    ///
    /// 这条直接复现用户报的问题：混合空间按「相对身体」放脚，身体一转，
    /// 世界空间里支撑脚就会横着挪一小段。这里搭一套简化骨架（链长与 Amy 接近），
    /// 每帧模拟动画层重新写回腿部原姿态，再让身体偏航 0.9°/帧走 20 帧，
    /// 最后检查脚踝的世界位置还贴着锚点、骨长没被拉断。
    #[test]
    fn ik_pins_the_ankle_while_the_body_turns() {
        let mut app = App::new();
        app.insert_resource(FootIkSettings::default());
        app.insert_resource(Time::<()>::default());

        let world = app.world_mut();
        let root = world
            .spawn((Name::new("Avatar"), Transform::default()))
            .id();
        let model = world
            .spawn((Name::new("Model"), Transform::default(), ChildOf(root)))
            .id();
        let armature = world
            .spawn((Name::new("Armature"), Transform::default(), ChildOf(model)))
            .id();
        let hips = world
            .spawn((
                Name::new("mixamorig:Hips"),
                Transform::from_xyz(0.0, 0.9, 0.0),
                ChildOf(armature),
            ))
            .id();

        // 左右各一条腿：大腿 0.35m + 小腿 0.35m，膝盖预先弯 0.2rad 否则极点退化
        let make_leg = |world: &mut World, side: &str, x: f32| {
            let bend = Quat::from_rotation_x(0.2);
            let upper = world
                .spawn((
                    Name::new(format!("mixamorig:{side}UpLeg")),
                    Transform::from_xyz(x, 0.0, 0.0),
                    ChildOf(hips),
                ))
                .id();
            let lower = world
                .spawn((
                    Name::new(format!("mixamorig:{side}Leg")),
                    Transform::from_xyz(0.0, -0.35, 0.0).with_rotation(bend),
                    ChildOf(upper),
                ))
                .id();
            let foot = world
                .spawn((
                    Name::new(format!("mixamorig:{side}Foot")),
                    Transform::from_xyz(0.0, -0.35, 0.0),
                    ChildOf(lower),
                ))
                .id();
            let toe = world
                .spawn((
                    Name::new(format!("mixamorig:{side}ToeBase")),
                    Transform::from_xyz(0.0, -0.08, 0.12),
                    ChildOf(foot),
                ))
                .id();
            (
                Leg {
                    upper,
                    lower,
                    foot,
                    toe: Some(toe),
                },
                (upper, lower, foot),
            )
        };
        let (left, left_bones) = make_leg(world, "Left", 0.08);
        let (right, right_bones) = make_leg(world, "Right", -0.08);

        let mut ik = FootIk::default();
        ik.contacts = [1.0, 1.0];
        ik.blend = 1.0;
        ik.rig = Some(FootRig {
            spine: vec![root, model, armature, hips],
            legs: [left, right],
        });
        world.entity_mut(model).insert(ik);

        // 动画层每帧会重新写回这套姿态；不模拟的话上一帧的 IK 结果会累积
        let originals = [
            (left_bones.0, Quat::IDENTITY),
            (left_bones.1, Quat::from_rotation_x(0.2)),
            (left_bones.2, Quat::IDENTITY),
            (right_bones.0, Quat::IDENTITY),
            (right_bones.1, Quat::from_rotation_x(0.2)),
            (right_bones.2, Quat::IDENTITY),
        ];

        let mut hip_drift = 0.0f32;
        for frame in 0..20u32 {
            {
                let world = app.world_mut();
                for (entity, rotation) in originals {
                    world.get_mut::<Transform>(entity).expect("腿骨").rotation = rotation;
                }
                let yaw = 0.9f32.to_radians() * (frame + 1) as f32;
                world.get_mut::<Transform>(root).expect("根").rotation =
                    Quat::from_rotation_y(yaw);
                world
                    .resource_mut::<Time>()
                    .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
                world
                    .run_system_once(solve_foot_ik)
                    .expect("脚部 IK 系统跑不起来");
                hip_drift = (0.08 * (1.0 - yaw.cos())).abs() + (0.08 * yaw.sin()).abs();
            }
        }

        let world = app.world();
        let ik = world.get::<FootIk>(model).expect("FootIk 组件");
        let [anchor_left, anchor_right] = ik.anchors();
        let anchor_left = anchor_left.expect("左脚应当被锁住");
        assert!(anchor_right.is_some(), "右脚也应当被锁住");

        // 解完之后脚踝的世界位置（按父子链自己乘出来）
        let ankle_world = |bones: (Entity, Entity, Entity)| {
            let mut matrix = Affine3A::IDENTITY;
            for entity in [root, model, armature, hips, bones.0, bones.1, bones.2] {
                let local = world.get::<Transform>(entity).expect("骨骼");
                matrix *= local.compute_affine();
            }
            Vec3::from(matrix.translation)
        };
        let ankle = ankle_world(left_bones);
        let drift = Vec3::new(ankle.x - anchor_left.x, 0.0, ankle.z - anchor_left.z).length();
        assert!(
            drift < 0.005,
            "脚踝偏离锚点 {drift:.4}m（身体转 18° 本会把脚横向带走 {hip_drift:.4}m）"
        );

        // 骨长不能被 IK 拉长：两段都应保持 0.35m
        let mut matrix = Affine3A::IDENTITY;
        for entity in [root, model, armature, hips, left_bones.0, left_bones.1] {
            let local = world.get::<Transform>(entity).expect("骨骼");
            matrix *= local.compute_affine();
        }
        let knee = Vec3::from(matrix.translation);
        let mut matrix = Affine3A::IDENTITY;
        for entity in [root, model, armature, hips, left_bones.0] {
            let local = world.get::<Transform>(entity).expect("骨骼");
            matrix *= local.compute_affine();
        }
        let hip_joint = Vec3::from(matrix.translation);
        assert!(
            ((knee - hip_joint).length() - 0.35).abs() < 1e-3,
            "大腿被拉长到 {}",
            (knee - hip_joint).length()
        );
        assert!(
            ((ankle - knee).length() - 0.35).abs() < 1e-3,
            "小腿被拉长到 {}",
            (ankle - knee).length()
        );
    }

    /// 系统级验证「脚掌贴地」：支撑末期动画抬高脚跟时，承重点要滑到前掌，
    /// 让前掌留在原地、脚踝跟着抬 —— 而不是把踝按住、让前掌先离地。
    #[test]
    fn heel_off_pivots_on_the_toe_instead_of_dragging_it() {
        let mut app = App::new();
        // 这条测试的骨架腿短一点：抬脚刚好在可达范围内，测的才是锁脚逻辑
        app.insert_resource(FootIkSettings {
            toe_pivot_rise: 0.02,
            ..FootIkSettings::default()
        });
        app.insert_resource(Time::<()>::default());

        let world = app.world_mut();
        let root = world
            .spawn((Name::new("Avatar"), Transform::default()))
            .id();
        let model = world
            .spawn((Name::new("Model"), Transform::default(), ChildOf(root)))
            .id();
        let armature = world
            .spawn((Name::new("Armature"), Transform::default(), ChildOf(model)))
            .id();
        let hips = world
            .spawn((
                Name::new("mixamorig:Hips"),
                Transform::from_xyz(0.0, 0.9, 0.0),
                ChildOf(armature),
            ))
            .id();

        // 预弯要留足余量：两段 0.35m 在 1.0rad 下只剩 ~0.6m 跨度，
        // 腿抬起来时 IK 才够得着（不然脚会被骨长卡住，测的就不是锁脚逻辑了）
        let bend = Quat::from_rotation_x(1.0);
        let make_leg = |world: &mut World, side: &str, x: f32| {
            let upper = world
                .spawn((
                    Name::new(format!("mixamorig:{side}UpLeg")),
                    Transform::from_xyz(x, 0.0, 0.0),
                    ChildOf(hips),
                ))
                .id();
            let lower = world
                .spawn((
                    Name::new(format!("mixamorig:{side}Leg")),
                    Transform::from_xyz(0.0, -0.35, 0.0).with_rotation(bend),
                    ChildOf(upper),
                ))
                .id();
            let foot = world
                .spawn((
                    Name::new(format!("mixamorig:{side}Foot")),
                    Transform::from_xyz(0.0, -0.35, 0.0),
                    ChildOf(lower),
                ))
                .id();
            let toe = world
                .spawn((
                    Name::new(format!("mixamorig:{side}ToeBase")),
                    Transform::from_xyz(0.0, -0.08, 0.14),
                    ChildOf(foot),
                ))
                .id();
            (
                Leg {
                    upper,
                    lower,
                    foot,
                    toe: Some(toe),
                },
                (upper, lower, foot, toe),
            )
        };
        let (left, left_bones) = make_leg(world, "Left", 0.08);
        let (right, right_bones) = make_leg(world, "Right", -0.08);

        let mut ik = FootIk::default();
        ik.contacts = [1.0, 1.0];
        ik.blend = 1.0;
        ik.rig = Some(FootRig {
            spine: vec![root, model, armature, hips],
            legs: [left, right],
        });
        world.entity_mut(model).insert(ik);

        // 动画层每帧重新写回原姿态：大腿不动、小腿保持预弯、脚掌蹬地
        let originals = [
            (left_bones.0, Quat::IDENTITY),
            (left_bones.1, bend),
            (right_bones.0, Quat::IDENTITY),
            (right_bones.1, bend),
        ];

        let toe_world = |world: &World, bones: (Entity, Entity, Entity, Entity)| {
            let mut matrix = Affine3A::IDENTITY;
            for entity in [root, model, armature, hips, bones.0, bones.1, bones.2, bones.3] {
                let local = world.get::<Transform>(entity).expect("骨骼");
                matrix *= local.compute_affine();
            }
            Vec3::from(matrix.translation)
        };
        let ankle_world = |world: &World, bones: (Entity, Entity, Entity, Entity)| {
            let mut matrix = Affine3A::IDENTITY;
            for entity in [root, model, armature, hips, bones.0, bones.1, bones.2] {
                let local = world.get::<Transform>(entity).expect("骨骼");
                matrix *= local.compute_affine();
            }
            Vec3::from(matrix.translation)
        };

        let mut planted_toe = Vec3::ZERO;
        let mut planted_ankle = Vec3::ZERO;
        for frame in 0..12u32 {
            let world = app.world_mut();
            for (entity, rotation) in originals {
                world.get_mut::<Transform>(entity).expect("腿骨").rotation = rotation;
            }
            // 抬脚跟：整条腿升高 8mm/帧，同时脚掌绕前掌蹬地（脚踝相对前掌往上走）
            world.get_mut::<Transform>(left_bones.0).expect("大腿").translation.y += 0.004;
            world.get_mut::<Transform>(right_bones.0).expect("大腿").translation.y += 0.004;
            // 脚掌蹬地：抵消掉小腿预弯，让脚的世界朝向≈水平，再叠加蹬地俯仰
            let pitch = 0.04 * (frame + 1) as f32;
            let foot_rot = Quat::from_rotation_x(pitch - 1.0);
            world.get_mut::<Transform>(left_bones.2).expect("脚").rotation = foot_rot;
            world.get_mut::<Transform>(right_bones.2).expect("脚").rotation = foot_rot;
            if frame == 0 {
                planted_toe = toe_world(world, left_bones);
                planted_ankle = ankle_world(world, left_bones);
            }
            world
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
            world
                .run_system_once(solve_foot_ik)
                .expect("脚部 IK 系统跑不起来");
        }

        let world = app.world();
        let ik = world.get::<FootIk>(model).expect("FootIk 组件");
        let roll = ik.roll()[0];
        assert!(roll >= 0.99, "脚跟抬起来后承重点应当完全滑到前掌，实际 {roll:.2}");

        let toe = toe_world(world, left_bones);
        let toe_drift = (toe.y - planted_toe.y).abs().max(
            Vec3::new(toe.x - planted_toe.x, 0.0, toe.z - planted_toe.z).length(),
        );
        assert!(
            toe_drift < 0.01,
            "前掌被带离地面 {toe_drift:.4}m（锚在 {planted_toe:?}，现在 {toe:?}）"
        );

        // 踝确实被抬起来了：修复前它会被按在踩实那一刻的高度上，前掌因此先离地
        let ankle = ankle_world(world, left_bones);
        assert!(
            ankle.y > planted_ankle.y + 0.05,
            "脚踝应当随脚跟抬起：{} -> {}",
            planted_ankle.y,
            ankle.y
        );
    }

    /// 前掌被动画压到地面以下时，要靠转动脚踝把它抬回地面（只改俯仰，不动水平方位）
    #[test]
    fn toe_pitch_is_clamped_to_the_planted_floor() {
        let ankle = Vec3::new(0.0, 0.10, 0.0);
        let offset = Vec3::new(0.0, -0.08, 0.14); // 前掌在踝前方、下方
        let floor = ankle.y - 0.08; // 踩实时前掌贴地的高度

        // 脚尖往下压：前掌会扎进地里
        let down = Quat::from_rotation_x(0.6);
        let toe = ankle + down * offset;
        assert!(toe.y < floor - 0.05, "构造前提：脚尖确实扎进地里 {toe:?}");

        let clamped = clamp_toe_above_floor(down, offset, ankle, floor);
        let toe = ankle + clamped * offset;
        assert!((toe.y - floor).abs() < 1e-4, "前掌应贴回地面：{}", toe.y);
        // 水平方位不动：只绕「与脚掌水平方向垂直的轴」转
        assert!(toe.z > 1e-3 && toe.x.abs() < 1e-4, "脚掌被转向了：{toe:?}");

        // 已经在地面之上：原样返回，不无谓地转脚
        let up = Quat::from_rotation_x(-0.2);
        let toe = ankle + up * offset;
        assert!(toe.y > floor);
        assert_eq!(clamp_toe_above_floor(up, offset, ankle, floor), up);
    }

    /// 系统级验证「前掌不穿地」：钉着前掌时把脚掌越压越低，前掌也不许沉到地面以下
    #[test]
    fn planted_toe_never_sinks_below_the_floor() {
        let mut app = App::new();
        app.insert_resource(FootIkSettings::default());
        app.insert_resource(Time::<()>::default());

        let world = app.world_mut();
        let root = world
            .spawn((Name::new("Avatar"), Transform::default()))
            .id();
        let model = world
            .spawn((Name::new("Model"), Transform::default(), ChildOf(root)))
            .id();
        let armature = world
            .spawn((Name::new("Armature"), Transform::default(), ChildOf(model)))
            .id();
        let hips = world
            .spawn((
                Name::new("mixamorig:Hips"),
                Transform::from_xyz(0.0, 0.9, 0.0),
                ChildOf(armature),
            ))
            .id();

        let bend = Quat::from_rotation_x(1.0);
        let make_leg = |world: &mut World, side: &str, x: f32| {
            let upper = world
                .spawn((
                    Name::new(format!("mixamorig:{side}UpLeg")),
                    Transform::from_xyz(x, 0.0, 0.0),
                    ChildOf(hips),
                ))
                .id();
            let lower = world
                .spawn((
                    Name::new(format!("mixamorig:{side}Leg")),
                    Transform::from_xyz(0.0, -0.35, 0.0).with_rotation(bend),
                    ChildOf(upper),
                ))
                .id();
            let foot = world
                .spawn((
                    Name::new(format!("mixamorig:{side}Foot")),
                    Transform::from_xyz(0.0, -0.35, 0.0),
                    ChildOf(lower),
                ))
                .id();
            let toe = world
                .spawn((
                    Name::new(format!("mixamorig:{side}ToeBase")),
                    Transform::from_xyz(0.0, -0.08, 0.14),
                    ChildOf(foot),
                ))
                .id();
            (
                Leg {
                    upper,
                    lower,
                    foot,
                    toe: Some(toe),
                },
                (upper, lower, foot, toe),
            )
        };
        let (left, left_bones) = make_leg(world, "Left", 0.08);
        let (right, right_bones) = make_leg(world, "Right", -0.08);

        let mut ik = FootIk::default();
        ik.contacts = [1.0, 1.0];
        ik.blend = 1.0;
        ik.rig = Some(FootRig {
            spine: vec![root, model, armature, hips],
            legs: [left, right],
        });
        world.entity_mut(model).insert(ik);

        let originals = [
            (left_bones.0, Quat::IDENTITY),
            (left_bones.1, bend),
            (right_bones.0, Quat::IDENTITY),
            (right_bones.1, bend),
        ];

        let toe_world = |world: &World, bones: (Entity, Entity, Entity, Entity)| {
            let mut matrix = Affine3A::IDENTITY;
            for entity in [root, model, armature, hips, bones.0, bones.1, bones.2, bones.3] {
                let local = world.get::<Transform>(entity).expect("骨骼");
                matrix *= local.compute_affine();
            }
            Vec3::from(matrix.translation)
        };

        let mut floor = f32::NEG_INFINITY;
        for frame in 0..8u32 {
            let world = app.world_mut();
            for (entity, rotation) in originals {
                world.get_mut::<Transform>(entity).expect("腿骨").rotation = rotation;
            }
            // 脚掌越压越低（-0.05rad/帧），前掌本该扎进地里
            let foot_rot = Quat::from_rotation_x(-0.05 * frame as f32 - 1.0);
            world.get_mut::<Transform>(left_bones.2).expect("脚").rotation = foot_rot;
            world.get_mut::<Transform>(right_bones.2).expect("脚").rotation = foot_rot;
            if frame == 0 {
                floor = toe_world(world, left_bones).y;
            }
            world
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
            world
                .run_system_once(solve_foot_ik)
                .expect("脚部 IK 系统跑不起来");

            let toe = toe_world(world, left_bones);
            assert!(
                toe.y >= floor - 0.002,
                "第 {frame} 帧前掌穿地：{} < {floor}",
                toe.y
            );
        }
    }

    /// 抬脚那一帧不能把「前掌不穿地」的转动一把丢回去：那样脚掌会一帧内翻十几度
    #[test]
    fn toe_pitch_clamp_fades_out_instead_of_snapping_at_release() {
        let mut app = App::new();
        app.insert_resource(FootIkSettings::default());
        app.insert_resource(Time::<()>::default());

        let world = app.world_mut();
        let root = world
            .spawn((Name::new("Avatar"), Transform::default()))
            .id();
        let model = world
            .spawn((Name::new("Model"), Transform::default(), ChildOf(root)))
            .id();
        let armature = world
            .spawn((Name::new("Armature"), Transform::default(), ChildOf(model)))
            .id();
        let hips = world
            .spawn((
                Name::new("mixamorig:Hips"),
                Transform::from_xyz(0.0, 0.9, 0.0),
                ChildOf(armature),
            ))
            .id();

        let bend = Quat::from_rotation_x(1.0);
        let make_leg = |world: &mut World, side: &str, x: f32| {
            let upper = world
                .spawn((
                    Name::new(format!("mixamorig:{side}UpLeg")),
                    Transform::from_xyz(x, 0.0, 0.0),
                    ChildOf(hips),
                ))
                .id();
            let lower = world
                .spawn((
                    Name::new(format!("mixamorig:{side}Leg")),
                    Transform::from_xyz(0.0, -0.35, 0.0).with_rotation(bend),
                    ChildOf(upper),
                ))
                .id();
            let foot = world
                .spawn((
                    Name::new(format!("mixamorig:{side}Foot")),
                    Transform::from_xyz(0.0, -0.35, 0.0),
                    ChildOf(lower),
                ))
                .id();
            let toe = world
                .spawn((
                    Name::new(format!("mixamorig:{side}ToeBase")),
                    Transform::from_xyz(0.0, -0.08, 0.14),
                    ChildOf(foot),
                ))
                .id();
            (
                Leg {
                    upper,
                    lower,
                    foot,
                    toe: Some(toe),
                },
                (upper, lower, foot, toe),
            )
        };
        let (left, left_bones) = make_leg(world, "Left", 0.08);
        let (right, right_bones) = make_leg(world, "Right", -0.08);

        let mut ik = FootIk::default();
        ik.contacts = [1.0, 1.0];
        ik.blend = 1.0;
        ik.rig = Some(FootRig {
            spine: vec![root, model, armature, hips],
            legs: [left, right],
        });
        world.entity_mut(model).insert(ik);

        // 动画：小腿预弯 1.0，脚掌先水平踩实，随后逐帧跖屈蹬地（脚尖往地里压）
        let foot_local = |frame: f32| Quat::from_rotation_x(0.12 * frame - 1.0);
        let foot_world_rot = |world: &World| {
            let mut rotation = Quat::IDENTITY;
            for entity in [root, model, armature, hips, left_bones.0, left_bones.1, left_bones.2] {
                let local = world.get::<Transform>(entity).expect("骨骼");
                rotation *= local.rotation;
            }
            rotation
        };

        let mut previous: Option<Quat> = None;
        let mut max_step = 0.0f32;
        for frame in 0..48u32 {
            let world = app.world_mut();
            // 平踩一帧后逐帧跖屈到 4 个单位，然后保持；第 7 帧抬脚（保持住才有可比性）
            let pitch = (frame as f32 - 1.0).clamp(0.0, 4.0);
            for (entity, rotation) in [
                (left_bones.0, Quat::IDENTITY),
                (left_bones.1, bend),
                (right_bones.0, Quat::IDENTITY),
                (right_bones.1, bend),
                (left_bones.2, foot_local(pitch)),
                (right_bones.2, foot_local(pitch)),
            ] {
                world.get_mut::<Transform>(entity).expect("腿骨").rotation = rotation;
            }
            // 身体持续前移：锁脚才有位置修正量，朝向夹紧也才会生效
            world.get_mut::<Transform>(hips).expect("hips").translation.z += 0.004;
            if frame == 7 {
                // 抬脚：支撑掉到 0，锁脚在这一帧交接给动画
                world.get_mut::<FootIk>(model).expect("FootIk").contacts = [0.0, 0.0];
            }
            world
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
            world
                .run_system_once(solve_foot_ik)
                .expect("脚部 IK 系统跑不起来");

            let rotation = foot_world_rot(world);

            if let Some(previous) = previous {
                max_step = max_step.max(previous.angle_between(rotation));
            }
            previous = Some(rotation);
        }
        assert!(
            max_step < 0.16,
            "抬脚前后脚掌单帧转了 {:.1}°，会看到「卡」一下",
            max_step.to_degrees()
        );
    }

    #[test]
    fn approach_ramps_and_clamps() {
        assert!((approach(0.0, 1.0, 0.25) - 0.25).abs() < 1e-6);
        assert!((approach(0.9, 1.0, 0.25) - 1.0).abs() < 1e-6);
        assert!((approach(1.0, 0.0, 0.25) - 0.75).abs() < 1e-6);
    }

    /// Mixamo 的 GLB 里同一个位置有三套骨：`mixamorig:*` 真骨、`Ctrl_*` 控制骨、
    /// `*_IK_*` 目标骨。名字匹配必须只挑真骨，否则会把控制骨当成腿去求解。
    #[test]
    fn rig_picks_real_bones_and_skips_helpers() {
        let mut app = App::new();
        let world = app.world_mut();

        let root = world.spawn(Name::new("Scene")).id();
        let armature = world
            .spawn((Name::new("Armature"), Transform::default(), ChildOf(root)))
            .id();
        let hips = world
            .spawn((
                Name::new("mixamorig:Hips"),
                Transform::default(),
                ChildOf(armature),
            ))
            .id();
        // 干扰项：名字以 LeftFoot 结尾但带 Ctrl，必须被排除
        let ctrl_foot = world
            .spawn((
                Name::new("Ctrl_LeftFoot"),
                Transform::default(),
                ChildOf(hips),
            ))
            .id();
        let upper = world
            .spawn((
                Name::new("mixamorig:LeftUpLeg"),
                Transform::default(),
                ChildOf(hips),
            ))
            .id();
        let lower = world
            .spawn((
                Name::new("mixamorig:LeftLeg"),
                Transform::default(),
                ChildOf(upper),
            ))
            .id();
        let foot = world
            .spawn((
                Name::new("mixamorig:LeftFoot"),
                Transform::default(),
                ChildOf(lower),
            ))
            .id();
        let right_upper = world
            .spawn((
                Name::new("mixamorig:RightUpLeg"),
                Transform::default(),
                ChildOf(hips),
            ))
            .id();
        let right_lower = world
            .spawn((
                Name::new("mixamorig:RightLeg"),
                Transform::default(),
                ChildOf(right_upper),
            ))
            .id();
        let right_foot = world
            .spawn((
                Name::new("mixamorig:RightFoot"),
                Transform::default(),
                ChildOf(right_lower),
            ))
            .id();

        app.world_mut()
            .run_system_once(
                move |children: Query<&Children>,
                      names: Query<&Name>,
                      parents: Query<&ChildOf>| {
                    let rig =
                        build_rig(root, &children, &names, &parents).expect("骨架应当能找到");
                    // 脊椎链从场景顶部排到 hips：乘矩阵时才是父 → 子
                    assert_eq!(rig.spine.first().copied(), Some(root));
                    assert_eq!(rig.spine.last().copied(), Some(hips));
                    assert!(rig.spine.contains(&armature));
                    assert_eq!(rig.legs[0].upper, upper);
                    assert_eq!(rig.legs[0].lower, lower);
                    assert_eq!(rig.legs[0].foot, foot, "不能被 Ctrl_LeftFoot 抢走");
                    assert_eq!(rig.legs[1].upper, right_upper);
                    assert_eq!(rig.legs[1].lower, right_lower);
                    assert_eq!(rig.legs[1].foot, right_foot);
                    assert_ne!(rig.legs[0].foot, ctrl_foot);
                },
            )
            .expect("骨架发现跑不起来");
    }
}
