//! 移动端双指缩放：一套**独立的触摸输入**，不和鼠标滚轮、摇杆、拖拽视角混在一起。
//!
//! 这里只做两件事：把「两根手指的间距变化」算成一个倍率，发给相机层；
//! 以及告诉视角拖拽「现在是捏合，别转」（见 [`gesture_ongoing`]）。
//! 米数区间和缓动都在 [`crate::camera`] 里，输入层碰不到相机实体。
//!
//! 只有「视角区」的手指才算参与捏合：[`LookInputIgnorePointers`] 里的那根
//! （摇杆按住、或左半屏按下）是走路手，让它进来就会出现「左手走路 + 右手看路」
//! 边走边缩放的联动。
//!
//! 整个插件只在触屏设备上生效，见 [`TouchSupport`]。

use bevy::{picking::pointer::PointerId, prelude::*};
use crab_feast_library::platform::PlatformInfo;

use crate::GameState;
use crate::camera::PinchZoom;
use crate::input::{ControlSet, LookInputIgnorePointers};

pub struct PinchPlugin;

impl Plugin for PinchPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TouchSupport>()
            .init_resource::<PinchGesture>()
            // 能力探测每帧都跑（很便宜）：第一次真摸上来就永久打开，大厅里摸一下也算
            .add_systems(Update, detect_touch_support)
            .add_systems(
                Update,
                pinch_input
                    .run_if(touchscreen)
                    .run_if(in_state(GameState::Game))
                    // 先让能力探测把「这次真的摸上来了」判完，同一帧才开始捏合
                    .after(detect_touch_support)
                    // 倍率要先写进消息，同一帧的 zoom_camera 才读得到
                    .before(ControlSet::Camera),
            );
    }
}

/// 这台设备有没有触屏，也就是「双指缩放该不该存在」。
///
/// winit / Bevy 都**没有** `has_touchscreen()` 这种能力查询：窗口不会在创建时
/// 告诉你支不支持触摸，只会在真的摸上来之后发 `TouchInput`。能拿到的证据只有两条：
///
/// 1. **编译目标**：iOS / Android 的屏幕就是触屏，[`PlatformInfo::is_mobile`] 直接成立；
/// 2. **运行时事件**：Web、Windows 触控本、带触摸的外接显示器只能等第一次触摸
///    ——`Touches` 里出现按下点就说明有，而且以后还会有。
///
/// 取两者的并：移动端第一帧就开，桌面在没有触摸事件之前永远不开，
/// 于是鼠标滚轮那条路完全不受这套逻辑影响。
#[derive(Resource, Debug, Default)]
pub struct TouchSupport {
    /// 本进程是否认定触屏可用
    pub enabled: bool,
}

/// 能力探测：第一次满足条件后置真，之后不再查。
fn detect_touch_support(
    platform: Res<PlatformInfo>,
    touches: Res<Touches>,
    mut support: ResMut<TouchSupport>,
) {
    if support.enabled {
        return;
    }
    let mobile = platform.is_mobile();
    if !mobile && touches.iter().next().is_none() {
        return;
    }
    support.enabled = true;
    info!(
        "双指缩放已启用：{}",
        if mobile {
            "移动端平台自带触屏"
        } else {
            "检测到真实触摸输入"
        }
    );
}

/// 运行条件：没有触屏的设备上，捏合系统一条都不跑。
fn touchscreen(support: Res<TouchSupport>) -> bool {
    support.enabled
}

/// 捏合手势的跨帧状态。
#[derive(Resource, Default)]
struct PinchGesture {
    /// 锁定的一对指头（触控 id，升序）。换手（第二根落下 / 一根抬起）就重设基准
    pair: Option<(u64, u64)>,
    /// 这对手指上一帧的间距（物理像素）
    spread: f32,
}

/// 基准间距下限（像素）：两指叠在一起时，位置量化误差会被放大成几倍
const MIN_SPREAD: f32 = 24.0;
/// 单帧倍率夹紧：丢一帧触摸事件也不会把相机甩飞
const RATIO_CLAMP: (f32, f32) = (0.85, 1.18);
/// 相对变化小于它就算手指没动（读数抖动不该让相机一直缓慢漂）
const RATIO_DEADZONE: f32 = 0.002;

/// 视角区里此刻按着的手指（触控 id + 屏幕坐标，按 id 升序）。
/// 「捏合」和「捏合时先别转视角」共用这一份口径，免得两边对「几根手指」的判断分叉。
pub fn look_area_fingers(touches: &Touches, ignored: &LookInputIgnorePointers) -> Vec<(u64, Vec2)> {
    let mut fingers: Vec<(u64, Vec2)> = touches
        .iter()
        .filter(|touch| !ignored.contains(PointerId::Touch(touch.id())))
        .map(|touch| (touch.id(), touch.position()))
        .collect();
    fingers.sort_unstable_by_key(|(id, _)| *id);
    fingers
}

/// 现在是不是处在双指手势里：视角区至少两根手指。
///
/// 视角拖拽系统在触发 [`crate::input::LookInput`] 之前问一句：两根手指各拖各的，
/// 会各自产生一条 `PointerDrag`，叠起来就是两倍转速，还和缩放抢同一段位移。
pub fn gesture_ongoing(touches: &Touches, ignored: &LookInputIgnorePointers) -> bool {
    look_area_fingers(touches, ignored).len() >= 2
}

/// 每帧把「手指间距」折算成相机距离倍率。
fn pinch_input(
    touches: Res<Touches>,
    ignored: Res<LookInputIgnorePointers>,
    mut gesture: ResMut<PinchGesture>,
    mut zoom: MessageWriter<PinchZoom>,
) {
    let fingers = look_area_fingers(&touches, &ignored);
    if let Some(ratio) = advance(&mut gesture, &fingers) {
        zoom.write(PinchZoom(ratio));
    }
}

/// 状态机本体：记基准、算本帧倍率。抽成纯函数，单测不用造 [`Touches`]。
///
/// 第一帧拿到第二根手指时只记基准（返回 `None`），不然手势开始那一刻的间距差
/// 会被当成一次「已经捏完了」，相机先跳一下。
fn advance(gesture: &mut PinchGesture, fingers: &[(u64, Vec2)]) -> Option<f32> {
    let pair = pinch_pair(fingers);
    if pair.map(|(ids, _)| ids) != gesture.pair {
        gesture.pair = pair.map(|(ids, _)| ids);
        gesture.spread = pair.map(|(_, spread)| spread).unwrap_or(0.0);
        return None;
    }

    let (_, spread) = pair?;
    let ratio = pinch_ratio(gesture.spread, spread);
    gesture.spread = spread;
    ratio
}

/// 从在场手指里挑一对：id 最小的两根，加上它们的间距。
///
/// id 在整段手势里不变，按它排序就不会因为取指顺序漂移而每帧换手。
fn pinch_pair(fingers: &[(u64, Vec2)]) -> Option<((u64, u64), f32)> {
    let [first, second, ..] = fingers else {
        return None;
    };
    Some(((first.0, second.0), first.1.distance(second.1)))
}

/// 两指间距的相对变化 → 相机距离倍率。
///
/// 间距变小（捏拢）→ `previous / current > 1` → 距离变大、相机拉远；
/// 间距变大（张开）→ 倍率 < 1 → 距离变小、相机拉近。和设备上「张开放大、捏拢缩小」一致。
fn pinch_ratio(previous: f32, current: f32) -> Option<f32> {
    if previous < MIN_SPREAD || current < MIN_SPREAD {
        return None;
    }
    let ratio = (previous / current).clamp(RATIO_CLAMP.0, RATIO_CLAMP.1);
    ((ratio - 1.0).abs() >= RATIO_DEADZONE).then_some(ratio)
}

#[cfg(test)]
mod tests {
    use bevy::input::touch::{TouchInput, TouchPhase, touch_screen_input_system};

    use super::*;

    fn fingers(points: &[(u64, f32, f32)]) -> Vec<(u64, Vec2)> {
        points
            .iter()
            .map(|(id, x, y)| (*id, Vec2::new(*x, *y)))
            .collect()
    }

    /// 手势开始那一帧不能缩放：先记基准，手指不动就不该有倍率
    #[test]
    fn first_frame_only_records_baseline() {
        let mut gesture = PinchGesture::default();
        assert_eq!(
            advance(
                &mut gesture,
                &fingers(&[(1, 100.0, 100.0), (2, 200.0, 100.0)])
            ),
            None
        );
        // 第二帧手指也没动
        assert_eq!(
            advance(
                &mut gesture,
                &fingers(&[(1, 100.0, 100.0), (2, 200.0, 100.0)])
            ),
            None
        );
        assert!(gesture.pair.is_some());
    }

    /// 张开 → 倍率 < 1（相机拉近）；捏拢 → 倍率 > 1（相机拉远）
    #[test]
    fn spreading_zooms_in_and_pinching_zooms_out() {
        let mut gesture = PinchGesture::default();
        advance(&mut gesture, &fingers(&[(1, 0.0, 0.0), (2, 100.0, 0.0)]));

        let closer = advance(
            &mut gesture,
            &[(1, Vec2::new(0.0, 0.0)), (2, Vec2::new(110.0, 0.0))],
        )
        .unwrap();
        assert!(closer < 1.0, "张开手指该拉近相机，倍率 {closer}");

        let mut gesture = PinchGesture::default();
        advance(&mut gesture, &fingers(&[(1, 0.0, 0.0), (2, 100.0, 0.0)]));
        let farther = advance(
            &mut gesture,
            &[(1, Vec2::new(0.0, 0.0)), (2, Vec2::new(90.0, 0.0))],
        )
        .unwrap();
        assert!(farther > 1.0, "捏拢手指该拉远相机，倍率 {farther}");
    }

    /// 单帧倍率有上限：丢帧后间距突然差一大截，也只走一档
    #[test]
    fn ratio_is_clamped_per_frame() {
        let far = pinch_ratio(200.0, 40.0).unwrap();
        assert!((far - RATIO_CLAMP.1).abs() < 1e-6);
        let close = pinch_ratio(40.0, 200.0).unwrap();
        assert!((close - RATIO_CLAMP.0).abs() < 1e-6);
    }

    /// 两指几乎叠在一起时不算：位置噪声会放大成离谱的倍率
    #[test]
    fn tiny_spread_is_ignored() {
        assert_eq!(pinch_ratio(MIN_SPREAD - 1.0, 100.0), None);
        assert_eq!(pinch_ratio(100.0, MIN_SPREAD - 1.0), None);
        assert!(pinch_ratio(100.0, 90.0).is_some());
    }

    /// 手指没动（读数抖动在死区内）就不产生消息，相机不会慢慢漂
    #[test]
    fn jitter_inside_deadzone_does_not_zoom() {
        assert_eq!(pinch_ratio(200.0, 200.1), None);
        assert_eq!(pinch_ratio(200.0, 199.9), None);
    }

    /// 换手要重设基准：一根抬起、另一根进来，不能拿旧间距接着算
    #[test]
    fn pair_change_resets_the_gesture() {
        let mut gesture = PinchGesture::default();
        advance(&mut gesture, &fingers(&[(1, 0.0, 0.0), (2, 100.0, 0.0)]));
        // 2 号抬起，3 号落下（间距突然变成 200）
        assert_eq!(
            advance(
                &mut gesture,
                &[(1, Vec2::new(0.0, 0.0)), (3, Vec2::new(200.0, 0.0))]
            ),
            None,
            "换手指那一帧只记新基准"
        );
        assert_eq!(gesture.pair, Some((1, 3)));
    }

    /// 少于两根手指就退出相位，下一帧再落第二根时重新记基准
    #[test]
    fn lifting_one_finger_ends_the_gesture() {
        let mut gesture = PinchGesture::default();
        advance(&mut gesture, &fingers(&[(1, 0.0, 0.0), (2, 100.0, 0.0)]));
        assert_eq!(advance(&mut gesture, &fingers(&[(1, 0.0, 0.0)])), None);
        assert_eq!(gesture.pair, None);
        assert_eq!(gesture.spread, 0.0);
    }

    /// 三根手指时锁最小的一对：第三根不影响，也不会每帧换手
    #[test]
    fn third_finger_does_not_steal_the_pair() {
        let mut gesture = PinchGesture::default();
        let three = fingers(&[(1, 0.0, 0.0), (2, 100.0, 0.0), (7, 50.0, 50.0)]);
        advance(&mut gesture, &three);
        assert_eq!(gesture.pair, Some((1, 2)));
    }

    // ---- 系统层面：真的把 TouchInput 喂进 Bevy 的 `Touches`，跑到发消息为止 ----

    /// 只挂触摸输入 + 捏合的最小 App
    #[derive(Resource, Default)]
    struct Collected(Vec<f32>);

    #[derive(Resource)]
    struct TestWindow(Entity);

    fn collect(mut reader: MessageReader<PinchZoom>, mut out: ResMut<Collected>) {
        out.0.extend(reader.read().map(|pinch| pinch.0));
    }

    fn pinch_app() -> App {
        let mut app = App::new();
        app.add_message::<TouchInput>()
            .add_message::<PinchZoom>()
            .init_resource::<Touches>()
            .init_resource::<LookInputIgnorePointers>()
            .init_resource::<PinchGesture>()
            .init_resource::<Collected>()
            .add_systems(PreUpdate, touch_screen_input_system)
            .add_systems(Update, (pinch_input, collect).chain());
        let window = app.world_mut().spawn_empty().id();
        app.insert_resource(TestWindow(window));
        app
    }

    fn touch(app: &mut App, phase: TouchPhase, id: u64, position: Vec2) {
        let window = app.world().resource::<TestWindow>().0;
        app.world_mut().write_message(TouchInput {
            phase,
            position,
            window,
            force: None,
            id,
        });
    }

    /// 两根手指落在视角区、只把其中一根拉开：必须产生一条「拉近」的倍率
    #[test]
    fn two_fingers_in_look_area_emit_a_zoom_ratio() {
        let mut app = pinch_app();
        touch(&mut app, TouchPhase::Started, 1, Vec2::new(100.0, 100.0));
        touch(&mut app, TouchPhase::Started, 2, Vec2::new(200.0, 100.0));
        app.update();
        // 第二根手指刚落下这一帧只记基准
        assert!(app.world().resource::<Collected>().0.is_empty());

        touch(&mut app, TouchPhase::Moved, 2, Vec2::new(300.0, 100.0));
        app.update();
        let ratios = &app.world().resource::<Collected>().0;
        assert_eq!(ratios.len(), 1, "应当只有一条倍率：{ratios:?}");
        assert!(ratios[0] < 1.0, "张开手指该拉近：{}", ratios[0]);
    }

    /// 摇杆那根手指不参与捏合：左手推杆 + 右手看路不能联动改距离
    #[test]
    fn joystick_finger_is_excluded() {
        let mut app = pinch_app();
        app.world_mut()
            .resource_mut::<LookInputIgnorePointers>()
            .0
            .insert(PointerId::Touch(1));

        touch(&mut app, TouchPhase::Started, 1, Vec2::new(100.0, 100.0));
        touch(&mut app, TouchPhase::Started, 2, Vec2::new(200.0, 100.0));
        app.update();
        touch(&mut app, TouchPhase::Moved, 2, Vec2::new(300.0, 100.0));
        app.update();

        assert!(
            app.world().resource::<Collected>().0.is_empty(),
            "只剩一根视角手指，不该缩放"
        );
    }

    /// 两根手指都在视角区时，视角拖拽要让位（否则两条 PointerDrag 叠成两倍转速）
    #[test]
    fn gesture_is_ongoing_only_with_two_free_fingers() {
        let mut app = pinch_app();
        let ignored = LookInputIgnorePointers::default();

        touch(&mut app, TouchPhase::Started, 1, Vec2::new(100.0, 100.0));
        app.update();
        assert!(!gesture_ongoing(
            app.world().resource::<Touches>(),
            &ignored
        ));

        touch(&mut app, TouchPhase::Started, 2, Vec2::new(200.0, 100.0));
        app.update();
        assert!(gesture_ongoing(app.world().resource::<Touches>(), &ignored));
    }
}
