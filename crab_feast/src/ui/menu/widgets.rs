//! 页面通用 UI 积木：文字、小标题、按钮、下拉框、滑动条。
//!
//! 这里只负责生成视觉节点，不关心页面业务：下拉框展开哪一项、
//! 按钮点击做什么、滑块数值怎么用，都由调用方挂标记和观察者决定。
//! 所有函数都返回生成实体的 [`Entity`]，方便调用方继续 `.insert(...)` / `.observe(...)`。

use bevy::{
    prelude::*,
    ui_widgets::{
        Slider, SliderOrientation, SliderRange, SliderStep, SliderThumb, SliderValue, TrackClick,
    },
};

pub const BUTTON_IDLE: Color = Color::srgba(0.16, 0.18, 0.24, 0.9);
pub const BUTTON_SELECTED: Color = Color::srgba(0.22, 0.44, 0.85, 0.95);
pub const POPUP_BACKGROUND: Color = Color::srgb(0.07, 0.09, 0.14);
pub const SLIDER_FILL: Color = Color::srgba(0.28, 0.52, 0.92, 0.9);
pub const BORDER_SOFT: Color = Color::srgba(1.0, 1.0, 1.0, 0.12);
pub const LABEL_COLOR: Color = Color::srgb(0.62, 0.68, 0.78);
pub const TEXT_COLOR: Color = Color::srgb(0.88, 0.91, 0.96);

/// 滑动条滑块直径
pub const SLIDER_THUMB_SIZE: f32 = 14.0;

/// 下拉框生成后的关键节点
pub struct DropdownParts {
    pub trigger: Entity,
    /// 浮层选项列表
    pub popup: Entity,
    /// 触发器里显示当前值的文字
    pub label: Entity,
    /// 每个选项对应一个实体，顺序与传入的 labels 一致
    pub items: Vec<Entity>,
}

/// 滑动条生成后的关键节点
pub struct SliderParts {
    pub root: Entity,
    pub thumb: Entity,
    /// 已播放进度条
    pub fill: Entity,
}

/// 滑动条已填充进度部分（供 `update_slider_styling` 定位）
#[derive(Component, Clone, Debug, Default)]
pub struct SliderFill;

/// 通用滑动条滑块定位：核心 Slider 不会自己摆滑块/进度条，
/// 这里按 `SliderValue` 在 `SliderRange` 里的比例实时摆放，支持负区间（如 -1..1）。
pub fn update_slider_styling(
    sliders: Query<(Entity, &SliderValue, &SliderRange)>,
    mut thumbs: Query<&mut Node, (With<SliderThumb>, Without<SliderFill>)>,
    mut fills: Query<&mut Node, (With<SliderFill>, Without<SliderThumb>)>,
    children: Query<&Children>,
) {
    for (root, value, range) in &sliders {
        let position = range.thumb_position(value.0.clamp(range.start(), range.end()));
        for child in children.iter_descendants(root) {
            if let Ok(mut node) = thumbs.get_mut(child) {
                node.left = Val::Percent(position * 100.0);
            }
            if let Ok(mut node) = fills.get_mut(child) {
                node.right = Val::Percent((1.0 - position) * 100.0);
            }
        }
    }
}

/// 一段普通文字
pub fn text(
    commands: &mut Commands,
    parent: Entity,
    font: Option<&Handle<Font>>,
    content: String,
    size: f32,
    color: Color,
) -> Entity {
    let text_font = text_font(font, size);
    commands
        .spawn_scene(bsn! {
            Text::new(content)
            TextColor(color)
            ChildOf(parent)
        })
        .insert(text_font)
        .id()
}

/// 段落小标题（比正文稍大、带一点上间距）
pub fn heading(
    commands: &mut Commands,
    parent: Entity,
    font: Option<&Handle<Font>>,
    title: &str,
) -> Entity {
    let label = title.to_string();
    let text_font = text_font(font, 14.0);
    let margin = UiRect::top(Val::Px(6.0));
    commands
        .spawn_scene(bsn! {
            Text::new(label)
            TextColor(LABEL_COLOR)
            Node { margin: margin }
            ChildOf(parent)
        })
        .insert(text_font)
        .id()
}

/// 图标按钮（正方形、内容居中，例如返回箭头）
pub fn icon_button(
    commands: &mut Commands,
    parent: Entity,
    font: Option<&Handle<Font>>,
    glyph: String,
    size: f32,
) -> Entity {
    let button = commands
        .spawn_scene(bsn! {
            Node {
                width: Val::Px(30.0),
                height: Val::Px(30.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border_radius: BorderRadius::all(Val::Px(6.0)),
            }
            BackgroundColor(BUTTON_IDLE)
            ChildOf(parent)
        })
        .id();

    text(commands, button, font, glyph, size, TEXT_COLOR);
    button
}

/// 普通按钮（只有外观，点击行为由调用方挂 observer）
pub fn button(
    commands: &mut Commands,
    parent: Entity,
    font: Option<&Handle<Font>>,
    label: String,
) -> Entity {
    let button = commands
        .spawn_scene(bsn! {
            Node {
                padding: UiRect::axes(Val::Px(9.0), Val::Px(5.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(5.0)),
            }
            BackgroundColor(BUTTON_IDLE)
            BorderColor::all(BORDER_SOFT)
            ChildOf(parent)
        })
        .id();

    text(commands, button, font, label, 13.0, TEXT_COLOR);
    button
}

/// 下拉框：触发器 + 浮层选项列表。
///
/// `selected` 只用于初始显示与初始高亮，后续由调用方自己维护。
pub fn dropdown(
    commands: &mut Commands,
    parent: Entity,
    font: Option<&Handle<Font>>,
    labels: &[String],
    selected: usize,
) -> DropdownParts {
    let root = commands
        .spawn_scene(bsn! {
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
            }
            ChildOf(parent)
        })
        .id();

    let current = labels.get(selected).cloned().unwrap_or_default();

    let trigger = commands
        .spawn_scene(bsn! {
            Node {
                width: Val::Percent(100.0),
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                padding: UiRect::axes(Val::Px(10.0), Val::Px(7.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
            }
            BackgroundColor(BUTTON_IDLE)
            BorderColor::all(BORDER_SOFT)
            ChildOf(root)
        })
        .id();

    let label = text(commands, trigger, font, current, 13.0, TEXT_COLOR);
    text(commands, trigger, font, "▼".to_string(), 8.0, LABEL_COLOR);

    let popup = commands
        .spawn_scene(bsn! {
            Node {
                position_type: PositionType::Absolute,
                top: Val::Percent(100.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                margin: UiRect::top(Val::Px(4.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                padding: UiRect::all(Val::Px(4.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(8.0)),
                max_height: Val::Px(340.0),
                overflow: Overflow::clip_y(),
                display: Display::None,
            }
            BackgroundColor(POPUP_BACKGROUND)
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.16))
            GlobalZIndex(150)
            ChildOf(root)
        })
        .id();

    let mut items = Vec::with_capacity(labels.len());
    for (index, label) in labels.iter().enumerate() {
        let item_color = if index == selected {
            BUTTON_SELECTED
        } else {
            Color::NONE
        };

        let item = commands
            .spawn_scene(bsn! {
                Node {
                    width: Val::Percent(100.0),
                    padding: UiRect::axes(Val::Px(8.0), Val::Px(5.0)),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                }
                BackgroundColor(item_color)
                ChildOf(popup)
            })
            .id();

        text(commands, item, font, label.clone(), 13.0, TEXT_COLOR);
        items.push(item);
    }

    DropdownParts {
        trigger,
        popup,
        label,
        items,
    }
}

/// 水平滑动条。`step` 是数值步长，`value` 是初始值。
pub fn slider(
    commands: &mut Commands,
    parent: Entity,
    value: f32,
    min: f32,
    max: f32,
    step: f32,
) -> SliderParts {
    let travel_left = Val::Px(SLIDER_THUMB_SIZE / 2.0);
    let travel_right = Val::Px(SLIDER_THUMB_SIZE / 2.0);
    let thumb_margin = UiRect::left(Val::Px(-SLIDER_THUMB_SIZE / 2.0));

    let root = commands
        .spawn_scene(bsn! {
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(24.0),
            }
            Slider {
                track_click: TrackClick::Snap,
                orientation: SliderOrientation::Horizontal,
            }
            SliderValue(value)
            SliderRange::new(min, max)
            SliderStep(step)
            ChildOf(parent)
        })
        .id();

    // 滑块可移动的区域（左右各留出半个滑块，避免滑块越出轨道）
    let travel = commands
        .spawn_scene(bsn! {
            Node {
                position_type: PositionType::Absolute,
                left: travel_left,
                right: travel_right,
                top: Val::Px(0.0),
                bottom: Val::Px(0.0),
            }
            ChildOf(root)
        })
        .id();

    commands.spawn_scene(bsn! {
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            top: Val::Px(9.0),
            height: Val::Px(6.0),
            border_radius: BorderRadius::all(Val::Px(3.0)),
        }
        BackgroundColor(Color::srgba(0.2, 0.23, 0.3, 0.95))
        ChildOf(travel)
    });

    let fill = commands
        .spawn_scene(bsn! {
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Percent(100.0),
                top: Val::Px(9.0),
                height: Val::Px(6.0),
                border_radius: BorderRadius::all(Val::Px(3.0)),
            }
            BackgroundColor(SLIDER_FILL)
            SliderFill
            ChildOf(travel)
        })
        .id();

    let thumb = commands
        .spawn_scene(bsn! {
            Node {
                position_type: PositionType::Absolute,
                width: Val::Px(SLIDER_THUMB_SIZE),
                height: Val::Px(SLIDER_THUMB_SIZE),
                top: Val::Px(4.0),
                left: Val::Percent(0.0),
                margin: thumb_margin,
                border_radius: BorderRadius::MAX,
            }
            BackgroundColor(BUTTON_SELECTED)
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.35))
            SliderThumb
            ChildOf(travel)
        })
        .id();

    SliderParts { root, thumb, fill }
}

fn text_font(font: Option<&Handle<Font>>, size: f32) -> TextFont {
    TextFont {
        font: font
            .map(|font| FontSource::from(font.clone()))
            .unwrap_or_default(),
        font_size: FontSize::Px(size),
        ..default()
    }
}
