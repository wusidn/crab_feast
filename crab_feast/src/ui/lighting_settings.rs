//! 「画面设置」菜单页：调整全局光照（主平行光）的亮度。
//!
//! 页面结构（字体族由菜单内容区继承，这里只写字号）：
//!
//! ```text
//! root
//! └─ 卡片 全局光照亮度      300 lux
//!          轨道 ────────●──────────
//!          说明（单位、默认值、0 = 关灯）
//! ```
//!
//! 交互闭环：拖滑杆写回 [`LightingSettings`]，[`crate::lighting::LightingPlugin`]
//! 再把亮度刷进带 `GlobalLight` 标记的主平行光，同步系统把资源状态刷回界面。

use bevy::prelude::*;
use bevy::ui_widgets::{Slider, SliderValue, ValueChange, slider_self_update};

use crate::GameState;
use crate::lighting::{GLOBAL_ILLUMINANCE_RANGE, LightingSettings};
use crate::ui::menu::{MenuConfig, MenuPageBody, MenuPageDef, MenuSet, widgets};

/// 菜单页面 id
pub const LIGHTING_SETTINGS_PAGE: &str = "lighting_settings";

/// 卡片底色（与其它设置页保持一致）
const CARD_BACKGROUND: Color = Color::srgba(0.16, 0.18, 0.24, 0.55);

/// 亮度滑杆步长（lux）：2000 lux 的量程下每格 10，约合 200 档
const ILLUMINANCE_STEP: f32 = 10.0;

/// 亮度滑杆根节点
#[derive(Component, Clone, Copy, Default)]
struct IlluminanceSlider;

/// 滑杆右侧的实时数值
#[derive(Component, Clone, Copy, Default)]
struct IlluminanceValueLabel;

/// 画面设置插件
pub struct LightingSettingsPlugin;

impl Plugin for LightingSettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LightingSettings>()
            .add_systems(
                OnEnter(GameState::Game),
                register_menu_page.before(MenuSet::Setup),
            )
            .add_systems(
                OnEnter(GameState::Game),
                fill_menu_page.after(MenuSet::Setup),
            )
            .add_systems(
                Update,
                (sync_slider, sync_value_label).run_if(in_state(GameState::Game)),
            );
    }
}

/// 把「画面设置」注册成菜单里的一级入口
fn register_menu_page(mut config: ResMut<MenuConfig>) {
    if config
        .pages
        .iter()
        .any(|page| page.id == LIGHTING_SETTINGS_PAGE)
    {
        return;
    }
    config.pages.push(MenuPageDef::section(
        LIGHTING_SETTINGS_PAGE,
        "画面设置",
        "调整全局光照亮度",
    ));
}

/// 把亮度卡片挂进「画面设置」页面的内容区
fn fill_menu_page(
    mut commands: Commands,
    settings: Res<LightingSettings>,
    bodies: Query<(Entity, &MenuPageBody)>,
) {
    let Some((body, _)) = bodies
        .iter()
        .find(|(_, body)| body.page == Some(LIGHTING_SETTINGS_PAGE))
    else {
        warn!("菜单里没有找到画面设置页面，跳过内容填充");
        return;
    };

    commands.spawn_scene(page_scene(body, &settings));
}

/// 整页结构：根 → 全局光照亮度卡片
fn page_scene(body: Entity, settings: &LightingSettings) -> impl Scene + use<> {
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
        }
        ChildOf(body)
        Children [
            @{illuminance_card(settings)}
        ]
    }
}

/// 亮度卡片：标题行（名称 + 当前值）+ 滑杆 + 说明
fn illuminance_card(settings: &LightingSettings) -> impl Scene + use<> {
    let value = settings.global_illuminance;
    let value_text = illuminance_text(value);

    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            row_gap: Val::Px(8.0),
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
                @{widgets::text("全局光照亮度", 12.0, widgets::LABEL_COLOR)}
                --
                Text::new(value_text)
                TextFont { font_size: FontSize::Px(12.0) }
                TextColor(widgets::TEXT_COLOR)
                IlluminanceValueLabel
            ]
            --
            @{widgets::slider(
                value,
                GLOBAL_ILLUMINANCE_RANGE.0,
                GLOBAL_ILLUMINANCE_RANGE.1,
                ILLUMINANCE_STEP,
            )}
            IlluminanceSlider
            on(slider_self_update)
            // 拖动写回资源：区间在 sanitized 里再夹一次，滑杆量程只是第一道闸
            on(move |change: On<ValueChange<f32>>, mut settings: ResMut<LightingSettings>| {
                let mut next = *settings;
                next.global_illuminance = change.value;
                *settings = next.sanitized();
            })
            --
            @{widgets::text(
                "主平行光的 illuminance（lux），默认 300。0 等于关掉全局光照、只留点光源；往上调会显著变亮，超过 1000 后地面高光容易过曝。",
                11.0,
                widgets::LABEL_COLOR
            )}
        ]
    }
}

/// 亮度显示文本
fn illuminance_text(illuminance: f32) -> String {
    format!("{illuminance:.0} lux")
}

/// 资源被别处改过（读盘 / 别的入口）时把滑杆拨回去，避免界面与资源不一致
fn sync_slider(
    settings: Res<LightingSettings>,
    sliders: Query<(Entity, &IlluminanceSlider, &SliderValue), With<Slider>>,
    mut commands: Commands,
) {
    if !settings.is_changed() {
        return;
    }
    for (entity, _, current) in &sliders {
        let desired = settings.global_illuminance;
        if (current.0 - desired).abs() > 0.001 {
            commands.entity(entity).insert(SliderValue(desired));
        }
    }
}

/// 数值文本跟随资源
fn sync_value_label(
    settings: Res<LightingSettings>,
    mut labels: Query<&mut Text, With<IlluminanceValueLabel>>,
) {
    if !settings.is_changed() {
        return;
    }
    let value = illuminance_text(settings.global_illuminance);
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
    use bevy::ui_widgets::SliderRange;

    fn build_test_page() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            ScenePlugin::default(),
        ));
        app.insert_resource(LightingSettings::default());

        let body = app
            .world_mut()
            .spawn(MenuPageBody {
                page: Some(LIGHTING_SETTINGS_PAGE),
            })
            .id();

        app.world_mut()
            .run_system_once(
                move |mut commands: Commands, settings: Res<LightingSettings>| {
                    commands.spawn_scene(page_scene(body, &settings));
                },
            )
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

    /// 页面只注册一次：重进游戏不该堆出第二个入口
    #[test]
    fn registers_the_page_once() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<MenuConfig>();
        app.world_mut()
            .run_system_once(register_menu_page)
            .expect("注册系统跑不起来");
        app.world_mut()
            .run_system_once(register_menu_page)
            .expect("注册系统跑不起来");

        let config = app.world().resource::<MenuConfig>();
        assert_eq!(
            config
                .pages
                .iter()
                .filter(|page| page.id == LIGHTING_SETTINGS_PAGE)
                .count(),
            1
        );
    }

    /// 结构：根 → 亮度卡 → 标题行 / 滑杆 / 说明；滑杆量程就是全局区间
    #[test]
    fn page_is_a_single_illuminance_card() {
        let (app, body) = build_test_page();
        let world = app.world();

        let roots = children_of(world, body);
        assert_eq!(roots.len(), 1, "页面根应该只有一个");
        let card = children_of(world, roots[0]);
        assert_eq!(card.len(), 1, "页面只有一张亮度卡");

        let parts = children_of(world, card[0]);
        assert_eq!(parts.len(), 3, "亮度卡 = 标题行 + 滑杆 + 说明");
        assert_eq!(
            children_of(world, parts[0]).len(),
            2,
            "标题行 = 名称 + 数值"
        );

        let slider = parts[1];
        assert!(world.get::<Slider>(slider).is_some());
        assert!(world.get::<IlluminanceSlider>(slider).is_some());
        assert_eq!(
            world
                .get::<SliderRange>(slider)
                .expect("滑杆要带量程")
                .start(),
            GLOBAL_ILLUMINANCE_RANGE.0
        );
        assert_eq!(
            world.get::<SliderRange>(slider).unwrap().end(),
            GLOBAL_ILLUMINANCE_RANGE.1
        );
    }
}
