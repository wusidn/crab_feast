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
            description: "移动混合：朝向 × 速度，yaw 只覆盖前后半球的左右偏角，前后由速度符号承担"
                .to_string(),
            params: [
                ParamSpec::new(
                    "yaw",
                    "朝向",
                    "移动方向相对角色正前方的偏角：0 正前、-90 正左、90 正右；超过 90 的部分等价于速度取负。speed=0 行的 ±90 是原地转向剪辑",
                    -90.0,
                    90.0,
                    0.0,
                ),
                ParamSpec::new(
                    "speed",
                    "速度",
                    "沿当前朝向的带符号速度：0 静止、±0.5 走档、±1 跑档，负值即倒退（倒着走）",
                    -1.0,
                    1.0,
                    0.5,
                ),
            ],
            speed: 1.0,
            // 规则网格：三列朝向 × 带符号的走/跑两档。负速度层就是把同一档剪辑
            // 的播放方向翻转（人没转身、脚往回跨）。斜向不单独放点，
            // 由相邻两列的插值合成，避免给没有素材的方向硬绑一个剪辑。
            // speed=0 行的左右两端放转向剪辑：只有「松手 + 朝向没对上」会采样到这里，
            // 中间仍是 Idle，所以小角度误差只微微侧身，大角度才真的转身。
            default_samples: vec![
                ([-90.0, 0.0], "LeftTurn".to_string(), false),
                ([0.0, 0.0], "Idle".to_string(), false),
                ([90.0, 0.0], "RightTurn".to_string(), false),
                ([-90.0, -0.5], "LeftStrafeWalking".to_string(), true),
                ([0.0, -0.5], "Walking".to_string(), true),
                ([90.0, -0.5], "RightStrafeWalking".to_string(), true),
                ([-90.0, 0.5], "LeftStrafeWalking".to_string(), false),
                ([0.0, 0.5], "Walking".to_string(), false),
                ([90.0, 0.5], "RightStrafeWalking".to_string(), false),
                ([-90.0, -1.0], "LeftStrafe".to_string(), true),
                ([0.0, -1.0], "Running".to_string(), true),
                ([90.0, -1.0], "RightStrafe".to_string(), true),
                ([-90.0, 1.0], "LeftStrafe".to_string(), false),
                ([0.0, 1.0], "Running".to_string(), false),
                ([90.0, 1.0], "RightStrafe".to_string(), false),
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
