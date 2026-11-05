//! 游戏内的右侧抽屉菜单：图标、抽屉面板、页面栈，以及页面通用 UI 积木。
//!
//! - [`drawer`]：抽屉本体、页面注册与页面切换
//! - [`widgets`]：文字、小标题、按钮、下拉框、滑动条等页面通用控件
//!
//! 具体功能页面（例如动画预览）由各功能模块在进入游戏状态时注册并填充内容。

use bevy::prelude::*;

use crate::GameState;

mod drawer;
pub(crate) mod widgets;

pub(crate) use drawer::*;

/// 游戏侧的菜单配置：只提供标题、字体、快捷键，
/// 页面由各功能模块在进入游戏状态时自行注册。
pub struct GameMenuPlugin;

impl Plugin for GameMenuPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(MenuConfig {
            root_title: "功能菜单",
            font_path: Some("fonts/PingFang-SC-Regular.ttf"),
            toggle_keys: vec![KeyCode::F2, KeyCode::Backquote],
            pages: Vec::new(),
        })
        .add_plugins(MenuPlugin::new(GameState::Game));
    }
}
