//! 菜单页面内容区的**触屏上下滑动**：手指拖动滚动、松手带惯性。
//!
//! 滚轮那套逻辑仍留在 `drawer` 的 `PointerScroll` 观察者里，这里只管手指：
//!
//! - 只认 [`PointerId::Touch`]。鼠标拖动（桌面、以及带鼠标的模拟器）完全不参与，
//!   桌面上的滚轮路径一行没动；
//! - 第一根落在内容区里的手指成为「滑动手指」，其余手指（第二根、捏合的另一根）一律忽略，
//!   双指手势不会把页面当滚轮转；
//! - 位移超过 [`DRAG_START_THRESHOLD`] 才算滑动，之前攒下的那几像素在激活时一次性补上：
//!   「点一下」不会让内容挪位，起手也不会跳；
//! - 松手带着速度，按 [`MOMENTUM_FRICTION`] 指数衰减继续走，撞到上下边界立刻收手；
//! - 滑动期间页面上盖一层透明的 [`ScrollDragShield`]——Bevy 的 `PointerClick` 不检查位移，
//!   手指抬起时会照样点在按下时的那个控件上（会误触按钮、误切页面），盖住之后松手落在遮罩上，
//!   下面那一行既收不到 `Click` 也不会被按下。
//!
//! 页面切换 / 菜单关闭时把滑动与惯性一并收掉，新页面从顶部开始。

use bevy::{picking::pointer::PointerId, prelude::*};

use super::drawer::{MenuEvent, MenuPageBody};

/// 手指位移超过它（逻辑像素）才算「在滑动」，否则按点击算。
///
/// 6px 约等于 iOS 的触摸容差：手指静止时的抖动到不了这个量级，
/// 而真要滑的人抬手就是几十像素，不会觉得「要推一下才动」。
const DRAG_START_THRESHOLD: f32 = 6.0;

/// 拖动测速的一阶平滑系数。0.5 能压掉触摸采样的锯齿，
/// 又不至于让松手那一瞬间的速度滞后（惯性距离全靠它）。
const DRAG_VELOCITY_SMOOTHING: f32 = 0.5;

/// 惯性摩擦（每秒衰减到 e^-x）：12 大约 300ms 掉到初速的 3%。
///
/// 按 `v0 = 1000px/s` 算，惯性还能再滑 83px 左右——手指一甩能多走小半屏，
/// 但不会一路滑到底。嫌「甩得太溜」就调大，嫌「松手就钉住」就调小。
const MOMENTUM_FRICTION: f32 = 12.0;

/// 松手速度低于它就不做惯性（逻辑像素/秒）。太小会让内容永远差几像素停不下来。
const MOMENTUM_MIN_SPEED: f32 = 60.0;

/// 单帧最多按这么长的时间积分。掉帧到 20fps 以下时，
/// 再用真实 delta 会让一帧滚过半屏，看着像闪断。
const MAX_FRAME_DELTA: f32 = 1.0 / 20.0;

/// 页面内容区的滑动状态。
///
/// 同一时间最多一根手指在滑，所以按下 / 拖动 / 抬起都用「那唯一的一根」来记账。
#[derive(Resource, Default, Debug)]
pub struct PageScroll {
    drag: Option<PageDrag>,
    momentum: Option<PageMomentum>,
}

impl PageScroll {
    /// 当前是否有手指正在滑（遮罩的显隐按它决定）
    pub fn is_scrolling(&self) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.scrolling)
    }

    /// 收掉滑动与惯性（切页、关菜单时用）
    pub fn cancel(&mut self) {
        self.drag = None;
        self.momentum = None;
    }
}

/// 正在进行的一次手指拖动
#[derive(Debug)]
struct PageDrag {
    pointer: PointerId,
    /// 被拖的内容区
    body: Entity,
    /// 阈值判定用的累计位移（取绝对值累加，来回抖不会互相抵消）
    travelled: f32,
    /// 越过阈值之前攒下的位移，激活时一次性补上
    pending: f32,
    /// 是否已经越过阈值、正式在滑
    scrolling: bool,
    /// 内容偏移速度（逻辑像素/秒，正数 = 内容继续往上走 / 页面向下滚）
    velocity: f32,
}

/// 松手后的惯性
#[derive(Debug, Clone, Copy)]
struct PageMomentum {
    body: Entity,
    velocity: f32,
}

/// 拖动期间盖在页面上的透明遮罩。
///
/// 由 `drawer::setup_menu` 生成（和抽屉同一层级，`GlobalZIndex` 压在页面之上），
/// 平时 `Display::None`，手指真的在滑时才显示。
#[derive(Component, Clone, Copy, Default, Debug)]
pub struct ScrollDragShield;

/// 落指：认下「这次滑动归哪根手指、哪个页面内容区」，并把上一手的惯性停住。
///
/// 取反要在拖动阶段做，这里只负责记账；桌面（鼠标）与多指手势在这一步就被挡掉。
pub(crate) fn page_scroll_begin(
    mut scroll: ResMut<PageScroll>,
    mut presses: MessageReader<PointerPress>,
    parents: Query<&ChildOf>,
    bodies: Query<(), With<MenuPageBody>>,
) {
    for event in presses.read() {
        // 只认手指；已经有一根在滑时，后面落下的手指一律不管
        if !is_touch(event.pointer.id) || scroll.drag.is_some() {
            continue;
        }
        let Some(body) = menu_page_ancestor(event.entity, &parents, &bodies) else {
            continue;
        };
        scroll.drag = Some(PageDrag {
            pointer: event.pointer.id,
            body,
            travelled: 0.0,
            pending: 0.0,
            scrolling: false,
            velocity: 0.0,
        });
        // 手指一落，上一手的惯性立刻停：跟手比让它自己滑完重要
        scroll.momentum = None;
    }
}

/// 跟手：越过阈值之后内容区按手指位移走，并把积攒的那几像素一次性补上
pub(crate) fn page_scroll_move(
    mut scroll: ResMut<PageScroll>,
    mut drags: MessageReader<PointerDrag>,
    mut scrollers: Query<(&mut ScrollPosition, &ComputedNode)>,
    time: Res<Time>,
) {
    let dt = time.delta_secs().clamp(0.0, MAX_FRAME_DELTA);

    for event in drags.read() {
        let Some(drag) = scroll.drag.as_mut() else {
            continue;
        };
        if drag.pointer != event.pointer.id {
            continue;
        }
        let dy = event.delta.y;
        if dy == 0.0 {
            continue;
        }

        // 手指往上（dy < 0）→ 内容往上走（偏移变大），所以取反
        let instant = -dy / dt.max(1.0 / 240.0);
        drag.velocity += (instant - drag.velocity) * DRAG_VELOCITY_SMOOTHING;

        let body = drag.body;
        let mut delta = -dy;
        if !drag.scrolling {
            drag.travelled += dy.abs();
            drag.pending += delta;
            if drag.travelled < DRAG_START_THRESHOLD {
                continue;
            }
            drag.scrolling = true;
            delta = std::mem::take(&mut drag.pending);
        }

        if let Ok((mut position, node)) = scrollers.get_mut(body) {
            scroll_by(&mut position, node, delta);
        }
    }
}

/// 抬指：还带着速度就转成惯性，否则就停在这里
pub(crate) fn page_scroll_end(
    mut scroll: ResMut<PageScroll>,
    mut releases: MessageReader<PointerRelease>,
) {
    for event in releases.read() {
        let mine = scroll
            .drag
            .as_ref()
            .map(|drag| drag.pointer == event.pointer.id)
            .unwrap_or(false);
        if !mine {
            continue;
        }
        let Some(finished) = scroll.drag.take() else {
            continue;
        };
        if finished.scrolling && finished.velocity.abs() >= MOMENTUM_MIN_SPEED {
            scroll.momentum = Some(PageMomentum {
                body: finished.body,
                velocity: finished.velocity,
            });
        }
    }
}

/// 松手后的惯性：指数衰减 + 撞边界即停
pub(crate) fn page_scroll_momentum(
    mut scroll: ResMut<PageScroll>,
    time: Res<Time>,
    mut scrollers: Query<(&mut ScrollPosition, &ComputedNode)>,
) {
    let Some(momentum) = scroll.momentum else {
        return;
    };
    // 又按下去了：让位给拖动
    if scroll.drag.is_some() {
        scroll.momentum = None;
        return;
    }

    let dt = time.delta_secs().clamp(0.0, MAX_FRAME_DELTA);
    let step = momentum.velocity * dt;
    let Ok((mut position, node)) = scrollers.get_mut(momentum.body) else {
        scroll.momentum = None;
        return;
    };
    let moved = scroll_by(&mut position, node, step);
    // 请求走 step、实际只挪了不到这么多 ⇒ 撞到上下边界了
    let blocked = moved.abs() + 0.01 < step.abs();

    let decayed = momentum.velocity * (-MOMENTUM_FRICTION * dt).exp();
    if blocked || decayed.abs() < MOMENTUM_MIN_SPEED {
        scroll.momentum = None;
    } else {
        scroll.momentum = Some(PageMomentum {
            velocity: decayed,
            ..momentum
        });
    }
}

/// 拖动遮罩的显隐：手指真的在滑时才盖上去
pub(crate) fn sync_scroll_shield(
    scroll: Res<PageScroll>,
    mut shields: Query<&mut Node, With<ScrollDragShield>>,
) {
    let want = if scroll.is_scrolling() {
        Display::Flex
    } else {
        Display::None
    };
    for mut node in &mut shields {
        if node.display != want {
            node.display = want;
        }
    }
}

/// 切页 / 关闭菜单：收掉滑动与惯性，进入的页面从顶部开始看
pub(crate) fn reset_page_scroll(
    mut events: MessageReader<MenuEvent>,
    mut scroll: ResMut<PageScroll>,
    mut bodies: Query<(&MenuPageBody, &mut ScrollPosition)>,
) {
    for event in events.read() {
        match event {
            MenuEvent::PageChanged { to, .. } => {
                scroll.cancel();
                for (body, mut position) in &mut bodies {
                    if body.page == *to {
                        position.0 = Vec2::ZERO;
                    }
                }
            }
            MenuEvent::Opened => scroll.cancel(),
            MenuEvent::Closed => scroll.cancel(),
        }
    }
}

/// 这根指针是不是手指
fn is_touch(pointer: PointerId) -> bool {
    matches!(pointer, PointerId::Touch(_))
}

/// 从落点往上找最近的内容区（`MenuPageBody`）；落在页眉、抽屉空白处就返回 `None`
fn menu_page_ancestor(
    entity: Entity,
    parents: &Query<&ChildOf>,
    bodies: &Query<(), With<MenuPageBody>>,
) -> Option<Entity> {
    let mut current = entity;
    loop {
        if bodies.contains(current) {
            return Some(current);
        }
        current = parents.get(current).ok()?.parent();
    }
}

/// 内容区最多能往上滚多远（逻辑像素）
fn max_scroll(node: &ComputedNode) -> f32 {
    (node.content_size.y - node.size.y).max(0.0) * node.inverse_scale_factor
}

/// 改内容区偏移，返回实际挪动的像素（撞边界时会少于请求量）。
///
/// `delta` 是**偏移量的变化**：正数让页面向下滚（看后面的内容），负数往回滚。
fn scroll_by(position: &mut ScrollPosition, node: &ComputedNode, delta: f32) -> f32 {
    let before = position.0.y;
    position.0.y = (before + delta).clamp(0.0, max_scroll(node));
    position.0.y - before
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::camera::NormalizedRenderTarget;
    use bevy::window::WindowRef;

    /// 一个页面内容区 + 它里面的一个子节点（模拟按下的是哪一行）
    struct Fixture {
        app: App,
        body: Entity,
        child: Entity,
    }

    /// 内容 1000px、可视 300px → 最多能滚 700px；缩放因子按 1 算
    fn fixture(content: f32, size: f32) -> Fixture {
        // 不要 `MinimalPlugins`：TimePlugin 会用真实时钟覆盖掉测试手推的 delta
        let mut app = App::new();
        app.insert_resource(Time::<()>::default())
            .init_resource::<PageScroll>()
            .add_message::<PointerPress>()
            .add_message::<PointerDrag>()
            .add_message::<PointerRelease>()
            .add_message::<MenuEvent>()
            .add_systems(
                Update,
                (
                    reset_page_scroll,
                    page_scroll_begin,
                    page_scroll_move,
                    page_scroll_end,
                    page_scroll_momentum,
                    sync_scroll_shield,
                )
                    .chain(),
            );

        let body = app
            .world_mut()
            .spawn((
                MenuPageBody { page: Some("page") },
                ScrollPosition::default(),
                ComputedNode {
                    size: Vec2::new(340.0, size),
                    content_size: Vec2::new(340.0, content),
                    inverse_scale_factor: 1.0,
                    ..Default::default()
                },
                Node::default(),
            ))
            .id();
        let child = app.world_mut().spawn((Node::default(), ChildOf(body))).id();
        // 和 `setup_menu` 里一致：平时 `Display::None`，滑动时才由系统翻成 `Flex`
        app.world_mut().spawn((
            ScrollDragShield,
            Node {
                display: Display::None,
                ..Default::default()
            },
        ));

        Fixture { app, body, child }
    }

    fn pointer(id: PointerId, y: f32) -> Pointer {
        // 测试里不关心渲染目标，借一个占位窗口实体凑出 `NormalizedWindowRef`
        let target = WindowRef::Primary
            .normalize(Some(Entity::PLACEHOLDER))
            .expect("占位主窗口");
        Pointer::new(
            id,
            bevy::picking::pointer::Location {
                target: NormalizedRenderTarget::Window(target),
                position: Vec2::new(100.0, y),
            },
        )
    }

    fn touch(id: u64) -> PointerId {
        PointerId::Touch(id)
    }

    fn hit() -> bevy::picking::backend::HitData {
        bevy::picking::backend::HitData {
            camera: Entity::PLACEHOLDER,
            depth: 0.0,
            position: None,
            normal: None,
            extra: None,
        }
    }

    fn step(app: &mut App) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
        app.update();
    }

    fn press(f: &mut Fixture, id: PointerId) {
        f.app.world_mut().write_message(PointerPress {
            entity: f.child,
            pointer: pointer(id, 400.0),
            button: PointerButton::Primary,
            hit: hit(),
            count: 1,
        });
        step(&mut f.app);
    }

    fn drag(f: &mut Fixture, id: PointerId, dy: f32) {
        f.app.world_mut().write_message(PointerDrag {
            entity: f.child,
            pointer: pointer(id, 400.0),
            button: PointerButton::Primary,
            distance: Vec2::new(0.0, dy),
            delta: Vec2::new(0.0, dy),
        });
        step(&mut f.app);
    }

    fn release(f: &mut Fixture, id: PointerId) {
        f.app.world_mut().write_message(PointerRelease {
            entity: f.child,
            pointer: pointer(id, 400.0),
            button: PointerButton::Primary,
            hit: hit(),
        });
        step(&mut f.app);
    }

    fn offset(f: &mut Fixture) -> f32 {
        f.app.world().get::<ScrollPosition>(f.body).unwrap().0.y
    }

    fn shield(f: &mut Fixture) -> Display {
        let world = f.app.world_mut();
        let mut query = world.query::<(&ScrollDragShield, &Node)>();
        query.iter(world).next().unwrap().1.display
    }

    /// 手指往上拖 100px：页面跟着走 100px，全程不遮住别的逻辑
    #[test]
    fn vertical_drag_scrolls_the_page() {
        let mut f = fixture(1000.0, 300.0);
        press(&mut f, touch(1));
        // 第一段还在阈值内：内容不许动
        drag(&mut f, touch(1), -4.0);
        assert_eq!(offset(&mut f), 0.0, "没过阈值不该挪");
        // 第二段越过阈值：攒下的 -4 一起补上
        drag(&mut f, touch(1), -40.0);
        assert!((offset(&mut f) - 44.0).abs() < 0.01, "{}", offset(&mut f));
        release(&mut f, touch(1));
    }

    /// 点一下（抬起前没有超过阈值的位移）：内容一动不动，仍然是点击
    #[test]
    fn a_tap_does_not_scroll() {
        let mut f = fixture(1000.0, 300.0);
        press(&mut f, touch(2));
        drag(&mut f, touch(2), -3.0);
        release(&mut f, touch(2));
        assert_eq!(offset(&mut f), 0.0);
    }

    /// 滚到上下边界就停住，不会把内容甩出可视区
    #[test]
    fn scroll_is_clamped_to_content() {
        let mut f = fixture(1000.0, 300.0);
        press(&mut f, touch(3));
        for _ in 0..10 {
            drag(&mut f, touch(3), -100.0);
        }
        release(&mut f, touch(3));
        assert!((offset(&mut f) - 700.0).abs() < 0.01, "{}", offset(&mut f));

        // 回滚到底部同理
        press(&mut f, touch(4));
        for _ in 0..10 {
            drag(&mut f, touch(4), 100.0);
        }
        release(&mut f, touch(4));
        assert_eq!(offset(&mut f), 0.0);
    }

    /// 内容不够长（没有溢出）时怎么滑都不动
    #[test]
    fn short_content_never_scrolls() {
        let mut f = fixture(280.0, 300.0);
        press(&mut f, touch(5));
        for _ in 0..5 {
            drag(&mut f, touch(5), -100.0);
        }
        release(&mut f, touch(5));
        assert_eq!(offset(&mut f), 0.0);
    }

    /// 鼠标拖动不滚动：桌面上的滚轮/鼠标路径必须保持原样
    #[test]
    fn mouse_drag_is_ignored() {
        let mut f = fixture(1000.0, 300.0);
        let mouse = PointerId::Mouse;
        press(&mut f, mouse);
        for _ in 0..5 {
            drag(&mut f, mouse, -100.0);
        }
        release(&mut f, mouse);
        assert_eq!(offset(&mut f), 0.0);
    }

    /// 第二根手指不算滑动：第一根在滑时，后面落下的手指一律忽略
    #[test]
    fn a_second_finger_does_not_take_over() {
        let mut f = fixture(1000.0, 300.0);
        press(&mut f, touch(6));
        drag(&mut f, touch(6), -60.0);
        let after_first = offset(&mut f);
        // 第二根按住并往上推：手指不接力，页面不再动
        press(&mut f, touch(7));
        drag(&mut f, touch(7), -200.0);
        assert!((offset(&mut f) - after_first).abs() < 0.01);
        release(&mut f, touch(7));
        release(&mut f, touch(6));
    }

    /// 松手带速度：惯性继续往下走一小段后停下，不会一路滑到底
    #[test]
    fn release_keeps_momentum_then_stops() {
        let mut f = fixture(5000.0, 300.0);
        press(&mut f, touch(8));
        for _ in 0..4 {
            drag(&mut f, touch(8), -60.0);
        }
        let at_release = offset(&mut f);
        release(&mut f, touch(8));

        for _ in 0..60 {
            step(&mut f.app);
        }
        let settled = offset(&mut f);
        assert!(
            settled > at_release + 40.0,
            "松手后应当再滑一段：{at_release} → {settled}"
        );
        assert!(settled < 4999.0, "不该一路滑到底：{settled}");
        assert!(
            f.app.world().resource::<PageScroll>().momentum.is_none(),
            "一秒后惯性必须收干净"
        );
    }

    /// 滑动期间遮罩必须盖上（否则松手会误点到按下时的那一行），抬起后立刻撤掉
    #[test]
    fn shield_covers_the_page_while_dragging() {
        let mut f = fixture(1000.0, 300.0);
        assert_eq!(shield(&mut f), Display::None);

        press(&mut f, touch(9));
        drag(&mut f, touch(9), -40.0);
        assert_eq!(shield(&mut f), Display::Flex, "拖动中要盖住页面");

        release(&mut f, touch(9));
        eprintln!(
            "DEBUG scroll={:?} momentum={:?}",
            f.app.world().resource::<PageScroll>().drag,
            f.app.world().resource::<PageScroll>().momentum
        );
        assert_eq!(shield(&mut f), Display::None, "抬起后立刻撤掉");
    }

    /// 切页：滑动状态收干净，新页面从顶部开始
    #[test]
    fn page_change_resets_the_scroll() {
        let mut f = fixture(1000.0, 300.0);
        press(&mut f, touch(10));
        drag(&mut f, touch(10), -200.0);
        assert!(offset(&mut f) > 100.0);

        f.app.world_mut().write_message(MenuEvent::PageChanged {
            from: Some("page"),
            to: Some("page"),
        });
        step(&mut f.app);
        assert_eq!(offset(&mut f), 0.0);
        assert!(!f.app.world().resource::<PageScroll>().is_scrolling());
    }
}
