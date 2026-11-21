//! 跳跃剪辑资源参数表：一种跳跃姿态 = 一份剪辑资源 + 一套参数。
//!
//! 资源位置：`assets/characters/animations/<角色>/<clip>.glb#Animation0`。
//! 表里的 [`JumpPoseDef::clip`] 就是 glb 文件名主干，也是运行时 `ClipSet` 的剪辑名；
//! 表与资源一一对应，单测会扫描 `assets/` 检查每一行都有对应文件。
//!
//! # 新增一种跳跃姿态
//!
//! 1. **资源**：把 `<clip>.glb` 放进每个角色的动画目录（`scripts/` 里有程序化派生脚本）；
//! 2. **枚举**：在 [`JumpPose`] 里加一个变体，并把它追加到 [`JumpPose::ALL`]；
//! 3. **参数**：在 [`POSES`] 末尾加一行 [`JumpPoseDef`]，填剪辑名、适用大类
//!    （[`JumpKind`]）、手感档（[`JumpProfile`]）、同档套别（[`JumpPoseDef::variant`]）、
//!    支撑脚标签、时间轴锚点、这一档的默认手感参数与缺件回退。
//!
//! 逻辑层（`jump/`）只按 [`JumpPose`] 查表：剪辑名、锚点、蓄力策略、回退链、
//! 起跳分流阈值都从这里来，状态机里不再写死任何剪辑名或剪辑专属数字。
//!
//! # 时间轴锚点（`scripts/jump_params.py` 240Hz 复算，Amy 标定）
//!
//! Mixamo 逐帧重定向，跨角色时长与归一化相位一致。锚点是剪辑本地秒，
//! 运行时 `begin` 会再用髋高曲线跑一遍基线事件检测校正，检测不到才回退表内默认值。
//! 表内数字最初由 64 帧探针（`scripts/probe_jump_clips.py`）标定，与 240Hz 复算
//! 最多差一格（0.03~0.05s）；`scripts/jump_params.py --check` 按这个容差对账。
//!
//! | 剪辑 | 时长 | 离地 | 落地 | 蓄力段 | 说明 |
//! |---|---|---|---|---|---|
//! | `Jumping` | 1.900s | 0.543 | 1.116 | 1:1 保留 | 先下蹲（低点 ~0.30s）再蹬伸，落地后恢复 |
//! | `RunningJump1` | 0.900s | 0.025 | 0.658 | 1:1 保留 | 起手即蹬伸，几乎无蓄力；右脚蹬地 |
//! | `RunningJump2` | 1.000s | 0.017 | 0.700 | 1:1 保留 | 腾空更久、离地更陡；左脚蹬地 |
//! | `RunningJump3` | 0.867s | 0.038 | 0.633 | 1:1 保留 | 最短一跳、恢复段最短；右脚蹬地 |
//! | `JumpingUp` | 0.833s | 0.516 | 0.833 | 1:1 保留 | 蹲姿后登上高台，落地即结束 |
//! | `JumpingDown` | 3.067s | 0.876 | 1.168 | 压缩 | 高台准备段后下落，长尾恢复 |
//!
//! 奔跑跳的三套各有左 / 右镜像派生片（`RunningJumpXLeft` / `RunningJumpXRight`），
//! 与源片同长同锚点；镜像由 `scripts/make_runjump_mirror.py` 反射整套 FK 骨架生成。
//! 连续起跳按起跳序号在 1 → 2 → 3 之间轮换（见 [`RUN_VARIANTS`]），同一套里再按
//! 支撑脚选源片或镜像片。
//!
//! 地面深蹲类剪辑的蓄力段（下蹲→蹬伸）按剪辑原速播放；高台准备段不展示深蹲，
//! 只按 [`Anticipation::CompressTo`] 给的时长播。

use super::{JumpKind, JumpProfile, JumpTuning};

/// 一种跳跃姿态，对应一条剪辑资源。
///
/// 变体顺序必须与 [`POSES`] 的行顺序一致（[`JumpPose::index`] 直接拿它当下标，
/// 有单测兜底）。新增姿态时两处一起加。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JumpPose {
    /// 原地 / 慢速双脚跳
    Stand,
    /// 奔跑跳第 1 套（右脚蹬地）
    Run1,
    /// 奔跑跳第 1 套：左脚镜像
    Run1Left,
    /// 奔跑跳第 2 套（左脚蹬地）
    Run2,
    /// 奔跑跳第 2 套：右脚镜像
    Run2Right,
    /// 奔跑跳第 3 套（右脚蹬地）
    Run3,
    /// 奔跑跳第 3 套：左脚镜像
    Run3Left,
    /// 跳上高台
    Up,
    /// 跳下高台
    Down,
}

impl JumpPose {
    /// 全部姿态，顺序即表下标。
    pub const ALL: [JumpPose; 9] = [
        JumpPose::Stand,
        JumpPose::Run1,
        JumpPose::Run1Left,
        JumpPose::Run2,
        JumpPose::Run2Right,
        JumpPose::Run3,
        JumpPose::Run3Left,
        JumpPose::Up,
        JumpPose::Down,
    ];

    /// 姿态数量（表长）。
    pub const COUNT: usize = Self::ALL.len();

    /// 在 [`POSES`] 里的下标。
    pub const fn index(self) -> usize {
        match self {
            JumpPose::Stand => 0,
            JumpPose::Run1 => 1,
            JumpPose::Run1Left => 2,
            JumpPose::Run2 => 3,
            JumpPose::Run2Right => 4,
            JumpPose::Run3 => 5,
            JumpPose::Run3Left => 6,
            JumpPose::Up => 7,
            JumpPose::Down => 8,
        }
    }

    /// 这一姿态的资源参数行。
    pub fn def(self) -> &'static JumpPoseDef {
        &POSES[self.index()]
    }

    /// 剪辑名（= 资源文件名主干）。
    pub fn clip(self) -> &'static str {
        self.def().clip
    }

    /// 这一姿态的剪辑时间轴 / 分段策略。
    pub fn spec(self) -> &'static JumpClipSpec {
        &self.def().spec
    }

    /// 这一姿态的默认手感参数。
    pub fn tuning(self) -> JumpTuning {
        self.def().tuning
    }

    /// 缺件时退到哪一档姿态；`None` 表示没有更远的回退（逻辑层最后兜底到原地跳）。
    pub fn fallback(self) -> Option<JumpPose> {
        self.def().fallback
    }

    /// 剪辑名 → 姿态。表里没有的名字（新加资源但没登记）返回 `None`。
    pub fn from_clip(clip: &str) -> Option<JumpPose> {
        POSES
            .iter()
            .find(|def| def.clip == clip)
            .map(|def| def.pose)
    }

    /// 按「大类 + 手感档 + 支撑脚」在表里挑姿态。
    ///
    /// 先找支撑脚精确匹配的派生行；没有就退到同档的对称版本（`SupportFoot::Any`）；
    /// 再退到同大类同档的第一行；表里完全没有时兜底到 [`JumpPose::Stand`]。
    /// 按「大类 + 手感档 + 支撑脚 + 套别」在表里挑姿态。
    ///
    /// 套别（[`JumpPoseDef::variant`]）只在奔跑档有多个候选：连续起跳时按序号轮换，
    /// 避免每一跳都长一个样。其他档位全部 `variant = 0`，传非零值会退到第一套。
    ///
    /// 先找支撑脚精确匹配的派生行；没有就退到同档的对称版本（`SupportFoot::Any`）；
    /// 再退到同套的第一行（基础片）；那一套整行都没有时退回该档第一套；
    /// 表里完全没有时兜底到 [`JumpPose::Stand`]。
    pub fn select(
        kind: JumpKind,
        profile: JumpProfile,
        support: SupportFoot,
        variant: u8,
    ) -> JumpPose {
        let rows = |variant: u8| {
            POSES.iter().filter(move |def| {
                def.kind == kind && def.profile == profile && def.variant == variant
            })
        };
        rows(variant)
            .find(|def| def.support == support)
            .or_else(|| {
                rows(variant).find(|def| def.support == SupportFoot::Any)
            })
            .or_else(|| rows(variant).next())
            .or_else(|| {
                // 套别越界（或者这一档还没有派生套）：退回这一档的第一套
                POSES.iter().find(|def| {
                    def.kind == kind
                        && def.profile == profile
                        && def.variant == 0
                        && def.support == support
                })
            })
            .or_else(|| {
                POSES.iter().find(|def| {
                    def.kind == kind && def.profile == profile && def.variant == 0
                })
            })
            .or_else(|| POSES.iter().find(|def| def.kind == kind && def.profile == profile))
            .map(|def| def.pose)
            .unwrap_or(JumpPose::Stand)
    }
}

/// 支撑脚标签：在同类姿态里挑「这一脚蹬地 / 着地」的派生版本。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SupportFoot {
    /// 不区分 / 对称版本
    #[default]
    Any,
    Left,
    Right,
}

/// 奔跑跳的套数：连续起跳按这个数轮换剪辑，避免每一跳都长一个样。
pub const RUN_VARIANTS: u8 = 3;

/// 蓄力段（下蹲→蹬伸 / 高台准备）的播放策略。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anticipation {
    /// 1:1 保留剪辑时长：地面深蹲要按原速看到下蹲→蹬伸。
    Preserve,
    /// 压缩到固定时长（秒）：不展示深蹲的剪辑（下跳的高台准备段）。
    CompressTo(f32),
}

/// 单条跳跃剪辑的时间轴锚点（剪辑本地秒）与分段策略。
#[derive(Clone, Copy, Debug)]
pub struct JumpClipSpec {
    /// 动作起点（站立 / 预备帧）
    pub entry: f32,
    /// 默认离地帧（双脚蹬离地面）
    pub takeoff: f32,
    /// 默认落地帧（双脚回到地面）
    pub land: f32,
    /// 剪辑结束帧
    pub end: f32,
    /// 蓄力段策略
    pub anticipation: Anticipation,
    /// 落地恢复段压缩后的目标时长（秒）
    pub recover_secs: f32,
}

impl JumpClipSpec {
    /// 表内的名义蓄力段时长（秒）：1:1 保留时取 `takeoff - entry`，
    /// 压缩时取 [`Anticipation::CompressTo`] 的值。
    ///
    /// 运行时 `Timeline` 会用实测髋高曲线校正离地帧，
    /// 届时按校正后的 `takeoff` 重新算，这里只是表的默认值。
    pub const fn take_secs(self) -> f32 {
        match self.anticipation {
            Anticipation::Preserve => self.takeoff - self.entry,
            Anticipation::CompressTo(secs) => secs,
        }
    }
}

/// 一条跳跃姿态的全部资源参数。表里一行 = 一条剪辑资源。
#[derive(Clone, Copy, Debug)]
pub struct JumpPoseDef {
    /// 姿态键（与 [`POSES`] 行下标对齐）
    pub pose: JumpPose,
    /// 剪辑资源名：`<角色动画目录>/<clip>.glb#Animation0`
    pub clip: &'static str,
    /// 适用的大类（平地 / 上 / 下）
    pub kind: JumpKind,
    /// 手感调参档
    pub profile: JumpProfile,
    /// 同档内的套别（0 起）。只有奔跑档有多套（见 [`RUN_VARIANTS`]）：
    /// 连续起跳时按起跳序号轮换，同一套里再按支撑脚挑左 / 右派生。
    pub variant: u8,
    /// 起跳支撑脚：派生片的挑选依据；对称版本填 [`SupportFoot::Any`]
    pub support: SupportFoot,
    /// 剪辑末帧默认着地脚：步态数据缺失时，落地相位对齐的兜底约定。
    /// [`SupportFoot::Any`] = 沿用旧约定（按右脚对齐）。
    pub landing_foot: SupportFoot,
    /// 时间轴锚点与分段策略
    pub spec: JumpClipSpec,
    /// 这一档的默认手感参数（[`JumpTunings`](super::JumpTunings) 的初值）
    pub tuning: JumpTuning,
    /// 缺件回退链的下一环
    pub fallback: Option<JumpPose>,
}

impl JumpPoseDef {
    /// 该剪辑在角色动画目录下的资产路径：`<animations>/<clip>.glb#Animation0`。
    pub fn asset_path(&self, animations: &str) -> String {
        format!("{}/{}.glb#Animation0", animations, self.clip)
    }
}

/// 本地 Dynamic 角色的起跳初速度（m/s）。
///
/// 这是**下限**而不是兜底：实测速度只有在明显高于它（真的被别的力弹起来）
/// 时才采用，噪声级的小正值一律按这个速度起跳。低于它的起跳会变成几厘米的
/// 抽搐——「按了跳，人只抬了一下」。
const LAUNCH_VY: f32 = 5.0;
/// 远端 Kinematic 复制体无速度组件，按服务端高跳速度预测滞空（m/s）。
const NET_LAUNCH_VY: f32 = 5.0;
/// 滞空预测系数：对称抛物线 `2·vy/g` 再乘这个缓冲。
///
/// 实测联网跑跳（起跳 5.0 m/s、阻尼 0.3）真正滞空 0.95s，而 `2·5/9.81`
/// = 1.02s；乘 1.15 会预测成 1.17s，剪辑整段腾空被压成 0.54× 慢放，
/// **落地那一刻剪辑才播到 82%**：脚还在"往前伸着没踩实"的姿态上，收尾
/// 0.12s 被丢掉——画面就是人已经落地了，腿还在空中多迈一步。
/// 取 0.95 让剪辑的落地帧与真实触地落在同一时刻，剩下的误差由
/// 「钉在落地帧等接地」兜住。
const AIR_PREDICT: f32 = 0.95;
/// 空中姿态慢放系数下限：再低就成慢镜头了。
const SLOW_MIN: f32 = 0.35;
/// 滞空兜底上限（秒）：射线失效 / 落在高台边缘探测不到地面时，超时强制结束滞空段。
const MAX_AIR_HOLD: f32 = 2.0;

/// 联网蓄力段（秒）：根由服务端抬升，动画离地帧必须落在同一时刻，
/// 否则「人已经飞起来还在蹲」。与服务端 `drive_avatars` 的两档蓄力一致。
const STAND_TAKE: f32 = 0.45;
const RUN_TAKE: f32 = 0.05;
/// `JumpingUp` 蹲姿到离地 0.516s 是这一档自己的节奏，不跟平地的 0.45s 走。
const UP_TAKE: f32 = 0.516;
/// 下跳高台准备段压缩到 0.16s。
const DOWN_TAKE: f32 = 0.16;

/// 原地 / 慢走跳（`Jumping`）。
const TUNING_STAND: JumpTuning = JumpTuning {
    launch_vy: LAUNCH_VY,
    net_launch_vy: NET_LAUNCH_VY,
    take_secs: Some(STAND_TAKE),
    charge_secs: STAND_TAKE,
    air_predict: AIR_PREDICT,
    slow_min: SLOW_MIN,
    max_air_hold: MAX_AIR_HOLD,
};

/// 奔跑跳（`RunningJump1/2/3` 三套及其镜像派生共用同一档手感）。
const TUNING_RUN: JumpTuning = JumpTuning {
    launch_vy: LAUNCH_VY,
    net_launch_vy: NET_LAUNCH_VY,
    take_secs: Some(RUN_TAKE),
    charge_secs: RUN_TAKE,
    air_predict: AIR_PREDICT,
    slow_min: SLOW_MIN,
    max_air_hold: MAX_AIR_HOLD,
};

/// 跳上高台（`JumpingUp`）。
const TUNING_UP: JumpTuning = JumpTuning {
    launch_vy: LAUNCH_VY,
    net_launch_vy: NET_LAUNCH_VY,
    take_secs: Some(UP_TAKE),
    charge_secs: UP_TAKE,
    air_predict: AIR_PREDICT,
    slow_min: SLOW_MIN,
    max_air_hold: MAX_AIR_HOLD,
};

/// 跳下高台（`JumpingDown`）。
const TUNING_DOWN: JumpTuning = JumpTuning {
    launch_vy: LAUNCH_VY,
    net_launch_vy: NET_LAUNCH_VY,
    take_secs: Some(DOWN_TAKE),
    charge_secs: DOWN_TAKE,
    air_predict: AIR_PREDICT,
    slow_min: SLOW_MIN,
    max_air_hold: MAX_AIR_HOLD,
};

/// **资源参数表**。一行 = 一条跳跃剪辑资源；逻辑层只按 [`JumpPose`] 查这张表。
pub static POSES: [JumpPoseDef; JumpPose::COUNT] = [
    JumpPoseDef {
        pose: JumpPose::Stand,
        clip: "Jumping",
        kind: JumpKind::Flat,
        profile: JumpProfile::Stand,
        variant: 0,
        support: SupportFoot::Any,
        landing_foot: SupportFoot::Any,
        spec: JumpClipSpec {
            entry: 0.0,
            takeoff: 0.543,
            land: 1.116,
            end: 1.900,
            anticipation: Anticipation::Preserve,
            recover_secs: 0.22,
        },
        tuning: TUNING_STAND,
        fallback: None,
    },
    JumpPoseDef {
        pose: JumpPose::Run1,
        clip: "RunningJump1",
        kind: JumpKind::Flat,
        profile: JumpProfile::Run,
        variant: 0,
        // 原片右腿蹬地、末帧右脚撑地（实测见 `landing_alignment` 的注释）
        support: SupportFoot::Right,
        landing_foot: SupportFoot::Right,
        spec: JumpClipSpec {
            entry: 0.0,
            takeoff: 0.025,
            land: 0.658,
            // 恢复段 = 落地缓冲段长度（end − land = 0.242s），按 1× 播完
            end: 0.900,
            anticipation: Anticipation::Preserve,
            recover_secs: 0.24,
        },
        tuning: TUNING_RUN,
        fallback: Some(JumpPose::Stand),
    },
    JumpPoseDef {
        pose: JumpPose::Run1Left,
        clip: "RunningJump1Left",
        kind: JumpKind::Flat,
        profile: JumpProfile::Run,
        variant: 0,
        support: SupportFoot::Left,
        landing_foot: SupportFoot::Left,
        // 镜像派生片与源片同长同锚点
        spec: JumpClipSpec {
            entry: 0.0,
            takeoff: 0.025,
            land: 0.658,
            end: 0.900,
            anticipation: Anticipation::Preserve,
            recover_secs: 0.24,
        },
        tuning: TUNING_RUN,
        fallback: Some(JumpPose::Run1),
    },
    JumpPoseDef {
        pose: JumpPose::Run2,
        clip: "RunningJump2",
        kind: JumpKind::Flat,
        profile: JumpProfile::Run,
        variant: 1,
        // 第 2 套原片是左脚蹬地（实测左踝先离地），镜像片补右脚
        support: SupportFoot::Left,
        landing_foot: SupportFoot::Left,
        spec: JumpClipSpec {
            entry: 0.0,
            takeoff: 0.017,
            land: 0.700,
            end: 1.000,
            anticipation: Anticipation::Preserve,
            recover_secs: 0.30,
        },
        tuning: TUNING_RUN,
        fallback: Some(JumpPose::Run1),
    },
    JumpPoseDef {
        pose: JumpPose::Run2Right,
        clip: "RunningJump2Right",
        kind: JumpKind::Flat,
        profile: JumpProfile::Run,
        variant: 1,
        support: SupportFoot::Right,
        landing_foot: SupportFoot::Right,
        // 镜像派生片与源片同长同锚点
        spec: JumpClipSpec {
            entry: 0.0,
            takeoff: 0.017,
            land: 0.700,
            end: 1.000,
            anticipation: Anticipation::Preserve,
            recover_secs: 0.30,
        },
        tuning: TUNING_RUN,
        fallback: Some(JumpPose::Run2),
    },
    JumpPoseDef {
        pose: JumpPose::Run3,
        clip: "RunningJump3",
        kind: JumpKind::Flat,
        profile: JumpProfile::Run,
        variant: 2,
        // 第 3 套原片右脚蹬地、腾空段更短（0.867s），落点比前两套更靠前
        support: SupportFoot::Right,
        landing_foot: SupportFoot::Right,
        spec: JumpClipSpec {
            entry: 0.0,
            takeoff: 0.038,
            land: 0.633,
            end: 0.867,
            anticipation: Anticipation::Preserve,
            recover_secs: 0.23,
        },
        tuning: TUNING_RUN,
        fallback: Some(JumpPose::Run1),
    },
    JumpPoseDef {
        pose: JumpPose::Run3Left,
        clip: "RunningJump3Left",
        kind: JumpKind::Flat,
        profile: JumpProfile::Run,
        variant: 2,
        support: SupportFoot::Left,
        landing_foot: SupportFoot::Left,
        // 镜像派生片与源片同长同锚点
        spec: JumpClipSpec {
            entry: 0.0,
            takeoff: 0.038,
            land: 0.633,
            end: 0.867,
            anticipation: Anticipation::Preserve,
            recover_secs: 0.23,
        },
        tuning: TUNING_RUN,
        fallback: Some(JumpPose::Run3),
    },
    JumpPoseDef {
        pose: JumpPose::Up,
        clip: "JumpingUp",
        kind: JumpKind::Up,
        profile: JumpProfile::Up,
        variant: 0,
        support: SupportFoot::Any,
        landing_foot: SupportFoot::Any,
        spec: JumpClipSpec {
            entry: 0.0,
            // 蹲姿到离地 0.516s 是这一档自己的节奏，不跟平地的 0.45s 走
            takeoff: 0.516,
            land: 0.833,
            end: 0.833,
            anticipation: Anticipation::Preserve,
            recover_secs: 0.05,
        },
        tuning: TUNING_UP,
        fallback: Some(JumpPose::Stand),
    },
    JumpPoseDef {
        pose: JumpPose::Down,
        clip: "JumpingDown",
        kind: JumpKind::Down,
        profile: JumpProfile::Down,
        variant: 0,
        support: SupportFoot::Any,
        landing_foot: SupportFoot::Any,
        spec: JumpClipSpec {
            entry: 0.0,
            takeoff: 0.876,
            land: 1.168,
            end: 3.067,
            // 高台准备段不展示深蹲，压缩到 0.16s
            anticipation: Anticipation::CompressTo(0.16),
            recover_secs: 0.30,
        },
        tuning: TUNING_DOWN,
        fallback: Some(JumpPose::Stand),
    },
];

impl JumpPose {
    /// 当前表里注册的全部姿态（= 全部跳跃剪辑资源）。
    pub fn defs() -> &'static [JumpPoseDef] {
        &POSES
    }
}

/// 某一手感档的默认参数：取该档在表里的第一行（派生片与基础片共用同一套）。
pub fn default_tuning(profile: JumpProfile) -> JumpTuning {
    POSES
        .iter()
        .find(|def| def.profile == profile)
        .map_or_else(JumpTuning::default, |def| def.tuning)
}

/// 起跳分流与剪辑选择的规则参数：不属于某一条剪辑，属于整套跳跃资源的选择规则。
#[derive(Clone, Copy, Debug)]
pub struct JumpRules {
    /// 平地跳在「站 / 跑」两套剪辑之间的地速分界（m/s）。
    /// 走路档（~1.15）落在静止侧，奔跑档（~3.0）落在奔跑侧。
    pub run_jump_speed: f32,
    /// 台阶高度阈值（米）：落点与当前地面高差绝对值超过它才判上 / 下平台。
    /// 普通小碎步高差在几厘米内，1m 高的关卡台阶远超此值。
    pub step_height: f32,
    /// 落点向前探测距离（米）：沿当前移动 / 朝向方向前瞻一段再打地面射线。
    pub land_probe_dist: f32,
}

impl Default for JumpRules {
    fn default() -> Self {
        RULES
    }
}

/// 全局规则参数表（默认值即当前调好的手感）。约束成 `const` 是为了让服务端 / 测试
/// 能把它当编译期常量引用，两端不必各抄一份数字。
pub const RULES: JumpRules = JumpRules {
    run_jump_speed: 1.5,
    step_height: 0.25,
    land_probe_dist: 0.0,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// 表行顺序与枚举下标必须对齐，否则 `def()` 会拿错行
    #[test]
    fn table_rows_line_up_with_enum() {
        assert_eq!(POSES.len(), JumpPose::COUNT);
        for (index, def) in POSES.iter().enumerate() {
            assert_eq!(def.pose, JumpPose::ALL[index], "第 {index} 行姿态错位");
        }
        for pose in JumpPose::ALL {
            assert_eq!(pose.def().pose, pose);
        }
    }

    /// 剪辑名唯一且能反查回同一个姿态
    #[test]
    fn clip_names_are_unique_and_reversible() {
        for pose in JumpPose::ALL {
            assert_eq!(
                JumpPose::from_clip(pose.clip()),
                Some(pose),
                "剪辑 {} 反查失败",
                pose.clip()
            );
        }
        for (index, def) in POSES.iter().enumerate() {
            for other in &POSES[index + 1..] {
                assert_ne!(def.clip, other.clip, "剪辑名重复：{}", def.clip);
            }
        }
        assert_eq!(JumpPose::from_clip("NotAJump"), None);
    }

    /// 表内时间轴单调、分段时长非负
    #[test]
    fn specs_are_monotonic() {
        for def in POSES {
            let spec = def.spec;
            assert!(spec.entry <= spec.takeoff, "{} entry 晚于离地", def.clip);
            assert!(spec.takeoff <= spec.land, "{} 离地晚于落地", def.clip);
            assert!(spec.land <= spec.end, "{} 落地晚于结尾", def.clip);
            assert!(spec.take_secs() >= 0.0, "{} 蓄力段为负", def.clip);
            assert!(spec.recover_secs >= 0.0, "{} 恢复段为负", def.clip);
        }
    }

    /// 回退链必须收敛：每一条都以原地跳或 `None` 收尾，不能成环
    #[test]
    fn fallback_chains_terminate() {
        for pose in JumpPose::ALL {
            let mut current = pose;
            for _ in 0..=JumpPose::COUNT {
                match current.fallback() {
                    Some(next) => {
                        assert_ne!(next, current, "{} 回退到自己", pose.clip());
                        current = next;
                    }
                    None => break,
                }
            }
            assert!(
                current.fallback().is_none(),
                "{} 的回退链成环或不收敛",
                pose.clip()
            );
        }
        for pose in [
            JumpPose::Run1Left,
            JumpPose::Run2Right,
            JumpPose::Run3Left,
        ] {
            // 镜像片退到自己的源片，不会横跳到别的套
            assert_eq!(pose.fallback(), Some(match pose {
                JumpPose::Run1Left => JumpPose::Run1,
                JumpPose::Run2Right => JumpPose::Run2,
                JumpPose::Run3Left => JumpPose::Run3,
                other => panic!("{other:?} 不在镜像片列表里"),
            }));
        }
        for pose in [JumpPose::Run2, JumpPose::Run3] {
            assert_eq!(pose.fallback(), Some(JumpPose::Run1));
        }
    }

    /// 每个大类 + 档位 + 支撑脚都能选到一个姿态，且落在表里
    #[test]
    fn select_covers_every_kind_and_support() {
        for kind in [JumpKind::Flat, JumpKind::Up, JumpKind::Down] {
            let profile = match kind {
                JumpKind::Flat => JumpProfile::Stand,
                JumpKind::Up => JumpProfile::Up,
                JumpKind::Down => JumpProfile::Down,
            };
            for support in [SupportFoot::Any, SupportFoot::Left, SupportFoot::Right] {
                let pose = JumpPose::select(kind, profile, support, 0);
                assert_eq!(pose.def().kind, kind);
                assert_eq!(pose.def().profile, profile);
            }
        }
        // 奔跑档：每一套里左 / 右派生精确命中，支撑脚分不出来时用该套的源片
        for (variant, left, right, base) in [
            (0u8, JumpPose::Run1Left, JumpPose::Run1, JumpPose::Run1),
            (1u8, JumpPose::Run2, JumpPose::Run2Right, JumpPose::Run2),
            (2u8, JumpPose::Run3Left, JumpPose::Run3, JumpPose::Run3),
        ] {
            assert_eq!(
                JumpPose::select(JumpKind::Flat, JumpProfile::Run, SupportFoot::Left, variant),
                left,
                "第 {variant} 套左脚"
            );
            assert_eq!(
                JumpPose::select(JumpKind::Flat, JumpProfile::Run, SupportFoot::Right, variant),
                right,
                "第 {variant} 套右脚"
            );
            assert_eq!(
                JumpPose::select(JumpKind::Flat, JumpProfile::Run, SupportFoot::Any, variant),
                base,
                "第 {variant} 套支撑脚未知"
            );
        }
        // 套别越界（只有一个套的档位也走这条）：退回第一套
        assert_eq!(
            JumpPose::select(JumpKind::Flat, JumpProfile::Stand, SupportFoot::Any, 7),
            JumpPose::Stand
        );
        assert_eq!(
            JumpPose::select(JumpKind::Flat, JumpProfile::Run, SupportFoot::Any, RUN_VARIANTS),
            JumpPose::Run1
        );
    }

    /// 表必须与磁盘资源放在一起：每一行的 glb 在每个角色目录下都要存在
    #[test]
    fn every_row_has_its_clip_asset() {
        let Some(root) = crate::anim::character::find_asset_root() else {
            // 没有资源目录的开发环境（比如脱离仓库运行单测）跳过
            return;
        };
        let animations_root = root.join("characters/animations");
        let Ok(entries) = std::fs::read_dir(&animations_root) else {
            return;
        };
        let mut characters = 0usize;
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            characters += 1;
            for def in POSES {
                let file = entry.path().join(format!("{}.glb", def.clip));
                assert!(
                    file.is_file(),
                    "资源参数表登记了 {}，但 {} 缺少 {}",
                    def.clip,
                    entry.path().display(),
                    file.display()
                );
            }
        }
        assert!(
            characters > 0,
            "{} 下没有角色目录",
            animations_root.display()
        );
    }
}
