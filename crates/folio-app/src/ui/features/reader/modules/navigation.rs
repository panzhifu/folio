//! 导航：摆法（连续 / 分页 / 双页）、翻一屏、跳到某个比例、以及把滚动增量落到正文。
//!
//! 这些是**纯状态迁移**：给定当前模式与视口，动 `scroll_y` / `page` 并顺手落库。
//! 位置本身存在 [`super::super::session::ReaderSession`] 上，方法的可见性靠 `pub(super)`。

use folio_core::Locator;
use gpui_kit::{Context, ScrollDelta, ScrollWheelEvent};

use super::super::book::Book;
use super::super::chrome::{Arrow, PageMode};
use super::super::session::{PageRef, ReaderSession, Source, Turn};
use super::super::turn::{PagingFacts, TurnIntent, TurnPlan};

impl ReaderSession {
    /// 换成另一种摆法。位置**不丢**：进度存的是「第几个字」，所以先把当前字偏移取下来，
    /// 等下一帧按它在新模式里落位。落库的模式名跟着走。
    pub fn set_mode(&mut self, mode: PageMode, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        // 这一下是读者选的：默认档那面旗落下，之后落库就写他选的这一档。
        self.mode_undecided = false;
        // 摆法换了，同一句话会挪到另一处坐标位置；选中跟着字走虽不至于指错，
        // 但「换摆法」在读者心里是一次新动作，留着一个旧高亮只会碍事。
        self.clear_selection();
        // 连续模式的落点是**全局 y**（章基线 + 章内行顶），基线未必是 0 ——
        // 上一章可能正拼在上方，所以哪怕偏移是 0 也挂恢复请求，让渲染帧把它
        // 摆到正确的那一像素。分页落到页号，偏移 0 就是第 0 页，不需要请求。
        let (chapter, offset) = self.resolve_position().unwrap_or((self.chapter_index(), 0));
        self.pend_restore((mode.is_continuous() || offset > 0).then_some(Locator {
            chapter: chapter as u32,
            offset,
        }));
        self.scroll_y = 0.0;
        self.page = 0;
        self.wheel_accum = 0.0;
        // 摆法换了，动画里那张出场页属于旧摆法（旧片高、旧栏数），让它继续平移只会
        // 把两张对不上的纸叠在屏幕上。
        self.cancel_turn();
        cx.notify();
    }

    /// 按「一屏」翻正文。这一趟**会**播动画（分页沿当前轴平移两张纸，连续滚动补间
    /// 画出来的那个 y），因为这一句的来路都是「一次离散动作」：点屏幕左右两边、
    /// 音量键。滚轮与触摸拖动走 [`ReaderSession::on_scroll`]，那一趟是手在直接控制，
    /// 不该有东西追在手指后面补间。
    ///
    /// 连续滚动走 90% 而不是 100%：整屏跳会让上一屏的末行彻底消失，读者找不到接续的那句。
    /// 分页 / 双页是一页一号，跳半页在这里没有意义。
    /// 方向键翻页（PC）。返回 `false` = 这一键不归正文，调用方该放手。
    ///
    /// 左右两颗按整本的翻页方向换手（判据在 [`Arrow::turn`]，与点屏幕左右分区同一个
    /// 口径），上下不换。文字输入框到不了这里：焦点是文字输入时 gpui **根本不派发键绑定**
    /// （`InputPreference::CharacterInput`，见 `Window::dispatch_key_event`）。
    ///
    /// 图片查看器开着时这一层让开：那一层自己的 `on_key_down` 比键绑定后跑，这里不放手
    /// 就轮不到它 —— 而「左右 = 上一张 / 下一张」正是那一层的功能。
    pub fn arrow_turn(&mut self, arrow: Arrow, cx: &mut Context<Self>) -> bool {
        if self
            .image_viewer
            .as_ref()
            .is_some_and(|viewer| viewer.read(cx).is_open())
        {
            return false;
        }
        let delta = arrow.turn(self.progression_rtl());
        if let Some(intent) = TurnIntent::from_delta(delta) {
            self.turn(intent, cx);
        }
        true
    }

    /// 兼容旧入口：方向键/点击分区/音量键最终都转成 [`TurnIntent`] 再走 [`Self::turn`]。
    /// 保留这个按 `isize` 取方向的口子，是因为宿主测试与页面事件都按「±1 屏」说话。
    pub fn scroll_page(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(intent) = TurnIntent::from_delta(delta) {
            self.turn(intent, cx);
        }
    }

    /// 一次离散翻页的唯一入口。
    ///
    /// 决策在 [`super::super::turn`] 里（连续 / 分页 / 双页 + 章界），这里只负责把
    /// `TurnPlan` 落成状态与动画。方向键、点击分区、音量键都从这里过；滚轮章界也会。
    pub(in crate::ui::features::reader) fn turn(
        &mut self,
        intent: TurnIntent,
        cx: &mut Context<Self>,
    ) {
        // 章界已在等排版或正在滑：再按也只有一个去向，重开一趟只会把动画打断。
        if self.chapter_turn_in_flight() {
            return;
        }
        let facts = self.paging_facts();
        match self.laid_out.mode.paging().plan(&facts, intent) {
            TurnPlan::None => {}
            TurnPlan::Scroll { to } => {
                // 起点是**画出来的**那一个数而不是 `scroll_y`：读者连点两下时，第二趟
                // 要从他眼前那一屏起步，否则画面先跳回上一趟的终点再走，读成卡了一下。
                self.begin_turn(Turn {
                    seq: 0,
                    leaving: None,
                    arriving: None,
                    scroll: Some((self.scroll_painted, to)),
                    forward: intent == TurnIntent::Next,
                });
                self.set_scroll_y(to, cx);
            }
            TurnPlan::Page { to } => {
                let before = self.page();
                self.set_page(to, cx);
                // 定版式那一臂不播：它的「一页」是一整张图，摆图那条代码路与正文的切片
                // 是两套（`view.rs` 的 `Source::Fixed` 臂），平移要在那边另接一处。
                if !self.is_fixed() && self.page() != before {
                    let chapter = self.chapter_index();
                    self.begin_turn(Turn {
                        seq: 0,
                        leaving: Some(PageRef {
                            chapter,
                            page: before,
                        }),
                        arriving: Some(PageRef {
                            chapter,
                            page: self.page(),
                        }),
                        scroll: None,
                        forward: intent == TurnIntent::Next,
                    });
                }
            }
            TurnPlan::Chapter { delta } => self.begin_chapter_turn(delta, cx),
        }
    }

    /// 决策层要的那份事实快照。不借 `self` 进策略：策略只认这些数。
    fn paging_facts(&self) -> PagingFacts {
        let fixed = self.is_fixed();
        let continuous = self.laid_out.mode.is_continuous();
        let ready = if fixed {
            true
        } else if let Some(book) = self.book() {
            // 分页还要等切片；连续滚动只要正文排版在手就能滚。
            book.document().is_current() && (continuous || self.laid_out.pages > 0)
        } else {
            false
        };
        PagingFacts {
            page: self.page,
            pages: self.laid_out.pages,
            scroll: self.scroll_y,
            max_scroll: self.max_scroll(),
            viewport: self.laid_out.viewport,
            can_prev_chapter: !fixed && self.can_advance(-1),
            can_next_chapter: !fixed && self.can_advance(1),
            ready,
            fixed,
        }
    }

    /// 章界那一下：先把目标章放上「等排版」的位子，等渲染帧把它的邻章
    /// `ensure_viewport` 做完，再起跨章滑动。目标章已预取时，下一帧就开滑。
    fn begin_chapter_turn(&mut self, delta: isize, cx: &mut Context<Self>) {
        if delta == 0 {
            return;
        }
        self.cancel_turn();
        self.clear_selection();
        self.selection_menu_open = false;
        self.wheel_accum = 0.0;
        self.pending_chapter_turn = Some(delta.signum());
        cx.notify();
    }

    /// 跨章滑动落地：把目标章换成锚章，页号落到它的第一页 / 最后一页。
    ///
    /// 只在动画走到 1.0 的那一帧调用；此时目标章的排版与切片已经在 `spans` 里备好，
    /// `set_anchor` 会把它摘下来复用，不会再排一遍。
    pub(in crate::ui::features::reader) fn finish_chapter_turn(&mut self, to: PageRef) {
        if to.chapter == self.chapter_index() {
            return;
        }
        let columns = self.laid_out.columns.max(1);
        let prefs = self.prefs.clone();
        let (pages, offset) = {
            let Source::Book(book) = &mut self.source else {
                return;
            };
            book.set_anchor(to.chapter, &prefs);
            let pages = book.document().page_count().max(1);
            let page = to.page.min(pages.saturating_sub(1));
            let offset = book.document().offset_of_page(page, columns);
            (pages, offset)
        };
        self.page = to.page.min(pages.saturating_sub(1));
        self.laid_out.pages = pages;
        self.scroll_y = 0.0;
        self.scroll_painted = 0.0;
        self.content_height = 0.0;
        self.wheel_accum = 0.0;
        self.page_shift = (0.0, 0.0);
        self.turn = None;
        self.pending_chapter_turn = None;
        self.clear_selection();
        self.persist_locator(Locator {
            chapter: to.chapter as u32,
            offset,
        });
    }

    /// 起一次翻页动画，把上一趟还没走完的那次**换掉**而不是排队：读者的第二下点击要看见
    /// 新的一趟在走，而两张以上的纸同时在屏幕上位移没有意义（出场页只留最近一张）。
    fn begin_turn(&mut self, turn: Turn) {
        self.turn_seq += 1;
        self.turn = Some(Turn {
            seq: self.turn_seq,
            ..turn
        });
    }

    /// 作废正在走的动画，画面当场落定。跳章、换摆法、改字号、排版恢复落地都走这一句 ——
    /// 它们的共同点是**位置本身换了参照系**，让一张按旧参照系摆出来的纸继续平移没有意义。
    pub(in crate::ui::features::reader) fn cancel_turn(&mut self) {
        self.turn = None;
        self.pending_chapter_turn = None;
    }

    /// 章界正在等排版或正在跨章滑动：这时正文上的点按/长按/拖选一律让位。
    /// 两个章落在同一屏上，命中坐标只可能属于其中一套；让位比猜错强。
    pub(in crate::ui::features::reader) fn chapter_turn_in_flight(&self) -> bool {
        self.pending_chapter_turn.is_some()
            || self
                .turn
                .and_then(|turn| turn.arriving)
                .is_some_and(|to| to.chapter != self.chapter_index())
    }

    /// 跳到本章的某个比例处（进度条）。
    ///
    /// 连续模式里进度条是**视口顶那一章**的章内刻度（与 `progress()` 同一口径），
    /// 拖到 1.0 就是把这一章的末屏摆上来。
    pub fn seek_ratio(&mut self, ratio: f32, cx: &mut Context<Self>) {
        // 进度条那一拖的意思是「跳到哪儿」，不是「翻一页」：位移可以大到跨越整章，
        // 播一张出场页的平移只会把读者甩进一段他没法预期的滑行里。
        self.cancel_turn();
        if self.is_fixed() {
            let last = self.page_count().saturating_sub(1);
            self.set_page((ratio.clamp(0.0, 1.0) * last as f32).round() as usize, cx);
            return;
        }
        if self.laid_out.mode.is_continuous() {
            let target = {
                let Some(book) = self.book() else {
                    return;
                };
                let gap = Book::chapter_gap(&self.prefs);
                let chapter = book
                    .chapter_at_y(self.scroll_y, gap)
                    .unwrap_or_else(|| book.chapter_index());
                let base = book.base_of(chapter, gap).unwrap_or(0.0);
                let height = book
                    .span_doc(chapter)
                    .map(|doc| doc.height())
                    .unwrap_or(0.0);
                let local_max = (height - self.laid_out.viewport).max(0.0);
                base + ratio.clamp(0.0, 1.0) * local_max
            };
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
    /// 翻走即清选中：高亮还指得上字，但被翻走的内容留在看不见的那一页上没有意义。
    pub(in crate::ui::features::reader) fn set_page(
        &mut self,
        target: usize,
        cx: &mut Context<Self>,
    ) {
        // 定版式：页号由页表持有，翻页即改它（进度、恢复都按 `locator.chapter = 页号`）。
        let fixed = self.fixed_mut().map(|book| {
            let before = book.page();
            book.set_page(target);
            (before, book.page(), book.page_count())
        });
        if let Some((before, now, count)) = fixed {
            if now == before {
                return;
            }
            self.page = now;
            self.laid_out.pages = count;
            self.clear_selection();
            self.wheel_accum = 0.0;
            self.persist_if_moved();
            cx.notify();
            return;
        }
        let target = target.min(self.laid_out.pages.saturating_sub(1));
        if target == self.page {
            return;
        }
        self.page = target;
        self.clear_selection();
        self.wheel_accum = 0.0;
        self.persist_if_moved();
        cx.notify();
    }

    /// 平台层算好的滚动增量落到正文。返回「这一下真的挪动了吗」——
    /// 调用方拿它决定要不要重画（见 `view.rs` 的滚轮监听）。
    ///
    /// 触摸向下滑（dy > 0）时内容跟着手指往下走 ⇒ 露出的行号变小，所以是减。
    pub(in crate::ui::features::reader) fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        // 手指还按在屏幕上这件事，即使内容已被夹在章尾、一个字都没挪，也是「在读」。
        // 所以这一条不只靠 `persist_locator` 那个证据点（它要求位置真的变过）。
        self.note_reading_activity();
        // 帧间隔归谁的那道判据之一：滚动事件到了几个（与「正文画了几次」「会话出了几次
        // 元素树」三条一起看，见 `perf.rs` 模块头）。挂在最前面，因为它要数的是**事件**，
        // 而下面那几条 `return false` 恰恰是「事件到了但没挪动」。
        super::super::perf::note_scroll();
        // 跨章滑动（或等目标章排版）期间滚轮不参与：这一趟的坐标属于两章，
        // 再叠一次页码会把两张纸的方向搅乱。等它落地再滚。
        if self.chapter_turn_in_flight() {
            return false;
        }
        // 手已经在直接控制这一屏了，补间不该追在手指后面：正在走的那一趟当场落定。
        // 落定之后画出来的位置由渲染帧按「没有动画」那一条重新对齐到 `scroll_y`。
        self.cancel_turn();
        let dy = match event.delta {
            ScrollDelta::Pixels(delta) => f32::from(delta.y),
            // 鼠标滚轮（外接键盘/调试时）按「行」给增量，按正文字号折算，别当 0 忽略。
            ScrollDelta::Lines(lines) => lines.y * self.prefs.font_size,
        };
        if dy == 0.0 {
            return false;
        }
        // 位置有没有真的变过：滚到章尾被夹住、分页攒不够半屏都不该重画。
        // 这条判据与 gpui 自己的 `div` 滚动容器、zed editor 的 `scroll` 同源
        // （位置没变就不 notify），省下的是一整帧的场景重建。
        let before = (self.scroll_y, self.page);
        let mut boundary = None;
        if !self.is_fixed() && self.laid_out.mode.is_continuous() {
            let max = self.max_scroll();
            self.scroll_y = (self.scroll_y - dy).clamp(0.0, max);
        } else {
            // 分页 / 双页 / 定版式：滚轮不「滚」，它翻页：攒够半屏翻一页，方向与连续滚动一致。
            self.wheel_accum -= dy;
            let threshold = (self.laid_out.viewport * 0.5).max(1.0);
            let last = self.page_count().saturating_sub(1) as isize;
            let before = self.page() as isize;
            let mut page = before;
            while self.wheel_accum.abs() >= threshold {
                let step = if self.wheel_accum > 0.0 { 1 } else { -1 };
                self.wheel_accum -= step as f32 * threshold;
                let next = page + step;
                if next < 0 || next > last {
                    // 章界：这一格攒出的那一页交给跨章滑动，不再夹在 `0..last` 里。
                    boundary = (!self.is_fixed()).then_some(step);
                    break;
                }
                page = next;
            }
            // 翻走即清选中（与 `set_page` 同一条规矩；这里直接动页号，绕过了它）。
            if page != before {
                if let Some(fixed) = self.fixed_mut() {
                    fixed.set_page(page as usize);
                }
                self.page = page as usize;
                self.clear_selection();
            }
            if let Some(step) = boundary {
                self.begin_chapter_turn(step, cx);
            }
        }
        self.persist_if_moved();
        boundary.is_some() || (self.scroll_y, self.page) != before
    }
}
