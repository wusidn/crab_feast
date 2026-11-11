//! 网格视图：坐标轴、采样点、当前参数标记。
//!
//! 外壳（外框 / 网格 / 网格线 / 轴名 / 采样点容器 / 参数标记）由 [`grid_scene`] 一次搭成；
//! 采样点会随文档改动整层重建，每个点由 [`dot_scene`] 生成（点 + 名字标签两个兄弟实体）。

use bevy::{picking::pointer::PointerButton, prelude::*, scene::SceneList};

use super::{
    DOT_COLORS, DOT_RADIUS, FRAME_BORDER, ForceRebuild, GRID_AXIS, GRID_BACKGROUND, GRID_H,
    GRID_LINE, GRID_W, GridLayer, MARKER_COLOR, MUTED_TEXT, ParamMarker, SampleDot, SampleDrag,
    SampleLayer, SpaceEditor, px_to_param, snap_point, to_px, widgets,
};

/// 网格外框的内边距：采样点贴在 ±1 边界时会伸出网格，这里留出余量
const FRAME_PAD: f32 = 14.0;
/// 手势累计位移超过这个像素数才算拖动，避免拖动结束的 Click 误删采样点
const DRAG_DELETE_GUARD: f32 = 4.0;

/// 网格外壳：外框 → 网格 →（网格线 / 轴名 / 采样点容器 / 参数标记）
///
/// 字体族由页面根节点传播下来，这里只写字号。
pub(super) fn grid_scene(editor: &SpaceEditor) -> impl Scene + use<> {
    let axis0 = editor.doc.axis(0).to_string();
    let axis1 = editor.doc.axis(1).to_string();
    let lines = grid_lines();
    let point = [
        editor.params.get(&axis0).copied().unwrap_or(0.0),
        editor.params.get(&axis1).copied().unwrap_or(0.0),
    ];
    let marker = to_px(point, editor.doc.axis_ranges());
    let marker_left = Val::Px(marker.x - 9.0);
    let marker_top = Val::Px(marker.y - 9.0);
    let x_label_left = Val::Px(GRID_W * 0.5 - 16.0);
    let x_label_top = Val::Px(GRID_H + 6.0);
    let y_label_top = Val::Px(-18.0);
    let a0 = axis0.clone();
    let a1 = axis1.clone();
    let a0_drag = a0.clone();
    let a1_drag = a1.clone();

    bsn! {
        Node {
            padding: UiRect::all(Val::Px(FRAME_PAD)),
            border: UiRect::all(Val::Px(2.0)),
            border_radius: BorderRadius::all(Val::Px(8.0)),
        }
        BackgroundColor(Color::srgba(0.03, 0.04, 0.07, 0.55))
        BorderColor::all(FRAME_BORDER)
        Children [
            Node {
                width: Val::Px(GRID_W),
                height: Val::Px(GRID_H),
                border: UiRect::all(Val::Px(1.0)),
            }
            BackgroundColor(GRID_BACKGROUND)
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.16))
            GridLayer
            Children [
                // 网格线：按范围的 1/4、1/2、3/4 画，中线加粗（与轴的实际取值区间对齐）
                {lines}
                --
                @{widgets::text(axis0, 11.0, MUTED_TEXT)}
                Node {
                    position_type: PositionType::Absolute,
                    left: x_label_left,
                    top: x_label_top,
                }
                Pickable::IGNORE
                --
                @{widgets::text(axis1, 11.0, MUTED_TEXT)}
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: y_label_top,
                }
                Pickable::IGNORE
                --
                // 采样点容器：里面的点会整层重建，容器本身保持稳定
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                }
                SampleLayer
                --
                // 当前参数标记（可拖动：直接在网格上改当前值）
                Node {
                    position_type: PositionType::Absolute,
                    left: marker_left,
                    top: marker_top,
                    width: Val::Px(18.0),
                    height: Val::Px(18.0),
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::MAX,
                }
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.0))
                BorderColor::all(MARKER_COLOR)
                ParamMarker
                on(move |mut start: On<PointerDragStart>, mut editor: ResMut<SpaceEditor>| {
                    if start.button != PointerButton::Primary {
                        return;
                    }
                    start.propagate(false);
                    editor.gesture_travel = 0.0;
                    editor.marker_drag = Some([
                        editor.params.get(&a0).copied().unwrap_or(0.0),
                        editor.params.get(&a1).copied().unwrap_or(0.0),
                    ]);
                })
                on(move |mut drag: On<PointerDrag>, mut editor: ResMut<SpaceEditor>| {
                    drag.propagate(false);
                    let Some(start) = editor.marker_drag else {
                        return;
                    };
                    editor.gesture_travel += drag.distance.length();
                    let ranges = editor.doc.axis_ranges();
                    let delta = px_to_param(drag.distance, ranges);
                    editor.params.insert(
                        a0_drag.clone(),
                        (start[0] + delta[0]).clamp(ranges[0][0], ranges[0][1]),
                    );
                    editor.params.insert(
                        a1_drag.clone(),
                        (start[1] + delta[1]).clamp(ranges[1][0], ranges[1][1]),
                    );
                })
                on(move |mut end: On<PointerDragEnd>, mut editor: ResMut<SpaceEditor>| {
                    end.propagate(false);
                    editor.marker_drag = None;
                })
            ]
        ]
    }
}

/// 网格线：竖线与横线用同一个闭包生成，保证列表元素类型一致
fn grid_lines() -> Vec<impl Scene + use<>> {
    let line = |left: f32, top: f32, width: f32, height: f32, color: Color| {
        bsn! {
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(left),
                top: Val::Px(top),
                width: Val::Px(width),
                height: Val::Px(height),
            }
            BackgroundColor(color)
        }
    };

    let mut out = Vec::new();
    for frac in [0.25_f32, 0.5, 0.75] {
        let is_mid = (frac - 0.5).abs() < f32::EPSILON;
        let color = if is_mid { GRID_AXIS } else { GRID_LINE };
        let thickness = if is_mid { 2.0 } else { 1.0 };
        out.push(line(frac * GRID_W, 0.0, thickness, GRID_H, color));
        out.push(line(0.0, (1.0 - frac) * GRID_H, GRID_W, thickness, color));
    }
    out
}

pub(super) fn rebuild_grid(
    mut commands: Commands,
    editor: Res<SpaceEditor>,
    mut force: ResMut<ForceRebuild>,
    layers: Query<Entity, With<SampleLayer>>,
    children: Query<&Children>,
    mut last: Local<u64>,
) {
    let Some(layer) = layers.iter().next() else {
        return;
    };
    if !force.0 && editor.revision == *last {
        return;
    }
    force.0 = false;
    *last = editor.revision;

    for child in children
        .get(layer)
        .map(|list| list.to_vec())
        .unwrap_or_default()
    {
        commands.entity(child).despawn();
    }

    let ranges = editor.doc.axis_ranges();
    for (index, sample) in editor.doc.samples.iter().enumerate() {
        commands.spawn_scene_list(dot_scene(index, sample.at, &sample.clip, ranges, layer));
    }
}

/// 一个采样点：圆点 + 名字标签（两个兄弟实体，都挂在采样点容器下）
fn dot_scene(
    index: usize,
    at: [f32; 2],
    clip: &str,
    ranges: [[f32; 2]; 2],
    layer: Entity,
) -> impl SceneList + use<> {
    let px = to_px(at, ranges);
    let color = DOT_COLORS[index % DOT_COLORS.len()];
    let name = clip.to_string();
    let label_left = Val::Px((px.x + DOT_RADIUS + 5.0).min(GRID_W - 78.0).max(2.0));
    let label_top = Val::Px(if px.y < 24.0 {
        px.y + DOT_RADIUS + 2.0
    } else {
        px.y - 7.0
    });
    let dot_left = Val::Px(px.x - DOT_RADIUS);
    let dot_top = Val::Px(px.y - DOT_RADIUS);
    let dot_size = Val::Px(DOT_RADIUS * 2.0);

    bsn_list! {
        Node {
            position_type: PositionType::Absolute,
            left: dot_left,
            top: dot_top,
            width: dot_size,
            height: dot_size,
            border: UiRect::all(Val::Px(1.5)),
            border_radius: BorderRadius::MAX,
        }
        BackgroundColor(color)
        BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.55))
        SampleDot { index: index }
        ChildOf(layer)
        on(|press: On<PointerPress>, mut editor: ResMut<SpaceEditor>| {
            if press.button != PointerButton::Primary {
                return;
            }
            // 每次手势开始都重置，避免上一次拖动的累计污染下一次点击
            editor.gesture_travel = 0.0;
        })
        on(move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
            if click.button != PointerButton::Primary {
                return;
            }
            click.propagate(false);
            // 拖动结束后 Bevy 仍会派发一次 Click；本手势累计移动过就当作拖动而非删除
            let travel = editor.gesture_travel;
            editor.gesture_travel = 0.0;
            if travel > DRAG_DELETE_GUARD {
                return;
            }
            if editor.selected == Some(index) {
                // 再次点击已选中的采样点：删除它
                if editor.doc.remove_sample(index) {
                    editor.selected = None;
                    editor.status = format!("已删除采样点 #{index}");
                    editor.revision = editor.revision.wrapping_add(1);
                }
            } else {
                editor.selected = Some(index);
            }
        })
        on(move |mut start: On<PointerDragStart>, mut editor: ResMut<SpaceEditor>| {
            if start.button != PointerButton::Primary {
                return;
            }
            start.propagate(false);
            editor.gesture_travel = 0.0;
            let at = editor
                .doc
                .samples
                .get(index)
                .map(|sample| sample.at)
                .unwrap_or([0.0, 0.0]);
            editor.selected = Some(index);
            editor.drag = Some(SampleDrag {
                index,
                start_at: at,
            });
        })
        on(move |mut drag: On<PointerDrag>,
              keys: Res<ButtonInput<KeyCode>>,
              mut editor: ResMut<SpaceEditor>| {
            drag.propagate(false);
            // 累计整个手势的位移：慢速拖动单帧位移很小，用单帧阈值会漏判成点击
            editor.gesture_travel += drag.distance.length();
            let Some(state) = editor.drag.as_ref() else {
                return;
            };
            if state.index != index {
                return;
            }
            let start = state.start_at;
            let ranges = editor.doc.axis_ranges();
            let delta = px_to_param(drag.distance, ranges);
            // 按住 Shift 可以临时脱离整点吸附，方便摆细位置
            let free = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
            let at = snap_point([start[0] + delta[0], start[1] + delta[1]], free, ranges);
            if let Some(sample) = editor.doc.samples.get_mut(index) {
                sample.at = at;
            }
        })
        on(move |mut end: On<PointerDragEnd>, mut editor: ResMut<SpaceEditor>| {
            end.propagate(false);
            if editor.drag.take().is_some() {
                // 拖动结束后刷新采样点位置文本与网格
                editor.revision = editor.revision.wrapping_add(1);
            }
        })
        --
        @{widgets::text(name, 10.0, MUTED_TEXT)}
        Node {
            position_type: PositionType::Absolute,
            left: label_left,
            top: label_top,
        }
        Pickable::IGNORE
        ChildOf(layer)
    }
}

pub(super) fn apply_sample_positions(
    editor: Res<SpaceEditor>,
    mut dots: Query<(&SampleDot, &mut Node)>,
) {
    if !editor.is_changed() {
        return;
    }
    for (dot, mut node) in &mut dots {
        let Some(sample) = editor.doc.samples.get(dot.index) else {
            continue;
        };
        let px = to_px(sample.at, editor.doc.axis_ranges());
        node.left = Val::Px(px.x - DOT_RADIUS);
        node.top = Val::Px(px.y - DOT_RADIUS);
    }
}

pub(super) fn update_sample_selection(
    editor: Res<SpaceEditor>,
    mut dots: Query<(
        &SampleDot,
        &mut BorderColor,
        &mut BackgroundColor,
        &mut Node,
    )>,
) {
    if !editor.is_changed() {
        return;
    }
    for (dot, mut border, mut background, mut node) in &mut dots {
        let selected = editor.selected == Some(dot.index);
        *border = BorderColor::all(if selected {
            Color::srgb(1.0, 1.0, 1.0)
        } else {
            Color::srgba(1.0, 1.0, 1.0, 0.4)
        });
        background.0 = DOT_COLORS[dot.index % DOT_COLORS.len()];
        let radius = if selected {
            DOT_RADIUS + 2.0
        } else {
            DOT_RADIUS
        };
        node.width = Val::Px(radius * 2.0);
        node.height = Val::Px(radius * 2.0);
    }
}

pub(super) fn update_marker(
    editor: Res<SpaceEditor>,
    mut markers: Query<&mut Node, With<ParamMarker>>,
) {
    let point = [
        editor
            .params
            .get(editor.doc.axis(0))
            .copied()
            .unwrap_or(0.0),
        editor
            .params
            .get(editor.doc.axis(1))
            .copied()
            .unwrap_or(0.0),
    ];
    let px = to_px(point, editor.doc.axis_ranges());
    for mut node in &mut markers {
        // 只在真的移动时写回：无条件写 Node 会让整棵 UI 每帧重新布局
        let left = Val::Px(px.x - 9.0);
        let top = Val::Px(px.y - 9.0);
        if node.left != left || node.top != top {
            node.left = left;
            node.top = top;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        asset::AssetPlugin, ecs::system::RunSystemOnce, picking::Pickable, scene::ScenePlugin,
    };

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            ScenePlugin::default(),
        ));
        app.init_resource::<SpaceEditor>();
        app
    }

    fn children_of(world: &World, entity: Entity) -> Vec<Entity> {
        world
            .get::<Children>(entity)
            .map(|children| children.iter().collect())
            .unwrap_or_default()
    }

    fn parent_of(world: &World, entity: Entity) -> Option<Entity> {
        world.get::<ChildOf>(entity).map(|parent| parent.parent())
    }

    /// 网格外壳必须是「外框 → 网格 → 线/轴名/采样点容器/标记」的嵌套，容器与标记各自独立
    #[test]
    fn grid_shell_nests_layers_under_frame() {
        let mut app = test_app();
        let area = app.world_mut().spawn_empty().id();
        app.world_mut()
            .run_system_once(move |mut commands: Commands, editor: Res<SpaceEditor>| {
                commands.spawn_scene(bsn! {
                    @{grid_scene(&editor)}
                    ChildOf(area)
                });
            })
            .expect("网格构建系统跑不起来");
        app.update();

        let world = app.world();
        let frames = children_of(world, area);
        assert_eq!(frames.len(), 1, "外框唯一");
        let grid = children_of(world, frames[0])[0];
        assert!(world.get::<GridLayer>(grid).is_some(), "外框里是网格本体");

        // 3×2 条网格线 + 两个轴名 + 采样点容器 + 参数标记
        let kids = children_of(world, grid);
        assert_eq!(kids.len(), 10, "6 条线 + 2 个轴名 + 容器 + 标记");

        let layer = kids
            .iter()
            .copied()
            .find(|entity| world.get::<SampleLayer>(*entity).is_some())
            .expect("采样点容器");
        let marker = kids
            .iter()
            .copied()
            .find(|entity| world.get::<ParamMarker>(*entity).is_some())
            .expect("参数标记");

        assert_eq!(parent_of(world, layer), Some(grid), "容器直接挂在网格下");
        assert_eq!(parent_of(world, marker), Some(grid), "标记直接挂在网格下");
        assert!(
            children_of(world, layer).is_empty(),
            "容器里只有会被重建的采样点，外壳不自带点"
        );
        assert_eq!(
            world.get::<Observer>(marker).is_some(),
            false,
            "观察者不在标记实体本身上"
        );
    }

    /// 一个采样点是「圆点 + 标签」两个兄弟实体，都挂在容器下，标签不参与点选
    #[test]
    fn dot_is_two_siblings_under_layer() {
        let mut app = test_app();
        let layer = app.world_mut().spawn_empty().id();
        app.world_mut()
            .run_system_once(move |mut commands: Commands| {
                commands.spawn_scene_list(dot_scene(
                    0,
                    [0.5, 0.5],
                    "walk",
                    [[-1.0, 1.0]; 2],
                    layer,
                ));
            })
            .expect("采样点构建系统跑不起来");
        app.update();

        let world = app.world();
        let kids = children_of(world, layer);
        assert_eq!(kids.len(), 2, "圆点 + 标签");

        let dot = kids
            .iter()
            .copied()
            .find(|entity| world.get::<SampleDot>(*entity).is_some())
            .expect("圆点带编号");
        let label = kids
            .iter()
            .copied()
            .find(|entity| world.get::<Pickable>(*entity) == Some(&Pickable::IGNORE))
            .expect("标签不参与点选");

        assert_eq!(world.get::<SampleDot>(dot).map(|dot| dot.index), Some(0));
        assert!(world.get::<Text>(label).is_some(), "标签是文本，显示剪辑名");
        assert_eq!(parent_of(world, dot), Some(layer));
        assert_eq!(parent_of(world, label), Some(layer));
        assert!(
            world.get::<SampleDot>(label).is_none(),
            "标签不是可点选的点"
        );
    }
}
