//! 字体族的层级继承：页面根节点定字体，具体字号各自写。
//!
//! Bevy 的 `TextFont` 是一个整体组件（字体族 + 字号 + 字重 + 变体…），而 `Text` /
//! `TextSpan` 都 `#[require(TextFont)]`，父节点的字体**不会**传给子节点：每个文本实体
//! 拿到的都是 `TextFont::default()`。
//!
//! 这里用 `bevy_app` 的层级传播机制（`HierarchyPropagatePlugin`）把只含字体族的
//! [`FontFamily`] 沿 `ChildOf` 往下传，再由 [`apply_inherited_font_family`] 把字体族
//! 写进每个文本实体自己的 `TextFont`。
//!
//! 为什么不直接传播 `TextFont`：它是整体拷贝，字号会被一起盖回根节点的值，
//! 「特殊字号单独写」就永远不生效。拆成两路后，字体族统一、字号各管各的。

use bevy::app::{HierarchyPropagatePlugin, Inherited, Propagate, PropagateSet};
use bevy::prelude::*;
use bevy::text::FontSource;
use bevy::ui::UiSystems;

/// 字体族（不含字号），挂在面板 / 页面根节点上，沿 `ChildOf` 向下传播。
///
/// 想给某个子树换字体，在子树根上再插一个 [`font_scope`] 即可，它自己会成为新的传播源头。
#[derive(Component, Clone, PartialEq, Debug)]
pub struct FontFamily(pub FontSource);

impl FontFamily {
    /// 从一个字体句柄构造。
    pub fn new(font: &Handle<Font>) -> Self {
        Self(FontSource::from(font.clone()))
    }
}

/// 声明一个字体作用域：**本实体及其所有后代**的文本都用这个字体族。
///
/// ```ignore
/// commands.entity(header).insert(font_scope(&font_handle));
/// ```
///
/// 作用域内的文本只需要写自己的字号：
/// ```ignore
/// TextFont { font_size: FontSize::Px(12.0) }
/// ```
pub fn font_scope(font: &Handle<Font>) -> Propagate<FontFamily> {
    Propagate(FontFamily::new(font))
}

/// 把继承来的字体族写进每个文本实体自己的 `TextFont`，字号保持实体自身的值。
fn apply_inherited_font_family(mut texts: Query<(&mut TextFont, &Inherited<FontFamily>)>) {
    for (mut text_font, inherited) in &mut texts {
        // 取 DerefMut 才会打脏标记，所以先比较再写，避免每帧无意义的重排版
        if text_font.font != inherited.0.0 {
            text_font.font = inherited.0.0.clone();
        }
    }
}

/// 注册字体族传播：文本的度量系统（`UiSystems::Content`）排在它后面，
/// 保证当帧排版用的就是继承来的字体。
pub struct FontFamilyPlugin;

impl Plugin for FontFamilyPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(HierarchyPropagatePlugin::<FontFamily>::new(PostUpdate))
            .configure_sets(
                PostUpdate,
                PropagateSet::<FontFamily>::default().in_set(UiSystems::Propagate),
            )
            .add_systems(
                PostUpdate,
                apply_inherited_font_family
                    .after(PropagateSet::<FontFamily>::default())
                    .before(UiSystems::Content),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::hierarchy::ChildOf;

    fn app_with_plugin() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(FontFamilyPlugin);
        app
    }

    #[test]
    fn child_inherits_font_family_but_keeps_its_own_size() {
        let mut app = app_with_plugin();
        let font = Handle::<Font>::default();

        let root = app.world_mut().spawn(font_scope(&font)).id();
        let panel = app.world_mut().spawn(ChildOf(root)).id();
        let label = app
            .world_mut()
            .spawn((
                TextFont {
                    font_size: FontSize::Px(12.0),
                    ..default()
                },
                ChildOf(panel),
            ))
            .id();
        app.update();

        let text_font = app.world().get::<TextFont>(label).unwrap();
        assert_eq!(text_font.font, FontSource::from(font));
        // 字号没被根节点的值盖掉
        assert_eq!(text_font.font_size, FontSize::Px(12.0));
    }

    #[test]
    fn nested_scope_overrides_outer_family() {
        let mut app = app_with_plugin();
        let outer = Handle::<Font>::default();

        let root = app.world_mut().spawn(font_scope(&outer)).id();
        let inner = app.world_mut().spawn(font_scope(&outer)).id();
        app.world_mut().entity_mut(inner).insert(ChildOf(root));

        let label = app
            .world_mut()
            .spawn((TextFont::default(), ChildOf(inner)))
            .id();
        app.update();

        assert_eq!(
            app.world().get::<TextFont>(label).unwrap().font,
            FontSource::from(outer)
        );
    }
}
