//! 移动端的阅读视图：一章的排版结果 → 逐行绘制，加触摸滚动、章节切换与位置落盘。
//!
//! 排版本身**不在这里**，在 [`crate::ui::shared::document`]；这里只做四件事：
//! 按 `scroll_y` 挑出可见行、把文字摆成 [`GlyphRun`]、把图片摆成 `img` 元素、
//! 把读者的偏好推给排版状态。桌面端同一个 `Layout` 走的是虚拟列表，
//! 所以两端的行内容必然一致，只有滚动机制不同。
//!
//! 滚动**不自己做惯量**：`gpui-mobile` 的平台层已经把触摸翻译成了 `ScrollWheel` 事件
//! （`gpui-mobile/src/android/window.rs:1316` 起，用 `momentum::VelocityTracker` 估速度、
//! 用 `MomentumScroller` 在松手后每帧派发一次衰减增量），视图只要把事件里的 dy 累加到
//! `scroll_y`。在这里再套一个 `MomentumScroller` 会把同一份惯量算两遍，滚起来直接翻倍。

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use folio_core::layout::{FontKey, LineFragment};
use folio_core::store::Store;
use folio_core::{Chapter, DocMeta, FlowDoc, Format, Locator, Reader};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    App, Context, Hsla, InteractiveElement, IntoElement, ParentElement, Render, ScrollDelta,
    ScrollWheelEvent, SharedString, Styled, Window, div, img, px,
};

use super::paint::{GlyphRun, GlyphsElement};
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::document::{Document, chapter_xhtml, open};
use crate::ui::shared::images::{BookImages, ChapterImages};
use crate::ui::shared::l10n::tf;
use crate::ui::shared::library::PACKAGE;
use crate::ui::shared::measure::gpui_font;
use crate::ui::widgets::run_color;

/// 视图现在拿的是什么内容。
pub enum Source {
    /// 兜底：一行硬编码文本。找不到任何文档时用它，为的是冷启动失败也能看见画面，
    /// 而不是一片黑 —— 「装上了但读不到书」和「根本没起来」在 logcat 里很难分开。
    Sample(SharedString),
    /// 一本打开的书。
    ///
    /// 装箱是因为两臂差着数量级：`Book` 带着章节表与排版结果（三百多字节），
    /// 而兜底那一行只要 24。不装箱的话，整个 `Source` 连同它外面的视图都按大的那臂算，
    /// 每次赋值都在搬一整本书。
    Book(Box<Book>),
}

/// 一台设备上打开着的一本书：章节表、当前章、以及当前章的排版状态。
pub struct Book {
    title: SharedString,
    chapters: Vec<Chapter>,
    chapter: usize,
    /// 文档格式。Markdown / TXT 的源文本要先包成 XHTML 才进得了排版层。
    format: Format,
    /// 整本书的图片索引，换章时按新章的目录重建视图。
    book_images: Arc<BookImages>,
    document: Document,
}

impl Book {
    /// 从已解析的文档建出一本书，定位到 `locator` 那一章。
    fn new(meta: &DocMeta, flow: FlowDoc, prefs: Reader, locator: Locator) -> Self {
        let chapter = (locator.chapter as usize).min(flow.chapters.len().saturating_sub(1));
        let book_images = Arc::new(BookImages::new(&flow.images));
        let document = Document::new(
            String::new(),
            &flow.css,
            prefs.clone(),
            Arc::new(ChapterImages::empty()),
        );
        let mut this = Self {
            title: meta.title.clone().into(),
            chapters: flow.chapters,
            chapter,
            format: meta.format,
            book_images,
            document,
        };
        this.show_chapter(&prefs);
        this
    }

    /// 把当前章的源文本与图片索引灌进排版状态。
    ///
    /// 图片索引必须跟着换章重建：`<img src>` 是相对本章目录的，
    /// 上一章的基准目录解不出下一章的路径。
    fn show_chapter(&mut self, prefs: &Reader) {
        let Some(chapter) = self.chapters.get(self.chapter) else {
            return;
        };
        let xhtml = chapter_xhtml(chapter, self.format);
        let images = Arc::new(ChapterImages::new(
            self.book_images.clone(),
            &chapter.spine_href,
        ));
        self.document.set_prefs(prefs.clone());
        self.document.set_chapter(xhtml);
        self.document.set_images(images);
    }

    /// 往某个方向换一章。真的换了返回 `true`，到头返回 `false`（按钮据此置灰）。
    pub fn advance(&mut self, delta: isize, prefs: &Reader) -> bool {
        let target = self.chapter as isize + delta;
        if target < 0 || target as usize >= self.chapters.len() {
            return false;
        }
        self.chapter = target as usize;
        self.show_chapter(prefs);
        true
    }

    pub fn chapter(&self) -> usize {
        self.chapter
    }

    /// 跳到第 `index` 章。越界与「已经在这章」都返回 `false` 且不动任何东西 ——
    /// 目录里点当前章不该把读者已经滚到的位置甩回章首。
    pub fn goto(&mut self, index: usize, prefs: &Reader) -> bool {
        if index >= self.chapters.len() || index == self.chapter {
            return false;
        }
        self.chapter = index;
        self.show_chapter(prefs);
        true
    }

    pub fn chapter_count(&self) -> usize {
        self.chapters.len()
    }

    /// 当前章的标题，没有章节表时是空串。
    pub fn chapter_title(&self) -> &str {
        self.chapters
            .get(self.chapter)
            .map(|chapter| chapter.title.as_str())
            .unwrap_or_default()
    }

    pub fn title(&self) -> &SharedString {
        &self.title
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.document
    }
}

/// 可见范围内的一张图：位置与尺寸已由排版算好，这里只负责摆。
struct VisibleImage {
    x: f32,
    top: f32,
    width: f32,
    height: f32,
    image: Arc<gpui_kit::Image>,
}

pub struct ReaderView {
    source: Source,
    /// 上一次同步下来的读者偏好。与 [`ConfigHolder`] 里的不一致就推给排版状态。
    prefs: Reader,
    /// 排版实际用的正文家族：偏好里那个在设备上解析不出来时换成设备有的那个。
    family: SharedString,
    /// 上次读到的偏好家族名。挑设备字体要枚举全部字族，只在名字真变了时做。
    wanted_family: String,
    /// 已滚过的像素（内容坐标，向下为正）。
    scroll_y: f32,
    /// 上一帧的内容总高与**有效**视口高（已减掉 `bottom_reserve`）。滚动事件发生时手上没有
    /// `Window`，夹范围只能用这两个记下来的值，下一帧渲染时再校正一次。
    content_height: f32,
    viewport_height: f32,
    /// 窗口底部被系统外壳占掉的高度（逻辑像素，就是那条导航栏）。
    /// 正文的可见行与滚动上限都按「窗口高减掉它」这个有效视口算，
    /// 否则文档最后这么一截永远滚不进视野。
    bottom_reserve: f32,
    /// 阅读页顶部那行小字（章名）占掉的高度。正文从它下面开始画，
    /// 滚动上限也按它扣 —— 可见行筛选、`max_scroll`、`on_scroll` 的夹范围必须同一个数。
    header_reserve: f32,
    /// 阅读页底部那行小字（进度）占掉的高度。与 `header_reserve` 同理。
    footer_reserve: f32,
    /// 还没落地的位置。恢复必须等排版做过一次才能做（坐标 → 行顶的映射要有行盒），
    /// 而构造时手上没有窗口，所以先记下来，第一帧渲染时取走。
    pending_restore: Option<Locator>,
    /// 上一次落库的章内偏移。滚动的落库按它去重：偏移只在**跨过一行**时才变，
    /// 所以「变了才写」等于「每行至多写一次」，比每帧一次或每次触摸一次都便宜，
    /// 也不需要定时器。
    saved_offset: usize,
    /// 书库。移动端的导入还没有界面，文件是 `adb push` 进应用目录的，
    /// 但位置与书目照常入库，所以杀掉进程回来还在同一章。
    store: Option<Rc<Store>>,
    book_id: Option<i64>,
}

/// 兜底用的那行字。中英混排，顺带把 fallback 路径也验一遍。
const SAMPLE_LINE: &str = "Folio — 中文正文与 Latin 混排的一行";

impl ReaderView {
    /// 冷启动：读偏好、挑设备字体，找得到文档就打开书，否则退回那一行兜底文本。
    pub fn boot(cx: &App) -> Self {
        let prefs = ConfigHolder::reader(cx);
        let found = candidate_paths().into_iter().find(|path| path.is_file());
        let Some(path) = found else {
            log::info!("ReaderView: 没有可读文档，进入兜底的单行文本");
            return Self::sample(&prefs, cx);
        };
        // 书库没装不是 fatal：照样能读，只是进度与偏好落不了盘，
        // 而「读了但没记住」比「直接退出」更可接受。
        let store = crate::ui::shared::app_store::handle(cx);
        match Self::open_document(&path, &prefs, store, cx) {
            Ok(view) => {
                log::info!("ReaderView: 打开 {path:?}");
                view
            }
            Err(err) => {
                log::error!("ReaderView: 打开 {path:?} 失败：{err:#}");
                Self::sample(&prefs, cx)
            }
        }
    }

    /// 只有那一行兜底文本时用。
    fn sample(prefs: &Reader, cx: &App) -> Self {
        let family = device_family(&prefs.font_family, cx);
        let mut effective = prefs.clone();
        effective.font_family = family.clone();
        Self {
            source: Source::Sample(SAMPLE_LINE.into()),
            wanted_family: prefs.font_family.clone(),
            prefs: effective,
            family: family.into(),
            scroll_y: 0.0,
            content_height: 0.0,
            viewport_height: 0.0,
            bottom_reserve: 0.0,
            header_reserve: 0.0,
            footer_reserve: 0.0,
            pending_restore: None,
            saved_offset: 0,
            store: None,
            book_id: None,
        }
    }

    /// 打开一个文档并登记进书库，阅读位置从库里恢复。
    ///
    /// 登记不是为了「有个书架」（移动端的书架还没接），而是因为进度与书目都按
    /// `book_id` 存；不入库就没有可回写的键。同一路径重复导入是更新，不是第二本书，
    /// 所以每次冷启动调它不会把书库越堆越长。
    pub fn open_document(
        path: &Path,
        prefs: &Reader,
        store: Option<Rc<Store>>,
        cx: &App,
    ) -> folio_core::Result<Self> {
        let (meta, flow) = open(path)?;
        let family = device_family(&prefs.font_family, cx);
        let mut effective = prefs.clone();
        effective.font_family = family.clone();

        let book_id = store.as_ref().and_then(|store| {
            let id = store.add_book(path, &meta).ok()?;
            let _ = store.save_flow(id, &flow);
            let _ = store.touch_opened(id);
            Some(id)
        });
        // 恢复失败就当没读过：回到章首，而不是停在某个对不上的位置上。
        let locator = book_id
            .and_then(|id| store.as_ref()?.position(id).ok().flatten())
            .map(|(_, locator)| locator)
            .unwrap_or_else(Locator::start);
        let book = Book::new(&meta, flow, effective.clone(), locator);
        Ok(Self {
            source: Source::Book(Box::new(book)),
            wanted_family: prefs.font_family.clone(),
            prefs: effective,
            family: family.into(),
            // 章已经由 `Book::new` 定好了；章内偏移留到第一帧排版做完再落地。
            pending_restore: (locator.offset > 0).then_some(locator),
            saved_offset: locator.offset,
            scroll_y: 0.0,
            content_height: 0.0,
            viewport_height: 0.0,
            bottom_reserve: 0.0,
            header_reserve: 0.0,
            footer_reserve: 0.0,
            store,
            book_id,
        })
    }

    /// 从书库打开一本书。
    ///
    /// 按主键读库而不是按 `books.path` 去开那个文件：导入时那份副本躺在系统给文件
    /// 选择器的临时目录里，随时会被清掉，而正文、图片、CSS 早在 `save_flow` 时就
    /// 进了 SQLite。书架点开一张卡片要的是「这本书还在」这个保证，不是「那个文件还在」。
    ///
    /// 读不到的一切情况都只写日志、不动当前那一屏：读者正在读的书不该因为点错一本
    /// 坏记录而变成空白页。
    pub fn open_book(&mut self, book_id: i64, cx: &mut Context<Self>) {
        let opened = self.store.as_ref().and_then(|store| {
            let row = store.book(book_id).ok().flatten()?;
            let flow = store.load_flow(book_id).ok().flatten()?;
            let format = Format::parse(&row.format)?;
            let _ = store.touch_opened(book_id);
            let locator = store
                .position(book_id)
                .ok()
                .flatten()
                .map(|(_, locator)| locator)
                .unwrap_or_else(Locator::start);
            Some((
                DocMeta {
                    title: row.title,
                    authors: row.authors,
                    format,
                    size: row.size,
                },
                flow,
                locator,
            ))
        });
        let Some((meta, flow, locator)) = opened else {
            log::error!("ReaderView: 书库里第 {book_id} 本读不出来（章节没入库或格式不认识）");
            return;
        };
        // `self.prefs` 里的家族已经是这台设备解析得出来的那个，不再挑一遍。
        let book = Book::new(&meta, flow, self.prefs.clone(), locator);
        self.source = Source::Book(Box::new(book));
        self.book_id = Some(book_id);
        self.scroll_y = 0.0;
        self.content_height = 0.0;
        self.saved_offset = locator.offset;
        // 章内偏移还是要等第一帧排版做完才能落地。
        self.pending_restore = (locator.offset > 0).then_some(locator);
        cx.notify();
    }

    /// 当前开着的那本书的主键；兜底那一行没有书，返回 `None`。
    /// 外壳用它判断「被删掉的那本是不是正开着的那本」。
    pub fn book_id(&self) -> Option<i64> {
        self.book_id
    }

    pub fn title(&self) -> SharedString {
        match &self.source {
            Source::Sample(_) => "folio".into(),
            Source::Book(book) => book.title().clone(),
        }
    }

    /// 告知底部导航栏的高度（逻辑像素），正文据此缩短自己的有效视口。
    pub(crate) fn set_bottom_reserve(&mut self, height: f32) {
        self.bottom_reserve = height;
    }

    /// 告知阅读页自己那两条带子的高度（页眉的章名、页脚的进度）。
    ///
    /// 与 `set_bottom_reserve` 分成两个入口是因为**归属不同**：那条由外壳每帧报告
    /// （导航栏收起时是 0），这两条由阅读页自己定。合成一个数的话，外壳那一句
    /// 「现在导航栏占 0」会把阅读页自己让出来的两条带一起冲掉。
    pub(crate) fn set_chrome_bands(&mut self, header: f32, footer: f32, cx: &mut Context<Self>) {
        if (self.header_reserve - header).abs() > 0.5 || (self.footer_reserve - footer).abs() > 0.5
        {
            self.header_reserve = header;
            self.footer_reserve = footer;
            cx.notify();
        }
    }

    /// 本章还能往下滚多少像素。视口还没量过（第一帧之前）时是 0。
    fn max_scroll(&self) -> f32 {
        (self.content_height - self.viewport_height).max(0.0)
    }

    /// 本章已读比例。进度条与页脚都念它。
    pub fn progress(&self) -> f32 {
        let max = self.max_scroll();
        if max <= 0.0 {
            0.0
        } else {
            (self.scroll_y / max).clamp(0.0, 1.0)
        }
    }

    /// 按「一屏」翻正文。
    ///
    /// 走 90% 而不是 100%：整屏跳会让上一屏的末行彻底消失，读者找不到接续的那句。
    pub fn scroll_page(&mut self, delta: isize, cx: &mut Context<Self>) {
        let page = self.viewport_height * 0.9;
        self.set_scroll_y(self.scroll_y + delta as f32 * page, cx);
    }

    /// 滚到本章的某个比例处（进度条）。
    pub fn seek_ratio(&mut self, ratio: f32, cx: &mut Context<Self>) {
        let target = ratio.clamp(0.0, 1.0) * self.max_scroll();
        self.set_scroll_y(target, cx);
    }

    /// 两条滚动入口共用的落点：夹进范围、落库、重画。
    fn set_scroll_y(&mut self, target: f32, cx: &mut Context<Self>) {
        let clamped = target.clamp(0.0, self.max_scroll());
        if (clamped - self.scroll_y).abs() < 0.5 {
            return;
        }
        self.scroll_y = clamped;
        // 与手势滚动同一条落库判据：跨过一行才写盘。
        let offset = self.visible_offset();
        if offset != self.saved_offset {
            self.persist_position();
        }
        cx.notify();
    }

    /// 跳到第 `index` 章的章首。
    pub fn goto_chapter(&mut self, index: usize, cx: &mut Context<Self>) {
        let prefs = self.prefs.clone();
        let moved = match &mut self.source {
            Source::Book(book) => book.goto(index, &prefs),
            Source::Sample(_) => false,
        };
        if !moved {
            return;
        }
        self.scroll_y = 0.0;
        self.content_height = 0.0;
        self.pending_restore = None;
        self.persist_position();
        cx.notify();
    }

    /// 章节数（兜底那一行没有章，是 0）。目录面板的列表长度。
    pub fn chapter_count(&self) -> usize {
        match &self.source {
            Source::Book(book) => book.chapter_count(),
            Source::Sample(_) => 0,
        }
    }

    /// 当前章的下标。
    pub fn chapter_index(&self) -> usize {
        match &self.source {
            Source::Book(book) => book.chapter(),
            Source::Sample(_) => 0,
        }
    }

    /// 第 `index` 章的标题。越界是空串，不是 panic —— 列表在换章那一帧可能还按旧长度画。
    pub fn chapter_title_at(&self, index: usize) -> String {
        match &self.source {
            Source::Book(book) => book
                .chapters
                .get(index)
                .map(|chapter| chapter.title.clone())
                .unwrap_or_default(),
            Source::Sample(_) => String::new(),
        }
    }

    /// 「第几节 / 共几节」，比整本书名短得多。顶部那条章导航用它。
    pub fn chapter_position(&self, cx: &App) -> String {
        let Source::Book(book) = &self.source else {
            return String::new();
        };
        if book.chapter_count() == 0 {
            return String::new();
        }
        tf(
            cx,
            "reader.chapter_position",
            &[
                ("index", &(book.chapter() + 1)),
                ("total", &book.chapter_count()),
            ],
        )
    }

    /// 当前章的标题；兜底那一行没有章，返回空串。
    pub fn chapter_title(&self) -> String {
        match &self.source {
            Source::Sample(_) => String::new(),
            Source::Book(book) => book.chapter_title().to_string(),
        }
    }

    /// 能不能往这个方向换章。按钮的 disabled 态由它决定，
    /// 而不是让用户点一下什么也没发生。
    pub fn can_advance(&self, delta: isize) -> bool {
        let Source::Book(book) = &self.source else {
            return false;
        };
        let target = book.chapter() as isize + delta;
        (0..book.chapter_count() as isize).contains(&target)
    }

    /// 换一章：作废滚动位置并把新位置落库。
    ///
    /// 换章后 `scroll_y` 归零，于是落库的章内偏移也就是新章的开头 —— 这是对的，
    /// 不是妥协：读者要求的就是从新的一章开始读。
    pub fn advance_chapter(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Source::Book(book) = &mut self.source else {
            return;
        };
        let prefs = self.prefs.clone();
        if !book.advance(delta, &prefs) {
            return;
        }
        self.scroll_y = 0.0;
        self.content_height = 0.0;
        self.persist_position();
        cx.notify();
    }

    /// 当前这一章的位置写进书库。没有 `book_id`（库打不开）时静默跳过 ——
    /// 进度落盘失败不该让阅读停下来。
    /// 当前屏幕顶那一行的可见文本偏移。排版还没做过时是 0（章首），不是脏数据。
    fn visible_offset(&self) -> usize {
        match &self.source {
            Source::Book(book) => book.document().offset_at_y(self.scroll_y),
            Source::Sample(_) => 0,
        }
    }

    fn persist_position(&mut self) {
        let (Some(store), Some(id), Source::Book(book)) = (&self.store, self.book_id, &self.source)
        else {
            return;
        };
        let locator = Locator {
            chapter: book.chapter() as u32,
            // 可见文本的字符偏移。换算在共用层（与桌面端同一个函数），
            // 所以「滚到哪个像素」这件事在两台上不会各算各的。
            offset: book.document().offset_at_y(self.scroll_y),
        };
        self.saved_offset = locator.offset;
        if let Err(err) = store.set_position(id, "continuous", &locator) {
            log::warn!("persist_position: 写入失败 {err}");
        }
    }

    /// 平台层算好的滚动增量落到 `scroll_y`。
    ///
    /// 触摸向下滑（dy > 0）时内容跟着手指往下走 ⇒ 露出的行号变小，所以是减。
    fn on_scroll(&mut self, event: &ScrollWheelEvent) {
        let dy = match event.delta {
            ScrollDelta::Pixels(delta) => f32::from(delta.y),
            // 鼠标滚轮（外接键盘/调试时）按「行」给增量，按正文字号折算，别当 0 忽略。
            ScrollDelta::Lines(lines) => lines.y * self.prefs.font_size,
        };
        if dy == 0.0 {
            return;
        }
        let max = (self.content_height - self.viewport_height).max(0.0);
        self.scroll_y = (self.scroll_y - dy).clamp(0.0, max);
        // 跨行才落库：进度不是「滚到第几个像素」而是「读到第几个字」，
        // 同一个字里再怎么滑都不必写盘。杀进程回来还在同一行，靠的就是这一步。
        let offset = self.visible_offset();
        if offset != self.saved_offset {
            self.persist_position();
        }
    }

    /// 读者在设置里改了字号、行距、缩进或字体，在这一帧同步进来。
    ///
    /// 每帧读一次 Global 再比较，比给这个视图挂一条订阅更合适：偏好本来就是每帧都要用
    /// 的输入，而订阅者一多，就容易出现「在 `render` 里改状态」那一类被禁止的写法。
    fn sync_prefs(&mut self, cx: &App) {
        let wanted = ConfigHolder::reader(cx);
        if wanted.font_family != self.wanted_family {
            // 偏好换了家族名：重挑一次设备真能解析出来的那一个。
            self.wanted_family = wanted.font_family.clone();
            let family: SharedString = device_family(&wanted.font_family, cx).into();
            self.family = family;
        }
        let mut effective = wanted.clone();
        effective.font_family = self.family.to_string();
        if effective != self.prefs {
            self.prefs = effective.clone();
            if let Source::Book(book) = &mut self.source {
                book.document_mut().set_prefs(effective);
            }
        }
    }

    /// 把 `[scroll_y, scroll_y + 视口高]` 覆盖到的行盒摊成绘制用的 run 与图片列表。
    ///
    /// 几何全部来自排版层，这里只加三个偏移：左边的 `margin_x`（版心在窗口里的位置）、
    /// 向上的 `scroll_y`、以及 `offset_y` —— 页眉那条带子占掉的高度，正文从它下面开始画。
    /// 行内每个 run 用自己的宽度累加定 x；
    /// 基线一律 `line.y + line.baseline` —— 桌面端 `line_element` 先把 run 的
    /// line-height 盒顶摆到 `baseline - (盒高 + ascent − descent)/2`，
    /// 再由 gpui 把基线落回盒内同一个偏移，两头抵消，结果就是这一条线。
    /// 这里自己画字形，所以直接用它。
    ///
    /// 自由函数而不是方法：渲染时要一边可变借用 `self.source`（拿 `&mut Document` 排版）
    /// 一边读 `self.scroll_y`，写成 `&self` 方法会直接撞借用检查器。
    fn visible_fragments(
        document: &Document,
        scroll_y: f32,
        viewport: f32,
        offset_y: f32,
        foreground: Hsla,
        known: &[String],
    ) -> (Vec<GlyphRun>, Vec<VisibleImage>) {
        let prefs = document.prefs();
        let layout = document.layout();
        let top = scroll_y;
        let bottom = top + viewport.max(1.0);
        let family: SharedString = prefs.font_family.clone().into();
        let mut runs = Vec::new();
        let mut images = Vec::new();
        for (ix, line) in layout.lines.iter().enumerate() {
            if line.y + line.height < top || line.y > bottom {
                continue;
            }
            // 行文本住在所属块的那个归一化串里（`LineRun::norm` 是它的字节区间）。
            let Some(block) = layout.block_of_line(ix) else {
                continue;
            };
            let mut x = prefs.margin_x + line.x;
            let baseline = line.y + line.baseline - top + offset_y;
            for fragment in &line.fragments {
                match fragment {
                    LineFragment::Text(run) => {
                        let text = block.text.get(run.norm.clone()).unwrap_or("");
                        if text.is_empty() {
                            x += run.width;
                            continue;
                        }
                        runs.push(
                            GlyphRun::new(
                                SharedString::new(text),
                                gpui_font(&FontKey::clone(&run.style.font), &family, known),
                                run.style.font_size,
                            )
                            // 每条 run 自己的基线：`vertical-align` 折出来的偏移在这里减掉，
                            // 正数（往上抬）就是基线 y 变小。行盒高与行基线已由排版层算过，
                            // 这里只搬运那一个数。
                            .at(x, baseline - run.style.baseline_shift)
                            .color(run_color(run.style.color, foreground))
                            .decorate(run.style.underline, run.style.line_through),
                        );
                        x += run.width;
                    }
                    LineFragment::Image(placed) => {
                        // 图片交给 gpui 自己的 `img` 元素画（纹理上传由它负责），
                        // 底贴基线：盒顶 = 基线 − 图高。解不出资源的 `<img>` 留位不画。
                        if let Some(image) = document.images().render_source(&placed.href) {
                            images.push(VisibleImage {
                                x,
                                top: baseline - placed.height,
                                width: placed.width,
                                height: placed.height,
                                image,
                            });
                        }
                        x += placed.width;
                    }
                }
            }
        }
        (runs, images)
    }
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_prefs(cx);
        let viewport = window.viewport_size();
        // 有效视口 = 窗口高 − 底部导航栏 − 页眉 − 页脚。可见行筛选、滚动上限、
        // `on_scroll` 的夹范围全都从这里取，所以底部那一截内容仍然滚得进来，
        // 也不会滚出多余空白。浮层（顶栏/底栏/面板）压在正文**之上**，不参与这个数：
        // 点一下屏幕字不该跳走。
        let viewport_height = (f32::from(viewport.height)
            - self.bottom_reserve
            - self.header_reserve
            - self.footer_reserve)
            .max(1.0);
        // 内容高度 / 视口任一变化后重新夹一次滚动范围（转屏、换字号都在这一刻生效）。
        let max_scroll = (self.content_height - viewport_height).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
        self.viewport_height = viewport_height;

        let foreground = cx.theme().foreground;
        // 这台设备注册的字族名：`gpui_font` 靠它把书里不存在的 `font-family` 落回偏好字体
        // （`resolve_font` 解析不到是 panic，不是回退）。
        let known = cx.text_system().all_font_names();
        let mut element = GlyphsElement::new().background(cx.theme().background);
        let mut column = div().size_full().relative();
        // `&mut self.source` 只借用这一个字段，所以臂里写 `self.content_height`、
        // 读 `self.scroll_y` 都不冲突（前提是别把 `self` 整个传进函数）。
        match &mut self.source {
            Source::Sample(text) => {
                let font = gpui_font(
                    &FontKey {
                        family: String::new(),
                        weight: 400,
                        italic: false,
                    },
                    &self.family,
                    &known,
                );
                // 单行：左边距 `margin_x`，基线给到页眉下面 `margin_x + font_size` 处。
                element.push(
                    GlyphRun::new(text.clone(), font, self.prefs.font_size)
                        .at(
                            self.prefs.margin_x,
                            self.header_reserve + self.prefs.margin_x + self.prefs.font_size,
                        )
                        .color(foreground),
                );
                self.content_height = 0.0;
            }
            Source::Book(book) => {
                book.document_mut().ensure_layout(window, cx, 0.0);
                // 上次读到第几个字 → 那一行的行顶。只有排版做完了才映射得出，
                // 所以这一步在 `ensure_layout` 之后而不是构造时。
                if let Some(locator) = self.pending_restore.take() {
                    self.scroll_y = book.document().y_at_offset(locator.offset);
                }
                self.content_height = book.document().height();
                let (runs, images) = ReaderView::visible_fragments(
                    book.document(),
                    self.scroll_y,
                    viewport_height,
                    self.header_reserve,
                    foreground,
                    &known,
                );
                element.extend(runs);
                for image in images {
                    column = column.child(
                        div()
                            .absolute()
                            .left(px(image.x))
                            .top(px(image.top))
                            .w(px(image.width))
                            .h(px(image.height))
                            .child(img(image.image).size_full()),
                    );
                }
            }
        }
        column = column.child(element);

        let listener = cx.listener(|this: &mut Self, event: &ScrollWheelEvent, _window, cx| {
            this.on_scroll(event);
            cx.notify();
        });
        div()
            .id("folio-reader")
            .size_full()
            .overflow_hidden()
            .on_scroll_wheel(listener)
            .child(column)
    }
}

/// 挑一个这台设备**真的能解析出来**的正文家族。
///
/// 必须挑，不能照桌面那样直接把偏好名字交给 GPUI：`TextSystem::resolve_font`
/// 在请求家族和它整串回退名都不存在时是 **panic**（`gpui-pre-0.3.8/src/text_system.rs:370`），
/// 而读者的默认偏好 `LXGW WenKai` 只在开发机上装着，那份回退表又全是桌面名字
/// （`.ZedSans` / Helvetica / Segoe UI / Noto Sans / DejaVu Sans / Arial…），
/// 结果就是文档刚打开就 SIGABRT。Android 平台的字族是从 `/system/fonts/` 预载进来的。
///
/// 手上只有 `all_font_names()` 这一个公开判据（真正的 `font_id` 是私有的），所以按名字比。
/// 一个都没命中时把设备报出来的名字整体写进日志 —— 候选表该照数据加长，而不是照猜。
pub(crate) fn device_family(wanted: &str, cx: &App) -> String {
    const PREFERRED: &[&str] = &[
        "LXGW WenKai",      // 随包字体：push 进应用目录后应当优先于系统的
        "Noto Sans CJK SC", // /system/fonts/NotoSansCJK-Regular.ttc 的简体那一面
        "Source Han Sans SC",
        "Noto Sans SC",
        "Microsoft YaHei",
        "PingFang SC",
        "Heiti SC",
        "Droid Sans Fallback", // Android 上覆盖 CJK 的传统兜底家族
        "Roboto",              // 只剩拉丁，但至少不 panic
    ];
    let names = cx.text_system().all_font_names();
    let find = |want: &str| {
        names
            .iter()
            .find(|name| name.eq_ignore_ascii_case(want))
            .cloned()
    };

    if let Some(hit) = find(wanted).or_else(|| PREFERRED.iter().find_map(|want| find(want))) {
        log::info!(
            "device_family: 用 {hit:?}（设备注册了 {} 个家族）",
            names.len()
        );
        return hit;
    }
    log::error!(
        "候选家族一个都没命中；设备注册的 {n} 个名字：{names:?}",
        n = names.len()
    );
    // 兜底也必须是一个真实存在的名字，否则下一次 resolve 还是 panic。
    names
        .into_iter()
        .next()
        .unwrap_or_else(|| wanted.to_string())
}

/// 冷启动时依次试探的文档位置。
///
/// 首选**应用专属外部目录** `/sdcard/Android/data/<包名>/files/`：它的属主就是本应用，
/// 所以不需要任何存储权限，`adb push` 也写得进去。`/sdcard/Download/` 排在后面而不是
/// 前面 —— 那里的文件属主是媒体提供者（另一个 uid），scoped storage 下我们的进程读到
/// 的是 EACCES，放对了文件名也不会出现画面。`FOLIO_MOBILE_DOC` 留给调试时指任意路径。
fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(path) = std::env::var("FOLIO_MOBILE_DOC") {
        out.push(PathBuf::from(path));
    }
    for dir in [
        format!("/sdcard/Android/data/{PACKAGE}/files"),
        format!("/storage/emulated/0/Android/data/{PACKAGE}/files"),
        format!("/data/user/0/{PACKAGE}/files"),
        "/sdcard/Download".to_string(),
    ] {
        for name in ["folio-sample.md", "folio-sample.txt", "folio-sample.epub"] {
            out.push(PathBuf::from(format!("{dir}/{name}")));
        }
    }
    out
}

impl crate::ui::features::reader::ReaderHost for ReaderView {
    fn book_id(&self) -> Option<i64> {
        self.book_id
    }

    fn title(&self) -> gpui_kit::SharedString {
        ReaderView::title(self)
    }

    fn position_label(&self, cx: &gpui_kit::App) -> String {
        let title = self.chapter_title();
        let position = self.chapter_position(cx);
        match (title.is_empty(), position.is_empty()) {
            (false, false) => format!("{title} · {position}"),
            (false, true) => title,
            (true, false) => position,
            (true, true) => String::new(),
        }
    }

    fn mode_name(&self) -> &'static str {
        "continuous"
    }

    fn can_advance(&self, delta: isize) -> bool {
        ReaderView::can_advance(self, delta)
    }

    fn advance_chapter(&mut self, delta: isize, cx: &mut gpui_kit::Context<Self>) {
        ReaderView::advance_chapter(self, delta, cx);
    }

    fn set_reserve(&mut self, reserve: f32, _cx: &mut gpui_kit::Context<Self>) {
        self.set_bottom_reserve(reserve);
    }

    fn persist(&mut self, _cx: &mut gpui_kit::Context<Self>) {
        self.persist_position();
    }
}
