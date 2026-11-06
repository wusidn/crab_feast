//! 代码里注册的混合空间定义：参数类型、取值范围、描述，以及默认采样点。
//!
//! 这里只有「空间长什么样」的声明，不含任何求值逻辑；采样点位置等可调配置
//! 由资源系统（`BlendSpaceConfig`）覆盖，缺失时回落到这里的默认值。

use serde::{Deserialize, Serialize};

use super::{BlendSpaceDoc, Sample2D};

/// 一个参数轴的定义
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParamSpec {
    /// 求值与存档用的键名，例如 `yaw`
    pub key: String,
    /// 界面上显示的短名
    pub label: String,
    /// 这个轴代表什么（给编辑者看）
    pub description: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

impl ParamSpec {
    pub fn new(
        key: &str,
        label: &str,
        description: &str,
        min: f32,
        max: f32,
        default: f32,
    ) -> Self {
        Self {
            key: key.to_string(),
            label: label.to_string(),
            description: description.to_string(),
            min,
            max,
            default,
        }
    }
}

/// 一个混合空间的注册项
#[derive(Clone, Debug)]
pub struct BlendSpaceSpec {
    /// 资源 id，也是存档文件名
    pub id: String,
    /// 显示名
    pub title: String,
    /// 一句话说明
    pub description: String,
    /// 两个参数轴，顺序即 (x, y)
    pub params: [ParamSpec; 2],
    pub speed: f32,
    /// 默认采样点：位置 + 期望的剪辑名 + 是否倒放（角色缺这个剪辑时跳过该点）
    pub default_samples: Vec<([f32; 2], String, bool)>,
}

impl BlendSpaceSpec {
    /// 按注册定义生成默认文档（剪辑名先原样放进去，由调用方按角色清单过滤）
    pub fn default_doc(&self) -> BlendSpaceDoc {
        BlendSpaceDoc {
            version: 1,
            params: [self.params[0].key.clone(), self.params[1].key.clone()],
            ranges: [
                [self.params[0].min, self.params[0].max],
                [self.params[1].min, self.params[1].max],
            ],
            samples: self
                .default_samples
                .iter()
                .map(|(at, clip, reverse)| Sample2D {
                    at: *at,
                    clip: clip.clone(),
                    reverse: *reverse,
                })
                .collect(),
            speed: self.speed,
        }
    }

    /// 默认参数值
    pub fn default_params(&self) -> [(&str, f32); 2] {
        [
            (self.params[0].key.as_str(), self.params[0].default),
            (self.params[1].key.as_str(), self.params[1].default),
        ]
    }
}

/// 系统启动时注册的内置空间。
///
/// 新增一个混合空间 = 往这里加一条注册项；求值、UI、存档、资源加载都是通用的。
pub fn builtin_spaces() -> Vec<BlendSpaceSpec> {
    vec![
        BlendSpaceSpec {
            id: "locomotion".to_string(),
            title: "移动混合".to_string(),
            description: "移动混合：朝向 × 速度，yaw 只覆盖前后半球的左右偏角，前后由速度符号承担".to_string(),
            params: [
                ParamSpec::new(
                    "yaw",
                    "朝向",
                    "移动方向相对角色正前方的偏角：0 正前、-90 正左、90 正右；超过 90 的部分等价于速度取负",
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
            default_samples: vec![
                ([-90.0, 0.0], "Idle".to_string(), false),
                ([0.0, 0.0], "Idle".to_string(), false),
                ([90.0, 0.0], "Idle".to_string(), false),
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
                ParamSpec::new("aim_pitch", "瞄准纵向", "瞄准方向的垂直偏移", -1.0, 1.0, 0.0),
            ],
            speed: 1.0,
            default_samples: Vec::new(),
        },
    ]
}
