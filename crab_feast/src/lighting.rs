//! 全局光照：场景主平行光的运行时亮度设置。
//!
//! 场景里的 `DirectionalLight` 带 [`GlobalLight`] 标记，[`LightingPlugin`] 只在
//! 「设置变了」或「灯刚生成」这两种时刻把 [`LightingSettings::global_illuminance`]
//! 写进它的 `illuminance`，不做每帧无意义覆盖。
//!
//! 落盘由 `settings` 模块负责：`<assets>/settings/lighting.json`。

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// 全局光照亮度的合法区间（lux）。
///
/// 上限 2000：再往上是 Bevy 默认日光（10_000 lux）的地盘，第三人称视角会直接过曝；
/// 下限 0 允许彻底关掉主光，只留点光源做暗场调试。
pub const GLOBAL_ILLUMINANCE_RANGE: (f32, f32) = (0.0, 2000.0);

/// 场景默认亮度（lux）：比 Bevy 默认日光（10_000 lux）暗一级。
pub const DEFAULT_GLOBAL_ILLUMINANCE: f32 = 300.0;

/// 全局光照设置：目前只有主平行光一项。
///
/// 落在 `<assets>/settings/lighting.json`，由 `settings` 模块读写与防抖。
#[derive(Resource, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LightingSettings {
    /// 主平行光的 `illuminance`（lux）
    pub global_illuminance: f32,
}

impl Default for LightingSettings {
    fn default() -> Self {
        Self {
            global_illuminance: DEFAULT_GLOBAL_ILLUMINANCE,
        }
    }
}

impl LightingSettings {
    /// 夹回合法区间：读盘、滑杆、落盘前都过一遍
    pub fn sanitized(mut self) -> Self {
        self.global_illuminance = self
            .global_illuminance
            .clamp(GLOBAL_ILLUMINANCE_RANGE.0, GLOBAL_ILLUMINANCE_RANGE.1);
        self
    }
}

/// 场景主平行光的标记：亮度由 [`LightingSettings`] 驱动。
///
/// 挂在 `DirectionalLight` 上，而不是按名字/句柄找灯：退出 `Game` 时整组
/// `SceneDecor` 被清掉、重进时重新生成，标记跟着实体走，不需要额外记住句柄。
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct GlobalLight;

/// 全局光照插件：把设置同步到主平行光
pub struct LightingPlugin;

impl Plugin for LightingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LightingSettings>()
            // 先补新生成的灯（重进 Game 时设置没变，也得按磁盘值上色），
            // 再处理设置改动；两个系统都写同一个组件，串行避免歧义
            .add_systems(
                Update,
                (apply_global_light_on_spawn, apply_global_light_on_change).chain(),
            );
    }
}

/// 灯刚生成时按当前设置初始化：启动读盘早于 `OnEnter(Game)`，
/// 所以「设置没变」也要在这里补一次
fn apply_global_light_on_spawn(
    settings: Res<LightingSettings>,
    mut lights: Query<&mut DirectionalLight, (With<GlobalLight>, Added<GlobalLight>)>,
) {
    for mut light in &mut lights {
        light.illuminance = settings.global_illuminance;
    }
}

/// 设置被改过（拖滑杆 / 读盘）时同步到已有的主平行光
fn apply_global_light_on_change(
    settings: Res<LightingSettings>,
    mut lights: Query<&mut DirectionalLight, With<GlobalLight>>,
) {
    if !settings.is_changed() {
        return;
    }
    let target = settings.global_illuminance;
    for mut light in &mut lights {
        if light.illuminance != target {
            light.illuminance = target;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份只有一个主平行光的最小 world
    fn light_app(illuminance: f32) -> (App, Entity) {
        let mut app = App::new();
        app.init_resource::<LightingSettings>();
        let light = app
            .world_mut()
            .spawn((
                DirectionalLight {
                    illuminance,
                    ..default()
                },
                GlobalLight,
            ))
            .id();
        (app, light)
    }

    /// 读盘/手改的越界值要夹回区间
    #[test]
    fn out_of_range_values_are_clamped() {
        assert_eq!(
            LightingSettings {
                global_illuminance: -20.0
            }
            .sanitized()
            .global_illuminance,
            GLOBAL_ILLUMINANCE_RANGE.0
        );
        assert_eq!(
            LightingSettings {
                global_illuminance: 99_999.0
            }
            .sanitized()
            .global_illuminance,
            GLOBAL_ILLUMINANCE_RANGE.1
        );
    }

    /// 灯刚生成时就用设置里的值，而不是等到下一次改设置
    #[test]
    fn spawn_applies_current_setting() {
        use bevy::ecs::system::RunSystemOnce;

        let (mut app, light) = light_app(300.0);
        app.world_mut()
            .resource_mut::<LightingSettings>()
            .global_illuminance = 750.0;
        app.world_mut()
            .run_system_once(apply_global_light_on_spawn)
            .expect("同步系统跑不起来");
        assert_eq!(
            app.world()
                .get::<DirectionalLight>(light)
                .expect("灯还在")
                .illuminance,
            750.0
        );
    }

    /// 设置改动后立刻写进灯
    #[test]
    fn change_updates_existing_light() {
        use bevy::ecs::system::RunSystemOnce;

        let (mut app, light) = light_app(300.0);
        let mut settings = *app.world().resource::<LightingSettings>();
        settings.global_illuminance = 120.0;
        *app.world_mut().resource_mut::<LightingSettings>() = settings;
        app.world_mut()
            .run_system_once(apply_global_light_on_change)
            .expect("同步系统跑不起来");
        assert_eq!(
            app.world()
                .get::<DirectionalLight>(light)
                .expect("灯还在")
                .illuminance,
            120.0
        );
    }
}
