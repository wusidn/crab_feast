//! 「动画预览」菜单页：只负责控件与交互，状态全在 `crate::anim` 里。
//!
//! 角色、剪辑、倍速、根运动写进 [`AnimSelection`]；模型加载、播放与根运动位移由
//! `anim::character` 完成，混合空间求值与骨骼写入由 `anim::driver` 完成。

use bevy::prelude::*;

use crate::ui::menu::{MenuConfig, MenuEvent, MenuPageBody, MenuPageDef, MenuSet, MenuState};
use crate::{
    GameState,
    anim::{AnimCatalog, AnimFocus, AnimSelection},
};

pub(crate) mod ui;

/// 菜单页面 id
pub const ANIMATION_PREVIEW_PAGE: &str = "animation_preview";

/// 面板自己的状态：只有「哪个下拉框展开」，选择结果一律写回动画系统
#[derive(Resource, Debug, Default)]
pub struct PreviewUi {
    pub dropdown_open: Option<DropdownKind>,
}

/// 面板上两个下拉框
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum DropdownKind {
    #[default]
    Character,
    Animation,
}

/// 面板按钮携带的动作
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) enum PreviewAction {
    #[default]
    ToggleRootMotion,
}

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewButton(pub(crate) PreviewAction);

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewDropdownTrigger;

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewDropdownPopup(pub(crate) DropdownKind);

/// 下拉框里的选项
#[derive(Component, Clone, Default)]
pub(crate) struct PreviewDropdownItem {
    pub(crate) kind: DropdownKind,
    pub(crate) index: usize,
}

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewDropdownLabel(pub(crate) DropdownKind);

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewSpeedSlider;

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewSpeedThumb;

/// 滑动条已播放到的部分
#[derive(Component, Clone, Default)]
pub(crate) struct PreviewSpeedFill;

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewSpeedLabel;

#[derive(Component, Clone, Default)]
pub(crate) struct PreviewRootMotionLabel;

pub struct AnimationPreviewPlugin;

impl Plugin for AnimationPreviewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PreviewUi>()
            // 先把页面挂进菜单树，再在菜单 UI 生成后往页面内容区填控件
            .add_systems(
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
                    ui::update_speed_thumb,
                    ui::update_labels,
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
    asset_server: Res<AssetServer>,
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

    ui::build_page(&mut commands, entity, &asset_server, &catalog, &selection);
}

/// 停在「动画预览」页时，姿态归 AnimationPlayer 的单条剪辑。
///
/// 不开这个口子，默认启用的移动混合会每帧覆写整具骨架，选哪条剪辑都看不出来；
/// 混合空间本身仍在求值（见 `anim::driver::drive_pose`），所以权重与地速不会断档。
fn own_skeleton_for_preview(menu: Res<MenuState>, mut focus: ResMut<AnimFocus>) {
    let on_page = menu.open && menu.current() == Some(ANIMATION_PREVIEW_PAGE);
    focus.set_preview(on_page);
}

fn reset_dropdown_on_menu_event(
    mut events: MessageReader<MenuEvent>,
    mut preview: ResMut<PreviewUi>,
) {
    if events.read().count() > 0 {
        preview.dropdown_open = None;
    }
}
