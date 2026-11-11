//! 「移动设置」菜单页：调整输入平滑参数
//!
//! 页面结构（字体族由菜单内容区继承，这里只写字号）：
//!
//! ```text
//! root
//! ├─ 开关卡片    输入平滑                        [开启]
//! └─ 系数卡片    平滑系数                0.50
//!                轨道 ──────────────●──────────
//! ```
//!
//! 交互是闭环的：控件改动写回 [`MovementSettings`]，同步系统再把资源状态刷回界面。

use bevy::picking::pointer::PointerButton;
use bevy::prelude::*;
use bevy::ui_widgets::{Slider, SliderValue, ValueChange, slider_self_update};

use crate::GameState;
use crate::input::{MovementSettings, SMOOTHING_MIN};
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
    bodies: Query<(Entity, &MenuPageBody)>,
) {
    let Some((body, _)) = bodies
        .iter()
        .find(|(_, body)| body.page == Some(MOVEMENT_SETTINGS_PAGE))
    else {
        warn!("菜单里没有找到移动设置页面，跳过内容填充");
        return;
    };

    commands.spawn_scene(page_scene(body, &settings));
}

/// 整页结构：根 → 开关卡片 + 系数卡片
fn page_scene(body: Entity, settings: &MovementSettings) -> impl Scene + use<> {
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

        let body = app
            .world_mut()
            .spawn(MenuPageBody {
                page: Some(MOVEMENT_SETTINGS_PAGE),
            })
            .id();

        app.world_mut()
            .run_system_once(
                move |mut commands: Commands, settings: Res<MovementSettings>| {
                    commands.spawn_scene(page_scene(body, &settings));
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

    /// 页面必须是「根 → 两张卡片 → 卡片内的行/控件」的嵌套，而不是一列扁平兄弟
    #[test]
    fn page_is_two_cards_with_nested_controls() {
        let (app, body) = build_test_page();
        let world = app.world();

        let roots = children_of(world, body);
        assert_eq!(roots.len(), 1, "页面根应该只有一个");
        let root = roots[0];
        let cards = children_of(world, root);
        assert_eq!(cards.len(), 2, "应该是开关卡片 + 系数卡片");

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
    }
}
