//! 导航：摆法（连续 / 分页 / 双页）、翻一屏、跳到某个比例、以及把滚动增量落到正文。
//!
//! 这些是**纯状态迁移**：给定当前模式与视口，动 `scroll_y` / `page` 并顺手落库。
//! 位置本身存在 [`super::super::session::ReaderSession`] 上，方法的可见性靠 `pub(super)`。

use folio_core::Locator;
use gpui_kit::{Context, ScrollDelta, ScrollWheelEvent};

use super::super::chrome::PageMode;
use super::super::session::ReaderSession;

impl ReaderSession {
    /// 换成另一种摆法。位置**不丢**：进度存的是「第几个字」，所以先把当前字偏移取下来，
    /// 等下一帧按它在新模式里落位。落库的模式名跟着走。
    pub fn set_mode(&mut self, mode: PageMode, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        let offset = self.current_offset().unwrap_or_default();
        self.pending_restore = (offset > 0).then_some(Locator {
            chapter: self.chapter_index() as u32,
            offset,
        });
        self.scroll_y = 0.0;
        self.page = 0;
        self.wheel_accum = 0.0;
        cx.notify();
    }

    /// 按「一屏」翻正文。
    ///
    /// 连续滚动走 90% 而不是 100%：整屏跳会让上一屏的末行彻底消失，读者找不到接续的那句。
    /// 分页 / 双页是一页一号，跳半页在这里没有意义。
    pub fn scroll_page(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.laid_out.mode.is_continuous() {
            let page = self.laid_out.viewport * 0.9;
            self.set_scroll_y(self.scroll_y + delta as f32 * page, cx);
            return;
        }
        self.set_page(self.page.saturating_add_signed(delta), cx);
    }

    /// 跳到本章的某个比例处（进度条）。
    pub fn seek_ratio(&mut self, ratio: f32, cx: &mut Context<Self>) {
        if self.laid_out.mode.is_continuous() {
            let target = ratio.clamp(0.0, 1.0) * self.max_scroll();
            self.set_scroll_y(target, cx);
        } else {
            let last = self.laid_out.pages.saturating_sub(1);
            self.set_page((ratio.clamp(0.0, 1.0) * last as f32).round() as usize, cx);
        }
    }

    /// 两条滚动入口共用的落点：夹进范围、落库、重画。
    pub(super) fn set_scroll_y(&mut self, target: f32, cx: &mut Context<Self>) {
        let clamped = target.clamp(0.0, self.max_scroll());
        if (clamped - self.scroll_y).abs() < 0.5 {
            return;
        }
        self.scroll_y = clamped;
        self.persist_if_moved();
        cx.notify();
    }

    /// 翻到第 `target` 页（越界夹到最后一页）。页号变了才落库、才重画。
    pub(super) fn set_page(&mut self, target: usize, cx: &mut Context<Self>) {
        let target = target.min(self.laid_out.pages.saturating_sub(1));
        if target == self.page {
            return;
        }
        self.page = target;
        self.wheel_accum = 0.0;
        self.persist_if_moved();
        cx.notify();
    }

    /// 平台层算好的滚动增量落到正文。
    ///
    /// 触摸向下滑（dy > 0）时内容跟着手指往下走 ⇒ 露出的行号变小，所以是减。
    pub(in crate::ui::features::reader) fn on_scroll(&mut self, event: &ScrollWheelEvent) {
        let dy = match event.delta {
            ScrollDelta::Pixels(delta) => f32::from(delta.y),
            // 鼠标滚轮（外接键盘/调试时）按「行」给增量，按正文字号折算，别当 0 忽略。
            ScrollDelta::Lines(lines) => lines.y * self.prefs.font_size,
        };
        if dy == 0.0 {
            return;
        }
        if self.laid_out.mode.is_continuous() {
            let max = self.max_scroll();
            self.scroll_y = (self.scroll_y - dy).clamp(0.0, max);
        } else {
            // 分页模式下滚轮不「滚」，它翻页：攒够半屏翻一页，方向与连续滚动一致。
            self.wheel_accum -= dy;
            let threshold = (self.laid_out.viewport * 0.5).max(1.0);
            while self.wheel_accum.abs() >= threshold {
                let step = if self.wheel_accum > 0.0 { 1 } else { -1 };
                self.wheel_accum -= step as f32 * threshold;
                let last = self.laid_out.pages.saturating_sub(1) as isize;
                self.page = (self.page as isize + step).clamp(0, last) as usize;
            }
        }
        self.persist_if_moved();
    }
}
