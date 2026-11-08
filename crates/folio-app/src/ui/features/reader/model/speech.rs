//! 听书（合成朗读）的会话侧：播放状态机、逐段推进与进度落库。
//!
//! 分工（与引擎侧 `folio_core::speech` 的模块文档对读）：引擎线程只回答「怎么念」，
//! 这里回答「念什么、念到哪儿、什么时候停」。设计稿在本机 `plans/听书.md`（不入库），
//! 几条落进实现的口径：
//!
//! - **句，不是段。** 一句 = `hit::paragraph_range` 给出的段再按句末标点切开的字符
//!   区间（长句再按句内停顿标点下刀，切法见本模块的 `split_sentences`），一句一条
//!   utterance，`Done` 里推下一句，
//!   不做预排 —— 「读者手动翻页时队列里那几句怎么办」这个分支整个不存在。
//!   粒度下沉到句是为了文稿那一面能像 Apple Music 歌词那样让当前行跟着声音走；
//!   正文屏按「这句看得见才不动」跟（同一段跨页、长段超屏也要动，见
//!   `speak_sentence` 与 `position::offset_visible`）。
//! - **听的位置与读的位置是同一个东西。** `auto_scroll` 开着时每开新的一段
//!   `jump_to_offset` 把那一屏摆过来（它自带落库），停听时再按句起点补一账
//!   —— `positions` 表里从此只有一个答案。不新开表。
//! - **停不依赖回调。** 引擎侧的静音窗（`speech::gate::reduce`）负责吞掉 Stop 之后
//!   的回调；这一侧的另一半是：`Done` 只在 `Playing` 里推进，暂停/停止之后撞到的
//!   尾巴在这里被挡住。两边各有一条测试钉着。
//! - **听就是读。** 每句 `Done` 记一次 `note_reading_activity`，阅读时长的宽限线
//!   照常接住，不新写计时。
//!
//! 手动翻页/滚动**不**暂停听书：播放中的位置归听书管（下一段开念时 `auto_scroll`
//! 会把屏摆回念的那段），读者要自由浏览就先按一下暂停 —— 可预期，且不需要在
//! 每个输入口各插一刀。

use std::ops::Range;
use std::time::{Duration, Instant};

use folio_core::speech::{Command, Event};
use folio_core::{Engine, Locator};
use gpui_kit::{Context, Task};

use crate::ui::features::reader::model::chapter_layout::Document;
use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::services::config::ConfigHolder;

/// 新会话接手时，通道里那些「上一本留下的」事件该怎么处理。
///
/// `Done` / `Begin` 属于旧会话，丢掉；`Ready` 是进程级引擎**唯一**一次的可用性
/// 结论，必须回放 —— 引擎线程可能比这个监听任务的首次 poll 更快，那时它已经在
/// 通道里等着了，抽干时丢掉它 `available` 就永远停在 `None`。
fn attach_replay(event: Event) -> Option<Event> {
    matches!(event, Event::Ready(_)).then_some(event)
}

/// 播放状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::ui::features::reader) enum Listen {
    /// 可用但没在念。引擎不存在时也停在这一态 —— 入口摆不摆看
    /// [`ReaderSession::speech_entry_visible`]，不看这里。
    Idle,
    /// 正在念哪一章的哪一句。这一句同时是给视图画当前行高亮的全部信息。
    Playing { chapter: usize, range: Range<usize> },
    /// 停在了哪一句。恢复 = 从这句开头再念（引擎没有句内暂停，见引擎模块文档）。
    Paused { chapter: usize, range: Range<usize> },
}

/// 会话侧的听书状态。字段打进 [`ReaderSession`]（一个实体、多个 `impl` 块）。
pub(in crate::ui::features::reader) struct Speech {
    /// 引擎句柄。`None` = 没挂上（还没开过书）或探测失败。
    engine: Option<Engine>,
    /// 引擎的 `Ready` 到了没有。`None` = 还没点过听书钮，或点了还在探测（几秒内
    /// 见分晓）；`Some(false)` = **上一次**点开的探测终审失败 —— 圆钮仍常在，下一
    /// 次点开听书屏就是重试。屏内用「准备中 / 没有可用的语音引擎」把这两格说清。
    available: Option<bool>,
    state: Listen,
    /// 这一句开口的时刻。`Done` 拿它与此刻的差当这一句的实测时长（喂给
    /// [`Self::chars_per_sec`]）；暂停/停止时清掉，暂停的那段不混进时长里。
    speaking_since: Option<Instant>,
    /// 实测语速：**字/秒，已归一到 1×**。引擎、语言、音色之间能差三倍（实测
    /// 227–604 ms/字），听书屏的「快进 N 秒」靠它把秒折成字；还没念完过一句时
    /// 为 `None`，由视图拿兜底值算。每句 `Done` 按三成权重并进去（见
    /// [`blend_chars_per_sec`]），单句的抖动不会让下一跳飘走。
    chars_per_sec: Option<f32>,
    /// 跨章续读的接力棒：`advance_chapter` 之后新章还没排完，这一格挂着
    /// 「排版落地就念这一章的第一句」。轮询见 [`Self::wait_for_chapter_then_speak`]。
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
            speaking_since: None,
            chars_per_sec: None,
            pending_chapter_start: false,
            listening: None,
        }
    }
}

/// 一句念完，把实测时长并进语速估计（字/秒，1×）。
///
/// 丢掉三类样本：字数 0（空白句）、时长过短（引擎抖动或立即回 `Done`）、
/// 时长过长（读者中途暂停过、或进程被挂起）—— 它们会把估计拉歪。其余按
/// 0.7 / 0.3 加权：一次换声/换语言的阶跃，三句之内就跟上，而单句的噪声
/// 只让它走三成。
fn blend_chars_per_sec(previous: Option<f32>, chars: usize, secs: f32, rate: f32) -> Option<f32> {
    if chars == 0 || secs < 0.5 || secs > 120.0 {
        return previous;
    }
    let rate = rate.max(0.01);
    let sample = (chars as f32 / secs / rate).clamp(0.5, 60.0);
    Some(match previous {
        Some(old) => old * 0.7 + sample * 0.3,
        None => sample,
    })
}

impl ReaderSession {
    /// 挂引擎、起事件循环。**点开听书那一屏时调**（`arm_speech`），幂等 —— 引擎
    /// 进程级只有一份，每个会话挂的是自己的事件循环。
    ///
    /// 上一次探测**终审失败**（`available == Some(false)`）时不是跳过，而是整个重来：
    /// 核心侧会把死掉的引擎换新（见 `folio_core::speech::engine`），事件循环也要跟着
    /// 换到新引擎的通道上 —— 旧循环捧着的旧通道已经没人写了，随任务替换自然收场。
    /// 这就是「圆钮常在，点了再试」的那一下。
    pub(in crate::ui::features::reader) fn speech_attach(&mut self, cx: &mut Context<Self>) {
        if self.speech.listening.is_some() && self.speech.available != Some(false) {
            return;
        }
        // 定版式书没有可念的排版链（段落区间那一套不成立），入口由
        // [`Self::speech_entry_visible`] 里 `book().is_some()` 那一格挡住，
        // 走不到这里 —— 别在这里替它把 `available` 写成什么。
        let Some(engine) = folio_core::speech::engine() else {
            // Mutex 被毒化才会到这（引擎线程不碰这个锁）。按不可用降级，屏内说明。
            self.speech.available = Some(false);
            return;
        };
        // 先读**持久快照**：`Event::Ready` 全进程只发一次，可能早就被上一个会话
        // 取走（或在这个任务的首次 poll 之前就到了）。赌事件还在管道里，赌输的那
        // 一次就是真机上「听书按钮有时候不显示」。
        if let Some(ok) = engine.ready() {
            self.speech.available = Some(ok);
            if !ok {
                self.speech.engine = None;
            }
            cx.notify();
        }
        self.speech.engine = Some(engine.clone());
        let rx = engine.events();
        let weak = cx.entity().downgrade();
        self.speech.listening = Some(cx.spawn(async move |_, cx| {
            // 事件管道是进程级的一条：上一个会话可能留了尾巴，先抽干。
            // `Done` / `Begin` 属于旧会话，丢掉；`Ready` 是唯一的可用性结论，
            // 回放给这一本 —— 它可能在监听挂上来之前就已经在通道里了。
            while let Ok(event) = rx.try_recv() {
                if let Some(ready) = attach_replay(event) {
                    let Ok(()) = weak.update(cx, |session, cx| session.on_speech_event(ready, cx))
                    else {
                        return;
                    };
                }
            }
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

    /// 入口摆不摆。**只看有没有一本可念的书**：引擎可不可用不再决定圆钮的生死 ——
    /// 从前开书时探一次针，撞上 TTS 冷启动的 500ms 窗口就误判失败并进程级钉死，
    /// 圆钮整轮消失（真机 2026-11-07）。现在圆钮常在，点它才起引擎；探测中与终审
    /// 失败都在听书屏里说清楚（「准备中」/「没有可用的语音引擎」），失败后下一次
    /// 点击就是重试。
    pub fn speech_entry_visible(&self) -> bool {
        self.book().is_some()
    }

    /// 正在播放吗。入口的图标切换与「这一格现在是什么」都问它。
    pub fn speech_playing(&self) -> bool {
        matches!(self.speech.state, Listen::Playing { .. })
    }

    /// 引擎这一刻的结论。`None` = 还没点过听书钮、或点了还在探测；`Some(false)` =
    /// 上一次点开的探测终审失败。听书屏拿它在播放键旁边把现状说清，圆钮不看它。
    pub(in crate::ui::features::reader) fn speech_available(&self) -> Option<bool> {
        self.speech.available
    }

    /// 实测语速（字/秒，1× 档）。听书屏「快进 N 秒」拿它把秒折算成字；
    /// `None` = 还没念完过一句，视图用兜底值。
    pub(in crate::ui::features::reader) fn speech_chars_per_sec(&self) -> Option<f32> {
        self.speech.chars_per_sec
    }

    /// 现在念到（或停在）哪一句：`(章, 字符区间)`。视图拿它画当前句高亮。
    pub(in crate::ui::features::reader) fn speech_range(&self) -> Option<(usize, Range<usize>)> {
        match &self.speech.state {
            Listen::Playing { chapter, range } | Listen::Paused { chapter, range } => {
                Some((*chapter, range.clone()))
            }
            Listen::Idle => None,
        }
    }

    /// 听书那一屏里点某一句：直接从这一句开头念（与暂停恢复同一个入口）。
    ///
    /// 外面只给「哪一章 + 这一句在章内的字符区间」，不碰引擎那一侧的队列 —— 与
    /// [`Self::toggle_speech`] 走同一条 `speak_sentence`，两条路的行为不会分叉。
    pub(in crate::ui::features::reader) fn speak_from(
        &mut self,
        chapter: usize,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) {
        if !self.speech_entry_visible() {
            return;
        }
        self.speak_sentence(chapter, range, cx);
    }

    /// 入口那一下：没念 → 从视口当前句开始念；念着 → 暂停；暂停 → 这一句从头再念。
    pub fn toggle_speech(&mut self, cx: &mut Context<Self>) {
        if !self.speech_entry_visible() {
            return;
        }
        match self.speech.state.clone() {
            Listen::Idle => self.start_speech(cx),
            Listen::Playing { .. } => self.pause_speech(),
            Listen::Paused { chapter, range } => self.speak_sentence(chapter, range, cx),
        }
    }

    /// 打开听书那一屏时把「将从哪句开始」摆好：**不开口**，只把待念的位置定位出来。
    /// 从前打开那一屏就顺势起播 —— 读者想先看看文稿再决定从哪听，门一推开声音就
    /// 出来了。起播交给圆钮：摆好的这一句按下去（[`Self::toggle_speech`]）从它开口。
    ///
    /// 引擎也在这扇门被推开的那一刻才第一次碰（[`Self::speech_attach`]）：开书路径
    /// 不为 TTS 付一分钱，冷启动的那一两秒被「读者找播放键」的时间盖掉。
    ///
    /// 「停在一句、还没念」的形状与暂停（[`Listen::Paused`]）完全相同，推进、文稿
    /// 定位、上一句/下一句全部共用同一套，不另立状态。已经有位置（在念 / 暂停过）
    /// 就不动：重开那一屏，停的地方还在原地。
    pub(in crate::ui::features::reader) fn arm_speech(&mut self, cx: &mut Context<Self>) {
        if !self.speech_entry_visible() {
            return;
        }
        // 引擎在这里才第一次被碰：点圆钮 = 初始化（幂等；上一次终审失败的话这一次
        // 就是重试）。开书路径因此一个字节都不为 TTS 付出。
        self.speech_attach(cx);
        if self.speech_range().is_some() {
            return;
        }
        // 与 start_speech 同一个起点：视口顶所在的那一句（含头）。
        let Some((chapter, offset)) = self.resolve_position() else {
            return;
        };
        let Some(range) = self
            .book()
            .and_then(|book| book.span_doc(chapter))
            .and_then(|doc| sentence_from(doc, offset))
        else {
            return;
        };
        self.speech.state = Listen::Paused { chapter, range };
        cx.notify();
    }

    /// 停：离开阅读页（`Drop`）与换书（`open_book`）都走它。位置按当前句起点落库
    /// —— 「听到哪儿」从此在 `positions` 里有数，与读书共用一个坐标系。
    pub(in crate::ui::features::reader) fn stop_speech(&mut self) {
        self.speech.pending_chapter_start = false;
        self.speech.speaking_since = None;
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
                // 这一句念了多久：并进实测语速，供听书屏的「快进 N 秒」把秒折成字。
                // 放在 advance 之前：advance 会重置 `speaking_since`（下一句开口）。
                if let Some(since) = self.speech.speaking_since.take() {
                    self.speech.chars_per_sec = blend_chars_per_sec(
                        self.speech.chars_per_sec,
                        range.end.saturating_sub(range.start),
                        since.elapsed().as_secs_f32(),
                        ConfigHolder::speech(cx).rate,
                    );
                }
                self.note_reading_activity();
                self.advance_from(chapter, range, cx);
            }
        }
    }

    fn start_speech(&mut self, cx: &mut Context<Self>) {
        // 从读者此刻看着的地方开口：视口顶所在的那一句（含头）。
        let Some((chapter, offset)) = self.resolve_position() else {
            return;
        };
        let Some(book) = self.book() else {
            return;
        };
        let found = book
            .span_doc(chapter)
            .and_then(|doc| sentence_from(doc, offset));
        let Some(range) = found else {
            return;
        };
        self.speak_sentence(chapter, range, cx);
    }

    fn pause_speech(&mut self) {
        if let Listen::Playing { chapter, range } = self.speech.state.clone() {
            if let Some(engine) = &self.speech.engine {
                engine.send(Command::Stop);
            }
            self.speech.state = Listen::Paused { chapter, range };
            // 暂停的那段不算进这一句的实测时长：恢复时 `speak_sentence` 会重新记起点。
            self.speech.speaking_since = None;
            // 暂停不落库：位置那一行跟着 `auto_scroll` 的逐段跳转已经在了，
            // 而停听（`stop_speech`）才是设计稿里「落库」的那一刻。句比段密，
            // 逐句落库会把 positions 写成高频流水账，所以屏仍按段跟。
        }
    }

    /// 念一句。`auto_scroll` 开着又把读者带到这一句：跨了段动；同一段里、这句的
    /// 行顶还**看得见**就不动 —— 句比段密得多，每句都 `jump_to_offset` 会每几秒
    /// 写一次 `positions` 并把屏翻来翻去。但「看不见」时（同一段从这页漫到下一页、
    /// 一段长过一屏）也必须动：屏要跟的是声音，不是段落记号。听书文稿那一面自己
    /// 按句滚，不受这一格管。跨了章就换锚章（走目录跳转那条落点）。语速/音高每句
    /// 随发 —— 设置页改了配置，下一句自然生效，不需要另开一条命令通路。
    fn speak_sentence(&mut self, chapter: usize, range: Range<usize>, cx: &mut Context<Self>) {
        // 同一段里就不为换句动屏；这一句看不见（跨页/长段）时例外。
        let same_paragraph = self.speech_range().is_some_and(|(old_chapter, old)| {
            old_chapter == chapter
                && self.book().is_some_and(|book| {
                    book.span_doc(chapter).is_some_and(|doc| {
                        doc.paragraph_range(old.start) == doc.paragraph_range(range.start)
                    })
                })
        });
        if ConfigHolder::speech(cx).auto_scroll
            && (!same_paragraph || !self.offset_visible(chapter, range.start))
        {
            // 跟读的决定一句一条：真机上「声音在念、画面不动」时，这一行与引擎的
            // Done 一对照，是引擎没回事件还是跟读没动屏一望可知。
            tracing::info!(
                chapter,
                at = range.start,
                same_paragraph,
                "跟读：把屏带过去"
            );
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
            // 实测时长的起点：挂在真正送出这一句的时刻，探测/空句都不算。
            self.speech.speaking_since = Some(Instant::now());
        }
        cx.notify();
    }

    /// 从刚念完的那一句往后推进：段内的下一句 → 念；这一章到头 → 下一章的接力；
    /// 最后一章到头 → 停。
    fn advance_from(&mut self, chapter: usize, after: Range<usize>, cx: &mut Context<Self>) {
        // 先在不可变借用里把「下一句是什么」算完，再动状态 —— 直接在循环里改
        // 会跟 `self.book()` 打架。
        let next = self
            .book()
            .and_then(|book| book.span_doc(chapter))
            .and_then(|doc| next_sentence(doc, after));
        match next {
            Some(range) => self.speak_sentence(chapter, range, cx),
            None => {
                let has_next = self
                    .book()
                    .is_some_and(|book| chapter + 1 < book.chapter_count());
                if !has_next {
                    // 整本书念完：位置已经随段落的跳转落过库，这里只管停下。
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

    /// 跨章续读的接力：新章排版落地（恢复请求落定、行盒非空）就念它的第一句。
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

    /// 从某章的某个偏移起（含）念第一个非空句。章首接力与「从视口开口」共用。
    fn speak_from_offset(&mut self, chapter: usize, offset: usize, cx: &mut Context<Self>) {
        let found = self
            .book()
            .and_then(|book| book.span_doc(chapter))
            .and_then(|doc| sentence_from(doc, offset));
        let Some(range) = found else {
            return;
        };
        self.speak_sentence(chapter, range, cx);
    }
}

impl Drop for ReaderSession {
    /// 离开阅读页 = 停听 + 落库。谁在念跟着那一屏走，不跟着进程走。
    fn drop(&mut self) {
        self.stop_speech();
    }
}

/// 句末标点。`；` 与 ASCII 的 `.` 不在内：前者是并列分句（长句下刀的刀口，见
/// [`CLAUSE_ENDERS`]），后者要看后面是不是空白（小数点、缩写不切，英文句末才切）。
const SENTENCE_ENDERS: &[char] = &['。', '！', '？', '!', '?', '…'];

/// 句末标点后面跟的引号 / 括号归前一句：`他说：“走吧。”` 的 `。”` 不能拆到下一句。
const CLOSERS: &[char] = &[
    '”', '’', '」', '』', '）', ')', '】', '》', '〕', '］', ']', '"', '\'',
];

/// 长句下刀的档：一句超过这个字数，就在句内的停顿标点处再切成几片。念与显共用
/// [`split_sentences`] 这一把尺，所以 TTS 的 utterance 与文稿的行同一时刻变细。
/// 24 个字上下的依据：文稿亮的那句在一两行以内（Apple Music 那面歌词的行感），
/// 引擎一次也念不到半分钟。
const SENTENCE_PIECE_MAX: usize = 24;

/// 句内的停顿标点（长句下刀处）。`，` 与 `、` 是最自然的换气口；`；` 在句末那把尺里
/// 刻意不切（并列分句常长过一屏，把整句拖成一句），在句内这一把里算一刀；ASCII 的
/// `,` 与 `;` 同理 —— 英文除句末标点外只有它们算停顿。
const CLAUSE_ENDERS: &[char] = &['，', '、', '；', ',', ';'];

fn is_clause_ender(chars: &[char], at: usize) -> bool {
    match chars[at] {
        // 千分位逗号不是停顿：1,000 不能切成两半。
        ',' => {
            !chars
                .get(at.wrapping_sub(1))
                .is_some_and(|prev| prev.is_ascii_digit())
                && !chars.get(at + 1).is_some_and(|next| next.is_ascii_digit())
        }
        ch => CLAUSE_ENDERS.contains(&ch),
    }
}

fn is_sentence_ender(chars: &[char], at: usize) -> bool {
    match chars[at] {
        '.' => chars.get(at + 1).is_none_or(|next| next.is_whitespace()),
        ch => SENTENCE_ENDERS.contains(&ch),
    }
}

fn is_closer(ch: char) -> bool {
    CLOSERS.contains(&ch)
}

fn push_trimmed(out: &mut Vec<Range<usize>>, chars: &[char], start: usize, end: usize) {
    let mut a = start;
    let mut b = end;
    while a < b && chars[a].is_whitespace() {
        a += 1;
    }
    while b > a && chars[b - 1].is_whitespace() {
        b -= 1;
    }
    if a < b {
        out.push(a..b);
    }
}

fn push_sentence(out: &mut Vec<Range<usize>>, chars: &[char], range: Range<usize>) {
    let mut start = range.start;
    let mut end = range.end;
    while start < end && chars[start].is_whitespace() {
        start += 1;
    }
    while end > start && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    if start >= end {
        return;
    }
    if end - start <= SENTENCE_PIECE_MAX {
        out.push(start..end);
        return;
    }
    push_clauses(out, chars, start, end);
}

/// 把一句长的按句内停顿标点解成小段（标点连同连续标点与尾随的引号/括号归前片），再
/// 贪心地把放得进一档的相邻段并回去 —— 片界只落在停顿标点上。单片本身长过一档时
/// 原样保留：中间没有刀口，切在字中间比长更糟。
fn push_clauses(out: &mut Vec<Range<usize>>, chars: &[char], start: usize, end: usize) {
    let mut pieces: Vec<Range<usize>> = Vec::new();
    let mut piece = start;
    let mut at = start;
    while at < end {
        if is_clause_ender(chars, at) {
            let mut boundary = at + 1;
            while boundary < end
                && (CLAUSE_ENDERS.contains(&chars[boundary]) || is_closer(chars[boundary]))
            {
                boundary += 1;
            }
            pieces.push(piece..boundary);
            piece = boundary;
            at = boundary;
        } else {
            at += 1;
        }
    }
    pieces.push(piece..end);

    let mut merged: Vec<Range<usize>> = Vec::new();
    for piece in pieces {
        match merged.last_mut() {
            Some(last) if piece.end - last.start <= SENTENCE_PIECE_MAX => last.end = piece.end,
            _ => merged.push(piece),
        }
    }
    for piece in merged {
        push_trimmed(out, chars, piece.start, piece.end);
    }
}

/// 把 `range` 切成句子：句末标点（连同连续的标点与尾随的引号 / 括号）之后断，
/// 换行也断；首尾与句间的空白不进任何一句。长过 [`SENTENCE_PIECE_MAX`] 的句再在
/// 句内停顿标点处切成几片（见 [`push_clauses`]）。返回的是章内字符区间，可以直接
/// 交给 `Command::Speak` —— 与文稿那一面同一把尺，念的 utterance 与文稿的行
/// 同一时刻变细。
pub(in crate::ui::features::reader) fn split_sentences(
    chars: &[char],
    range: Range<usize>,
) -> Vec<Range<usize>> {
    let end = range.end.min(chars.len());
    let mut out = Vec::new();
    let mut start = range.start.min(end);
    let mut at = start;
    while at < end {
        if is_sentence_ender(chars, at) {
            let mut boundary = at + 1;
            while boundary < end
                && (SENTENCE_ENDERS.contains(&chars[boundary]) || is_closer(chars[boundary]))
            {
                boundary += 1;
            }
            push_sentence(&mut out, chars, start..boundary);
            start = boundary;
            at = boundary;
        } else if chars[at] == '\n' {
            push_sentence(&mut out, chars, start..at);
            start = at + 1;
            at += 1;
        } else {
            at += 1;
        }
    }
    push_sentence(&mut out, chars, start..end);
    out
}

/// 从 `offset` 起（含）的第一句。空白与空段跳过；`offset` 落在段间空白时取下一句。
/// `None` = 这一章从这里往后没有可念的了。
pub(in crate::ui::features::reader) fn sentence_from(
    doc: &Document,
    offset: usize,
) -> Option<Range<usize>> {
    let chars: Vec<char> = doc.text_map().text().chars().collect();
    let mut probe = offset;
    while probe < chars.len() {
        let Some(paragraph) = doc.paragraph_range(probe) else {
            probe += 1;
            continue;
        };
        if paragraph.end <= probe {
            probe = paragraph.end.max(probe + 1);
            continue;
        }
        let sentences = split_sentences(&chars, paragraph.clone());
        if let Some(hit) = sentences
            .iter()
            .find(|sentence| sentence.start <= probe && probe < sentence.end)
        {
            return Some(hit.clone());
        }
        if let Some(next) = sentences.iter().find(|sentence| sentence.start >= probe) {
            return Some(next.clone());
        }
        probe = paragraph.end.max(probe + 1);
    }
    None
}

/// [`sentence_from`] 的「下一句」版：从刚念完的那一句往后找。
fn next_sentence(doc: &Document, after: Range<usize>) -> Option<Range<usize>> {
    sentence_from(doc, after.end)
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

    use super::{
        Listen, attach_replay, blend_chars_per_sec, next_sentence, sentence_from, split_sentences,
    };
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

    /// 第 `index` 个非空句的区间（0 起），从排版产物里解出来当断言的基准。
    fn sentence(
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
            let mut found = super::sentence_from(doc, 0).expect("至少应有一句");
            for _ in 0..index {
                found = super::next_sentence(doc, found).expect("句应存在");
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

    /// 语速估计：第一句直接采信实测，之后按 0.7 / 0.3 并进去；坏样本（0 字、
    /// 过短、过长）不碰估计 —— 听书屏「快进 N 秒」的落点全靠这一个数，抖一下
    /// 就是多跳或少跳几句。
    #[test]
    fn the_measured_rate_blends_and_ignores_bad_samples() {
        // 20 字念 5 秒 = 4 字/秒（250 ms/字）：没有旧值，直接就是它。
        assert_eq!(blend_chars_per_sec(None, 20, 5.0, 1.0), Some(4.0));
        // 倍率归一：同样 20 字 5 秒、但在 2× 下念完 → 1× 档是 2 字/秒。
        assert_eq!(blend_chars_per_sec(None, 20, 5.0, 2.0), Some(2.0));
        // 并进去：旧 4、新样本 8 → 4×0.7 + 8×0.3 = 5.2，单句只让估计走三成。
        let blended = blend_chars_per_sec(Some(4.0), 40, 5.0, 1.0).expect("样本有效");
        assert!((blended - 5.2).abs() < 1e-4, "实测并出来是 {blended}");
        // 坏样本原样交回：空白句、0.2 秒的引擎抖动、200 秒的挂起都不算数。
        for (chars, secs) in [(0, 5.0), (20, 0.2), (20, 200.0)] {
            assert_eq!(
                blend_chars_per_sec(Some(4.0), chars, secs, 1.0),
                Some(4.0),
                "{chars} 字 / {secs} 秒不该改动估计"
            );
        }
        // 越界样本夹回可用段：天文数字不把下一跳扔到章外。
        assert_eq!(blend_chars_per_sec(None, 1_000_000, 120.0, 1.0), Some(60.0));
        assert_eq!(blend_chars_per_sec(None, 10, 120.0, 1.0), Some(0.5));
    }

    /// 上下滚动里，朗读跨到别的章：屏要跟过去，不能停在上一章。
    ///
    /// 从前的判据是「锚章 == 正在念的章才摆屏」。跨章那一帧里恢复请求与锚章迁移
    /// 有先后，锚章可能还留在上一章 —— 于是音频继续念下一章，画面却停在原地，
    /// 读者看到的就是「跨章时往回跳」。这一条把状态直接摆成「audio 在第二章、
    /// 锚章还停在第一章」，再问一次 `speak_sentence`：锚章必须被带过去。
    #[gpui_kit::test]
    fn speaking_a_sentence_in_another_chapter_brings_the_view_along(cx: &mut TestAppContext) {
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
        // 翻页方式现在按书存，「听书把这本带进滚动」写的就是这一本 —— 所以这一条要带
        // 一个真有 id 的库行：`None` 那一档没有书挂着，按书的写入会被忽略。
        let (store, book_id) = store_with_book(&flow);
        let (view, handle) = open(cx, &flow, Some((store, book_id)));
        cx.update(|app| {
            ConfigHolder::edit_book_appearance(app, |a| a.page_turn = PageTurn::Scroll);
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

        let first = sentence(&view, 1, 0, cx);
        view.update(cx, |session, cx| {
            session.speak_sentence(1, first.clone(), cx);
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

    /// 逐句推进 + 跨章续读 + 念完整本书停下。`Done` 是唯一的推进事件，
    /// 推到哪儿断言就落到哪儿 —— 全程没有引擎在场。语料每段正好一句，
    /// 所以这条与「逐段推进」的落点相同；段内多句那条见下面 `sentence_flow`。
    #[gpui_kit::test]
    fn done_advances_sentence_by_sentence_until_the_book_ends(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = speech_flow();
        let (store, book_id) = store_with_book(&flow);
        let (view, handle) = open(cx, &flow, Some((store.clone(), book_id)));

        let first = sentence(&view, 0, 0, cx);
        view.update(cx, |session, _| {
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

        let first = sentence(&view, 0, 0, cx);
        view.update(cx, |session, _| {
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

    /// 暂停留在本句；`Done` 撞上暂停不推进；恢复从同一句开头再念。
    #[gpui_kit::test]
    fn pausing_holds_the_sentence_and_resuming_repeats_it(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = speech_flow();
        let (view, _handle) = open(cx, &flow, None);

        let first = sentence(&view, 0, 0, cx);
        view.update(cx, |session, _| {
            session.speech.state = Listen::Playing {
                chapter: 0,
                range: first.clone(),
            };
        });

        view.update(cx, |session, cx| session.toggle_speech(cx));
        assert_eq!(
            view.read_with(cx, |session, _| session.speech_range()),
            Some((0, first.clone())),
            "暂停留在本句"
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

    /// 打开听书屏的那一下只摆位置不开口：摆好之后停在第一句（暂停的形状），按下
    /// 播放才从它开口 —— 「点了开始才开始」。
    #[gpui_kit::test]
    fn arming_positions_the_sentence_without_speaking_and_playing_starts_it(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let flow = speech_flow();
        let (view, _handle) = open(cx, &flow, None);
        let first = sentence(&view, 0, 0, cx);
        view.update(cx, |session, cx| session.arm_speech(cx));
        assert_eq!(
            view.read_with(cx, |session, _| session.speech_range()),
            Some((0, first.clone())),
            "摆好的就是视口那句"
        );
        assert!(
            !view.read_with(cx, |session, _| session.speech_playing()),
            "摆位置不开口"
        );

        view.update(cx, |session, cx| session.toggle_speech(cx));
        assert!(
            view.read_with(cx, |session, _| session.speech_playing()),
            "按下播放才开口"
        );
        assert_eq!(
            view.read_with(cx, |session, _| session.speech_range()),
            Some((0, first)),
            "从摆好的那句开口"
        );
    }

    /// 已经有位置（暂停过）再摆就不动：重开那一屏，停的地方还在原地。
    #[gpui_kit::test]
    fn arming_keeps_an_existing_position(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = speech_flow();
        let (view, _handle) = open(cx, &flow, None);
        let second = sentence(&view, 0, 1, cx);
        view.update(cx, |session, _| {
            session.speech.state = Listen::Paused {
                chapter: 0,
                range: second.clone(),
            };
        });
        view.update(cx, |session, cx| session.arm_speech(cx));
        assert_eq!(
            view.read_with(cx, |session, _| session.speech_range()),
            Some((0, second)),
            "已有的位置不被摆场覆盖"
        );
    }

    /// 入口的显隐只看有没有一本可念的书：引擎探测的三种结论（还没探 / 失败 / 可用）
    /// 都不改变圆钮的生死。从前开书时探一次针，撞上 TTS 冷启动的 500ms 窗口就误判
    /// 失败并钉死，圆钮整轮消失（真机 2026-11-07）—— 现在结论只落在会话上，由听书
    /// 屏在播放键旁说明，下一次点开就是重试。没有书（兜底那一屏）依旧不摆。
    #[gpui_kit::test]
    fn the_entry_stays_up_through_every_probe_verdict(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = speech_flow();
        let (view, _handle) = open(cx, &flow, None);

        assert!(
            view.read_with(cx, |session, _| session.speech_entry_visible()),
            "书开着，探测没回来（None）圆钮也摆"
        );
        view.update(cx, |session, _| session.speech.available = Some(false));
        assert!(
            view.read_with(cx, |session, _| session.speech_entry_visible()),
            "终审失败圆钮也不收 —— 下一次点开听书屏就是重试"
        );
        view.update(cx, |session, _| session.speech.available = Some(true));
        assert!(view.read_with(cx, |session, _| session.speech_entry_visible()));

        // 兜底那一屏没有书：同样不摆。
        let sample = cx.update(|cx| cx.new(|cx| ReaderSession::boot(cx)));
        assert!(
            sample.read_with(cx, |session, _| !session.speech_entry_visible()),
            "没有书就不摆"
        );
    }

    /// 句切分：句末标点、连续标点、尾随引号、换行与空白；小数点与 `；` 不切。
    #[test]
    fn sentences_break_on_end_punctuation_but_not_inside_numbers_or_after_semicolons() {
        let text = "他说：“走吧。”然后走了。价格 3.14 元；不算新句！真的？\n最后一行";
        let chars: Vec<char> = text.chars().collect();
        let ranges = split_sentences(&chars, 0..chars.len());
        let got: Vec<String> = ranges
            .iter()
            .map(|range| chars[range.clone()].iter().collect())
            .collect();
        assert_eq!(
            got,
            vec![
                "他说：“走吧。”",
                "然后走了。",
                "价格 3.14 元；不算新句！",
                "真的？",
                "最后一行",
            ]
        );
    }

    /// 长句下刀：超过一档的句在句内停顿标点处再切，每一片都落在停顿上，放得进
    /// 一档的相邻段并回去；短句（≤一档）一个字不动。
    #[test]
    fn a_long_sentence_is_cut_at_clause_pauses_but_a_short_one_is_not() {
        let long = "由于莫雷蒂家家境贫寒，克莱恩生活上非常节俭，再加上克莱恩对金钱的爱好，导致每一便士都如数家珍的性格非常深入人心。";
        let chars: Vec<char> = long.chars().collect();
        let ranges = split_sentences(&chars, 0..chars.len());
        let got: Vec<String> = ranges
            .iter()
            .map(|range| chars[range.clone()].iter().collect())
            .collect();
        assert_eq!(
            got,
            vec![
                "由于莫雷蒂家家境贫寒，克莱恩生活上非常节俭，",
                "再加上克莱恩对金钱的爱好，",
                "导致每一便士都如数家珍的性格非常深入人心。",
            ]
        );

        let short = "克莱恩生活上非常节俭。";
        let chars: Vec<char> = short.chars().collect();
        let ranges = split_sentences(&chars, 0..chars.len());
        assert_eq!(ranges.len(), 1, "短句整句保留");
    }

    /// 千分位逗号不是刀口：`1,000` 必须整段保留，`，` 才是停顿。
    #[test]
    fn thousand_separator_commas_do_not_become_clause_cuts() {
        let text =
            "这件货一共 1,000 件，再加上仓库里剩下的全部存货，统统都要在明天之前如数交割清楚才行。";
        let chars: Vec<char> = text.chars().collect();
        let ranges = split_sentences(&chars, 0..chars.len());
        let got: Vec<String> = ranges
            .iter()
            .map(|range| chars[range.clone()].iter().collect())
            .collect();
        assert_eq!(
            got,
            vec![
                "这件货一共 1,000 件，",
                "再加上仓库里剩下的全部存货，",
                "统统都要在明天之前如数交割清楚才行。",
            ]
        );
    }

    /// 推进的两个纯函数：空段（pretty-print 的空白文本节点）跳过，章尾给 `None`；
    /// 段内的多句按句走，不整段吞。
    #[gpui_kit::test]
    fn empty_paragraphs_are_skipped_and_sentences_advance_one_at_a_time(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = FlowDoc {
            chapters: vec![Chapter::inline(
                "空白章",
                "<p>甲一。甲二！甲三？</p><p>   </p><p>收尾。</p>",
            )],
            ..Default::default()
        };
        let (view, _handle) = open(cx, &flow, None);
        view.read_with(cx, |session, _| {
            let doc = session
                .book()
                .and_then(|book| book.span_doc(0))
                .expect("应有一章");
            let first = sentence_from(doc, 0).expect("首句应可解");
            assert_eq!(doc.text_map().slice(first.clone()), "甲一。");

            let second = next_sentence(doc, first).expect("段内下一句");
            assert_eq!(doc.text_map().slice(second.clone()), "甲二！");

            let third = next_sentence(doc, second).expect("段内最后一句");
            assert_eq!(doc.text_map().slice(third.clone()), "甲三？");

            let fourth = next_sentence(doc, third).expect("下一段（跳过空白段）");
            assert_eq!(doc.text_map().slice(fourth.clone()), "收尾。");

            assert!(next_sentence(doc, fourth).is_none(), "章尾停住");
        });
    }

    /// 抽干那条不能把 `Ready` 一起丢掉：它全进程只发一次，丢了就是「有时候按钮不显示」。
    #[test]
    fn draining_old_events_replays_ready_and_only_ready() {
        assert_eq!(attach_replay(Event::Ready(true)), Some(Event::Ready(true)));
        assert_eq!(
            attach_replay(Event::Ready(false)),
            Some(Event::Ready(false))
        );
        assert_eq!(attach_replay(Event::Begin), None);
        assert_eq!(attach_replay(Event::Done), None);
    }

    /// 可用性结论落在会话上（与它是不是「早于监听到达」无关），听书屏拿它说明现状：
    /// 还没探 / 可用 / 终审失败三格都要能落到 [`ReaderSession::speech_available`]。
    #[gpui_kit::test]
    fn a_ready_that_arrived_before_the_listener_still_lands_on_the_session(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let flow = speech_flow();
        let (view, _handle) = open(cx, &flow, None);
        assert_eq!(
            view.read_with(cx, |session, _| session.speech_available()),
            None,
            "前置：还没点过听书钮"
        );
        // 模拟「Ready 在监听挂上来之前就到了」：直接喂事件，不管它从哪条路来的。
        view.update(cx, |session, cx| {
            session.on_speech_event(Event::Ready(true), cx)
        });
        assert_eq!(
            view.read_with(cx, |session, _| session.speech_available()),
            Some(true),
            "结论到了就落在会话上；从前这一条被抽干吞掉，按钮永久不出现"
        );
        view.update(cx, |session, cx| {
            session.on_speech_event(Event::Ready(false), cx)
        });
        assert_eq!(
            view.read_with(cx, |session, _| session.speech_available()),
            Some(false)
        );
    }

    /// 分页档也跟读：念到下一页的那一句，屏要翻过去，高亮区间跟着走。
    /// 连续滚动那条在 `speaking_a_sentence_in_another_chapter_brings_the_view_along`；
    /// 这一条钉「听书自动翻页」不是滚动模式独有的。
    #[gpui_kit::test]
    fn following_a_sentence_on_a_later_page_turns_the_page(cx: &mut TestAppContext) {
        use crate::ui::features::reader::model::chrome::PageMode;
        use folio_core::infra::config::PageTurn;

        cx.update(gpui_kit::init);
        let body = "<p>这是一段用来把章节撑过一屏的正文，重复许多次。</p>".repeat(30);
        let flow = FlowDoc {
            chapters: vec![Chapter::inline("第一章", &body)],
            ..Default::default()
        };
        let (store, book_id) = store_with_book(&flow);
        let (view, handle) = open(cx, &flow, Some((store, book_id)));
        cx.update(|app| {
            ConfigHolder::edit_book_appearance(app, |a| a.page_turn = PageTurn::Cover);
        });
        settle(cx, handle);
        assert!(
            view.read_with(cx, |session, _| session.mode() == PageMode::Paged),
            "前置：这一屏该站在分页档里"
        );

        // 找一句落在第一页之后的：它的行顶在下一页，屏必须动。
        let (range, target) = view.read_with(cx, |session, _| {
            let doc = session
                .book()
                .and_then(|book| book.span_doc(0))
                .expect("这一章应该在拼接窗口里");
            let mut found = super::sentence_from(doc, 0).expect("至少应有一句");
            loop {
                let page = doc.page_of_line(doc.line_at_offset(found.start));
                if page > 0 {
                    return (found, page);
                }
                found = super::next_sentence(doc, found).expect("后面应该还有句子");
            }
        });

        view.update(cx, |session, cx| {
            session.speak_sentence(0, range.clone(), cx);
        });
        settle(cx, handle);

        assert_eq!(
            view.read_with(cx, |session, _| session.page()),
            target,
            "念到下一页时屏要翻过去"
        );
        assert_eq!(
            view.read_with(cx, |session, _| session.speech_range()),
            Some((0, range)),
            "高亮区间跟着声音走"
        );
    }
}
