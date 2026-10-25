//! 听书（合成朗读）的会话侧：播放状态机、逐段推进与进度落库。
//!
//! 分工（与引擎侧 `folio_core::speech` 的模块文档对读）：引擎线程只回答「怎么念」，
//! 这里回答「念什么、念到哪儿、什么时候停」。设计稿在本机 `plans/听书.md`（不入库），
//! 几条落进实现的口径：
//!
//! - **段，不是句。** 一段 = `hit::paragraph_range` 给出的字符区间（排版已经算过的
//!   事实），一段一条 utterance，`Done` 里推下一段，不做预排 —— 「读者手动翻页时
//!   队列里那三段怎么办」这个分支整个不存在。
//! - **听的位置与读的位置是同一个东西。** `auto_scroll` 开着时每开一段
//!   `jump_to_offset` 把那一屏摆过来（它自带落库），停听时再按段起点补一账
//!   —— `positions` 表里从此只有一个答案。不新开表。
//! - **停不依赖回调。** 引擎侧的静音窗（`speech::gate::reduce`）负责吞掉 Stop 之后
//!   的回调；这一侧的另一半是：`Done` 只在 `Playing` 里推进，暂停/停止之后撞到的
//!   尾巴在这里被挡住。两边各有一条测试钉着。
//! - **听就是读。** 每段 `Done` 记一次 `note_reading_activity`，阅读时长的宽限线
//!   照常接住，不新写计时。
//!
//! 手动翻页/滚动**不**暂停听书：播放中的位置归听书管（下一段开念时 `auto_scroll`
//! 会把屏摆回念的那段），读者要自由浏览就先按一下暂停 —— 可预期，且不需要在
//! 每个输入口各插一刀。

use std::ops::Range;
use std::time::Duration;

use folio_core::speech::{Command, Event};
use folio_core::{Engine, Locator};
use gpui_kit::{Context, Task};

use crate::ui::features::reader::model::chapter_layout::Document;
use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::services::config::ConfigHolder;

/// 播放状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::ui::features::reader) enum Listen {
    /// 可用但没在念。引擎不存在时也停在这一态 —— 入口摆不摆看
    /// [`ReaderSession::speech_entry_visible`]，不看这里。
    Idle,
    /// 正在念哪一章的哪一段。这一段同时是给视图画整段高亮的全部信息。
    Playing { chapter: usize, range: Range<usize> },
    /// 停在了哪一段。恢复 = 从这段开头再念（引擎没有段内暂停，见引擎模块文档）。
    Paused { chapter: usize, range: Range<usize> },
}

/// 会话侧的听书状态。字段打进 [`ReaderSession`]（一个实体、多个 `impl` 块）。
pub(in crate::ui::features::reader) struct Speech {
    /// 引擎句柄。`None` = 没挂上（还没开过书）或探测失败。
    engine: Option<Engine>,
    /// 引擎的 `Ready` 到了没有。`None` = 还在探测（入口先不摆，免得闪一下）；
    /// `Some(false)` = 这台设备没有可用的引擎（Linux 没有 speechd / stub 平台）。
    available: Option<bool>,
    state: Listen,
    /// 跨章续读的接力棒：`advance_chapter` 之后新章还没排完，这一格挂着
    /// 「排版落地就念这一章的第一段」。轮询见 [`Self::wait_for_chapter_then_speak`]。
    pending_chapter_start: bool,
    /// 事件接收任务。开书时挂一次；会话被扔掉时随字段 drop（任务取消）。
    listening: Option<Task<()>>,
}

impl Default for Speech {
    fn default() -> Self {
        Self {
            engine: None,
            available: None,
            state: Listen::Idle,
            pending_chapter_start: false,
            listening: None,
        }
    }
}

impl ReaderSession {
    /// 挂引擎、起事件循环。开书时调（`open_book`），幂等 —— 引擎进程级只有一份，
    /// 每个会话挂的是自己的事件循环。
    pub(in crate::ui::features::reader) fn speech_attach(&mut self, cx: &mut Context<Self>) {
        if self.speech.listening.is_some() {
            return;
        }
        // 定版式书没有可念的排版链（段落区间那一套不成立），这一格不摆。
        // `available` 给 `Some(false)` 而不是 `None`：别让读者等一场注定不来的探测。
        if self.is_fixed() {
            self.speech.available = Some(false);
            return;
        }
        let Some(engine) = folio_core::speech::engine() else {
            // stub 平台，或这个进程里探测已经失败过（进程内缓存）。
            self.speech.available = Some(false);
            return;
        };
        self.speech.engine = Some(engine.clone());
        let rx = engine.events();
        let weak = cx.entity().downgrade();
        self.speech.listening = Some(cx.spawn(async move |_, cx| {
            // 事件管道是进程级的一条：上一个会话可能留了尾巴，先抽干，
            // 别把上一本书的 `Done` 当成这一本的开场。
            while rx.try_recv().is_ok() {}
            while let Ok(event) = rx.recv().await {
                // 会话没了（阅读页被弹掉）就退出：这是这条循环唯一的正常出口。
                let Ok(()) = weak.update(cx, |session, cx| session.on_speech_event(event, cx))
                else {
                    return;
                };
            }
        }));
        cx.notify();
    }

    /// 入口摆不摆。引擎不可用、没有书（兜底那一屏）都不摆 —— 按「拿不到引擎」设计，
    /// 不弹错误，这与「这台设备根本没有这一格」共用同一条判据。
    pub fn speech_entry_visible(&self) -> bool {
        self.speech.available == Some(true) && self.book().is_some()
    }

    /// 正在播放吗。入口的图标切换与「这一格现在是什么」都问它。
    pub fn speech_playing(&self) -> bool {
        matches!(self.speech.state, Listen::Playing { .. })
    }

    /// 现在念到（或停在）哪一段：`(章, 字符区间)`。视图拿它画整段高亮。
    pub(in crate::ui::features::reader) fn speech_range(&self) -> Option<(usize, Range<usize>)> {
        match &self.speech.state {
            Listen::Playing { chapter, range } | Listen::Paused { chapter, range } => {
                Some((*chapter, range.clone()))
            }
            Listen::Idle => None,
        }
    }

    /// 入口那一下：没念 → 从视口当前段开始念；念着 → 暂停；暂停 → 这一段从头再念。
    pub fn toggle_speech(&mut self, cx: &mut Context<Self>) {
        if !self.speech_entry_visible() {
            return;
        }
        match self.speech.state.clone() {
            Listen::Idle => self.start_speech(cx),
            Listen::Playing { .. } => self.pause_speech(),
            Listen::Paused { chapter, range } => self.speak_paragraph(chapter, range, cx),
        }
    }

    /// 停：离开阅读页（`Drop`）与换书（`open_book`）都走它。位置按当前段起点落库
    /// —— 「听到哪儿」从此在 `positions` 里有数，与读书共用一个坐标系。
    pub(in crate::ui::features::reader) fn stop_speech(&mut self) {
        self.speech.pending_chapter_start = false;
        if let Listen::Playing { chapter, range } | Listen::Paused { chapter, range } =
            &self.speech.state
        {
            if let Some(engine) = &self.speech.engine {
                engine.send(Command::Stop);
            }
            self.persist_locator(Locator {
                chapter: *chapter as u32,
                offset: range.start,
            });
        }
        self.speech.state = Listen::Idle;
    }

    /// 引擎事件喂进来的那一步。公开给事件循环与测试 —— 测试直接喂事件，
    /// 不需要一台真引擎。
    pub(in crate::ui::features::reader) fn on_speech_event(
        &mut self,
        event: Event,
        cx: &mut Context<Self>,
    ) {
        match event {
            Event::Ready(ok) => {
                self.speech.available = Some(ok);
                if !ok {
                    self.speech.engine = None;
                }
                cx.notify();
            }
            // 段内逐句高亮按时钟估的那条路线，设计稿明写第一版不做，Begin 先不用。
            Event::Begin => {}
            Event::Done => {
                // 停不依赖回调的另一半：只有 Playing 才推进。暂停/停止之后撞到的
                // 尾巴（竞态、或直接喂进来的）在这里被挡住。
                let Listen::Playing { chapter, range } = self.speech.state.clone() else {
                    return;
                };
                self.note_reading_activity();
                self.advance_from(chapter, range, cx);
            }
        }
    }

    fn start_speech(&mut self, cx: &mut Context<Self>) {
        // 从读者此刻看着的地方开口：视口顶所在的那一段（含头）。
        let Some((chapter, offset)) = self.resolve_position() else {
            return;
        };
        let Some(book) = self.book() else {
            return;
        };
        let found = book
            .span_doc(chapter)
            .and_then(|doc| paragraph_from(doc, offset));
        let Some(range) = found else {
            return;
        };
        self.speak_paragraph(chapter, range, cx);
    }

    fn pause_speech(&mut self) {
        if let Listen::Playing { chapter, range } = self.speech.state.clone() {
            if let Some(engine) = &self.speech.engine {
                engine.send(Command::Stop);
            }
            self.speech.state = Listen::Paused { chapter, range };
            // 暂停不落库：位置那一行跟着 `auto_scroll` 的逐段跳转已经在了，
            // 而停听（`stop_speech`）才是设计稿里「落库」的那一刻。
        }
    }

    /// 念一段。`auto_scroll` 开着就把那一屏摆过来：同一章里是就地滚到那一段，
    /// 跨了章就换锚章（走目录跳转那条落点）。语速/音高每段随发 —— 设置页改了配置，
    /// 下一段自然生效，不需要另开一条命令通路。
    fn speak_paragraph(&mut self, chapter: usize, range: Range<usize>, cx: &mut Context<Self>) {
        if ConfigHolder::speech(cx).auto_scroll {
            if self.chapter_index() == chapter {
                self.jump_to_offset(range.start, cx);
            } else {
                // 锚章已经不在这一章了：跨章那一帧里恢复请求与锚章迁移可能把书留在
                // 上一章。从前这里**直接不摆屏**（判据是 `chapter_index() == chapter`），
                // 于是音频往前念、画面留在前一章，读者听到/看到的就是「跨章时往回跳」。
                // 走目录跳转同一条落点：换锚章 → 挂恢复请求 → 落库，位置只有一个真相。
                self.goto_offset(chapter, range.start, cx);
            }
        }
        let text = self
            .book()
            .and_then(|book| book.span_doc(chapter))
            .map(|doc| doc.text_map().slice(range.clone()));
        self.speech.state = Listen::Playing { chapter, range };
        if let (Some(engine), Some(text)) = (&self.speech.engine, text) {
            let settings = ConfigHolder::speech(cx);
            engine.send(Command::SetRate(settings.rate));
            engine.send(Command::SetPitch(settings.pitch));
            engine.send(Command::Speak { text });
        }
        cx.notify();
    }

    /// 从刚念完的那一段往后推进：下一个非空段 → 念；这一章到头 → 下一章的接力；
    /// 最后一章到头 → 停。
    fn advance_from(&mut self, chapter: usize, after: Range<usize>, cx: &mut Context<Self>) {
        // 先在不可变借用里把「下一段是什么」算完，再动状态 —— 直接在循环里改
        // 会跟 `self.book()` 打架。
        let next = self
            .book()
            .and_then(|book| book.span_doc(chapter))
            .and_then(|doc| next_paragraph(doc, after));
        match next {
            Some(range) => self.speak_paragraph(chapter, range, cx),
            None => {
                let has_next = self
                    .book()
                    .is_some_and(|book| chapter + 1 < book.chapter_count());
                if !has_next {
                    // 整本书念完：位置已经随每一段落过库，这里只管停下。
                    self.stop_speech();
                    cx.notify();
                    return;
                }
                self.speech.pending_chapter_start = true;
                self.advance_chapter(1, cx);
                self.wait_for_chapter_then_speak(chapter + 1, cx);
            }
        }
    }

    /// 跨章续读的接力：新章排版落地（恢复请求落定、行盒非空）就念它的第一段。
    ///
    /// 轮询而不是挂回调，因为「排完了」此刻没有事件可订；100 ms 一拍、至多五秒，
    /// 超时就停在那章开头 —— 比念错安全。真机上换章排版远快于这个上限。
    fn wait_for_chapter_then_speak(&mut self, chapter: usize, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            for _ in 0..50 {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let mut done = false;
                let Ok(()) = weak.update(cx, |session, cx| {
                    // 中途暂停/停了就不接力：那一格交给读者的下一次播放。
                    if !session.speech.pending_chapter_start || !session.speech_playing() {
                        done = true;
                        return;
                    }
                    let ready = !session.has_pending_restore()
                        && session.book().is_some_and(|book| {
                            book.span_doc(chapter)
                                .is_some_and(|doc| !doc.lines().is_empty())
                        });
                    if !ready {
                        return;
                    }
                    session.speech.pending_chapter_start = false;
                    session.speak_from_offset(chapter, 0, cx);
                    done = true;
                }) else {
                    return;
                };
                if done {
                    return;
                }
            }
        })
        // 发出去就不管：接力要么念上（done = true）要么被暂停/停止取消，
        // 会话没了 update 回 Err 也自己退。
        .detach();
    }

    /// 从某章的某个偏移起（含）念第一个非空段。章首接力与「从视口开口」共用。
    fn speak_from_offset(&mut self, chapter: usize, offset: usize, cx: &mut Context<Self>) {
        let found = self
            .book()
            .and_then(|book| book.span_doc(chapter))
            .and_then(|doc| paragraph_from(doc, offset));
        let Some(range) = found else {
            return;
        };
        self.speak_paragraph(chapter, range, cx);
    }
}

impl Drop for ReaderSession {
    /// 离开阅读页 = 停听 + 落库。谁在念跟着那一屏走，不跟着进程走。
    fn drop(&mut self) {
        self.stop_speech();
    }
}

/// 从 `offset` 起（含）的第一个**非空**段。纯文本空白段（pretty-print 的换行缩进
/// 是文本节点，自己就能成「段」）跳过 —— 念它出来的是一段静默。`None` = 这一章
/// 从这里往后没有可念的了。
fn paragraph_from(doc: &Document, offset: usize) -> Option<Range<usize>> {
    let mut probe = offset;
    while probe < doc.text_map().chars() {
        match doc.paragraph_range(probe) {
            Some(range) => {
                if doc.text_map().slice(range.clone()).trim().is_empty() {
                    probe = range.end.max(probe + 1);
                } else {
                    return Some(range);
                }
            }
            None => probe += 1,
        }
    }
    None
}

/// [`paragraph_from`] 的「下一段」版：从刚念完的那一段往后找。空段连环跳。
fn next_paragraph(doc: &Document, after: Range<usize>) -> Option<Range<usize>> {
    let mut cursor = after;
    while let Some(range) = doc.paragraph_after(cursor.clone()) {
        if !doc.text_map().slice(range.clone()).trim().is_empty() {
            return Some(range);
        }
        cursor = range;
    }
    None
}

#[cfg(test)]
mod tests {
    //! 四条验收门里能 headless 钉的三条（段切分那条在 folio-core 的 `hit::tests`）。
    //! 引擎在测试进程里不存在（`speech::engine()` 对不上的机器回 `None`，探测失败
    //! 还有进程内缓存），所以这里直接喂状态与事件 —— 状态机不该依赖引擎在场。

    use std::sync::Arc;

    use folio_core::content::document::Document;
    use folio_core::speech::Event;
    use folio_core::store::Store;
    use folio_core::{Chapter, Config, Content, DocMeta, FlowDoc, Format, Locator, Reader};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, px, size};

    use super::{Listen, next_paragraph, paragraph_from};
    use crate::ui::features::reader::model::session::ReaderSession;
    use crate::ui::services::config::ConfigHolder;

    /// 两章各几段的语料：段落短到不折行，段界一眼可断。
    fn speech_flow() -> FlowDoc {
        FlowDoc {
            chapters: vec![
                Chapter::inline("第一章", "<p>甲一段。</p><p>甲二段。</p><p>甲三段。</p>"),
                Chapter::inline("第二章", "<p>乙一段。</p><p>乙二段。</p>"),
            ],
            ..Default::default()
        }
    }

    fn meta() -> DocMeta {
        DocMeta {
            title: "听书测试".into(),
            format: Format::parse("markdown").expect("格式名要认得"),
            ..DocMeta::default()
        }
    }

    fn store_with_book(flow: &FlowDoc) -> (Arc<Store>, i64) {
        let store = Arc::new(Store::in_memory().expect("内存库"));
        let id = store
            .add_book(std::path::Path::new("memory://fixture"), &meta())
            .expect("入库");
        store
            .save_flow(id, flow, Format::Markdown)
            .expect("章节入库");
        (store, id)
    }

    /// 开一扇窗、装好配置、把会话摆上去。`store` 给 `(库, book_id)` 就落库，
    /// 给 `None` 就是一条纯会话。
    fn open(
        cx: &mut TestAppContext,
        flow: &FlowDoc,
        store: Option<(Arc<Store>, i64)>,
    ) -> (Entity<ReaderSession>, AnyWindowHandle) {
        let doc: Arc<dyn Document> = Arc::from(folio_core::content::document::from_content(
            meta(),
            Content::Reflow(flow.clone()),
        ));
        let (store, book_id) = match store {
            Some((store, id)) => (Some(store), Some(id)),
            None => (None, None),
        };
        let mut view = None;
        let handle = cx.open_window(size(px(460.), px(520.)), |window, cx| {
            ConfigHolder::install(Config::default(), cx);
            let built = ReaderSession::with_document(
                doc,
                Reader::default(),
                store,
                book_id,
                Locator::start(),
                cx,
            );
            let created = cx.new(|_| built);
            view = Some(created.clone());
            Root::new(created, window, cx)
        });
        let view = view.expect("ReaderSession 应该被创建");
        let handle: AnyWindowHandle = handle.into();
        settle(cx, handle);
        (view, handle)
    }

    /// 两轮「跑空事件 → 渲染一帧」：首轮挂恢复请求，次轮把它落到行盒上。
    fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("渲染应成功");
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("渲染应成功");
    }

    /// 第 `index` 个非空段的区间（0 起），从排版产物里解出来当断言的基准。
    fn paragraph(
        view: &Entity<ReaderSession>,
        chapter: usize,
        index: usize,
        cx: &mut TestAppContext,
    ) -> std::ops::Range<usize> {
        view.read_with(cx, |session, _| {
            let doc = session
                .book()
                .and_then(|book| book.span_doc(chapter))
                .expect("这一章应该在拼接窗口里");
            let mut found = super::paragraph_from(doc, 0).expect("至少应有一段");
            for _ in 0..index {
                found = super::next_paragraph(doc, found).expect("段应存在");
            }
            found
        })
    }

    fn playing_text(view: &Entity<ReaderSession>, cx: &mut TestAppContext) -> Option<String> {
        view.read_with(cx, |session, _| {
            let (chapter, range) = session.speech_range()?;
            Some(
                session
                    .book()
                    .and_then(|book| book.span_doc(chapter))
                    .map(|doc| doc.text_map().slice(range))
                    .unwrap_or_default(),
            )
        })
    }

    /// 上下滚动里，朗读跨到别的章：屏要跟过去，不能停在上一章。
    ///
    /// 从前的判据是「锚章 == 正在念的章才摆屏」。跨章那一帧里恢复请求与锚章迁移
    /// 有先后，锚章可能还留在上一章 —— 于是音频继续念下一章，画面却停在原地，
    /// 读者看到的就是「跨章时往回跳」。这一条把状态直接摆成「audio 在第二章、
    /// 锚章还停在第一章」，再问一次 `speak_paragraph`：锚章必须被带过去。
    #[gpui_kit::test]
    fn speaking_a_paragraph_in_another_chapter_brings_the_view_along(cx: &mut TestAppContext) {
        use crate::ui::features::reader::model::chrome::PageMode;
        use folio_core::infra::config::PageTurn;

        cx.update(gpui_kit::init);
        // 两章都撑得比一屏高：短章会被「滚到底」夹回 0，锚章归谁就测不出来了。
        let body = "<p>这是一段用来把章节撑过一屏的正文，重复许多次。</p>".repeat(30);
        let flow = FlowDoc {
            chapters: vec![
                Chapter::inline("第一章", &body),
                Chapter::inline("第二章", &body),
            ],
            ..Default::default()
        };
        let (view, handle) = open(cx, &flow, None);
        cx.update(|app| {
            ConfigHolder::edit_appearance(app, |a| a.page_turn = PageTurn::Scroll);
        });
        settle(cx, handle);
        assert!(
            view.read_with(cx, |session, _| session.mode() == PageMode::Continuous),
            "前置：这一屏该已经站在连续滚动里"
        );
        assert_eq!(
            view.read_with(cx, |session, _| session.chapter_index()),
            0,
            "前置：锚章还在第一章"
        );

        let first = paragraph(&view, 1, 0, cx);
        view.update(cx, |session, cx| {
            session.speech.available = Some(true);
            session.speak_paragraph(1, first.clone(), cx);
        });
        settle(cx, handle);

        assert_eq!(
            view.read_with(cx, |session, _| session.chapter_index()),
            1,
            "念到第二章时锚章该跟过去"
        );
        assert!(
            view.read_with(cx, |session, _| session.scroll_y) > 0.0,
            "视口该落在第二章的章首，而不是留在第一章"
        );
    }

    /// 逐段推进 + 跨章续读 + 念完整本书停下。`Done` 是唯一的推进事件，
    /// 推到哪儿断言就落到哪儿 —— 全程没有引擎在场。
    #[gpui_kit::test]
    fn done_advances_paragraph_by_paragraph_until_the_book_ends(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = speech_flow();
        let (store, book_id) = store_with_book(&flow);
        let (view, handle) = open(cx, &flow, Some((store.clone(), book_id)));

        let first = paragraph(&view, 0, 0, cx);
        view.update(cx, |session, _| {
            session.speech.available = Some(true);
            session.speech.state = Listen::Playing {
                chapter: 0,
                range: first,
            };
        });

        view.update(cx, |session, cx| session.on_speech_event(Event::Done, cx));
        assert_eq!(playing_text(&view, cx).as_deref(), Some("甲二段。"));
        view.update(cx, |session, cx| session.on_speech_event(Event::Done, cx));
        assert_eq!(playing_text(&view, cx).as_deref(), Some("甲三段。"));

        // 第一章念完：跨进第二章，等排版落地后从它的第一段开口。
        view.update(cx, |session, cx| session.on_speech_event(Event::Done, cx));
        assert!(
            view.read_with(cx, |session, _| session.speech.pending_chapter_start),
            "章末应挂上接力棒"
        );
        settle(cx, handle);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(150));
        cx.run_until_parked();
        assert_eq!(playing_text(&view, cx).as_deref(), Some("乙一段。"));

        view.update(cx, |session, cx| session.on_speech_event(Event::Done, cx));
        assert_eq!(playing_text(&view, cx).as_deref(), Some("乙二段。"));

        // 最后一章的最后一念完：停，位置落在末段起点。
        let (_, last) = view.read_with(cx, |session, _| {
            session.speech_range().expect("停听前那一段的区间")
        });
        view.update(cx, |session, cx| session.on_speech_event(Event::Done, cx));
        assert!(view.read_with(cx, |session, _| session.speech.state == Listen::Idle));
        let (_, locator) = store
            .position(book_id)
            .expect("位置可读")
            .expect("停听该落一次库");
        assert_eq!(
            locator,
            Locator {
                chapter: 1,
                offset: last.start
            },
            "「听到哪儿」落在 positions 表上，与读书同一个坐标系"
        );
    }

    /// 停不依赖回调（验收门）：停了之后撞到的 `Done` 不推进、不卡住。
    #[gpui_kit::test]
    fn a_done_after_stop_changes_nothing(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = speech_flow();
        let (store, book_id) = store_with_book(&flow);
        let (view, _handle) = open(cx, &flow, Some((store.clone(), book_id)));

        let first = paragraph(&view, 0, 0, cx);
        view.update(cx, |session, _| {
            session.speech.available = Some(true);
            session.speech.state = Listen::Playing {
                chapter: 0,
                range: first.clone(),
            };
        });

        view.update(cx, |session, _| session.stop_speech());
        assert!(view.read_with(cx, |session, _| session.speech.state == Listen::Idle));
        let (_, locator) = store.position(book_id).expect("位置可读").expect("落库");
        assert_eq!(
            locator,
            Locator {
                chapter: 0,
                offset: first.start
            }
        );

        // 迟到的 Done（静音窗之外的最后一道保险：会话侧只认 Playing）。
        view.update(cx, |session, cx| session.on_speech_event(Event::Done, cx));
        assert!(view.read_with(cx, |session, _| session.speech.state == Listen::Idle));
        let (_, again) = store.position(book_id).expect("位置可读").expect("落库");
        assert_eq!(again, locator, "停了之后位置不该再动");
    }

    /// 暂停留在本段；`Done` 撞上暂停不推进；恢复从同一段开头再念。
    #[gpui_kit::test]
    fn pausing_holds_the_paragraph_and_resuming_repeats_it(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = speech_flow();
        let (view, _handle) = open(cx, &flow, None);

        let first = paragraph(&view, 0, 0, cx);
        view.update(cx, |session, _| {
            session.speech.available = Some(true);
            session.speech.state = Listen::Playing {
                chapter: 0,
                range: first.clone(),
            };
        });

        view.update(cx, |session, cx| session.toggle_speech(cx));
        assert_eq!(
            view.read_with(cx, |session, _| session.speech_range()),
            Some((0, first.clone())),
            "暂停留在本段"
        );

        view.update(cx, |session, cx| session.on_speech_event(Event::Done, cx));
        assert_eq!(
            view.read_with(cx, |session, _| session.speech_range()),
            Some((0, first.clone())),
            "暂停之后 Done 不推进"
        );

        view.update(cx, |session, cx| session.toggle_speech(cx));
        assert!(
            view.read_with(cx, |session, _| session.speech_playing()),
            "恢复即播放"
        );
        assert_eq!(
            view.read_with(cx, |session, _| session.speech_range()),
            Some((0, first)),
            "恢复从这段开头再念"
        );
    }

    /// 降级（验收门）：引擎不在 / 没有书，入口都不摆 —— 不弹错误。
    #[gpui_kit::test]
    fn the_entry_stays_hidden_without_an_engine(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = speech_flow();
        let (view, _handle) = open(cx, &flow, None);

        assert!(
            !view.read_with(cx, |session, _| session.speech_entry_visible()),
            "探测没回来（available = None）时先不摆"
        );
        view.update(cx, |session, _| session.speech.available = Some(false));
        assert!(
            !view.read_with(cx, |session, _| session.speech_entry_visible()),
            "引擎不可用不摆"
        );
        view.update(cx, |session, _| session.speech.available = Some(true));
        assert!(
            view.read_with(cx, |session, _| session.speech_entry_visible()),
            "可用且有书才摆"
        );

        // 兜底那一屏没有书：同样不摆。
        let sample = cx.update(|cx| cx.new(|cx| ReaderSession::boot(cx)));
        assert!(
            sample.read_with(cx, |session, _| !session.speech_entry_visible()),
            "没有书就不摆"
        );
    }

    /// 段推进的两个纯函数：空段（pretty-print 的空白文本节点）跳过，章尾给 `None`。
    #[gpui_kit::test]
    fn empty_paragraphs_are_skipped_and_the_chapter_end_holds(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = FlowDoc {
            chapters: vec![Chapter::inline(
                "空白章",
                "<p>有字的一段。</p><p>   </p><p>收尾。</p>",
            )],
            ..Default::default()
        };
        let (view, _handle) = open(cx, &flow, None);
        view.read_with(cx, |session, _| {
            let doc = session
                .book()
                .and_then(|book| book.span_doc(0))
                .expect("应有一章");
            let first = paragraph_from(doc, 0).expect("首段应可解");
            assert_eq!(doc.text_map().slice(first.clone()), "有字的一段。");

            let second = next_paragraph(doc, first).expect("下一段应跳过空白段");
            assert_eq!(doc.text_map().slice(second.clone()), "收尾。");

            assert!(next_paragraph(doc, second).is_none(), "章尾停住");
        });
    }
}
