//! 检查器：参数滑杆、采样点列表、剪辑选择、存取。
//!
//! 两段内容各自是一棵 `bsn!` 子树：
//! - [`params_scene`]：混合系统的说明、启用开关、参数滑杆、播放速度，跟着整页一起生成；
//! - [`inspector_scene`]：采样点列表与选中项详情，文档或选择变化时整段重建。

use bevy::{
    ecs::system::SystemParam,
    picking::pointer::PointerButton,
    prelude::*,
    ui_widgets::{ValueChange, slider_self_update},
};

use crate::{
    anim::{AnimCatalog, AnimSelection, MixerCache, load_space_doc, save_space_doc},
    ui::menu::widgets,
};

use super::{
    CLIP_DROPDOWN, EnabledLabel, HINT_COLOR, InspectorBody, MUTED_TEXT, ParamSlider, ParamValue,
    SpaceEditor, TEXT_COLOR, WARN_COLOR, WeightText,
};

/// 参数区：说明 + 启用开关 + 两个轴的滑杆 + 播放速度滑杆
pub(super) fn params_scene(editor: &SpaceEditor, cache: &MixerCache) -> impl Scene + use<> {
    let mut parts: Vec<Box<dyn Scene>> = Vec::new();

    if let Some(mixer) = cache.mixers.get(editor.mixer) {
        let description = mixer.description.clone();
        parts.push(Box::new(bsn! {
            @{widgets::text(description, 9.0, MUTED_TEXT)}
        }));

        let caption = if editor.enabled { "开" } else { "关" }.to_string();
        let title = mixer.title.clone();
        parts.push(Box::new(bsn! {
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
            }
            Children [
                @{widgets::text("游戏中启用", 10.0, MUTED_TEXT)}
                --
                @{widgets::button(caption)}
                EnabledLabel
                on(move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
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
                })
            ]
        }));
    }

    for axis in 0..2 {
        let name = editor.doc.axis(axis).to_string();
        let value = editor.params.get(&name).copied().unwrap_or(0.0);
        // 滑杆范围跟文档自身对齐：yaw 是角度、speed 是归一化速度，量纲不同
        let range = editor.doc.range(axis);
        let step = ((range[1] - range[0]) / 200.0).max(0.001);
        let key = name.clone();
        parts.push(Box::new(bsn! {
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
            }
            Children [
                Text::new(format!("{name} = {value:.2}"))
                TextFont { font_size: FontSize::Px(11.0) }
                TextColor(MUTED_TEXT)
                ParamValue { slot: axis }
                --
                @{widgets::slider(value, range[0], range[1], step)}
                ParamSlider { slot: axis }
                on(slider_self_update)
                on(move |change: On<ValueChange<f32>>, mut editor: ResMut<SpaceEditor>| {
                    editor.params.insert(key.clone(), change.value);
                })
            ]
        }));
    }

    let speed = editor.doc.speed;
    parts.push(Box::new(bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(2.0),
            margin: UiRect::top(Val::Px(6.0)),
        }
        Children [
            Text::new(format!("播放速度 {:.2}x", speed))
            TextFont { font_size: FontSize::Px(11.0) }
            TextColor(MUTED_TEXT)
            ParamValue { slot: 2 }
            --
            @{widgets::slider(speed, 0.1, 3.0, 0.05)}
            ParamSlider { slot: 2 }
            on(slider_self_update)
            on(|change: On<ValueChange<f32>>, mut editor: ResMut<SpaceEditor>| {
                editor.doc.speed = change.value;
            })
        ]
    }));

    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(4.0),
        }
        Children [
            {parts}
        ]
    }
}

/// 重建检查器要读的只读输入：角色清单 + 当前选中的角色。
#[derive(SystemParam)]
pub(super) struct InspectorRead<'w> {
    catalog: Res<'w, AnimCatalog>,
    selection: Res<'w, AnimSelection>,
}

pub(super) fn rebuild_inspector(
    mut commands: Commands,
    editor: ResMut<SpaceEditor>,
    read: InspectorRead,
    bodies: Query<(Entity, Option<&Children>), With<InspectorBody>>,
    mut last: Local<(Option<usize>, u64)>,
) {
    let Some((body, kids)) = bodies.iter().next() else {
        return;
    };
    let key = (editor.selected, editor.revision);
    // 刚 spawn 出来的容器还没有 Children，同样算「空」，需要重建
    let empty = kids.is_none_or(|list| list.is_empty());
    if *last == key && !empty {
        return;
    }
    *last = key;

    for child in kids.map(|list| list.to_vec()).unwrap_or_default() {
        commands.entity(child).despawn();
    }

    commands.spawn_scene(inspector_scene(
        body,
        &editor,
        &read.catalog,
        &read.selection,
    ));
}

/// 采样点区：列表 + 当前位置添加 + 选中项详情 + 当前权重
fn inspector_scene(
    body: Entity,
    editor: &SpaceEditor,
    catalog: &AnimCatalog,
    selection: &AnimSelection,
) -> impl Scene + use<> {
    let clips = catalog
        .character(selection.character)
        .map(|def| def.clips.clone())
        .unwrap_or_default();
    let mut parts: Vec<Box<dyn Scene>> = Vec::new();

    // 采样点列表
    for (index, sample) in editor.doc.samples.iter().enumerate() {
        let selected = editor.selected == Some(index);
        let text = format!(
            "#{index}  {}{}  ({:.2}, {:.2})",
            sample.clip,
            if sample.reverse { " ↺" } else { "" },
            sample.at[0],
            sample.at[1]
        );
        let background = if selected {
            Color::srgba(0.28, 0.42, 0.72, 0.55)
        } else {
            Color::NONE
        };
        parts.push(Box::new(bsn! {
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
                padding: UiRect::axes(Val::Px(4.0), Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
            }
            BackgroundColor(background)
            on(move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                editor.selected = Some(index);
            })
            Children [
                @{widgets::text(text, 11.0, TEXT_COLOR)}
            ]
        }));
    }

    parts.push(Box::new(bsn! {
        @{widgets::button("在当前位置添加采样点")}
        on(move |mut click: On<PointerClick>,
              mut editor: ResMut<SpaceEditor>,
              catalog: Res<AnimCatalog>,
              selection: Res<AnimSelection>| {
            if click.button != PointerButton::Primary {
                return;
            }
            click.propagate(false);
            let point = [
                editor
                    .params
                    .get(&editor.doc.params[0])
                    .copied()
                    .unwrap_or(0.0),
                editor
                    .params
                    .get(&editor.doc.params[1])
                    .copied()
                    .unwrap_or(0.0),
            ];
            let def = catalog.character(selection.character);
            let clip = def
                .as_ref()
                .and_then(|def| {
                    def.clips
                        .iter()
                        .find(|clip| {
                            !editor
                                .doc
                                .samples
                                .iter()
                                .any(|sample| sample.clip.as_str() == clip.as_str())
                        })
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
        })
    }));

    // 选中采样点详情
    if let Some(index) = editor.selected
        && let Some(sample) = editor.doc.samples.get(index).cloned()
    {
        parts.push(Box::new(bsn! {
            @{widgets::text(
                format!("采样点 #{index} 的剪辑"),
                11.0,
                MUTED_TEXT,
            )}
        }));

        // 反转播放：勾选框（用于做后退）
        let mark: Vec<_> = if sample.reverse {
            vec![bsn! {
                Node {
                    width: Val::Px(7.0),
                    height: Val::Px(7.0),
                    border_radius: BorderRadius::all(Val::Px(1.5)),
                }
                BackgroundColor(TEXT_COLOR)
            }]
        } else {
            Vec::new()
        };
        parts.push(Box::new(bsn! {
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                padding: UiRect::axes(Val::Px(4.0), Val::Px(3.0)),
            }
            on(move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                if let Some(sample) = editor.doc.samples.get_mut(index) {
                    sample.reverse = !sample.reverse;
                    let state = if sample.reverse {
                        "反转（后退）"
                    } else {
                        "正放"
                    };
                    editor.status = format!("采样点 #{index} 播放方向 → {state}");
                }
                editor.revision = editor.revision.wrapping_add(1);
            })
            Children [
                Node {
                    width: Val::Px(15.0),
                    height: Val::Px(15.0),
                    flex_shrink: 0.0,
                    border: UiRect::all(Val::Px(1.0)),
                    border_radius: BorderRadius::all(Val::Px(3.0)),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                }
                BackgroundColor({if sample.reverse {
                    widgets::BUTTON_SELECTED
                } else {
                    widgets::BUTTON_IDLE
                }})
                BorderColor::all(widgets::BORDER_SOFT)
                Children [
                    {mark}
                ]
                --
                @{widgets::text("反转播放（后退）", 11.0, TEXT_COLOR)}
            ]
        }));

        // 剪辑选择（下拉框）：选中结果由 `mod::apply_dropdown_pick` 写回
        let selected_clip = clips
            .iter()
            .position(|clip| clip == &sample.clip)
            .unwrap_or(0);
        parts.push(Box::new(bsn! {
            @{widgets::dropdown(CLIP_DROPDOWN, &clips, selected_clip)}
        }));

        parts.push(Box::new(bsn! {
            @{widgets::button("删除该采样点")}
            on(move |mut click: On<PointerClick>, mut editor: ResMut<SpaceEditor>| {
                if click.button != PointerButton::Primary {
                    return;
                }
                click.propagate(false);
                if editor.doc.remove_sample(index) {
                    editor.selected = None;
                    editor.status = format!("已删除采样点 #{index}");
                    editor.revision = editor.revision.wrapping_add(1);
                }
            })
        }));
    }

    // 当前权重
    let point = [
        editor
            .params
            .get(&editor.doc.params[0])
            .copied()
            .unwrap_or(0.0),
        editor
            .params
            .get(&editor.doc.params[1])
            .copied()
            .unwrap_or(0.0),
    ];
    let weights = editor.doc.weights(point);
    let hint = if weights.is_empty() {
        "无可用采样点".to_string()
    } else {
        weights
            .iter()
            .map(|(key, weight)| {
                let (clip, reverse) = crate::anim::core::blendspace::split_key(key);
                format!(
                    "{clip}{} {:.0}%",
                    if reverse { " ↺" } else { "" },
                    weight * 100.0
                )
            })
            .collect::<Vec<_>>()
            .join(" · ")
    };
    parts.push(Box::new(bsn! {
        Text::new(format!("当前权重：{hint}"))
        TextFont { font_size: FontSize::Px(11.0) }
        TextColor(HINT_COLOR)
        WeightText
    }));

    for issue in editor.doc.validate() {
        parts.push(Box::new(bsn! {
            @{widgets::text(format!("! {issue}"), 11.0, WARN_COLOR)}
        }));
    }

    bsn! {
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(4.0),
        }
        ChildOf(body)
        Children [
            {parts}
        ]
    }
}

pub(super) fn save_space(editor: &mut SpaceEditor) {
    let Some(key) = editor.loaded_key.clone() else {
        editor.status = "还没选定混合系统，无法保存".to_string();
        return;
    };
    if editor.character.is_empty() {
        editor.status = "还不知道当前角色，无法保存".to_string();
        return;
    }
    match save_space_doc(&editor.character, &key, &editor.doc) {
        Ok(path) => editor.status = format!("已保存 {path}"),
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
    let (Some(key), false) = (editor.loaded_key.clone(), editor.character.is_empty()) else {
        return false;
    };
    let Some(doc) = load_space_doc(&editor.character, &key) else {
        return false;
    };

    // 保证两个轴参数都有值
    for axis in 0..2 {
        editor.params.entry(doc.params[axis].clone()).or_insert(0.0);
    }
    editor.doc = doc;
    editor.selected = None;
    editor.revision = editor.revision.wrapping_add(1);
    true
}
