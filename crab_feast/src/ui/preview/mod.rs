//! 「动画预览」菜单页：只负责控件与交互，状态全在 `crate::anim` 里。
//!
//! 角色、剪辑、倍速、根运动写进 [`AnimSelection`]；模型加载、播放与根运动位移由
//! `anim::character` 完成，混合空间求值与骨骼写入由 `anim::driver` 完成。

use bevy::prelude::*;

use crate::ui::menu::{
    MenuConfig, MenuEvent, MenuPageBody, MenuPageDef, MenuSet, MenuState, widgets,
};
use crate::{
    GameState,
    anim::{AnimCatalog, AnimFocus, AnimSelection},
};

pub(crate) mod ui;

/// 菜单页面 id
pub const ANIMATION_PREVIEW_PAGE: &str = "animation_preview";

/// 面板上两个下拉框的标识（展开状态由 `widgets::OpenDropdown` 统一持有）
pub(crate) const CHARACTER_DROPDOWN: &str = "preview/character";
pub(crate) const CLIP_DROPDOWN: &str = "preview/clip";

/// 面板按钮携带的动作
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) enum PreviewAction {
    #[default]
    ToggleRootMotion,
}

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewButton(pub(crate) PreviewAction);

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewSpeedSlider;

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewSpeedLabel;

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewRootMotionLabel;

pub struct AnimationPreviewPlugin;

impl Plugin for AnimationPreviewPlugin {
    fn build(&self, app: &mut App) {
        // 先把页面挂进菜单树，再在菜单 UI 生成后往页面内容区填控件
        app.add_systems(
            OnEnter(GameState::Game),
            register_menu_page.before(MenuSet::Setup),
        )
        .add_systems(
            OnEnter(GameState::Game),
            fill_menu_page.after(MenuSet::Setup),
        )
        // 菜单开合或换页时收起展开的下拉框
        .add_systems(
            Update,
            reset_dropdown_on_menu_event.run_if(in_state(GameState::Game)),
        )
        // 停在预览页时把骨骼让给单条剪辑，离开页面再交还混合空间
        .add_systems(
            Update,
            own_skeleton_for_preview.run_if(in_state(GameState::Game)),
        )
        .add_systems(
            Update,
            (
                ui::update_button_styles,
                ui::update_dropdown,
                widgets::update_slider_styling,
                ui::update_labels,
                ui::apply_dropdown_pick,
            )
                .run_if(in_state(GameState::Game)),
        );
    }
}

/// 把「动画预览」注册成菜单里的一级入口
fn register_menu_page(mut config: ResMut<MenuConfig>) {
    if config
        .pages
        .iter()
        .any(|page| page.id == ANIMATION_PREVIEW_PAGE)
    {
        return;
    }
    config.pages.push(MenuPageDef::section(
        ANIMATION_PREVIEW_PAGE,
        "动画预览",
        "切换角色、动画剪辑、播放倍速与根运动",
    ));
}

/// 把预览控件挂进菜单的「动画预览」页面内容区
fn fill_menu_page(
    mut commands: Commands,
    catalog: Res<AnimCatalog>,
    selection: Res<AnimSelection>,
    bodies: Query<(Entity, &MenuPageBody)>,
) {
    let Some((entity, _)) = bodies
        .iter()
        .find(|(_, body)| body.page == Some(ANIMATION_PREVIEW_PAGE))
    else {
        warn!("菜单里没有找到动画预览页面，跳过内容填充");
        return;
    };

    ui::build_page(&mut commands, entity, &catalog, &selection);
}

/// 停在「动画预览」页时，姿态归 AnimationPlayer 的单条剪辑。
///
/// 不开这个口子，默认启用的移动混合会每帧覆写整具骨架，选哪条剪辑都看不出来；
/// 混合空间本身仍在求值（见 `anim::driver::drive_pose`），所以权重与地速不会断档。
///
/// 预览页就是「真的把游戏里的角色换成试看的那个」：下拉框写的是全局
/// [`AnimSelection`]，模型、剪辑表、骨架、FootIk 尺寸全部跟着这一份选择走。
/// 离页不还原 —— 还原会让模型和动画分成两个来源（模型是自己的、剪辑还是试看那个的），
/// 那种错位比「留在试看角色上」难看得多。
fn own_skeleton_for_preview(
    menu: Res<MenuState>,
    catalog: Res<AnimCatalog>,
    mut focus: ResMut<AnimFocus>,
    selection: Res<AnimSelection>,
    mut on_page: Local<bool>,
) {
    let now_on_page = menu.open && menu.current() == Some(ANIMATION_PREVIEW_PAGE);
    focus.set_preview(now_on_page);
    if now_on_page == *on_page {
        return;
    }
    *on_page = now_on_page;
    if now_on_page {
        return;
    }
    // 离页只记一笔「现在游戏里是谁」，方便看日志对账
    let name = catalog
        .character(selection.character)
        .map(|def| def.name.as_str())
        .unwrap_or("?");
    info!(
        "离开动画预览页：角色留在 {name}（#{}）、剪辑 #{}（模型与动画同一来源）",
        selection.character, selection.clip
    );
}

fn reset_dropdown_on_menu_event(
    mut events: MessageReader<MenuEvent>,
    mut open: ResMut<widgets::OpenDropdown>,
) {
    if events.read().count() > 0 {
        open.0 = None;
    }
}
