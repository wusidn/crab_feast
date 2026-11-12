//! 支撑期锁脚（anti-slide）：把「踩实」那段时间的脚踝世界位置钉住。
//!
//! 混合空间给的是**相对身体**的脚部轨迹。身体朝向与真实速度方向不一致时
//! （转身、侧移、起停），脚在地面上会多走一小段，看上去就是滑步。
//! 这里在支撑期记下脚踝的世界位置，之后每帧把它当成 IK 目标：
//! 髋往哪走、身体怎么转都由腿自己吸收，脚不动。
//!
//! 三条设计约束，对应「高频开关」与「联机所有可见角色」：
//! - **纯状态**：一只脚一份，不共享、不依赖 Ressource，联机下每个角色互不干扰；
//! - **零分配、零历史**：开关当帧生效；关闭时清空锚点，重新打开后从下一次支撑重新贴地，
//!   不会因为「关了一会儿」而拿旧锚点把人往地里拽；
//! - **有上限的修正**：锚点偏离超过 `max_correction` 就跟着脚走。腿不会被拉到极限，
//!   极端情况下退化成「少滑一点」，而不是反关节或穿模；
//! - **强度是 smoothstep 形状的进度，不是斜坡、也不是二阶跟随器**：踩实/抬脚时施加的
//!   比例 = `smoothstep(进度)`，位置、速度、**加速度**三者在两端都连续。
//!   线性斜坡只在位置连续（速度是折线，60fps 下 40ms 的 engage 只有两三帧）；
//!   指数衰减更糟，第一帧就吃掉四分之一残差，起点速度最大；
//!   临界阻尼二阶跟随器速度连续了，但加速度在起步帧是 `A·ω²` 的台阶 ——
//!   6cm 残差 / 0.18s 就有 ~30m/s²，抬脚那一瞬间就是「被拽了一下」。
//!   smoothstep 把同样的位移摊成 `6A/T²`：0.26s 下只有 5m/s²，肉眼看不出突变。

use bevy::math::Vec3;

/// 水平方向夹角/位移的极小阈值（米）
const MIN_OFFSET: f32 = 1e-5;

/// 强度小到这个值就认为锁脚已经退干净：清零后不再每帧多解一次 IK
const MIN_STRENGTH: f32 = 1e-4;

/// 「抬跟」进度里，承重点从踝滑到前掌所占的比例。
///
/// 剩下的 `1 - ROLL_SPAN` 留给脚掌俯仰本身（由调用方按同样的进度插值）：
/// 承重点还没接到前掌上就先把脚掌掰起来，前掌会被压进地里再被「不穿地」夹回来，
/// 看起来就是脚掌先翘一下、再放平。先迁承重点、后补俯仰，两者各自单调、无回头。
///
/// 0.3 而不是 0.4：承重点迁移只是**为了能掰脚掌**的必要前置，它本身不被看见。
/// 占掉四成的话，抬跟触发之后还要等 0.1s（走路 6 帧）才看得见脚跟起来，
/// 观感上又是一次「晚」；压到三成后俯仰拿到七成时间，那段更像蹬出去。
/// 再往下压就得给迁移单独限速了 —— 它是一段 2~4cm 的踝位移，一两帧走完会滑
pub const ROLL_SPAN: f32 = 0.3;

/// 抬跟至少要摊多久（秒）。再短就是一帧到位，脚跟等于「弹」起来的
const MIN_HEEL_RAMP: f32 = 0.10;

/// 抬跟要在支撑期结束前的哪个位置走满：按上一段支撑期估出来的长度 × 它。
///
/// 0.75 的意思是「离地前 25% 就该已经蹬到位」——最后那段停住不动的、完全伸展的
/// 脚掌就是「蹬住了」的观感；如果一直爬到离地那一帧才到位，动作和离地同时结束，
/// 看起来就是被身体带着走。走路时剩余 15% ≈ 90ms、跑步 ≈ 35ms，都够看出一个
/// 站住的拍子。0.85 时只剩 15%，观感上蹬地还是压在离地那一瞬
const HEEL_RAMP_OF_STANCE: f32 = 0.75;

/// 支撑期长度估计的平滑系数：新测到的一段和旧估计按 1:x 混合。
/// 0.35 让换速度（走 → 跑）在两步之内跟上，又不会让单次异常步幅把节奏带偏
const STANCE_EMA: f32 = 0.35;

/// 太短的「支撑」不算数（中途抖了一下又重新踩实）：短于它不参与估计
const MIN_STANCE_SAMPLE: f32 = 0.08;

/// 锁脚参数。由全局设置构造，逐帧传给 [`FootLock::update`]，所以改参数立刻生效。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FootLockConfig {
    /// 锚点最多允许偏离脚踝多少米（水平方向）；超过就让锚点跟着脚走
    pub max_correction: f32,
    /// 支撑权重升到它才判定为踩实（防止抬脚/落地边缘反复触发）
    pub plant_enter: f32,
    /// 支撑权重降到它才判定为抬脚
    pub plant_exit: f32,
    /// 解除锁定后锁脚强度从 1 落回 0 的过渡时长（秒）。二阶过渡：首尾速度都是 0，
    /// 比指数衰减慢一点，但松开那一帧没有速度台阶。0 = 立刻归位（会看到跳变）
    pub release_secs: f32,
    /// 刚判定踩实时，锁脚强度从 0 升到 1 的过渡时长（秒）。二阶过渡，
    /// 首尾速度都是 0，所以脚不会被一帧掐断。0 = 一踩实就满强度（会「顿」）
    pub engage_secs: f32,
    /// 支撑期内，动画把脚跟抬起多少米之后改成「钉前掌」。
    /// 0 = 从踩实第一帧就钉前掌（脚跟会过早抬起）
    pub toe_pivot_rise: f32,
    /// 重心（骨盆）沿脚掌朝向越过锚点多远（米）才开始抬脚跟。
    ///
    /// 真实步态里脚跟在**重心压过着地脚之后**才抬；动画自己的抬跟时刻常偏晚，
    /// 光等动画抬踝，锁脚就会把脚按平到接近离地那一瞬。0 = 重心正好压到脚踝上，
    /// 负值 = 提前抬（-0.05 ≈ 重心压到脚掌中段，也就是解剖上的 heel-off 时刻）。
    pub heel_off_lead: f32,
    /// 抬跟推进到「完全抬跟」的**最长**时长（秒）。前 [`ROLL_SPAN`] 段迁承重点，
    /// 后段交给脚掌俯仰补齐目标角。
    ///
    /// 实际用的是「上一段支撑期估出的剩余时间 × 0.85」，只有它比这个值更短时才压缩：
    /// 跑步支撑期只有 0.25s，固定 0.30s 的斜坡根本爬不满，俯角到离地也没到位
    pub heel_off_secs: f32,
    /// 触发前至少要踩实多久（秒）：刚落地的几帧重心可能已经在踝上方（短步幅、
    /// 刹停），这时不该立刻抬跟
    pub heel_off_min_secs: f32,
}

impl Default for FootLockConfig {
    fn default() -> Self {
        Self {
            // 腿长约 0.9m，横向拉 22cm 还有余量；再大就会明显看出膝盖被拽直
            max_correction: 0.22,
            plant_enter: 0.55,
            plant_exit: 0.25,
            // 260ms ≈ 一个摆动期：抬脚后残差要在「脚离地的那段时间」里还完。
            // 时间给足比给形状更重要 —— 残差越大（转身时能到 20cm），越需要整段摆动去摊
            release_secs: 0.26,
            // 100ms 走到 90%：落地不是「咬住」，而是一段有形的加压
            engage_secs: 0.10,
            // 3cm：正常抬脚跟到 3cm 时前掌是承重点，再往上前掌就该接管了
            toe_pivot_rise: 0.03,
            // -8cm：重心还在踝后方 8cm（脚掌的一半处）就起跟。解剖上真正的
            // heel-off 就发生在重心过踝之前；-5cm 实测还晚，-8cm 更接近真实步态
            heel_off_lead: -0.08,
            // 300ms：走路剩余支撑约 0.3s，正好摊满；跑步由自适应缩短（见字段文档）
            heel_off_secs: 0.30,
            // 80ms ≈ 5 帧：盖住落地时的重心抖动，又不至于把正常的起跟推后
            heel_off_min_secs: 0.08,
        }
    }
}

impl FootLockConfig {
    /// 夹到合法区间：`plant_exit < plant_enter`、修正量非负
    pub fn sanitized(mut self) -> Self {
        self.max_correction = self.max_correction.max(0.0);
        self.plant_enter = self.plant_enter.clamp(0.0, 1.0);
        self.plant_exit = self.plant_exit.clamp(0.0, self.plant_enter);
        self.release_secs = self.release_secs.max(0.0);
        self.engage_secs = self.engage_secs.max(0.0);
        self.toe_pivot_rise = self.toe_pivot_rise.max(0.0);
        self.heel_off_lead = self.heel_off_lead.clamp(-0.2, 0.2);
        self.heel_off_secs = self.heel_off_secs.max(1e-3);
        self.heel_off_min_secs = self.heel_off_min_secs.max(0.0);
        self
    }
}

/// 一次锁脚求解需要的两处世界坐标。
///
/// - `ankle`：脚踝（脚跟侧承重点）；
/// - `toe`：前掌关节（`*ToeBase` 的世界位置），骨架里没有就传 `None`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FootContact {
    pub ankle: Vec3,
    pub toe: Option<Vec3>,
    /// 踝到前掌的竖直距离（静止姿态量的，米）。用它把「踩实那一帧的踝高」换算成
    /// 真正的地面高度：落地那帧如果脚跟先着地、脚掌还背屈着，前掌位置是偏高的，
    /// 拿它当地面，脚掌就永远压不下去（看起来落地后翘着）。
    /// 0 = 没量过，退回旧行为（用当帧前掌高度）
    pub toe_drop: f32,
    /// 射线量到的真实地面高度（世界 y）。有它就用它，没有才靠推断——
    /// 推断在「落地那帧脚还在空中」「脚掌跖屈/背屈」时都会偏，
    /// 而真实地面在爬楼梯、斜坡这种复杂地形上是唯一可信的来源
    pub floor: Option<f32>,
    /// 重心（骨盆）的世界位置，用来判断「重心越过着地脚」这个抬跟时机。
    ///
    /// 只取水平分量，沿踩实那一刻的脚掌朝向投影：投影为正且超过
    /// [`FootLockConfig::heel_off_lead`] 就认为重心已经压过着地脚，开始抬脚跟。
    /// `None`（没有身体信息）时退回「看动画抬踝」的旧触发。
    pub body: Option<Vec3>,
}

impl FootContact {
    pub fn ankle_only(ankle: Vec3) -> Self {
        Self {
            ankle,
            toe: None,
            toe_drop: 0.0,
            floor: None,
            body: None,
        }
    }
}

/// 单只脚的锁脚状态。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FootLock {
    /// 支撑期里脚踝的世界位置；`None` = 没踩实，完全交给动画
    anchor: Option<Vec3>,
    /// 是否处于支撑期（带迟滞）
    planted: bool,
    /// 还没还回去的修正量（世界空间）。踩实时就是「锚点 − 脚踝」，
    /// 解除锁定后按时间衰减到 0，抬脚不会看到一脚跳回去。
    residual: Vec3,
    /// 本段支撑期施加的锁脚强度（0..1）= `smoothstep(progress)`。
    /// 形状函数保证位置/速度/加速度都连续，脚不会被「一帧掐断」或「一帧拽回」
    strength: f32,
    /// 强度的线性进度（0..1）：按 1/`secs` 匀速推进，形状完全交给 smoothstep。
    /// 抬脚中途又重新踩实时，进度原路返回，不会跳
    progress: f32,
    /// 承重点沿脚掌滑动的比例：0 = 钉脚踝（脚是平的），1 = 钉前掌（绕前掌转）
    roll: f32,
    /// 抬跟进度 0..1：重心越过着地脚（或动画自己抬踝）后单调推进。
    /// 承重点滑动只吃它的前 [`ROLL_SPAN`]，脚掌俯仰吃剩下的部分
    heel: f32,
    /// 踩实那一刻脚掌的水平朝向（单位向量，踝 → 前掌）。重心越过判定沿它投影
    heel_dir: Vec3,
    /// 本段支撑期已经踩了多久（秒），给抬跟触发一个最小支撑时长，
    /// 也是「还剩多少时间」的基准
    stance_secs: f32,
    /// 上一段支撑期有多长（秒，指数平滑）。抬跟的斜坡按它估的剩余时间压缩，
    /// 所以走→跑的换挡两步之内就能跟上；0 = 还没测过，退回配置里的上限
    stance_est: f32,
    /// 本段抬跟摊多长（秒），触发那一帧按 `stance_est` 定下来就不再变：
    /// 逐帧重算会让同一段支撑里的进度忽快忽慢（像被拽了一下）
    ramp_secs: f32,
    /// 踩实那一帧的踝高：动画把踝抬高说明该抬脚跟了
    ankle_base: f32,
    /// 踩实那一帧「前掌 − 踝」的向量（脚掌朝向）。没有前掌骨时为零
    toe_vec: Vec3,
    /// 踩实那一帧的**地面高度**（世界 y）。抬脚后保留，供朝向夹紧淡出用。
    /// 由「踝高 − 踝到前掌的竖直距离」算，不是当帧前掌位置——见 `FootContact::toe_drop`
    toe_floor: Option<f32>,
    /// `toe_floor` 是射线量到的（true）还是推断的（false）。推断值可能是假的，
    /// 夹紧那边要据此决定要不要相信它
    toe_floor_probed: bool,
    /// 朝向夹紧的剩余强度（0..1），跟 [`FootLock::strength`] 同步：
    /// 少了它，「前掌不穿地」的转动会在抬脚那一帧被整段撤掉，脚掌一帧翻十几度
    presence: f32,
}

impl FootLock {
    pub fn reset(&mut self) {
        self.anchor = None;
        self.planted = false;
        self.residual = Vec3::ZERO;
        self.strength = 0.0;
        self.progress = 0.0;
        self.roll = 0.0;
        self.heel = 0.0;
        self.heel_dir = Vec3::ZERO;
        self.stance_secs = 0.0;
        self.stance_est = 0.0;
        self.ramp_secs = 0.0;
        self.ankle_base = 0.0;
        self.toe_vec = Vec3::ZERO;
        self.toe_floor = None;
        self.toe_floor_probed = false;
        self.presence = 0.0;
    }

    /// 本段抬跟要摊多长（秒）：触发那一帧按「上一段支撑长度 − 已经踩的时间」
    /// 的 85% 定，夹在 [`MIN_HEEL_RAMP`] 与配置上限之间。
    ///
    /// 这一条是「蹬地时机」的骨头：固定的 0.30s 斜坡放在跑步的 0.25s 支撑期里，
    /// 重心越过脚之后只剩 0.10s，俯角根本爬不到目标，脚就是平的、被身体带着走。
    /// 按实测的支撑长度往前推，走路摊得慢、跑步摊得快，俯角总在离地前到位
    pub fn heel_ramp_secs(&self) -> f32 {
        self.ramp_secs
    }

    /// 还没还回去的修正量（世界空间，米）。调试面板可以拿它当过冲读数
    pub fn residual(&self) -> Vec3 {
        self.residual
    }

    /// 本帧施加的锁脚强度（0..1）。调试面板可以拿它看加压/放开的过程
    pub fn strength(&self) -> f32 {
        self.strength
    }

    /// 把水平方向拉超过 `max_correction` 的修正量夹回来（腿不会被拽断），
    /// 竖直方向不动：抬脚跟是正常动作，不该被夹。
    fn clamp_residual(residual: Vec3, config: &FootLockConfig) -> Vec3 {
        let horizontal = Vec3::new(residual.x, 0.0, residual.z);
        let distance = horizontal.length();
        if distance > config.max_correction && distance > MIN_OFFSET {
            let scale = config.max_correction / distance;
            Vec3::new(horizontal.x * scale, residual.y, horizontal.z * scale)
        } else {
            residual
        }
    }

    pub fn is_planted(&self) -> bool {
        self.planted
    }

    pub fn anchor(&self) -> Option<Vec3> {
        self.anchor
    }

    /// 承重点沿脚掌滑动的比例（0 = 脚踝，1 = 前掌），调试面板可读
    pub fn roll(&self) -> f32 {
        self.roll
    }

    /// 抬跟进度（0..1，支撑期内只增不减）。调用方拿它的后半段去补脚掌俯仰
    pub fn heel_off(&self) -> f32 {
        self.heel
    }

    /// 踩实时的地面高度（世界 y）。没有踩实或没有前掌骨时是 `None`。
    /// 调用方拿它当「地面高度」，把前掌朝向夹在它之上，脚掌就不会扎进地里；
    /// 反过来，它偏高的话脚掌就永远压不下去（落地后翘着）。
    pub fn toe_floor(&self) -> Option<f32> {
        self.toe_floor
    }

    /// `toe_floor` 是射线量到的真实地面（true），还是从脚踝/前掌推断的（false）
    pub fn toe_floor_is_probed(&self) -> bool {
        self.toe_floor_probed
    }

    /// 朝向夹紧还剩多少强度（0..1）。抬脚后与残差同步衰减，脚掌朝向才不会有台阶
    pub fn presence(&self) -> f32 {
        self.presence
    }

    /// 推进一帧，返回 `(脚踝目标位置, 权重 0..1)`。
    ///
    /// - `contact`：本帧**动画给出**的脚踝与前掌世界位置（还没被 IK 改过）；
    /// - `support`：支撑权重（0 = 完全在空中，1 = 完全踩实）；
    /// - `enabled`：本帧是否允许锁脚；
    /// - `dt`：帧间隔（秒），用于强度过渡与残差过渡，与帧率无关。
    ///
    /// 承重点是沿脚掌滑动的：踩实初期钉**脚踝**（脚是平的）；重心越过着地脚
    /// （或动画自己把脚跟抬到 `toe_pivot_rise`）之后，承重点在 `heel_off_secs`
    /// 的前 [`ROLL_SPAN`] 段里逐渐滑到**前掌**，脚变成绕前掌转 ——
    /// 于是脚跟先离地、前掌最后离地，和真实蹬地顺序一致。
    ///
    /// 锚点 = 踩实时踝的世界位置，加上 `roll × 踩实时的脚向量`：锚点在世界里不动，
    /// 但它是「脚掌上的哪一点」随 `roll` 从踝滑到前掌。所以抬脚跟时脚踝目标是
    /// 平滑上移的，不会出现「一帧从踝锚点跳到前掌锚点」的台阶。
    ///
    /// 修正量分两步施加：`residual`（锚点差，支撑期内逐帧精确跟随）和
    /// `strength`（0..1 的二阶过渡）。踩实/抬脚改变的是 `strength`，它的速度在
    /// 起点与终点都是 0，所以脚的世界速度没有台阶 —— 顿挫感来自速度突变，不是位置跳变。
    pub fn update(
        &mut self,
        contact: FootContact,
        support: f32,
        enabled: bool,
        dt: f32,
        config: &FootLockConfig,
    ) -> (Vec3, f32) {
        let ankle = contact.ankle;
        if !ankle.is_finite() {
            self.reset();
            return (ankle, 0.0);
        }
        let toe = contact.toe.filter(|toe| toe.is_finite());

        let support = if support.is_finite() {
            support.clamp(0.0, 1.0)
        } else {
            0.0
        };

        // 迟滞：进支撑要更实，离开支撑要更松，边缘抖一下不会反复重建锚点。
        // 关闭时锚点一律丢掉（重新打开不能拿旧位置拽人），但这一帧的修正量会被
        // 转成残差继续还，所以关掉本身也没有台阶。
        let was_planted = self.planted;
        let old_anchor = self.anchor;
        if enabled {
            if !self.planted {
                if support >= config.plant_enter {
                    self.planted = true;
                    self.roll = 0.0;
                    self.heel = 0.0;
                    self.stance_secs = 0.0;
                    self.ramp_secs = 0.0;
                    // 踩实的第一帧就把当前脚踝记成锚点：这一帧的残差为 0，不会跳
                    self.anchor = Some(ankle);
                    self.ankle_base = ankle.y;
                    self.toe_vec = toe.map(|toe| toe - ankle).unwrap_or(Vec3::ZERO);
                    self.heel_dir =
                        Vec3::new(self.toe_vec.x, 0.0, self.toe_vec.z).normalize_or_zero();
                    // 地面高度用「踝高 − 踝到前掌的竖直距离」算，不用当帧前掌位置：
                    // 落地那帧脚跟先着地时前掌还在空中，拿它当地面会让脚掌一直翘着
                    // 两种近似各错一边，取更低的那个：
                    // - 当帧前掌位置：脚跟先着地（脚掌背屈）时偏高
                    // - 踝高 − 踝到前掌距：脚尖先着地（脚掌跖屈）时也偏高
                    // 真实地面只会比两者中更低那个更接近脚掌的实际最低点
                    // 射线量到的地面最可信（台阶、斜坡都靠它）；没有物理时退回推断
                    self.toe_floor_probed = contact.floor.is_some_and(|floor| floor.is_finite());
                    self.toe_floor = contact.floor.filter(|floor| floor.is_finite()).or_else(|| {
                        toe.map(|toe| {
                            if contact.toe_drop > 1e-4 {
                                toe.y.min(ankle.y - contact.toe_drop)
                            } else {
                                toe.y
                            }
                        })
                    });
                }
            } else if support <= config.plant_exit {
                self.planted = false;
                self.anchor = None;
                // 这一段支撑量出了准确长度：抬跟的斜坡按它估下一段的剩余时间。
                // 太短的段（中途抖了一下）不算，免得估计被一次误判带跑
                if self.stance_secs >= MIN_STANCE_SAMPLE {
                    self.stance_est = if self.stance_est <= f32::EPSILON {
                        self.stance_secs
                    } else {
                        self.stance_est * (1.0 - STANCE_EMA) + self.stance_secs * STANCE_EMA
                    };
                }
            }
        } else {
            self.planted = false;
            self.anchor = None;
        }

        // 本段支撑已经踩了多久：抬跟触发前要求至少踩实 `heel_off_min_secs`
        if self.planted {
            self.stance_secs += dt.max(0.0);
        } else {
            self.stance_secs = 0.0;
        }

        // 抬跟进度：两个触发源取更早的那个，全程只增不减（倒回去脚踝会当帧下坠）
        // 1) 重心（骨盆）沿脚掌朝向越过锚点：真实步态里脚跟在**重心压过着地脚之后**
        //    才抬。等动画自己抬踝往往已经接近离地那一瞬，锁脚就把脚按平到那时候；
        // 2) 动画自己把踝抬高了 `toe_pivot_rise`：站立、倒退、侧移时重心可能永远
        //    不越过脚掌朝向（投影一直为负），这时只能信动画。
        // 用 smoothstep 而不是线性：线性在阈值处有一个曲率折角，抬脚会「咯噔」一下
        if self.planted && toe.is_some() {
            let start = config.toe_pivot_rise.max(1e-3);
            let rise = (ankle.y - self.ankle_base).max(0.0);
            let from_rise = smoothstep(((rise - start) / start).clamp(0.0, 1.0));

            let mut from_body = self.heel;
            if self.stance_secs >= config.heel_off_min_secs
                && let Some(body) = contact.body.filter(|body| body.is_finite())
                && let Some(anchor) = self.anchor
                && self.heel_dir.length_squared() > 0.5
            {
                let advance =
                    Vec3::new(body.x - anchor.x, 0.0, body.z - anchor.z).dot(self.heel_dir);
                if advance >= config.heel_off_lead {
                    if self.ramp_secs <= f32::EPSILON {
                        self.ramp_secs = self.heel_ramp(config);
                    }
                    from_body = (self.heel + dt.max(0.0) / self.ramp_secs).min(1.0);
                }
            }

            self.heel = self.heel.max(from_body).max(from_rise);
            // 承重点只吃前 ROLL_SPAN 段；剩下那段留给脚掌俯仰（调用方按同一个
            // 进度插值），先接过承重点再掰脚掌，前掌才不会被压进地里
            self.roll = smoothstep((self.heel / ROLL_SPAN).clamp(0.0, 1.0));
        }

        // 还钉着时的修正量：锚点 + roll × 脚向量差 − 当前踝位置。
        // 刚松开的那一帧用松开前的锚点算，修正量原样留在 `residual` 里，
        // 之后不再改动它 —— 放开的形状只由 `strength` 决定，这样首尾速度才能都是 0
        let anchor = self.anchor.or(old_anchor);
        let toe_vec_now = toe.map(|toe| toe - ankle).unwrap_or(self.toe_vec);
        let pinned = match anchor {
            Some(anchor) => FootLock::clamp_residual(
                anchor + self.roll * (self.toe_vec - toe_vec_now) - ankle,
                config,
            ),
            None => Vec3::ZERO,
        };

        if self.planted {
            // 支撑期内精确跟随：这里不能滤波，否则脚会一直「橡皮筋」往后滑
            self.residual = pinned;
        } else if was_planted {
            // 松开那一帧把最新一次锚点差原样留下来，之后不再改动它：
            // 放开的形状只由 `strength` 决定，首尾速度才能都是 0。
            // `release_secs == 0` 表示硬切，只留给 A/B 对比观感，默认不开
            self.residual = if config.release_secs > f32::EPSILON {
                pinned
            } else {
                Vec3::ZERO
            };
        }
        // 已经在空中：残差冻结，交给 `strength` 平滑放掉

        // 强度：进度匀速推进，形状走 smoothstep —— 踩实 → 1、抬脚 → 0，
        // 两端的位置/速度/加速度都是连续的（见模块注释）
        let want = if self.planted { 1.0 } else { 0.0 };
        let secs = if self.planted {
            config.engage_secs
        } else {
            config.release_secs
        };
        self.progress = advance_progress(self.progress, want, dt, secs);
        self.strength = smoothstep(self.progress);

        // 退干净后清零：残差留着会让这一脚每帧多解一次 IK
        if !self.planted && self.strength <= MIN_STRENGTH {
            self.strength = 0.0;
            self.progress = 0.0;
            self.residual = Vec3::ZERO;
        }

        // 前掌朝向夹紧跟强度同步淡出，脚掌朝向不会有台阶
        self.presence = if self.planted {
            1.0
        } else {
            self.strength
        };

        let applied = self.residual * self.strength;
        let weight = if applied.length_squared() > MIN_OFFSET * MIN_OFFSET {
            1.0
        } else {
            0.0
        };
        (ankle + applied, weight)
    }

    /// 本次抬跟的摊开时长：按「估计的支撑长度 − 已经踩的时间」留
    /// [`HEEL_RAMP_OF_STANCE`] 给这一段，夹在 [`MIN_HEEL_RAMP`] 与
    /// [`FootLockConfig::heel_off_secs`] 之间（配置值只当上限）
    fn heel_ramp(&self, config: &FootLockConfig) -> f32 {
        let cap = config.heel_off_secs.max(1e-3);
        if self.stance_est <= f32::EPSILON {
            // 还没走过一段完整的支撑（刚生成、或者一直没离地）：用配置的上限
            return cap;
        }
        let left = (self.stance_est - self.stance_secs).max(0.0);
        (left * HEEL_RAMP_OF_STANCE).clamp(MIN_HEEL_RAMP.min(cap), cap)
    }
}

/// `smoothstep`：`t` 在 0..1 上单调、两端导数为 0，把折角磨平
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 把进度匀速推向 `target`：`secs` 秒走完全程，`secs = 0` 表示当帧到位。
/// 线性进度 + [`smoothstep`] 形状 = 两端位置/速度/加速度全连续的那种过渡。
fn advance_progress(progress: f32, target: f32, dt: f32, secs: f32) -> f32 {
    if dt <= 0.0 {
        return progress;
    }
    if secs <= f32::EPSILON {
        return target;
    }
    let step = (dt / secs).clamp(0.0, 1.0);
    if target > progress {
        (progress + step).min(target)
    } else {
        (progress - step).max(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 60fps 的一帧
    const DT: f32 = 1.0 / 60.0;

    fn config() -> FootLockConfig {
        FootLockConfig::default()
    }

    /// 连续踩实若干帧，把强度建立到满
    fn plant(lock: &mut FootLock, ankle: Vec3, frames: usize, cfg: &FootLockConfig) {
        for _ in 0..frames {
            lock.update(FootContact::ankle_only(ankle), 1.0, true, DT, cfg);
        }
    }

    #[test]
    fn planted_foot_holds_its_anchor() {
        // 只看稳态：关掉咬合斜坡，第二帧就应当完全钉住
        let cfg = FootLockConfig {
            engage_secs: 0.0,
            ..config()
        };
        let mut lock = FootLock::default();
        let ankle = Vec3::new(0.1, 0.05, 0.0);
        let (target, weight) = lock.update(FootContact::ankle_only(ankle), 1.0, true, DT, &cfg);
        assert!(lock.is_planted());
        assert!((target - ankle).length() < 1e-6, "踩实第一帧不应有修正");
        assert_eq!(weight, 0.0, "踩实第一帧还没有修正量，不该白解一次 IK");

        // 身体往前走，脚踝被动画带走了 5cm：目标仍然是锚点
        let moved = ankle + Vec3::new(0.05, 0.0, 0.0);
        let (target, weight) = lock.update(FootContact::ankle_only(moved), 1.0, true, DT, &cfg);
        assert!((target - ankle).length() < 1e-6, "{target:?}");
        assert!((weight - 1.0).abs() < 1e-6);
    }

    #[test]
    fn correction_is_capped_and_anchor_follows() {
        let cfg = FootLockConfig {
            engage_secs: 0.0,
            ..config()
        };
        let mut lock = FootLock::default();
        let ankle = Vec3::ZERO;
        lock.update(FootContact::ankle_only(ankle), 1.0, true, DT, &cfg);

        let far = Vec3::new(0.5, 0.0, 0.0);
        let (target, _) = lock.update(FootContact::ankle_only(far), 1.0, true, DT, &cfg);
        let offset = (target - far).length();
        assert!(
            (offset - config().max_correction).abs() < 1e-4,
            "修正量应被夹在 {}，实际 {offset}",
            config().max_correction
        );
        // 锚点是「脚掌上的点」，不会被拖着走：每帧都按它重算，偏移不会累积
        assert_eq!(lock.anchor().unwrap(), Vec3::ZERO);
        let (again, _) = lock.update(FootContact::ankle_only(far), 1.0, true, DT, &cfg);
        assert!(((again - far).length() - config().max_correction).abs() < 1e-4);
    }

    #[test]
    fn hysteresis_keeps_planted_between_enter_and_exit() {
        let mut lock = FootLock::default();
        lock.update(
            FootContact::ankle_only(Vec3::ZERO),
            1.0,
            true,
            DT,
            &config(),
        );
        // 0.4 低于 enter、高于 exit：仍然算踩实
        let _ = lock.update(
            FootContact::ankle_only(Vec3::ZERO),
            0.4,
            true,
            DT,
            &config(),
        );
        assert!(lock.is_planted(), "0.4 仍在迟滞区间内，不该松脚");
        assert!(lock.anchor().is_some());
        // 掉到 exit 以下才松开
        lock.update(
            FootContact::ankle_only(Vec3::ZERO),
            0.1,
            true,
            DT,
            &config(),
        );
        assert!(!lock.is_planted());
        assert!(lock.anchor().is_none());
    }

    /// 关闭时必须立刻丢掉锚点（重新打开不能拿旧位置拽人），
    /// 但强度按二阶过渡落下去，所以关掉本身没有速度台阶
    #[test]
    fn disabling_drops_the_anchor_but_eases_the_residual() {
        let cfg = config();
        let mut lock = FootLock::default();
        plant(&mut lock, Vec3::ZERO, 40, &cfg);
        assert!((lock.strength() - 1.0).abs() < 1e-3);

        let ankle = Vec3::new(0.3, 0.0, 0.0);
        let (target, weight) = lock.update(FootContact::ankle_only(ankle), 1.0, false, DT, &cfg);
        assert!(!lock.is_planted());
        assert!(lock.anchor().is_none());
        assert_eq!(weight, 1.0, "残差还没还完");
        // 残差被夹在 max_correction，脚踝不会一步跳回动画位置
        let offset = target - ankle;
        assert!(
            (offset.length() - cfg.max_correction * lock.strength()).abs() < 1e-4,
            "{offset:?}（强度 {}）",
            lock.strength()
        );

        // 之后几帧里强度落回 0，目标单调回到纯动画位置
        let mut target = target;
        for _ in 0..60 {
            let (next, weight) = lock.update(FootContact::ankle_only(ankle), 1.0, false, DT, &cfg);
            if weight == 0.0 {
                assert_eq!(next, ankle, "残差还完后目标应等于动画位置");
                break;
            }
            assert!((next - ankle).length() < (target - ankle).length());
            target = next;
        }
        assert_eq!(lock.residual(), Vec3::ZERO);
    }

    #[test]
    fn rapid_toggling_never_scrambles_the_anchor() {
        // 高频开关：每一帧开/关交替，锚点要么是当前脚踝、要么被清掉，
        // 不能留着几帧前的旧位置把人拽回去
        let mut lock = FootLock::default();
        let mut ankle = Vec3::ZERO;
        for frame in 0..20 {
            ankle += Vec3::new(0.01, 0.0, 0.0);
            let enabled = frame % 2 == 0;
            let (target, weight) = lock.update(FootContact::ankle_only(ankle), 1.0, enabled, DT, &config());
            assert!(target.is_finite());
            assert!((0.0..=1.0).contains(&weight));
            if enabled {
                if let Some(anchor) = lock.anchor() {
                    // 锚点不会比脚落后超过修正上限
                    let lag = (Vec3::new(ankle.x, 0.0, ankle.z)
                        - Vec3::new(anchor.x, 0.0, anchor.z))
                    .length();
                    assert!(lag <= config().max_correction + 1e-4, "lag={lag}");
                }
            } else {
                assert!(lock.anchor().is_none(), "关闭时必须清掉锚点");
            }
        }
    }

    /// 抬脚解除锁定不能给脚一个速度台阶：强度是二阶过渡，首帧只放掉几个百分点，
    /// 中途才最快、末尾又收回到 0（指数衰减是「首帧最猛」，那一下就是顿挫来源）
    #[test]
    fn release_eases_out_without_a_velocity_step() {
        let cfg = config();
        let mut lock = FootLock::default();
        plant(&mut lock, Vec3::ZERO, 40, &cfg);
        assert!((lock.strength() - 1.0).abs() < 1e-3, "强度应先建立起来");

        // 身体走了 6cm：锚点还钉在世界原点，脚踝被动画带走
        let ankle = Vec3::new(0.06, 0.0, 0.0);
        let (target, _) = lock.update(FootContact::ankle_only(ankle), 1.0, true, DT, &cfg);
        assert!((target - Vec3::ZERO).length() < 1e-3, "支撑期应当钉在锚点上");
        let residual = 0.06f32;

        // 松开：支撑掉到 0（低于 exit）。这一帧只放掉几个百分点，位置基本不动
        let (target, weight) = lock.update(FootContact::ankle_only(ankle), 0.0, true, DT, &cfg);
        assert!(!lock.is_planted());
        assert!(lock.anchor().is_none(), "松开后不该留着旧锚点");
        assert_eq!(weight, 1.0, "残差还没还完，这一脚还有活要干");
        assert!(
            (target - Vec3::ZERO).length() < 0.06 * residual,
            "松开当帧就放掉 {:?}，脚会被弹一下",
            target
        );

        // 逐帧位移（从「钉在锚点」开始算）：首帧要远小于「指数衰减的首帧 24%」，
        // 中途最快，末尾收回到 0
        let mut previous = Vec3::ZERO;
        let mut steps = vec![(target - previous).length()];
        for _ in 0..60 {
            let (next, weight) = lock.update(FootContact::ankle_only(ankle), 0.0, true, DT, &cfg);
            if weight == 0.0 {
                break;
            }
            assert!(
                (next - ankle).length() <= (previous - ankle).length() + 1e-6,
                "目标不能在残差方向反弹"
            );
            steps.push((next - previous).length());
            previous = next;
        }
        assert!(steps.len() >= 8, "释放窗口应当跨好几帧，实际 {}", steps.len());
        assert!(
            steps[0] < 0.06 * residual,
            "松开首帧就放掉 {:.1}%（指数衰减是 24%），脚会被弹一下",
            steps[0] / residual * 100.0
        );
        // 最快的一帧不在开头：加速度从 0 起，脚是「慢慢离地」而不是被松开
        let peak = steps.iter().cloned().fold(0.0f32, f32::max);
        let peak_at = steps
            .iter()
            .position(|&step| step >= peak - 1e-6)
            .expect("总有峰值");
        assert!(peak_at >= 1, "峰值在第 {peak_at} 帧，太靠前了");

        // release_secs = 0 表示硬切（只用于对比观感，默认不开）
        let hard = FootLockConfig {
            release_secs: 0.0,
            ..cfg
        };
        let mut snap = FootLock::default();
        plant(&mut snap, Vec3::ZERO, 40, &cfg);
        let (target, weight) = snap.update(FootContact::ankle_only(ankle), 0.0, true, DT, &hard);
        assert_eq!(target, ankle);
        assert_eq!(weight, 0.0);
    }

    /// 抬脚时那道「残差」必须摊在整个摆动期里还完：支撑期钉住的脚离动画位置越远
    /// （转身时能到 20cm），越不能一抬脚就「瞬间」跳回动画位置。
    /// 这里量的是**每帧位移的形状**：首帧极小、中段最快、相邻两帧的位移差（也就是
    /// 加速度）不超过残差的 3% —— 加速度有台阶，肉眼就是「被拽了一下」。
    #[test]
    fn release_spreads_the_residual_over_the_whole_swing() {
        let cfg = config();
        let mut lock = FootLock::default();
        plant(&mut lock, Vec3::ZERO, 40, &cfg);
        assert!((lock.strength() - 1.0).abs() < 1e-3);

        // 猛地转个身：动画把脚踝甩到 20cm 以外，这里仍然钉在锚点
        let residual = 0.20f32;
        let ankle = Vec3::new(residual, 0.0, 0.0);
        let (target, _) = lock.update(FootContact::ankle_only(ankle), 1.0, true, DT, &cfg);
        assert!((target - Vec3::ZERO).length() < 1e-3, "支撑期应当钉在锚点上");

        // 抬脚：从「离动画位置 20cm」开始，逐帧记录还给动画的位移
        let mut previous = residual;
        let mut steps: Vec<f32> = Vec::new();
        for _ in 0..120 {
            let (next, weight) = lock.update(FootContact::ankle_only(ankle), 0.0, true, DT, &cfg);
            if weight == 0.0 {
                assert_eq!(next, ankle, "残差还完后目标应等于动画位置");
                break;
            }
            let offset = (next - ankle).length();
            steps.push(previous - offset);
            previous = offset;
        }

        // 整个摆动期都要用到：0.26s ≈ 16 帧，不能两三帧就还完
        let expected = (cfg.release_secs / DT).ceil() as usize;
        assert!(
            steps.len() >= expected - 1,
            "释放只用了 {} 帧（release_secs = {}s）",
            steps.len(),
            cfg.release_secs
        );
        assert!(
            steps[0] < 0.02 * residual,
            "抬脚首帧就还了 {:.1}%，脚会被拽一下",
            steps[0] / residual * 100.0
        );
        let peak = steps.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            peak < 0.12 * residual,
            "最快的一帧还了 {:.1}%，超过残差的 12%",
            peak / residual * 100.0
        );
        assert!(
            steps[0] < 0.3 * peak,
            "首帧 {:.1}mm 已经接近峰值 {:.1}mm，是「先猛后松」而不是「慢慢加速」",
            steps[0] * 1e3,
            peak * 1e3
        );
        // 加速度不能有台阶：相邻两帧的位移差足够小
        let biggest_jerk = steps
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            biggest_jerk < 0.03 * residual,
            "两帧之间位移差跳了 {:.1}mm（残差 {:.0}cm），加速度有台阶",
            biggest_jerk * 1e3,
            residual * 100.0
        );
    }

    /// 支撑末期不能钉着踝让前掌先离地：脚跟该先抬，前掌最后离地
    /// 支撑末期不能钉着踝让前掌先离地：脚跟该先抬，前掌最后离地
    #[test]
    fn heel_off_rolls_onto_the_toe_instead_of_lifting_the_ball() {
        // 踩实：踝高 10cm、前掌高 2cm、前掌在踝前方 14cm（脚掌朝前下方）
        let cfg = FootLockConfig {
            engage_secs: 0.0,
            ..config()
        };
        let mut lock = FootLock::default();
        let ankle0 = Vec3::new(0.0, 0.10, 0.0);
        let toe0 = Vec3::new(0.0, 0.02, 0.14);
        let contact = |ankle: Vec3, toe: Vec3| FootContact {
            ankle,
            toe: Some(toe),
            toe_drop: 0.0,
            floor: None,
            body: None,
        };

        let (target, _) = lock.update(contact(ankle0, toe0), 1.0, true, DT, &cfg);
        assert!((target - ankle0).length() < 1e-6, "踩实第一帧不该有修正");
        assert_eq!(lock.roll(), 0.0);
        // 踩实那一刻的前掌高度就是地面高度，朝向夹紧拿它当基准
        let floor = lock.toe_floor().expect("踩实时应有前掌高度");
        assert!((floor - toe0.y).abs() < 1e-6, "前掌地面高度 {floor}");

        // 支撑末期：身体继续前走，动画把脚跟抬起来，前掌仍贴在地面
        let mut ankle = ankle0;
        let mut toe = toe0;
        let mut last_target = target;
        let mut rolled = 0.0f32;
        for frame in 1..=14 {
            // 动画抬踝 0.3cm/帧（18cm/s，接近真实蹬地）；前掌水平方向被带着走 1.2cm
            ankle += Vec3::new(0.012, 0.003, 0.0);
            toe += Vec3::new(0.012, 0.0, 0.0);
            let (target, _) = lock.update(contact(ankle, toe), 1.0, true, DT, &cfg);
            assert!(
                (target - last_target).length() < 0.03,
                "第 {frame} 帧踝目标跳了 {last_target:?} -> {target:?}"
            );
            last_target = target;
            rolled = lock.roll();
            if rolled >= 1.0 {
                // 承重点完全在前掌：把修正量加回踝之后，前掌还留在踩实时的世界位置
                let implied_toe = toe + (target - ankle);
                assert!(
                    (implied_toe - toe0).length() < 5e-3,
                    "前掌被带走了：{implied_toe:?}（应在 {toe0:?}）"
                );
                break;
            }
        }
        assert!(rolled >= 1.0, "脚跟抬过 2×toe_pivot_rise 后应完全钉前掌");
        // 踝被抬起来了：修复前它会被按在踩实那一刻的高度上，前掌因此先离地
        assert!(
            last_target.y > ankle0.y + 0.02,
            "踝应随脚跟抬起，实际 {}",
            last_target.y
        );
    }

    /// 起跟时机跟**重心**走：重心还在着地脚后方时，脚跟一律不许抬；
    /// 越过脚踝之后按 `heel_off_secs` 单调推进到 1。
    ///
    /// 这条是与「等动画自己抬踝」的分界线：动画的抬跟时刻常常偏晚，锁脚会把脚掌
    /// 按平到接近离地那一瞬，看起来就是「没踩实就抬脚」。
    #[test]
    fn heel_off_waits_for_the_body_to_cross_the_planted_foot() {
        let cfg = FootLockConfig {
            engage_secs: 0.0,
            heel_off_secs: 0.30,
            heel_off_min_secs: 0.0,
            ..config()
        };
        let mut lock = FootLock::default();
        // 脚掌朝前：踝在原点上方 10cm，前掌在前方 16cm、低 4cm
        let ankle = Vec3::new(0.0, 0.10, 0.0);
        let toe = Vec3::new(0.0, 0.06, 0.16);
        let contact = |body_z: f32| FootContact {
            ankle,
            toe: Some(toe),
            toe_drop: 0.0,
            floor: None,
            body: Some(Vec3::new(0.0, 0.90, body_z)),
        };

        // 踩实：重心还在脚掌后方 10cm
        lock.update(contact(-0.10), 1.0, true, DT, &cfg);
        assert_eq!(lock.heel_off(), 0.0);
        assert_eq!(lock.roll(), 0.0, "重心没过脚，承重点该留在踝上");

        // 0.15s 之内重心一直没越过脚踝：脚跟不许抬
        for frame in 0..9 {
            lock.update(contact(-0.10), 1.0, true, DT, &cfg);
            assert_eq!(lock.heel_off(), 0.0, "第 {frame} 帧：脚跟不该在重心之前抬");
        }

        // 重心压过着地脚：这一帧就该起跟
        lock.update(contact(0.01), 1.0, true, DT, &cfg);
        assert!(lock.heel_off() > 0.0, "重心过了脚踝应当立刻起跟");
        assert!(lock.roll() > 0.0, "起跟同时开始把承重点迁向前掌");

        // 再 0.30s：抬跟走满，承重点完全在前掌
        for _ in 0..18 {
            lock.update(contact(0.01), 1.0, true, DT, &cfg);
        }
        assert!((lock.heel_off() - 1.0).abs() < 1e-3, "抬跟应当走满");
        assert!((lock.roll() - 1.0).abs() < 1e-3, "承重点应当完全在前掌");
    }

    /// 抬跟的斜坡必须装得进**真实剩余**的支撑期。
    ///
    /// 固定的 0.30s 斜坡放在跑步的 0.25s 支撑里，重心越过脚之后只剩 0.10s：
    /// 俯角爬到一半脚就离地了，看着就是脚掌平的、被身体带着走。这里先量一段
    /// 短支撑，再让下一段同样短的支撑触发，检查斜坡被压缩到剩余时间里。
    #[test]
    fn heel_ramp_compresses_into_a_short_stance() {
        let cfg = FootLockConfig {
            engage_secs: 0.0,
            heel_off_min_secs: 0.0,
            heel_off_secs: 0.30,
            ..config()
        };
        let mut lock = FootLock::default();
        let ankle = Vec3::new(0.0, 0.10, 0.0);
        let toe = Vec3::new(0.0, 0.06, 0.16);
        let contact = |body_z: f32| FootContact {
            ankle,
            toe: Some(toe),
            toe_drop: 0.0,
            floor: None,
            body: Some(Vec3::new(0.0, 0.90, body_z)),
        };

        // 第一段：0.25s 的支撑（跑步一类的短支撑）。重心一直在脚后，不起跟，
        // 只为了量出这段有多长
        for _ in 0..15 {
            lock.update(contact(-0.10), 1.0, true, DT, &cfg);
        }
        for _ in 0..20 {
            lock.update(contact(-0.10), 0.0, true, DT, &cfg);
        }
        assert!(!lock.is_planted(), "抬脚之后不该还算踩实");

        // 第二段：同样的短支撑。先踩 5 帧，再让重心压到脚掌中段触发
        for _ in 0..5 {
            lock.update(contact(-0.10), 1.0, true, DT, &cfg);
        }
        assert_eq!(lock.heel_off(), 0.0, "重心还没到脚掌中段，不该起跟");
        lock.update(contact(-0.02), 1.0, true, DT, &cfg);
        assert!(lock.heel_off() > 0.0, "重心压过脚掌中段应当起跟");

        // 估计支撑 0.25s、已踩 0.1s → 剩 0.15s，斜坡 0.85 × 0.15 ≈ 0.13s
        let ramp = lock.heel_ramp_secs();
        assert!(
            (0.11..=0.17).contains(&ramp),
            "0.25s 的支撑里斜坡该压到 0.13s 上下，实际 {ramp}"
        );
        assert!(ramp < cfg.heel_off_secs, "斜坡必须短于配置的上限");

        // 走满这段斜坡时脚还在地上：俯角在离地**之前**就到目标
        let frames = (ramp / DT).ceil() as usize;
        for _ in 0..frames {
            lock.update(contact(-0.02), 1.0, true, DT, &cfg);
        }
        assert!(
            (lock.heel_off() - 1.0).abs() < 1e-3,
            "斜坡走完时抬跟该满，实际 {}",
            lock.heel_off()
        );
        assert!(lock.is_planted(), "此时还没离地：俯角是在支撑期里做完的");
    }

    /// 还没量过任何一段支撑时（刚生成），斜坡退回配置的上限，不会一帧到位
    #[test]
    fn heel_ramp_falls_back_to_the_configured_cap_before_any_measurement() {
        let cfg = FootLockConfig {
            engage_secs: 0.0,
            heel_off_min_secs: 0.0,
            ..config()
        };
        let mut lock = FootLock::default();
        let ankle = Vec3::new(0.0, 0.10, 0.0);
        let toe = Vec3::new(0.0, 0.06, 0.16);
        lock.update(
            FootContact {
                ankle,
                toe: Some(toe),
                toe_drop: 0.0,
                floor: None,
                body: Some(Vec3::new(0.0, 0.90, 0.05)),
            },
            1.0,
            true,
            DT,
            &cfg,
        );
        assert!((lock.heel_ramp_secs() - cfg.heel_off_secs).abs() < 1e-5);
    }

    /// 地面高度必须由「踝高 − 踝到前掌的竖直距离」算出来。
    ///
    /// 落地那帧如果脚跟先着地、脚掌还背屈着，前掌位置比地面高好几厘米；拿它当地面，
    /// 之后脚掌放平时就会被夹在那个高平面上，看起来就是「落地后脚掌翘着、没贴地」。
    #[test]
    fn toe_floor_comes_from_the_ankle_height_not_the_heel_first_contact() {
        let cfg = FootLockConfig {
            engage_secs: 0.0,
            ..config()
        };
        let mut lock = FootLock::default();
        // 踝高 10cm，踝到前掌 8cm → 真实地面 2cm
        let ankle = Vec3::new(0.0, 0.10, 0.0);
        // 脚跟先着地，脚掌背屈：前掌还在 8cm 的空中
        let toe = Vec3::new(0.0, 0.08, 0.14);
        let contact = FootContact {
            ankle,
            toe: Some(toe),
            toe_drop: 0.08,
            floor: None,
            body: None,
        };
        lock.update(contact, 1.0, true, DT, &cfg);
        let floor = lock.toe_floor().expect("踩实时应有前掌地面高度");
        assert!(
            (floor - 0.02).abs() < 1e-6,
            "地面应是 踝高 0.10 − 踝到前掌 0.08 = 0.02，实际 {floor}"
        );
    }

    /// 脚尖先着地（脚掌跖屈）时，「踝高 − 踝到前掌距」反而偏高：脚掌垂下去，
    /// 踝相对脚底更高。这时必须用当帧前掌位置（它才是真的最低点）
    #[test]
    fn toe_floor_takes_the_lower_of_the_ball_and_the_ankle_geometry() {
        let cfg = FootLockConfig {
            engage_secs: 0.0,
            ..config()
        };
        let mut lock = FootLock::default();
        // 踝高 18cm（跖屈时踝被抬起来了），前掌已经踩在 2cm 的地面上
        let ankle = Vec3::new(0.0, 0.18, 0.0);
        let toe = Vec3::new(0.0, 0.02, 0.14);
        let contact = FootContact {
            ankle,
            toe: Some(toe),
            toe_drop: 0.08,
            floor: None,
            body: None,
        };
        lock.update(contact, 1.0, true, DT, &cfg);
        let floor = lock.toe_floor().expect("踩实时应有地面高度");
        assert!(
            (floor - 0.02).abs() < 1e-6,
            "地面应取更低的那个（前掌 0.02），踝高推导会算出 0.10，实际 {floor}"
        );
    }

    /// 射线量到的地面必须无条件优先：脚还在半空（落地帧）时靠位置推断会错一整层，
    /// 台阶上更是直接错一层
    #[test]
    fn probed_floor_wins_over_the_inferred_one() {
        let cfg = FootLockConfig {
            engage_secs: 0.0,
            ..config()
        };
        let mut lock = FootLock::default();
        // 脚踝还在 50cm 高，但脚下 2cm 处已经有台阶面
        let ankle = Vec3::new(0.0, 0.50, 0.0);
        let toe = Vec3::new(0.0, 0.42, 0.14);
        let contact = FootContact {
            ankle,
            toe: Some(toe),
            toe_drop: 0.08,
            floor: Some(0.02),
            body: None,
        };
        lock.update(contact, 1.0, true, DT, &cfg);
        let floor = lock.toe_floor().expect("踩实时应有地面高度");
        assert!(
            (floor - 0.02).abs() < 1e-6,
            "应使用射线量到的台阶面 0.02，实际 {floor}"
        );
        assert!(lock.toe_floor_is_probed(), "来源标记应是「实测」");
    }

    /// 刚踩实不能一帧满强度：支撑脚从「跟着身体走」到「钉死」的速度突变要摊开
    /// 抬脚那一帧不能让「前掌地面基准」和「夹紧强度」一起消失：脚掌朝向会一帧翻回去
    #[test]
    fn toe_floor_survives_release_and_presence_fades() {
        let config = FootLockConfig::default();
        let mut lock = FootLock::default();
        let ankle = Vec3::new(0.0, 0.2, 0.0);
        let toe = ankle + Vec3::new(0.0, -0.08, 0.14);
        let contact = FootContact {
            ankle,
            toe: Some(toe),
            toe_drop: 0.0,
            floor: None,
            body: None,
        };

        for _ in 0..30 {
            lock.update(contact, 1.0, true, DT, &config);
        }
        assert_eq!(lock.presence(), 1.0);
        let floor = lock.toe_floor().expect("踩实时应有前掌地面高度");
        assert!((floor - toe.y).abs() < 1e-6, "前掌地面高度 {floor}");

        // 抬脚那一帧：地面基准还得在，强度也还是接近满的（这一帧没有台阶）
        lock.update(contact, 0.0, true, DT, &config);
        assert!(!lock.is_planted());
        assert!(lock.toe_floor().is_some(), "抬脚那一帧地面基准不能立刻变成 None");
        assert!(lock.presence() > 0.9, "抬脚那一帧仍按接近满强度");

        // 之后每帧掉一点，而不是一帧从 1 掉到 0
        let mut previous = lock.presence();
        let mut frames = 0;
        while lock.presence() > 0.0 && frames < 600 {
            lock.update(contact, 0.0, true, DT, &config);
            assert!(
                previous - lock.presence() < 0.15,
                "夹紧强度单帧掉了 {}，脚掌会翻一下",
                previous - lock.presence()
            );
            previous = lock.presence();
            frames += 1;
        }
        assert!(frames < 40, "夹紧强度应在一个释放窗口内淡出，实际用了 {frames} 帧");
    }

    #[test]
    fn engage_ramps_the_lock_in() {
        let cfg = config();
        let mut lock = FootLock::default();
        lock.update(FootContact::ankle_only(Vec3::ZERO), 1.0, true, DT, &cfg);
        assert!(lock.strength() > 0.05 && lock.strength() < 0.3, "踩实首帧只轻轻咬住：{}", lock.strength());

        // 脚踝已经被动画带走 5cm：修正量恒定，但施加的比例要「加速—减速」地升上来
        let ankle = Vec3::new(0.05, 0.0, 0.0);
        let mut applied = 0.0f32;
        let mut strength = lock.strength();
        let mut biggest_jump = 0.0f32;
        for _ in 0..30 {
            let (target, _) = lock.update(FootContact::ankle_only(ankle), 1.0, true, DT, &cfg);
            let now = (target - ankle).length();
            assert!(now >= applied - 1e-6, "咬合不能倒退：{applied} -> {now}");
            applied = now;
            biggest_jump = biggest_jump.max(lock.strength() - strength);
            strength = lock.strength();
        }
        // 单帧最多吃掉整段过渡的一小半：不是一帧就钉死
        assert!(biggest_jump < 0.4, "单帧强度跳了 {biggest_jump:.2}");
        // `engage_secs` 内应当走到 90%
        let frames = (cfg.engage_secs / DT).ceil() as usize;
        let mut probe = FootLock::default();
        probe.update(FootContact::ankle_only(Vec3::ZERO), 1.0, true, DT, &cfg);
        for _ in 0..frames {
            probe.update(FootContact::ankle_only(Vec3::ZERO), 1.0, true, DT, &cfg);
        }
        assert!(
            probe.strength() >= 0.9,
            "{} 帧后强度只有 {:.2}，过渡太久",
            frames,
            probe.strength()
        );
        assert!((applied - 0.05).abs() < 1e-3, "30 帧后应当升满：{applied}");

        // 关掉斜坡时立刻满强度
        let hard = FootLockConfig {
            engage_secs: 0.0,
            ..cfg
        };
        let mut snap = FootLock::default();
        snap.update(FootContact::ankle_only(Vec3::ZERO), 1.0, true, DT, &hard);
        assert_eq!(snap.strength(), 1.0, "engage_secs = 0 应当直接满强度");
        let (target, _) = snap.update(FootContact::ankle_only(ankle), 1.0, true, DT, &hard);
        assert!((target - Vec3::ZERO).length() < 1e-5, "应当直接钉在锚点");
    }

    #[test]
    fn closed_config_is_sanitized() {
        let cfg = FootLockConfig {
            max_correction: -1.0,
            plant_enter: 0.2,
            plant_exit: 0.9,
            release_secs: -1.0,
            engage_secs: -1.0,
            toe_pivot_rise: -1.0,
            heel_off_lead: 10.0,
            heel_off_secs: -1.0,
            heel_off_min_secs: -1.0,
        }
        .sanitized();
        assert_eq!(cfg.max_correction, 0.0);
        assert!(cfg.plant_exit <= cfg.plant_enter);
        assert_eq!(cfg.release_secs, 0.0);
        assert_eq!(cfg.engage_secs, 0.0);
        assert!(cfg.heel_off_lead <= 0.2, "提前量要夹在合理范围");
        assert!(cfg.heel_off_secs > 0.0, "秒数不能是 0，否则起跟在当帧走完");
        assert_eq!(cfg.heel_off_min_secs, 0.0);
    }
}
