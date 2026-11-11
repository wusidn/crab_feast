//! 页面通用 UI 积木：文字、小标题、按钮、下拉框、滑动条。
//!
//! 每个积木都是**整棵子树**的 `bsn!` 场景函数：父子关系统一写在 `Children[...]` 里，
//! 调用方用 `@{widgets::text(...)}` 组合进自己的页面，页面结构一眼就能读出来。
//!
//! 需要业务标记或交互时，在组合处直接往后写组件和 `on(...)` 观察者：
//!
//! ```ignore
//! bsn! {
//!     @{widgets::slider(value, min, max, step)}
//!     MySlider
//!     on(slider_self_update)
//! }
//! ```
//!
//! 下拉框的展开状态由 [`OpenDropdown`] 统一持有（同一时间只展开一个），
//! 选中结果以 [`DropdownPicked`] 消息交回页面，页面不再需要自己保存“哪个下拉框开着”。

use bevy::{
    input::mouse::MouseScrollUnit,
    picking::pointer::PointerButton,
    prelude::*,
    ui::{OverflowAxis, UiGlobalTransform},
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
/// 下拉框浮层最大高度
const POPUP_MAX_HEIGHT: f32 = 340.0;

/// 滑动条已填充进度部分（供 `update_slider_styling` 定位）
#[derive(Component, Clone, Debug, Default)]
pub struct SliderFill;

/// 下拉框标识：同一时间只展开一个
#[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct DropdownTrigger(pub &'static str);

/// 下拉框浮层
#[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct DropdownPopup(pub &'static str);

/// 触发器里显示当前值的文字
#[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct DropdownLabel(pub &'static str);

/// 浮层里的一个选项
#[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct DropdownItem {
    pub id: &'static str,
    pub index: usize,
}

/// 当前展开的下拉框（`None` 表示全部收起）
#[derive(Resource, Default, Debug)]
pub struct OpenDropdown(pub Option<&'static str>);

/// 用户选中了某个下拉框的某一项
#[derive(Message, Clone, Copy, Debug)]
pub struct DropdownPicked {
    pub id: &'static str,
    pub index: usize,
}

/// 积木自己的运行系统：下拉框展开状态、浮层显隐与摆位、滑动条跟随
pub struct WidgetPlugin;

impl Plugin for WidgetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OpenDropdown>()
            .add_message::<DropdownPicked>()
            .add_systems(Update, (sync_dropdown_visibility, sync_dropdown_geometry));
    }
}

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
                // 每帧无条件写会把 UI 布局标记为脏、每帧重排，只在真的变了才动
                let left = Val::Percent(position * 100.0);
                if node.left != left {
                    node.left = left;
                }
            }
            if let Ok(mut node) = fills.get_mut(child) {
                let right = Val::Percent((1.0 - position) * 100.0);
                if node.right != right {
                    node.right = right;
                }
            }
        }
    }
}

/// 按 [`OpenDropdown`] 显隐浮层；收起时顺手把滚动位置归零
fn sync_dropdown_visibility(
    open: Res<OpenDropdown>,
    mut popups: Query<(&DropdownPopup, &mut Node, &mut ScrollPosition)>,
) {
    for (popup, mut node, mut scroll) in &mut popups {
        let desired = if open.0 == Some(popup.0) {
            Display::Flex
        } else {
            Display::None
        };
        if node.display == desired {
            continue;
        }
        node.display = desired;
        if desired == Display::None {
            scroll.0 = Vec2::ZERO;
        }
    }
}

/// 浮层朝上还是朝下、最多能展开多高：按触发器上下剩余空间决定，避免被面板裁掉。
///
/// 触发器和浮层是下拉框根节点的前两个子节点，所以几何关系直接从层级里取，不用额外存锚点。
// 两组查询的形状写成别名 Bevy 会推不出 SystemParam，只能就地写全。
#[expect(clippy::type_complexity)]
fn sync_dropdown_geometry(
    open: Res<OpenDropdown>,
    popups: Query<(Entity, &DropdownPopup, &mut Node)>,
    parents: Query<&ChildOf>,
    children: Query<&Children>,
    mut ui: ParamSet<(
        Query<(&ComputedNode, &UiGlobalTransform)>,
        Query<(&Node, Option<&ChildOf>), Without<DropdownPopup>>,
    )>,
) {
    let Some(open_id) = open.0 else {
        return;
    };

    for (popup_entity, popup, mut node) in popups {
        if popup.0 != open_id {
            continue;
        }
        // 下拉框根节点：触发器（第一个子节点）+ 浮层（本实体）
        let Some(trigger) = parents
            .get(popup_entity)
            .ok()
            .map(|parent| parent.parent())
            .and_then(|root| children.get(root).ok())
            .and_then(|list| list.iter().next())
        else {
            continue;
        };

        // 触发器的上下边界（UI 逻辑像素，原点左上）
        let trigger_rect = {
            let nodes = ui.p0();
            nodes.get(trigger).ok().map(|(computed, transform)| {
                let height = computed.size().y * computed.inverse_scale_factor();
                let center = transform.translation.y;
                (center - height * 0.5, center + height * 0.5)
            })
        };
        let Some((trigger_top, trigger_bottom)) = trigger_rect else {
            continue;
        };

        // 沿父链找第一个会裁剪后代的祖先（面板 body 的滚动区 / clip 容器）
        let host = {
            let nodes = ui.p1();
            let mut current = trigger;
            let mut found = None;
            while let Ok((_style, child_of)) = nodes.get(current) {
                let Some(parent) = child_of.map(|child_of| child_of.parent()) else {
                    break;
                };
                current = parent;
                if let Ok((parent_style, _)) = nodes.get(parent)
                    && parent_style.overflow.y != OverflowAxis::Visible
                {
                    found = Some(parent);
                    break;
                }
            }
            found
        };
        let host_rect = {
            let nodes = ui.p0();
            host.and_then(|host| {
                nodes.get(host).ok().map(|(computed, transform)| {
                    let height = computed.size().y * computed.inverse_scale_factor();
                    let center = transform.translation.y;
                    (center - height * 0.5, center + height * 0.5)
                })
            })
        };
        let Some((host_top, host_bottom)) = host_rect else {
            continue;
        };

        let space_up = (trigger_top - host_top - 6.0).max(0.0);
        let space_down = (host_bottom - trigger_bottom - 6.0).max(0.0);
        let upward = space_up > space_down;
        let available = if upward { space_up } else { space_down };
        // 高度按 8px 取整，避免每帧微调布局
        let max_height = Val::Px(((available.min(300.0)) / 8.0).floor().max(2.0) * 8.0);

        let want_top = if upward {
            Val::Auto
        } else {
            Val::Percent(100.0)
        };
        let want_bottom = if upward {
            Val::Percent(100.0)
        } else {
            Val::Auto
        };
        if node.top != want_top || node.bottom != want_bottom {
            node.top = want_top;
            node.bottom = want_bottom;
            node.margin = if upward {
                UiRect::bottom(Val::Px(4.0))
            } else {
                UiRect::top(Val::Px(4.0))
            };
        }
        if node.max_height != max_height {
            node.max_height = max_height;
        }
    }
}

/// 一段普通文字
///
/// 字体族由所在页面根节点的 [`FontFamily`](crate::ui::font::FontFamily) 传播进来，
/// 这里只决定字号。
pub fn text(content: impl Into<String>, size: f32, color: Color) -> impl Scene {
    let content = content.into();
    bsn! {
        Text::new(content)
        TextFont { font_size: FontSize::Px(size) }
        TextColor(color)
    }
}

/// 段落小标题（比正文稍大、带一点上间距）
pub fn heading(title: impl Into<String>) -> impl Scene {
    let title = title.into();
    bsn! {
        Text::new(title)
        TextFont { font_size: FontSize::Px(14.0) }
        TextColor(LABEL_COLOR)
        Node { margin: UiRect::top(Val::Px(6.0)) }
    }
}

/// 图标按钮（正方形、内容居中，例如返回箭头）
pub fn icon_button(glyph: impl Into<String>, size: f32) -> impl Scene {
    let glyph = glyph.into();
    bsn! {
        Node {
            width: Val::Px(30.0),
            height: Val::Px(30.0),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border_radius: BorderRadius::all(Val::Px(6.0)),
        }
        BackgroundColor(BUTTON_IDLE)
        Children [
            @{text(glyph, size, TEXT_COLOR)}
        ]
    }
}

/// 普通按钮（只有外观，点击行为由调用方在组合处挂 `on(...)`）
pub fn button(label: impl Into<String>) -> impl Scene {
    let label = label.into();
    bsn! {
        Node {
            padding: UiRect::axes(Val::Px(9.0), Val::Px(5.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(5.0)),
        }
        BackgroundColor(BUTTON_IDLE)
        BorderColor::all(BORDER_SOFT)
        Children [
            @{text(label, 13.0, TEXT_COLOR)}
        ]
    }
}

/// 下拉框：触发器 + 浮层选项列表。
///
/// `id` 是页面自己起的名字，用它区分「哪个下拉框开着」和「选中了谁」；
/// `selected` 只用于初始显示与初始高亮，后续由页面自己维护。
pub fn dropdown(id: &'static str, labels: &[String], selected: usize) -> impl Scene + use<> {
    let current = labels.get(selected).cloned().unwrap_or_default();
    let items = dropdown_items(id, labels, selected);

    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
        }
        Children [
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
            DropdownTrigger(id)
            on(move |mut click: On<PointerClick>, mut open: ResMut<OpenDropdown>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                // 阻断冒泡，否则页面的"点空白收起"会立刻把它关掉
                click.propagate(false);
                open.0 = if open.0 == Some(id) { None } else { Some(id) };
            })
            Children [
                Text::new(current)
                TextFont { font_size: FontSize::Px(13.0) }
                TextColor(TEXT_COLOR)
                DropdownLabel(id)
                --
                @{text("▼", 8.0, LABEL_COLOR)}
            ]
            --
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
                max_height: Val::Px(POPUP_MAX_HEIGHT),
                overflow: Overflow::scroll_y(),
                display: Display::None,
            }
            BackgroundColor(POPUP_BACKGROUND)
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.16))
            GlobalZIndex(150)
            ScrollPosition::DEFAULT
            DropdownPopup(id)
            // 浮层自己的滚轮：拦在这里，避免继续冒泡去滚面板
            on(move |mut scroll: On<PointerScroll>,
                  mut positions: Query<&mut ScrollPosition>,
                  nodes: Query<&ComputedNode>| {
                scroll.propagate(false);
                let pixels = match scroll.unit {
                    MouseScrollUnit::Line => scroll.y * 40.0,
                    MouseScrollUnit::Pixel => scroll.y,
                }
                .clamp(-320.0, 320.0);
                let (Ok(mut position), Ok(computed)) =
                    (positions.get_mut(scroll.entity), nodes.get(scroll.entity))
                else {
                    return;
                };
                let max = (computed.content_size().y - computed.size().y).max(0.0)
                    * computed.inverse_scale_factor();
                position.0.y = (position.0.y - pixels).clamp(0.0, max);
            })
            Children [
                {items}
            ]
        ]
    }
}

/// 下拉框浮层里的一列选项
fn dropdown_items(id: &'static str, labels: &[String], selected: usize) -> Vec<impl Scene + use<>> {
    labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let background = if index == selected {
                BUTTON_SELECTED
            } else {
                Color::NONE
            };
            let label = label.clone();
            bsn! {
                Node {
                    width: Val::Percent(100.0),
                    padding: UiRect::axes(Val::Px(8.0), Val::Px(5.0)),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                }
                BackgroundColor(background)
                DropdownItem { id: id, index: index }
                on(move |mut click: On<PointerClick>,
                      mut open: ResMut<OpenDropdown>,
                      mut picked: MessageWriter<DropdownPicked>| {
                    if click.button != PointerButton::Primary {
                        return;
                    }
                    click.propagate(false);
                    open.0 = None;
                    picked.write(DropdownPicked { id, index });
                })
                Children [
                    @{text(label, 13.0, TEXT_COLOR)}
                ]
            }
        })
        .collect()
}

/// 水平滑动条。`step` 是数值步长，`value` 是初始值。
pub fn slider(value: f32, min: f32, max: f32, step: f32) -> impl Scene + use<> {
    let travel_left = Val::Px(SLIDER_THUMB_SIZE / 2.0);
    let travel_right = Val::Px(SLIDER_THUMB_SIZE / 2.0);
    let thumb_margin = UiRect::left(Val::Px(-SLIDER_THUMB_SIZE / 2.0));

    bsn! {
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
        // 滑块可移动的区域（左右各留出半个滑块，避免滑块越出轨道）
        Children [
            Node {
                position_type: PositionType::Absolute,
                left: travel_left,
                right: travel_right,
                top: Val::Px(0.0),
                bottom: Val::Px(0.0),
            }
            Children [
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    right: Val::Px(0.0),
                    top: Val::Px(9.0),
                    height: Val::Px(6.0),
                    border_radius: BorderRadius::all(Val::Px(3.0)),
                }
                BackgroundColor(Color::srgba(0.2, 0.23, 0.3, 0.95))
                --
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
                --
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
            ]
        ]
    }
}
