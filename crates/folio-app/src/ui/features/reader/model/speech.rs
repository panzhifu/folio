//! 听书会话：按句播放、按原文字符坐标跟读；暂停或失败均留在当前句。

use std::ops::Range;
use std::time::{Duration, Instant};

use folio_core::speech::{Command, Event};
use folio_core::{Engine, Locator};
use gpui_kit::{Context, Task};

use crate::ui::features::reader::model::chapter_layout::Document;
use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::features::reader::model::speech_routing::voice_for_paragraph;
use crate::ui::services::config::ConfigHolder;

/// 新会话只重放就绪状态，丢弃上一会话的播放事件。
fn attach_replay(event: &Event) -> bool {
    matches!(event, Event::Ready(_))
}

/// 播放状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::ui::features::reader) enum Listen {
    Idle,
    Playing {
        chapter: usize,
        range: Range<usize>,
    },
    /// 恢复从句首重读，后端没有句内暂停。
    Paused {
        chapter: usize,
        range: Range<usize>,
    },
}

pub(in crate::ui::features::reader) struct Speech {
    engine: Option<Engine>,
    /// None = 未准备或正在初始化。
    available: Option<bool>,
    pub(in crate::ui::features::reader) state: Listen,
    speaking_since: Option<Instant>,
    /// 归一到 1× 的实测字/秒；切换声音后失效。
    chars_per_sec: Option<f32>,
    voice: Option<folio_core::speech::VoiceKey>,
    error: Option<String>,
    /// 跨章排版完成后续读；暂停、失败和换声取消。
    pending_chapter_start: bool,
    /// 仅当前阅读会话消费播放事件；drop 取消任务。
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
            voice: None,
            error: None,
            pending_chapter_start: false,
            listening: None,
        }
    }
}

/// 只接受 0.5–120 秒样本，以 0.3 权重更新归一语速。
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
    /// 接入进程级引擎，失败后重试并替换旧监听任务。
    pub(in crate::ui::features::reader) fn speech_attach(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = crate::ui::services::speech::engine(cx) else {
            self.speech.available = Some(false);
            return;
        };
        if self.speech.listening.is_some()
            && self
                .speech
                .engine
                .as_ref()
                .is_some_and(|old| old.same_instance(&engine))
        {
            return;
        }
        self.speech.error = engine.snapshot().last_error;
        self.speech.available = engine.ready();
        self.speech.engine = Some(engine.clone());
        let rx = engine.events();
        let weak = cx.entity().downgrade();
        self.speech.listening = Some(cx.spawn(async move |_, cx| {
            while let Ok(delivery) = rx.try_recv() {
                if attach_replay(&delivery.event) {
                    let Ok(()) = weak.update(cx, |session, cx| {
                        session.on_speech_delivery(&engine, delivery, cx)
                    }) else {
                        return;
                    };
                }
            }
            while let Ok(delivery) = rx.recv().await {
                let Ok(()) = weak.update(cx, |session, cx| {
                    session.on_speech_delivery(&engine, delivery, cx)
                }) else {
                    return;
                };
            }
        }));
        cx.notify();
    }

    fn on_speech_delivery(
        &mut self,
        engine: &Engine,
        delivery: folio_core::speech::Delivery,
        cx: &mut Context<Self>,
    ) {
        if self
            .speech
            .engine
            .as_ref()
            .is_some_and(|active| active.same_instance(engine))
            && engine.accepts(&delivery)
        {
            self.on_speech_event(delivery.event, cx);
        }
    }

    /// 可重排正文始终显示入口，准备与失败状态在听书页反馈。
    pub fn speech_entry_visible(&self) -> bool {
        self.book().is_some()
    }

    pub fn speech_playing(&self) -> bool {
        matches!(self.speech.state, Listen::Playing { .. })
    }

    pub(in crate::ui::features::reader) fn speech_available(&self) -> Option<bool> {
        self.speech.available
    }

    pub(in crate::ui::features::reader) fn speech_error(&self) -> Option<&str> {
        self.speech.error.as_deref()
    }

    /// 实测字/秒（1×）；无样本时由视图使用兜底值。
    pub(in crate::ui::features::reader) fn speech_chars_per_sec(&self) -> Option<f32> {
        self.speech.chars_per_sec
    }

    pub(in crate::ui::features::reader) fn speech_range(&self) -> Option<(usize, Range<usize>)> {
        match &self.speech.state {
            Listen::Playing { chapter, range } | Listen::Paused { chapter, range } => {
                Some((*chapter, range.clone()))
            }
            Listen::Idle => None,
        }
    }

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

    /// 打开听书页只定位当前句；已暂停的位置不变，不自动发声。
    pub(in crate::ui::features::reader) fn arm_speech(&mut self, cx: &mut Context<Self>) {
        if !self.speech_entry_visible() {
            return;
        }
        if self.speech.available == Some(false) {
            crate::ui::services::speech::refresh(cx);
        }
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

    /// 关闭或换书时停止，并以当前句起点保存阅读位置。
    pub(in crate::ui::features::reader) fn stop_speech(&mut self) {
        self.speech.pending_chapter_start = false;
        self.speech.speaking_since = None;
        self.speech.error = None;
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
        self.sync_speech_remote();
    }

    pub(in crate::ui::features::reader) fn on_speech_event(
        &mut self,
        event: Event,
        cx: &mut Context<Self>,
    ) {
        match event {
            Event::Ready(ok) => {
                self.speech.available = Some(ok);
                if !ok {
                    self.pause_speech();
                    self.speech.error = self
                        .speech
                        .engine
                        .as_ref()
                        .and_then(|engine| engine.snapshot().last_error);
                }
                cx.notify();
            }
            Event::Failed(reason) => {
                self.pause_speech();
                self.speech.error = Some(reason);
                cx.notify();
            }
            Event::VoiceChanged => {
                self.pause_speech();
                self.speech.chars_per_sec = None;
                self.speech.voice = None;
                cx.notify();
            }
            Event::Begin => {
                if self.speech_playing() {
                    self.speech.speaking_since = Some(Instant::now());
                }
            }
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
        self.speech.pending_chapter_start = false;
        if let Listen::Playing { chapter, range } = self.speech.state.clone() {
            if let Some(engine) = &self.speech.engine {
                engine.send(Command::Stop);
            }
            self.speech.state = Listen::Paused { chapter, range };
            // 暂停的那段不算进这一句的实测时长：恢复时 `speak_sentence` 会重新记起点。
            self.speech.speaking_since = None;
            self.sync_speech_remote();
        }
    }

    /// 跟读仅在跨段或当前句不可见时移动，位置仍使用原文字符坐标。
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
            tracing::info!(
                chapter,
                at = range.start,
                same_paragraph,
                "跟读：把屏带过去"
            );
            if self.chapter_index() == chapter {
                self.jump_to_offset(range.start, cx);
            } else {
                self.goto_offset(chapter, range.start, cx);
            }
        }
        let spoken = self
            .book()
            .and_then(|book| book.span_doc(chapter))
            .map(|doc| {
                let text = doc.text_map().slice(range.clone());
                let paragraph = doc
                    .paragraph_range(range.start)
                    .map(|span| doc.text_map().slice(span))
                    .unwrap_or_else(|| text.clone());
                (text, paragraph)
            });
        self.speech.state = Listen::Playing { chapter, range };
        self.speech.error = None;
        if let (Some(engine), Some((text, paragraph))) = (&self.speech.engine, spoken) {
            let settings = ConfigHolder::speech(cx);
            let snapshot = engine.snapshot();
            let voice = voice_for_paragraph(&settings, &paragraph, &snapshot.voices);
            let effective = folio_core::speech::resolve_voice(
                &snapshot.voices,
                voice.as_ref(),
                &settings.language,
                snapshot.default_voice.as_ref(),
            )
            .map(|voice| voice.key.clone());
            if self.speech.voice != effective {
                self.speech.chars_per_sec = None;
                self.speech.voice = effective;
            }
            engine.send(Command::SpeakWithVoice {
                text,
                voice,
                language: settings.language,
                rate: settings.rate,
                pitch: settings.pitch,
            });
            self.speech.speaking_since = None;
            self.sync_speech_remote();
        }
        cx.notify();
    }

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

    /// 排版无完成事件，100 ms 轮询，至多等待五秒。
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

/// 句末标点。`；` 与 ASCII 的 `.` 不在内：前者是并列分句（长句下刀的刀口，见
/// [`CLAUSE_ENDERS`]），后者要看后面是不是空白（小数点、缩写不切，英文句末才切）。
const SENTENCE_ENDERS: &[char] = &['。', '！', '？', '!', '?', '…'];

/// 句末标点后面跟的引号 / 括号归前一句：`他说：“走吧。”` 的 `。”` 不能拆到下一句。
const CLOSERS: &[char] = &[
    '”', '’', '」', '』', '）', ')', '】', '》', '〕', '］', ']', '"', '\'',
];

/// 长句按停顿标点切分的目标字符数，不强制截断无停顿长片段。
const SENTENCE_PIECE_MAX: usize = 24;

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

/// 章内字符区间；保留标点与闭合引号，剔除空白，并按停顿拆分长句。
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
    use std::sync::Arc;

    use folio_core::ChapterExt;
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

    #[gpui_kit::test]
    fn queued_speech_done_and_replaced_engine_cannot_change_current_sentence(
        cx: &mut TestAppContext,
    ) {
        use folio_core::speech::{Command, Delivery, Engine};
        cx.update(gpui_kit::init);
        let (view, _) = open(cx, &speech_flow(), None);
        let engine = Engine::with_backend(Arc::new(|_| Err("test backend unavailable".into())));
        let old = Delivery {
            generation: Some(engine.playback_generation()),
            event: Event::Done,
        };
        engine.send(Command::Stop);
        view.update(cx, |session, cx| {
            session.speech.engine = Some(engine.clone());
            session.speech.state = Listen::Playing {
                chapter: 0,
                range: 0..4,
            };
            session.speech.available = Some(true);
            session.on_speech_delivery(&engine, old, cx);
            assert_eq!(session.speech_range(), Some((0, 0..4)));
            assert!(session.speech_playing());
            let replaced = Engine::with_backend(Arc::new(|_| Err("old backend".into())));
            session.on_speech_delivery(
                &replaced,
                Delivery {
                    generation: None,
                    event: Event::Ready(false),
                },
                cx,
            );
            assert_eq!(session.speech_available(), Some(true));
            replaced.shutdown();
        });
        engine.shutdown();
    }

    #[gpui_kit::test]
    fn speech_failure_and_voice_change_keep_the_current_sentence(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, _) = open(cx, &speech_flow(), None);
        view.update(cx, |session, cx| {
            session.speech.state = Listen::Playing {
                chapter: 0,
                range: 0..4,
            };
            session.speech.pending_chapter_start = true;
            session.speech.chars_per_sec = Some(8.0);
            session.on_speech_event(Event::Failed("设备拒绝合成".into()), cx);
            assert_eq!(
                session.speech.state,
                Listen::Paused {
                    chapter: 0,
                    range: 0..4
                }
            );
            assert_eq!(session.speech_error(), Some("设备拒绝合成"));
            assert!(!session.speech.pending_chapter_start);
            session.on_speech_event(Event::Done, cx);
            assert_eq!(session.speech_range(), Some((0, 0..4)));
            session.on_speech_event(Event::VoiceChanged, cx);
            assert_eq!(session.speech_chars_per_sec(), None);
            assert_eq!(session.speech_range(), Some((0, 0..4)));
            assert!(!session.speech_playing());
        });
    }

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

    #[test]
    fn draining_old_events_replays_ready_and_only_ready() {
        assert!(attach_replay(&Event::Ready(true)));
        assert!(attach_replay(&Event::Ready(false)));
        assert!(!attach_replay(&Event::Begin));
        assert!(!attach_replay(&Event::Done));
    }

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
