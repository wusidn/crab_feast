//! 「移动设置」菜单页：调整输入平滑参数、脚部 IK 开关等
//!
//! 页面结构（字体族由菜单内容区继承，这里只写字号）：
//!
//! ```text
//! root
//! ├─ 开关卡片    输入平滑                        [开启]
//! ├─ 系数卡片    平滑系数                0.50
//! │              轨道 ──────────────●──────────
//! ├─ 开关卡片    脚部 IK（锁脚防滑步）           [开启]
//! │              权重 …·修正 … m（实时读数）
//! └─ 开关卡片    模拟端脚部 IK（远端角色）          [开启]
//!                远端 N 具·本帧最大修正 … m
//! ```
//!
//! 交互是闭环的：控件改动写回对应资源，同步系统再把资源状态刷回界面。

use bevy::picking::pointer::PointerButton;
use bevy::prelude::*;
use bevy::ui_widgets::{Slider, SliderValue, ValueChange, slider_self_update};

use crate::GameState;
use crate::anim::{FootIk, FootIkSettings};
use crate::input::{FacingController, FacingState, MovementSettings, SMOOTHING_MIN};
use crate::ui::debug::DebugTimeScale;
use crate::ui::menu::{MenuConfig, MenuPageBody, MenuPageDef, MenuSet, widgets};

/// 菜单页面 id
pub const MOVEMENT_SETTINGS_PAGE: &str = "movement_settings";

/// 平滑开关按钮
#[derive(Component, Clone, Default)]
pub struct SmoothingToggle;

/// 平滑系数标签
#[derive(Component, Clone, Default)]
pub struct SmoothingFactorLabel;

/// 平滑系数滑动条标记
#[derive(Component, Clone, Default)]
pub struct SmoothingSlider;

/// 脚部 IK 开关按钮
#[derive(Component, Clone, Default)]
pub struct FootIkToggle;

/// 脚部 IK 实时读数（权重 / 修正量）
#[derive(Component, Clone, Default)]
pub struct FootIkStatus;

/// 模拟端（联机远端角色）脚部 IK 开关按钮
#[derive(Component, Clone, Default)]
pub struct RemoteFootIkToggle;

/// 模拟端脚部 IK 实时读数（远端具数 / 本帧最大修正）
#[derive(Component, Clone, Default)]
pub struct RemoteFootIkStatus;

/// 卡片底色（比面板稍亮一点，用来分隔区块）
const CARD_BACKGROUND: Color = Color::srgba(0.16, 0.18, 0.24, 0.55);
/// 开关「关」时的底色
const TOGGLE_IDLE: Color = Color::srgba(0.30, 0.30, 0.30, 0.55);
/// 开关「开」时的底色
const TOGGLE_ON: Color = widgets::BUTTON_SELECTED;

/// 移动设置插件
pub struct MovementSettingsPlugin;

impl Plugin for MovementSettingsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            OnEnter(GameState::Game),
            register_menu_page.before(MenuSet::Setup),
        )
        .add_systems(
            OnEnter(GameState::Game),
            fill_menu_page.after(MenuSet::Setup),
        )
        .add_systems(
            Update,
            (
                update_smoothing_toggle_label.run_if(in_state(GameState::Game)),
                update_slider_from_settings.run_if(in_state(GameState::Game)),
                update_smoothing_factor_label.run_if(in_state(GameState::Game)),
                update_foot_ik_label.run_if(in_state(GameState::Game)),
                update_foot_ik_status.run_if(in_state(GameState::Game)),
                update_remote_foot_ik_label.run_if(in_state(GameState::Game)),
                update_remote_foot_ik_status.run_if(in_state(GameState::Game)),
            ),
        );
    }
}

/// 把「移动设置」注册成菜单里的一级入口
fn register_menu_page(mut config: ResMut<MenuConfig>) {
    if config
        .pages
        .iter()
        .any(|page| page.id == MOVEMENT_SETTINGS_PAGE)
    {
        return;
    }
    config.pages.push(MenuPageDef::section(
        MOVEMENT_SETTINGS_PAGE,
        "移动设置",
        "调整输入平滑和移动参数",
    ));
}

/// 把移动设置控件挂进菜单的「移动设置」页面内容区
///
/// 字体族由 [`MenuPageBody`]（字体作用域）继承下来，这里不关心字体。
fn fill_menu_page(
    mut commands: Commands,
    settings: Res<MovementSettings>,
    foot_ik: Res<FootIkSettings>,
    bodies: Query<(Entity, &MenuPageBody)>,
) {
    let Some((body, _)) = bodies
        .iter()
        .find(|(_, body)| body.page == Some(MOVEMENT_SETTINGS_PAGE))
    else {
        warn!("菜单里没有找到移动设置页面，跳过内容填充");
        return;
    };

    commands.spawn_scene(page_scene(body, &settings, &foot_ik));
}

/// 整页结构：根 → 输入平滑开关卡片 + 平滑系数卡片 + 脚部 IK 卡片
fn page_scene(
    body: Entity,
    settings: &MovementSettings,
    foot_ik: &FootIkSettings,
) -> impl Scene + use<> {
    let foot_ik_enabled = foot_ik.enabled;
    let remote_foot_ik_enabled = foot_ik.remote_enabled;
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
        }
        ChildOf(body)
        Children [
            @{toggle_card(settings)}
            --
            @{factor_card(settings)}
            --
            @{foot_ik_card(foot_ik_enabled)}
            --
            @{remote_foot_ik_card(remote_foot_ik_enabled)}
        ]
    }
}

/// 开关卡片：左标签、右胶囊按钮，点一下切换「输入平滑」
fn toggle_card(settings: &MovementSettings) -> impl Scene + use<> {
    let enabled = settings.smoothing_enabled;
    let toggle_color = if enabled { TOGGLE_ON } else { TOGGLE_IDLE };
    let toggle_text = if enabled { "开启" } else { "关闭" };

    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            padding: UiRect::axes(Val::Px(12.0), Val::Px(12.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(8.0)),
        }
        BackgroundColor(CARD_BACKGROUND)
        BorderColor::all(widgets::BORDER_SOFT)
        Children [
            @{widgets::text("输入平滑", 14.0, widgets::TEXT_COLOR)}
            --
            // 按钮本身就是文字节点：`update_smoothing_toggle_label` 直接改它的 Text 与底色
            Node {
                width: Val::Px(52.0),
                height: Val::Px(26.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border_radius: BorderRadius::all(Val::Px(13.0)),
            }
            Text::new(toggle_text)
            TextFont { font_size: FontSize::Px(12.0) }
            TextColor(Color::WHITE)
            BackgroundColor(toggle_color)
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.22))
            SmoothingToggle
            // 点击即取反，状态存进资源，刷新交给同步系统
            on(|mut click: On<PointerClick>, mut settings: ResMut<MovementSettings>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                settings.smoothing_enabled = !settings.smoothing_enabled;
            })
        ]
    }
}

/// 系数卡片：标题行（名称 + 当前值）+ 滑杆
fn factor_card(settings: &MovementSettings) -> impl Scene + use<> {
    let factor = settings.smoothing_factor;
    let factor_text = format!("{factor:.2}");
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            row_gap: Val::Px(10.0),
            padding: UiRect::axes(Val::Px(12.0), Val::Px(12.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(8.0)),
        }
        BackgroundColor(CARD_BACKGROUND)
        BorderColor::all(widgets::BORDER_SOFT)
        Children [
            Node {
                width: Val::Percent(100.0),
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
            }
            Children [
                @{widgets::text("平滑系数", 12.0, widgets::LABEL_COLOR)}
                --
                Text::new(factor_text)
                TextFont { font_size: FontSize::Px(12.0) }
                TextColor(widgets::TEXT_COLOR)
                SmoothingFactorLabel
            ]
            --
            // 滑杆：滑块与进度条的定位由 widgets::update_slider_styling 统一处理
            // 下限是 SMOOTHING_MIN：系数到 0 会让看向冻住
            @{widgets::slider(
                factor,
                SMOOTHING_MIN,
                1.0,
                0.01,
            )}
            SmoothingSlider
            on(slider_self_update)
            // 拖动写回资源；两位小数的显示由同步系统负责
            on(|change: On<ValueChange<f32>>, mut settings: ResMut<MovementSettings>| {
                settings.smoothing_factor = change.value.clamp(0.0, 1.0);
            })
        ]
    }
}

/// 更新平滑开关标签显示
fn update_smoothing_toggle_label(
    settings: Res<MovementSettings>,
    mut toggle_labels: Query<(&mut Text, &mut BackgroundColor), With<SmoothingToggle>>,
) {
    for (mut text, mut bg_color) in &mut toggle_labels {
        let (new_text, color) = if settings.smoothing_enabled {
            ("开启", TOGGLE_ON)
        } else {
            ("关闭", TOGGLE_IDLE)
        };
        if text.0 != new_text {
            text.0 = new_text.to_string();
        }
        if bg_color.0 != color {
            bg_color.0 = color;
        }
    }
}

/// 从 MovementSettings 更新滑动条显示
fn update_slider_from_settings(
    settings: Res<MovementSettings>,
    sliders: Query<(Entity, &SmoothingSlider, &SliderValue), With<Slider>>,
    mut commands: Commands,
) {
    for (entity, _slider_marker, current) in &sliders {
        let desired = settings.smoothing_factor;
        if (current.0 - desired).abs() > 0.001 {
            commands.entity(entity).insert(SliderValue(desired));
        }
    }
}

/// 更新平滑系数标签显示
fn update_smoothing_factor_label(
    settings: Res<MovementSettings>,
    mut labels: Query<&mut Text, With<SmoothingFactorLabel>>,
) {
    let value = format!("{:.2}", settings.smoothing_factor);
    for mut text in &mut labels {
        if text.0 != value {
            text.0 = value.clone();
        }
    }
}

/// 脚部 IK 卡片：点一下切换全局开关。运行时只是改资源里的布尔值，
/// 淡出由 `FootIkSettings::blend_secs` 负责，所以可以反复快速切换。
/// 下面一行是实时读数：混合权重 + 两只脚这一帧被拉动的距离，用来判断锁脚有没有在干活。
fn foot_ik_card(enabled: bool) -> impl Scene + use<> {
    let toggle_color = if enabled { TOGGLE_ON } else { TOGGLE_IDLE };
    let toggle_text = if enabled { "开启" } else { "关闭" };

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
            }
            Children [
                @{widgets::text("脚部 IK（锁脚防滑步）", 14.0, widgets::TEXT_COLOR)}
                --
                Node {
                    width: Val::Px(52.0),
                    height: Val::Px(26.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border_radius: BorderRadius::all(Val::Px(13.0)),
                }
                Text::new(toggle_text)
                TextFont { font_size: FontSize::Px(12.0) }
                TextColor(Color::WHITE)
                BackgroundColor(toggle_color)
                BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.22))
                FootIkToggle
                on(|mut click: On<PointerClick>, mut settings: ResMut<FootIkSettings>| {
                    if click.button != PointerButton::Primary {
                        return;
                    }
                    click.propagate(false);
                    settings.enabled = !settings.enabled;
                })
            ]
            --
            Text::new("")
            TextFont { font_size: FontSize::Px(11.0) }
            TextColor(Color::srgba(0.86, 0.86, 0.86, 0.72))
            FootIkStatus
        ]
    }
}

/// 模拟端脚部 IK 卡片：只关远端角色（`FootIkTarget` 标记的那些），本地角色照旧。
///
/// 远端没有本地物理兜底，脚部数据全靠服务器实测速度 + 混合空间姿态，排查远端
/// 滑步/髋锁时常常需要单独停掉它；和总开关分开，免得连带把本地锁脚也关了。
/// 下面一行是实时读数：当前有几具远端骨架在解、这一帧最大修正多少米。
fn remote_foot_ik_card(enabled: bool) -> impl Scene + use<> {
    let toggle_color = if enabled { TOGGLE_ON } else { TOGGLE_IDLE };
    let toggle_text = if enabled { "开启" } else { "关闭" };

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
            }
            Children [
                @{widgets::text("模拟端脚部 IK（远端角色）", 14.0, widgets::TEXT_COLOR)}
                --
                Node {
                    width: Val::Px(52.0),
                    height: Val::Px(26.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border_radius: BorderRadius::all(Val::Px(13.0)),
                }
                Text::new(toggle_text)
                TextFont { font_size: FontSize::Px(12.0) }
                TextColor(Color::WHITE)
                BackgroundColor(toggle_color)
                BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.22))
                RemoteFootIkToggle
                on(|mut click: On<PointerClick>, mut settings: ResMut<FootIkSettings>| {
                    if click.button != PointerButton::Primary {
                        return;
                    }
                    click.propagate(false);
                    settings.remote_enabled = !settings.remote_enabled;
                })
            ]
            --
            Text::new("")
            TextFont { font_size: FontSize::Px(11.0) }
            TextColor(Color::srgba(0.86, 0.86, 0.86, 0.72))
            RemoteFootIkStatus
        ]
    }
}

/// 更新模拟端脚部 IK 开关标签显示
fn update_remote_foot_ik_label(
    settings: Res<FootIkSettings>,
    mut toggle_labels: Query<(&mut Text, &mut BackgroundColor), With<RemoteFootIkToggle>>,
) {
    let (new_text, color) = if settings.remote_enabled {
        ("开启", widgets::BUTTON_SELECTED)
    } else {
        ("关闭", TOGGLE_IDLE)
    };
    for (mut text, mut bg_color) in &mut toggle_labels {
        if text.0 != new_text {
            text.0 = new_text.to_string();
        }
        if bg_color.0 != color {
            bg_color.0 = color;
        }
    }
}

/// 更新模拟端实时读数：远端具数 + 本帧最大修正距离
fn update_remote_foot_ik_status(
    settings: Res<FootIkSettings>,
    iks: Query<(&FootIk, &crate::anim::FootIkTarget)>,
    mut labels: Query<&mut Text, With<RemoteFootIkStatus>>,
) {
    let line = if !settings.enabled {
        "总开关已关闭".to_string()
    } else if !settings.remote_enabled {
        "已关闭（远端角色按动画姿态落地）".to_string()
    } else {
        let mut ready = 0usize;
        let mut worst = 0.0f32;
        for (ik, _) in &iks {
            if !ik.is_ready() {
                continue;
            }
            ready += 1;
            let [left, right] = ik.correction();
            worst = worst.max(left.max(right));
        }
        if ready == 0 {
            "当前没有远端角色".to_string()
        } else {
            format!("远端 {ready} 具 · 本帧最大修正 {worst:.3} m")
        }
    };
    for mut text in &mut labels {
        if text.0 != line {
            text.0 = line.clone();
        }
    }
}

/// 更新脚部 IK 开关标签显示
fn update_foot_ik_label(
    settings: Res<FootIkSettings>,
    mut toggle_labels: Query<(&mut Text, &mut BackgroundColor), With<FootIkToggle>>,
) {
    let (new_text, color) = if settings.enabled {
        ("开启", widgets::BUTTON_SELECTED)
    } else {
        ("关闭", TOGGLE_IDLE)
    };
    for (mut text, mut bg_color) in &mut toggle_labels {
        if text.0 != new_text {
            text.0 = new_text.to_string();
        }
        if bg_color.0 != color {
            bg_color.0 = color;
        }
    }
}

/// 更新脚部 IK 实时读数：权重、支撑、两只脚这一帧的修正距离
fn update_foot_ik_status(
    settings: Res<FootIkSettings>,
    scale: Res<DebugTimeScale>,
    // 只看本地角色：远端骨架也带 `FootIk`，混在一起读数会随机挑到远端
    ik: Query<&FootIk, Without<crate::anim::FootIkTarget>>,
    facing: Query<(&FacingState, &FacingController)>,
    mut labels: Query<&mut Text, With<FootIkStatus>>,
) {
    // 转向门控读数：support → 实际顶速。跑动时看这一对数就知道腾空段有没有压住
    let turn = facing
        .iter()
        .next()
        .map(|(state, controller)| (state.support, controller.turn_speed_at(state.support)));
    let line = if !settings.enabled {
        "已关闭".to_string()
    } else if let Some(ik) = ik.iter().next() {
        if !ik.is_ready() {
            "等待角色模型…".to_string()
        } else {
            let [left, right] = ik.correction();
            let [contact_left, contact_right] = ik.contacts;
            let [planted_left, planted_right] = ik.planted();
            let [roll_left, roll_right] = ik.roll();
            let [heel_left, heel_right] = ik.heel_off();
            let [ankle_left, ankle_right] = ik.ankle_deg();
            // 前掌骨折角：抬跟时脚趾要留在地上，这个数应该跟着抬跟一起长
            let [toe_left, toe_right] = ik.toe_bend_deg();
            // 偏航补偿：踩实期间把脚掌拧回来的角度，顶到 foot_yaw_limit 就是撑不住了
            let [yaw_left, yaw_right] = ik.yaw_lock_deg();
            let [strength_left, strength_right] = ik.strength();
            // 承重点：踩实后期承重点滑到前掌（「掌」），前期钉踝（「踝」）。
            // 踝角 = 脚掌与小腿的夹角，90 = 站立、蹬地时该到 130~160
            let pivot = |planted: bool, roll: f32| {
                if !planted {
                    "-"
                } else if roll > 0.5 {
                    "掌"
                } else {
                    "踝"
                }
            };
            // 射线量到的地面：没打中显示「-」，打中显示高度和坡度
            // （坡度 = 命中法线偏离竖直的角度，爬楼梯/斜坡时看它有没有跳）
            let ground = |index: usize| match ik.ground(index) {
                Some(hit) => format!(
                    "{:.2}/{:.0}°",
                    hit.height,
                    hit.normal.dot(Vec3::Y).clamp(-1.0, 1.0).acos().to_degrees()
                ),
                None => "-".to_string(),
            };
            // 转向门控：前一个数是低通过的「脚在地上」权重，后一个数是它算出来的顶速。
            // 跑动时应当看到它随跑步节奏在 45~90°/s 之间来回走（腾空 45、支撑 90）
            let turn = match turn {
                Some((support, speed)) => format!("{support:.2}→{speed:.0}°/s"),
                None => "-".to_string(),
            };
            format!(
                "权重 {:.2} · 支撑 {:.2}/{:.2} · 强度 {:.2}/{:.2} · 承重 {}/{} · 抬跟 {:.0}%/{:.0}% · 踝角 {:.0}°/{:.0}° · 趾 {:.0}°/{:.0}° · 偏航 {:.0}°/{:.0}° · 转向 {} · 修正 {:.3}/{:.3} m · 地面 {}/{} · 时间 ×{:.2}",
                ik.blend(),
                contact_left,
                contact_right,
                strength_left,
                strength_right,
                pivot(planted_left, roll_left),
                pivot(planted_right, roll_right),
                heel_left * 100.0,
                heel_right * 100.0,
                ankle_left,
                ankle_right,
                toe_left,
                toe_right,
                yaw_left,
                yaw_right,
                turn,
                left,
                right,
                ground(0),
                ground(1),
                scale.speed
            )
        }
    } else {
        "当前没有角色".to_string()
    };

    for mut text in &mut labels {
        if text.0 != line {
            text.0 = line.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::scene::ScenePlugin;
    use bevy::ui_widgets::{SliderRange, SliderThumb};

    fn build_test_page() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            ScenePlugin::default(),
        ));
        app.insert_resource(MovementSettings::default());
        app.insert_resource(FootIkSettings::default());
        app.insert_resource(DebugTimeScale::default());

        let body = app
            .world_mut()
            .spawn(MenuPageBody {
                page: Some(MOVEMENT_SETTINGS_PAGE),
            })
            .id();

        app.world_mut()
            .run_system_once(
                move |mut commands: Commands,
                      settings: Res<MovementSettings>,
                      foot_ik: Res<FootIkSettings>| {
                    commands.spawn_scene(page_scene(body, &settings, &foot_ik));
                },
            )
            .expect("页面构建系统跑不起来");

        // 场景是排队生成的，需要走一帧让 SceneSpawner 落地
        app.update();
        (app, body)
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

    fn children_of(world: &World, entity: Entity) -> Vec<Entity> {
        world
            .get::<Children>(entity)
            .map(|children| children.iter().collect())
            .unwrap_or_default()
    }

    /// 页面必须是「根 → 四张卡片 → 卡片内的行/控件」的嵌套，而不是一列扁平兄弟
    #[test]
    fn page_is_four_cards_with_nested_controls() {
        let (app, body) = build_test_page();
        let world = app.world();

        let roots = children_of(world, body);
        assert_eq!(roots.len(), 1, "页面根应该只有一个");
        let root = roots[0];
        let cards = children_of(world, root);
        assert_eq!(
            cards.len(),
            4,
            "应该是输入平滑开关卡片 + 系数卡片 + 脚部 IK 卡片 + 模拟端脚部 IK 卡片"
        );

        // 开关卡片：标签和按钮都得在卡片里
        let toggle_card = cards[0];
        let toggle_card_children = children_of(world, toggle_card);
        assert_eq!(toggle_card_children.len(), 2, "开关卡片 = 标签 + 按钮");
        let toggle = toggle_card_children[1];
        assert!(world.get::<SmoothingToggle>(toggle).is_some());
        // Text 与 BackgroundColor 在同一个实体上，`update_smoothing_toggle_label` 才匹配得到
        assert!(world.get::<Text>(toggle).is_some(), "开关按钮本身要有文字");
        assert!(world.get::<BackgroundColor>(toggle).is_some());

        // 系数卡片：标题行 + 滑杆
        let slider_card = cards[1];
        let slider_card_children = children_of(world, slider_card);
        assert_eq!(slider_card_children.len(), 2, "系数卡片 = 标题行 + 滑杆");
        assert_eq!(
            children_of(world, slider_card_children[0]).len(),
            2,
            "标题行 = 名称 + 当前值"
        );

        let slider_root = slider_card_children[1];
        assert!(world.get::<Slider>(slider_root).is_some());
        assert!(
            world.get::<SliderRange>(slider_root).is_some(),
            "没有 Range 摆不了滑块"
        );
        assert!(world.get::<SliderValue>(slider_root).is_some());
        assert!(world.get::<SmoothingSlider>(slider_root).is_some());

        // 滑块与进度条必须在滑杆子树里，`update_slider_styling` 只遍历 root 的后代
        let descendants = descendants_of(world, slider_root);
        assert!(
            descendants
                .iter()
                .any(|entity| world.get::<SliderThumb>(*entity).is_some()),
            "滑块必须是滑杆的后代"
        );
        assert!(
            descendants
                .iter()
                .any(|entity| world.get::<widgets::SliderFill>(*entity).is_some()),
            "进度条必须是滑杆的后代"
        );

        // 脚部 IK 卡片：第一行是「标签 + 按钮」，下面一行是实时读数
        let ik_card = cards[2];
        let ik_children = children_of(world, ik_card);
        assert_eq!(ik_children.len(), 2, "脚部 IK 卡片 = 开关行 + 读数行");
        let ik_row = ik_children[0];
        let ik_row_children = children_of(world, ik_row);
        assert_eq!(ik_row_children.len(), 2, "开关行 = 标签 + 按钮");
        assert!(world.get::<FootIkToggle>(ik_row_children[1]).is_some());
        assert!(world.get::<FootIkStatus>(ik_children[1]).is_some());

        // 模拟端脚部 IK 卡片：同样的「开关行 + 读数行」结构，标记必须是独立的那一对，
        // 否则两个开关会互相改写对方的状态
        let remote_card = cards[3];
        let remote_children = children_of(world, remote_card);
        assert_eq!(remote_children.len(), 2, "模拟端卡片 = 开关行 + 读数行");
        let remote_row = children_of(world, remote_children[0]);
        assert_eq!(remote_row.len(), 2, "开关行 = 标签 + 按钮");
        assert!(
            world.get::<RemoteFootIkToggle>(remote_row[1]).is_some(),
            "模拟端开关按钮要有自己的标记"
        );
        assert!(
            world.get::<FootIkToggle>(remote_row[1]).is_none(),
            "模拟端按钮不能复用本地脚部 IK 的标记"
        );
        assert!(
            world
                .get::<RemoteFootIkStatus>(remote_children[1])
                .is_some(),
            "模拟端读数行要有自己的标记"
        );
        assert!(
            world.get::<FootIkStatus>(remote_children[1]).is_none(),
            "模拟端读数不能复用本地读数标记"
        );

        // 默认状态：总开关关、模拟端开
        assert!(
            app.world().resource::<FootIkSettings>().enabled,
            "脚部 IK 默认应该是开启"
        );
        assert!(
            app.world().resource::<FootIkSettings>().remote_enabled,
            "模拟端脚部 IK 默认应该是开启"
        );
    }
}
