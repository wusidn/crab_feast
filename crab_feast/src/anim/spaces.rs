//! 本作的混合空间定义：参数轴、取值范围与默认采样点。
//!
//! 这些是**游戏内容**（剪辑名、档位划分、界面文案都跟着本作角色走）；
//! `super::core` 那边只留 `BlendSpaceSpec` / `ParamSpec` 这两个与具体作品无关的
//! 声明类型。
//!
//! 新增一个混合空间 = 往 [`game_spaces`] 里加一条注册项；求值、UI、存档都是通用的。

use super::core::{BlendSpaceSpec, ParamSpec};

pub fn game_spaces() -> Vec<BlendSpaceSpec> {
    vec![
        BlendSpaceSpec {
            id: "locomotion".to_string(),
            title: "移动混合".to_string(),
            description: "移动混合：朝向 × 速度，yaw 覆盖整圈（±180°），速度恒为非负"
                .to_string(),
            params: [
                ParamSpec::new(
                    "yaw",
                    "朝向",
                    "移动方向相对角色正前方的偏角：0 正前、-90 正左、90 正右、±180 正后；整圈连续，不做折回",
                    -180.0,
                    180.0,
                    0.0,
                ),
                ParamSpec::new(
                    "speed",
                    "速度",
                    "沿当前朝向的速度量级：0 静止、0.5 走档、1 跑档；倒退由「反放」采样承担，不用负值",
                    0.0,
                    1.0,
                    0.5,
                ),
            ],
            speed: 1.0,
            // 7 列朝向 × 走/跑两档，斜向由相邻两点插值合成，不给没有素材的方向
            // 硬绑一个剪辑。
            //
            // ±180 两列是「反放的走 / 跑」：人朝前、脚往后迈，就是倒退。
            //
            // 素材里没有「45° 斜后走」这类剪辑，所以只要两套步法同时有权重，
            // 腿就必然处在「交叉迈步」的中间态。这个中间态没法消掉，只能压短：
            //  0°~60°   钉死正走（重复采样同一剪辑 → 权重独占）
            //  60°~90°  30° 交叉淡入（180°/s 下 0.17s）换到横移
            //  90°~150° 钉死横移
            //  150°~180° 30° 交叉淡出，换到反放走路（真正的倒退）
            // 以前 0°~90° 全程线性淡入，交叉态持续 0.5s，腿一直是「半走半步」。
            //
            // 关键：速度**不再用符号表示前后**。以前 180° 要折回 ±90 再给负速度，
            // 那条路径必然穿过 speed=0，而 speed=0 上只有 Idle——转身时腿会闪一下。
            // 现在整圈连续，转身是参数沿圆周从 180 滑到 0。
            // speed=0 每一列都放 Idle：整行都是 Idle 才算「静止」，
            // 否则某个朝向静止会退化成 0.5 行的侧移姿势。
            default_samples: vec![
                ([-180.0, 0.0], "Idle".to_string(), false),
                ([-150.0, 0.0], "Idle".to_string(), false),
                ([-90.0, 0.0], "Idle".to_string(), false),
                ([-60.0, 0.0], "Idle".to_string(), false),
                ([0.0, 0.0], "Idle".to_string(), false),
                ([60.0, 0.0], "Idle".to_string(), false),
                ([90.0, 0.0], "Idle".to_string(), false),
                ([150.0, 0.0], "Idle".to_string(), false),
                ([180.0, 0.0], "Idle".to_string(), false),
                ([-150.0, 0.5], "LeftStrafeWalking".to_string(), false),
                ([-90.0, 0.5], "LeftStrafeWalking".to_string(), false),
                ([-60.0, 0.5], "Walking".to_string(), false),
                ([0.0, 0.5], "Walking".to_string(), false),
                ([60.0, 0.5], "Walking".to_string(), false),
                ([90.0, 0.5], "RightStrafeWalking".to_string(), false),
                ([150.0, 0.5], "RightStrafeWalking".to_string(), false),
                ([-180.0, 0.5], "Walking".to_string(), true),
                ([180.0, 0.5], "Walking".to_string(), true),
                ([-150.0, 1.0], "LeftStrafe".to_string(), false),
                ([-90.0, 1.0], "LeftStrafe".to_string(), false),
                ([-60.0, 1.0], "Running".to_string(), false),
                ([0.0, 1.0], "Running".to_string(), false),
                ([60.0, 1.0], "Running".to_string(), false),
                ([90.0, 1.0], "RightStrafe".to_string(), false),
                ([150.0, 1.0], "RightStrafe".to_string(), false),
                ([-180.0, 1.0], "Running".to_string(), true),
                ([180.0, 1.0], "Running".to_string(), true),
            ],
        },
        BlendSpaceSpec {
            id: "look".to_string(),
            title: "看向混合".to_string(),
            description: "朝向偏移：偏航与俯仰，通常只驱动上半身或头颈".to_string(),
            params: [
                ParamSpec::new("yaw", "偏航", "左转 / 右转的朝向输入", -1.0, 1.0, 0.0),
                ParamSpec::new("pitch", "俯仰", "低头 / 抬头的朝向输入", -1.0, 1.0, 0.0),
            ],
            speed: 1.0,
            default_samples: vec![
                ([0.0, 0.0], "Idle".to_string(), false),
                ([-1.0, 0.0], "LeftTurn".to_string(), false),
                ([1.0, 0.0], "RightTurn".to_string(), false),
            ],
        },
        BlendSpaceSpec {
            id: "aim".to_string(),
            title: "瞄准混合".to_string(),
            description: "瞄准偏移：映射到可用的瞄准剪辑，默认留空由编辑者指派".to_string(),
            params: [
                ParamSpec::new("aim_yaw", "瞄准横向", "瞄准方向的水平偏移", -1.0, 1.0, 0.0),
                ParamSpec::new(
                    "aim_pitch",
                    "瞄准纵向",
                    "瞄准方向的垂直偏移",
                    -1.0,
                    1.0,
                    0.0,
                ),
            ],
            speed: 1.0,
            default_samples: Vec::new(),
        },
    ]
}
