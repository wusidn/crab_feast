//! 动画预览页面：角色、动画剪辑、播放倍速与根运动。
//!
//! 页面里的通用积木（文字、小标题、按钮、下拉框、滑动条）来自
//! `crate::ui::menu::widgets`，这里只负责挂业务标记、观察者与状态刷新。

use bevy::{
    picking::pointer::PointerButton,
    prelude::*,
    ui_widgets::{SliderRange, SliderValue, ValueChange, slider_self_update},
};
use crate::ui::menu::widgets;

use super::{
    DropdownKind, PreviewAction, PreviewButton, PreviewCatalog, PreviewDropdownItem,
    PreviewDropdownLabel, PreviewDropdownPopup, PreviewDropdownTrigger, PreviewRootMotionLabel,
    PreviewSpeedFill, PreviewSpeedLabel, PreviewSpeedSlider, PreviewSpeedThumb, PreviewState,
    SPEED_RANGE,
};

/// 在菜单抽屉的「动画预览」页面里搭建内容区
pub(crate) fn build_page(
    commands: &mut Commands,
    panel: Entity,
    asset_server: &AssetServer,
    catalog: &PreviewCatalog,
    state: &PreviewState,
) {
    let font: Handle<Font> = asset_server.load("fonts/PingFang-SC-Regular.ttf");

    widgets::text(
        commands,
        panel,
        Some(&font),
        "F2 / ` 呼出或收起，Esc 返回上一级".to_string(),
        12.0,
        widgets::LABEL_COLOR,
    );

    let character_names: Vec<String> = catalog
        .characters
        .iter()
        .map(|character| character.name.clone())
        .collect();
    let clip_names: Vec<String> = catalog.clips.clone();

    widgets::heading(commands, panel, Some(&font), "角色");
    dropdown(
        commands,
        panel,
        &font,
        DropdownKind::Character,
        &character_names,
        state.character,
    );

    widgets::heading(commands, panel, Some(&font), "动画");
    dropdown(
        commands,
        panel,
        &font,
        DropdownKind::Animation,
        &clip_names,
        state.animation,
    );

    widgets::heading(commands, panel, Some(&font), "播放倍速");
    speed_slider(commands, panel, state);
    let speed_label = widgets::text(
        commands,
        panel,
        Some(&font),
        format_speed(state.speed),
        13.0,
        widgets::LABEL_COLOR,
    );
    commands.entity(speed_label).insert(PreviewSpeedLabel);

    widgets::heading(commands, panel, Some(&font), "根运动");
    let action_row = commands
        .spawn_scene(bsn! {
            Node {
                width: Val::Percent(100.0),
                flex_wrap: FlexWrap::Wrap,
            }
            ChildOf(panel)
        })
        .id();
    button(
        commands,
        action_row,
        &font,
        "开启 / 关闭".to_string(),
        PreviewAction::ToggleRootMotion,
    );
    let motion_label = widgets::text(
        commands,
        panel,
        Some(&font),
        format_root_motion(state.root_motion),
        13.0,
        widgets::LABEL_COLOR,
    );
    commands
        .entity(motion_label)
        .insert(PreviewRootMotionLabel);

    let hint = widgets::text(
        commands,
        panel,
        Some(&font),
        "根运动关闭时角色原地播放；开启后骨骼位移会带动模型移动，剪辑循环回到起点时角色回到原位。"
            .to_string(),
        11.0,
        widgets::LABEL_COLOR,
    );
    commands.entity(hint).insert(Node {
        margin: UiRect::top(Val::Px(8.0)),
        ..default()
    });
}

/// 下拉框：通用外观由 widgets 生成，这里只挂业务标记与点击处理
fn dropdown(
    commands: &mut Commands,
    panel: Entity,
    font: &Handle<Font>,
    kind: DropdownKind,
    labels: &[String],
    selected: usize,
) {
    let parts = widgets::dropdown(commands, panel, Some(font), labels, selected);

    commands
        .entity(parts.trigger)
        .insert(PreviewDropdownTrigger)
        .observe(
            move |mut click: On<PointerClick>, mut state: ResMut<PreviewState>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                // 阻断冒泡，否则面板根节点的"点空白收起"会立刻把它关掉
                click.propagate(false);
                state.dropdown_open = if state.dropdown_open == Some(kind) {
                    None
                } else {
                    Some(kind)
                };
            },
        );

    commands
        .entity(parts.popup)
        .insert(PreviewDropdownPopup(kind));
    commands
        .entity(parts.label)
        .insert(PreviewDropdownLabel(kind));

    for (index, item) in parts.items.into_iter().enumerate() {
        commands
            .entity(item)
            .insert(PreviewDropdownItem { kind, index })
            .observe(
                move |mut click: On<PointerClick>, mut state: ResMut<PreviewState>| {
                    if click.button != PointerButton::Primary {
                        return;
                    }
                    click.propagate(false);
                    match kind {
                        DropdownKind::Character => state.character = index,
                        DropdownKind::Animation => state.animation = index,
                    }
                    state.dropdown_open = None;
                },
            );
    }
}

/// 倍速滑动条：外观由 widgets 生成，滑块位置仍由预览自己跟随播放进度
fn speed_slider(commands: &mut Commands, panel: Entity, state: &PreviewState) {
    let (min, max) = SPEED_RANGE;
    let parts = widgets::slider(commands, panel, state.speed, min, max, 0.05);

    commands
        .entity(parts.root)
        .insert(PreviewSpeedSlider)
        .observe(
            |change: On<ValueChange<f32>>, mut state: ResMut<PreviewState>| {
                // 滑块是连续值，按 0.05 对齐，避免显示成 1.03x 这种零碎数字
                state.speed = (change.value * 20.0).round() / 20.0;
            },
        )
        .observe(slider_self_update);

    commands.entity(parts.thumb).insert(PreviewSpeedThumb);
    commands.entity(parts.fill).insert(PreviewSpeedFill);
}

pub(super) fn update_button_styles(
    state: Res<PreviewState>,
    mut buttons: Query<(&PreviewButton, &mut BackgroundColor), Without<PreviewDropdownItem>>,
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

/// 下拉框的浮层显隐、触发器里显示的当前值、选中项高亮
pub(super) fn update_dropdown(
    state: Res<PreviewState>,
    catalog: Res<PreviewCatalog>,
    mut popups: Query<(&PreviewDropdownPopup, &mut Node), Without<PreviewDropdownItem>>,
    mut labels: Query<(&PreviewDropdownLabel, &mut Text), Without<PreviewDropdownItem>>,
    mut items: Query<(&PreviewDropdownItem, &mut BackgroundColor), Without<PreviewDropdownPopup>>,
) {
    if !state.is_changed() {
        return;
    }

    for (popup, mut node) in &mut popups {
        let desired = if state.dropdown_open == Some(popup.0) {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != desired {
            node.display = desired;
        }
    }

    for (label, mut text) in &mut labels {
        let value = current_label(&state, &catalog, label.0);
        if text.0 != value {
            text.0 = value;
        }
    }

    for (item, mut background) in &mut items {
        let selected = selected_index(&state, item.kind) == item.index;
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

/// 让滑块和已播放进度跟着当前倍速走
pub(super) fn update_speed_thumb(
    sliders: Query<(&SliderValue, &SliderRange), With<PreviewSpeedSlider>>,
    mut thumbs: Query<&mut Node, With<PreviewSpeedThumb>>,
    mut fills: Query<&mut Node, (With<PreviewSpeedFill>, Without<PreviewSpeedThumb>)>,
) {
    let Ok((value, range)) = sliders.single() else {
        return;
    };
    let value = value.0.clamp(range.start(), range.end());
    let position = range.thumb_position(value);

    for mut node in &mut thumbs {
        node.left = Val::Percent(position * 100.0);
    }
    for mut node in &mut fills {
        node.right = Val::Percent((1.0 - position) * 100.0);
    }
}

pub(super) fn update_labels(
    state: Res<PreviewState>,
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

fn selected_index(state: &PreviewState, kind: DropdownKind) -> usize {
    match kind {
        DropdownKind::Character => state.character,
        DropdownKind::Animation => state.animation,
    }
}

fn current_label(state: &PreviewState, catalog: &PreviewCatalog, kind: DropdownKind) -> String {
    match kind {
        DropdownKind::Character => catalog
            .character(state.character)
            .map(|character| character.name.clone())
            .unwrap_or_else(|| "无角色".to_string()),
        DropdownKind::Animation => catalog
            .clip(state.animation)
            .map(|clip| clip.to_string())
            .unwrap_or_else(|| "无动画".to_string()),
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

/// 业务按钮：外观来自 widgets，点击行为在这里挂
fn button(
    commands: &mut Commands,
    row: Entity,
    font: &Handle<Font>,
    label: String,
    action: PreviewAction,
) {
    let button = widgets::button(commands, row, Some(font), label);
    commands
        .entity(button)
        .insert(PreviewButton(action))
        .observe(
            move |click: On<PointerClick>, mut state: ResMut<PreviewState>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                match action {
                    PreviewAction::ToggleRootMotion => {
                        state.root_motion = !state.root_motion;
                    }
                }
            },
        );
}
