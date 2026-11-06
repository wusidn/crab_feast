//! 检查器：参数滑杆、采样点列表、剪辑选择、存取。

use std::path::PathBuf;

use bevy::{
    picking::pointer::PointerButton,
    prelude::*,
    ui_widgets::{slider_self_update, ValueChange},
};

use crab_feast_library::anim::BlendSpaceDoc;

use crate::{
    ui::menu::widgets,
    ui::preview::{PreviewCatalog, PreviewState},
};

use super::{
    label, EnabledLabel, HINT_COLOR, InspectorBody, ParamValue, SpaceEditor, WeightText, MUTED_TEXT, TEXT_COLOR, WARN_COLOR,
};

/// 文档落盘位置
pub fn space_path(key: &str) -> Option<PathBuf> {
    super::registry::space_file(key)
}

pub(super) fn build_params(
    commands: &mut Commands,
    root: Entity,
    font: &Handle<Font>,
    editor: &SpaceEditor,
    cache: &super::registry::MixerCache,
) {
    if let Some(mixer) = cache.mixers.get(editor.mixer) {
        label(
            commands,
            root,
            font,
            mixer.description.clone(),
            9.0,
            MUTED_TEXT,
        );

        let toggle_row = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    ..default()
                },
                ChildOf(root),
            ))
            .id();
        label(commands, toggle_row, font, "游戏中启用", 10.0, MUTED_TEXT);
        let caption = if editor.enabled { "开".to_string() } else { "关".to_string() };
        let title = mixer.title.clone();
        let button = widgets::button(commands, toggle_row, Some(font), caption);
        commands.entity(button).insert(EnabledLabel);
        commands.entity(button).observe(
            move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                editor.enabled = !editor.enabled;
                editor.status = format!(
                    "混合系统「{}」已{}",
                    title,
                    if editor.enabled { "启用" } else { "停用" }
                );
                editor.revision = editor.revision.wrapping_add(1);
            },
        );
    }

    for axis in 0..2 {
        let name = editor.doc.axis(axis).to_string();
        let row = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(2.0),
                    ..default()
                },
                ChildOf(root),
            ))
            .id();

        let caption = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    flex_direction: FlexDirection::Row,
                    ..default()
                },
                ChildOf(row),
            ))
            .id();
        let value = editor.params.get(&name).copied().unwrap_or(0.0);
        let text_entity = label(commands, caption, font, format!("{name} = {value:.2}"), 11.0, MUTED_TEXT);
        commands.entity(text_entity).insert(ParamValue { slot: axis });

        // 滑杆范围跟文档自身对齐：yaw 是角度、speed 是归一化速度，量纲不同
        let range = editor.doc.range(axis);
        let step = ((range[1] - range[0]) / 200.0).max(0.001);
        let parts = widgets::slider(commands, row, value, range[0], range[1], step);
        let key = name.clone();
        commands
            .entity(parts.root)
            .insert(super::ParamSlider { slot: axis })
            .observe(
                move |change: On<ValueChange<f32>>, mut editor: ResMut<SpaceEditor>| {
                    editor.params.insert(key.clone(), change.value);
                },
            )
            .observe(slider_self_update);
    }

    let row = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                margin: UiRect::top(Val::Px(6.0)),
                ..default()
            },
            ChildOf(root),
        ))
        .id();
    let speed_text = label(
        commands,
        row,
        font,
        format!("播放速度 {:.2}x", editor.doc.speed),
        11.0,
        MUTED_TEXT,
    );
    commands.entity(speed_text).insert(ParamValue { slot: 2 });
    let parts = widgets::slider(commands, row, editor.doc.speed, 0.1, 3.0, 0.05);
    commands
        .entity(parts.root)
        .insert(super::ParamSlider { slot: 2 })
        .observe(
            move |change: On<ValueChange<f32>>, mut editor: ResMut<SpaceEditor>| {
                editor.doc.speed = change.value;
            },
        )
        .observe(slider_self_update);
}

pub(super) fn rebuild_inspector(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    editor: ResMut<SpaceEditor>,
    catalog: Res<PreviewCatalog>,
    preview: Res<PreviewState>,
    bodies: Query<Entity, With<InspectorBody>>,
    children: Query<&Children>,
    mut last: Local<(Option<usize>, u64)>,
) {
    let Some(body) = bodies.iter().next() else {
        return;
    };
    let key = (editor.selected, editor.revision);
    let empty = children.get(body).map(|list| list.is_empty()).unwrap_or(true);
    if *last == key && !empty {
        return;
    }
    *last = key;

    for child in children.get(body).map(|list| list.to_vec()).unwrap_or_default() {
        commands.entity(child).despawn();
    }

    let font: Handle<Font> = asset_server.load("fonts/PingFang-SC-Regular.ttf");
    let clips = catalog
        .character(preview.character)
        .map(|def| def.clips.clone())
        .unwrap_or_default();

    // 采样点列表
    for (index, sample) in editor.doc.samples.iter().enumerate() {
        let selected = editor.selected == Some(index);
        let row = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    padding: UiRect::axes(Val::Px(4.0), Val::Px(2.0)),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(if selected {
                    Color::srgba(0.28, 0.42, 0.72, 0.55)
                } else {
                    Color::srgba(0.0, 0.0, 0.0, 0.0)
                }),
                ChildOf(body),
            ))
            .id();
        let text = format!(
            "#{index}  {}{}  ({:.2}, {:.2})",
            sample.clip,
            if sample.reverse { " ↺" } else { "" },
            sample.at[0],
            sample.at[1]
        );
        let text_entity = label(&mut commands, row, &font, text, 11.0, TEXT_COLOR);
        let _ = text_entity;

        commands.entity(row).observe(
            move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                editor.selected = Some(index);
            },
        );
    }

    let add = widgets::button(&mut commands, body, Some(&font), "在当前位置添加采样点".to_string());
    commands.entity(add).observe(
        move |mut click: On<PointerClick>,
              mut editor: ResMut<SpaceEditor>,
              catalog: Res<PreviewCatalog>,
              preview: Res<PreviewState>| {
            if click.button != PointerButton::Primary {
                return;
            }
            click.propagate(false);
            let point = [
                editor.params.get(&editor.doc.params[0]).copied().unwrap_or(0.0),
                editor.params.get(&editor.doc.params[1]).copied().unwrap_or(0.0),
            ];
            let def = catalog.character(preview.character);
            let clip = def
                .as_ref()
                .and_then(|def| {
                    def.clips
                        .iter()
                        .find(|clip| !editor.doc.samples.iter().any(|sample| sample.clip.as_str() == clip.as_str()))
                        .cloned()
                })
                .or_else(|| def.as_ref().and_then(|def| def.clips.first().cloned()))
                .unwrap_or_default();
            if clip.is_empty() {
                editor.status = "没有可用剪辑".to_string();
                return;
            }
            let index = editor.doc.add_sample(point, clip.clone());
            editor.selected = Some(index);
            editor.status = format!("在 ({:.2}, {:.2}) 添加了 {}", point[0], point[1], clip);
            editor.revision = editor.revision.wrapping_add(1);
        },
    );

    // 选中采样点详情
    if let Some(index) = editor.selected {
        if let Some(sample) = editor.doc.samples.get(index).cloned() {
            label(
                &mut commands,
                body,
                &font,
                format!("采样点 #{index} 的剪辑"),
                11.0,
                MUTED_TEXT,
            );

            // 反转播放：勾选框（用于做后退）
            let check_row = commands
                .spawn((
                    Node {
                        width: Val::Percent(100.0),
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(8.0),
                        padding: UiRect::axes(Val::Px(4.0), Val::Px(3.0)),
                        ..default()
                    },
                    ChildOf(body),
                ))
                .id();
            let box_entity = commands
                .spawn((
                    Node {
                        width: Val::Px(15.0),
                        height: Val::Px(15.0),
                        flex_shrink: 0.0,
                        border: UiRect::all(Val::Px(1.0)),
                        border_radius: BorderRadius::all(Val::Px(3.0)),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    BackgroundColor(if sample.reverse {
                        widgets::BUTTON_SELECTED
                    } else {
                        widgets::BUTTON_IDLE
                    }),
                    BorderColor::all(widgets::BORDER_SOFT),
                    ChildOf(check_row),
                ))
                .id();
            if sample.reverse {
                commands.spawn((
                    Node {
                        width: Val::Px(7.0),
                        height: Val::Px(7.0),
                        border_radius: BorderRadius::all(Val::Px(1.5)),
                        ..default()
                    },
                    BackgroundColor(super::TEXT_COLOR),
                    ChildOf(box_entity),
                ));
            }
            label(
                &mut commands,
                check_row,
                &font,
                "反转播放（后退）".to_string(),
                11.0,
                TEXT_COLOR,
            );
            commands.entity(check_row).observe(
                move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
                    if click.button != PointerButton::Primary {
                        return;
                    }
                    click.propagate(false);
                    if let Some(sample) = editor.doc.samples.get_mut(index) {
                        sample.reverse = !sample.reverse;
                        let state = if sample.reverse { "反转（后退）" } else { "正放" };
                        editor.status = format!("采样点 #{index} 播放方向 → {state}");
                    }
                    editor.revision = editor.revision.wrapping_add(1);
                },
            );

            // 剪辑选择（下拉框）
            let selected_clip = clips
                .iter()
                .position(|clip| clip == &sample.clip)
                .unwrap_or(0);
            let parts = super::spawn_dropdown(
                &mut commands,
                body,
                &font,
                super::BlendDropdown::Clip,
                &clips,
                selected_clip,
            );
            for (clip_index, item) in parts.items.into_iter().enumerate() {
                let clip = clips.get(clip_index).cloned().unwrap_or_default();
                commands.entity(item).observe(
                    move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
                        if click.button != PointerButton::Primary {
                            return;
                        }
                        click.propagate(false);
                        editor.open_dropdown = None;
                        if let Some(sample) = editor.doc.samples.get_mut(index) {
                            sample.clip = clip.clone();
                        }
                        editor.revision = editor.revision.wrapping_add(1);
                        editor.status = format!("采样点 #{index} → {clip}");
                    },
                );
            }

            let remove = widgets::button(&mut commands, body, Some(&font), "删除该采样点".to_string());
            commands.entity(remove).observe(
                move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
                    if click.button != PointerButton::Primary {
                        return;
                    }
                    click.propagate(false);
                    if editor.doc.remove_sample(index) {
                        editor.selected = None;
                        editor.status = format!("已删除采样点 #{index}");
                        editor.revision = editor.revision.wrapping_add(1);
                    }
                },
            );
        }
    }

    // 当前权重
    let point = [
        editor.params.get(&editor.doc.params[0]).copied().unwrap_or(0.0),
        editor.params.get(&editor.doc.params[1]).copied().unwrap_or(0.0),
    ];
    let weights = editor.doc.weights(point);
    let hint = if weights.is_empty() {
        "无可用采样点".to_string()
    } else {
        weights
            .iter()
            .map(|(key, weight)| {
                let (clip, reverse) = crab_feast_library::anim::blendspace::split_key(key);
                format!("{clip}{} {:.0}%", if reverse { " ↺" } else { "" }, weight * 100.0)
            })
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let weight_text = label(
        &mut commands,
        body,
        &font,
        format!("当前权重：{hint}"),
        11.0,
        HINT_COLOR,
    );
    commands.entity(weight_text).insert(WeightText);

    for issue in editor.doc.validate() {
        label(&mut commands, body, &font, format!("! {issue}"), 11.0, WARN_COLOR);
    }

}

pub(super) fn save_space(editor: &mut SpaceEditor) {
    let key = editor.loaded_key.clone().unwrap_or_else(|| "locomotion".to_string());
    let Some(path) = space_path(&key) else {
        editor.status = "找不到 assets 目录，无法保存".to_string();
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let result = serde_json::to_string_pretty(&editor.doc)
        .map_err(|error| error.to_string())
        .and_then(|json| std::fs::write(&path, json).map_err(|error| error.to_string()));
    match result {
        Ok(()) => editor.status = format!("已保存 {}", path.display()),
        Err(error) => editor.status = format!("保存失败：{error}"),
    }
}

pub(super) fn load_space(editor: &mut SpaceEditor) {
    if try_load_space(editor) {
        editor.status = "已从文件载入混合空间".to_string();
    } else {
        editor.status = "载入失败：文件不存在或格式错误".to_string();
    }
}

pub(super) fn try_load_space(editor: &mut SpaceEditor) -> bool {
    let key = editor.loaded_key.clone().unwrap_or_else(|| "locomotion".to_string());
    let Some(path) = space_path(&key) else {
        return false;
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(doc) = serde_json::from_str::<BlendSpaceDoc>(&content) else {
        warn!("混合空间 JSON 解析失败：{}", path.display());
        return false;
    };

    // 保证两个轴参数都有值
    for axis in 0..2 {
        editor
            .params
            .entry(doc.params[axis].clone())
            .or_insert(0.0);
    }
    editor.doc = doc;
    editor.selected = None;
    editor.revision = editor.revision.wrapping_add(1);
    true
}
