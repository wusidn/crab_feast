//! 「系统诊断」菜单页：把每条移动/表现链路的实时指标与状态灯列出来。
//!
//! 页面结构：
//!
//! ```text
//! root
//! ├─ 总览卡片      当前整体状态（正常 / 警告 / 异常 / 离线）+ 一句汇总
//! ├─ 链路卡片 ×7    [状态灯] 链路名              主数值
//! │                 副信息（固定一行高）
//! │                 原因（固定一行高，仅警告/异常时有字）
//! ```
//!
//! 数据来自 [`crate::diagnose::Diagnostics`]：采集系统在后台每帧读数，本页只负责
//! 把资源里的行渲染出来并刷新文本/灯色，不做任何计算或写回。
//!
//! **布局必须全静态**：这个面板是叠在对局上的，任何高度变化都会让整页重排、连带
//! 滚动范围跳动，看起来就是「面板在抖」。所以三行文本都锁死成常量高度（
//! [`SUB_LINE_H`] / [`VALUE_SLOT_W`]），一律 `LineBreak::NoWrap` + 裁剪，内容超出
//! 就在单行内截断；文本刷新按 [`SYNC_INTERVAL`] 节流，内容不变不赋值。

use bevy::prelude::*;

use crate::GameState;
use crate::diagnose::{DiagnosticRowId, DiagnosticStatus, Diagnostics, SUB_MAX_UNITS, clip_text};
use crate::ui::menu::{MenuConfig, MenuPageBody, MenuPageDef, MenuSet, widgets};

/// 菜单页面 id
pub const DIAGNOSE_PAGE: &str = "diagnose";

/// 卡片底色（与其它设置页保持一致）
const CARD_BACKGROUND: Color = Color::srgba(0.16, 0.18, 0.24, 0.55);

/// 副信息 / 原因行高：一行 11px 字的高度。**固定住**，文本空不空都占这么高，
/// 卡片高度就不会在 0 行 / 1 行之间跳。
const SUB_LINE_H: f32 = 14.0;

/// 主数值槽宽度：固定宽度 + 右对齐，数字位数变化时数值不会左右抖。
const VALUE_SLOT_W: f32 = 84.0;

/// 主数值最多占几个半角单位（12px 字、84px 槽约这么宽）。
const VALUE_MAX_UNITS: usize = 14;

/// 文本刷新的最小间隔（秒）：实时指标每帧都在变，逐帧写文本会让排版每帧重算。
const SYNC_INTERVAL: f32 = 0.15;

/// 总览状态灯
#[derive(Component, Clone, Copy, Default)]
struct OverviewLight;
/// 总览状态文字
#[derive(Component, Clone, Copy, Default)]
struct OverviewLabel;
/// 总览汇总说明
#[derive(Component, Clone, Copy, Default)]
struct OverviewDetail;

/// 一条链路卡片上的状态灯；组件带链路 id，同步系统据此找对应行。
#[derive(Component, Clone, Default)]
struct RowLight(DiagnosticRowId);
/// 一条链路的主数值
#[derive(Component, Clone, Default)]
struct RowPrimary(DiagnosticRowId);
/// 一条链路的副信息
#[derive(Component, Clone, Default)]
struct RowDetail(DiagnosticRowId);
/// 一条链路的原因说明（警告/异常时才填文字）
#[derive(Component, Clone, Default)]
struct RowReason(DiagnosticRowId);
/// 系统诊断插件
pub struct DiagnoseUiPlugin;

impl Plugin for DiagnoseUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            OnEnter(GameState::Game),
            register_menu_page.before(MenuSet::Setup),
        )
        .add_systems(
            OnEnter(GameState::Game),
            fill_menu_page.after(MenuSet::Setup),
        )
        .add_systems(Update, sync_page.run_if(in_state(GameState::Game)));
    }
}

/// 把「系统诊断」注册成菜单里的一级入口
fn register_menu_page(mut config: ResMut<MenuConfig>) {
    if config.pages.iter().any(|page| page.id == DIAGNOSE_PAGE) {
        return;
    }
    config.pages.push(MenuPageDef::section(
        DIAGNOSE_PAGE,
        "系统诊断",
        "逐条链路实时状态：定位抖动来源",
    ));
}

/// 把总览卡片 + 九条链路卡片挂进诊断页面内容区
fn fill_menu_page(mut commands: Commands, bodies: Query<(Entity, &MenuPageBody)>) {
    let Some((body, _)) = bodies
        .iter()
        .find(|(_, body)| body.page == Some(DIAGNOSE_PAGE))
    else {
        warn!("菜单里没有找到系统诊断页面，跳过内容填充");
        return;
    };

    commands.spawn_scene(page_scene(body));
}

/// 整页结构：根 → 总览卡片 + 七条链路卡片
fn page_scene(body: Entity) -> impl Scene + use<> {
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
        }
        ChildOf(body)
        Children [
            @{overview_card()}
            --
            @{row_card(DiagnosticRowId::PhysicsStepRate)}
            --
            @{row_card(DiagnosticRowId::Facing)}
            --
            @{row_card(DiagnosticRowId::LinearVelocity)}
            --
            @{row_card(DiagnosticRowId::NetSnapshot)}
            --
            @{row_card(DiagnosticRowId::AnimBlend)}
            --
            @{row_card(DiagnosticRowId::FootIk)}
            --
            @{row_card(DiagnosticRowId::CameraFollow)}
        ]
    }
}

/// 总览卡片：状态灯 + 整体状态 + 一句汇总。
fn overview_card() -> impl Scene + use<> {
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(6.0),
            padding: UiRect::axes(Val::Px(12.0), Val::Px(12.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(8.0)),
        }
        BackgroundColor(CARD_BACKGROUND)
        BorderColor::all(widgets::BORDER_SOFT)
        Children [
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(10.0),
                align_items: AlignItems::Center,
            }
            Children [
                @{status_light(DiagnosticStatus::Offline)}
                OverviewLight
                --
                @{widgets::text("整体状态", 14.0, widgets::TEXT_COLOR)}
                --
                Node {
                    width: Val::Px(56.0),
                    overflow: Overflow::clip(),
                }
                Text::new("—")
                TextFont { font_size: FontSize::Px(13.0) }
                TextColor(widgets::TEXT_COLOR)
                TextLayout::new(Justify::Right, LineBreak::NoWrap)
                OverviewLabel
            ]
            --
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(SUB_LINE_H),
                overflow: Overflow::clip(),
            }
            Text::new("")
            TextFont { font_size: FontSize::Px(11.0) }
            TextColor(widgets::LABEL_COLOR)
            TextLayout::no_wrap()
            OverviewDetail
        ]
    }
}

/// 一条链路卡片：状态灯 + 标题 + 主数值，下面副信息与原因。
///
/// 三行文本都锁死成一行：主数值固定槽 + 右对齐，副信息/原因固定行高 + 裁剪。
/// 这样卡片高度是常量，文本怎么变都不会让整页重新排版。
fn row_card(id: DiagnosticRowId) -> impl Scene + use<> {
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(4.0),
            padding: UiRect::axes(Val::Px(12.0), Val::Px(12.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(8.0)),
        }
        BackgroundColor(CARD_BACKGROUND)
        BorderColor::all(widgets::BORDER_SOFT)
        Children [
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                column_gap: Val::Px(10.0),
            }
            Children [
                Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(10.0),
                    align_items: AlignItems::Center,
                }
                Children [
                    @{status_light(DiagnosticStatus::Offline)}
                    RowLight(id)
                    --
                    @{widgets::text(id.title(), 13.0, widgets::TEXT_COLOR)}
                ]
                --
                Node {
                    width: Val::Px(VALUE_SLOT_W),
                    overflow: Overflow::clip(),
                }
                Text::new("—")
                TextFont { font_size: FontSize::Px(12.0) }
                TextColor(widgets::TEXT_COLOR)
                TextLayout::new(Justify::Right, LineBreak::NoWrap)
                RowPrimary(id)
            ]
            --
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(SUB_LINE_H),
                overflow: Overflow::clip(),
            }
            Text::new("")
            TextFont { font_size: FontSize::Px(11.0) }
            TextColor(widgets::LABEL_COLOR)
            TextLayout::no_wrap()
            RowDetail(id)
            --
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(SUB_LINE_H),
                overflow: Overflow::clip(),
            }
            Text::new("")
            TextFont { font_size: FontSize::Px(11.0) }
            TextColor(Color::srgb(0.92, 0.55, 0.52))
            TextLayout::no_wrap()
            RowReason(id)
        ]
    }
}

/// 一枚圆形状态灯：颜色由状态决定。
fn status_light(status: DiagnosticStatus) -> impl Scene + use<> {
    let light_color = status.color();
    bsn! {
        Node {
            width: Val::Px(10.0),
            height: Val::Px(10.0),
            border_radius: BorderRadius::all(Val::Px(5.0)),
        }
        BackgroundColor(light_color)
    }
}

/// 汇总一句：有几条警告/异常。
fn summarize(diag: &Diagnostics) -> String {
    let mut warn = 0usize;
    let mut bad = 0usize;
    let mut online = 0usize;
    for id in DiagnosticRowId::ALL {
        match diag.row(id).status {
            DiagnosticStatus::Bad => bad += 1,
            DiagnosticStatus::Warn => warn += 1,
            DiagnosticStatus::Ok => online += 1,
            DiagnosticStatus::Offline => {}
        }
    }
    if online == 0 && warn == 0 && bad == 0 {
        return "等待进入对局…".to_string();
    }
    match (bad, warn) {
        (0, 0) => "所有在线链路指标正常".to_string(),
        (0, w) => format!("{w} 条链路逼近阈值，建议留意"),
        (b, 0) => format!("{b} 条链路异常，很可能是抖动来源"),
        (b, w) => format!("{b} 条异常 · {w} 条警告"),
    }
}

/// 刷新节拍：累计到 [`SYNC_INTERVAL`] 才写文本（首帧直接写，避免开局空一行）。
#[derive(Default)]
struct SyncClock {
    accum: f32,
    primed: bool,
}

/// 把诊断资源刷进界面：状态灯颜色每帧跟，文本按 [`SYNC_INTERVAL`] 节流。
///
/// 文本一律先裁到单行宽度再写，且内容没变就不赋值——`Text` 被改动会触发重新
/// 排版，逐帧无条件写值是整页抖动的来源。
#[expect(clippy::type_complexity)]
fn sync_page(
    diag: Res<Diagnostics>,
    time: Res<Time>,
    mut clock: Local<SyncClock>,
    mut lights: Query<(
        Option<&OverviewLight>,
        Option<&RowLight>,
        &mut BackgroundColor,
    )>,
    mut texts: Query<(
        Option<&OverviewLabel>,
        Option<&OverviewDetail>,
        Option<&RowPrimary>,
        Option<&RowDetail>,
        Option<&RowReason>,
        &mut Text,
    )>,
) {
    let overall = diag.overall();
    for (overview_light, row_light, mut bg) in &mut lights {
        let color = match row_light {
            Some(id) => diag.row(id.0).status.color(),
            None if overview_light.is_some() => overall.color(),
            None => continue,
        };
        if bg.0 != color {
            bg.0 = color;
        }
    }

    clock.accum += time.delta_secs();
    if clock.primed && clock.accum < SYNC_INTERVAL {
        return;
    }
    clock.primed = true;
    clock.accum = 0.0;

    let summary = summarize(&diag);
    for (overview_label, overview_detail, primary, detail, reason, mut text) in &mut texts {
        if overview_label.is_some() {
            if text.0 != overall.label() {
                text.0 = overall.label().to_string();
            }
            continue;
        }
        if overview_detail.is_some() {
            if text.0 != summary {
                text.0 = summary.clone();
            }
            continue;
        }
        let (source, max_units) = if let Some(id) = primary {
            (&diag.row(id.0).primary, VALUE_MAX_UNITS)
        } else if let Some(id) = detail {
            (&diag.row(id.0).detail, SUB_MAX_UNITS)
        } else if let Some(id) = reason {
            (&diag.row(id.0).reason, SUB_MAX_UNITS)
        } else {
            continue;
        };
        let value = clip_text(source, max_units);
        if text.0 != value {
            text.0 = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::app::App;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::scene::ScenePlugin;

    fn build_test_page() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            ScenePlugin::default(),
        ));
        app.init_resource::<Diagnostics>();
        let body = app
            .world_mut()
            .spawn(MenuPageBody {
                page: Some(DIAGNOSE_PAGE),
            })
            .id();

        app.world_mut()
            .run_system_once(move |mut commands: Commands| {
                commands.spawn_scene(page_scene(body));
            })
            .expect("页面构建系统跑不起来");

        app.update();
        (app, body)
    }

    fn children_of(world: &World, entity: Entity) -> Vec<Entity> {
        world
            .get::<Children>(entity)
            .map(|children| children.iter().collect())
            .unwrap_or_default()
    }

    /// 在子树里找第一个带指定组件的实体。
    fn find_in_tree<C: Component>(world: &World, root: Entity) -> Option<Entity> {
        let mut stack = vec![root];
        while let Some(entity) = stack.pop() {
            if world.get::<C>(entity).is_some() {
                return Some(entity);
            }
            stack.extend(children_of(world, entity));
        }
        None
    }

    /// 页面 = 总览卡片 + 九条链路卡片，共十张。
    #[test]
    fn page_is_overview_plus_seven_rows() {
        let (app, body) = build_test_page();
        let world = app.world();
        let root = children_of(world, body)[0];
        let cards = children_of(world, root);
        assert_eq!(cards.len(), 8, "总览 + 七条链路");

        // 总览卡片：第一行（灯 + 标题 + 状态文字），第二行汇总说明
        let overview = cards[0];
        let overview_kids = children_of(world, overview);
        let overview_head = overview_kids[0];
        let head_kids = children_of(world, overview_head);
        assert!(world.get::<OverviewLight>(head_kids[0]).is_some());
        assert!(world.get::<OverviewLabel>(head_kids[2]).is_some());
        assert!(world.get::<OverviewDetail>(overview_kids[1]).is_some());

        // 七条链路按固定顺序注册，每张卡片都有灯/主值/副信息/原因四个标记
        for (index, id) in DiagnosticRowId::ALL.iter().enumerate() {
            let card = cards[index + 1];
            let kids = children_of(world, card);
            assert_eq!(kids.len(), 3, "链路卡片 = 标题行 + 副信息 + 原因");
            let head = kids[0];
            // 各标记在子树里按组件查找，不依赖具体嵌套层级
            let light = find_in_tree::<RowLight>(world, head);
            assert!(light.is_some_and(|entity| {
                world
                    .get::<RowLight>(entity)
                    .is_some_and(|marker| marker.0 == *id)
            }));
            assert!(find_in_tree::<RowPrimary>(world, head).is_some());
            assert!(world.get::<RowDetail>(kids[1]).is_some());
            assert!(world.get::<RowReason>(kids[2]).is_some());
        }
    }

    /// 同步系统把诊断资源里的状态写进总览灯与文字。
    #[test]
    fn sync_overview_reflects_diagnostics() {
        let (mut app, _body) = build_test_page();

        // 全部离线 → 总览离线
        app.world_mut()
            .run_system_once(sync_page)
            .expect("同步系统跑不起来");
        let overview = app
            .world_mut()
            .query_filtered::<&Text, With<OverviewLabel>>()
            .iter(app.world())
            .next()
            .unwrap();
        assert_eq!(overview.0, DiagnosticStatus::Offline.label());

        // 标一条异常 → 总览异常
        {
            let mut diag = app.world_mut().resource_mut::<Diagnostics>();
            diag.row_mut(DiagnosticRowId::NetSnapshot).set(
                DiagnosticStatus::Bad,
                "0.200 m",
                "命中 0 · 未命中 10",
                "残差过大",
            );
        }
        app.world_mut()
            .run_system_once(sync_page)
            .expect("同步系统跑不起来");
        let overview = app
            .world_mut()
            .query_filtered::<&Text, With<OverviewLabel>>()
            .iter(app.world())
            .next()
            .unwrap();
        assert_eq!(overview.0, DiagnosticStatus::Bad.label());
    }
}
