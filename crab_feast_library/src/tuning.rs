//! 角色运动的调参常量：客户端本地面向逻辑与服务器朝向复制共用同一份。
//!
//! 这些值决定「转身看起来有多重」：单机时由客户端的 `facing_system` 用它做
//! 比例控制 + 角加速度限速；联网后位移和朝向归服务器，服务器必须用同一组值，
//! 否则同一个动作在本地和远端会转出两种节奏。

/// 服务器推动角色的顶速（米/秒）。
///
/// 两端都要用：服务器拿它写 `LinearVelocity`，客户端拿它把复制下来的
/// `NetMotion.speed` 换算成动画混合空间的 `speed` 轴值（1.0 = 跑档），
/// 空中操控的角速度也按它折算（见 [`crate::air::air_steer_velocity`]）。
/// 分开写两份必然对不上：真实地速和步频一错位就是滑步。
pub const MOVE_SPEED_MPS: f32 = 3.0;

/// 最大转向角速度（度/秒）。
///
/// 180°/s：180° 反向 1.26 秒、90° 0.6 秒。
///
/// 历史值：90°/s、350°/s² → 180° 要 2.2 秒。这个「有重量」的节奏在实机上被判定
/// 成「转身时人像在原地打转、看着抖」——真机每帧都在转、但用户几乎转两秒才到位，
/// 中间还压着降速地板，看起来就是原地打摆子。调快顶速后转身一次到位，抖动消失。
/// 30°/s、120°/s² → 6.1 秒（太沉，已否决）；450°/s、3000°/s² → 0.63 秒（观感偏假）。
pub const TURN_SPEED_DEG_PER_SEC: f32 = 180.0;

/// 最大转向角加速度（度/秒²）。
///
/// 没有它，180° 反向的第一帧角速度就从 0 跳到上限，看着像原地瞬移朝向。
/// 按 180/700 的原始比例跟着 [`TURN_SPEED_DEG_PER_SEC`] 走：起步段恒定
/// 0.26 秒走 11.6°，顶速调快调慢都不改起步形状。
pub const TURN_ACCEL_DEG_PER_SEC2: f32 = 700.0;

/// 误差小于它就算已经对准，避免在阈值附近抖。
pub const TURN_TOLERANCE_DEG: f32 = 2.0;

/// 判断「身体真的开始转了没有」用的参考角速度（度/秒）。
///
/// 必须和 [`TURN_SPEED_DEG_PER_SEC`] 解耦：刹车力按 `|转速|/这个值` 加权，
/// 一旦用顶速当分母，把顶速调快就等于把「起步那 0.26 秒」判成「还没开始转」，
/// 于是甩头第一秒仍然按满速冲出去——这正是实机上「转身没减速」的样子。
pub const TURN_RATE_NORM_DEG_PER_SEC: f32 = 90.0;

/// 比例增益（度/秒 每 度误差）：90° 误差时立刻跑满上限，最后十几度平滑收到 0。
///
/// 这个值只影响**收尾**：前半段的快慢由 [`TURN_SPEED_DEG_PER_SEC`] 决定，
/// 觉得「转得拖沓」但增大会显得假时，先调它，别去抬最大角速度。
pub const TURN_RESPONSE: f32 = 10.0;

/// 「转身降速起点」的可调区间（度）。上限留 170：再大就等于永远不降速
pub const TURN_BRAKE_START_RANGE: (f32, f32) = (20.0, 170.0);

/// 「转身降速下限」的可调区间：1.0 = 完全不降速，0.2 已经接近「甩头就停」
pub const TURN_BRAKE_MIN_RANGE: (f32, f32) = (0.2, 1.0);

/// 转身降速的默认「到位角」（度）：夹角到这个数就压到下限，再大也不更低。
///
/// 取 90 是玩家的原话「转弯大于 90° 才开始那种减速转身」：小角度侧移照常跑，
/// 掉头 / 大拐弯才进入「先刹住惯性、再折线转过去」的段。
pub const TURN_BRAKE_START_DEG_DEFAULT: f32 = 90.0;

/// 转身降速的默认下限（0~1）：大角度转弯时保留的地速。
///
/// 0.25 的用意是**折线**：甩头时人只往前滑一小段（0.75 m/s），身体趁这段低速
/// 转过去，转完再顺着新方向加速。**不归零**——完全停住会变成「原地转身」，
/// 那是上一版被否掉的观感。
pub const TURN_BRAKE_MIN_SCALE_DEFAULT: f32 = 0.4;

/// 夹角小于它时完全不加刹车（度）：这段是「正常跑，顺手歪一点」。
///
/// 曲线从这里才开始往下走，到 [`TURN_BRAKE_START_DEG_DEFAULT`] 压到下限。
/// 两端导数都是 0，所以既不会在小角度突然顿一下，也不会在拉满时硬跳。
pub const TURN_BRAKE_RECOVER_DEG: f32 = 30.0;

/// 降速有多快（比例/秒）：**转速吃满之后**满速压到下限约 0.34 秒。
///
/// 不按固定时间常数、而按**速率**限幅，是为了让两端的曲线都只是时间的函数，
/// 帧率高低不影响走到哪一格。
pub const TURN_BRAKE_FALL_PER_SEC: f32 = 2.2;

/// 提速有多快（比例/秒）：从下限回到满速约 0.7 秒，比刹车慢——转身完还要一点
/// 时间把步频提上来，太快会显得「刚转完就窜出去」。
pub const TURN_BRAKE_RISE_PER_SEC: f32 = 1.1;

/// 朝向误差 → 地速保留比例（0~1），单机、预测端、服务器共用。
///
/// 两段：
/// - `|误差| <= start_deg`：1.0，正常走跑不受影响；
/// - `start_deg ~ 180°`：余弦缓降，从 1.0 平滑收到 `min_scale`（两端导数为 0，
///   跨过起点没有折角，也不会在 180° 处突然收住）。
///
/// 只用**夹角**，不看符号：往左甩 180° 和往右甩 180° 一样。
pub fn turn_speed_scale_with(error_deg: f32, start_deg: f32, min_scale: f32) -> f32 {
    let start = start_deg.clamp(TURN_BRAKE_START_RANGE.0, TURN_BRAKE_START_RANGE.1);
    let floor = min_scale.clamp(TURN_BRAKE_MIN_RANGE.0, TURN_BRAKE_MIN_RANGE.1);
    let deg = error_deg.abs();
    // 小角度：正常跑，别去碰
    if deg <= TURN_BRAKE_RECOVER_DEG {
        return 1.0;
    }
    // 大角度：一路压在同一个下限里，整段转弯都是「低速折线」
    if deg >= start {
        return floor;
    }
    // 中间只留一小段过渡：从 RECOVER 处的 1.0 缓降到 start 处的 floor
    let span = (start - TURN_BRAKE_RECOVER_DEG).max(1.0);
    let t = ((deg - TURN_BRAKE_RECOVER_DEG) / span).clamp(0.0, 1.0);
    // ease = 0.5 - 0.5cos(πt)：两端斜率 0，进 / 出刹车带都不打顿
    let ease = 0.5 - 0.5 * (std::f32::consts::PI * t).cos();
    1.0 - (1.0 - floor) * ease
}

/// 把降速比例向目标推一格：**刹得比提速快**，这就是「惯性」。
///
/// 这层斜坡是必须的：`turn_speed_scale_with` 只看夹角，摇杆一甩夹角当场就是
/// 180°，没有它速度会一帧内从满速掉到下限，像被拉住而不是被惯性带走。
/// 单机、预测端、服务端每 tick 调一次，所以它就是纯时间的函数，帧率不影响落点。
///
/// `turn_rate_deg` 是**本 tick 身体实际的转向角速度**（度/秒，取绝对值）。
/// 刹车力按它占 [`TURN_RATE_NORM_DEG_PER_SEC`] 的比例给：人还没开始转的那一两帧
/// 不给刹车力，转速爬满才吃满 2.2/s。
///
/// 这是「刹车跟着转身走」——不回绕、不额外存状态，两端的转速本来就同源
/// （[`step_yaw`]），所以斜坡逐 tick 对得上。
/// 早先按纯时间 2.2/s 刹：0.34 秒就压到下限，而转身要 0.26 秒才爬满转速，
/// 于是 0.35 秒里人只转了 20° 却已经从 3 m/s 掉到 0.75 m/s，
/// 观感是「先站住、再开始转」（手感上就是那一下僵硬）。
pub fn ramp_turn_brake(current: f32, target: f32, turn_rate_deg: f32, dt: f32) -> f32 {
    let current = current.clamp(0.0, 1.0);
    let rate = if target < current {
        // 转速吃满之前只给一部分刹车力；松手 / 没在转时刹车自然停手
        let weight = (turn_rate_deg.abs() / TURN_RATE_NORM_DEG_PER_SEC).clamp(0.0, 1.0);
        TURN_BRAKE_FALL_PER_SEC * weight
    } else {
        TURN_BRAKE_RISE_PER_SEC
    };
    approach(current, target.clamp(0.0, 1.0), rate * dt)
}

/// 反向（夹角超过 90°）时速度矢量翻转的加速度（米/秒²）。
///
/// 180° 掉头不该一帧把速度翻过去，也不该沿一条弧划出去：这里把「当前速度」
/// 按有限加速度掰向新目标，于是先沿原方向刹到 0、再过零、再往反方向加速。
/// 6 m/s² 下 3.0 → 0 约 0.5 秒、翻到 0.75 再 0.12 秒，和转身降速同一个尺度。
pub const REVERSE_ACCEL_MPS2: f32 = 6.0;

/// 顺向变向「一帧到位」允许的最大速度差（m/s）。
///
/// 顺向（≤70°）且需要调整不到这么多时，直接写目标：这是「跟手」的那一档，
/// 也是**恢复快**的那一档——同步重对齐把速度打断成 0 之后，下一 tick 立刻回到满速，
/// 不会留一段「从 0 重新加速」的肉感（那会被看成「服务端在一拉一拉」）。
/// 0.35 m/s 差不多是「摇杆抖一下 / 同步偏置」的量级；超过它就是真的换挡，
/// 走下面的加速度分支（掉头过零那一下虽然速度小，但目标离它 0.7~3 m/s，
/// 也落在限速分支里，不会一帧弹上去）。
pub const SNAP_LEN_MPS: f32 = 0.35;

/// 顺向（夹角 ≤70°）但需要大幅调整时的加速度（米/秒²）。
///
/// 20 m/s² 下 0 → 3.0 只要 0.15 秒；它只负责「跨过掉头、被打断之后重新起来」这类
/// 大跨度调整，正常微调走上面的一帧到位。
pub const GROUND_ACCEL_MPS2: f32 = 20.0;

/// 顺向 / 大角度 的分界（70°）。
pub const SNAP_ALIGN_COS: f32 = 0.342;

/// 地速按有限加速度逼近目标：**只有大角度（>70°）才掰**，正常变向 / 起步照旧一帧到位。
///
/// 大角度那一段走直线：速度矢量沿两点连线插值，先减到 0 再反向增大，
/// 不会像「方向取身体朝向」那样绕出一个圈。判据是**夹角**而不是点积的正负号，
/// 详见 [`SNAP_ALIGN_COS`]。
/// 单机、预测端、服务端每 tick 调同一个，两端才能逐 tick 对齐。
pub fn ground_velocity_step(current: [f32; 2], target: [f32; 2], accel: f32, dt: f32) -> [f32; 2] {
    let dot = current[0] * target[0] + current[1] * target[1];
    let len2 = current[0] * current[0] + current[1] * current[1];
    let tlen2 = target[0] * target[0] + target[1] * target[1];
    // 夹角：顺向（≤70°）用 [`GROUND_ACCEL_MPS2`]，大角度 / 反向用调用方给的 `accel`（惯性）。
    //
    // 早先这里是「点积为正就一帧到位」。掉头途中速度被掰到接近 0、方向几乎垂直于
    // 目标的那一 tick，点积在 0 上下翻，翻成正的那一帧直接返回满速
    // （实测 0.46 → 2.49 m/s 一 tick，画面上就是转身末尾那一下「抖/僵」）。
    // 现在任何一步都受加速度上限约束，那条不连续曲线就没有了。
    let cos = if len2 > 1e-8 && tlen2 > 1e-8 {
        dot / (len2.sqrt() * tlen2.sqrt())
    } else {
        -1.0
    };
    let dx = target[0] - current[0];
    let dz = target[1] - current[1];
    let len = (dx * dx + dz * dz).sqrt();
    // 顺向 + 小跨度：一帧到位（跟手、也是被打断之后立刻恢复的那条路）。
    // 顺向 + 大跨度（比如已经刹到 0 又要回满速）：按 GROUND_ACCEL 起，别一帧弹上去。
    // 大角度 / 反向：按调用方给的 `accel` 走惯性。
    let rate = if cos >= SNAP_ALIGN_COS {
        if len <= SNAP_LEN_MPS || len2 <= 1e-8 {
            return target;
        }
        GROUND_ACCEL_MPS2
    } else {
        accel
    };
    let max_step = rate * dt;
    if len <= max_step {
        return target;
    }
    [
        current[0] + dx / len * max_step,
        current[1] + dz / len * max_step,
    ]
}

/// 用默认参数算地速保留比例（固定参数的调用点与测试用这个）
pub fn turn_speed_scale(error_deg: f32) -> f32 {
    turn_speed_scale_with(
        error_deg,
        TURN_BRAKE_START_DEG_DEFAULT,
        TURN_BRAKE_MIN_SCALE_DEFAULT,
    )
}

/// 朝向弧度 → 水平单位方向 `[x, z]`。
///
/// 约定和客户端的 `yaw_of` 一致（`yaw = atan2(x, z)`，0 = 正 +Z），所以这里
/// 直接把角度还原成向量。位移方向现在恒取**身体朝向**：甩头时人还沿原方向冲，
/// 转身的过程就是一条弧（惯性），单机 / 预测端 / 服务端都走这一个函数。
pub fn dir_of_yaw(yaw: f32) -> [f32; 2] {
    [yaw.sin(), yaw.cos()]
}

/// 把 `current` 以 `max_step` 的步长逼近 `target`（不越过）。
pub fn approach(current: f32, target: f32, max_step: f32) -> f32 {
    let delta = target - current;
    if delta.abs() <= max_step {
        target
    } else {
        current + delta.signum() * max_step
    }
}

/// 把角度折到 (-π, π]，用于求「最短转向」的带符号误差。
pub fn wrap_pi(angle: f32) -> f32 {
    (angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

/// 一次比例 + 角加速度限速的转向步进，单机与服务器共用同一套算式。
///
/// 返回 `(新的 yaw, 新的 yaw_rate)`。`error` 是「还需绕 +Y 转多少」，
/// 由调用方按 [`wrap_pi`] 算好。
pub fn step_yaw(yaw: f32, yaw_rate: f32, error: f32, dt: f32) -> (f32, f32) {
    let error_deg = error.to_degrees();
    let desired = if error_deg.abs() <= TURN_TOLERANCE_DEG {
        0.0
    } else {
        error.signum() * (error_deg.abs() * TURN_RESPONSE).min(TURN_SPEED_DEG_PER_SEC)
    };
    let rate = approach(yaw_rate, desired, TURN_ACCEL_DEG_PER_SEC2 * dt)
        .clamp(-TURN_SPEED_DEG_PER_SEC, TURN_SPEED_DEG_PER_SEC);
    // 单帧转角不许超过剩余误差，避免高转速下冲过头再往回摆
    let step = error.signum() * (rate.abs().to_radians() * dt).min(error.abs());
    (wrap_pi(yaw + step), rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定步长把一次转身跑到「进入容差」，返回到位用时（秒）与第一帧转角（度）。
    fn turn_seconds(error_deg: f32, dt: f32) -> (f32, f32) {
        let target = error_deg.to_radians();
        let mut yaw = 0.0;
        let mut rate = 0.0;
        let mut secs = 0.0;
        let mut first = 0.0f32;
        for tick in 0..2000 {
            let error = wrap_pi(target - yaw);
            if error.to_degrees().abs() <= TURN_TOLERANCE_DEG {
                break;
            }
            let (next, next_rate) = step_yaw(yaw, rate, error, dt);
            let delta = wrap_pi(next - yaw).to_degrees().abs();
            assert!(
                delta <= TURN_SPEED_DEG_PER_SEC * dt + 1e-3,
                "单帧转了 {delta}°"
            );
            if tick == 0 {
                first = delta;
            }
            yaw = next;
            rate = next_rate;
            secs += dt;
        }
        (secs, first)
    }

    /// 手感就是这个数：180° 反向 1.133 秒、90° 0.6 秒。
    /// 顶速没被吃掉（一下子转过去）或加速度丢了（第一帧就满速）都在这里露出来。
    #[test]
    fn reversal_takes_the_designed_time() {
        let (reversal, first) = turn_seconds(180.0, 1.0 / 60.0);
        assert!((1.0..1.35).contains(&reversal), "180° 用了 {reversal}s");
        assert!(
            (0.4..0.8).contains(&turn_seconds(90.0, 1.0 / 60.0).0),
            "90° 用得太久/太快"
        );
        // 起步段受加速度限制：60 Hz 第一帧只该走 350°/s² × dt² ≈ 0.1°，不是 1.5°
        assert!(
            first < TURN_SPEED_DEG_PER_SEC / 60.0 / 3.0,
            "第一帧就到顶速了：{first}°"
        );
    }

    /// 容差内不动：站在目标朝向上不该每帧抖 0.0x 度。
    #[test]
    fn inside_tolerance_holds_still() {
        let (yaw, rate) = step_yaw(0.0, 0.0, 0.5f32.to_radians(), 1.0 / 60.0);
        assert_eq!(yaw, 0.0);
        assert_eq!(rate, 0.0);
    }

    /// 反向转身时角速度要穿过 0 再反向加速，不能一帧翻符号。
    #[test]
    fn reversal_accelerates_through_zero() {
        let mut rate = TURN_SPEED_DEG_PER_SEC;
        let dt = 1.0 / 60.0;
        let (_, first) = step_yaw(1.5, rate, -0.2, dt);
        assert!(first < rate, "角速度没有开始刹车：{rate} → {first}");
        assert!(first > 0.0, "一帧就翻成反向转了：{first}");
        rate = first;
        for _ in 0..30 {
            let (_, next) = step_yaw(1.5, rate, -0.2, dt);
            assert!(next < rate + 1e-3);
            rate = next;
        }
        assert!(rate < 0.0, "刹车太慢，最后没转过来");
    }

    /// 不越过目标：转向途中不许冲过头再用容差抖回来。
    #[test]
    fn never_overshoots_the_target() {
        let target = 2.0f32.to_radians();
        let mut yaw = 0.0;
        let mut rate = TURN_SPEED_DEG_PER_SEC;
        let dt = 1.0 / 60.0;
        for _ in 0..200 {
            let error = wrap_pi(target - yaw);
            let (next, next_rate) = step_yaw(yaw, rate, error, dt);
            assert!(next <= target + 1e-6, "冲过头：{next} > {target}");
            yaw = next;
            rate = next_rate;
        }
        assert!((yaw - target).abs() < 1e-3);
    }

    /// 转身降速：小角度满速、180° 收但不归零、中间单调无折角
    #[test]
    fn turn_brake_curve_holds_its_shape() {
        assert_eq!(turn_speed_scale(0.0), 1.0);
        assert_eq!(turn_speed_scale(TURN_BRAKE_RECOVER_DEG), 1.0);
        // 到位角及更大：一路压在下限，整段转弯是同一个低速
        let floor = turn_speed_scale(180.0);
        assert!(
            (floor - TURN_BRAKE_MIN_SCALE_DEFAULT).abs() < 1e-6,
            "180° 应保留 {TURN_BRAKE_MIN_SCALE_DEFAULT}，实测 {floor}"
        );
        assert_eq!(turn_speed_scale(TURN_BRAKE_START_DEG_DEFAULT), floor);
        assert!(floor > 0.0, "下限不能归零，那是「原地转身」");
        // 左右对称
        assert!((turn_speed_scale(-120.0) - turn_speed_scale(120.0)).abs() < 1e-6);
        // 过渡段单调下降（31° → 90°）
        let mut last = 1.0;
        for deg in 31..=180 {
            let now = turn_speed_scale(deg as f32);
            assert!(now <= last + 1e-6, "deg {deg}: {now} > {last}");
            last = now;
        }
        // 起点处不能有折角（跨过 start 那一度斜率接近 0）
        let eps = 0.5;
        let slope_in = (turn_speed_scale(TURN_BRAKE_START_DEG_DEFAULT)
            - turn_speed_scale(TURN_BRAKE_START_DEG_DEFAULT + eps))
            / eps;
        assert!(slope_in.abs() < 0.01, "起点斜率 {slope_in}");
        // 180° 收尾同样平滑，到极限角度也不会再往下掉
        let slope_out = (turn_speed_scale(179.5) - turn_speed_scale(180.0)) / 0.5;
        assert!(slope_out.abs() < 0.01, "终点斜率 {slope_out}");
        assert_eq!(turn_speed_scale(200.0), floor, "超过 180° 保持下限");
    }

    /// 面板上的两个旋钮：起点越早 / 下限越低，降速越明显
    /// 反向走直线：速度先减到 0 再反向增大，横向不许冒出来
    #[test]
    fn reverse_velocity_passes_through_zero_on_a_straight_line() {
        let dt = 1.0 / 60.0;
        let mut v = [0.0f32, MOVE_SPEED_MPS];
        let mut min_speed = MOVE_SPEED_MPS;
        let mut max_lateral = 0.0f32;
        for _ in 0..120 {
            v = ground_velocity_step(
                v,
                [0.0, -MOVE_SPEED_MPS * TURN_BRAKE_MIN_SCALE_DEFAULT],
                REVERSE_ACCEL_MPS2,
                dt,
            );
            min_speed = min_speed.min((v[0] * v[0] + v[1] * v[1]).sqrt());
            max_lateral = max_lateral.max(v[0].abs());
        }
        assert!(min_speed < 0.05, "反向要经过静止，实测最低 {min_speed}");
        assert!(max_lateral < 1e-6, "反向不许横向偏出去：{max_lateral}");
        assert!(v[1] < 0.0, "最后该朝反方向走了：{v:?}");
    }

    /// 顺向的小修一帧到位（跟手、也是被打断之后立刻恢复的那条路），
    /// 顺向的大跨度按加速度起，掉头过零不会一帧弹满速。
    #[test]
    fn aligned_snaps_are_crisp_but_big_steps_ramp() {
        let dt = 1.0 / 60.0;
        let near = std::f32::consts::FRAC_PI_3; // 60°
        let target = [MOVE_SPEED_MPS * near.sin(), MOVE_SPEED_MPS * near.cos()];
        let mut v = [0.0, MOVE_SPEED_MPS];
        let mut ticks = 0;
        while (v[0] - target[0]).abs() > 1e-4 || (v[1] - target[1]).abs() > 1e-4 {
            v = ground_velocity_step(v, target, REVERSE_ACCEL_MPS2, dt);
            ticks += 1;
            assert!(ticks < 30, "60° 变向 0.5s 内该到位：{v:?}");
        }
        assert!(ticks <= 12, "60° 变向要跟手（≤0.2s），实测 {ticks} tick");

        // 小跨度（正常跑动摇杆微抖、同步偏置）：一 tick 就到，别留尾巴
        let lateral = [0.2f32, MOVE_SPEED_MPS];
        let v = ground_velocity_step([0.0, MOVE_SPEED_MPS], lateral, REVERSE_ACCEL_MPS2, dt);
        assert_eq!(v, lateral, "0.2m/s 的顺向修正不该被摊成惯性");

        // 静止起步：宽度超过 SNAP_LEN 才走加速曲线，0 → 3.0 约 0.15 秒
        let mut v = [0.0, 0.0];
        for _ in 0..9 {
            v = ground_velocity_step(v, [0.0, MOVE_SPEED_MPS], REVERSE_ACCEL_MPS2, dt);
        }
        assert!(v[1] > MOVE_SPEED_MPS * 0.9, "起步 0.15s 该到顶速：{v:?}");
        let v = ground_velocity_step(
            [0.0, MOVE_SPEED_MPS],
            [0.0, MOVE_SPEED_MPS],
            REVERSE_ACCEL_MPS2,
            dt,
        );
        assert_eq!(v, [0.0, MOVE_SPEED_MPS]);
    }

    /// 掉头过零那几 tick 不许「一帧弹满速」：那正是真机上「最后一下抖/僵」的来源。
    ///
    /// 复现实测形态：速度已经被掰到 0.46 m/s、方向差不多垂直于目标
    /// （点积在 0 附近翻符号），旧实现会在翻正的那一 tick直接返回满速 2.49 m/s。
    #[test]
    fn reversal_never_snaps_back_to_full_speed() {
        let dt = 1.0 / 60.0;
        let target = [2.45f32, -0.42];
        let mut v = [-0.08f32, -0.45];
        let before = (v[0] * v[0] + v[1] * v[1]).sqrt();
        v = ground_velocity_step(v, target, REVERSE_ACCEL_MPS2, dt);
        let after = (v[0] * v[0] + v[1] * v[1]).sqrt();
        assert!(
            after - before < REVERSE_ACCEL_MPS2 * dt + 1e-4,
            "一 tick 的加速不许超过加速度上限：{before} → {after}"
        );
        // 掉头全程逐 tick 检查：过零那一下会从 6 m/s² 切到顺向的 20 m/s²，
        // 所以上限按大的那个算 —— 但绝不允许一帧从 0.46 弹到 2.49（那是 120 m/s²）
        let mut v = [0.0f32, MOVE_SPEED_MPS];
        let mut prev = MOVE_SPEED_MPS;
        for _ in 0..90 {
            v = ground_velocity_step(v, [0.0, -MOVE_SPEED_MPS * 0.4], REVERSE_ACCEL_MPS2, dt);
            let now = (v[0] * v[0] + v[1] * v[1]).sqrt();
            assert!(
                (now - prev).abs() <= GROUND_ACCEL_MPS2 * dt + 1e-4,
                "掉头途中速率跳变：{prev} → {now}"
            );
            prev = now;
        }
    }

    #[test]
    fn turn_brake_knobs_shift_the_curve() {
        // 到位角调大：同一个 60° 夹角刹得更少
        let late = turn_speed_scale_with(60.0, 150.0, 0.25);
        let early = turn_speed_scale_with(60.0, 60.0, 0.25);
        assert!(
            late > early,
            "到位角提前后同一夹角要降更多：{early} vs {late}"
        );
        assert_eq!(early, 0.25, "到位角等于当前夹角时应已压到下限");
        let hard = turn_speed_scale_with(180.0, 90.0, 0.2);
        let soft = turn_speed_scale_with(180.0, 90.0, 1.0);
        assert!((hard - 0.2).abs() < 1e-6);
        assert!((soft - 1.0).abs() < 1e-6, "下限 1.0 = 完全不降速");
    }

    /// 斜坡：刹车约 0.34 秒压到下限，提速约 0.7 秒回满，两者不对称
    #[test]
    fn turn_brake_follows_the_body_and_brakes_harder_than_it_recovers() {
        // 180° 甩头：身体按 step_yaw 转，刹车按本 tick 转速给力
        let dt = 1.0 / 60.0;
        let mut yaw = 0.0f32;
        let mut rate = 0.0f32;
        let mut scale = 1.0f32;
        let mut secs = 0.0f32;
        let mut floor_at = None;
        for _ in 0..600 {
            let error = wrap_pi(std::f32::consts::PI - yaw);
            let target = turn_speed_scale(error.to_degrees());
            scale = ramp_turn_brake(scale, target, rate, dt);
            if floor_at.is_none() && scale <= TURN_BRAKE_MIN_SCALE_DEFAULT + 1e-4 {
                floor_at = Some(secs);
            }
            if scale <= TURN_BRAKE_MIN_SCALE_DEFAULT + 1e-4 {
                break;
            }
            let (next, next_rate) = step_yaw(yaw, rate, error, dt);
            yaw = next;
            rate = next_rate;
            secs += dt;
        }
        let floor_at = floor_at.expect("2.2/s 的刹车力最终要压到下限");
        // 转速爬满之前不许刹狠：顶速/加速度就是爬满的时间（这里 0.26 秒），
        // 刹车力最早也只能在它之后再到底，否则「人还没转就开始刹车」= 原地站住
        let spin_up = TURN_SPEED_DEG_PER_SEC / TURN_ACCEL_DEG_PER_SEC2;
        assert!(
            floor_at > spin_up * 1.05,
            "刹车没等身体转起来就到底：{floor_at:.2}s（爬满要 {spin_up:.2}s）"
        );

        // 提速不受转速门控，且必须比刹车慢
        let mut rise = 0;
        while scale < 1.0 - 1e-4 && rise < 600 {
            scale = ramp_turn_brake(scale, 1.0, 0.0, dt);
            rise += 1;
        }
        // 从下限（`TURN_BRAKE_MIN_SCALE_DEFAULT`）爬回 1.0 要按 `TURN_BRAKE_RISE_PER_SEC`
        // 走完这段距离：0.4 → 1.0 就是 0.55 秒，33 个 60Hz tick
        assert!(rise > 30, "回满太快：{rise} 个 60Hz tick");
    }
}
