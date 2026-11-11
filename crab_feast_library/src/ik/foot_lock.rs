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
//! - **解除锁定是插值**：抬脚/关掉时残留的修正量按 `release_secs` 指数衰减回动画姿态。
//!   当帧清零会让脚踝从一个锚点瞬移回动画位置，肉眼就是「卡一下」。

use bevy::math::Vec3;

/// 水平方向夹角/位移的极小阈值（米）
const MIN_OFFSET: f32 = 1e-5;

/// 锁脚参数。由全局设置构造，逐帧传给 [`FootLock::update`]，所以改参数立刻生效。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FootLockConfig {
    /// 锚点最多允许偏离脚踝多少米（水平方向）；超过就让锚点跟着脚走
    pub max_correction: f32,
    /// 支撑权重升到它才判定为踩实（防止抬脚/落地边缘反复触发）
    pub plant_enter: f32,
    /// 支撑权重降到它才判定为抬脚
    pub plant_exit: f32,
    /// 解除锁定后残差衰减的时间常数（秒）。0 = 立刻归位（会看到跳变）
    pub release_secs: f32,
    /// 刚判定踩实时，锁脚强度从 0 升到 1 的时间（秒）。
    /// 0 = 一踩实就满强度（脚会从「跟着身体走」一帧内变成「钉死」，同样会「卡」）
    pub engage_secs: f32,
    /// 支撑期内，动画把脚跟抬起多少米之后改成「钉前掌」。
    /// 0 = 从踩实第一帧就钉前掌（脚跟会过早抬起）
    pub toe_pivot_rise: f32,
}

impl Default for FootLockConfig {
    fn default() -> Self {
        Self {
            // 腿长约 0.9m，横向拉 22cm 还有余量；再大就会明显看出膝盖被拽直
            max_correction: 0.22,
            plant_enter: 0.55,
            plant_exit: 0.25,
            // 60ms 一帧衰减约 24%：抬脚时脚踝是平滑追上动画，而不是当帧跳回去
            release_secs: 0.06,
            // 40ms 大约是 2~3 帧：落地瞬间脚有一个「咬住地面」的短促减速
            engage_secs: 0.04,
            // 3cm：正常抬脚跟到 3cm 时前掌是承重点，再往上前掌就该接管了
            toe_pivot_rise: 0.03,
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
}

impl FootContact {
    pub fn ankle_only(ankle: Vec3) -> Self {
        Self { ankle, toe: None }
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
    /// 本段支撑期的咬合权重（0..1）：刚踩实时从 0 升到 1，避免脚的速度一帧内被掐死
    engage: f32,
    /// 承重点沿脚掌滑动的比例：0 = 钉脚踝（脚是平的），1 = 钉前掌（绕前掌转）
    roll: f32,
    /// 踩实那一帧的踝高：动画把踝抬高说明该抬脚跟了
    ankle_base: f32,
    /// 踩实那一帧「前掌 − 踝」的向量（脚掌朝向）。没有前掌骨时为零
    toe_vec: Vec3,
    /// 踩实那一帧前掌的世界高度（地面基准）。抬脚后保留，供朝向夹紧淡出用
    toe_floor: Option<f32>,
    /// 朝向夹紧的剩余强度（0..1）：踩实时为 1，抬脚后与残差同步衰减。
    /// 少了它，「前掌不穿地」的转动会在抬脚那一帧被整段撤掉，脚掌一帧翻十几度
    presence: f32,
}

impl FootLock {
    pub fn reset(&mut self) {
        self.anchor = None;
        self.planted = false;
        self.residual = Vec3::ZERO;
        self.engage = 0.0;
        self.roll = 0.0;
        self.ankle_base = 0.0;
        self.toe_vec = Vec3::ZERO;
        self.toe_floor = None;
        self.presence = 0.0;
    }

    /// 还没还回去的修正量（世界空间，米）。调试面板可以拿它当过冲读数
    pub fn residual(&self) -> Vec3 {
        self.residual
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

    /// 踩实时前掌贴地的高度（世界 y）。没有踩实或没有前掌骨时是 `None`。
    /// 调用方拿它当「地面高度」，把前掌朝向夹在它之上，脚掌就不会扎进地里。
    pub fn toe_floor(&self) -> Option<f32> {
        self.toe_floor
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
    /// - `dt`：帧间隔（秒），用于残差衰减与咬合斜坡，与帧率无关。
    ///
    /// 承重点是沿脚掌滑动的：踩实初期钉**脚踝**（脚是平的）；动画把脚跟抬到
    /// `toe_pivot_rise` 之后，承重点逐渐滑到**前掌**，脚变成绕前掌转 ——
    /// 于是脚跟先离地、前掌最后离地，和真实蹬地顺序一致。
    ///
    /// 锚点 = 踩实时踝的世界位置，加上 `roll × 踩实时的脚向量`：锚点在世界里不动，
    /// 但它是「脚掌上的哪一点」随 `roll` 从踝滑到前掌。所以抬脚跟时脚踝目标是
    /// 平滑上移的，不会出现「一帧从踝锚点跳到前掌锚点」的台阶。
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
                    // 踩实的第一帧就把当前脚踝记成锚点：这一帧的残差为 0，不会跳
                    self.anchor = Some(ankle);
                    self.ankle_base = ankle.y;
                    self.toe_vec = toe.map(|toe| toe - ankle).unwrap_or(Vec3::ZERO);
                    self.toe_floor = toe.map(|toe| toe.y);
                    self.presence = 1.0;
                }
            } else if support <= config.plant_exit {
                self.planted = false;
                self.anchor = None;
            }
        } else {
            self.planted = false;
            self.anchor = None;
        }

        // 动画把脚跟抬起来 → 承重点向脚掌滑。只增不减：支撑期内不倒回去
        if self.planted && toe.is_some() {
            let start = config.toe_pivot_rise.max(1e-3);
            let rise = (ankle.y - self.ankle_base).max(0.0);
            let roll = ((rise - start) / start).clamp(0.0, 1.0);
            self.roll = self.roll.max(roll);
        }

        // 还钉着时的修正量：锚点 + roll × 脚向量差 − 当前踝位置。
        // 刚松开的那一帧用松开前的锚点算，修正量原样交给残差（不当帧清零）
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
            // 咬合：刚踩实那一帧从 0 起，`engage_secs` 内升到满强度
            self.engage = if was_planted {
                approach(self.engage, 1.0, dt, config.engage_secs)
            } else {
                0.0
            };
            self.residual = pinned;
        } else {
            // 还残差期间按满强度：此时被平滑的量就是残差本身
            self.engage = 1.0;
            if was_planted {
                // 刚松开的那一帧不衰减（`release_secs > 0` 时）：修正量原样留下，
                // 从下一帧才开始往 0 收敛，所以松开本身没有台阶。
                // `release_secs == 0` 表示硬切，当帧就归位。
                self.residual = if config.release_secs > f32::EPSILON {
                    pinned
                } else {
                    Vec3::ZERO
                };
                // 同理，朝向夹紧这一帧还是满强度，从下一帧起跟着残差一起淡出
                if config.release_secs <= f32::EPSILON {
                    self.presence = 0.0;
                }
            } else {
                self.residual = decay(self.residual, dt, config.release_secs);
                self.presence = decay_scalar(self.presence, dt, config.release_secs);
            }
        }

        let applied = self.residual * self.engage;
        let weight = if applied.length_squared() > MIN_OFFSET * MIN_OFFSET {
            1.0
        } else {
            0.0
        };
        (ankle + applied, weight)
    }
}

/// 每帧最多推进 `dt / secs` 的比例（线性咬合），`secs = 0` 直接到位
fn approach(current: f32, target: f32, dt: f32, secs: f32) -> f32 {
    if secs <= f32::EPSILON || dt <= 0.0 {
        return target;
    }
    let step = dt / secs;
    if (target - current).abs() <= step {
        target
    } else {
        current + step * (target - current).signum()
    }
}

/// 残差指数衰减。低于亚毫米直接归零：指数永远到不了 0，
/// 不截断的话这一脚每一帧都会多解一次 IK。
fn decay_scalar(value: f32, dt: f32, release_secs: f32) -> f32 {
    if dt <= 0.0 || release_secs <= f32::EPSILON {
        return 0.0;
    }
    let next = value * (-dt / release_secs).exp();
    if next < 1e-3 {
        0.0
    } else {
        next
    }
}

fn decay(residual: Vec3, dt: f32, release_secs: f32) -> Vec3 {    if dt <= 0.0 || release_secs <= f32::EPSILON {
        return Vec3::ZERO;
    }
    let next = residual * (-dt / release_secs).exp();
    if next.length_squared() < 1e-8 {
        Vec3::ZERO
    } else {
        next
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
        lock.update(FootContact::ankle_only(Vec3::ZERO), 1.0, true, DT, &config());
        // 0.4 低于 enter、高于 exit：仍然算踩实
        let _ = lock.update(FootContact::ankle_only(Vec3::ZERO), 0.4, true, DT, &config());
        assert!(lock.is_planted(), "0.4 仍在迟滞区间内，不该松脚");
        assert!(lock.anchor().is_some());
        // 掉到 exit 以下才松开
        lock.update(FootContact::ankle_only(Vec3::ZERO), 0.1, true, DT, &config());
        assert!(!lock.is_planted());
        assert!(lock.anchor().is_none());
    }

    /// 关闭时必须立刻丢掉锚点（重新打开不能拿旧位置拽人），
    /// 但残差仍按时间还回去，所以关掉本身也不会「卡」一下
    #[test]
    fn disabling_drops_the_anchor_but_eases_the_residual() {
        let cfg = config();
        let mut lock = FootLock::default();
        lock.update(FootContact::ankle_only(Vec3::ZERO), 1.0, true, DT, &cfg);
        let ankle = Vec3::new(0.3, 0.0, 0.0);
        let (target, weight) = lock.update(FootContact::ankle_only(ankle), 1.0, false, DT, &cfg);
        assert!(!lock.is_planted());
        assert!(lock.anchor().is_none());
        assert_eq!(weight, 1.0, "残差还没还完");
        // 残差被夹在 max_correction，脚踝不会一步跳回动画位置
        let offset = target - ankle;
        assert!(
            (offset.length() - cfg.max_correction).abs() < 1e-4,
            "{offset:?}"
        );

        // 之后几帧里残差衰减到 0，目标回到纯动画位置
        let mut target = target;
        for _ in 0..30 {
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

    /// 抬脚解除锁定不能当帧清零：残差要按时间衰减，脚踝平滑追上动画
    #[test]
    fn release_ramps_the_foot_back_instead_of_snapping() {
        let cfg = config();
        let mut lock = FootLock::default();
        lock.update(FootContact::ankle_only(Vec3::ZERO), 1.0, true, DT, &cfg);

        // 身体走了 6cm、支撑掉到 0.1（低于 exit）：这一帧只是松开锚点
        let ankle = Vec3::new(0.06, 0.0, 0.0);
        let (target, weight) = lock.update(FootContact::ankle_only(ankle), 0.1, true, DT, &cfg);
        assert!(!lock.is_planted());
        assert!(lock.anchor().is_none(), "松开后不该留着旧锚点");
        assert_eq!(weight, 1.0, "残差还没还完，这一脚还有活要干");
        let offset = target - ankle;
        // 松开当帧不衰减：残差原样是 -6cm（不是 0，那就是「卡一下」）
        assert!(
            (offset.x + 0.06).abs() < 1e-4,
            "解除锁定当帧残差 {offset:?} 应完整保留修正量"
        );
        assert!(offset.length() > 0.05);

        // 之后脚踝不动，残差单调收敛到 0，收敛后权重回到 0
        let mut last = offset.length();
        for frame in 0..30 {
            let (target, weight) = lock.update(FootContact::ankle_only(ankle), 0.0, true, DT, &cfg);
            let now = (target - ankle).length();
            assert!(now <= last + 1e-6, "第 {frame} 帧残差反弹：{last} -> {now}");
            last = now;
            if last == 0.0 {
                assert_eq!(weight, 0.0, "残差归零后不该再解 IK");
                break;
            }
        }
        assert!(last < 1e-3, "残差没有收敛：{last}");

        // release_secs = 0 表示硬切（只用于对比观感，默认不开）
        let hard = FootLockConfig {
            release_secs: 0.0,
            ..cfg
        };
        let mut snap = FootLock::default();
        snap.update(FootContact::ankle_only(Vec3::ZERO), 1.0, true, DT, &cfg);
        let (target, weight) = snap.update(FootContact::ankle_only(ankle), 0.0, true, DT, &hard);
        assert_eq!(target, ankle);
        assert_eq!(weight, 0.0);
    }

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
        for frame in 1..=10 {
            ankle += Vec3::new(0.012, 0.010, 0.0);
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
        };

        lock.update(contact, 1.0, true, DT, &config);
        assert_eq!(lock.presence(), 1.0);
        let floor = lock.toe_floor().expect("踩实时应有前掌地面高度");
        assert!((floor - toe.y).abs() < 1e-6, "前掌地面高度 {floor}");

        // 抬脚那一帧：地面基准还得在，强度仍是满的（这一帧没有台阶）
        lock.update(contact, 0.0, true, DT, &config);
        assert!(!lock.is_planted());
        assert!(lock.toe_floor().is_some(), "抬脚那一帧地面基准不能立刻变成 None");
        assert_eq!(lock.presence(), 1.0, "抬脚那一帧仍按满强度，下一帧才开始淡出");

        let mut frames = 0;
        while lock.presence() > 0.0 && frames < 600 {
            lock.update(contact, 0.0, true, DT, &config);
            frames += 1;
        }
        assert!(frames < 40, "夹紧强度应在一个释放窗口内淡出，实际用了 {frames} 帧");
    }

    #[test]
    fn engage_ramps_the_lock_in() {
        let cfg = config();
        let mut lock = FootLock::default();
        lock.update(FootContact::ankle_only(Vec3::ZERO), 1.0, true, DT, &cfg);
        assert_eq!(lock.engage, 0.0, "踩实第一帧不咬合");

        // 脚踝已经被动画带走 5cm：修正量恒定，但施加的比例要在几帧内升上来
        let ankle = Vec3::new(0.05, 0.0, 0.0);
        let mut applied = 0.0f32;
        for frame in 0..4 {
            let (target, _) = lock.update(FootContact::ankle_only(ankle), 1.0, true, DT, &cfg);
            let now = (target - ankle).length();
            assert!(now >= applied - 1e-6, "第 {frame} 帧咬合倒退：{applied} -> {now}");
            applied = now;
        }
        // 40ms 斜坡在 60fps 下 3 帧走满
        assert!((applied - 0.05).abs() < 1e-3, "咬合没有升满：{applied}");

        // 关掉斜坡时立刻满强度
        let hard = FootLockConfig {
            engage_secs: 0.0,
            ..cfg
        };
        let mut snap = FootLock::default();
        snap.update(FootContact::ankle_only(Vec3::ZERO), 1.0, true, DT, &hard);
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
        }
        .sanitized();
        assert_eq!(cfg.max_correction, 0.0);
        assert!(cfg.plant_exit <= cfg.plant_enter);
        assert_eq!(cfg.release_secs, 0.0);
        assert_eq!(cfg.engage_secs, 0.0);
    }
}
