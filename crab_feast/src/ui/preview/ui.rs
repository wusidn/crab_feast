//! 动画预览页面：角色、动画剪辑、播放倍速与根运动。
//!
//! 整页结构写在 [`page_scene`] 的 `Children[...]` 里；文字、标题、下拉框、按钮、滑杆
//! 都来自 `crate::ui::menu::widgets`，这里只负责挂业务标记、观察者与状态刷新。

use crate::ui::menu::widgets;
use bevy::{
    picking::pointer::PointerButton,
    prelude::*,
    ui_widgets::{ValueChange, slider_self_update},
};

use crate::anim::{AnimCatalog, AnimSelection, SPEED_RANGE};

use super::{
    CHARACTER_DROPDOWN, CLIP_DROPDOWN, PreviewAction, PreviewButton, PreviewRootMotionLabel,
    PreviewSpeedLabel, PreviewSpeedSlider,
};

/// 在菜单抽屉的「动画预览」页面里搭建内容区
///
/// 字体族由菜单内容区（字体作用域）继承，这里只写各文本的字号。
pub(crate) fn build_page(
    commands: &mut Commands,
    panel: Entity,
    catalog: &AnimCatalog,
    state: &AnimSelection,
) {
    commands.spawn_scene(page_scene(panel, catalog, state));
}

/// 整页结构：提示 → 角色 → 动画 → 倍速 → 根运动
fn page_scene(panel: Entity, catalog: &AnimCatalog, state: &AnimSelection) -> impl Scene + use<> {
    let character_names: Vec<String> = catalog
        .characters
        .iter()
        .map(|character| character.name.clone())
        .collect();
    let clip_names: Vec<String> = catalog.clips.clone();
    let speed = format_speed(state.speed);
    let root_motion = format_root_motion(state.root_motion);

    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
        }
        ChildOf(panel)
        Children [
            @{widgets::text(
                "F2 / ` 呼出或收起，Esc 返回上一级",
                12.0,
                widgets::LABEL_COLOR,
            )}
            --
            @{widgets::heading("角色")}
            --
            @{widgets::dropdown(CHARACTER_DROPDOWN, &character_names, state.character)}
            --
            @{widgets::heading("动画")}
            --
            @{widgets::dropdown(CLIP_DROPDOWN, &clip_names, state.clip)}
            --
            @{widgets::heading("播放倍速")}
            --
            // 滑块位置由 widgets::update_slider_styling 跟随 SliderValue
            @{widgets::slider(state.speed, SPEED_RANGE.0, SPEED_RANGE.1, 0.05)}
            PreviewSpeedSlider
            on(slider_self_update)
            on(|change: On<ValueChange<f32>>, mut state: ResMut<AnimSelection>| {
                // 滑块是连续值，按 0.05 对齐，避免显示成 1.03x 这种零碎数字
                state.speed = (change.value * 20.0).round() / 20.0;
            })
            --
            Text::new(speed)
            TextFont { font_size: FontSize::Px(13.0) }
            TextColor(widgets::LABEL_COLOR)
            PreviewSpeedLabel
            --
            @{widgets::heading("根运动")}
            --
            Node {
                width: Val::Percent(100.0),
                flex_wrap: FlexWrap::Wrap,
            }
            Children [
                @{widgets::button("开启 / 关闭")}
                PreviewButton(PreviewAction::ToggleRootMotion)
                on(|click: On<PointerClick>, mut state: ResMut<AnimSelection>| {
                    if click.button != PointerButton::Primary {
                        return;
                    }
                    match PreviewAction::ToggleRootMotion {
                        PreviewAction::ToggleRootMotion => {
                            state.root_motion = !state.root_motion;
                        }
                    }
                })
            ]
            --
            Text::new(root_motion)
            TextFont { font_size: FontSize::Px(13.0) }
            TextColor(widgets::LABEL_COLOR)
            PreviewRootMotionLabel
            --
            Text::new(
                "根运动关闭时角色原地播放；开启后骨骼位移会带动模型移动，\
                 剪辑循环回到起点时角色回到原位。",
            )
            TextFont { font_size: FontSize::Px(11.0) }
            TextColor(widgets::LABEL_COLOR)
            Node { margin: UiRect::top(Val::Px(8.0)) }
        ]
    }
}

pub(super) fn update_button_styles(
    state: Res<AnimSelection>,
    mut buttons: Query<(&PreviewButton, &mut BackgroundColor), Without<widgets::DropdownItem>>,
) {
    if !state.is_changed() {
        return;
    }
    for (button, mut background) in &mut buttons {
        let selected = match button.0 {
            PreviewAction::ToggleRootMotion => state.root_motion,
        };
        let color = if selected {
            widgets::BUTTON_SELECTED
        } else {
            widgets::BUTTON_IDLE
        };
        if background.0 != color {
            background.0 = color;
        }
    }
}

/// 选中下拉框的某一项：把下标写回动画系统
pub(super) fn apply_dropdown_pick(
    mut picked: MessageReader<widgets::DropdownPicked>,
    mut state: ResMut<AnimSelection>,
) {
    for pick in picked.read() {
        match pick.id {
            CHARACTER_DROPDOWN => state.character = pick.index,
            CLIP_DROPDOWN => state.clip = pick.index,
            _ => {}
        }
    }
}

/// 触发器里显示的当前值、选中项高亮（浮层显隐由 widgets 自己按 `OpenDropdown` 处理）
pub(super) fn update_dropdown(
    state: Res<AnimSelection>,
    catalog: Res<AnimCatalog>,
    mut labels: Query<(&widgets::DropdownLabel, &mut Text)>,
    mut items: Query<(&widgets::DropdownItem, &mut BackgroundColor)>,
) {
    if !state.is_changed() {
        return;
    }

    for (label, mut text) in &mut labels {
        let value = current_label(&state, &catalog, label.0);
        if text.0 != value {
            text.0 = value;
        }
    }

    for (item, mut background) in &mut items {
        let selected = selected_index(&state, item.id) == item.index;
        let color = if selected {
            widgets::BUTTON_SELECTED
        } else {
            Color::NONE
        };
        if background.0 != color {
            background.0 = color;
        }
    }
}

pub(super) fn update_labels(
    state: Res<AnimSelection>,
    mut speed_labels: Query<&mut Text, (With<PreviewSpeedLabel>, Without<PreviewRootMotionLabel>)>,
    mut motion_labels: Query<&mut Text, (With<PreviewRootMotionLabel>, Without<PreviewSpeedLabel>)>,
) {
    if !state.is_changed() {
        return;
    }
    for mut text in &mut speed_labels {
        let value = format_speed(state.speed);
        if text.0 != value {
            text.0 = value;
        }
    }
    for mut text in &mut motion_labels {
        let value = format_root_motion(state.root_motion);
        if text.0 != value {
            text.0 = value;
        }
    }
}

fn selected_index(state: &AnimSelection, id: &'static str) -> usize {
    match id {
        CHARACTER_DROPDOWN => state.character,
        CLIP_DROPDOWN => state.clip,
        _ => 0,
    }
}

fn current_label(state: &AnimSelection, catalog: &AnimCatalog, id: &'static str) -> String {
    match id {
        CHARACTER_DROPDOWN => catalog
            .character(state.character)
            .map(|character| character.name.clone())
            .unwrap_or_else(|| "无角色".to_string()),
        CLIP_DROPDOWN => catalog
            .clip(state.clip)
            .map(|clip| clip.to_string())
            .unwrap_or_else(|| "无动画".to_string()),
        _ => String::new(),
    }
}

fn format_speed(speed: f32) -> String {
    format!("当前倍速：{speed:.2}x")
}

fn format_root_motion(enabled: bool) -> String {
    if enabled {
        "根运动：开启".to_string()
    } else {
        "根运动：关闭".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::scene::ScenePlugin;
    use bevy::ui_widgets::{Slider, SliderThumb};

    use crate::anim::character::CharacterDef;

    fn test_catalog() -> AnimCatalog {
        AnimCatalog {
            characters: vec![CharacterDef {
                name: "Crab".to_string(),
                rig: "characters/crab.glb".to_string(),
                animations: "characters/animations/crab".to_string(),
                clips: vec!["idle".to_string(), "walk".to_string()],
                gaits: Default::default(),
            }],
            clips: vec!["idle".to_string(), "walk".to_string()],
        }
    }

    fn build_test_page() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            ScenePlugin::default(),
        ));
        app.insert_resource(test_catalog());
        app.insert_resource(AnimSelection::default());

        let panel = app.world_mut().spawn_empty().id();
        app.world_mut()
            .run_system_once(
                move |mut commands: Commands,
                      catalog: Res<AnimCatalog>,
                      selection: Res<AnimSelection>| {
                    build_page(&mut commands, panel, &catalog, &selection);
                },
            )
            .expect("页面构建系统跑不起来");
        app.update();
        (app, panel)
    }

    fn children_of(world: &World, entity: Entity) -> Vec<Entity> {
        world
            .get::<Children>(entity)
            .map(|children| children.iter().collect())
            .unwrap_or_default()
    }

    fn descendants_of(world: &World, root: Entity) -> Vec<Entity> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(entity) = stack.pop() {
            for child in children_of(world, entity) {
                out.push(child);
                stack.push(child);
            }
        }
        out
    }

    /// 页面必须是「根 → 各分区」的嵌套，下拉框的触发器和浮层都在下拉框根节点里
    #[test]
    fn page_is_nested_sections_with_dropdowns() {
        let (app, panel) = build_test_page();
        let world = app.world();

        let roots = children_of(world, panel);
        assert_eq!(roots.len(), 1, "页面根应该只有一个");
        let root = roots[0];
        assert_eq!(
            world.get::<Node>(root).map(|node| node.width),
            Some(Val::Percent(100.0))
        );

        let sections = children_of(world, root);
        assert_eq!(
            sections.len(),
            12,
            "提示 + 两个下拉框 + 三个标题 + 倍速滑杆/标签 + 根运动按钮/标签 + 说明"
        );

        // 两个下拉框：根节点 Children = 触发器 + 浮层
        let dropdowns: Vec<Entity> = descendants_of(world, root)
            .into_iter()
            .filter(|entity| {
                let children = children_of(world, *entity);
                children.len() == 2
                    && world.get::<widgets::DropdownTrigger>(children[0]).is_some()
                    && world.get::<widgets::DropdownPopup>(children[1]).is_some()
            })
            .collect();
        assert_eq!(dropdowns.len(), 2, "角色与动画两个下拉框");

        let mut total_items = 0;
        for dropdown in dropdowns {
            let children = children_of(world, dropdown);
            let trigger = children[0];
            let popup = children[1];
            assert_eq!(
                children_of(world, trigger).len(),
                2,
                "触发器 = 当前值 + 箭头"
            );
            let items = children_of(world, popup);
            assert!(!items.is_empty(), "浮层里至少有一个选项");
            for item in &items {
                assert!(
                    world.get::<widgets::DropdownItem>(*item).is_some(),
                    "选项带编号，选中结果靠消息交回页面"
                );
            }
            total_items += items.len();
        }
        assert_eq!(
            total_items,
            1 + 2,
            "角色 1 个选项 + 动画 2 个选项，直接铺在各自浮层里"
        );

        // 倍速滑杆：滑块必须在滑杆子树里
        let slider = descendants_of(world, root)
            .into_iter()
            .find(|entity| world.get::<PreviewSpeedSlider>(*entity).is_some())
            .expect("倍速滑杆");
        assert!(world.get::<Slider>(slider).is_some());
        assert!(
            descendants_of(world, slider)
                .iter()
                .any(|entity| world.get::<SliderThumb>(*entity).is_some()),
            "滑块必须是滑杆的后代"
        );
    }
}
