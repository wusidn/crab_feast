//! 「跳跃设置」菜单页：起跳速度、空中操控力度、空中接管时长。
//!
//! 页面结构（字体族由菜单内容区继承，这里只写字号）：
//!
//! ```text
//! root
//! ├─ 卡片 起跳速度       5.0 m/s · 跳高 1.27 m
//! │       轨道 ────────●──────────
//! │       说明（调高会跳得更远更久、剪辑会被慢放拉伸）
//! ├─ 卡片 空中操控力度   4.0 m/s² · 约 76°/s
//! │       轨道 ────●──────────────
//! │       说明（0 = 锁死落点，越大越跟手）
//! └─ 卡片 空中接管时长   0.75 s
//!         轨道 ──────●────────────
//!         说明（起跳后这么久之后落点定死，推杆只转朝向）
//! ```
//!
//! 三项都是**联网参数**：每条上行快照都会把它们带给服务端（见
//! `crab_feast_library::net::MoveInput`），服务端按同一区间夹取后照同样的规则推人。
//! 所以拖动滑杆在对局中立即生效，不需要重连，也不会出现「本地跳得高、服务端跳得低」。
//!
//! 交互闭环：拖滑杆写回 [`JumpSettings`] → 落盘（防抖）→ 同步系统把资源状态刷回界面；
//! [`crate::anim::JumpPlugin`] 的 `sync_jump_settings` 再把起跳速度灌进档位表。

use bevy::prelude::*;
use bevy::ui_widgets::{Slider, SliderValue, ValueChange, slider_self_update};

use crate::GameState;
use crate::anim::JumpSettings;
use crate::ui::menu::{MenuConfig, MenuPageBody, MenuPageDef, MenuSet, widgets};
use crab_feast_library::air::{AIR_ACCEL_RANGE, AIR_STEER_SECS_RANGE, JUMP_LAUNCH_VY_RANGE};
use crab_feast_library::tuning::MOVE_SPEED_MPS;

/// 菜单页面 id
pub const JUMP_SETTINGS_PAGE: &str = "jump_settings";

/// 卡片底色（与其它设置页保持一致）
const CARD_BACKGROUND: Color = Color::srgba(0.16, 0.18, 0.24, 0.55);

/// 起跳速度滑杆步长（m/s）
const LAUNCH_STEP: f32 = 0.1;
/// 空中操控力度滑杆步长（m/s²）
const AIR_ACCEL_STEP: f32 = 0.1;
/// 空中接管时长滑杆步长（秒）
const AIR_STEER_STEP: f32 = 0.05;

/// 重力（只用于把起跳速度换算成「跳多高」给玩家看）
///
/// 必须与 `anim::jump::air::GRAVITY` 一致；那边是私有常量，这里只做显示换算，
/// 真要改重力时两处一起改（改错的表现只是标签上的高度对不上，不影响物理）。
const DISPLAY_GRAVITY: f32 = 9.81;

/// 起跳速度滑杆
#[derive(Component, Clone, Copy, Default)]
struct LaunchSlider;
/// 起跳速度数值
#[derive(Component, Clone, Copy, Default)]
struct LaunchLabel;

/// 空中操控力度滑杆
#[derive(Component, Clone, Copy, Default)]
struct AirAccelSlider;
/// 空中操控力度数值
#[derive(Component, Clone, Copy, Default)]
struct AirAccelLabel;

/// 空中接管时长滑杆
#[derive(Component, Clone, Copy, Default)]
struct AirSteerSlider;
/// 空中接管时长数值
#[derive(Component, Clone, Copy, Default)]
struct AirSteerLabel;

/// 跳跃设置插件
pub struct JumpSettingsPlugin;

impl Plugin for JumpSettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<JumpSettings>()
            .add_systems(
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

/// 把「跳跃设置」注册成菜单里的一级入口
fn register_menu_page(mut config: ResMut<MenuConfig>) {
    if config
        .pages
        .iter()
        .any(|page| page.id == JUMP_SETTINGS_PAGE)
    {
        return;
    }
    config.pages.push(MenuPageDef::section(
        JUMP_SETTINGS_PAGE,
        "跳跃设置",
        "调整起跳速度与空中操控",
    ));
}

/// 把三张卡片挂进「跳跃设置」页面的内容区
fn fill_menu_page(
    mut commands: Commands,
    settings: Res<JumpSettings>,
    bodies: Query<(Entity, &MenuPageBody)>,
) {
    let Some((body, _)) = bodies
        .iter()
        .find(|(_, body)| body.page == Some(JUMP_SETTINGS_PAGE))
    else {
        warn!("菜单里没有找到跳跃设置页面，跳过内容填充");
        return;
    };

    commands.spawn_scene(page_scene(body, &settings));
}

/// 整页结构：根 → 三张滑杆卡片
fn page_scene(body: Entity, settings: &JumpSettings) -> impl Scene + use<> {
    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(10.0),
        }
        ChildOf(body)
        Children [
            @{launch_card(settings)}
            --
            @{air_accel_card(settings)}
            --
            @{air_steer_card(settings)}
        ]
    }
}

/// 起跳速度卡片：标题行（名称 + 当前值）+ 滑杆 + 说明
fn launch_card(settings: &JumpSettings) -> impl Scene + use<> {
    let value = settings.launch_vy;

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
                @{widgets::text("起跳速度", 12.0, widgets::LABEL_COLOR)}
                --
                Text::new(launch_text(value))
                TextFont { font_size: FontSize::Px(12.0) }
                TextColor(widgets::TEXT_COLOR)
                LaunchLabel
            ]
            --
            @{widgets::slider(
                value,
                JUMP_LAUNCH_VY_RANGE.0,
                JUMP_LAUNCH_VY_RANGE.1,
                LAUNCH_STEP,
            )}
            LaunchSlider
            on(slider_self_update)
            // 拖动写回资源：区间在 sanitized 里再夹一次，滑杆量程只是第一道闸
            on(move |change: On<ValueChange<f32>>, mut settings: ResMut<JumpSettings>| {
                let mut next = *settings;
                next.launch_vy = change.value;
                *settings = next.sanitized();
            })
            --
            @{widgets::text(
                "起跳的竖直初速度，默认 5.0 m/s（约 1.27 m 高）。调高会跳得更高、滞空更久，跳上高台更稳；调低变得贴地，适合低矮关卡。联网时这个值随每条输入上送服务端，两端一起变。",
                11.0,
                widgets::LABEL_COLOR
            )}
        ]
    }
}

/// 空中操控力度卡片
fn air_accel_card(settings: &JumpSettings) -> impl Scene + use<> {
    let value = settings.air_accel;

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
                @{widgets::text("空中操控力度", 12.0, widgets::LABEL_COLOR)}
                --
                Text::new(air_accel_text(value))
                TextFont { font_size: FontSize::Px(12.0) }
                TextColor(widgets::TEXT_COLOR)
                AirAccelLabel
            ]
            --
            @{widgets::slider(
                value,
                AIR_ACCEL_RANGE.0,
                AIR_ACCEL_RANGE.1,
                AIR_ACCEL_STEP,
            )}
            AirAccelSlider
            on(slider_self_update)
            on(move |change: On<ValueChange<f32>>, mut settings: ResMut<JumpSettings>| {
                let mut next = *settings;
                next.air_accel = change.value;
                *settings = next.sanitized();
            })
            --
            @{widgets::text(
                "滞空时把水平速度转向输入方向的加速度，默认 4.0 m/s²（跑速下约 76°/s）。0 = 落点完全锁死，起跳角一错误就差出半个方块；调大更跟手，但太大会退化成「空中随便拐」。只转方向，速度大小一字不改。",
                11.0,
                widgets::LABEL_COLOR
            )}
        ]
    }
}

/// 空中接管时长卡片
fn air_steer_card(settings: &JumpSettings) -> impl Scene + use<> {
    let value = settings.air_steer_secs;

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
                @{widgets::text("空中接管时长", 12.0, widgets::LABEL_COLOR)}
                --
                Text::new(air_steer_text(value))
                TextFont { font_size: FontSize::Px(12.0) }
                TextColor(widgets::TEXT_COLOR)
                AirSteerLabel
            ]
            --
            @{widgets::slider(
                value,
                AIR_STEER_SECS_RANGE.0,
                AIR_STEER_SECS_RANGE.1,
                AIR_STEER_STEP,
            )}
            AirSteerSlider
            on(slider_self_update)
            on(move |change: On<ValueChange<f32>>, mut settings: ResMut<JumpSettings>| {
                let mut next = *settings;
                next.air_steer_secs = change.value;
                *settings = next.sanitized();
            })
            --
            @{widgets::text(
                "起跳后这段时间内还能改落点，过了就彻底锁死、推杆只转身体朝向。默认 0.75 s：按 5.0 m/s 起跳约等于落地前 0.2 秒定死落点，触地判定不会被最后一刻的输入抖动带偏。",
                11.0,
                widgets::LABEL_COLOR
            )}
        ]
    }
}

/// 起跳速度显示文本：速度 + 换算出的跳高（只做展示）
fn launch_text(vy: f32) -> String {
    let height = vy * vy / (2.0 * DISPLAY_GRAVITY);
    format!("{vy:.1} m/s · 跳高 {height:.2} m")
}

/// 空中操控力度显示文本：加速度 + 跑速下换算出的角速度
fn air_accel_text(accel: f32) -> String {
    if accel <= 0.0 {
        return "0.0 m/s² · 落点锁死".to_string();
    }
    let deg_per_sec = accel / MOVE_SPEED_MPS * 180.0 / std::f32::consts::PI;
    format!("{accel:.1} m/s² · 约 {deg_per_sec:.0}°/s")
}

/// 空中接管时长显示文本
fn air_steer_text(secs: f32) -> String {
    if secs <= 0.0 {
        return "0.00 s · 全程锁死".to_string();
    }
    format!("{secs:.2} s")
}

/// 资源被别处改过（读盘 / 联机写回）时把三根滑杆和数值拨回去
fn sync_page(
    settings: Res<JumpSettings>,
    sliders: Query<
        (
            Entity,
            Option<&LaunchSlider>,
            Option<&AirAccelSlider>,
            Option<&AirSteerSlider>,
            &SliderValue,
        ),
        With<Slider>,
    >,
    mut labels: Query<(
        Option<&LaunchLabel>,
        Option<&AirAccelLabel>,
        Option<&AirSteerLabel>,
        &mut Text,
    )>,
    mut commands: Commands,
) {
    if settings.is_changed() {
        let current = *settings;
        for (entity, launch, accel, steer, value) in &sliders {
            let desired = if launch.is_some() {
                current.launch_vy
            } else if accel.is_some() {
                current.air_accel
            } else if steer.is_some() {
                current.air_steer_secs
            } else {
                continue;
            };
            if (value.0 - desired).abs() > 1e-3 {
                commands.entity(entity).insert(SliderValue(desired));
            }
        }
    }
    if !settings.is_changed() {
        return;
    }
    let current = *settings;
    for (launch, accel, steer, mut text) in &mut labels {
        let next = if launch.is_some() {
            launch_text(current.launch_vy)
        } else if accel.is_some() {
            air_accel_text(current.air_accel)
        } else if steer.is_some() {
            air_steer_text(current.air_steer_secs)
        } else {
            continue;
        };
        if text.0 != next {
            text.0 = next;
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
        app.insert_resource(JumpSettings::default());

        let body = app
            .world_mut()
            .spawn(MenuPageBody {
                page: Some(JUMP_SETTINGS_PAGE),
            })
            .id();

        app.world_mut()
            .run_system_once(move |mut commands: Commands, settings: Res<JumpSettings>| {
                commands.spawn_scene(page_scene(body, &settings));
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

    /// 页面只注册一次：重进游戏不该堆出第二个入口
    #[test]
    fn registers_the_page_once() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<MenuConfig>();
        app.world_mut().run_system_once(register_menu_page).unwrap();
        app.world_mut().run_system_once(register_menu_page).unwrap();

        let config = app.world().resource::<MenuConfig>();
        assert_eq!(
            config
                .pages
                .iter()
                .filter(|page| page.id == JUMP_SETTINGS_PAGE)
                .count(),
            1
        );
    }

    /// 结构：根 → 三张卡片 → 标题行 / 滑杆 / 说明；滑杆量程就是共享库的区间
    #[test]
    fn page_is_three_slider_cards() {
        let (app, body) = build_test_page();
        let world = app.world();

        let roots = children_of(world, body);
        assert_eq!(roots.len(), 1, "页面根应该只有一个");
        let cards = children_of(world, roots[0]);
        assert_eq!(cards.len(), 3, "起跳速度 / 空中力度 / 接管时长");

        for card in &cards {
            let parts = children_of(world, *card);
            assert_eq!(parts.len(), 3, "每张卡 = 标题行 + 滑杆 + 说明");
            assert_eq!(
                children_of(world, parts[0]).len(),
                2,
                "标题行 = 名称 + 数值"
            );
            let slider = parts[1];
            assert!(world.get::<Slider>(slider).is_some());
            assert!(world.get::<SliderRange>(slider).is_some());
        }

        let sliders = children_of(world, cards[1])
            .into_iter()
            .filter(|entity| world.get::<Slider>(*entity).is_some());
        for slider in sliders {
            let range = world.get::<SliderRange>(slider).unwrap();
            assert!(
                (range.start() - AIR_ACCEL_RANGE.0).abs() < 1e-6
                    && (range.end() - AIR_ACCEL_RANGE.1).abs() < 1e-6,
                "空中力度滑杆量程应当来自共享库"
            );
        }
    }

    /// 数值文本：0 力度 / 0 时长各有专门说法，别显示成「0 m/s² · 约 0°/s」
    #[test]
    fn zero_values_have_their_own_text() {
        assert_eq!(air_accel_text(0.0), "0.0 m/s² · 落点锁死");
        assert_eq!(air_steer_text(0.0), "0.00 s · 全程锁死");
        assert!(launch_text(5.0).contains("跳高 1.27 m"));
    }
}
