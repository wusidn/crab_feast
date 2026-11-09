//! 混合空间的声明类型：参数轴、取值范围、描述，以及默认采样点。
//!
//! 只有「空间长什么样」的类型定义，不含求值逻辑，也不含任何具体作品的剪辑名；
//! 某个作品有哪些空间由上层自己注册（本作在 `crab_feast::anim::spaces`）。
//! 采样点位置等可调配置由资源系统覆盖，缺失时回落到这里的默认值。

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
            ranges: Some([
                [self.params[0].min, self.params[0].max],
                [self.params[1].min, self.params[1].max],
            ]),
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
