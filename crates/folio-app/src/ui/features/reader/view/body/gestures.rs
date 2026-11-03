//! 正文上的原始触摸、定版式缩放/平移、长按看大图。

use crate::ui::features::reader::components::image_viewer::ImageViewer;
use crate::ui::features::reader::model::session::ReaderSession;
use gpui_kit::{
    AnyElement, App, Entity, IntoElement, LongPressEvent, PinchEvent, Styled, TouchEvent,
    TouchPhase, WeakEntity, Window, canvas,
};

use super::images::open_picture;

/// 把**原始**触摸流记进会话的那块监听。
///
/// 为什么非要这一路：跟手翻页吃的是识别器折出来的 `ScrollEvent`，而它在把一笔定性成
/// 某一轴的那一刻就把另一轴的增量抹成 0（`lock_delta_to_axis`）。于是「起手点 + 累计位移」
/// 反推出来的手指此刻只剩一个自由度，折线转不起来。同一笔的原始触摸是唯一还带着第二个自由
/// 度的地方。
///
/// 只记账，不 `prevent_default`、不停传播：这一格要是把事件认下来，点屏翻页与长按吸词就一起
/// 没了。挂在 `canvas` 的绘制回调里也是同一规矩 —— `window.on_mouse_event` 只活到本帧事件
/// 分发结束，每个绘制帧都要重挂一遍。
pub(super) fn touch_follow(reader: WeakEntity<ReaderSession>) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window: &mut Window, _cx: &mut App| {
            let reader = reader.clone();
            let origin = bounds.origin;
            window.on_mouse_event(move |event: &TouchEvent, phase, _window, cx| {
                // 同一条事件会走 Capture 与 Bubble 两遍，只在 Bubble 那一次记账。
                if !phase.bubble() {
                    return;
                }
                let _ = reader.update(cx, |session, _| {
                    let tracked = session.finger.is_some_and(|(id, _)| id == event.id);
                    match event.phase {
                        // 新的那一根手指只有落在这一栏里才算数；已经认下的那根不受这条约束
                        // —— 拖出页外正是「把整页折走」那一笔会做的事。
                        TouchPhase::Started => {
                            // 起手落在某一叠上：这一笔先按「切图」记账（`swipe_scroll`
                            // 那一头决定它到底算切图还是算滚）。
                            //
                            // 浮层（顶栏底栏）压着的时候不记这一笔：横划切图、以及划到叠的
                            // 尽头那一下交回翻页（`swipe_scroll` 末尾那句 `turn`）都是翻页，
                            // 而工具栏露头之后正文上的输入只表达「收浮层」—— 与
                            // `claim_swipe`、滚轮那两条同一规矩。不认领这一叠，这一笔随后落回
                            // 那两条已经会让位的路。
                            let grab = if session.overlay_open {
                                None
                            } else {
                                session.stack_hit_at(event.position)
                            };
                            session.stack_grab = grab;
                            session.stack_dx = 0.0;
                            session.stack_dy = 0.0;
                            if bounds.contains(&event.position) {
                                let at = session.finger_of(event.position, origin);
                                session.finger = Some((event.id, at));
                            }
                        }
                        TouchPhase::Moved if tracked => {
                            let at = session.finger_of(event.position, origin);
                            session.finger = Some((event.id, at));
                        }
                        TouchPhase::Ended | TouchPhase::Cancelled if tracked => {
                            session.finger = None;
                        }
                        _ => {}
                    }
                });
            });
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// 定版式的双指缩放，以及放大之后的单指平移。
///
/// 挂法与 [`touch_follow`] 同一条：`window.on_mouse_event` 只活到本帧事件分发结束，
/// 每个绘制帧都要在 `canvas` 的绘制回调里重挂一遍。
///
/// 两路事件分工不同的理由：
///
/// - **捏合**吃 `PinchEvent`。`gpui-mobile` 的 `PinchGuard` 在平台层就把第二根手指折成了
///   它，并且把这几根手指的单点流一起吞掉 —— 所以捏合不会顺手翻一页。
/// - **拖动**吃原始 `TouchEvent`。识别器在把一笔定性成某一轴的那一刻就把另一轴的增量
///   抹成 0，而平移一张放大过的纸两个轴都要；原始触摸是唯一还带着两个自由度的地方。
///
/// 两路都**不** `prevent_default`：这一趟也可能只是一次点击（点屏幕左右翻页），
/// 在起手那一刻认领，翻页就没了。识别器那边折出来的滚动在 `on_scroll` 里让掉即可。
pub(super) fn fixed_gestures(reader: WeakEntity<ReaderSession>) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window: &mut Window, _cx: &mut App| {
            let origin = bounds.origin;
            let pinch_reader = reader.clone();
            let pan_reader = reader;
            window.on_mouse_event(move |event: &PinchEvent, phase, _window, cx| {
                // 同一条事件会走 Capture 与 Bubble 两遍，只记 Bubble 那一次。
                if !phase.bubble() {
                    return;
                }
                let focal = (
                    f32::from(event.position.x) - f32::from(origin.x),
                    f32::from(event.position.y) - f32::from(origin.y),
                );
                let _ = pinch_reader.update(cx, |session, cx| {
                    session.pinch_fixed(event.delta, focal, cx);
                });
            });

            let inside = bounds;
            window.on_mouse_event(move |event: &TouchEvent, phase, _window, cx| {
                if !phase.bubble() {
                    return;
                }
                let at = (
                    f32::from(event.position.x) - f32::from(origin.x),
                    f32::from(event.position.y) - f32::from(origin.y),
                );
                let _ = pan_reader.update(cx, |session, cx| match event.phase {
                    TouchPhase::Started => {
                        session.fixed_pan_start(event.id, at, inside.contains(&event.position));
                    }
                    TouchPhase::Moved => {
                        session.fixed_pan_move(event.id, at, cx);
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => session.fixed_pan_end(event.id),
                });
            });
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// 「按住这一页」的监听，挂在 `canvas` 的绘制回调里：`window.on_mouse_event` 只活到本帧
/// 事件分发结束，每个绘制帧都要重挂一遍（与 [`crate::ui::features::reader::view::paint`] 里选中那几条同一规矩）。
///
/// `prevent_default` 就是**认领**这一手势：gpui 拿 `default_prevented` 当「这一手势被
/// 接走了」的依据（`window.rs` 里 `resolve_long_press(claimed)`）。不认领的话手指抬起时
/// 会补出一个合成点击，而那一击的意思正是翻页 —— 按住看大图会变成「翻一页再看大图」。
pub(super) fn long_press_to_open(
    viewer: Entity<ImageViewer>,
    reader: WeakEntity<ReaderSession>,
    key: String,
) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window: &mut Window, _cx: &mut App| {
            let viewer = viewer.clone();
            let reader = reader.clone();
            window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
                if !phase.bubble() || event.phase != TouchPhase::Started {
                    return;
                }
                // 这一格只负责自己那一页：抬起之前手指挪出图外就不算「按住这张图」。
                if !bounds.contains(&event.start_position) {
                    return;
                }
                open_picture(&viewer, &reader, &key, window, cx);
                window.prevent_default();
                cx.stop_propagation();
            });
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}
