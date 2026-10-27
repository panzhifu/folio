//! 书架滚动帧探针（临时）。
//!
//! 只为回答一件事：滑动书架时，帧到底是**多久一帧**、`render` 占掉多少。出不出声由
//! `logging.targets = ["folio_app::perf=debug"]` 决定，与阅读页那支探针同一个开关
//! （见 `reader::model::perf` 的模块头）。
//!
//! 两条线各记各的：
//!
//! - **paint**：元素树每画一帧，挂在书架根元素最后那个 `canvas` 的绘制回调里。两条
//!   paint 之间的间隔就是读者感觉到的帧间隔 —— 它涨到 33 ms 就是掉到 30 帧。
//! - **render**：`ShelfView::render` 自己的耗时（建格子、建卡片、算尺寸）。
//!
//! 不逐帧写日志：`LogFile` 是同步写，逐帧一条会把要量的东西自己改掉。攒够
//! [`REPORT_EVERY`] 帧报一条汇总。

use std::cell::RefCell;
use std::time::{Duration, Instant};

/// 攒多少帧报一条。
const REPORT_EVERY: usize = 60;

#[derive(Default)]
struct State {
    last_paint: Option<Instant>,
    paints: Vec<f32>,
    renders: Vec<f32>,
    /// 这一帧里列表闭包造了几排（>0 = 有新的排进了视口）。
    items: u32,
    /// 这一帧里列表闭包一共花了多久（微秒）。它在 `ShelfView::render` 之外，
    /// 只有单独记才知道那几排的构造是不是帧预算里的大头。
    build_us: u64,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn enabled() -> bool {
    tracing::enabled!(target: "folio_app::perf", tracing::Level::DEBUG)
}

fn percentiles(values: &mut [f32]) -> (f32, f32, f32) {
    values.sort_by(|a, b| a.total_cmp(b));
    let at = |q: f32| {
        let ix = ((values.len() - 1) as f32 * q).round() as usize;
        values[ix]
    };
    (at(0.5), at(0.9), *values.last().unwrap_or(&0.0))
}

/// 元素树画完这一帧（由书架根元素最后那块 `canvas` 调用）。
pub(in crate::ui::features::shelf) fn note_paint() {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let items = std::mem::take(&mut state.items);
        let build_us = std::mem::take(&mut state.build_us);
        if let Some(last) = state.last_paint.replace(now) {
            let interval = now.saturating_duration_since(last).as_secs_f32() * 1_000.0;
            // 超过 25 ms 的帧单独记一条：它是不是恰好落在「有新的排进视口」那一帧，
            // 决定这笔钱该记在视口换血上还是记在别处。
            if interval > 25.0 {
                tracing::debug!(
                    target: "folio_app::perf",
                    interval_ms = interval,
                    items,
                    build_us,
                    "书架慢帧"
                );
            }
            state.paints.push(interval);
        }
        if state.paints.len() >= REPORT_EVERY {
            let (p50, p90, max) = percentiles(&mut state.paints);
            let (r50, r90, rmax) = percentiles(&mut state.renders);
            tracing::debug!(
                target: "folio_app::perf",
                frames = REPORT_EVERY,
                paint_ms = ?(p50, p90, max),
                render_ms = ?(r50, r90, rmax),
                "书架滚动帧"
            );
            state.paints.clear();
            state.renders.clear();
        }
    });
}

/// 列表闭包造了一排，以及这一排花了多久。
pub(in crate::ui::features::shelf) fn note_item(elapsed: Duration) {
    if !enabled() {
        return;
    }
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.items += 1;
        state.build_us += elapsed.as_micros() as u64;
    });
}

/// `ShelfView::render` 这一趟花了多久。
pub(in crate::ui::features::shelf) fn note_render(elapsed: Duration) {
    if !enabled() {
        return;
    }
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.renders.push(elapsed.as_secs_f32() * 1_000.0);
    });
}
