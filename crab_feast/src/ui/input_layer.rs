use std::any::TypeId;

use bevy::{
    animation::{AnimatedBy, AnimationEntityMut, AnimationEvaluationError, AnimationTargetId},
    picking::{hover::PointerCaptureMap, pointer::PointerId},
    prelude::*,
};

use crab_feast_ui_joysticks::{
    Joystick, JoystickEvent, JoystickInteraction, JoystickMarionette, JoystickPlugin,
};

use crab_feast_library::platform::PlatformInfo;
use crab_feast_ui_joysticks::JoystickMarionettePlugin;

use crate::GameState;
use crate::input::{JumpInput, LookInput, LookInputIgnorePointers, MoveGait, MovementInput};
use crate::state::SessionReady;

pub struct InputPlugin;

/// 输入 UI 的根节点标记：等世界就绪后再生成，退出场景时整棵子树一起收掉，
/// 这样断线回大厅、重新进游戏还能再建一次。
#[derive(Component)]
struct InputUiRoot;

#[derive(Clone)]
struct BackgroundColorProperty;

#[derive(Component)]
struct JoystickFadeAnimatePlayer {
    fade_in_index: AnimationNodeIndex,
    fade_out_index: AnimationNodeIndex,
}

#[derive(Component, FromTemplate)]
struct MoveInputJoystick;

/// 移动端右下角的跳跃按钮。
///
/// 和摇杆一样挂在输入层根节点下，退出 `Game` 时随整棵子树一起回收。
#[derive(Component)]
struct JumpButton;

/// 跳跃按钮的按住状态。
///
/// 只认第一根指头：第二根指头按上来不该再触发一次，松开时也不该把第一根按着
/// 产生的跳跃电平清掉。
///
/// `release_pending` 是「松手了，但电平要等帧末才回落」的标志：动作层的跳跃锁存
/// 在 `Update` 里读 [`JumpInput`]，如果点按的按下 / 松开落在同一帧（<16ms 的快速
/// 点击），松开当场写 `Idle` 会让这一跳谁都看不见，所以松手只记账，帧末统一回落。
#[derive(Resource, Debug, Default)]
struct JumpButtonState {
    held: Option<PointerId>,
    release_pending: bool,
}

impl JumpButtonState {
    /// 按下：只有第一根指头能按住按钮
    fn press(&mut self, pointer: PointerId) -> bool {
        if self.held.is_some() {
            return false;
        }
        self.held = Some(pointer);
        self.release_pending = false;
        true
    }

    /// 松开：只认按住的那一根，别人的松开事件不算
    fn release(&mut self, pointer: PointerId) -> bool {
        if self.held != Some(pointer) {
            return false;
        }
        self.held = None;
        self.release_pending = true;
        true
    }
}

/// 跳跃按钮直径（占屏宽百分比）
const JUMP_BUTTON_DIAMETER_VW: f32 = 10.0;
/// 跳跃按钮距屏幕右 / 下边缘的距离（占屏宽百分比），与输入层的 padding 对齐
const JUMP_BUTTON_MARGIN_VW: f32 = 10.0;
/// 按钮里「跳」字的字号（占屏宽百分比）
const JUMP_BUTTON_LABEL_VW: f32 = 4.0;
/// 按钮没被按住时的底色
const JUMP_BUTTON_IDLE: Color = Color::hsla(160.0, 0.5, 0.6, 0.06);
/// 按钮被按住时的底色
const JUMP_BUTTON_PRESSED: Color = Color::hsla(160.0, 0.5, 0.7, 0.20);
/// 按钮描边：按住时亮一档
const JUMP_BUTTON_BORDER_IDLE: Color = Color::hsla(160.0, 0.6, 0.8, 0.30);
/// 按钮描边：按住时
const JUMP_BUTTON_BORDER_PRESSED: Color = Color::hsla(160.0, 0.7, 0.9, 0.65);
/// 「跳」字用的字体，和菜单 / 大厅同一套中文字体
const JUMP_BUTTON_FONT: &str = "fonts/PingFang-SC-Regular.ttf";

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(JoystickPlugin)
            .init_resource::<MovementInput>()
            .init_resource::<MoveGait>()
            .init_resource::<JumpButtonState>()
            // 「就绪」才算能操作：角色模型与动画图都在了才生成摇杆与键盘层
            .add_systems(
                Update,
                Self::setup
                    .run_if(in_state(GameState::Game))
                    .run_if(session_ready),
            )
            .add_systems(
                Update,
                update_jump_button_visual.run_if(in_state(GameState::Game)),
            )
            // 松手后让跳跃电平在帧末回落：锁存 / 状态机 / 上行发包都已经在本帧读过了，
            // 既不吞掉「按下和松开在同一帧」的点按，也不会让一帧的电平漏到下一帧
            .add_systems(
                PostUpdate,
                flush_jump_button_release.run_if(in_state(GameState::Game)),
            )
            .add_systems(OnExit(GameState::Game), Self::cleanup)
            .add_systems(
                PreUpdate,
                on_keyboard_event
                    .run_if(is_non_mobile)
                    .run_if(in_state(GameState::Game))
                    .run_if(session_ready),
            );

        if !PlatformInfo::current().is_mobile() {
            app.add_plugins(JoystickMarionettePlugin);
        }
    }
}

fn is_non_mobile(platform: Res<PlatformInfo>) -> bool {
    !platform.is_mobile()
}

fn session_ready(ready: Res<SessionReady>) -> bool {
    ready.0
}

impl InputPlugin {
    fn setup(
        mut commands: Commands,
        mut animation_graphs: ResMut<Assets<AnimationGraph>>,
        mut animation_clips: ResMut<Assets<AnimationClip>>,
        platform: Res<PlatformInfo>,
        asset_server: Res<AssetServer>,
        existing: Query<(), With<InputUiRoot>>,
    ) {
        // 已经是「就绪」状态但这一局的输入层已经建过：不重复生成
        if existing.iter().next().is_some() {
            return;
        }
        let joystick_idle_color = Color::hsla(160.0, 0.5, 0.6, 0.03);
        let joystick_active_color = Color::hsla(160.0, 0.5, 0.7, 0.08);

        let animation_target_name = Name::new("joystick_background_fade");
        let animation_target_id = AnimationTargetId::from_name(&animation_target_name);

        // 创建淡出动画
        let mut fade_out_animation_clip = AnimationClip::default();
        fade_out_animation_clip.add_curve_to_target(
            animation_target_id,
            AnimatableCurve::new(
                BackgroundColorProperty,
                AnimatableKeyframeCurve::new([0.0, 1.0, 1.5].into_iter().zip([
                    Srgba::from(joystick_active_color),
                    Srgba::from(joystick_active_color),
                    Srgba::from(joystick_idle_color),
                ]))
                .unwrap(),
            ),
        );

        // 创建淡入动画
        let mut fade_in_animation_clip = AnimationClip::default();
        fade_in_animation_clip.add_curve_to_target(
            animation_target_id,
            AnimatableCurve::new(
                BackgroundColorProperty,
                AnimatableKeyframeCurve::new([0.0, 0.2].into_iter().zip([
                    Srgba::from(joystick_idle_color),
                    Srgba::from(joystick_active_color),
                ]))
                .unwrap(),
            ),
        );

        // 存储动画句柄
        let fade_out_animation_clip_handle = animation_clips.add(fade_out_animation_clip);
        let fade_in_animation_clip_handle = animation_clips.add(fade_in_animation_clip);

        // 创建包含两个动画的动画图
        let mut animation_graph = AnimationGraph::default();
        let fade_in_index =
            animation_graph.add_clip(fade_in_animation_clip_handle, 1.0, animation_graph.root);
        let fade_out_index =
            animation_graph.add_clip(fade_out_animation_clip_handle, 1.0, animation_graph.root);
        let animation_graph_handle = animation_graphs.add(animation_graph);

        let animation_player = AnimationPlayer::default();
        // 初始不播放任何动画（或根据需要选择默认动画）

        let animation_player_entity = commands
            .spawn((
                animation_player,
                AnimationGraphHandle(animation_graph_handle),
                JoystickFadeAnimatePlayer {
                    fade_in_index,
                    fade_out_index,
                },
            ))
            .id();

        let input_layer_entity = commands
            .spawn((
                InputUiRoot,
                Node {
                    width: Val::Vw(100.0),
                    height: Val::Vh(100.0),
                    padding: UiRect::all(Val::Vw(10.0)),
                    display: Display::Flex,
                    justify_content: JustifyContent::FlexStart,
                    align_items: AlignItems::FlexEnd,
                    ..Default::default()
                },
            ))
            .observe(on_rotate_plane_press)
            .observe(on_rotate_plane_release)
            .observe(on_rotate_plane_drag)
            .id();

        commands
            .spawn_scene(bsn! {
                Node {
                    width: Val::Vw(10.0),
                    height: Val::Vw(10.0),
                    display: Display::Flex,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border: UiRect::all(Val::Percent(0.05)),
                    border_radius: BorderRadius::all(Val::Percent(50.0)),
                }
                ChildOf(input_layer_entity)
                BackgroundColor(joystick_idle_color)
                BorderColor::all(Color::hsla(0.0, 1.0, 1.0, 0.2))
                animation_target_id
                MoveInputJoystick
                Joystick
            })
            .insert(AnimatedBy(animation_player_entity))
            .observe(on_joystick_event);

        // 移动端：右下角补一个跳跃按钮。桌面有空格，不占这块屏幕
        if platform.is_mobile() {
            spawn_jump_button(&mut commands, input_layer_entity, &asset_server);
        }

        // 动画 player 挂到输入层根节点下：退出场景时随根一起回收
        commands
            .entity(input_layer_entity)
            .add_child(animation_player_entity);
    }

    fn cleanup(
        mut commands: Commands,
        roots: Query<Entity, With<InputUiRoot>>,
        mut look_ignore_pointers: ResMut<LookInputIgnorePointers>,
        mut jump: ResMut<JumpInput>,
        mut jump_button: ResMut<JumpButtonState>,
    ) {
        for entity in &roots {
            commands.entity(entity).try_despawn();
        }
        // 退出时可能还有手指按在屏幕上：留下旧 pointer 会让下一局同 id 的指头一直不转视角
        look_ignore_pointers.0.clear();
        // 按钮状态和跳跃电平一起复位，免得断线重进时角色带着上一局的按住状态
        *jump_button = JumpButtonState::default();
        *jump = JumpInput::Idle;
    }
}

fn on_joystick_event(
    joystick_event: On<JoystickEvent>,
    mut joystick_fade_animate_player_query: Query<(
        &mut AnimationPlayer,
        &JoystickFadeAnimatePlayer,
    )>,
    mut move_input_state: ResMut<MovementInput>,
    mut look_ignore_pointers: ResMut<LookInputIgnorePointers>,
) {
    match joystick_event.event {
        JoystickInteraction::Activated(pointer_id) => {
            // println!("Joystick activated: {:?}", joystick_event.entity);
            look_ignore_pointers.0.insert(pointer_id);
            *move_input_state = MovementInput::Activated {
                direction: Vec2::ZERO,
                force: 0.0,
            };

            joystick_fade_animate_player_query.iter_mut().for_each(
                |(mut animation_player, joystick_fade_animate_player)| {
                    animation_player.stop_all();
                    // 播放淡入动画
                    animation_player.play(joystick_fade_animate_player.fade_in_index);
                },
            );
        }
        JoystickInteraction::Moved(new_direction, new_force) => {
            // println!("Joystick moved: {:?}", joystick_event.entity);
            if let MovementInput::Activated { direction, force } = move_input_state.as_mut() {
                *direction = new_direction;
                *force = new_force;
            }
        }
        JoystickInteraction::Deactivated(pointer_id) => {
            // println!("Joystick deactivated: {:?}", joystick_event.entity);
            *move_input_state = MovementInput::Idle;
            look_ignore_pointers.0.remove(&pointer_id);
        }
        JoystickInteraction::Rebound => {
            // println!("Joystick rebound: {:?}", joystick_event.entity);

            joystick_fade_animate_player_query.iter_mut().for_each(
                |(mut animation_player, joystick_fade_animate_player)| {
                    animation_player.stop_all();
                    // 播放淡出动画
                    animation_player.play(joystick_fade_animate_player.fade_out_index);
                },
            );
        }
    }
}

/// 生成右下角的跳跃按钮：一个圆形按钮 + 居中一个「跳」字。
///
/// 用绝对定位而不是 flex 排下去：输入层根节点是 `justify_content: FlexStart` +
/// `align_items: FlexEnd` 的左下对齐，摇杆靠它，按钮再靠它就只能贴在摇杆右边。
/// `right` / `bottom` 的基准是父节点的边框盒（不含 padding），所以这里直接写 10vw，
/// 与摇杆那 10vw 的 padding 边距对齐。
fn spawn_jump_button(commands: &mut Commands, parent: Entity, asset_server: &AssetServer) {
    let font: Handle<Font> = asset_server.load(JUMP_BUTTON_FONT);

    let button = commands
        .spawn((
            JumpButton,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Vw(JUMP_BUTTON_MARGIN_VW),
                bottom: Val::Vw(JUMP_BUTTON_MARGIN_VW),
                width: Val::Vw(JUMP_BUTTON_DIAMETER_VW),
                height: Val::Vw(JUMP_BUTTON_DIAMETER_VW),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Percent(50.0)),
                ..default()
            },
            BackgroundColor(JUMP_BUTTON_IDLE),
            BorderColor::all(JUMP_BUTTON_BORDER_IDLE),
            ChildOf(parent),
        ))
        .observe(on_jump_button_press)
        .observe(on_jump_button_release)
        .observe(on_jump_button_cancel)
        .id();

    // 文字是按钮的子节点，但自己不参与拾取：命中目标始终是按钮本身，
    // 后面要按 `press.entity` 把指针捕获到按钮上，才不会被字挡一层
    commands.spawn((
        Text::new("跳"),
        TextFont {
            font: font.into(),
            font_size: FontSize::Vw(JUMP_BUTTON_LABEL_VW),
            ..default()
        },
        TextColor(Color::srgba(1.0, 1.0, 1.0, 0.92)),
        Pickable::IGNORE,
        ChildOf(button),
    ));
}

/// 按下按钮：跳跃电平立起来，同时把这根指头从「转视角」里摘出去。
///
/// `propagate(false)` 让输入层根节点的「左半屏按下算走路手」逻辑看不到这一下；
/// 按钮在右下角本来也不在那半屏里，这里只是把事件拦在按钮自己的边界内。
///
/// 还要把指针捕获到按钮上：手指按住后滑出按钮圆圈是很常见的，没有捕获的话
/// 松开事件会落到手指下面那个实体，按钮就永远停在「按住」状态、跳跃电平也降不下来。
fn on_jump_button_press(
    mut press: On<PointerPress>,
    mut button: ResMut<JumpButtonState>,
    mut jump: ResMut<JumpInput>,
    mut look_ignore_pointers: ResMut<LookInputIgnorePointers>,
    mut captures: ResMut<PointerCaptureMap>,
) {
    if press.button != PointerButton::Primary {
        return;
    }
    // 已经有一根指头按着：第二根不重复触发，也不抢归属
    if !button.press(press.pointer.id) {
        return;
    }
    press.propagate(false);
    // 按住按钮的那根指头不许再拖视角，否则边跳边看会把镜头甩飞
    look_ignore_pointers.0.insert(press.pointer.id);
    // 捕获到按钮：松开时无论手指在哪，`PointerRelease` 都会回到按钮上
    captures.capture(press.pointer.id, press.entity, press.hit.clone());
    *jump = JumpInput::Activated;
}

/// 松开按钮：只记账，电平留给 [`flush_jump_button_release`] 在帧末回落
fn on_jump_button_release(
    mut release: On<PointerRelease>,
    mut button: ResMut<JumpButtonState>,
    mut look_ignore_pointers: ResMut<LookInputIgnorePointers>,
    mut captures: ResMut<PointerCaptureMap>,
) {
    if release.button != PointerButton::Primary {
        return;
    }
    if !button.release(release.pointer.id) {
        return;
    }
    release.propagate(false);
    look_ignore_pointers.0.remove(&release.pointer.id);
    // 捕获在这一帧的 hover 重建里已经会被释放，这里显式收掉，不依赖时序
    captures.release(release.pointer.id);
}

/// 触摸被系统取消（来电、切后台、手指划出窗口）：按松开处理
fn on_jump_button_cancel(
    mut cancel: On<PointerCancel>,
    mut button: ResMut<JumpButtonState>,
    mut look_ignore_pointers: ResMut<LookInputIgnorePointers>,
    mut captures: ResMut<PointerCaptureMap>,
) {
    if !button.release(cancel.pointer.id) {
        return;
    }
    cancel.propagate(false);
    look_ignore_pointers.0.remove(&cancel.pointer.id);
    captures.release(cancel.pointer.id);
}

/// 帧末把跳跃电平从 `Activated` 落回 `Idle`。
///
/// 放在 `PostUpdate`：本帧的跳跃锁存（`AnimationSet::Publish`）、状态机（`Drive`）
/// 和联机上行采样都跑在 `Update` 里，已经读过这一帧的电平了。这样即使按下和松开
/// 落在同一帧，点按也一定被看见一次；按住不放时则要等松手才回落。
fn flush_jump_button_release(mut button: ResMut<JumpButtonState>, mut jump: ResMut<JumpInput>) {
    if !button.release_pending || button.held.is_some() {
        return;
    }
    button.release_pending = false;
    if matches!(*jump, JumpInput::Activated) {
        *jump = JumpInput::Idle;
    }
}

/// 按住时亮一档，松开复原。按钮没有淡入淡出动画，直接按状态刷色。
fn update_jump_button_visual(
    button: Res<JumpButtonState>,
    mut buttons: Query<(&mut BackgroundColor, &mut BorderColor), With<JumpButton>>,
) {
    let pressed = button.held.is_some();
    let (background, border) = if pressed {
        (JUMP_BUTTON_PRESSED, JUMP_BUTTON_BORDER_PRESSED)
    } else {
        (JUMP_BUTTON_IDLE, JUMP_BUTTON_BORDER_IDLE)
    };
    for (mut bg, mut border_color) in &mut buttons {
        if bg.0 != background {
            bg.0 = background;
        }
        if border_color.top != border {
            *border_color = BorderColor::all(border);
        }
    }
}

impl AnimatableProperty for BackgroundColorProperty {
    type Property = Srgba;

    fn evaluator_id(&self) -> EvaluatorId<'_> {
        EvaluatorId::Type(TypeId::of::<Self>())
    }

    fn get_mut<'a>(
        &self,
        entity: &'a mut AnimationEntityMut,
    ) -> Result<&'a mut Self::Property, AnimationEvaluationError> {
        let background_color = entity
            .get_mut::<BackgroundColor>()
            .ok_or(AnimationEvaluationError::ComponentNotPresent(TypeId::of::<
                BackgroundColor,
            >(
            )))?
            .into_inner();

        if let Color::Srgba(_) = background_color.0 {
        } else {
            background_color.0 = Color::from(background_color.0.to_srgba());
        }

        match background_color.0 {
            Color::Srgba(ref mut color) => Ok(color),
            _ => Err(AnimationEvaluationError::PropertyNotPresent(TypeId::of::<
                Srgba,
            >(
            ))),
        }
    }
}

fn on_rotate_plane_press(
    event: On<PointerPress>,
    target_camera_query: Query<&ComputedUiTargetCamera>,
    mut look_ignore_pointers: ResMut<LookInputIgnorePointers>,
    camera_query: Query<&Camera>,
) {
    if look_ignore_pointers.0.contains(&event.pointer.id) {
        return;
    }
    info!("on_rotate_plane_press: {:?}", event);
    if event.button != PointerButton::Primary {
        return;
    }
    info!("on_rotate_plane_press: {:?}", event.pointer.location());

    let hit_left = target_camera_query
        .get(event.entity)
        .ok()
        .and_then(|target| target.get())
        .and_then(|camera_entity| camera_query.get(camera_entity).ok())
        .map(|camera| {
            camera
                .logical_viewport_size()
                .map(|viewport_size| event.pointer.position.x < viewport_size.x / 2.0)
                .unwrap_or(true)
        });
    if hit_left.unwrap_or(true) {
        look_ignore_pointers.0.insert(event.pointer.id);
    }
}

fn on_rotate_plane_release(
    event: On<PointerRelease>,
    mut look_ignore_pointers: ResMut<LookInputIgnorePointers>,
) {
    look_ignore_pointers.0.remove(&event.pointer.id);
}

fn on_rotate_plane_drag(
    event: On<PointerDrag>,
    mut commands: Commands,
    look_ignore_pointers: ResMut<LookInputIgnorePointers>,
    touches: Res<Touches>,
    target_camera_query: Query<&ComputedUiTargetCamera>,
    camera_query: Query<&Camera>,
) {
    if look_ignore_pointers.0.contains(&event.pointer.id) {
        return;
    }

    // 视角区里已经是两根手指：这一下算捏合，交给 pinch 系统改相机距离，不再转视角。
    // 不然两根手指各产生一条 PointerDrag，转起来是两倍转速，还和缩放抢同一段位移。
    // 桌面没有 TouchInput，`Touches` 恒为空，这句对滚轮 / 鼠标路径没有影响
    if crate::pinch::gesture_ongoing(&touches, &look_ignore_pointers) {
        return;
    }

    let scaled_delta = target_camera_query
        .get(event.entity)
        .ok()
        .and_then(|target| target.get())
        .and_then(|camera_entity| camera_query.get(camera_entity).ok())
        .map(|camera| {
            camera
                .physical_viewport_size()
                .map(|viewport_size| event.delta / viewport_size.x as f32)
                .unwrap_or(event.delta)
        });
    if let Some(delta) = scaled_delta {
        commands.trigger(LookInput(delta));
    }
}

fn on_keyboard_event(
    mut commands: Commands,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut gait: ResMut<MoveGait>,
    move_input_joystick_query: Query<Entity, With<MoveInputJoystick>>,
    mut joystick_marionette_query: Query<(Entity, &mut JoystickMarionette)>,
) {
    // Shift 是切档键：按一下在走 / 跑之间切换，不用按住。
    // 只有「这一帧刚按下」才切，按住不放不会反复翻转。
    if keyboard_input.just_pressed(KeyCode::ShiftLeft)
        || keyboard_input.just_pressed(KeyCode::ShiftRight)
    {
        gait.toggle();
    }
    // 档位量级，不是米/秒：混合空间里 0.5 是走档、1.0 是跑档
    let speed = gait.speed();

    let mut direction = Vec2::ZERO;

    let is_pressed = [
        KeyCode::KeyW,
        KeyCode::KeyA,
        KeyCode::KeyS,
        KeyCode::KeyD,
        KeyCode::ArrowUp,
        KeyCode::ArrowLeft,
        KeyCode::ArrowDown,
        KeyCode::ArrowRight,
    ]
    .iter()
    .any(|&key| keyboard_input.pressed(key));
    if !is_pressed {
        joystick_marionette_query.iter().for_each(|(entity, ..)| {
            commands.entity(entity).remove::<JoystickMarionette>();
        });
        return;
    }

    [
        ((KeyCode::KeyW, KeyCode::ArrowUp), -Vec2::Y),
        ((KeyCode::KeyS, KeyCode::ArrowDown), Vec2::Y),
        ((KeyCode::KeyA, KeyCode::ArrowLeft), -Vec2::X),
        ((KeyCode::KeyD, KeyCode::ArrowRight), Vec2::X),
    ]
    .iter()
    .for_each(|&(keys, offset)| {
        if keyboard_input.pressed(keys.0) || keyboard_input.pressed(keys.1) {
            direction += offset;
        }
    });

    direction = direction.normalize_or_zero();

    move_input_joystick_query.iter().for_each(|entity| {
        if let Ok((_, mut joystick_marionette)) = joystick_marionette_query.get_mut(entity) {
            joystick_marionette.direction = direction;
            joystick_marionette.force = speed;
        } else {
            commands.entity(entity).insert(JoystickMarionette {
                direction,
                force: speed,
                ..Default::default()
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::asset::AssetPlugin;
    use bevy::ecs::system::RunSystemOnce;

    fn touch(index: u64) -> PointerId {
        PointerId::Touch(index)
    }

    /// 多点触控：只有第一根指头算「按住」，第二根按 / 松都不该改状态
    #[test]
    fn only_the_first_pointer_owns_the_button() {
        let mut state = JumpButtonState::default();

        assert!(state.press(touch(1)));
        assert!(!state.press(touch(2)), "第二根指头不该再触发一次");
        assert_eq!(state.held, Some(touch(1)));

        assert!(!state.release(touch(2)), "第二根指头松开不该清掉按住状态");
        assert_eq!(state.held, Some(touch(1)));
        assert!(!state.release_pending);

        assert!(state.release(touch(1)));
        assert_eq!(state.held, None);
        assert!(state.release_pending);
    }

    fn flush_app() -> App {
        let mut app = App::new();
        app.init_resource::<JumpButtonState>()
            .init_resource::<JumpInput>()
            .add_systems(PostUpdate, flush_jump_button_release);
        app
    }

    /// 点按落在同一帧：按下立刻置 Activated，松开只记账，帧末才回落
    #[test]
    fn same_frame_tap_is_visible_for_one_frame() {
        let mut app = flush_app();
        {
            let mut state = app.world_mut().resource_mut::<JumpButtonState>();
            assert!(state.press(touch(1)));
            assert!(state.release(touch(1)));
        }
        *app.world_mut().resource_mut::<JumpInput>() = JumpInput::Activated;

        app.update();

        assert!(
            matches!(*app.world().resource::<JumpInput>(), JumpInput::Idle),
            "帧末应当已经回落"
        );
        assert!(!app.world().resource::<JumpButtonState>().release_pending);
    }

    /// 按住不放：帧末不能把电平落掉，否则按住也只跳一次
    #[test]
    fn holding_keeps_the_level_up() {
        let mut app = flush_app();
        assert!(
            app.world_mut()
                .resource_mut::<JumpButtonState>()
                .press(touch(1))
        );
        *app.world_mut().resource_mut::<JumpInput>() = JumpInput::Activated;

        app.update();

        assert!(
            matches!(*app.world().resource::<JumpInput>(), JumpInput::Activated),
            "按住期间电平必须保持"
        );
    }

    /// 按钮必须贴在输入层根节点的右下角，并带一个居中的「跳」字
    #[test]
    fn jump_button_is_anchored_bottom_right_with_label() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            // `asset_server.load` 要先给字体类型建好资产池，否则连句柄都分配不出来
            .init_asset::<Font>();

        let root = app
            .world_mut()
            .spawn(Node {
                width: Val::Vw(100.0),
                height: Val::Vh(100.0),
                ..default()
            })
            .id();
        app.world_mut()
            .run_system_once(
                move |mut commands: Commands, asset_server: Res<AssetServer>| {
                    spawn_jump_button(&mut commands, root, &asset_server);
                },
            )
            .expect("按钮生成系统跑不起来");
        app.update();

        let world = app.world_mut();
        let mut query = world.query_filtered::<Entity, With<JumpButton>>();
        let button = query.iter(world).next().expect("应该生成跳跃按钮");

        let node = world.get::<Node>(button).expect("按钮要有 Node");
        assert_eq!(node.position_type, PositionType::Absolute);
        assert_eq!(node.right, Val::Vw(JUMP_BUTTON_MARGIN_VW));
        assert_eq!(node.bottom, Val::Vw(JUMP_BUTTON_MARGIN_VW));
        assert_eq!(node.width, Val::Vw(JUMP_BUTTON_DIAMETER_VW));
        assert_eq!(node.height, Val::Vw(JUMP_BUTTON_DIAMETER_VW));
        assert_eq!(
            world.get::<ChildOf>(button).map(|parent| parent.parent()),
            Some(root),
            "按钮要挂在输入层根节点下，随场景一起回收"
        );
        // 圆形：圆角半径写的是 50%，铺满半边长
        assert_eq!(
            node.border_radius.top_left.x,
            Val::Percent(50.0),
            "按钮应该是圆形"
        );

        // 文字是按钮的子节点，但不参与拾取：命中目标仍是按钮本身
        let children: Vec<Entity> = world
            .get::<Children>(button)
            .map(|children| children.iter().collect())
            .unwrap_or_default();
        assert_eq!(children.len(), 1, "按钮里只放一个标签");
        let label = children[0];
        assert_eq!(
            world.get::<Text>(label).map(|text| text.0.as_str()),
            Some("跳")
        );
        assert!(
            world
                .get::<TextFont>(label)
                .is_some_and(|font| font.font_size == FontSize::Vw(JUMP_BUTTON_LABEL_VW)),
            "标签字号应当跟着屏宽走"
        );
    }
}
