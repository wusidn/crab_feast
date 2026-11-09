//! 可注册的动画混合系统：每个混合器是一个 Resource，带自己的混合空间文档与参数表。

use std::collections::HashMap;

use bevy::prelude::*;

use super::BlendSpaceDoc;

/// 混合器的公共数据。每个具体混合器只需包一层 newtype。
#[derive(Clone, Debug)]
pub struct MixerCore {
    /// 存档文件名与预览里的稳定标识
    pub key: String,
    /// 预览里显示的名字
    pub title: String,
    /// 一句话说明
    pub description: String,
    /// 该混合器的混合空间
    pub space: BlendSpaceDoc,
    /// 参数值（轴名 → 数值），轴名来自 `space.params`
    pub params: HashMap<String, f32>,
    /// 是否在游戏中驱动角色
    pub enabled: bool,
    /// 驱动哪个角色
    pub character: String,
}

impl Default for MixerCore {
    fn default() -> Self {
        Self {
            key: String::new(),
            title: String::new(),
            description: String::new(),
            space: BlendSpaceDoc::default(),
            params: HashMap::new(),
            enabled: true,
            character: String::new(),
        }
    }
}

impl MixerCore {
    pub fn new(
        key: impl Into<String>,
        title: impl Into<String>,
        space: BlendSpaceDoc,
        character: impl Into<String>,
    ) -> Self {
        let mut params = HashMap::new();
        for axis in &space.params {
            params.insert(axis.clone(), 0.0);
        }
        Self {
            key: key.into(),
            title: title.into(),
            description: String::new(),
            space,
            params,
            enabled: true,
            character: character.into(),
        }
    }
}

/// 任何实现了这个 trait 的类型都能注册到预览。
pub trait BlendMixer: Resource {
    fn core(&self) -> &MixerCore;
    fn core_mut(&mut self) -> &mut MixerCore;
}

/// 为一个 `XxxMixer(pub MixerCore)` newtype 生成 `BlendMixer` 实现。
#[macro_export]
macro_rules! impl_blend_mixer {
    ($ty:ident) => {
        impl $crate::anim::core::BlendMixer for $ty {
            fn core(&self) -> &$crate::anim::core::MixerCore {
                &self.0
            }
            fn core_mut(&mut self) -> &mut $crate::anim::core::MixerCore {
                &mut self.0
            }
        }
    };
}
