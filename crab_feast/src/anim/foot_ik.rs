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
//! - 锁脚钉的是**位置**（锚点），脚掌的**朝向**另有一条偏航锁（[`FootYawLock`]）：
//!   踩实那一刻的脚掌世界朝向记成基准，身体转了就把脚掌反向拧回去 ——
//!   否则脚掌的旋转一直跟着动画的 hips 走，抬起一只脚原地转身时着地那只脚
//!   会在原地「拧螺丝」。

use crate::GameState;
use crate::input::wrap_pi;
use avian3d::prelude::{SpatialQuery, SpatialQueryFilter};
use bevy::math::{Affine3A, Dir3};
use bevy::prelude::*;
use bevy::transform::TransformSystems;
use crab_feast_library::ik::{
    FootContact, FootLock, FootLockConfig, ROLL_SPAN, aim_world_rotation, solve_two_bone,
};

use super::character::{AnimModelRoot, AnimRuntime};
use super::driver::{AnimFocus, PoseDriver, drive_pose};
use super::jump::JumpAction;

pub struct FootIkPlugin;

/// 让「非本地动画栈」也参与脚部 IK 的标记。
///
/// 本地角色由 [`AnimModelRoot`] 自动纳入（它本来就是动画系统的模型根）；
/// 联机远端角色走的是 `net.rs` 里的混合空间驱动，没有那个标记，所以由那边显式插这个。
#[derive(Component, Debug, Default)]
pub struct FootIkTarget;

/// 脚部 IK 的 `PostUpdate` 执行集合。
///
/// 谁往骨骼里写姿态，谁就必须排在它前面（远端的 `drive_remote_pose` 就靠它排序）：
/// IK 解的是「这一帧最终的动画姿态」，提前一帧算出来的锚点会跟脚对着不上。
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FootIkSet;

/// 全局参数。逐帧读取，所以改这里的值当帧生效；联机时每个角色可以再各自
/// 用 [`FootIk::enabled`] 做二级开关。
///
/// **单位约定**：标着「米」的项（`max_correction`、`toe_pivot_rise`、`heel_off_lead`、
/// `ground_probe_rise/depth`）写的是「在参考骨架（Amy，腿长 0.571 m）上的米数」，
/// 运行时按这台角色实测的 `scale_leg` / `scale_foot` 换算成它自己的米数 ——
/// 同一套参数在 0.57 m 的腿和 0.89 m 的腿上比例一致。角度（度）、时间（秒）、
/// 权重（0..1）不动，因为它们本来就跟骨骼尺寸无关
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct FootIkSettings {
    /// 总开关。关掉时所有角色的 `blend` 都会平滑降到 0，求解整段跳过
    pub enabled: bool,
    /// 模拟端（联机时复制下来的其他玩家）单独开关。
    ///
    /// 远端角色的脚部数据是「服务器给的实测速度 + 混合空间姿态」，没有本地
    /// 物理兜底，所以有时候会想单独关掉它（例如排查远端滑步、或者远端数量多时
    /// 省开销），而不想连带把本地角色的锁脚一起停掉。
    /// 判定「谁是模拟端」看实体上有没有 [`FootIkTarget`]。
    pub remote_enabled: bool,
    /// 目标权重 0→1 的过渡时长（秒）。0 表示硬切（不推荐：会看到膝盖瞬移）
    pub blend_secs: f32,
    /// 锚点最多允许偏离脚踝多少米（参考骨架口径，见结构体注释的换算约定），
    /// 超过就让锚点跟着脚走
    pub max_correction: f32,
    /// 支撑权重高于它才算踩实
    pub plant_enter: f32,
    /// 支撑权重低于它才算抬脚
    pub plant_exit: f32,
    /// 抬脚解除锁定后，锁脚强度从 1 落回 0 的过渡时长（秒）。smoothstep 形状，
    /// 位置/速度/加速度都连续；时间要盖住整个摆动期，抬脚后的残差才不会「一瞬间」还完。
    /// 0 = 一抬脚就回到动画位置（会看到跳变）。默认 0.26
    pub release_secs: f32,
    /// 刚踩实时锁脚强度从 0 升到 1 的过渡时长（秒）。同样是 smoothstep 形状；
    /// 0 = 一踩实就钉死（脚的速度被一帧掐断，会「顿」）。默认 0.10
    pub engage_secs: f32,
    /// 支撑期里动画把脚跟抬起多少米（参考骨架的**脚长**口径）之后，
    /// 承重点从踝滑到前掌（绕前掌转）。这是脚掌绕前掌转出来的抬升，
    /// 跟脚有多长成正比
    /// 0 = 从踩实第一帧就钉前掌（脚跟会过早抬起），默认 0.03。
    /// 现在它只是**兜底**触发：重心可能永远不越过脚（倒退、侧移、站立），
    /// 正常走跑时的起跟由 `heel_off_lead` 接管
    pub toe_pivot_rise: f32,
    /// 重心（骨盆）沿脚掌朝向越过锚点多远（米）才开始抬脚跟。默认 0 = 重心
    /// 正好压到踩实那一刻的脚踝上；负值提前，正值推后
    pub heel_off_lead: f32,
    /// 抬跟触发后推进到「完全抬跟」的时长（秒）。前 40% 迁承重点，后段补脚掌俯仰
    pub heel_off_secs: f32,
    /// 起跟前至少要踩实多久（秒）：落地那几帧重心可能已经在脚上方
    pub heel_off_min_secs: f32,
    /// 支撑末期要蹬到的**踝角**（度）：脚掌与小腿的夹角。
    ///
    /// 角度约定：90° = 站立（脚掌平、小腿竖直），跖屈蹬地时正常人能到
    /// 130~160°。之所以不能只看「脚掌相对地面的俯角」：支撑末期小腿常常是前倾的
    /// （膝在踝前方），同样的地面俯角会打折扣 —— 小腿前倾 30° 时，脚掌俯 55° 只有
    /// 115° 的踝角，看着就是「折叠严重」。这里按当帧小腿的前倾量折算回地面俯角，
    /// 只补不足：动画自己蹬得更大就原样保留。<= 90 = 不干预
    pub heel_off_ankle: f32,
    /// 支撑期踝角的**解剖下限**（度）。低于它就不是「踝在弯」，是脚踝被压过头了。
    ///
    /// 脚掌平贴地面时，小腿每前倾一度，踝角就掉一度（φ = 90 + P − L）。真实步态
    /// 不会让踝角这么掉下去 —— 重心过了脚踝，脚跟就该离地、靠脚掌俯下去抵消小腿
    /// 的前倾；我们的锁脚却一直把踝钉在地上、脚掌保持动画的平放姿态，于是看起来
    /// 就是「小腿压着脚掌往前折」。
    ///
    /// 这条给踝角一个地板：**整个支撑期**只要 φ 低于它，就绕前掌把脚掌俯下去
    /// （脚跟跟着抬高），差多少补多少。它不挂靠「重心越过锚点」那个抬跟时机 ——
    /// 折角是整段支撑里一点点长出来的，等那个时机再补已经折到 60°，来不及。
    /// 副作用是它会比抬跟斜坡更早起跟（这正是「重心过脚踝就该离地」的样子）。
    /// 默认 90
    pub heel_off_ankle_min: f32,
    /// 膝盖朝向（极点）跟随动画的时间常数（秒）。混合空间里两条剪辑的膝盖朝向
    /// 差一点，逐帧跟着动画走会让膝盖左右抖，看起来就是支撑腿「顿」。
    /// 0 = 完全跟随动画（不推荐），默认 0.05
    pub pole_secs: f32,
    /// 脚掌离地后，踝角（脚相对小腿）允许的最大角速度（度/秒）。
    ///
    /// 这里用**限速**而不是低通：低通对任何速度都有稳态滞后（滞后 ≈ 速度 × 时间常数），
    /// 摆动期本来就有 200~300°/s 的跖屈，滤一下脚掌就永远拖在后面；而我们要掐掉的
    /// 只是「某一帧突然跳十几度」。限速器对平滑的动画零滞后，只把跳变摊成几帧。
    /// 默认 360°/s ≈ 6°/帧：正常跖屈不削，单帧 20° 的跳变摊成 4 帧。
    /// `f32::INFINITY` = 不限制（会闪）
    pub foot_max_rate: f32,
    /// 每只脚向下打一条射线量地面。关掉就退回「从脚踝/前掌推断」——
    /// 推断在台阶、斜坡、以及落地那一帧脚还在空中时会偏
    pub ground_probe: bool,
    /// 射线起点在脚踝上方多少米（参考骨架口径，按腿长换算）。抬高了才能越过薄地板 / 台阶边缘，
    /// 也从自己的脚掌上方起射，不会一出来就打在脚上
    pub ground_probe_rise: f32,
    /// 射线最长向下多少米（参考骨架口径，按腿长换算）。太小的话站在台阶上方量不到下一层
    pub ground_probe_depth: f32,
    /// 支撑期脚掌**世界朝向**允许被身体带偏多少度：踩实那一刻的脚掌朝向记成基准，
    /// 身体转了之后把脚掌反向拧回去；偏到这条上限之外就不再拧（脚掌跟着身体撇）。
    ///
    /// 为什么要它：锁脚只钉了**位置**（锚点），脚掌的旋转一直原样跟着动画的 hips 走
    /// —— 抬起一只脚原地转身时，着地那只脚就被身体拖着在地上原地拧。
    /// 为什么不无限拧：踝关节本身不产生偏航，全靠髋的外旋。真人撑着脚转到 ~30° 就
    /// 拧不动了，再转就是脚在地上撇。0 = 不锁（退回旧行为）
    pub foot_yaw_limit: f32,
    /// 踝关节自己能扛多少偏航（度）：本体转过去、脚掌还压在地上的那一段，
    /// 先由踝（其实是髋外旋，腿两侧分摊）吸收这么多，**再多的部分改由绕前掌转**
    /// 实现（口径见 [`FootYawLock`] 与 [`FootLock::set_ball_pin`]）。
    ///
    /// 为什么不是把全部给踝：脚掌平放在地上、身体转 28°，踝关节在画面上就是一
    /// 个歪掉的角，腿还得横伸出去够它。真人这时不是「拧脚踝」，是**踩到前掌、
    /// 脚跟抬起来、整个身体绕那颗球转** —— 球的接触点不动，踝跟着小腿走，
    /// 腿只需要几度外展，画面上看不出任何滑动。
    ///
    /// 取 12°：踝的横轴（距下关节）活动范围本来就这么大，再加上小腿自己能带
    /// 一点。0 = 一有偏航就绕球转（脚掌从踩实第一帧就跟着身体走）；
    /// 等于 `foot_yaw_limit` = 关掉绕球转，退回「拧着脚踝硬扛」的旧行为
    pub ball_pivot_enter: f32,
    /// 踝关节替身体扛不下的那点偏航，分多少给**腿骨**（0..1）。
    ///
    /// 脚被钉在地上、身体转过去，这段角差最后全压在踝上：胫骨相对脚掌被拧
    /// 二三十度，膝盖的前面跟着身体撇出去 —— 观感就是「小腿被强行扭转、膝盖
    /// 外翻」。真人不是这么转的：髋内旋能吃到 20~40°，胫骨相对脚掌只有十几度。
    /// 所以先分一段给股骨+胫骨（同向同量地绕自己骨轴滚，膝盖两端仍对齐），
    /// 剩下的才留给踝。0 = 全压踝上（旧行为），默认 0.65
    pub leg_roll_share: f32,
    /// 腿骨最多内/外旋多少度（度）。再大就不是「跟着转」，是「腿被拧」了
    pub leg_roll_max: f32,
    /// 落地后「把脚掌从空中的限速值过渡到动画值」的时长（秒）。
    ///
    /// 空中限速会让脚掌滞后动画十几度，落地那一帧如果直接把 `planted` 的分支切成
    /// 「精确跟随动画」，脚掌就会一帧跳回去 —— 那就是「落地瞬间弹一下」。
    /// 这一项把那笔差价摊开还。0 = 落地就精确跟随（会弹）
    pub foot_landing_secs: f32,
}

impl Default for FootIkSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            remote_enabled: true,
            // 80ms 大约是 5 帧：足够盖住膝盖的一次跳变，又快到跟手
            blend_secs: 0.08,
            max_correction: 0.22,
            plant_enter: 0.55,
            plant_exit: 0.25,
            release_secs: 0.26,
            engage_secs: 0.10,
            toe_pivot_rise: 0.03,
            // 0：重心压到踩实时的脚踝上就起跟，不等动画自己抬
            heel_off_lead: 0.0,
            // 300ms：走路的支撑期后半段大约这么长，起跟 → 蹬地摊满整段
            heel_off_secs: 0.30,
            // 80ms ≈ 5 帧：盖住落地时的重心抖动
            heel_off_min_secs: 0.08,
            // 140°：真实蹬地时踝角能到 130~160°（跖屈 40~70°），取中间偏保守；
            // 动画剪辑通常只做到 110~120°，看着就是「折叠」
            heel_off_ankle: 140.0,
            heel_off_ankle_min: 90.0,
            // 50ms：盖住混合空间里两条剪辑的膝盖朝向差异，又不至于跟不上转身
            pole_secs: 0.05,
            // 60ms（约 4 帧）：掐掉抬脚时的单帧朝向跳变，又不至于让脚掌跟不上摆动
            foot_max_rate: 360.0,
            ground_probe: true,
            // 30°：髋外旋能撑住的量级；再大脚掌相对小腿就明显歪了
            foot_yaw_limit: 30.0,
            // 12°：踝（距下关节）自己扛的量；再多就绕前掌转，脚跟在离地
            ball_pivot_enter: 12.0,
            // 0.65 / 18°：28° 的一段转身，踝只留 10°（距下关节本来就吃得住），
            // 18° 交给髋内旋 + 胫骨自旋
            leg_roll_share: 0.65,
            leg_roll_max: 18.0,
            // 30cm：比一条小腿短得多，不会从台阶上方误打到下面的地面
            ground_probe_rise: 0.30,
            // 1.5m：够跨过一层楼梯（约 17cm）和常见落差，又不会打到很远的房间地板
            ground_probe_depth: 1.5,
            // 80ms ≈ 5 帧：够把十几度的差价摊平，又不至于落地后脚掌还在飘
            foot_landing_secs: 0.08,
        }
    }
}

impl FootIkSettings {
    /// 这具骨架这一帧该不该解 IK：总开关 + 本角色开关 + （模拟端）模拟端开关。
    ///
    /// 射线和求解必须共用这一个判据：只管求解不管射线的话，开关关掉后每帧
    /// 还在每个角色身上打两条向下的物理射线，一整场都在白发。
    fn is_active(&self, ik: &FootIk, remote: bool) -> bool {
        self.enabled && ik.enabled && (!remote || self.remote_enabled)
    }
}

impl FootIkSettings {
    /// 把「以米给」的阈值按骨架尺寸换算成这台角色自己的米数。
    ///
    /// 三个米数用到的长度不一样：`max_correction`（踝被拉走多远）和
    /// `heel_off_lead`（重心沿脚掌越过锚点多远）是**身体位移**，跟腿长成正比；
    /// `toe_pivot_rise`（踝被抬起多少才开始绕前掌转）是**脚掌绕前掌转出来的
    /// 抬升**，跟脚长成正比。所以这里是两个系数，不是一个缩放
    fn lock_config(&self, scale_leg: f32, scale_foot: f32) -> FootLockConfig {
        FootLockConfig {
            max_correction: self.max_correction.max(0.0) * scale_leg,
            plant_enter: self.plant_enter.clamp(0.0, 1.0),
            plant_exit: self.plant_exit.clamp(0.0, self.plant_enter),
            release_secs: self.release_secs.max(0.0),
            engage_secs: self.engage_secs.max(0.0),
            toe_pivot_rise: self.toe_pivot_rise.max(0.0) * scale_foot,
            heel_off_lead: self.heel_off_lead.clamp(-0.2, 0.2) * scale_leg,
            heel_off_secs: self.heel_off_secs.max(1e-3),
            heel_off_min_secs: self.heel_off_min_secs.max(0.0),
        }
    }
}

/// 一只脚脚下量到的地面（射线命中点）
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundHit {
    /// 命中点的世界 y
    pub height: f32,
    /// 命中点法线（世界空间，已归一化）。斜坡/台阶上脚掌要贴着它转
    pub normal: Vec3,
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
    /// 动画层每帧写入：这一帧混合输出是不是停在原地。
    /// [`calibrate_at_rest`] 靠它判断「现在量 `foot_tilt` 可不可信」——
    /// 本地角色由 [`feed_contacts`] 从 [`PoseDriver`] 写，远端由 `net.rs` 写
    pub resting: bool,
    /// 连续静止的帧数（内部状态，见 [`calibrate_at_rest`]）
    settle: u8,
    /// 两只脚各自的锁脚状态
    locks: [FootLock; 2],
    /// 低通后的膝盖朝向上一次值（世界空间的单位向量）。混合空间换剪辑时
    /// 动画的极点会逐帧抖，直接拿来当 `pole_hint` 会让膝盖左右晃
    poles: [Option<Vec3>; 2],
    /// 离地期间低通后的**踝角**（脚相对小腿的局部旋转）。支撑期不参与
    /// （那时脚掌要精确跟随动画当支点），滤的只是「脚在空中」的那一段
    foot_rots: [Option<Quat>; 2],
    /// 正在施加的「前掌不穿地」修正：`(轴, 角度)`。需要更多立刻加、需要收回才淡出，
    /// 所以在脚离地（夹紧不再触发 / 够不着地面）时不会一帧撤掉
    toe_clamps: [Option<(Vec3, f32)>; 2],
    /// 两只脚脚下量到的地面（每帧由 [`sample_ground`] 重写）
    ground: [Option<GroundHit>; 2],
    /// 落地过渡进度（0..1）：0 = 还停在空中的限速值，1 = 精确跟随动画
    landing: [f32; 2],
    /// 落地那一刻的踝角快照：落地过渡的插值起点
    landing_from: [Option<Quat>; 2],
    /// 连续离地的时长：够长才算「真的抬起来了」，才允许落地过渡重新开始
    air_secs: [f32; 2],
    /// 最近一帧的锚点与修正量，调试面板 / 测试读取
    anchors: [Option<Vec3>; 2],
    correction: [f32; 2],
    /// 两只脚最近一帧的抬跟进度（0..1）、脚掌俯角（度，相对地面）
    /// 与抬完之后的**踝角**（度，脚掌与小腿的夹角）
    heel: [f32; 2],
    pitch: [f32; 2],
    /// 抬跟**满量**时的俯角（没乘淡出系数）：`pitch` 是画面上那个角，
    /// 这一个只给 `ANKLE_FLOOR_RATE` 当跨帧累加的基准。两者混用会自己拽自己：
    /// 淡出把画面俯角拉低 → 限速基准跟着低 → 满量目标也被压下去，0.1s 的
    /// 淡出变成两三帧的跳变
    pitch_full: [f32; 2],
    ankle: [f32; 2],
    /// 前掌骨（球窝/跖趾关节）这一帧反向折了多少度（抬跟时脚趾留在地上靠它）
    toe_bend: [f32; 2],
    /// 抬跟的淡出进度（1 = 还在支撑期，0 = 离地后已经收干净），按
    /// [`LIFT_RELEASE_SECS`] 推进。和锁脚的强度分开，脚离地时不会拖着放平
    lift_out: [f32; 2],
    /// 两只脚的脚掌偏航锁状态（踩实期间不让脚掌被身体拧着走）
    yaw_locks: [FootYawLock; 2],
    /// 两只脚**踩实期间**前掌（球）在地面上的滑移速度（m/s）。
    ///
    /// 球是脚掌上真正压地的那一点，也是绕球转的支点：锚点正确时它必须接近 0。
    /// 不用踝当读数 —— 绕球转的时候踝本来就走一段弧，那不是滑步
    slip: [f32; 2],
    /// 上一帧的球位置；离地时置 None，下一段支撑重新起量
    slip_prev: [Option<Vec3>; 2],
    /// 两只脚这一帧分给腿骨的自旋（弧度，正负号同 `FootYawLock`）。
    /// 从踝手里接过来、换成绕骨轴滚的那一段
    leg_roll: [f32; 2],
    /// 两只脚踝关节里**还剩**多少扭转（弧度）：`yaw_out - leg_roll`。
    /// 也就是「胫骨相对脚掌还拧着多少」，面板上读它判断要不要继续加 `leg_roll_share`
    ankle_twist: [f32; 2],
}

impl Default for FootIk {
    fn default() -> Self {
        Self {
            enabled: true,
            contacts: [0.0; 2],
            // 默认「不在静止」：标定要等动画层明确报告静止（本地读混合输出、
            // 远端读实测速度）。建骨架那一刻角色可能正在跑，先量就是错的值
            resting: false,
            settle: 0,
            rig: None,
            blend: 0.0,
            locks: [FootLock::default(); 2],
            poles: [None; 2],
            foot_rots: [None; 2],
            toe_clamps: [None; 2],
            ground: [None; 2],
            landing: [0.0; 2],
            landing_from: [None; 2],
            air_secs: [1.0; 2],
            anchors: [None; 2],
            correction: [0.0; 2],
            heel: [0.0; 2],
            pitch: [0.0; 2],
            pitch_full: [0.0; 2],
            ankle: [0.0; 2],
            toe_bend: [0.0; 2],
            lift_out: [0.0; 2],
            yaw_locks: [FootYawLock::default(); 2],
            slip: [0.0; 2],
            slip_prev: [None; 2],
            leg_roll: [0.0; 2],
            ankle_twist: [0.0; 2],
        }
    }
}

impl FootIk {
    /// 骨架换了（或彻底关掉）：把锚点、姿态缓存、抬跟进度全部清空。
    ///
    /// 缓存里的世界坐标只对当初那具骨架有意义；把它们带进新骨架，脚会被拽到
    /// 上一具骨架的位置上，观感就是「体态被掰变形」
    fn forget_skeleton(&mut self) {
        self.rig = None;
        self.blend = 0.0;
        self.settle = 0;
        self.locks = [FootLock::default(); 2];
        self.poles = [None; 2];
        self.foot_rots = [None; 2];
        self.toe_clamps = [None; 2];
        self.ground = [None; 2];
        self.landing = [0.0; 2];
        self.landing_from = [None; 2];
        self.air_secs = [1.0; 2];
        self.anchors = [None; 2];
        self.correction = [0.0; 2];
        self.heel = [0.0; 2];
        self.pitch = [0.0; 2];
        self.ankle = [0.0; 2];
        self.toe_bend = [0.0; 2];
        self.lift_out = [0.0; 2];
        self.yaw_locks = [FootYawLock::default(); 2];
        self.slip = [0.0; 2];
        self.slip_prev = [None; 2];
        self.leg_roll = [0.0; 2];
        self.ankle_twist = [0.0; 2];
    }

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

    /// 两只脚**踩实期间**前掌（球）在地面上的滑移速度（m/s）。
    ///
    /// 诊断面板上的「滑步」就是它，比 `correction` 更直接：修正量是**欠的账**，
    /// 这个是**脚底压地那一点真的在地上走**。
    pub fn slip(&self) -> [f32; 2] {
        self.slip
    }

    /// 两只脚最近一帧分给腿骨的自旋（度）：踝扛不动的那段里，转了多少换成
    /// 「股骨+胫骨绕自己骨轴滚」。和 `ankle_twist_deg` 相加 = 整段偏航
    pub fn leg_roll_deg(&self) -> [f32; 2] {
        [
            self.leg_roll[0].to_degrees(),
            self.leg_roll[1].to_degrees(),
        ]
    }

    /// 两只脚踝关节里**还剩**多少扭转（度）。分给腿骨之后它应当掉到十几度以内；
    /// 一直在 25°+ 就是 `leg_roll_share` 还不够（也就是画面上看到的外翻）
    pub fn ankle_twist_deg(&self) -> [f32; 2] {
        [
            self.ankle_twist[0].to_degrees(),
            self.ankle_twist[1].to_degrees(),
        ]
    }

    /// 两只脚最近一帧**绕前掌（球）转**的角度（度，自踩实起；正负号同身体转身方向相反）。
    ///
    /// 和 [`Self::yaw_lock_deg`] 对着看就是那段转身的分工：踝扛了多少、绕球转了多少。
    /// 两者相加 = 本体这一段支撑里总共转过的角
    pub fn ball_pivot_deg(&self) -> [f32; 2] {
        [
            self.yaw_locks[0].ball().to_degrees(),
            self.yaw_locks[1].ball().to_degrees(),
        ]
    }

    /// 两只脚当前承重点沿脚掌滑到哪了（0 = 脚踝，1 = 前掌）
    pub fn roll(&self) -> [f32; 2] {
        [self.locks[0].roll(), self.locks[1].roll()]
    }

    /// 两只脚最近一帧的抬跟进度（0..1）
    #[allow(dead_code)] // 调试面板 / 测试读数用
    pub fn heel_off(&self) -> [f32; 2] {
        self.heel
    }

    /// 两只脚最近一帧的脚掌俯角（度，相对地面，脚跟抬起为正）
    #[allow(dead_code)] // 调试面板 / 测试读数用
    pub fn pitch(&self) -> [f32; 2] {
        self.pitch
    }

    /// 两只脚最近一帧的**踝角**（度，脚掌与小腿的夹角）。90 = 站立，
    /// 蹬地时应当爬向 130~160。
    ///
    /// 用的是**这一帧实际解出来的膝**（和抬跟/地板同一个口径），所以面板上的数
    /// 就是画面上那个角。早先版本用动画的膝，读数比画面高 5~10° —— 面板够 90、
    /// 腿上还折着
    #[allow(dead_code)] // 调试面板 / 测试读数用
    pub fn ankle_deg(&self) -> [f32; 2] {
        self.ankle
    }

    /// 前掌骨（球窝/跖趾关节）反向折角（度）。抬跟时脚趾留在地上、脚掌绕它
    /// 转过去，折的就是这个角；超过 `TOE_BEND_MAX` 之后脚趾才跟着脚掌走
    #[allow(dead_code)] // 调试面板 / 测试读数用
    pub fn toe_bend_deg(&self) -> [f32; 2] {
        self.toe_bend
    }

    /// 两只脚这一帧施加的偏航补偿（度）。正 = 把脚掌往左（+Y）拧回去。
    ///
    /// 面板上要跟 `foot_yaw_limit` 对着看：读数已经顶到上限，说明这只脚
    /// 撑不住了、正跟着身体在地上撇（那是真实反应，不是 bug）
    pub fn yaw_lock_deg(&self) -> [f32; 2] {
        [
            self.yaw_locks[0].applied.to_degrees(),
            self.yaw_locks[1].applied.to_degrees(),
        ]
    }

    /// 两只脚这一帧施加的锁脚强度（0..1），调试面板看「加压/放开」的过程
    pub fn strength(&self) -> [f32; 2] {
        [self.locks[0].strength(), self.locks[1].strength()]
    }

    /// 两只脚是否处于支撑期
    pub fn planted(&self) -> [bool; 2] {
        [self.locks[0].is_planted(), self.locks[1].is_planted()]
    }

    /// 第 `index` 只脚脚下量到的地面（没打中就是 `None`）
    pub fn ground(&self, index: usize) -> Option<GroundHit> {
        self.ground.get(index).copied().flatten()
    }
}

/// 参考骨架（默认角色 Amy）的腿长：`髋→膝 + 膝→踝`，静止姿态实测 0.571 m。
///
/// 所有以**米**给的参数（`max_correction`、`heel_off_lead`、地面射线的抬升/深度）
/// 都按 `scale_leg = 腿长 / 它` 换算。参数值本身仍然是「在参考骨架上看着顺眼」
/// 的那个米数，换到 0.89 m 的 X Bot 或 0.64 m 的 Big Vegas 上比例不变，
/// 不会一个起跟走得太早、一个被拽得太远
const REFERENCE_LEG_LENGTH: f32 = 0.571;

/// 参考骨架的「踝→前掌」长度（0.128 m）。抬跟那一侧的**高度**阈值
/// （`toe_pivot_rise`：动画把脚踝抬起多少米才开始绕前掌转）按它换算 ——
/// 那是脚掌绕前掌转出来的位移，跟脚有多长成正比，跟腿长无关
const REFERENCE_FOOT_LENGTH: f32 = 0.128;

/// 模型实体下的骨架缓存。实体 id 与模型同生共死，不需要额外失效判断。
struct FootRig {
    /// 从场景最顶层到 hips（含）的链：用局部变换自己乘出世界矩阵，
    /// 不依赖尚未传播的 `GlobalTransform`
    spine: Vec<Entity>,
    /// 两条腿 `[左, 右]`，顺序与 [`FootIk::contacts`] 一致
    legs: [Leg; 2],
    /// 腿长换算系数（见 [`REFERENCE_LEG_LENGTH`]），夹在 0.4~2.5 兜底
    scale_leg: f32,
    /// 脚长换算系数（见 [`REFERENCE_FOOT_LENGTH`]），同样夹住
    scale_foot: f32,
    /// `foot_tilt` 是否已经在「脚掌平放的帧」上量过（见 [`calibrate_at_rest`]）。
    ///
    /// 骨架一到位就量的话，联网/自动走路时量到的是摆动中的脚，踝角判据会整体偏掉；
    /// 没标定之前求解端照旧跑，等第一次静止帧再覆盖
    tilt_calibrated: bool,
}

#[derive(Clone, Copy)]
struct Leg {
    upper: Entity,
    lower: Entity,
    foot: Entity,
    /// 前掌关节（`*ToeBase`）。有它承重点才能在支撑末期滑到前掌
    toe: Option<Entity>,
    /// 踝到前掌的竖直距离（0 = 没量到）。锁脚拿它把踝高换算成真正的地面高度
    toe_drop: f32,
    /// 静止姿态下「踝→前掌骨」相对地面的俯角（度）。纯骨骼摆放的常量：
    /// 脚掌平放时 `foot_pitch_deg` 读出来的就是它（这个项目的 rig 是 26~62° 不等）
    foot_tilt: f32,
}

impl Plugin for FootIkPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FootIkSettings>()
            .add_systems(
                Update,
                // 早建 rig（腿长/骨骼链不受姿态影响），但 `foot_tilt` 交给
                // `calibrate_at_rest` 在静止帧上覆盖，见那边的注释
                attach_rigs
                    .before(super::character::finish_model_setup)
                    .run_if(in_state(GameState::Game)),
            )
            .configure_sets(
                PostUpdate,
                FootIkSet
                    .after(drive_pose)
                    .before(TransformSystems::Propagate),
            )
            .add_systems(
                PostUpdate,
                (
                    sample_ground,
                    // 支撑权重/静止判据要先就位：标定端读的是本帧的 `resting`
                    feed_contacts,
                    calibrate_at_rest,
                    solve_foot_ik,
                )
                    .chain()
                    .in_set(FootIkSet)
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
    // 本地角色（`AnimModelRoot`）与联机远端角色（`FootIkTarget`）走同一套求解
    models: Query<
        Entity,
        (
            Or<(With<AnimModelRoot>, With<FootIkTarget>)>,
            Without<FootIk>,
        ),
    >,
    mut iks: Query<(Entity, &mut FootIk)>,
    children: Query<&Children>,
    names: Query<&Name>,
    parents: Query<&ChildOf>,
    transforms: Query<&Transform>,
) {
    for model in &models {
        commands.entity(model).insert(FootIk::default());
    }

    for (model, mut ik) in &mut iks {
        // 骨架换了就重量一遍：缓存里的骨骼实体、静止俯角和骨骼尺寸都只属于
        // 当初那具骨架，绑错了会把整条腿的阈值（`scale_leg/scale_foot`）一起带偏。
        // 代价是每帧几次 `Query::get`，比每次重扫层级便宜得多
        if ik
            .rig
            .as_ref()
            .is_some_and(|rig| rig_is_stale(rig, model, &parents, &transforms))
        {
            ik.forget_skeleton();
        }
        if ik.rig.is_some() {
            continue;
        }
        if let Some(rig) = build_rig(model, &children, &names, &parents, &transforms) {
            debug!(
                "脚部 IK 骨架就绪：{model:?}，{} 节脊椎链，前掌骨静止俯角 {:.1}°/{:.1}°",
                rig.spine.len(),
                rig.legs[0].foot_tilt,
                rig.legs[1].foot_tilt
            );
            ik.rig = Some(rig);
        }
    }
}

/// 缓存的骨架是否已经不再属于这具骨架：腿骨实体失效，或者 hips 已经不在
/// `model` 底下（模型被就地换皮 / 场景重载时，实体可能还活着但骨架已经换了一具）。
fn rig_is_stale(
    rig: &FootRig,
    model: Entity,
    parents: &Query<&ChildOf>,
    transforms: &Query<&Transform>,
) -> bool {
    for leg in &rig.legs {
        if transforms.get(leg.upper).is_err()
            || transforms.get(leg.lower).is_err()
            || transforms.get(leg.foot).is_err()
        {
            return true;
        }
    }
    let Some(&hips) = rig.spine.last() else {
        return true;
    };
    if transforms.get(hips).is_err() {
        return true;
    }
    // hips 往上一路走到 model：走不到说明它已经挂到别的模型底下了
    let mut cursor = hips;
    for _ in 0..64 {
        if cursor == model {
            return false;
        }
        match parents.get(cursor) {
            Ok(child_of) => cursor = child_of.parent(),
            Err(_) => return true,
        }
    }
    true
}

/// 在当前动画栈里把两脚支撑权重灌给正在被驱动的那个角色。
///
/// 这是单人动画栈（`AnimRuntime` / `PoseDriver` 都是 Resource）的桥；
/// 联机版本里每个角色各有一套动画求值，直接写自己的 [`FootIk::contacts`]，
/// 求解系统不需要任何改动。
/// 每只脚从踝上方往下打一条射线量地面。
///
/// 为什么要打射线：脚部 IK 的「地面高度」以前是从脚踝/前掌的位置**推断**的，
/// 那只在平地上成立。上台阶时脚踝还在下一层、前掌可能已经悬在台阶上方，
/// 推断出的地面会和真实台阶面差一整层；斜坡上更是差在法线方向。
///
/// 现在把真实命中点写进 [`FootIk::ground`]，`FootLock` 踩实时优先用它
/// （见 `FootContact::floor`）。射线没打中才退回推断。
///
/// 起射点抬到脚踝上方 `ground_probe_rise`：既避开自己的脚掌碰撞体，
/// 也能在台阶边缘那几帧量到下一层而不是被自己挡住。
fn sample_ground(
    settings: Res<FootIkSettings>,
    mut iks: Query<(&mut FootIk, Option<&FootIkTarget>)>,
    transforms: Query<&Transform>,
    spatial: SpatialQuery,
) {
    for (mut ik, remote) in &mut iks {
        let mut ground = [None; 2];
        // 这条射线只服务 IK：开关关掉、或者这台角色已经淡出到 0，就不该再打。
        // `blend` 是上一帧的淡出进度，所以关掉后最多再走完一个 `blend_secs`
        let probing =
            settings.ground_probe && settings.is_active(&ik, remote.is_some()) && ik.blend > 0.0;
        if probing && let Some(rig) = ik.rig.as_ref() {
            // 排除角色自身：射线要穿过的第一个东西否则就是自己的胶囊/脚掌
            let filter =
                SpatialQueryFilter::from_excluded_entities(rig.spine.iter().copied().chain(
                    rig.legs.iter().flat_map(|leg| {
                        [leg.upper, leg.lower, leg.foot].into_iter().chain(leg.toe)
                    }),
                ));
            let mut spine = Affine3A::IDENTITY;
            for &entity in &rig.spine {
                if let Ok(transform) = transforms.get(entity) {
                    spine *= transform.compute_affine();
                }
            }
            for (index, leg) in rig.legs.iter().enumerate() {
                let (Ok(upper), Ok(lower), Ok(foot)) = (
                    transforms.get(leg.upper),
                    transforms.get(leg.lower),
                    transforms.get(leg.foot),
                ) else {
                    continue;
                };
                let ankle: Vec3 = (spine
                    * upper.compute_affine()
                    * lower.compute_affine()
                    * foot.compute_affine())
                .translation
                .into();
                // 抬升/深度都是「米」，按腿长换算：小个子角色不用打那么远的射线
                let origin = ankle + Vec3::Y * settings.ground_probe_rise * rig.scale_leg;
                let Some(hit) = spatial.cast_ray(
                    origin,
                    Dir3::NEG_Y,
                    settings.ground_probe_depth * rig.scale_leg,
                    true,
                    &filter,
                ) else {
                    continue;
                };
                ground[index] = Some(GroundHit {
                    height: origin.y - hit.distance,
                    normal: hit.normal,
                });
            }
        }
        ik.ground = ground;
    }
}

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
    // 「静止」= 等效地速和速度轴都归零。混合空间从 Running 滑回 Idle 的头几帧
    // 速度轴已经归零、脚却还在收，所以标定端还会多等几帧
    ik.resting = !focus.preview
        && driver.evaluated
        && driver.ground_speed.abs() < 0.05
        && driver.point[1].abs() < 0.02;
}

/// 静止帧重标定 `foot_tilt`（踝→前掌骨的俯角，脚掌平放时 ≈ 29.7°）。
///
/// 这个值按定义只在脚掌平放时成立，但它是跟着骨架一起量的：假设角色一进场就在跑
/// （联网时由服务器推着走、或 `--auto-walk`），量到的是摆动中的脚，实测 79.0°/28.6°。
/// 差 50° 会把「抬跟时的踝角判据」(`90 + 俯角 − 小腿前倾`) 整体推偏，蹬地那一下
/// 脚踝该抬多高就判错。
///
/// 骨架/腿长这类量不受影响，所以不推迟建 rig，只在混合空间连续输出静止时覆盖
/// 这一对常量，覆盖一次就不再动。
fn calibrate_at_rest(
    mut iks: Query<(Entity, &mut FootIk)>,
    parents: Query<&ChildOf>,
    transforms: Query<&Transform>,
) {
    for (model, mut ik) in iks.iter_mut() {
        // 判据由各自动画层自己写（本地读混合输出，远端读复制下来的实测速度）：
        // 每个角色一只脚在动的时机都不一样，用一个全局标志会让远端一直等不到静止帧
        if !ik.resting {
            ik.settle = 0;
            continue;
        }
        ik.settle = ik.settle.saturating_add(1);
        // 混合空间从 Running 滑回 Idle 的头几帧速度轴已经归零、脚却还在收
        if ik.settle < 3 {
            continue;
        }

        let Some(rig) = ik.rig.as_mut() else {
            continue;
        };
        if rig.tilt_calibrated {
            continue;
        }
        let mut tilts = [0.0f32; 2];
        let mut measured = false;
        for (index, leg) in rig.legs.iter().enumerate() {
            let Some(toe) = leg.toe else {
                continue;
            };
            let (Some(foot_pos), Some(toe_pos)) = (
                world_position(model, leg.foot, &parents, &transforms),
                world_position(model, toe, &parents, &transforms),
            ) else {
                continue;
            };
            let tilt = foot_pitch_deg(toe_pos, foot_pos);
            // 明显不是平放的帧（量到负角 / 太陡）不要，宁可继续等下一帧静止
            if !(-5.0..85.0).contains(&tilt) {
                continue;
            }
            tilts[index] = tilt;
            measured = true;
        }
        if !measured {
            continue;
        }
        for (index, leg) in rig.legs.iter_mut().enumerate() {
            leg.foot_tilt = tilts[index];
        }
        rig.tilt_calibrated = true;
        // 抬跟的跨帧基准是按旧俯角累计的，换常量后清零重新累
        ik.pitch_full = [0.0; 2];
        debug!(
            "脚部 IK 静止重标定：前掌骨俯角 {:.1}°/{:.1}°",
            tilts[0], tilts[1]
        );
    }
}

/// 逐角色求解：读本帧姿态 → 锁脚目标 → 两骨 IK → 写回腿部旋转。
fn solve_foot_ik(
    time: Res<Time>,
    settings: Res<FootIkSettings>,
    jump: Option<Res<JumpAction>>,
    mut iks: Query<(&mut FootIk, Option<&FootIkTarget>)>,
    mut transforms: Query<&mut Transform>,
) {
    let dt = time.delta_secs();
    let airborne = jump.as_ref().is_some_and(|action| action.is_airborne());

    for (mut ik, remote) in &mut iks {
        // 目标权重：全局开 + 本角色开 + （模拟端）模拟端开。
        // 关掉时平滑淡出而不是直接跳没
        let want = if settings.is_active(&ik, remote.is_some()) {
            1.0
        } else {
            0.0
        };
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
            poles,
            foot_rots,
            toe_clamps,
            ground,
            landing,
            landing_from,
            air_secs,
            heel,
            pitch,
            pitch_full,
            ankle,
            toe_bend,
            lift_out,
            yaw_locks,
            slip,
            slip_prev,
            leg_roll,
            ankle_twist,
            ..
        } = &mut *ik;
        let ground = *ground;

        // 腾空：立刻放下支撑期攒下的位置残差，这一段脚完全交给跳剪辑。
        // 旧地面锚点若还留着慢放，身体被抛起来、脚被留在起跳点，腿会被绷成
        // 垂直一条。蓄力蹬地（Takeoff）和落地缓冲（Land）不在此列，仍按锁脚走。
        if airborne {
            locks[0].release_airborne();
            locks[1].release_airborne();
        }

        // 骨架还没就绪：这一帧什么都不做（模型加载完会自己重试）
        let Some(rig) = rig.as_ref() else {
            continue;
        };
        // 逐角色换算：米制阈值 × 这台角色的骨骼尺寸
        let config = settings.lock_config(rig.scale_leg, rig.scale_foot);

        // 彻底关掉：清锚点，重新打开时从下一次支撑重新贴地，
        // 不会拿几帧前的旧位置把人拽回去
        if !on && blend <= 0.0 {
            locks[0].reset();
            locks[1].reset();
            *anchors = [None; 2];
            *correction = [0.0; 2];
            *poles = [None; 2];
            *foot_rots = [None; 2];
            *toe_clamps = [None; 2];
            *slip = [0.0; 2];
            *slip_prev = [None; 2];
            *landing = [0.0; 2];
            *landing_from = [None; 2];
            *air_secs = [1.0; 2];
            *heel = [0.0; 2];
            *pitch = [0.0; 2];
            *ankle = [0.0; 2];
            *lift_out = [0.0; 2];
            *yaw_locks = [FootYawLock::default(); 2];
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
                continue;
            };
            let Some(lower) = transforms.get(leg.lower).ok().map(|transform| *transform) else {
                continue;
            };
            let Some(foot) = transforms.get(leg.foot).ok().map(|transform| *transform) else {
                continue;
            };

            let upper_world = spine_world * upper.compute_affine();
            let lower_world = upper_world * lower.compute_affine();
            let foot_world = lower_world * foot.compute_affine();
            // 前掌：脚骨的世界矩阵 × 前掌骨局部变换
            let toe_local = leg
                .toe
                .and_then(|entity| transforms.get(entity).ok().map(|transform| *transform));
            let toe = toe_local
                .map(|transform| (foot_world * transform.compute_affine()).translation.into());
            let toe_rot = toe_local.map(|transform| {
                (spine_rot * upper.rotation * lower.rotation * foot.rotation * transform.rotation)
                    .normalize()
            });

            poses[index] = Some(LegPose {
                root: upper_world.translation.into(),
                joint: lower_world.translation.into(),
                end: foot_world.translation.into(),
                toe,
                toe_rot,
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
            // 重心（骨盆）= 脊椎链最末一节的世界位置。抬跟时机拿它沿脚掌朝向投影，
            // 判「重心有没有压过着地脚」；用本帧自己乘出来的矩阵，不落后一帧
            let body = Some(Vec3::from(spine_world.translation));

            // ---- 脚掌偏航锁：踩实期间脚掌的世界朝向不许被身体拧着转 ----
            // 必须在 `locks[index].update` **之前**做：锚点记录、抬跟的脚向量、
            // 「前掌不穿地」的夹紧全都按这具几何算，先拧再交出去，三者口径才一致。
            // 踩实判定取**上一帧**的状态：偏航锁要先把几何拧好再交给 `FootLock`，
            // 而 `FootLock` 的迟滞是在它自己的 `update` 里翻的。落地过渡那一段
            // 仍然用本帧翻过之后的状态（见下面 `planted_now`），语义和以前一致
            let planted_before = locks[index].is_planted();
            let replanted_before = planted_before && air_secs[index] >= TOUCHDOWN_RESTART_SECS;
            let yaw_anim = pose.toe.and_then(|toe| foot_yaw(pose.end, toe));
            let yaw_deg = yaw_locks[index]
                .update(
                    yaw_anim,
                    planted_before,
                    replanted_before,
                    settings.foot_yaw_limit,
                    settings.ball_pivot_enter,
                    dt,
                )
                .to_degrees();
            // 超出踝活动范围的那段角改成「绕前掌（球）转」：承重点挪到前掌、
            // 脚跟抬起来。球是脚掌上真正压在地上的那一点 —— 它不动，画面上就没有
            // 滑动；踝随小腿走（不再被拧），腿也只需要几度外展就能吸收。
            //
            // 两个量都是**绝对**的（自踩实起累计），不是每帧增量：`FootLock` 每帧
            // 用「踩实时的脚向量 − 当帧脚向量」算锚点，重复累加会转两倍
            let ball_deg = yaw_locks[index].ball().to_degrees();
            let enter = settings
                .ball_pivot_enter
                .max(0.0)
                .min(settings.foot_yaw_limit.max(0.0));
            let span = (settings.foot_yaw_limit - enter).max(1e-3);
            // 承重点用 2° 的短坡切过去：几何项本身 ∝ 绕球角（起步那几帧本来就接近 0），
            // 这里的坡只是为了不在一帧里把 `roll`（抬跟那条路）的项也一起翻倍
            let pin = smoothstep((ball_deg.abs() / 2.0).clamp(0.0, 1.0));
            let heel_up = smoothstep((ball_deg.abs() / span).clamp(0.0, 1.0));
            locks[index].set_ball_pin(pin);
            locks[index].set_heel_floor(HEEL_PIVOT_FLOOR_MAX * heel_up);

            // 再乘全局淡入：和位置修正一样，IK 关掉的过程中这条锁也要跟着放，
            // 否则位置已经交还动画、脚掌朝向还拧着 → 结束时脚突然撇一下
            let yaw_out = yaw_deg * blend;
            // 脚掌绕它转的那根轴（地面法线）。下面分给腿骨的自旋必须**绕同一根
            // 指上的轴**，否则拧的是反的：踝里的扭转会翻倍而不是缩小
            let up = ground[index].map_or(Vec3::Y, |hit| hit.normal);
            if yaw_out.abs() > 1e-3 {
                // 绕地面法线拧（斜坡上仍贴着坡面；没有射线命中就退回世界竖直轴）。
                // 脚掌连同前掌整块刚体转，绕的是踝：踝还钉在锚点上，转出去的是前掌
                let rigid = Quat::from_axis_angle(up, yaw_out.to_radians());
                pose.foot_rot = (rigid * pose.foot_rot).normalize();
                if let Some(toe_rot) = pose.toe_rot {
                    pose.toe_rot = Some((rigid * toe_rot).normalize());
                }
                if let Some(toe) = pose.toe {
                    pose.toe = Some(pose.end + rigid * (toe - pose.end));
                }
            }

            // 这段偏航现在全压在踝上：小腿跟着身体转过去、脚掌被钉在原地，
            // 胫骨相对脚掌就被拧了 `yaw_out` 那么多。画面上是「小腿被强行扭转，
            // 膝盖顶到外侧」。真人做不到那个角 —— 髋内旋 + 胫骨自旋先吃掉一大半，
            // 剩下的才进踝。分给腿骨的那段由 `solve_leg` 换成绕骨轴的滚
            //
            // 只在**踩实**的时候接：离地后脚不再被锁，这段角差本来就该还给动画。
            // 而且退回要限速（见 `LEG_ROLL_RELEASE`）—— 一帧退回 18° 就是膝盖被拧
            let planted_roll = locks[index].is_planted();
            let want_roll = if planted_roll {
                split_leg_roll(
                    yaw_out,
                    settings.leg_roll_share,
                    settings.leg_roll_max.to_radians(),
                )
            } else {
                0.0
            };
            let split = advance_leg_roll(leg_roll[index], want_roll, planted_roll, dt);
            leg_roll[index] = split;
            ankle_twist[index] = yaw_out - split;

            // 目标位置里已经带着「解除锁定后还没还完的残差」，
            // 所以抬脚是平滑追上动画，不会从锚点瞬移回去
            let (lock_target, lock_weight) = locks[index].update(
                FootContact {
                    ankle: pose.end,
                    toe: pose.toe,
                    toe_drop: rig.legs.get(index).map(|leg| leg.toe_drop).unwrap_or(0.0),
                    floor: ground[index].map(|hit| hit.height),
                    body,
                },
                support,
                on,
                dt,
                &config,
            );
            anchors[index] = locks[index].anchor();
            // 锁脚权重再乘全局淡入：任何开关/切换都在这里平滑掉
            let weight = (lock_weight * blend).clamp(0.0, 1.0);
            let mut target = pose.end.lerp(lock_target, weight);
            correction[index] = (target - pose.end).length();

            // 膝盖朝向（极点）：动画的膝盖方向低通一下再交给求解器。
            // 混合空间换剪辑 / 权重抖动时，动画极点会逐帧晃，膝盖跟着晃就是「顿」；
            // 平滑后膝盖只朝一个方向慢慢转，转身时 50ms 的滞后肉眼看不出来。
            // 抬跟段要拿它探解，所以在这里算一次、后面共用（每帧同一条腿只平滑一次）
            // 脚钉在地上、身体转过去的时候，膝盖该跟着**脚**的矢状面弯，而不是
            // 跟着身体：髋转、踝不动，股骨和胫骨只能在膝上横向掰开一个折角 ——
            // 这就是画面上那个「膝盖外翻」。极点换成脚掌指向（踝→前掌的水平投影），
            // 膝盖就一直在脚的正上方弯；偏航锁得越多，越以脚为准
            let foot_forward = pose
                .toe
                .map(|toe| {
                    let v = toe - pose.end;
                    Vec3::new(v.x, 0.0, v.z)
                })
                .filter(|v| v.length_squared() > 1e-6)
                .unwrap_or(Vec3::ZERO);
            // 只在**超出踝活动范围**的那段里以脚为准。走路时偏航锁只有几度
            // （脚跟落地→离地本来就会自然外旋几度），那时极点必须原样听动画的，
            // 否则膝盖会被一个纯水平的矢量带偏，站直也会外翻
            let enter_rad = settings.ball_pivot_enter.max(1.0).to_radians();
            let ramp = if locks[index].is_planted() {
                smoothstep(((yaw_out.abs() - enter_rad) / enter_rad.max(1e-3)).clamp(0.0, 1.0))
            } else {
                // 空中 / 抬腿：脚掌自己正在转（背屈、外旋），拿它当极点会让膝盖
                // 跟着甩出去 —— 摆动期一律听动画
                0.0
            };
            let animated_pole = knee_pole(
                (pose.joint - pose.root).normalize_or_zero(),
                foot_forward,
                ramp,
            );
            let smoothed_pole = smooth_pole(poles[index], animated_pole, dt, settings.pole_secs);
            if smoothed_pole.is_some() {
                poles[index] = smoothed_pole;
            }
            // 极点退化（腿完全伸直）：交给求解器自己兜底
            let pole = smoothed_pole.unwrap_or(pose.joint - pose.root);

            // ---- 抬跟：绕前掌把脚掌俯仰补到目标角 ----
            // 锁脚只保证「前掌在原地」，脚跟抬多高完全取决于脚掌俯仰多少；动画
            // 自己的蹬地幅度常常不到 45°，看起来像是还没踩实就抬脚。这里把脚掌
            // 继续俯下去，踝随之抬高，前掌仍钉在原地。只补不足：动画自己俯得更大
            // 就原样保留。
            //
            // 位置和旋转必须一起抬（绕同一个前掌轴转同一个角），否则脚掌转了、
            // 踝却没抬，前掌就被压进地里，再被「不穿地」的夹紧掰回来 —— 观感就是
            // 脚掌先翘一下又放平。离地后由 `lift_out` 在 0.1s 内收干净。
            //
            // 口径是**踝角** φ = 90 + P − L（P = 脚掌相对地面的俯角，L = 小腿前倾）。
            // 关键：L 必须拿**解出来的膝**算。锁脚把踝钉在原地之后，动画的膝位置
            // 就不是真实几何了 —— 拿动画膝算，读数能有 90，画面上小腿已经压到 70，
            // 面板和画面成了两个口径。所以下面每一轮都先探一次解
            let heel_progress = locks[index].heel_off();
            // 离地后 0.1s 内收回，而不是跟锁脚强度一起慢放 0.26s（见
            // LIFT_RELEASE_SECS 的说明）。踩实期间恒为 1，支撑里抖动也不会掉
            let lift_out_now = if locks[index].is_planted() {
                1.0
            } else {
                (lift_out[index] - dt / LIFT_RELEASE_SECS).max(0.0)
            };
            lift_out[index] = lift_out_now;
            let lift =
                smoothstep(((heel_progress - ROLL_SPAN) / (1.0 - ROLL_SPAN)).clamp(0.0, 1.0))
                    * smoothstep(lift_out_now);
            // 动画自己给的地面俯角（不含我们加的）
            // 骨骼自己的「踝→前掌」向量在静止姿态就是俯的：脚掌平放时它已经
            // 是 26~62°（每根 rig 的摆放不同）。下面 `pitch` / 踝角 / 目标全用
            // **脚掌面**口径（平放 = 0），所以每帧先减掉这个常量
            let foot_tilt = rig.legs.get(index).map(|leg| leg.foot_tilt).unwrap_or(0.0);
            let pitch_anim = match pose.toe {
                Some(toe) => foot_pitch_deg(toe, pose.end) - foot_tilt,
                None => 0.0,
            };
            let mut pitch_deg = pitch_anim;
            // 抬跟的旋转每次都从**动画原样**的脚掌出发（`heel_off_lift` 的位移也是
            // 相对动画踝算的）。迭代里如果在已经转过的 `pose.foot_rot` 上再乘，
            // 各轮的旋转会叠起来，俯角就远超目标
            let foot_rot0 = pose.foot_rot;
            let base_target = target;
            // 抬跟落定之后踝要抬多少（相对动画踝），后面 `lock_target` 要一起带上
            let lift_offset: Vec3;
            // 抬跟这一帧实际让脚掌绕前掌转了 (轴, 角多少度)：前掌骨要按它兜回来
            let mut lift_axis = Vec3::Y;
            let mut lift_angle = 0.0f32;
            if weight > 0.0
                && let Some(toe) = pose.toe
            {
                let floor = settings.heel_off_ankle_min;
                // 本帧最多补到多少度：按**上一帧**实际落到的俯角限速、跨帧累积。
                // 不能按单次调用限速 —— 抬跟每帧从头重算（无状态），单次限 4° 的话
                // 三圈也只有 12°，看上去就是卡住了
                let cap = pitch_full[index] + ANKLE_FLOOR_RATE * dt;
                // 目标踝角：抬跟斜坡把动画自己的踝角往 `heel_off_ankle` 推（只推不拉），
                // 地板 `heel_off_ankle_min` 是整段支撑期都得守住的底
                let mut phi_goal: Option<f32> = None;
                // 上一次探解时的 (俯角, 小腿前倾)，用来量 dL/dP
                let mut last = (pitch_anim, 0.0f32);
                let mut slope = 0.0f32;
                for _ in 0..4 {
                    let Some((knee, ankle_now)) = probe_leg(pose, target, pole) else {
                        break;
                    };
                    let lean = shank_lean_deg(knee, ankle_now, toe);
                    let phi = 90.0 + pitch_deg - lean;
                    let goal = *phi_goal.get_or_insert_with(|| {
                        let animated = 90.0 + pitch_anim - lean;
                        (animated + (settings.heel_off_ankle - animated).max(0.0) * lift).max(floor)
                    });
                    let need = goal - phi;
                    // 进入容差就停；冲过头超过 1° 也要往回修（斜率在低俯角处偏大，
                    // 一步跨过头是常事，不修就永远停在 98°）
                    if (-1.0..=0.15).contains(&need) {
                        break;
                    }
                    // 踝一抬起来小腿就更斜（实测 dL/dP ≈ 0.7），把差值直接加到俯角上
                    // 收敛比接近 0.67，每轮只爬一半 —— 用测到的斜率换算成一步到位
                    if (pitch_deg - last.0).abs() > 1e-2 {
                        slope = ((lean - last.1) / (pitch_deg - last.0)).clamp(-0.5, 0.8);
                    }
                    // 俯角上限是「骨向量不许接近竖直」：面口径要减掉静止俯角
                    let pitch_max = (HEEL_OFF_PITCH_MAX - foot_tilt).max(pitch_anim);
                    let want = (pitch_deg + need / (1.0 - slope).max(0.2) * 0.9)
                        .max(pitch_anim)
                        .min(cap.max(pitch_anim))
                        .min(pitch_max);
                    // 上一轮若冲过头（斜率在低俯角处偏大），这一轮允许往回修
                    if (want - pitch_deg).abs() <= 1e-3 {
                        break;
                    }
                    let Some((lift_rot, lift_delta, lifted_pitch)) =
                        heel_off_lift(toe, pose.end, want, foot_tilt, 1.0)
                    else {
                        break;
                    };
                    let (axis, angle) = lift_rot.to_axis_angle();
                    lift_axis = axis;
                    lift_angle = angle.to_degrees();
                    pose.foot_rot = (lift_rot * foot_rot0).normalize();
                    // `heel_off_lift` 的位移是**相对动画踝**算的，所以这里重置而不是累加。
                    // 这一轮算出来的是「抬满」的几何，只用来给下一轮探 `phi`；
                    // 真正落到画面上的位移在循环之后按 `heel_k` 重算
                    target = base_target + lift_delta * weight;
                    last = (pitch_deg, lean);
                    pitch_deg = lifted_pitch;
                }
            }
            // 这一轮迭代给出的「满量」俯角（还没淡出）：`ANKLE_FLOOR_RATE` 的
            // 跨帧累加基准，见 `FootIk::pitch_full`
            pitch_full[index] = pitch_deg;
            // 抬跟 = 绕前掌的**一个刚体变换**：脚掌的俯角、踝被抬起来的位移，
            // 是同一件事的两个面。所以两者必须按**同一个**系数淡入淡出。
            //
            // 原来只把位移乘 `weight`、旋转按满量施加，代价是锁脚一松开（`weight`
            // 是二值的：残差小于 `MIN_OFFSET` 就整段跳回 0）就会出现一帧台阶 ——
            // 位移已经还给动画、脚掌还翘着 17°多，脚上一帧被按在地上的前掌这一帧
            // 直接沉进地里 3cm，再靠踝角低通花四帧浮回来。玩家看到的就是
            // 「停下来之后脚又落下 → 抬起 → 重新踩在地上」。
            //
            // 系数只看两件事：整个 IK 的开关（`blend`）和「这只脚已经离地多久」
            // （`lift_out`，`LIFT_RELEASE_SECS` = 0.1s）。**不能乘 `strength`**：
            // 踩实那一帧 `strength` 还在从 0 爬，乘上去等于把抬跟整体推迟 3~4 帧，
            // 蹬地时机就晚了；离地之后 `lift_out` 从 1 渐隐，正好是这段该走的节奏
            let heel_k = (blend * smoothstep(lift_out_now)).clamp(0.0, 1.0);
            match pose.toe {
                // 按 `heel_k` 重算一遍最终变换：同一个角度，旋转和位移天然对齐
                Some(toe) => match heel_off_lift(toe, pose.end, pitch_deg, foot_tilt, heel_k) {
                    Some((lift_rot, lift_delta, lifted_pitch)) => {
                        let (axis, angle) = lift_rot.to_axis_angle();
                        lift_axis = axis;
                        lift_angle = angle.to_degrees();
                        pose.foot_rot = (lift_rot * foot_rot0).normalize();
                        lift_offset = lift_delta;
                        target = base_target + lift_delta;
                        pitch_deg = lifted_pitch;
                    }
                    None => {
                        pose.foot_rot = foot_rot0;
                        lift_offset = Vec3::ZERO;
                        target = base_target;
                        pitch_deg = pitch_anim;
                    }
                },
                None => {
                    pose.foot_rot = foot_rot0;
                    lift_offset = Vec3::ZERO;
                    target = base_target;
                    pitch_deg = pitch_anim;
                }
            }
            pitch[index] = pitch_deg;
            // 抬完之后实际落在多少度的踝角（调试面板拿它对着 90~160 的区间看）。
            // 同样按解出来的膝算 —— 这是**画面上的那个角**，不是动画膝的近似
            let ankle_read = match pose.toe {
                Some(toe) => probe_leg(pose, target, pole).map_or(0.0, |(knee, ankle_now)| {
                    (90.0 + pitch_deg - shank_lean_deg(knee, ankle_now, toe)).clamp(0.0, 180.0)
                }),
                None => 0.0,
            };
            ankle[index] = ankle_read;
            toe_bend[index] = 0.0;
            heel[index] = heel_progress;

            // 脚掌朝向（踝角低通 + 前掌不穿地的有记忆偏移）。注意这一段必须在
            // 「weight 为 0 就跳过」**之前**：抬脚后锁脚权重就是 0，摆动期整段
            // 会被跳过，脚掌就完全跟动画，低通和夹紧淡出等于没装（调 foot_secs
            // 不会有任何反应），而「脚掌闪一下」正是发生在这一整段里。
            // 夹紧权重只看「有没有踩实」，**不能乘 `weight`**：
            // 落地那一帧锚点刚装上、残差还是 0 → `weight` 是 0，但脚已经在地上、
            // 前掌该抬的一步不能省。乘上就等于落地首帧不夹、第二帧全量补上，
            // 观感就是「落下去又弹一下」
            let clamp_weight = locks[index].presence().clamp(0.0, 1.0);
            // 落地过渡：踩实后几帧内把脚掌从空中的限速值拉到动画值。
            // 这一段的 `planted_now` 要用**本帧**更新后的状态（锁脚的迟滞在这一帧才翻），
            // 用上一帧的会让抬脚/落地整体晚一帧，脚掌会多憋一帧再放
            let planted_now = locks[index].is_planted();
            let replanted = planted_now && air_secs[index] >= TOUCHDOWN_RESTART_SECS;
            let (landing_now, air_secs_now) = advance_landing(
                landing[index],
                planted_now,
                air_secs[index],
                dt,
                settings.foot_landing_secs,
            );
            // 真的落下来了：把「落地这一刻」的踝角记成过渡起点
            if replanted {
                landing_from[index] = foot_rots[index];
            }
            landing[index] = landing_now;
            air_secs[index] = air_secs_now;
            // 夹紧要按「IK **最终**把踝放到哪儿」算（`lock_target`），不能用
            // `target`：后者是 `pose.end` 到 `lock_target` 的渐变，落地后 2~5 帧
            // 还在从 0 爬到 1，需求（wanted = 地面 − 踝高）跟着一点点变 ——
            // 那 3~4° 的「落下去又反弹」就是这么来的。
            // 也不能钉成锚点：支撑后期承重点滑向前掌，踝会跟着抬高，
            // 钉在锚点算等于一直认为踝很低，前掌会被抬起 3cm（浮空）。
            // 抬跟把踝又绕前掌抬了一截（`lift_offset`），这里要一起算进去，
            // 否则夹紧会以为前掌还沉在地下，把脚掌往回掰
            let clamp_ankle = if planted_now {
                lock_target + lift_offset
            } else {
                target
            };
            let (foot_rot, rot_state, clamp_state) = resolve_foot_orientation(
                foot_rots[index],
                toe_clamps[index],
                pose.foot_rot,
                pose.lower_rot,
                planted_now,
                weight > 0.0,
                landing_now,
                landing_from[index],
                locks[index].toe_floor(),
                pose.toe,
                rig.legs.get(index).map(|leg| leg.toe_drop).unwrap_or(0.0),
                locks[index].toe_floor_is_probed(),
                pose.end,
                clamp_ankle,
                clamp_weight,
                dt,
                settings.foot_max_rate,
            );
            foot_rots[index] = rot_state;
            toe_clamps[index] = clamp_state;
            pose.foot_rot = foot_rot;

            // 前掌骨（球窝/跖趾关节）：脚掌绕前掌转过去 `lift_angle`，前掌骨要是
            // 跟着走，脚趾就钻进地里（画面上一整块平板脚跟着翘）。这里把它按
            // **世界朝向**反向兜回来同样一个角（最多 `TOE_BEND_MAX`），脚趾就留在
            // 地上、在球窝处折起来。用的是定稿后的 `pose.foot_rot`，所以低通和
            // 不穿地夹紧带来的偏差也一起算进去
            if lift_angle > 0.5
                && let Some(toe_rot0) = pose.toe_rot
                && let Some(leg) = rig.legs.get(index)
                && let Some(toe) = leg.toe
            {
                // 折满 `TOE_BEND_MAX` 之后才跟着脚掌走：世界朝向 = 动画 + 剩下的角
                let residual = (lift_angle - TOE_BEND_MAX).max(0.0).to_radians();
                let toe_world = Quat::from_axis_angle(lift_axis, residual) * toe_rot0;
                if let Ok(mut transform) = transforms.get_mut(toe) {
                    let local = (pose.foot_rot.inverse() * toe_world).normalize();
                    toe_bend[index] = local.angle_between(Quat::IDENTITY).to_degrees();
                    transform.rotation = local;
                }
            }

            if weight <= 0.0 {
                // 权重为 0：位置全交给动画，但脚掌朝向要写回去（低通/淡出还在跑）。
                // 极点已经在上面跟着动画走过了
                if let Some(leg) = rig.legs.get(index) {
                    if let Ok(mut transform) = transforms.get_mut(leg.foot) {
                        transform.rotation = (pose.lower_rot.inverse() * pose.foot_rot).normalize();
                    }
                }
                // 离地：这一段量不到滑步（脚本来就该走），下一段支撑重新起量
                measure_slip(
                    &mut slip[index],
                    &mut slip_prev[index],
                    false,
                    pose.toe.unwrap_or(pose.end),
                    dt,
                );
                continue;
            }

            let Some(solved) = solve_leg(pose, target, pole, split, up) else {
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
            // 球 = 落下去的踝 + 当帧的「踝→前掌」：锁脚把锚点钉在前掌上时，
            // 这一个点就是画面上贴着地的那一点，它的速度就是肉眼看到的滑步
            let ball = pose
                .toe
                .map_or(target, |toe| target + (toe - pose.end));
            measure_slip(
                &mut slip[index],
                &mut slip_prev[index],
                locks[index].is_planted(),
                ball,
                dt,
            );
        }
    }
}

/// 量这一帧脚底接触点在地面上走了多远（m/s）。
///
/// 口径是**前掌（球）**这一个点：它是脚掌上压地的那一点，也是绕球转的支点。
/// 不用踝读数 —— 绕球转的时候踝本来就该走一段弧；也不用脚掌朝向的扫弧 ——
/// 球钉住、脚跟抬起来的时候，脚掌转是对的，不是滑。
///
/// 只在连续两帧都踩实时有意义（飞行期脚本来就该走），离地时清掉参照点。
fn measure_slip(slip: &mut f32, previous: &mut Option<Vec3>, planted: bool, ball: Vec3, dt: f32) {
    if !planted {
        *slip = 0.0;
        *previous = None;
        return;
    }
    *slip = previous.map_or(0.0, |last| (ball - last).length() / dt.max(1e-4));
    *previous = Some(ball);
}

/// 只解不写：按当前姿态把腿解一次，返回 `(解出来的膝盖, 实际到达的踝)`。
///
/// 抬跟与踝角读数都要它：锁脚把踝钉在原位之后，动画的膝位置不再是真实几何，
/// 拿动画膝去量小腿前倾会系统性偏小（面板看着够 90，画面上小腿已经压到 70）
fn probe_leg(pose: LegPose, target: Vec3, pole: Vec3) -> Option<(Vec3, Vec3)> {
    solve_two_bone(pose.root, pose.joint, pose.end, target, pole)
        .map(|solved| (solved.joint, solved.end))
}

/// 一条腿求解前的姿态快照（全部是世界空间 / 世界旋转）。
#[derive(Clone, Copy)]
struct LegPose {
    root: Vec3,
    joint: Vec3,
    end: Vec3,
    /// 前掌关节（`*ToeBase`）的世界位置；骨架里没有就是 `None`
    toe: Option<Vec3>,
    /// 前掌骨的**世界旋转**（动画给的那个）。抬跟要把脚掌转过去，前掌骨要么
    /// 跟着扎进地里，要么在球窝处折起来 —— 这里留一份原样当参照
    toe_rot: Option<Quat>,
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
///
/// `pole`：膝盖应当偏向的世界方向（已归一化的单位向量）。传低通后的值，
/// 膝盖就不会跟着动画的极点逐帧抖。
///
/// `roll`：股骨 / 胫骨各自绕**自己骨轴**再滚多少（弧度）。两段骨滚同一个量，
/// 膝两端的前面仍然对齐（不会在膝上掰出折角），但整条腿的朝向往脚那边偏
/// 一个 `roll` —— 踝里剩下的扭转因此少这么多。脚的世界朝向、踝的位置都不变：
/// 它只是把「拧在踝上的那段」提前在腿上转掉
fn solve_leg(pose: LegPose, target: Vec3, pole: Vec3, roll: f32, up: Vec3) -> Option<LegSolution> {
    let solution = solve_two_bone(
        pose.root, pose.joint, pose.end, target,
        // 极点方向；退化时求解器会自己退回当前膝盖的偏移方向
        pole,
    )?;

    // 骨轴是「髋→膝」「膝→踝」，指向地面；而 `roll` 的口径和脚掌那个刚体旋转
    // 一样（绕地面法线、正方向按右手定则）。所以先把骨轴翻到「指上」再转，
    // 不然滚的是反方向 —— 踝里的扭转会翻倍
    let roll_upper = Quat::from_axis_angle(upward(solution.upper_dir, up), roll);
    let roll_lower = Quat::from_axis_angle(upward(solution.lower_dir, up), roll);
    let upper_world = roll_upper
        * aim_world_rotation(pose.upper_rot, pose.joint - pose.root, solution.upper_dir);
    let lower_world = roll_lower
        * aim_world_rotation(pose.lower_rot, pose.end - pose.joint, solution.lower_dir);

    Some(LegSolution {
        upper_local: (pose.parent_rot.inverse() * upper_world).normalize(),
        // 下段的父节点是已经转过的上段：只能用新的世界旋转换算
        lower_local: (upper_world.inverse() * lower_world).normalize(),
        foot_local: (lower_world.inverse() * pose.foot_rot).normalize(),
    })
}

/// 把一根骨轴翻成「朝上」的口径：`roll` 的正方向统一按「绕地面法线、右手定则」，
/// 而骨轴（髋→膝、膝→踝）是朝下的。不翻过来，股骨/胫骨就滚反了
fn upward(axis: Vec3, up: Vec3) -> Vec3 {
    if axis.dot(up) < 0.0 { -axis } else { axis }
}

/// 腿骨自旋**接上**的速率（度/秒）：踩实那一刻脚已经在地上，可以快一点。
/// 100°/s ≈ 1.7°/帧，18° 的量摊满 0.18s
const LEG_ROLL_RATE: f32 = 100.0;

/// 腿骨自旋**放开**的速率（度/秒）：脚离地之后才放的这一段。
///
/// 比接上慢得多（18° 要 0.23s）：抬腿那一帧如果把 18° 一口气退回 0，股骨和
/// 胫骨会在一帧里反向滚 18°，膝盖就像被一只看不见的手拧了一下 —— 用户报的
/// 「抬腿时膝盖严重外翻」就是它。摊开放，动画本来就在摆动期自己做旋转
const LEG_ROLL_RELEASE: f32 = 80.0;

/// 腿骨自旋的跨帧推进：踩实时向目标靠近，离地后一帧一帧退回 0。
///
/// 两个速率分开而不是共用一个：接上要跟得上支撑期开头（不然整个支撑期都在
/// 慢慢追），放开要慢（不然抬腿那一帧膝盖反向滚一下）
fn advance_leg_roll(current: f32, target: f32, planted: bool, dt: f32) -> f32 {
    let rate = if planted {
        LEG_ROLL_RATE
    } else {
        LEG_ROLL_RELEASE
    };
    let step = rate.to_radians() * dt.max(0.0);
    let delta = (target - current).clamp(-step, step);
    current + delta
}

/// 一段偏航里分给腿骨（自旋）多少弧度。
///
/// 按比例分再夹上限：小角度（正常走路里的几度）按比例吃掉一部分，看着就是腿
/// 轻轻跟着转；大角度（支撑期里转 25°+）夹在上限，剩下的留在踝里。
/// 夹上限而不是「全给腿」：腿真的滚到 20° 以上，脚掌相对地面就明显歪了
fn split_leg_roll(twist: f32, share: f32, max: f32) -> f32 {
    if !twist.is_finite() {
        return 0.0;
    }
    (twist * share.clamp(0.0, 1.0)).clamp(-max.abs(), max.abs())
}

/// 极点：动画的膝盖朝向 + 脚掌指向，按 `ramp`（偏航锁的强度）混合。
///
/// 走路/站立时两者本来就差不多（膝盖朝脚尖）；转身时它们差的就是那段偏航，
/// 这时以脚为准 —— 膝盖留在脚的矢状面里，不会横向掰开
fn knee_pole(animated: Vec3, foot_forward: Vec3, ramp: f32) -> Vec3 {
    let ramp = ramp.clamp(0.0, 1.0);
    if foot_forward.length_squared() <= 1e-6 {
        return animated;
    }
    animated.lerp(foot_forward.normalize(), ramp).normalize_or_zero()
}

/// 膝盖朝向的低通：`previous` / `animated` 都是世界空间的单位向量，
/// `secs = 0` 表示完全跟随动画。
///
/// 为什么要滤：混合空间里两条剪辑的膝盖朝向差几度，权重一动，动画的极点就逐帧
/// 在两个方向间来回跳；直接拿来当 `pole_hint`，膝盖会跟着每秒晃好几次 ——
/// 观感就是支撑腿「顿」。极点本身是慢变量，一阶指数平滑抹掉帧间噪声就够了，
/// 不需要二阶：那样反而会在转换时多一个「过冲」。
fn smooth_pole(previous: Option<Vec3>, animated: Vec3, dt: f32, secs: f32) -> Option<Vec3> {
    if animated.length_squared() <= 1e-6 {
        return None;
    }
    let animated = animated.normalize();
    let follow = if secs <= f32::MIN_POSITIVE {
        1.0
    } else {
        1.0 - (-dt.max(0.0) / secs).exp()
    };
    let smoothed = match previous {
        Some(previous) if previous.length_squared() > 1e-6 => {
            previous.normalize().lerp(animated, follow.clamp(0.0, 1.0))
        }
        _ => animated,
    };
    let smoothed = smoothed.normalize_or_zero();
    if smoothed.length_squared() > 0.5 {
        Some(smoothed)
    } else {
        Some(animated)
    }
}

/// 前掌夹紧的最大角度（弧度，约 15°）。前掌按 16cm 算，15° 能把前掌抬 4cm，
/// 足够盖住动画的正常过冲；再大就不是「修瑕疵」而是「脚掌被掰变形」了
const TOE_CLAMP_MAX: f32 = 0.26;

/// 抬跟一次最多补多少度（安全阀）。目标角本身才是上限，这个只防动画姿态异常时
/// 把脚掰成钩子；正常「动画俯 20°、目标 55°」只会补 35°
/// 前掌骨（球窝/跖趾关节）能反向折多少度。抬跟时脚掌绕前掌转过去，脚趾留在
/// 地上不动，靠的就是这个关节；超过它就跟着脚掌走（人踮到极限时也是这样）
const TOE_BEND_MAX: f32 = 60.0;

/// 单次调用（一帧内）能补的俯角上限。它只是安全网：真正的限速是
/// `ANKLE_FLOOR_RATE`（跨帧），口径是「脚掌面」，脚本来就背屈的 rig 需要的
/// 补角可以超过 60°
const HEEL_OFF_LIFT_MAX: f32 = 90.0;

/// 「踝角地板」的软起区间（度）：差得少时补得缓一点（只是数值上的平滑，
/// 「踝角地板」每帧最多补多少度（按每秒给）。它低于脚掌旋转限速
/// （`foot_max_rate` 默认 360°/s），脚掌的实际旋转才跟得上踝的位移，前掌不会被带跑
const ANKLE_FLOOR_RATE: f32 = 240.0;

/// 抬跟在脚离地后收回的时长（秒）。
///
/// **不能**跟着锁脚的 `release_secs`（0.26s）走：那是给「位置残差」还债用的，
/// 形状是两端速度为 0 的慢放。用它收俯角，脚离地后还会被拽在原地、一边慢慢放平
/// —— 观感正是「被身体带着走」。0.10s 只盖住三四帧，蹬完那一下还留在脚上，
/// 然后立刻交回动画去做摆动期的背屈
const LIFT_RELEASE_SECS: f32 = 0.10;

/// 脚离地后，压着的偏航补偿按这个时间常数（秒）指数放掉。
///
/// 用指数而不是匀速：偏航补偿是「脚掌朝向」这把锁，离地后动画马上要做摆动期的
/// 外旋/内旋，匀速还债在结尾会留一个折角；指数衰减前段快、后段几乎贴住 0，
/// 且跨帧连续 —— 抬脚那一帧不会看到脚掌「弹」一下。
const YAW_RELEASE_SECS: f32 = 0.10;

/// 绕前掌转时抬跟进度的下限（0..1）。
///
/// 刻度同 [`crab_feast_library::ik::foot_lock::ROLL_SPAN`]：0.3 是承重点刚滑到
/// 前掌、开始掰脚掌，再往上才真的把脚跟抬离地面。取 0.45：脚跟抬离一两厘米、
/// 脚掌俯 10~15° —— 刚好够「绕球转」看起来是在**转**，又不像在蹬地
const HEEL_PIVOT_FLOOR_MAX: f32 = 0.45;

/// 单只脚的偏航锁：踩实期间让脚掌的**世界朝向**不跟着身体的偏航转。
///
/// 锁脚（[`FootLock`]）只管位置，脚掌旋转一直原样跟着动画的 hips 走 ——
/// 抬起一只脚原地转身时，着地那只脚就在地上原地拧。这里把「踩实那一刻的脚掌
/// 世界朝向」记成基准，每帧反向补偿；补偿量夹在 `foot_yaw_limit` 内，
/// 超出之后脚掌跟着身体撇（真人靠髋外旋也就能撑住这个量级）。
#[derive(Clone, Copy, Default, Debug)]
struct FootYawLock {
    /// 踩实时锁定的脚掌世界朝向（弧度，见 [`foot_yaw`]）；`None` = 没有基准
    reference: Option<f32>,
    /// 本帧施加的补偿角（弧度，绕地面法线；正 = 让脚掌逆时针回去）
    applied: f32,
    /// 上一帧量到的脚掌朝向：用来认出「脚掌被动画掰到失控、方位角翻过去」的读数
    last_yaw: Option<f32>,
    /// 这一段支撑里本体一共转了多少（弧度，自踩实起，正负号同 `applied` 的相反数）：
    /// 「脚掌本来被身体带着要转多少」
    want: f32,
    /// 其中**超出踝关节活动范围、改由绕前掌转**的那一段（弧度，自踩实起）。
    /// 它是绝对量而不是增量：踩实那帧为 0，随本体转起来单调长，抬脚清零
    ball: f32,
}

/// 一帧之内脚掌朝向跳多少度就算「量错了」。真转身 90°/s 只有 1.5°/帧，
/// 脚掌接近竖直时踝→前掌的水平分量会翻到脚踝后面，读数直接错 180°
const YAW_JUMP_MAX: f32 = 0.5;

impl FootYawLock {
    /// 推进一帧，返回本帧要施加的补偿角（弧度）。
    ///
    /// `yaw_now`：动画给的（未补偿的）脚掌朝向；`planted`：是否在支撑期；
    /// `replanted`：本帧是不是一次**新的**踩实（离地够久、锚点重建的那一帧）。
    fn update(
        &mut self,
        yaw_now: Option<f32>,
        planted: bool,
        replanted: bool,
        limit_deg: f32,
        enter_deg: f32,
        dt: f32,
    ) -> f32 {
        let limit = limit_deg.max(0.0).to_radians();
        if !planted {
            // 离地：基准作废（下一段支撑重新记），压着的补偿指数放掉
            self.reference = None;
            self.last_yaw = None;
            self.want = 0.0;
            self.ball = 0.0;
            self.applied *= (-dt.max(0.0) / YAW_RELEASE_SECS).exp();
            if self.applied.abs() < 1e-4 {
                self.applied = 0.0;
            }
            return self.applied;
        }

        // 量不到朝向（骨架没有前掌骨 / 脚掌快竖直）：**保持现状**。
        // 不能归零 —— 那等于一帧把脚掌拧回去，比不锁还难看
        let Some(yaw_now) = yaw_now.filter(|yaw| yaw.is_finite()) else {
            return self.applied;
        };

        if replanted || self.reference.is_none() {
            // 新的一段支撑：把「此刻脚掌实际所在的朝向」记成基准。
            // 不能直接记 `yaw_now`：上一段支撑的补偿在离地后还没放完（按
            // `YAW_RELEASE_SECS` 衰减），一帧抹掉的话落地那一下脚掌会跳
            self.reference = Some(wrap_pi(yaw_now + self.applied));
        } else if let Some(last) = self.last_yaw
            && wrap_pi(yaw_now - last).abs() > YAW_JUMP_MAX
        {
            // 一帧跳了几十度：只可能是方位角翻过去了（脚掌被动画掰到竖直以上），
            // 这个读数作废，保持上一帧的补偿
            return self.applied;
        }
        self.last_yaw = Some(yaw_now);

        let want = self
            .reference
            .map_or(0.0, |reference| wrap_pi(reference - yaw_now));
        self.want = want;

        // 分工：踝自己拧 `enter`，剩下的绕前掌转。
        //
        // - `excess` = 超出踝活动范围的那段角 → 交给「绕球」；
        // - 绕球也不是无限的：踝到前掌只有十几厘米，转太多踝得甩出去半条腿。
        //   上限是 `limit - enter`（也就是「脚掌世界朝向还允许被带偏」的余量），
        //   再多的部分只能让脚掌在地上滑 —— 和旧行为一致，且只在 30° 以上的
        //   大角度上才会碰到（那种角度本来就得抬脚重踩，不是靠拧能解决的）
        let enter = enter_deg.max(0.0).to_radians().min(limit);
        let span = (limit - enter).max(0.0);
        self.ball = (want - want.clamp(-enter, enter)).clamp(-span, span);
        // 施加到脚掌上的补偿 = 脚掌要「被按住」的那部分
        self.applied = if limit > 0.0 {
            (want - self.ball).clamp(-limit, limit)
        } else {
            0.0
        };
        if limit <= 0.0 {
            self.ball = 0.0;
        }
        self.applied
    }

    /// 本段支撑里绕前掌转的角度（弧度，自踩实起；正负号同 [`Self::applied`] 相反）
    fn ball(&self) -> f32 {
        self.ball
    }
}

/// 脚掌在世界水平面内的朝向（弧度，与 `input::yaw_of` 同口径：`atan2(x, z)`）。
///
/// 只取 `踝 → 前掌` 这根骨头的水平分量：抬跟时它会被俯下去，但水平方位角不变，
/// 所以整个支撑期都量得到，不需要另外定义「脚掌正前方」这个轴。
///
/// 脚掌越接近竖直，水平分量越小、方位角越不可信（抬跟到 76° 时只剩 1/4），
/// 超过 60° 就直接判为量不到 —— 再往后这根向量会翻到踝后面，读数错 180°
fn foot_yaw(ankle: Vec3, toe: Vec3) -> Option<f32> {
    let full = toe - ankle;
    let flat = Vec3::new(full.x, 0.0, full.z);
    (flat.length_squared() >= 0.25 * full.length_squared()).then(|| flat.x.atan2(flat.z))
}

/// 折算出来的「脚掌相对地面的俯角」上限（度）。
///
/// 脚掌转到接近竖直时，绕前掌的转轴（Y × 脚掌水平朝向）会退化，
/// `heel_off_lift` 也会在 ~78° 处退出。目标最多到这里，免得算出个掰不动的角度
const HEEL_OFF_PITCH_MAX: f32 = 76.0;

/// 「前掌不穿地」那部分修正收回的时间常数（秒）。脚一离地这十几度就要赶紧收干净，
/// 所以它**不**跟着 [`FootIkSettings::foot_max_rate`] 走。
const TOE_RELEASE_SECS: f32 = 0.05;

/// 落地过渡进度：踩实后从 0 线性爬到 1，只增不减。
///
/// 只在**落地上升沿**（上一帧还没踩实、这一帧踩实）重置。
/// 支撑期里 `planted` 会随咬合迟滞在两帧之间抖，如果一抖就清零，
/// 脚掌会反复从旧姿态重新爬一遍 —— 观感就是抖动的残影。
/// 判定「真的离过地」的最短时长（秒）。支撑期里 `planted` 会随咬合迟滞抖一两帧，
/// 那么短的空档不算抬脚，不许把进度清零。
const TOUCHDOWN_RESTART_SECS: f32 = 0.05;

fn advance_landing(
    current: f32,
    planted_now: bool,
    air_secs: f32,
    dt: f32,
    secs: f32,
) -> (f32, f32) {
    if !planted_now {
        // 离地：进度保持不变，只累计空档时长
        return (current, air_secs + dt.max(0.0));
    }
    let from = if air_secs >= TOUCHDOWN_RESTART_SECS {
        0.0
    } else {
        current
    };
    ((from + dt.max(0.0) / secs.max(1e-4)).min(1.0), 0.0)
}

/// **踝角**（脚相对小腿的局部旋转）的限速：一帧最多转 `max_rate_deg_s * dt` 度。
///
/// 用限速而不是低通，是因为两者要的东西不一样：
/// - 低通对任何速度都有**稳态滞后**（滞后 ≈ 速度 × 时间常数）。摆动期踝角本身就有
///   200~300°/s 的跖屈/背屈，滤了以后脚掌会一直「拖在后面」，而时间常数越大越拖；
/// - 限速只削**超过阈值的**那部分：平滑的动画一帧转 5° 就当帧放行（零滞后），
///   只有单帧跳 20° 的突变会被摊成几帧。
///
/// 所以调这个参数的方向也和低通相反：「越平滑」= 阈值越小，但不会引入拖尾。
fn limit_ankle_rotation(
    previous: Option<Quat>,
    animated: Quat,
    dt: f32,
    max_rate_deg_s: f32,
) -> Option<Quat> {
    if !animated.is_finite() {
        return None;
    }
    let animated = animated.normalize();
    let Some(previous) = previous.filter(|previous| previous.is_finite()) else {
        return Some(animated);
    };
    let previous = previous.normalize();
    if !(max_rate_deg_s > 0.0) {
        return Some(animated);
    }
    let delta = previous.angle_between(animated);
    let limit = max_rate_deg_s.to_radians() * dt.max(0.0);
    if limit >= delta || delta <= 1e-6 {
        return Some(animated);
    }
    let rot = previous.slerp(animated, (limit / delta).clamp(0.0, 1.0));
    rot.is_finite().then_some(rot.normalize())
}

/// 一条腿这一帧的脚掌朝向：先按**踝角**限速，再把「前掌不穿地」的修正量
/// 叠加成一个有记忆的偏移。返回 `(最终世界朝向, 新踝角缓存, 新夹紧缓存)`。
///
/// 这个函数必须对**每一帧、每一条腿**都调用，包括锁脚权重为 0 的摆动期：
/// 抬脚后 weight 就是 0，如果只在求解腿的那条分支里做，限速和夹紧收尾等于没装。
#[allow(clippy::too_many_arguments)]
fn resolve_foot_orientation(
    previous: Option<Quat>,
    previous_clamp: Option<(Vec3, f32)>,
    animated_rot: Quat,
    lower_rot: Quat,
    planted: bool,
    ik_active: bool,
    landing: f32,
    landing_from: Option<Quat>,
    toe_floor: Option<f32>,
    toe: Option<Vec3>,
    toe_drop: f32,
    floor_probed: bool,
    ankle: Vec3,
    target: Vec3,
    clamp_weight: f32,
    dt: f32,
    max_rate: f32,
) -> (Quat, Option<Quat>, Option<(Vec3, f32)>) {
    // 1) 基准姿态：支撑期精确跟随动画（脚在地上，朝向就是支点），摆动期走限速。
    // 送还动画的这段（`planted` 已经落下、但锁脚强度还没收干净）也必须精确跟随：
    // 抬跟的抬角已经按强度淡出了，姿态本身是连续的，再叠一层限速只会让脚掌比
    // 踝慢半拍 —— 前掌绕着踝往下扫，一帧能扫进地里 2~3cm。限速只负责摆动期
    // 里动画自己的跳变
    let animated_local = (lower_rot.inverse() * animated_rot).normalize();
    let base_local = if planted || ik_active {
        animated_local
    } else {
        limit_ankle_rotation(previous, animated_local, dt, max_rate).unwrap_or(animated_local)
    };
    let base_world = (lower_rot * base_local).normalize();

    // 2) 前掌不穿地：必须基于**基准姿态**算，不能基于这一帧的过渡值。
    // 否则落地那几帧脚掌还在空中的跖屈姿态，算出来需求超上限被夹死，
    // 等姿态回平需求又变小 —— 观感就是「放平了又反弹」
    let wanted = match (toe_floor, toe) {
        (Some(floor_y), Some(toe)) => {
            let offset = animated_rot.inverse() * (toe - ankle);
            toe_floor_correction(base_world, offset, target, floor_y, toe_drop, floor_probed)
                .map(|(axis, angle)| (axis, angle * clamp_weight))
        }
        _ => None,
    };
    // 需要更多立刻加（这一项就是「把前掌抬到地面上」，摊开就等于穿地），
    // 需要收回才淡出（收回时多留一点只是脚掌略翘，比来回翻要好）
    let (previous_axis, previous_angle) = previous_clamp.unwrap_or((Vec3::Y, 0.0));
    let (axis, wanted_angle) = wanted.unwrap_or((previous_axis, 0.0));
    // 淡出**只给离地用**。脚在地上时需求是确定的物理量（动画姿态 + 地面几何），
    // 它怎么变就该怎么跟：任何人为的滞后都会留下一个「先翘着、几帧后放平」的
    // 反曲尾巴。离地时才需要淡出，否则两个 `return None` 出口正好落在抬脚那一瞬，
    // 夹紧量会一帧归零，脚掌「闪一下」
    let angle = if wanted_angle.abs() >= previous_angle.abs() || planted {
        wanted_angle
    } else {
        let follow = 1.0 - (-dt.max(0.0) / TOE_RELEASE_SECS).exp();
        previous_angle + (wanted_angle - previous_angle) * follow.clamp(0.0, 1.0)
    };

    // 3) 落地过渡只作用在「动画朝向」上，夹紧**全量立即生效**：
    // 夹紧就是「把前掌按到地面上」这件事本身，摊开就等于落地后前掌还沉在地下。
    // 拆开以后，落地首帧脚掌就已经贴地，只是踝角动画还在从空中姿态往上爬 ——
    // 平、且不会先抬过头再放回来
    // 起点是**落地那一刻**的姿态快照，不是「上一帧的输出」：
    // 逐帧递归地 slerp(previous, base, landing) 是指数逼近，收敛速度由 landing
    // 的步长决定，落地后动画本身在动的时候会一直追不上，等 landing 到 1 再猛然对齐
    // —— 那点残余的反曲就是这么来的。快照 + 定比例插值是线性收敛，到点即对齐
    let local = if planted && landing < 1.0 {
        let from = landing_from
            .or(previous)
            .filter(|from| from.is_finite())
            .map(|from| from.normalize());
        match from {
            Some(from) => from.slerp(base_local, landing.clamp(0.0, 1.0)),
            None => base_local,
        }
    } else {
        base_local
    };
    let smooth_world = (lower_rot * local).normalize();
    let rot = if angle.abs() > 1e-4 {
        (Quat::from_axis_angle(axis, angle) * smooth_world).normalize()
    } else {
        smooth_world
    };
    (rot, Some(local), Some((axis, angle)))
}

/// 把脚的世界朝向夹到「前掌不低于 `floor_y`」。
///
/// 只在绕踝的竖直平面里转：轴 = 水平面内与脚掌方向垂直的方向，所以前掌的水平方位
/// 不变，只有俯仰被抬起。已经在地面之上、脚掌几乎竖直（`offset` 水平分量太小）或者
/// 抬到顶也够不着 `floor_y` 时原样返回。
fn toe_floor_correction(
    foot_rot: Quat,
    toe_offset: Vec3,
    ankle: Vec3,
    floor_y: f32,
    toe_drop: f32,
    floor_probed: bool,
) -> Option<(Vec3, f32)> {
    // 推断出来的地面可能比「踝高 − 脚掌水平时的下沉量」还高。更高说明它不可信
    // （从高处落下时落地帧还在空中、或者那一帧脚掌还跖屈着），这时候硬掰会把
    // 脚掌往上折到极限，网格都跟着变形 —— 宁可让它穿一点地，也不能折。
    // 射线量到的真实地面（`floor_probed`）不做这个检查：那时地面比推算值高
    // 只说明脚已经穿进地里，正是该夹的时候
    if !floor_probed && toe_drop > 1e-4 && floor_y > ankle.y - toe_drop + 1e-3 {
        return None;
    }
    let v = foot_rot * toe_offset;
    let toe_y = ankle.y + v.y;
    if toe_y >= floor_y {
        return None;
    }
    let horizontal = Vec2::new(v.x, v.z).length();
    let length = v.length();
    if horizontal < 1e-4 || length < 1e-4 {
        return None;
    }
    let wanted = floor_y - ankle.y;
    if wanted >= length {
        // 脚掌再抬也够不着地面：这不是朝向能修的（交给位置 IK）
        return None;
    }
    // 在 (水平, 竖直) 平面里把角度从当前值转到「竖直分量 = wanted」
    let from = v.y.atan2(horizontal);
    let to = (wanted / length).clamp(-1.0, 1.0).asin();
    let axis = Vec3::new(-v.z, 0.0, v.x).normalize();
    // 硬上限：夹紧是修瑕疵的，不是主支点。超过这个角度说明几何本来就对不上，
    // 继续掰只会让脚掌翘得更明显
    Some((axis, (to - from).clamp(-TOE_CLAMP_MAX, TOE_CLAMP_MAX)))
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

/// `smoothstep`：0..1 单调、两端导数为 0，起跟收跟都不会有折角
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 脚掌俯角（度）：脚跟抬起为正 —— 前掌在踝之下越多，角度越大
fn foot_pitch_deg(toe: Vec3, ankle: Vec3) -> f32 {
    let foot = toe - ankle;
    let horizontal = Vec2::new(foot.x, foot.z).length();
    (-foot.y).atan2(horizontal.max(1e-4)).to_degrees()
}

/// 小腿的前倾角（度）：`knee − ankle` 在「水平 · 脚掌朝向」方向上的分量，
/// 膝在踝前方为正。锚定在脚掌朝向而不是世界 +Z，转身、斜向站位时口径才对得上
fn shank_lean_deg(knee: Vec3, ankle: Vec3, toe: Vec3) -> f32 {
    let shank = knee - ankle;
    let forward = Vec3::new(toe.x - ankle.x, 0.0, toe.z - ankle.z).normalize_or_zero();
    if forward.length_squared() < 0.5 || !shank.is_finite() {
        return 0.0;
    }
    // 前倾角 = 小腿在「上」与「前」两个方向上的分量之比
    shank.dot(forward).atan2(shank.y).to_degrees()
}

/// 抬跟：绕前掌把脚掌俯仰补到 `pitch_target`，返回 `(世界旋转, 踝的位移, 抬完的俯角)`。
///
/// - 轴取水平面内垂直于脚掌朝向的方向，所以只有俯仰变、前掌的水平方位不变；
/// - **绕前掌转**：踝的位移与旋转是同一件事的两个面（`lifted = toe - rot * (toe - ankle)`），
///   两边一起用，前掌就钉在原地不动；
/// - `k` 是本节拍的进度（0..1）：0 不动、1 到目标；动画自己已经俯得比目标更大时
///   不再往回掰 —— 这里补的是「没蹬够」，不削真实的蹬地。
/// - `pitch_target_deg` 与返回值都是**脚掌面**口径（脚掌平放 = 0）；`foot_tilt_deg`
///   是「踝→前掌」这条骨向量在静止姿态下的俯角 —— 骨向量口径 = 面口径 + 它
fn heel_off_lift(
    toe: Vec3,
    ankle: Vec3,
    pitch_target_deg: f32,
    foot_tilt_deg: f32,
    k: f32,
) -> Option<(Quat, Vec3, f32)> {
    // 目标可以是负的（脚掌面朝「背屈」那一侧也算合法姿态），只挡非有限值
    if !pitch_target_deg.is_finite() || k <= 1e-4 {
        return None;
    }
    let foot = toe - ankle;
    if !foot.is_finite() || foot.length_squared() <= 1e-6 {
        return None;
    }
    // 轴取水平面内垂直于脚掌朝向的方向：脚掌近乎竖直（脚尖朝下）时轴退化，
    // 那种姿态也谈不上「脚跟再抬」
    let horizontal_len = Vec2::new(foot.x, foot.z).length();
    if horizontal_len < foot.length() * 0.2 {
        return None;
    }
    let horizontal = Vec3::new(foot.x, 0.0, foot.z) / horizontal_len;
    let pitch_now = foot_pitch_deg(toe, ankle) - foot_tilt_deg;
    // 差多少补多少，但单次最多补 `HEEL_OFF_LIFT_MAX`
    let extra = (pitch_target_deg - pitch_now).clamp(0.0, HEEL_OFF_LIFT_MAX) * k.clamp(0.0, 1.0);
    if extra <= 1e-3 {
        return None;
    }
    // 正角 = 脚尖往下（脚跟往上）。轴 = 上 × 脚掌水平朝向
    let axis = Vec3::Y.cross(horizontal).normalize_or_zero();
    let rot = Quat::from_axis_angle(axis, extra.to_radians());
    let lifted = toe - rot * foot;
    Some((rot, lifted - ankle, pitch_now + extra))
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
    transforms: &Query<&Transform>,
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
        toe_drop: 0.0,
        foot_tilt: 0.0,
    }; 2];
    // 骨骼尺寸：腿长（髋→膝 + 膝→踝）与脚长（踝→前掌）。加起来取平均，
    // 单侧骨骼缺失时另一侧照样能定出比例
    let mut leg_sum = 0.0f32;
    let mut leg_count = 0u32;
    let mut foot_sum = 0.0f32;
    let mut foot_count = 0u32;
    for (index, side) in ["Left", "Right"].into_iter().enumerate() {
        let upper = find_bone(model, children, names, &format!("{side}UpLeg"))?;
        let lower = find_bone(model, children, names, &format!("{side}Leg"))?;
        let foot = find_bone(model, children, names, &format!("{side}Foot"))?;
        // 前掌缺失不影响踝部锁脚（承重点就滑不到前掌而已），所以是 Option
        let toe = find_bone(model, children, names, &format!("{side}ToeBase"));
        // 踝到前掌的竖直距离：动画只改旋转，这个局部偏移是骨架常量，
        // 随时读都稳定
        let toe_drop = toe
            .and_then(|entity| transforms.get(entity).ok())
            .map(|transform| (-transform.translation.y).max(0.0))
            .unwrap_or(0.0);
        // 踝角口径的标定：`foot_pitch_deg` 量的是踝→前掌骨这条向量，脚掌平放时
        // 它并不是 0 —— 前掌关节在踝下方 6~10cm，这根骨头本身就俯了 26~62°。
        // 不减掉它，「踝角 = 90 + P − L」就每根 rig 多算一个常数，
        // 地板写 90 实际只压到 55（Swat 这种脚骨更陡的 rig 只到 34）
        let foot_tilt = toe
            .and_then(|toe| {
                let foot_pos = world_position(model, foot, parents, transforms)?;
                let toe_pos = world_position(model, toe, parents, transforms)?;
                Some(foot_pitch_deg(toe_pos, foot_pos))
            })
            .filter(|tilt| tilt.is_finite() && (-5.0..85.0).contains(tilt))
            .unwrap_or(0.0);
        // 骨骼长度：关节位置是骨架常量，静止姿态量一次就够（分段相加，
        // 这样绑定姿态本身是屈膝也不会量短）
        if let (Some(hip), Some(knee), Some(ankle)) = (
            world_position(model, upper, parents, transforms),
            world_position(model, lower, parents, transforms),
            world_position(model, foot, parents, transforms),
        ) {
            let length = (knee - hip).length() + (ankle - knee).length();
            if length.is_finite() && length > 1e-3 {
                leg_sum += length;
                leg_count += 1;
            }
        }
        if let Some(ball) = toe.and_then(|toe| world_position(model, toe, parents, transforms))
            && let Some(ankle) = world_position(model, foot, parents, transforms)
        {
            let length = (ball - ankle).length();
            if length.is_finite() && length > 1e-3 {
                foot_sum += length;
                foot_count += 1;
            }
        }
        legs[index] = Leg {
            upper,
            lower,
            foot,
            toe,
            toe_drop,
            foot_tilt,
        };
    }

    let scale_leg = ratio(leg_sum, leg_count, REFERENCE_LEG_LENGTH);
    let scale_foot = ratio(foot_sum, foot_count, REFERENCE_FOOT_LENGTH);
    debug!(
        "骨骼尺寸：腿长 {:.3} m（×{scale_leg:.2}）、踝→前掌 {:.3} m（×{scale_foot:.2}）",
        leg_sum / leg_count.max(1) as f32,
        foot_sum / foot_count.max(1) as f32
    );

    Some(FootRig {
        spine,
        legs,
        scale_leg,
        scale_foot,
        // 这一帧的姿势是「骨架刚能走到这里」的姿势，不一定是脚掌平放：
        // 交给 `calibrate_at_rest` 在第一次静止帧上覆盖
        tilt_calibrated: false,
    })
}

/// 量到的尺寸 / 参考尺寸，夹在 0.4~2.5；没量到（骨架缺骨骼）就退回 1（按参考骨架算）
fn ratio(sum: f32, count: u32, reference: f32) -> f32 {
    if count == 0 || !sum.is_finite() || reference <= 1e-6 {
        return 1.0;
    }
    (sum / count as f32 / reference).clamp(0.4, 2.5)
}

/// 从 `model` 往下累乘局部变换，得到某个骨骼的世界位置（只在挂载骨架时用一次）
fn world_position(
    model: Entity,
    target: Entity,
    parents: &Query<&ChildOf>,
    transforms: &Query<&Transform>,
) -> Option<Vec3> {
    let mut chain = vec![target];
    let mut cursor = target;
    while cursor != model {
        cursor = parents.get(cursor).ok()?.parent();
        chain.push(cursor);
    }
    let mut world = Affine3A::IDENTITY;
    for entity in chain.iter().rev() {
        let local = transforms
            .get(*entity)
            .copied()
            .unwrap_or(Transform::IDENTITY);
        world *= local.compute_affine();
    }
    Some(world.translation.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    fn straight_pose() -> LegPose {
        LegPose {
            toe: None,
            toe_rot: None,
            root: Vec3::new(0.0, 1.0, 0.0),
            joint: Vec3::new(0.0, 0.45, 0.2),
            end: Vec3::ZERO,
            parent_rot: Quat::IDENTITY,
            upper_rot: Quat::IDENTITY,
            lower_rot: Quat::IDENTITY,
            foot_rot: Quat::IDENTITY,
        }
    }

    /// 米制阈值必须跟着骨骼尺寸换算：同一个 `FootIkSettings` 交给 1.8 倍腿长的
    /// 角色时，「踝被拉多远、重心越过脚多远、脚掌抬多高才滑到前掌」都要按比例放大，
    /// 否则大个子会被拽得更远、小个子起跟更晚
    #[test]
    fn meter_thresholds_follow_the_skeleton_scale() {
        let settings = FootIkSettings {
            max_correction: 0.20,
            heel_off_lead: -0.10,
            toe_pivot_rise: 0.04,
            ..FootIkSettings::default()
        };
        let reference = settings.lock_config(1.0, 1.0);
        let big = settings.lock_config(1.8, 1.3);
        // 身体位移那一组按腿长
        assert!((big.max_correction - 0.36).abs() < 1e-5);
        assert!((big.heel_off_lead - (-0.18)).abs() < 1e-5);
        // 脚掌抬升那一项按脚长
        assert!((big.toe_pivot_rise - 0.052).abs() < 1e-5);
        // 参考骨架上原样
        assert!((reference.max_correction - 0.20).abs() < 1e-5);
        assert!((reference.heel_off_lead - (-0.10)).abs() < 1e-5);
        assert!((reference.toe_pivot_rise - 0.04).abs() < 1e-5);
        // 与尺寸无关的那几项不许被动
        assert!((big.plant_enter - reference.plant_enter).abs() < 1e-6);
        assert!((big.release_secs - reference.release_secs).abs() < 1e-6);
    }

    /// 把解出来的局部旋转重新乘回世界，确认脚落到目标上、骨长不变
    #[test]
    fn solved_leg_lands_on_target() {
        let pose = straight_pose();
        let target = Vec3::new(0.12, 0.0, 0.0);
        let solved = solve_leg(pose, target, pose.joint - pose.root, 0.0, Vec3::Y).expect("应当有解");

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

    /// 滑步读数是「前掌（球）真的在地上走」：钉住时必须是 0，
    /// 被拖着走时按 m/s 算出来。它是分辨「拉扯 / 滑步 / 动画不匹配」三路的第一把尺子
    #[test]
    fn slip_reads_zero_when_the_ball_is_pinned() {
        let mut slip = 0.0;
        let mut prev = None;
        let ball = Vec3::new(0.12, 0.0, 0.13);
        let dt = 1.0 / 60.0;
        for _ in 0..12 {
            measure_slip(&mut slip, &mut prev, true, ball, dt);
            assert!(slip < 1e-4, "钉住的脚报出 {slip:.4} m/s 的滑步");
        }
    }

    /// 每帧挪 1.67cm（= 1 m/s @60Hz）要读出 1 m/s
    #[test]
    fn slip_reads_the_actual_ground_speed() {
        let mut slip = 0.0;
        let mut prev = None;
        let dt = 1.0 / 60.0;
        let mut ball = Vec3::ZERO;
        measure_slip(&mut slip, &mut prev, true, ball, dt);
        for _ in 0..6 {
            ball.x += 0.016_667;
            measure_slip(&mut slip, &mut prev, true, ball, dt);
            assert!((slip - 1.0).abs() < 1e-3, "滑步读出 {slip:.3} m/s，期望 1.0");
        }
    }

    /// 踝绕球转、球不动：那不是滑步。读数只看球，不看踝
    #[test]
    fn slip_ignores_the_ankle_orbiting_the_ball() {
        let mut slip = 0.0;
        let mut prev = None;
        let dt = 1.0 / 60.0;
        let ball = Vec3::new(0.12, 0.0, 0.13);
        let ankle = Vec3::new(0.12, 0.0, 0.0);
        measure_slip(&mut slip, &mut prev, true, ball, dt);
        // 踝绕球转 16°，球留在原地
        let spun = ball + Quat::from_rotation_y(16f32.to_radians()) * (ankle - ball);
        assert!((spun - ankle).length() > 0.02, "踝本来就该走出一段弧");
        measure_slip(&mut slip, &mut prev, true, ball, dt);
        assert!(slip < 1e-4, "绕球转被判成滑步：{slip:.4} m/s");
    }

    /// 离地那一段不算滑步，而且要清掉参照点：下一段支撑的第一帧不能拿
    /// 上一段支撑的球位置去比（那会凭空多出一条腿长的滑步）
    #[test]
    fn slip_resets_when_the_foot_leaves_the_ground() {
        let mut slip = 0.0;
        let mut prev = None;
        let dt = 1.0 / 60.0;
        measure_slip(&mut slip, &mut prev, true, Vec3::ZERO, dt);
        measure_slip(&mut slip, &mut prev, false, Vec3::new(0.0, 0.3, 0.8), dt);
        assert_eq!(slip, 0.0);
        assert!(prev.is_none(), "离地没清参照点");
    }

    /// 腿骨自旋只拧腿、不动脚：脚的**世界朝向**必须一点不差地保住
    /// （那是偏航锁的全部意义），胫骨绕自己的骨轴滚了 `roll` 那么多
    #[test]
    fn leg_roll_twists_the_shin_without_touching_the_foot() {
        let pose = straight_pose();
        let pole = pose.joint - pose.root;
        let roll = 20f32.to_radians();
        let flat = solve_leg(pose, pose.end, pole, 0.0, Vec3::Y).expect("应当有解");
        let rolled = solve_leg(pose, pose.end, pole, roll, Vec3::Y).expect("应当有解");

        let upper_flat = pose.parent_rot * flat.upper_local;
        let lower_flat = upper_flat * flat.lower_local;
        let upper_rolled = pose.parent_rot * rolled.upper_local;
        let lower_rolled = upper_rolled * rolled.lower_local;

        // 胫骨的世界旋转多了一个「绕骨轴」的滚
        let delta = lower_rolled * lower_flat.inverse();
        assert!(
            (delta.angle_between(Quat::IDENTITY) - roll).abs() < 1e-3,
            "腿骨只滚了 {:.1}°，期望 {:.1}°",
            delta.angle_between(Quat::IDENTITY).to_degrees(),
            roll.to_degrees()
        );
        // 脚的世界朝向不变：锁住的是脚，不是腿
        let foot_flat = lower_flat * flat.foot_local;
        let foot_rolled = lower_rolled * rolled.foot_local;
        assert!(
            foot_rolled.angle_between(foot_flat) < 1e-4,
            "脚掌被腿骨自旋带走了 {:.1}°",
            foot_rolled.angle_between(foot_flat).to_degrees()
        );
    }

    /// 腿骨自旋的跨帧推进：踩实时接上，离地后**一帧一帧**退回，绝不一帧跳到 0。
    /// 「抬腿时膝盖被拧一下」就是从这里漏出去的（18° 一帧退回）
    #[test]
    fn leg_roll_releases_gradually_after_the_foot_lifts() {
        let dt = 1.0 / 60.0;
        let max = 18f32.to_radians();
        // 踩实：从 0 爬上 18° 大概要 0.18s（11 帧）
        let mut roll = 0.0;
        let mut frames = 0;
        while roll < max - 1e-4 && frames < 120 {
            roll = advance_leg_roll(roll, max, true, dt);
            frames += 1;
        }
        assert!(frames < 14, "接上花了 {frames} 帧，太慢");
        // 离地：一帧的变化必须远小于总量，而且总帧数够多（没人看得见的慢放）
        let mut previous = roll;
        let mut release = 0;
        while roll.abs() > 1e-4 && release < 600 {
            roll = advance_leg_roll(roll, 0.0, false, dt);
            assert!(
                (previous - roll).to_degrees() <= 80.0 / 60.0 + 1e-3,
                "离地那一帧退了 {:.2}°",
                (previous - roll).to_degrees()
            );
            previous = roll;
            release += 1;
        }
        assert!(release >= 12, "18° 只用 {release} 帧就退完了，膝盖会被拧");
        assert!(roll.abs() <= 1e-4, "没退回 0：{:.3}°", roll.to_degrees());
    }

    /// 自旋的**方向**要对：分给腿骨的那段必须把踝里的扭转减掉，而不是翻倍。
    /// 骨轴（膝→踝）朝下，直接拿它转是反的 —— 这个测试就是钉住 `upward` 那一下
    #[test]
    fn leg_roll_reduces_the_ankle_twist_not_doubles_it() {
        // 身体没动、脚掌被偏航锁拧了 +20°：踝里本来有 20° 的扭转
        let pose = LegPose {
            foot_rot: Quat::from_rotation_y(20f32.to_radians()),
            ..straight_pose()
        };
        let pole = pose.joint - pose.root;
        let flat = solve_leg(pose, pose.end, pole, 0.0, Vec3::Y).expect("应当有解");
        let rolled = solve_leg(
            pose,
            pose.end,
            pole,
            10f32.to_radians(),
            Vec3::Y,
        )
        .expect("应当有解");

        let flat_twist = flat.foot_local.angle_between(Quat::IDENTITY).to_degrees();
        let rolled_twist = rolled.foot_local.angle_between(Quat::IDENTITY).to_degrees();
        assert!(flat_twist > 18.0, "初始扭转只有 {flat_twist:.1}°，测试摆错了");
        assert!(
            rolled_twist < flat_twist - 7.0,
            "腿骨自旋把踝的扭转从 {flat_twist:.1}° 变成 {rolled_twist:.1}°（应该在减）"
        );
    }

    /// 踝里剩下的扭转被分给腿骨之后要掉到人能做到的范围：28.5° 的一段转身，
    /// 踝只留十来度（距下关节的量），其余交给髋内旋 + 胫骨自旋
    #[test]
    fn split_leg_roll_caps_the_ankle_twist() {
        let settings = FootIkSettings::default();
        let max = settings.leg_roll_max.to_radians();
        let lock = 28.5f32.to_radians();
        let split = split_leg_roll(lock, settings.leg_roll_share, max);
        let residual = (lock - split).to_degrees();
        assert!(
            residual > 8.0 && residual < 12.0,
            "踝里还剩 {residual:.1}°（28.5° 的一段转身）"
        );
        // 小角度按比例吃：走路的几度不该被夹上限全给腿
        let small = split_leg_roll(6f32.to_radians(), settings.leg_roll_share, max);
        assert!((small / 6f32.to_radians() - settings.leg_roll_share).abs() < 1e-4);
        // 反向也一样，而且不会越过上限
        assert!(split_leg_roll(-40f32.to_radians(), 0.65, max).abs() <= max + 1e-5);
        assert_eq!(split_leg_roll(0.0, 0.65, max), 0.0);
    }

    /// 脚钉在地上、身体转过去时，极点要以**脚**为准：膝盖留在脚的矢状面里，
    /// 不然髋转、踝不动，股骨胫骨只能在膝上横向掰开（画面上就是外翻）
    #[test]
    fn knee_pole_follows_the_foot_when_the_body_turns() {
        let body_turned = Vec3::new(30f32.to_radians().sin(), 0.0, 30f32.to_radians().cos());
        let foot_forward = Vec3::Z;
        // 没锁（走路/站立）：还是听动画的（膝盖朝脚尖，两者本来就差不多）
        let idle = knee_pole(body_turned, foot_forward, 0.0);
        assert!(idle.angle_between(body_turned) < 1e-4);
        // 锁满（身体转过一段）：极点翻到脚那边
        let locked = knee_pole(body_turned, foot_forward, 1.0);
        assert!(
            locked.angle_between(foot_forward) < 1e-4,
            "极点还跟着身体：{locked:?}"
        );
        // 半锁：在两者之间，而且是个单位向量
        let half = knee_pole(body_turned, foot_forward, 0.5);
        assert!((half.length() - 1.0).abs() < 1e-4);
        assert!(
            half.angle_between(body_turned).to_degrees() > 1.0
                && half.angle_between(foot_forward).to_degrees() > 1.0
        );
        // 没有前掌骨（`toe = None`）：退回动画的极点，不能变成零向量
        let no_toe = knee_pole(body_turned, Vec3::ZERO, 1.0);
        assert!(no_toe.angle_between(body_turned) < 1e-4);
    }

    /// 目标就是当前脚踝时，解应当退化成恒等（不该凭空给姿态加旋转）
    #[test]
    fn unchanged_target_is_a_noop() {
        let pose = straight_pose();
        let solved = solve_leg(pose, pose.end, pose.joint - pose.root, 0.0, Vec3::Y).expect("应当有解");
        assert!(solved.upper_local.angle_between(Quat::IDENTITY) < 1e-4);
        assert!(solved.lower_local.angle_between(Quat::IDENTITY) < 1e-4);
        assert!(solved.foot_local.angle_between(Quat::IDENTITY) < 1e-4);
    }

    /// 够不着的目标不会把腿拉断，脚停在最大伸展处
    #[test]
    fn unreachable_target_keeps_bone_lengths() {
        let pose = straight_pose();
        let target = Vec3::new(0.0, 0.0, 0.6);
        let solved = solve_leg(pose, target, pose.joint - pose.root, 0.0, Vec3::Y).expect("应当有解");

        let upper_world = pose.parent_rot * solved.upper_local;
        let lower_world = upper_world * solved.lower_local;
        let joint = pose.root + upper_world * (pose.joint - pose.root);
        let foot = joint + lower_world * (pose.end - pose.joint);

        assert!(((joint - pose.root).length() - (pose.joint - pose.root).length()).abs() < 1e-4);
        assert!(((foot - joint).length() - (pose.end - pose.joint).length()).abs() < 1e-4);
        // 目标在 +Z 更远处，脚只会落在可达边界，不会跟过去
        assert!(foot.z < 0.6);
    }

    /// 模拟端开关只关带 `FootIkTarget` 的骨架：本地角色照旧、远端单独停。
    ///
    /// 权重是每帧按 `want` 逼近的，所以直接读 `blend` 就能看出门控有没有生效
    /// （不需要真骨架：`want` 在拿 rig 之前就算完了）。
    #[test]
    fn remote_rigs_obey_their_own_switch() {
        fn blends(settings: FootIkSettings) -> (f32, f32) {
            let mut app = App::new();
            app.insert_resource(settings);
            app.insert_resource(Time::<()>::default());
            let world = app.world_mut();
            let local = world
                .spawn(FootIk {
                    blend: 1.0,
                    ..FootIk::default()
                })
                .id();
            let remote = world
                .spawn((
                    FootIk {
                        blend: 1.0,
                        ..FootIk::default()
                    },
                    FootIkTarget,
                ))
                .id();
            for _ in 0..10 {
                app.world_mut()
                    .resource_mut::<Time>()
                    .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
                app.world_mut()
                    .run_system_once(solve_foot_ik)
                    .expect("脚部 IK 系统跑不起来");
            }
            let world = app.world();
            (
                world.get::<FootIk>(local).expect("本地").blend,
                world.get::<FootIk>(remote).expect("远端").blend,
            )
        }

        // 远端关：远端淡到 0，本地不动
        let (local, remote) = blends(FootIkSettings {
            remote_enabled: false,
            ..FootIkSettings::default()
        });
        assert!(remote < 1e-3, "远端应该淡出到 0，实际 {remote}");
        assert!(local > 0.99, "本地不该受影响，实际 {local}");

        // 远端开：两边都保持
        let (local, remote) = blends(FootIkSettings::default());
        assert!(remote > 0.99, "远端开着就该保持，实际 {remote}");
        assert!(local > 0.99, "本地开着就该保持，实际 {local}");

        // 总开关关：两边都掉
        let (local, remote) = blends(FootIkSettings {
            enabled: false,
            ..FootIkSettings::default()
        });
        assert!(remote < 1e-3, "总开关关掉远端也该停，实际 {remote}");
        assert!(local < 1e-3, "总开关关掉本地也该停，实际 {local}");
    }

    /// 系统级验证：身体持续转动时，支撑脚应当钉在原地不动。
    ///
    /// 这条直接复现用户报的问题：混合空间按「相对身体」放脚，身体一转，
    /// 世界空间里支撑脚就会横着挪一小段。这里搭一套简化骨架（链长与 Amy 接近），
    /// 每帧模拟动画层重新写回腿部原姿态，再让身体偏航 0.9°/帧走 20 帧，
    /// 最后检查脚踝的世界位置还贴着锚点、骨长没被拉断。
    /// 踝角的地板：脚掌平放、小腿前倾 17° 时，踝角本来会压到 73°（「折叠」）。
    /// 这条地板要求脚跟先离地、脚掌俯下去抵消小腿前倾，把踝角守在 90°
    ///
    /// 把蹬地目标（`heel_off_ankle`）关掉，让这条约束单独受检：脚掌要俯到 ≈ 17°，
    /// 踝角要停在 90° 而不是 73°
    #[test]
    fn the_ankle_angle_never_folds_below_the_floor() {
        let mut app = App::new();
        app.insert_resource(FootIkSettings {
            toe_pivot_rise: 100.0,
            heel_off_lead: 0.0,
            // 关掉「蹬到 140°」那个目标，只剩地板在管
            heel_off_ankle: 0.0,
            heel_off_ankle_min: 90.0,
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

        // 小腿前倾 0.3rad（17.2°）；脚掌**平放**：脚骨世界朝向要把前掌那 26.6° 的
        // 下俯抵消掉，所以 flat 要一起减掉，脚掌相对地面才是 0°
        let bend = Quat::from_rotation_x(0.3);
        let flat = Quat::from_rotation_x(-0.3 - 0.5191);
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
                    Transform::from_xyz(0.0, -0.35, 0.0).with_rotation(flat),
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
                    toe_drop: 0.0,
                    foot_tilt: 0.0,
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
            scale_leg: 1.0,
            scale_foot: 1.0,
            tilt_calibrated: true,
        });
        world.entity_mut(model).insert(ik);

        let originals = [
            (left_bones.0, Quat::IDENTITY),
            (left_bones.1, bend),
            (left_bones.2, flat),
            (left_bones.3, Quat::IDENTITY),
            (right_bones.0, Quat::IDENTITY),
            (right_bones.1, bend),
            (right_bones.2, flat),
        ];
        let toe_world = |world: &World| {
            let mut matrix = Affine3A::IDENTITY;
            for entity in [
                root,
                model,
                armature,
                hips,
                left_bones.0,
                left_bones.1,
                left_bones.2,
                left_bones.3,
            ] {
                let local = world.get::<Transform>(entity).expect("骨骼");
                matrix *= local.compute_affine();
            }
            Vec3::from(matrix.translation)
        };

        let toe_rot_world = |world: &World| {
            let mut rotation = Quat::IDENTITY;
            for entity in [
                root,
                model,
                armature,
                hips,
                left_bones.0,
                left_bones.1,
                left_bones.2,
                left_bones.3,
            ] {
                rotation *= world.get::<Transform>(entity).expect("骨骼").rotation;
            }
            rotation
        };

        let mut planted_toe = Vec3::ZERO;
        let mut toe_rot0 = Quat::IDENTITY;
        let mut angles = Vec::new();
        for frame in 0..30u32 {
            let world = app.world_mut();
            for (entity, rotation) in originals {
                world.get_mut::<Transform>(entity).expect("腿骨").rotation = rotation;
            }
            if frame < 12 {
                world.get_mut::<Transform>(root).expect("根").translation.z += 0.012;
            }
            if frame == 0 {
                planted_toe = toe_world(world);
                toe_rot0 = toe_rot_world(world);
            }
            world
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
            world
                .run_system_once(solve_foot_ik)
                .expect("脚部 IK 系统跑不起来");
            let ik = world.get::<FootIk>(model).expect("FootIk 组件");
            angles.push((ik.pitch()[0], ik.ankle_deg()[0]));
        }

        let world = app.world();
        // 抬跟把脚掌绕前掌转了 30°+，前掌骨必须在球窝处折起来同样的角，
        // 脚尖的世界朝向才不会跟着扎进地里
        let toe_rot1 = toe_rot_world(world);
        assert!(
            toe_rot0.angle_between(toe_rot1) < 0.035,
            "前掌骨的世界朝向应基本不变（脚趾留在地上），实际偏了 {:.1}°",
            toe_rot0.angle_between(toe_rot1).to_degrees()
        );
        // 折角要和脚掌面俯角同源：脚掌绕前掌转了多少，前掌骨就得反向折多少
        let toe_fold = world
            .get::<Transform>(left_bones.3)
            .expect("前掌骨")
            .rotation
            .angle_between(Quat::IDENTITY)
            .to_degrees();
        let sole = angles.last().expect("有帧").0;
        assert!(
            (toe_fold - sole).abs() < 3.0,
            "前掌骨折角应 ≈ 脚掌面俯角（{sole:.1}°），实际 {toe_fold:.1}°"
        );

        let ik = world.get::<FootIk>(model).expect("FootIk 组件");
        assert!(
            ik.heel_off()[0] > 0.0,
            "这个姿态下重心在踝前方，抬跟应当已经触发"
        );
        // 第一帧锁定那一刻踝角确实是折着的：脚掌平放 + 小腿前倾 → 90 − 17 ≈ 73。
        // 地板一圈只准补 4°，所以头一两帧还看得见这个折角
        let folded = angles[0].1;
        assert!(
            folded < 80.0,
            "动画本身就把踝角折到 {folded:.1}°（脚掌平放、小腿前倾 17°）"
        );
        // 地板生效之后：脚掌俯到 ≈ 19°（小腿前倾随踝抬高从 17° 变成 ~19°），
        // 踝角守在 90°
        let (pitch, ankle) = *angles.last().expect("有帧");
        assert!(
            pitch > 15.0,
            "脚掌应当绕前掌俯到小腿前倾的那个角度（≈19°），实际 {pitch:.1}°"
        );
        assert!(ankle >= 89.0, "踝角不许折到 90 以下，实际 {ankle:.1}°");
        // 抬完之后一路上都不许再掉下去。
        // 从这里起算而不是第 10 帧：地板按 ANKLE_FLOOR_RATE = 240°/s 限速（和动画
        // 自身的跖屈叠起来不能超过单帧 10°），从 72.8° 爬到 90° 要 ~0.22s = 13 帧，
        // 这十几帧里本来就还在爬
        let lowest = angles[13..]
            .iter()
            .map(|(_, angle)| angle)
            .cloned()
            .fold(f32::MAX, f32::min);
        assert!(lowest >= 89.0, "支撑后半段踝角掉到了 {lowest:.1}°");

        // 前掌还是钉在原地：地板也是绕前掌补的
        let toe = toe_world(world);
        let drift = (toe - planted_toe).length();
        assert!(drift < 0.02, "地板把前掌带走了 {drift:.3}m");
    }

    /// 踝角口径必须按**脚掌面**算，而 `foot_pitch_deg` 量的是「踝→前掌」这条骨
    /// 向量：脚掌平放时它已经俯了 26~62°（每根 rig 不同）。这条测试把同一套几何
    /// 的 `foot_tilt` 设成 29.7°，地板仍然要把**脚掌面**的踝角守在 90°，
    /// 也就是骨向量口径下要走到 29.7° + 小腿前倾
    #[test]
    fn the_ankle_floor_measures_the_sole_not_the_bone() {
        let mut app = App::new();
        app.insert_resource(FootIkSettings {
            toe_pivot_rise: 100.0,
            heel_off_lead: 0.0,
            heel_off_ankle: 0.0,
            heel_off_ankle_min: 90.0,
            max_correction: 0.40,
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

        // 前掌关节在踝下方 8cm、前方 14cm → 静止俯角 29.7°（真实 rig 是 26~62°）
        let tilt = (0.08f32).atan2(0.14).to_degrees();
        let bend = Quat::from_rotation_x(0.3);
        // 脚**平放**（脚掌面水平）：只抵消小腿前倾 17°，不再抵消脚骨自己那 29.7°
        let flat = Quat::from_rotation_x(-0.3);
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
                    Transform::from_xyz(0.0, -0.35, 0.0).with_rotation(flat),
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
                    toe_drop: 0.08,
                    foot_tilt: tilt,
                },
                (upper, lower, foot),
            )
        };
        let (left, left_bones) = make_leg(world, "Left", 0.08);
        let (right, _) = make_leg(world, "Right", -0.08);

        let mut ik = FootIk::default();
        ik.contacts = [1.0, 1.0];
        ik.blend = 1.0;
        ik.rig = Some(FootRig {
            spine: vec![root, model, armature, hips],
            legs: [left, right],
            scale_leg: 1.0,
            scale_foot: 1.0,
            tilt_calibrated: true,
        });
        world.entity_mut(model).insert(ik);

        let originals = [
            (left_bones.0, Quat::IDENTITY),
            (left_bones.1, bend),
            (left_bones.2, flat),
        ];
        let mut angles = Vec::new();
        for frame in 0..30u32 {
            let world = app.world_mut();
            for (entity, rotation) in originals {
                world.get_mut::<Transform>(entity).expect("腿骨").rotation = rotation;
            }
            if frame < 12 {
                world.get_mut::<Transform>(root).expect("根").translation.z += 0.012;
            }
            world
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
            world
                .run_system_once(solve_foot_ik)
                .expect("脚部 IK 系统跑不起来");
            let ik = world.get::<FootIk>(model).expect("FootIk 组件");
            angles.push((ik.pitch()[0], ik.ankle_deg()[0]));
        }

        let (pitch, ankle) = *angles.last().expect("有帧");
        // 脚掌平放、小腿前倾 17° → 踝角本来就在 73°，骨向量口径会把那 29.7°
        // 算进去（读成 102.7°），面口径必须读回 73°
        let folded = angles[0].1;
        assert!(
            (68.0..=76.0).contains(&folded),
            "第一帧的面口径踝角应 ≈73°（90 − 17），实际 {folded:.1}°"
        );
        assert!(
            angles[0].0.abs() < 2.0,
            "脚掌面平放时面口径俯角应 ≈0°，实际 {:.1}°",
            angles[0].0
        );
        // 地板生效后：脚掌面俯角要跟着小腿前倾走（踝抬 10cm 后小腿从 17° 斜到
        // ~39°），踝角守在 90°
        assert!(
            (12.0..=45.0).contains(&pitch),
            "面口径俯角应跟着小腿前倾（≈17° 起、随抬跟变大），实际 {pitch:.1}°"
        );
        assert!(ankle >= 89.0, "脚掌面的踝角应守在 90°，实际 {ankle:.1}°");
    }

    #[test]
    fn ik_pins_the_ankle_while_the_body_turns() {
        let mut app = App::new();
        // 这条测的是「转身时脚踝被钉在原地」本身：抬跟会把踝绕前掌抬起来、
        // 水平上也会跟着往后挪一截，那是另一条测试的事。这里把起跟的最小支撑
        // 时长拉到超过这 20 帧，抬跟不介入
        app.insert_resource(FootIkSettings {
            heel_off_min_secs: 1.0,
            ..FootIkSettings::default()
        });
        app.insert_resource(Time::<()>::default());

        let world = app.world_mut();
        // 起点往后放 15cm：小腿前倾会让脚落在骨盆后方，往回挪一点，
        // 让「身体往前蹬出去」这段位移在模型里真的发生
        let root = world
            .spawn((Name::new("Avatar"), Transform::from_xyz(0.0, 0.0, -0.15)))
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
                    toe_drop: 0.0,
                    foot_tilt: 0.0,
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
            scale_leg: 1.0,
            scale_foot: 1.0,
            tilt_calibrated: true,
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
                world.get_mut::<Transform>(root).expect("根").rotation = Quat::from_rotation_y(yaw);
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
        // 锚点现在钉的是**前掌**（球），踝是被允许绕球走的：18° 里踝扛 12°、
        // 剩下 6° 绕球，踝沿球走出 9mm。不锁时身体会把它横向带走 29mm。
        // 画面上真正压地的那一点是前掌（见 `FootLock::set_ball_pin`）
        assert!(
            drift < 0.012,
            "脚踝绕球走偏了 {drift:.4}m（不锁 {hip_drift:.4}m）"
        );

        // 位置锁住了还不够：脚掌的**朝向**也得钉住，否则身体一转脚掌就在地上「拧螺丝」。
        // 第 0 帧身体已经转了 0.9°，之后 20 帧转到 18° —— 没有偏航锁的话脚掌会跟着转 17° 多
        let toe_entity = left.toe.expect("左脚应当有前掌骨");
        let mut matrix = Affine3A::IDENTITY;
        for entity in [
            root,
            model,
            armature,
            hips,
            left_bones.0,
            left_bones.1,
            left_bones.2,
            toe_entity,
        ] {
            let local = world.get::<Transform>(entity).expect("骨骼");
            matrix *= local.compute_affine();
        }
        let toe = Vec3::from(matrix.translation);
        let yaw_deg = foot_yaw(ankle, toe).expect("脚掌平放，量得到").to_degrees();
        // 脚掌朝向：踝能扛的那段 `ball_pivot_enter` 仍被锁住，超出去的部分改成绕球转
        // ——所以脚掌**允许**跟着身体走那 6°，踝关节只拧 12°，不是 17°
        let enter = app
            .world()
            .resource::<FootIkSettings>()
            .ball_pivot_enter
            .max(0.0);
        let want = 18.0 - 0.9; // 踩实发生在第 0 帧，那时身体已经转到 0.9°
        let expect = (want - enter).max(0.0);
        assert!(
            (yaw_deg - expect).abs() < 2.0,
            "着地脚的朝向 {yaw_deg:+.1}°，期望 {expect:+.1}°（身体转 {want:.1}°，踝扛 {enter:.0}°）"
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

    /// 系统级验证「抬跟的幅度与绕点」：脚掌绕前掌蹬到**踝角** 140°，前掌留在原地，
    /// 离地后俯角要立刻交回动画。
    ///
    /// 口径是踝角（= 90 + 脚掌相对地面的俯角 − 小腿前倾角）：同样把脚掌俯到 55°，
    /// 小腿前倾 30° 时只有 115°，看着就是折叠。所以目标必须按小腿的倾斜折算回来。
    #[test]
    fn heel_off_starts_with_the_body_and_reaches_the_target_pitch() {
        let mut app = App::new();
        app.insert_resource(FootIkSettings {
            // 这条专门测「重心→抬跟→俯角」这条链，兜底的动画抬踝阈值不参与
            toe_pivot_rise: 100.0,
            // 几何上按「重心压到踝上」算触发帧：脚踩实时在骨盆前方 10cm，
            // 每帧 1.2cm → 第 9 帧越过。lead 取 0，预期帧才是算出来的那个
            heel_off_lead: 0.0,
            // 骨架腿短、步子大，横向修正远超默认 22cm，这里放开
            max_correction: 0.40,
            ..FootIkSettings::default()
        });
        app.insert_resource(Time::<()>::default());

        let world = app.world_mut();
        // 起点往后放 15cm：小腿前倾会让脚落在骨盆后方，往回挪一点，
        // 让「身体往前蹬出去」这段位移在模型里真的发生
        let root = world
            .spawn((Name::new("Avatar"), Transform::from_xyz(0.0, 0.0, -0.15)))
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

        // 小腿前倾 0.3rad（膝在踝前方，就是支撑末期真实的样子）。它决定了
        // 踝角比脚掌的地面俯角小 17°，所以同样的目标踝角要俯得更多
        let bend = Quat::from_rotation_x(0.3);
        // 脚掌本身回正：动画给的是一个平放的脚，俯角由踝-前掌的几何决定（约 30°）
        let flat = Quat::from_rotation_x(-0.3);
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
                    Transform::from_xyz(0.0, -0.35, 0.0).with_rotation(flat),
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
                    toe_drop: 0.0,
                    foot_tilt: 0.0,
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
            scale_leg: 1.0,
            scale_foot: 1.0,
            tilt_calibrated: true,
        });
        world.entity_mut(model).insert(ik);

        // 动画层每帧写回同一套姿态：脚掌始终平放（俯角 ≈ 30°，远不到 45°）
        let originals = [
            (left_bones.0, Quat::IDENTITY),
            (left_bones.1, bend),
            (left_bones.2, flat),
            (right_bones.0, Quat::IDENTITY),
            (right_bones.1, bend),
            (right_bones.2, flat),
        ];
        let toe_world = |world: &World, bones: (Entity, Entity, Entity, Entity)| {
            let mut matrix = Affine3A::IDENTITY;
            for entity in [
                root, model, armature, hips, bones.0, bones.1, bones.2, bones.3,
            ] {
                let local = world.get::<Transform>(entity).expect("骨骼");
                matrix *= local.compute_affine();
            }
            Vec3::from(matrix.translation)
        };

        let mut planted_toe = Vec3::ZERO;
        let mut crossed = None;
        for frame in 0..45u32 {
            let world = app.world_mut();
            for (entity, rotation) in originals {
                world.get_mut::<Transform>(entity).expect("腿骨").rotation = rotation;
            }
            // 前 12 帧身体往前走（每帧 1.2cm，共 14cm）：踩实那一刻骨盆已经在
            // 踝前方一点，走的这一段就是「蹬出去」的距离；之后停住让抬跟走完
            if frame < 12 {
                world.get_mut::<Transform>(root).expect("根").translation.z += 0.012;
            }
            if frame == 0 {
                planted_toe = toe_world(world, left_bones);
            }
            world
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
            world
                .run_system_once(solve_foot_ik)
                .expect("脚部 IK 系统跑不起来");

            let ik = world.get::<FootIk>(model).expect("FootIk 组件");
            if crossed.is_none() && ik.heel_off()[0] > 0.0 {
                crossed = Some(frame);
            }
        }

        let world = app.world();
        let ik = world.get::<FootIk>(model).expect("FootIk 组件");
        let started = crossed.expect("重心压在脚上之后应当起跟");
        // 这条测的是**幅度与绕点**（顶到哪个角、前掌动不动）；起跟时机由
        // `foot_lock::heel_off_waits_for_the_body_to_cross_the_planted_foot` 盯着。
        // 这里小腿前倾，骨盆一开始就小幅落在踝前方，所以最小支撑时长一过就起跟
        assert!(
            (4..=6).contains(&started),
            "最小支撑时长（0.08s ≈ 5 帧）一过才该起跟，实际第 {started} 帧"
        );

        let heel = ik.heel_off()[0];
        assert!(heel > 0.99, "抬跟应当走满，实际 {heel:.2}");
        // 小腿前倾 17°：目标踝角 140° → 脚掌相对地面要俯 140 − 90 + 17 ≈ 67°
        let pitch = ik.pitch()[0];
        assert!(
            pitch > 60.0,
            "支撑末期脚掌应当俯到 67° 上下（踝角 140°），实际 {pitch:.1}°"
        );
        // 踝角 = 90 + 俯角 − 小腿前倾。这条测试里锁脚把踝钉在原地、身体走出一大截，
        // 解出来的膝前倾 ~40°，要 140° 就得把脚掌俯到 90°（垂直）——
        // `heel_off_lift` 在脚掌接近垂直时（水平分量 < 20%）就不再做转轴了，
        // 所以上限被 `HEEL_OFF_PITCH_MAX = 76` 封在 ~126°。这里断言的是「确实
        // 蹬到了几何上限」，而不是那个理论值
        let ankle = ik.ankle_deg()[0];
        assert!(
            ankle >= 120.0,
            "踝角应当蹬到 120° 以上（动画自己只有 ~113°），实际 {ankle:.1}°"
        );

        // 前掌还是钉在原地：抬跟绕的是它，不是踝
        let toe = toe_world(world, left_bones);
        let drift = (toe - planted_toe).length();
        assert!(
            drift < 0.02,
            "抬跟把前掌带走了 {drift:.3}m（应在 {planted_toe:?}）"
        );

        // ---- 离地之后：俯角要在 0.1s 内交回动画，不能跟着锁脚残差慢慢放平 ----
        // 锁脚的 `release_secs` 是 0.26s（给位置残差还债用）。俯角要是也按它收回，
        // 脚离地后还会一边被拽住一边慢慢放下 —— 那正是「被身体带着走」的观感
        let world = app.world_mut();
        world.get_mut::<FootIk>(model).expect("FootIk").contacts = [0.0, 0.0];
        for _ in 0..7 {
            let world = app.world_mut();
            for (entity, rotation) in originals {
                world.get_mut::<Transform>(entity).expect("腿骨").rotation = rotation;
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
        let pitch_after = ik.pitch()[0];
        // 7 帧 = 0.12s：按 0.26s 的残差节奏这里还该有 ~45°（补的那份只掉一半），
        // 按 0.1s 的抬跟节奏已经收干净，只剩动画自己的 ~30°
        assert!(
            (pitch_after - 30.0).abs() < 6.0,
            "离地 0.12s 后脚掌该回到动画的俯角（约 30°），实际 {pitch_after:.1}°"
        );
        assert!(!ik.planted()[0], "抬脚后不该还算踩实");
    }

    /// 系统级验证「停下来那一脚不会先翘着、再砸下去」：脚掌平放、小腿前倾 17°
    /// （动画自己的踝角 ~73°，低于 90° 的地板），也就是 `Castle` 那种长脚 rig
    /// 停下之后的样子 —— 地板会把脚掌一直压在抬跟姿态上。脚离地之后，抬跟的
    /// **转角**必须和**踝抬高**按同一个系数收：转角留在满量、位移先还回去的话，
    /// 那一帧踝会凭空掉 4cm、前掌扎进地里 3cm，再花几帧浮回来（观感就是
    /// 「落下 → 抬起 → 重新跺在地上」）
    #[test]
    fn heel_off_fades_out_together_with_its_ankle_offset() {
        let mut app = App::new();
        app.insert_resource(FootIkSettings {
            toe_pivot_rise: 100.0,
            heel_off_lead: 0.0,
            heel_off_ankle: 0.0,
            heel_off_ankle_min: 90.0,
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

        let bend = Quat::from_rotation_x(0.3);
        let flat = Quat::from_rotation_x(-0.3 - 0.5191);
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
                    Transform::from_xyz(0.0, -0.35, 0.0).with_rotation(flat),
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
                    toe_drop: 0.0,
                    foot_tilt: 0.0,
                },
                (upper, lower, foot, toe),
            )
        };
        let (left, left_bones) = make_leg(world, "Left", 0.08);
        let (right, _) = make_leg(world, "Right", -0.08);

        let mut ik = FootIk::default();
        ik.contacts = [1.0, 1.0];
        ik.blend = 1.0;
        ik.rig = Some(FootRig {
            spine: vec![root, model, armature, hips],
            legs: [left, right],
            scale_leg: 1.0,
            scale_foot: 1.0,
            tilt_calibrated: true,
        });
        world.entity_mut(model).insert(ik);

        let originals = [
            (left_bones.0, Quat::IDENTITY),
            (left_bones.1, bend),
            (left_bones.2, flat),
            (left_bones.3, Quat::IDENTITY),
        ];
        let ankle_world = |world: &World| {
            let mut matrix = Affine3A::IDENTITY;
            for entity in [
                root,
                model,
                armature,
                hips,
                left_bones.0,
                left_bones.1,
                left_bones.2,
            ] {
                let local = world.get::<Transform>(entity).expect("骨骼");
                matrix *= local.compute_affine();
            }
            Vec3::from(matrix.translation)
        };
        let toe_world = |world: &World| {
            let mut matrix = Affine3A::IDENTITY;
            for entity in [
                root,
                model,
                armature,
                hips,
                left_bones.0,
                left_bones.1,
                left_bones.2,
                left_bones.3,
            ] {
                let local = world.get::<Transform>(entity).expect("骨骼");
                matrix *= local.compute_affine();
            }
            Vec3::from(matrix.translation)
        };
        let step = |world: &mut World, rotations: &[(Entity, Quat)]| {
            for (entity, rotation) in rotations {
                world.get_mut::<Transform>(*entity).expect("腿骨").rotation = *rotation;
            }
            world
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
            world
                .run_system_once(solve_foot_ik)
                .expect("脚部 IK 系统跑不起来");
        };

        // 解算之前先量「动画自己的踝/前掌高度」当基准（骨骼还没被写过）
        let rest_ankle = ankle_world(app.world()).y;
        let rest_toe = toe_world(app.world()).y;

        // 踩实 30 帧把抬跟走满：前 12 帧让身体从踝上方走出来（起跟要有重心越过
        // 支撑脚这件事），之后站住不动 —— 地板把脚掌压在 90° 踝角上，踝被抬离基准
        let mut raised_ankle = rest_ankle;
        for frame in 0..30u32 {
            if frame < 12 {
                app.world_mut()
                    .get_mut::<Transform>(root)
                    .expect("根")
                    .translation
                    .z += 0.012;
            }
            step(app.world_mut(), &originals);
            raised_ankle = ankle_world(app.world()).y;
        }
        let raised = raised_ankle - rest_ankle;
        assert!(
            raised > 0.01,
            "地板应当把踝抬高（脚掌翘着），实际只抬了 {:.1}mm",
            raised * 1000.0
        );

        // 松脚：动画姿态完全不动，只有锁脚在放
        app.world_mut()
            .get_mut::<FootIk>(model)
            .expect("FootIk")
            .contacts = [0.0, 0.0];
        let mut ankles = Vec::new();
        let mut toes = Vec::new();
        for _ in 0..18 {
            step(app.world_mut(), &originals);
            ankles.push(ankle_world(app.world()).y);
            toes.push(toe_world(app.world()).y);
        }

        // 1) 踝的下降必须是连续的。判据用**抬高量的比例**：0.1s 的 smoothstep
        // 摊开时，单帧最多落 ~0.24 倍抬高；旧写法（转角留满量、只在 `weight`
        // 归零那一帧甩掉）会一帧落掉整整一倍
        let mut biggest = 0.0f32;
        for pair in ankles.windows(2) {
            biggest = biggest.max((pair[0] - pair[1]).abs());
        }
        assert!(
            biggest < raised * 0.35,
            "离地之后踝一帧掉了 {:.1}cm（抬高 {:.1}cm，应当和抬跟转角同步、0.1s 摊开）",
            biggest * 100.0,
            raised * 100.0
        );
        // 2) 前掌不许被转角的残量带进地里
        let lowest = toes.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            lowest > rest_toe - 0.005,
            "前掌被压到比静止位低 {:.1}cm（静止 {:.1}mm，最低 {:.1}mm）",
            (rest_toe - lowest) * 100.0,
            rest_toe * 1000.0,
            lowest * 1000.0
        );
        // 3) 0.15s（9 帧）之内必须完全还给动画：抬跟不能跟着 0.26s 的残差节奏拖
        let settled = *ankles.last().expect("有帧");
        assert!(
            (settled - rest_ankle).abs() < 0.01,
            "0.3s 之后踝还差 {:.1}cm 没回位（静止 {:.1}mm，现在 {:.1}mm）",
            (settled - rest_ankle).abs() * 100.0,
            rest_ankle * 1000.0,
            settled * 1000.0
        );
        let ik = app.world().get::<FootIk>(model).expect("FootIk 组件");
        assert!(
            ik.pitch()[0] < 2.0,
            "离地后脚掌俯角应当回到动画的 0°，实际 {:.1}°",
            ik.pitch()[0]
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
        // 起点往后放 15cm：小腿前倾会让脚落在骨盆后方，往回挪一点，
        // 让「身体往前蹬出去」这段位移在模型里真的发生
        let root = world
            .spawn((Name::new("Avatar"), Transform::from_xyz(0.0, 0.0, -0.15)))
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
                    toe_drop: 0.0,
                    foot_tilt: 0.0,
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
            scale_leg: 1.0,
            scale_foot: 1.0,
            tilt_calibrated: true,
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
            for entity in [
                root, model, armature, hips, bones.0, bones.1, bones.2, bones.3,
            ] {
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
            world
                .get_mut::<Transform>(left_bones.0)
                .expect("大腿")
                .translation
                .y += 0.004;
            world
                .get_mut::<Transform>(right_bones.0)
                .expect("大腿")
                .translation
                .y += 0.004;
            // 脚掌蹬地：抵消掉小腿预弯，让脚的世界朝向≈水平，再叠加蹬地俯仰
            let pitch = 0.04 * (frame + 1) as f32;
            let foot_rot = Quat::from_rotation_x(pitch - 1.0);
            world
                .get_mut::<Transform>(left_bones.2)
                .expect("脚")
                .rotation = foot_rot;
            world
                .get_mut::<Transform>(right_bones.2)
                .expect("脚")
                .rotation = foot_rot;
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
        assert!(
            roll >= 0.99,
            "脚跟抬起来后承重点应当完全滑到前掌，实际 {roll:.2}"
        );

        let toe = toe_world(world, left_bones);
        let toe_drift = (toe.y - planted_toe.y)
            .abs()
            .max(Vec3::new(toe.x - planted_toe.x, 0.0, toe.z - planted_toe.z).length());
        // 1cm：夹紧量在脚掌离地后要淡出（十几个度摊在 60ms 里），那几帧的前掌会
        // 沿弧线收回 ~1cm——这是「平滑撤掉」的代价，比当帧翻回去好
        assert!(
            toe_drift < 0.015,
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

    /// 动画的膝盖朝向逐帧抖（混合空间里两条剪辑的极点差一点、权重一动就来回晃）
    /// 动画的膝盖朝向逐帧跳（混合空间换剪辑 / 权重抖动）时，极点必须低通：
    /// 不平滑时膝盖每帧跟着转 ±5°，平滑后只剩零头
    #[test]
    fn pole_smoothing_damps_frame_to_frame_jitter() {
        let dt = 1.0 / 60.0;
        let base = Vec3::Y;
        let tilt = |degrees: f32| Quat::from_rotation_x(degrees.to_radians()) * base;
        let mut previous = None;
        let mut max_deviation = 0.0f32;
        for frame in 0..120 {
            let animated = tilt(if frame % 2 == 0 { 5.0 } else { -5.0 });
            let pole = smooth_pole(previous, animated, dt, FootIkSettings::default().pole_secs)
                .expect("极点不该退化");
            previous = Some(pole);
            if frame >= 30 {
                max_deviation = max_deviation.max(pole.angle_between(base).to_degrees());
            }
        }
        assert!(
            max_deviation < 2.0,
            "极点还剩 {max_deviation:.1}° 的帧间抖动（输入 ±5°）"
        );

        // 平滑不能拖住真实的转动：角色以 180°/s 转身时，极点最多落后 10° 左右
        let mut previous = None;
        let mut max_lag = 0.0f32;
        for frame in 0..180 {
            let yaw = 180.0 * dt * frame as f32;
            let animated = Quat::from_rotation_y(yaw.to_radians()) * base;
            let pole = smooth_pole(previous, animated, dt, FootIkSettings::default().pole_secs)
                .expect("极点不该退化");
            previous = Some(pole);
            max_lag = max_lag.max(pole.angle_between(animated).to_degrees());
        }
        assert!(max_lag < 15.0, "转身时极点落后 {max_lag:.1}°");

        // 关掉平滑就是完全跟随
        let pole = smooth_pole(Some(base), tilt(5.0), dt, 0.0).expect("极点不该退化");
        assert!(pole.angle_between(tilt(5.0)) < 1e-4);
    }

    /// 混合空间换剪辑时，动画的踝角会在某一帧跳一下：离地后必须削平，
    /// 否则就是脚掌「闪一下」。支撑期不走这条（脚在地上当支点，朝向要精确）
    #[test]
    fn ankle_spike_in_flight_is_rate_limited() {
        let dt = 1.0 / 60.0;
        let cfg = FootIkSettings {
            foot_max_rate: 360.0,
            ..Default::default()
        };
        let rate = cfg.foot_max_rate;
        let base = Quat::IDENTITY;
        // 40° 的单帧跳变：直接传下去网格上就是一帧的「闪光」
        let spike = Quat::from_rotation_x(40f32.to_radians());

        let mut previous = None;
        let mut biggest_step = 0.0f32;
        for frame in 0..30 {
            // 前 10 帧平稳，第 10 帧跳一下，之后又平稳
            let animated = if frame == 10 { spike } else { base };
            let rot = limit_ankle_rotation(previous, animated, dt, rate).expect("朝向不该退化");
            biggest_step = biggest_step.max(rot.angle_between(base).to_degrees());
            previous = Some(rot);
        }
        // 每帧最多 360°/s × 1/60 = 6°：第一帧就把 40° 摊成 6°
        assert!(
            biggest_step <= rate * dt + 0.01,
            "限速 6°/帧，实际单帧转了 {biggest_step:.2}°"
        );
        // 40° 按 6°/帧 摊 7 帧，指定帧数内必须回到动画（限速器不会永久拖欠）
        let mut previous = Some(spike);
        let mut last = base;
        for _ in 0..7 {
            last = limit_ankle_rotation(previous, base, dt, rate).expect("朝向不该退化");
            previous = Some(last);
        }
        assert!(
            last.angle_between(base) < 0.01,
            "7 帧后还差 {:.2}°",
            last.angle_between(base).to_degrees()
        );

        // 关键差别：平滑的动画（≤ 阈值）必须**零滞后**。低通在这里会一直拖
        let mut previous = None;
        let mut animated = base;
        for _ in 0..60 {
            animated = Quat::from_rotation_x(300f32.to_radians() * dt) * animated;
            let rot = limit_ankle_rotation(previous, animated, dt, rate).expect("朝向不该退化");
            previous = Some(rot);
        }
        assert!(
            previous.unwrap().angle_between(animated).to_degrees() < 0.01,
            "300°/s 的平滑动画被拖慢了 {:.2}°",
            previous.unwrap().angle_between(animated).to_degrees()
        );

        // 关掉限速 = 完全跟随
        let exact =
            limit_ankle_rotation(Some(base), spike, dt, f32::INFINITY).expect("朝向不该退化");
        assert!(exact.angle_between(spike) < 1e-4);
    }

    /// 过渡的起点必须是「落地那一刻」的快照，不是上一帧的输出：
    /// 用上一帧递归地插值，落地后动画还在动时会一直追不上，等 landing 到 1 才猛然对齐
    #[test]
    fn landing_starts_from_the_touchdown_snapshot() {
        let snapshot = Quat::from_rotation_x(-0.20); // 落地那一刻的踝角
        let previous = Quat::from_rotation_x(0.05); // 上一帧输出（已经被插值带跑）
        let animated = Quat::from_rotation_x(0.30); // 动画面还在往前走
        let (rot, _, _) = resolve_foot_orientation(
            Some(previous),
            None,
            animated,
            Quat::IDENTITY,
            true,
            false,
            0.5,
            Some(snapshot),
            None,
            None,
            0.08,
            false,
            Vec3::ZERO,
            Vec3::ZERO,
            0.0,
            1.0 / 60.0,
            360.0,
        );
        // 半程 = 快照与动画的中点；绝不能是「上一帧输出」与动画的中点
        let expected = snapshot.slerp(animated, 0.5);
        assert!(
            rot.angle_between(expected) < 1e-4,
            "过渡起点错了：实际距「快照半程」{:.2}°",
            rot.angle_between(expected).to_degrees()
        );
        assert!(
            rot.angle_between(previous.slerp(animated, 0.5)) > 0.05,
            "起点还挂在上一帧输出上"
        );
    }

    /// 落地过渡必须是**单调趋近地面**：前掌高度一帧比一帧高，到点停住。
    /// 一旦中间某一帧比最终姿态还高（先抬过头再放回来），
    /// 观感就是用户说的「放平了又反弹」
    #[test]
    fn landing_approaches_the_floor_monotonically() {
        let ankle = Vec3::new(0.0, 0.10, 0.0);
        // 动画姿态：脚掌跖屈 0.10 rad，前掌被压到 0.0064（地面在 0.02）
        let animated = Quat::from_rotation_x(0.10);
        // 落地前空中的姿态：更跖屈
        let air = Quat::from_rotation_x(0.20);
        let toe_offset = Vec3::new(0.0, -0.08, 0.14);
        let toe_floor = 0.02;
        let toe_height = |landing: f32| {
            let (rot, _, _) = resolve_foot_orientation(
                Some(air),
                None,
                animated,
                Quat::IDENTITY,
                true,
                false,
                landing,
                None,
                Some(toe_floor),
                Some(ankle + animated * toe_offset), // 前掌世界位置（动画姿态下）
                0.08,
                true,
                ankle,
                ankle,
                1.0,
                1.0 / 60.0,
                360.0,
            );
            ankle.y + (rot * toe_offset).y
        };

        let steps: Vec<f32> = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0]
            .into_iter()
            .map(toe_height)
            .collect();
        for pair in steps.windows(2) {
            assert!(
                pair[1] >= pair[0] - 1e-4,
                "前掌高度回落了：{:.4} → {:.4}",
                pair[0],
                pair[1]
            );
        }
        let settled = *steps.last().expect("至少一帧");
        assert!(
            (settled - toe_floor).abs() < 2e-3,
            "过渡结束后前掌应贴在地面 {toe_floor}，实际 {settled:.4}"
        );
    }

    /// 夹紧量必须只取决于**基准姿态**（支撑期就是动画值），
    /// 不能随落地过渡的进度变：否则首帧按空中的跖屈姿态算出「超上限」的量，
    /// 等姿态回平需求又骤降，脚掌就会「先抬过头再放回来」
    #[test]
    fn clamp_angle_does_not_shift_with_the_landing_progress() {
        let animated = Quat::IDENTITY; // 脚掌水平
        // 踝高 0.10、脚掌水平时前掌在 0.02；射线量到地面其实在 0.05 → 脚已穿地 3cm
        let ankle = Vec3::new(0.0, 0.10, 0.0);
        let toe = Vec3::new(0.0, -0.08, 0.14);
        let air = Quat::from_rotation_x(-0.25); // 落地前空中的跖屈姿态
        let resolve = |landing: f32| {
            resolve_foot_orientation(
                Some(air),
                None,
                animated,
                Quat::IDENTITY,
                true,
                false,
                landing,
                None,
                Some(0.05), // 射线量到地面在 0.05，前掌已经穿下去了（要夹）
                Some(toe),
                0.08,
                true,
                ankle,
                ankle,
                1.0,
                1.0 / 60.0,
                360.0,
            )
        };
        let (_, start) = resolve(0.2).2.expect("这个构造下应当有夹紧量");
        let (_, settled) = resolve(1.0).2.expect("这个构造下应当有夹紧量");
        assert!(
            (start - settled).abs() < 1e-6,
            "夹紧量随过渡进度变了：0.2 时 {:.1}°、1.0 时 {:.1}°",
            start.to_degrees(),
            settled.to_degrees()
        );
        assert!(
            settled.abs() > 1e-3,
            "这个构造下应当有夹紧量，实际 {:.3}°",
            settled.to_degrees()
        );
    }

    /// 支撑期 `planted` 会随咬合迟滞抖，落地进度不能跟着清零重启，
    /// 否则脚掌会反复从旧姿态重爬（观感是抖动的残影）
    #[test]
    fn landing_keeps_climbing_while_contact_flickers() {
        let secs = 0.08;
        let dt = 1.0 / 60.0;
        let step = dt / secs;

        // 第一次落地（离地时长足够）：从 0 起
        let (first, air) = advance_landing(0.0, true, 1.0, dt, secs);
        assert!((first - step).abs() < 1e-5 && air == 0.0);
        // 连续踩实：累加
        let (second, _) = advance_landing(first, true, 0.0, dt, secs);
        assert!(second > first);
        // 抖一帧没踩实（空档很短）：进度保持
        let (flicker, air) = advance_landing(second, false, 0.0, dt, secs);
        assert!((flicker - second).abs() < 1e-6, "空档一帧就清零了");
        // 再踩实：继续爬，不回到 0
        let (resumed, _) = advance_landing(flicker, true, air, dt, secs);
        assert!(
            resumed > second,
            "接触抖一下就把进度打回 {:.2}（原来是 {:.2}）",
            resumed,
            second
        );
        // 真正离地（空档超过阈值）再落地：这时才清零重来
        let (fresh, _) = advance_landing(1.0, true, 0.2, dt, secs);
        assert!((fresh - step).abs() < 1e-5);
    }

    /// 落地那一帧不能把脚掌一帧从「空中的限速值」切到「动画值」——
    /// 十几度的差价要按 landing 摊成几帧，否则就是「落地瞬间弹一下」
    #[test]
    fn landing_spreads_the_rate_limited_pose_into_the_animated_one() {
        let previous = Quat::from_rotation_x(-0.30); // 空中限速后停在这儿
        let animated = Quat::from_rotation_x(0.20); // 动画已经是另一个角度
        let resolve = |landing: f32| {
            resolve_foot_orientation(
                Some(previous),
                None,
                animated,
                Quat::IDENTITY,
                true,
                false,
                landing,
                None,
                None,
                None,
                0.08,
                false,
                Vec3::ZERO,
                Vec3::ZERO,
                0.0,
                1.0 / 60.0,
                360.0,
            )
            .0
        };

        // 落地首帧：还停在限速值上，一帧都不许跳
        let at_zero = resolve(0.0);
        assert!(
            at_zero.angle_between(previous) < 1e-4,
            "落地首帧就跳了 {:.1}°",
            at_zero.angle_between(previous).to_degrees()
        );

        // 过渡到一半：落在两者之间，谁都不贴着
        let half = resolve(0.5);
        assert!(
            half.angle_between(previous) > 0.12 && half.angle_between(animated) > 0.12,
            "半程应落在两者之间：距限速值 {:.1}°、距动画 {:.1}°",
            half.angle_between(previous).to_degrees(),
            half.angle_between(animated).to_degrees()
        );

        // 过渡完：精确等于动画（支撑期该精确跟随）
        let done = resolve(1.0);
        assert!(
            done.angle_between(animated) < 1e-4,
            "过渡结束后应精确跟随动画，实际差 {:.2}°",
            done.angle_between(animated).to_degrees()
        );
    }

    /// 夹紧有硬上限：压得再深也只抬到 15°，不会把脚掌折成脚背朝天
    #[test]
    fn toe_clamp_is_capped_so_the_foot_never_folds() {
        let ankle = Vec3::new(0.0, 0.10, 0.0);
        let offset = Vec3::new(0.0, -0.08, 0.14);
        let floor = ankle.y - 0.08;
        // 压到几乎竖直：需要的角度远超上限
        let down = Quat::from_rotation_x(0.9);
        let (axis, angle) =
            toe_floor_correction(down, offset, ankle, floor, 0.08, false).expect("应当要抬脚掌");
        assert!(
            angle.abs() <= TOE_CLAMP_MAX + 1e-5,
            "夹紧角 {:.1}° 超过上限 {:.1}°",
            angle.to_degrees(),
            TOE_CLAMP_MAX.to_degrees()
        );
        // 抬完还在脚掌朝下的范围内，没有翻转
        let clamped = Quat::from_axis_angle(axis, angle) * down;
        assert!((clamped * offset).y < 0.0, "前掌被抬到脚背朝上了");
    }

    /// 记录下来的地面如果比「踝高 − 脚掌下沉量」还高，说明它不可信（落地帧还在空中，
    /// 或者当时脚掌还跖屈着）——宁可让它穿一点地，也不能把脚掌往上折
    #[test]
    fn toe_clamp_refuses_an_implausible_floor() {
        let ankle = Vec3::new(0.0, 0.10, 0.0);
        let offset = Vec3::new(0.0, -0.08, 0.14);
        // 偏高的假地面：比踝高扣除下沉量还高 3cm
        let fake_floor = ankle.y - 0.05;
        let down = Quat::from_rotation_x(0.6);
        let toe = ankle + down * offset;
        assert!(toe.y < fake_floor, "构造前提：前掌低于这个假地面");
        assert!(
            toe_floor_correction(down, offset, ankle, fake_floor, 0.08, false).is_none(),
            "这个地面不可信，不应该拿它掰脚掌"
        );
    }

    /// 前掌被动画压到地面以下时，要靠转动脚踝把它抬回地面（只改俯仰，不动水平方位）
    #[test]
    fn toe_pitch_is_clamped_to_the_planted_floor() {
        let ankle = Vec3::new(0.0, 0.10, 0.0);
        let offset = Vec3::new(0.0, -0.08, 0.14); // 前掌在踝前方、下方
        let floor = ankle.y - 0.08; // 踩实时前掌贴地的高度

        // 脚尖往下压：前掌会扎进地里（幅度控制在夹紧上限能修好的范围里）
        let down = Quat::from_rotation_x(0.25);
        let toe = ankle + down * offset;
        assert!(toe.y < floor - 0.03, "构造前提：脚尖确实扎进地里 {toe:?}");

        let (axis, angle) =
            toe_floor_correction(down, offset, ankle, floor, 0.08, false).expect("应当要抬脚掌");
        let clamped = Quat::from_axis_angle(axis, angle) * down;
        let toe = ankle + clamped * offset;
        assert!((toe.y - floor).abs() < 1e-4, "前掌应贴回地面：{}", toe.y);
        // 水平方位不动：只绕「与脚掌水平方向垂直的轴」转
        assert!(toe.z > 1e-3 && toe.x.abs() < 1e-4, "脚掌被转向了：{toe:?}");

        // 已经在地面之上：不需要修正，不无谓地转脚
        let up = Quat::from_rotation_x(-0.2);
        let toe = ankle + up * offset;
        assert!(toe.y > floor);
        assert!(toe_floor_correction(up, offset, ankle, floor, 0.08, false).is_none());
    }

    /// 系统级验证「前掌不穿地」：钉着前掌时把脚掌越压越低，前掌也不许沉到地面以下
    #[test]
    fn planted_toe_never_sinks_below_the_floor() {
        let mut app = App::new();
        app.insert_resource(FootIkSettings::default());
        app.insert_resource(Time::<()>::default());

        let world = app.world_mut();
        // 起点往后放 15cm：小腿前倾会让脚落在骨盆后方，往回挪一点，
        // 让「身体往前蹬出去」这段位移在模型里真的发生
        let root = world
            .spawn((Name::new("Avatar"), Transform::from_xyz(0.0, 0.0, -0.15)))
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
                    toe_drop: 0.0,
                    foot_tilt: 0.0,
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
            scale_leg: 1.0,
            scale_foot: 1.0,
            tilt_calibrated: true,
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
            for entity in [
                root, model, armature, hips, bones.0, bones.1, bones.2, bones.3,
            ] {
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
            world
                .get_mut::<Transform>(left_bones.2)
                .expect("脚")
                .rotation = foot_rot;
            world
                .get_mut::<Transform>(right_bones.2)
                .expect("脚")
                .rotation = foot_rot;
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
        // 起点往后放 15cm：小腿前倾会让脚落在骨盆后方，往回挪一点，
        // 让「身体往前蹬出去」这段位移在模型里真的发生
        let root = world
            .spawn((Name::new("Avatar"), Transform::from_xyz(0.0, 0.0, -0.15)))
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
                    toe_drop: 0.0,
                    foot_tilt: 0.0,
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
            scale_leg: 1.0,
            scale_foot: 1.0,
            tilt_calibrated: true,
        });
        world.entity_mut(model).insert(ik);

        // 动画：小腿预弯 1.0，脚掌先水平踩实，随后逐帧跖屈蹬地（脚尖往地里压）
        let foot_local = |frame: f32| Quat::from_rotation_x(0.12 * frame - 1.0);
        let foot_world_rot = |world: &World| {
            let mut rotation = Quat::IDENTITY;
            for entity in [
                root,
                model,
                armature,
                hips,
                left_bones.0,
                left_bones.1,
                left_bones.2,
            ] {
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
            world
                .get_mut::<Transform>(hips)
                .expect("hips")
                .translation
                .z += 0.004;
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

    /// 脚离地那一刻，前掌夹紧可能「不再触发」或者「够不着地面」——两种情况都不能
    /// 把上一帧施加的那十几度当帧丢掉，否则就是脚掌闪一下。
    #[test]
    fn toe_clamp_does_not_vanish_when_the_foot_leaves_the_floor() {
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
                    toe_drop: 0.0,
                    foot_tilt: 0.0,
                },
                (upper, lower, foot),
            )
        };
        let (left, left_bones) = make_leg(world, "Left", 0.08);
        let (right, _) = make_leg(world, "Right", -0.08);

        let mut ik = FootIk::default();
        ik.contacts = [1.0, 1.0];
        ik.blend = 1.0;
        ik.rig = Some(FootRig {
            spine: vec![root, model, armature, hips],
            legs: [left, right],
            scale_leg: 1.0,
            scale_foot: 1.0,
            tilt_calibrated: true,
        });
        world.entity_mut(model).insert(ik);

        let foot_world_rot = |world: &World| {
            let mut rotation = Quat::IDENTITY;
            for entity in [
                root,
                model,
                armature,
                hips,
                left_bones.0,
                left_bones.1,
                left_bones.2,
            ] {
                rotation *= world.get::<Transform>(entity).expect("骨骼").rotation;
            }
            rotation
        };

        // 0..6 平踩（记下地面高度）；6..14 跖屈把脚尖压进地里（夹紧开始施加十几度）；
        // 14 抬脚、整条腿往上抬：脚尖离地、直到「怎么抬都够不着地面」，夹紧必须放弃
        let mut previous: Option<Quat> = None;
        let mut worst = (0.0f32, 0u32);
        for frame in 0..45u32 {
            let world = app.world_mut();
            let pitch = if frame < 6 {
                0.0
            } else {
                ((frame - 6) as f32 * 0.06).min(0.55)
            };
            for (entity, rotation) in [
                (left_bones.0, Quat::IDENTITY),
                (left_bones.1, bend),
                (left_bones.2, Quat::from_rotation_x(pitch)),
            ] {
                world.get_mut::<Transform>(entity).expect("腿骨").rotation = rotation;
            }
            if frame == 14 {
                // 抬脚：交给动画，之后动画把整条腿抬起来（脚踝目标跟着上升）
                world.get_mut::<FootIk>(model).expect("FootIk").contacts = [0.0, 0.0];
            }
            if frame >= 14 {
                world
                    .get_mut::<Transform>(hips)
                    .expect("hips")
                    .translation
                    .y += 0.025;
            }
            world
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
            world
                .run_system_once(solve_foot_ik)
                .expect("脚部 IK 系统跑不起来");

            let rotation = foot_world_rot(world);
            if let Some(previous) = previous {
                let step = previous.angle_between(rotation).to_degrees();
                if step > worst.0 {
                    worst = (step, frame);
                }
            }
            previous = Some(rotation);
        }
        assert!(
            worst.0 < 8.0,
            "第 {} 帧脚掌单帧转了 {:.1}°（夹紧在离开地面时被整段撤掉）",
            worst.1,
            worst.0
        );
    }

    /// 摆动期（锁脚权重为 0）也必须滤踝角。
    ///
    /// 这是一条专门的回归测试：脚掌朝向那段原来被挡在「weight 为 0 就 continue」
    /// 后面，抬脚后整个摆动期 weight 恰好是 0，于是脚掌完全跟动画、`foot_secs`
    /// 怎么调都没反应，而「脚掌闪一下」正是发生在这整段里。
    #[test]
    fn ankle_jump_in_swing_is_low_passed() {
        let mut app = App::new();
        app.insert_resource(FootIkSettings {
            foot_max_rate: 360.0,
            ..Default::default()
        });
        app.insert_resource(Time::<()>::default());

        let world = app.world_mut();
        // 起点往后放 15cm：小腿前倾会让脚落在骨盆后方，往回挪一点，
        // 让「身体往前蹬出去」这段位移在模型里真的发生
        let root = world
            .spawn((Name::new("Avatar"), Transform::from_xyz(0.0, 0.0, -0.15)))
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
                    toe_drop: 0.0,
                    foot_tilt: 0.0,
                },
                (upper, lower, foot),
            )
        };
        let (left, left_bones) = make_leg(world, "Left", 0.08);
        let (right, _) = make_leg(world, "Right", -0.08);

        let mut ik = FootIk::default();
        // 摆动期：支撑 0 → 锁脚权重 0 → 原来整条腿被 continue 掉
        ik.contacts = [0.0, 0.0];
        ik.blend = 1.0;
        ik.rig = Some(FootRig {
            spine: vec![root, model, armature, hips],
            legs: [left, right],
            scale_leg: 1.0,
            scale_foot: 1.0,
            tilt_calibrated: true,
        });
        world.entity_mut(model).insert(ik);

        let foot_world_rot = |world: &World| {
            let mut rotation = Quat::IDENTITY;
            for entity in [
                root,
                model,
                armature,
                hips,
                left_bones.0,
                left_bones.1,
                left_bones.2,
            ] {
                rotation *= world.get::<Transform>(entity).expect("骨骼").rotation;
            }
            rotation
        };

        let mut previous: Option<Quat> = None;
        let mut max_step = 0.0f32;
        for frame in 0..45u32 {
            let world = app.world_mut();
            // 第 20 帧踝关节突然跖屈 40°（混合空间换剪辑时就是这样跳的）
            let pitch = if frame < 20 { 0.0 } else { 0.7 };
            world
                .get_mut::<Transform>(left_bones.2)
                .expect("脚")
                .rotation = Quat::from_rotation_x(pitch);
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
            max_step <= 6.5f32.to_radians(),
            "摆动期踝关节跳 40°，脚掌当帧就转了 {:.1}°（限速没有生效）",
            max_step.to_degrees()
        );
    }

    /// `toe_drop` 必须从骨架量出来：它是「踝到前掌的竖直距离」，
    /// 锁脚拿它把踝高换算成真正的地面高度（脚掌水平时的踝高差，
    /// 与动画把脚掌掰成什么角度无关）
    #[test]
    fn toe_drop_is_measured_from_the_skeleton() {
        let mut app = App::new();
        let world = app.world_mut();
        // 起点往后放 15cm：小腿前倾会让脚落在骨盆后方，往回挪一点，
        // 让「身体往前蹬出去」这段位移在模型里真的发生
        let root = world
            .spawn((Name::new("Avatar"), Transform::from_xyz(0.0, 0.0, -0.15)))
            .id();
        let model = world
            .spawn((Name::new("Model"), Transform::default(), ChildOf(root)))
            .id();
        let hips = world
            .spawn((
                Name::new("mixamorig:Hips"),
                Transform::from_xyz(0.0, 1.0, 0.0),
                ChildOf(model),
            ))
            .id();
        for side in ["Left", "Right"] {
            let upper = world
                .spawn((
                    Name::new(format!("mixamorig:{side}UpLeg")),
                    Transform::from_xyz(0.08, 0.0, 0.0),
                    ChildOf(hips),
                ))
                .id();
            let lower = world
                .spawn((
                    Name::new(format!("mixamorig:{side}Leg")),
                    Transform::from_xyz(0.0, -0.35, 0.0),
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
            world.spawn((
                Name::new(format!("mixamorig:{side}ToeBase")),
                Transform::from_xyz(0.0, -0.08, 0.14),
                ChildOf(foot),
            ));
        }
        app.world_mut()
            .run_system_once(
                move |children: Query<&Children>,
                      names: Query<&Name>,
                      parents: Query<&ChildOf>,
                      transforms: Query<&Transform>| {
                    let rig = build_rig(model, &children, &names, &parents, &transforms)
                        .expect("骨架应当能找到");
                    for leg in rig.legs {
                        assert!(
                            (leg.toe_drop - 0.08).abs() < 1e-6,
                            "踝到前掌的竖直距离应量到 0.08，实际 {}",
                            leg.toe_drop
                        );
                        // 前掌骨静止俯角：8cm 下、14cm 前 → 29.7°
                        let tilt = (0.08f32).atan2(0.14).to_degrees();
                        assert!(
                            (leg.foot_tilt - tilt).abs() < 0.1,
                            "前掌骨静止俯角应量到 {tilt:.1}°，实际 {:.1}°",
                            leg.foot_tilt
                        );
                    }
                    // 骨骼尺寸：腿 0.35+0.35，脚 √(0.08²+0.14²)
                    let expected = 0.7 / REFERENCE_LEG_LENGTH;
                    assert!(
                        (rig.scale_leg - expected).abs() < 0.01,
                        "腿长换算应 {expected:.3}，实际 {:.3}",
                        rig.scale_leg
                    );
                    let expected = 0.161_245 / REFERENCE_FOOT_LENGTH;
                    assert!(
                        (rig.scale_foot - expected).abs() < 0.01,
                        "脚长换算应 {expected:.3}，实际 {:.3}",
                        rig.scale_foot
                    );
                },
            )
            .expect("跑不起来");
    }

    #[test]
    fn approach_ramps_and_clamps() {
        assert!((approach(0.0, 1.0, 0.25) - 0.25).abs() < 1e-6);
        assert!((approach(0.9, 1.0, 0.25) - 1.0).abs() < 1e-6);
        assert!((approach(1.0, 0.0, 0.25) - 0.75).abs() < 1e-6);
    }

    /// 抬跟只补「动画没蹬够」的那部分，而且必须绕前掌转：
    /// 踝抬起来、前掌不动、脚掌俯角落到目标。
    /// 踝角口径：φ = 90 + P − L。小腿前倾越多，同样的地面俯角对应的踝角越小 —
    /// 这就是「脚掌俯 55° 却看着折叠」的算术来源
    #[test]
    fn ankle_angle_accounts_for_the_shank_lean() {
        let ankle = Vec3::ZERO;
        // 前掌在前方 16cm、低 8cm → 地面俯角 26.6°
        let toe = Vec3::new(0.0, -0.08, 0.16);
        let pitch = foot_pitch_deg(toe, ankle);

        // 小腿竖直：踝角 = 90 + 26.6
        let knee = Vec3::new(0.0, 0.35, 0.0);
        let lean = shank_lean_deg(knee, ankle, toe);
        assert!(lean.abs() < 0.5, "竖直小腿的前倾角应为 0，实际 {lean:.1}");
        assert!((90.0 + pitch - lean - 116.6).abs() < 0.5);

        // 膝在踝前方 20.7cm（小腿长 36.4cm）：前倾 34.6°，踝角掉到 82°
        let knee = Vec3::new(0.0, 0.30, 0.207);
        let lean = shank_lean_deg(knee, ankle, toe);
        assert!(
            (lean - 34.6).abs() < 0.5,
            "前倾角应为 34.6°，实际 {lean:.1}"
        );
        assert!(
            (90.0 + pitch - lean - 82.0).abs() < 0.5,
            "同样的脚掌在小腿前倾时只剩 82° 的踝角"
        );

        // 要蹬到 140° 的踝角，脚掌得俯 140 − 90 + 34.6 ≈ 85°（这就是「折叠」要补的量）
        assert!((140.0 - 90.0 + lean - 84.6).abs() < 0.5);
    }

    #[test]
    fn heel_off_lift_pivots_on_the_toe_and_reaches_the_target() {
        // 站姿：踝高 10cm，前掌在前方 16cm、低 4cm（俯角约 14°）
        let ankle = Vec3::new(0.0, 0.10, 0.0);
        let toe = Vec3::new(0.0, 0.06, 0.16);
        let pitch_now = foot_pitch_deg(toe, ankle);
        assert!((pitch_now - 14.0).abs() < 0.5, "初始俯角 {pitch_now:.1}°");

        // 补到位：俯角到 55°，踝明显抬高，前掌水平方位不变
        let (rot, delta, lifted) =
            heel_off_lift(toe, ankle, 55.0, 0.0, 1.0).expect("差 40°，应当补抬");
        assert!((lifted - 55.0).abs() < 0.2, "抬完的俯角 {lifted:.1}°");
        let ankle_after = ankle + delta;
        assert!(
            ankle_after.y > ankle.y + 0.05,
            "踝要跟着抬起来，实际抬了 {:.3}m",
            ankle_after.y - ankle.y
        );
        // 旋转和位移是一件事：把脚掌向量转过去，前掌正好还在原地
        let foot = toe - ankle;
        let lifted_toe = ankle_after + rot * foot;
        assert!(
            (lifted_toe - toe).length() < 1e-5,
            "前掌被带走了：{lifted_toe:?}（应在 {toe:?}）"
        );

        // 进度 0.5 → 补一半；0 → 不动
        let (_, _, half) = heel_off_lift(toe, ankle, 55.0, 0.0, 0.5).expect("半程应当补");
        assert!(
            (half - (pitch_now + (55.0 - pitch_now) * 0.5)).abs() < 0.2,
            "半程俯角 {half:.1}°"
        );
        assert!(heel_off_lift(toe, ankle, 55.0, 0.0, 0.0).is_none());

        // 动画自己已经蹬得更狠：不往回掰
        let steeper = Vec3::new(0.0, -0.10, 0.12); // 前掌比踝低 20cm → 约 59°
        let pitch_steep = foot_pitch_deg(steeper, ankle);
        assert!(
            pitch_steep > 55.0,
            "构造一个更陡的姿态，实际 {pitch_steep:.1}°"
        );
        assert!(
            heel_off_lift(steeper, ankle, 55.0, 0.0, 1.0).is_none(),
            "动画已经比目标更陡，不该往回削"
        );
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
                      parents: Query<&ChildOf>,
                      transforms: Query<&Transform>| {
                    let rig = build_rig(root, &children, &names, &parents, &transforms)
                        .expect("骨架应当能找到");
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

    // ---- 偏航锁：踩实期间脚掌的世界朝向必须钉住 ----

    /// 脚掌朝向与 `Quat::from_rotation_y` 同口径：绕 +Y 转 θ，读数就加 θ
    #[test]
    fn foot_yaw_matches_the_rotation_convention() {
        let ankle = Vec3::ZERO;
        assert!((foot_yaw(ankle, Vec3::new(0.0, 0.0, 1.0)).unwrap()).abs() < 1e-5);
        for deg in [-120.0_f32, -45.0, 30.0, 90.0, 170.0] {
            let rot = Quat::from_rotation_y(deg.to_radians());
            let toe = rot * Vec3::new(0.0, -0.1, 1.0);
            let yaw = foot_yaw(ankle, toe).unwrap();
            assert!(
                (wrap_pi(yaw - deg.to_radians()).abs()) < 1e-4,
                "deg={deg} yaw={}",
                yaw.to_degrees()
            );
        }
        // 几乎竖直的脚掌量不到朝向（水平分量不足一半，方位角会被俯仰搅乱）
        assert!(foot_yaw(ankle, Vec3::new(0.0, 1.0, 0.0)).is_none());
        assert!(foot_yaw(ankle, Vec3::new(0.0, 0.9, 0.2)).is_none());
    }

    /// 踩实不动脚、身体按 90°/s 转：脚掌的世界朝向必须原地不动，
    /// 补偿量线性长起来，超过 `foot_yaw_limit` 之后才跟着身体撇
    #[test]
    fn planted_foot_keeps_its_world_yaw() {
        let dt = 1.0 / 60.0;
        let limit = 30.0_f32;
        let mut lock = FootYawLock::default();
        let mut body = 0.0_f32;

        for frame in 0..90 {
            // 身体转了之后，动画给的脚掌朝向就跟着 body 的偏航走 —— 这正是 bug 的来源
            let applied = lock.update(Some(body), true, frame == 0, limit, limit, dt);
            let world_deg = wrap_pi(body + applied).to_degrees();
            let body_deg = body.to_degrees();
            // 撑得住：世界朝向钉死在 0；撑不住：跟着身体撇，但落后正好一个上限
            let want_world = if body_deg <= limit {
                0.0
            } else {
                body_deg - limit
            };
            assert!(
                (world_deg - want_world).abs() < 1e-3,
                "body={body_deg:.1}° world={world_deg:.2}° applied={:.2}°",
                applied.to_degrees()
            );
            assert!(
                applied.to_degrees() >= -limit - 1e-3,
                "补偿超了上限：{:.2}°",
                applied.to_degrees()
            );
            body += 1.5_f32.to_radians();
        }
        assert!(
            (lock.applied.to_degrees() + limit).abs() < 1e-3,
            "转了 135° 之后应当压在 -{limit}° 上限上，实际 {:.2}°",
            lock.applied.to_degrees()
        );
    }

    /// 抬脚之后补偿按指数放掉：单调、连续，一个摆动期内基本归零
    #[test]
    fn yaw_lock_releases_after_lift() {
        let dt = 1.0 / 60.0;
        let mut lock = FootYawLock::default();
        // 踩实时脚掌朝 0，随后身体慢慢把它带到 0.5 rad（1.7°/帧，和 90°/s 的实速同量级），
        // 补偿应当恰好是 -0.5；一步跳 34° 会被 `YAW_JUMP_MAX` 当成量错
        lock.update(Some(0.0), true, true, 30.0, 30.0, dt);
        let drift = 0.5_f32;
        let mut yaw = 0.0f32;
        while yaw < drift {
            yaw = (yaw + 0.03).min(drift);
            lock.update(Some(yaw), true, false, 30.0, 30.0, dt);
        }
        let held = lock.applied;
        assert!((held + drift).abs() < 1e-4, "基准没锁上：{held}");

        let mut previous = held;
        for _ in 0..30 {
            let now = lock.update(Some(drift), false, false, 30.0, 30.0, dt);
            assert!(now.abs() <= previous.abs() + 1e-6, "放掉的过程不能变大");
            assert!(previous.abs() - now.abs() < 0.15, "单帧不能跳");
            previous = now;
        }
        // 0.5s 后应当基本归零（时间常数 0.1s）
        assert!(previous.to_degrees().abs() < 0.5, "没放干净：{previous:?}");
        assert!(lock.reference.is_none(), "离地后基准应当作废");
    }

    /// 还没放完就重新踩实：补偿必须接得上，不能一帧跳回去
    #[test]
    fn replanting_keeps_the_compensation_continuous() {
        let dt = 1.0 / 60.0;
        let mut lock = FootYawLock::default();
        // 踩实时脚掌朝 0，随后动画逐帧把它带到 0.6 rad（34°），补偿顶到 -30°
        lock.update(Some(0.0), true, true, 30.0, 30.0, dt);
        let yaw = 0.6_f32;
        let mut now = 0.0f32;
        while now < yaw {
            now = (now + 0.03).min(yaw);
            lock.update(Some(now), true, false, 30.0, 30.0, dt);
        }
        assert!((lock.applied.to_degrees().abs() - 30.0).abs() < 1e-3);
        // 空中 2 帧：只放掉一点点
        for _ in 0..2 {
            lock.update(Some(yaw), false, false, 30.0, 30.0, dt);
        }
        let before = lock.applied;
        assert!(before.to_degrees().abs() > 20.0, "2 帧不该放光");

        // 重新踩实：基准按「当前实际所在」记，补偿不变
        let after = lock.update(Some(yaw), true, true, 30.0, 30.0, dt);
        assert!(
            (after - before).abs() < 1e-4,
            "重新踩实跳了：{:.3}° → {:.3}°",
            before.to_degrees(),
            after.to_degrees()
        );
    }

    /// 脚掌被动画掰到竖直以上时方位角会翻过去：读数作废，补偿冻结在原地
    #[test]
    fn flipped_foot_yaw_freezes_the_lock() {
        let dt = 1.0 / 60.0;
        let mut lock = FootYawLock::default();
        // 先锁住，再让动画把脚掌带到 20°，补偿攒到 -20°
        lock.update(Some(0.0), true, true, 30.0, 30.0, dt);
        let drift = 0.35_f32;
        for _ in 0..10 {
            lock.update(Some(drift), true, false, 30.0, 30.0, dt);
        }
        let held = lock.applied;
        assert!((held + drift).abs() < 1e-3, "基准没锁上：{held}");

        // 量不到（脚掌竖起来了）：冻结，不许清零
        let now = lock.update(None, true, false, 30.0, 30.0, dt);
        assert!(
            (now - held).abs() < 1e-6,
            "量不到时应当冻结，实际 {:.2}° → {:.2}°",
            held.to_degrees(),
            now.to_degrees()
        );

        // 读数突然翻 180°：同样算量错，不能采纳
        let flipped = wrap_pi(drift + std::f32::consts::PI);
        let now = lock.update(Some(flipped), true, false, 30.0, 30.0, dt);
        assert!(
            (now - held).abs() < 1e-6,
            "翻过去的读数被采纳了：{:.2}°",
            now.to_degrees()
        );
    }

    /// `foot_yaw_limit = 0` 关掉这条锁，行为退回「脚掌跟着身体转」
    #[test]
    fn zero_limit_disables_the_yaw_lock() {
        let dt = 1.0 / 60.0;
        let mut lock = FootYawLock::default();
        lock.update(Some(0.0), true, true, 0.0, 0.0, dt);
        for _ in 0..20 {
            let applied = lock.update(Some(0.4), true, false, 0.0, 0.0, dt);
            assert_eq!(applied, 0.0);
        }
        assert_eq!(lock.applied, 0.0);
    }
}
