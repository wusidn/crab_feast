//! 网格视图：坐标轴、采样点、当前参数标记。

use bevy::{picking::pointer::PointerButton, prelude::*};

use super::{
    label, px_to_param, snap_point, to_px, ForceRebuild, GridLayer, ParamMarker, SampleDot,
    SampleDrag, SampleLayer, SpaceEditor, DOT_COLORS, DOT_RADIUS, GRID_AXIS, GRID_BACKGROUND,
    GRID_H, GRID_LINE, GRID_W, FRAME_BORDER, MARKER_COLOR, MUTED_TEXT,
};

/// 网格外框的内边距：采样点贴在 ±1 边界时会伸出网格，这里留出余量
const FRAME_PAD: f32 = 14.0;
/// 手势累计位移超过这个像素数才算拖动，避免拖动结束的 Click 误删采样点
const DRAG_DELETE_GUARD: f32 = 4.0;

pub(super) fn build_grid(
    commands: &mut Commands,
    area: Entity,
    font: &Handle<Font>,
    editor: &SpaceEditor,
) {
    // 外框：给采样点留出溢出的余量，网格本体不再被裁掉
    let frame = commands
        .spawn((
            Node {
                padding: UiRect::all(Val::Px(FRAME_PAD)),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.03, 0.04, 0.07, 0.55)),
            BorderColor::all(FRAME_BORDER),
            ChildOf(area),
        ))
        .id();

    let grid = commands
        .spawn((
            Node {
                width: Val::Px(GRID_W),
                height: Val::Px(GRID_H),
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(GRID_BACKGROUND),
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.16)),
            GridLayer,
            ChildOf(frame),
        ))
        .id();

    // 网格线：按范围的 1/4、1/2、3/4 画，中线加粗（与轴的实际取值区间对齐）
    for frac in [0.25_f32, 0.5, 0.75] {
        let is_mid = (frac - 0.5).abs() < f32::EPSILON;
        let color = if is_mid { GRID_AXIS } else { GRID_LINE };
        let px_x = frac * GRID_W;
        let px_y = (1.0 - frac) * GRID_H;
        // 竖线
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(px_x),
                top: Val::Px(0.0),
                width: Val::Px(if is_mid { 2.0 } else { 1.0 }),
                height: Val::Percent(100.0),
                ..default()
            },
            BackgroundColor(color),
            ChildOf(grid),
        ));
        // 横线
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(px_y),
                width: Val::Percent(100.0),
                height: Val::Px(if is_mid { 2.0 } else { 1.0 }),
                ..default()
            },
            BackgroundColor(color),
            ChildOf(grid),
        ));
    }

    // 轴名
    let x_label = label(
        commands,
        grid,
        font,
        editor.doc.axis(0).to_string(),
        11.0,
        MUTED_TEXT,
    );
    commands.entity(x_label).insert(Node {
        position_type: PositionType::Absolute,
        left: Val::Px(GRID_W * 0.5 - 16.0),
        top: Val::Px(GRID_H + 6.0),
        ..default()
    });

    let y_label = label(
        commands,
        grid,
        font,
        editor.doc.axis(1).to_string(),
        11.0,
        MUTED_TEXT,
    );
    commands.entity(y_label).insert(Node {
        position_type: PositionType::Absolute,
        left: Val::Px(0.0),
        top: Val::Px(-18.0),
        ..default()
    });

    // 采样点容器
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        SampleLayer,
        ChildOf(grid),
    ));

    // 当前参数标记（可拖动：直接在网格上改当前值）
    let marker = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Px(18.0),
                height: Val::Px(18.0),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::MAX,
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
            BorderColor::all(MARKER_COLOR),
            ParamMarker,
            ChildOf(grid),
        ))
        .id();

    let axis0 = editor.doc.axis(0).to_string();
    let axis1 = editor.doc.axis(1).to_string();
    let a0 = axis0.clone();
    let a1 = axis1.clone();
    commands
        .entity(marker)
        .observe(
            move |mut start: On<PointerDragStart>, mut editor: ResMut<SpaceEditor>| {
                if start.button != PointerButton::Primary {
                    return;
                }
                start.propagate(false);
                editor.gesture_travel = 0.0;
                editor.marker_drag = Some([
                    editor.params.get(&a0).copied().unwrap_or(0.0),
                    editor.params.get(&a1).copied().unwrap_or(0.0),
                ]);
            },
        )
        .observe(
            move |mut drag: On<PointerDrag>, mut editor: ResMut<SpaceEditor>| {
                drag.propagate(false);
                let Some(start) = editor.marker_drag else {
                    return;
                };
                editor.gesture_travel += drag.distance.length();
                let ranges = editor.doc.ranges;
                let delta = px_to_param(drag.distance, ranges);
                editor.params.insert(
                    axis0.clone(),
                    (start[0] + delta[0]).clamp(ranges[0][0], ranges[0][1]),
                );
                editor.params.insert(
                    axis1.clone(),
                    (start[1] + delta[1]).clamp(ranges[1][0], ranges[1][1]),
                );
            },
        )
        .observe(
            move |mut end: On<PointerDragEnd>, mut editor: ResMut<SpaceEditor>| {
                end.propagate(false);
                editor.marker_drag = None;
            },
        );
}

pub(super) fn rebuild_grid(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
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

    for child in children.get(layer).map(|list| list.to_vec()).unwrap_or_default() {
        commands.entity(child).despawn();
    }

    let font: Handle<Font> = asset_server.load("fonts/PingFang-SC-Regular.ttf");
    let ranges = editor.doc.ranges;
    for (index, sample) in editor.doc.samples.iter().enumerate() {
        spawn_dot(
            &mut commands,
            layer,
            &font,
            index,
            sample.at,
            &sample.clip,
            ranges,
        );
    }
}

fn spawn_dot(
    commands: &mut Commands,
    layer: Entity,
    font: &Handle<Font>,
    index: usize,
    at: [f32; 2],
    clip: &str,
    ranges: [[f32; 2]; 2],
) {
    let px = to_px(at, ranges);
    let color = DOT_COLORS[index % DOT_COLORS.len()];

    let dot = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(px.x - DOT_RADIUS),
                top: Val::Px(px.y - DOT_RADIUS),
                width: Val::Px(DOT_RADIUS * 2.0),
                height: Val::Px(DOT_RADIUS * 2.0),
                border: UiRect::all(Val::Px(1.5)),
                border_radius: BorderRadius::MAX,
                ..default()
            },
            BackgroundColor(color),
            BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.55)),
            SampleDot { index },
            ChildOf(layer),
        ))
        .id();

    let name = label(commands, layer, font, clip.to_string(), 10.0, MUTED_TEXT);
    let label_left = (px.x + DOT_RADIUS + 5.0).min(GRID_W - 78.0);
    let label_top = if px.y < 24.0 {
        px.y + DOT_RADIUS + 2.0
    } else {
        px.y - 7.0
    };
    commands.entity(name).insert((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(label_left.max(2.0)),
            top: Val::Px(label_top),
            ..default()
        },
        Pickable::IGNORE,
    ));

    commands.entity(dot).observe(
        move |press: On<PointerPress>, mut editor: ResMut<SpaceEditor>| {
            if press.button != PointerButton::Primary {
                return;
            }
            // 每次手势开始都重置，避免上一次拖动的累计污染下一次点击
            editor.gesture_travel = 0.0;
        },
    );

    commands.entity(dot).observe(
        move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
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
        },
    );

    commands.entity(dot).observe(
        move |mut start: On<PointerDragStart>, mut editor: ResMut<SpaceEditor>| {
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
            editor.drag = Some(SampleDrag { index, start_at: at });
        },
    );

    commands.entity(dot).observe(
        move |mut drag: On<PointerDrag>,
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
            let ranges = editor.doc.ranges;
            let delta = px_to_param(drag.distance, ranges);
            // 按住 Shift 可以临时脱离整点吸附，方便摆细位置
            let free = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
            let at = snap_point([start[0] + delta[0], start[1] + delta[1]], free, ranges);
            if let Some(sample) = editor.doc.samples.get_mut(index) {
                sample.at = at;
            }
        },
    );

    commands.entity(dot).observe(
        move |mut end: On<PointerDragEnd>, mut editor: ResMut<SpaceEditor>| {
            end.propagate(false);
            if editor.drag.take().is_some() {
                // 拖动结束后刷新采样点位置文本与网格
                editor.revision = editor.revision.wrapping_add(1);
            }
        },
    );
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
        let px = to_px(sample.at, editor.doc.ranges);
        node.left = Val::Px(px.x - DOT_RADIUS);
        node.top = Val::Px(px.y - DOT_RADIUS);
    }
}

pub(super) fn update_sample_selection(
    editor: Res<SpaceEditor>,
    mut dots: Query<(&SampleDot, &mut BorderColor, &mut BackgroundColor, &mut Node)>,
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
        let radius = if selected { DOT_RADIUS + 2.0 } else { DOT_RADIUS };
        node.width = Val::Px(radius * 2.0);
        node.height = Val::Px(radius * 2.0);
    }
}

pub(super) fn update_marker(
    editor: Res<SpaceEditor>,
    mut markers: Query<&mut Node, With<ParamMarker>>,
) {
    let point = [
        editor.params.get(editor.doc.axis(0)).copied().unwrap_or(0.0),
        editor.params.get(editor.doc.axis(1)).copied().unwrap_or(0.0),
    ];
    let px = to_px(point, editor.doc.ranges);
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
