//! 阅读视图：一章的排版结果 → 逐行画到屏上，加滚动 / 翻页、章节切换与位置落盘。
//!
//! 排版**不在这里**，在 [`crate::ui::shared::document`]；这里只做四件事：按当前模式挑出
//! 要摆的那几「片」、把文字摆成 [`GlyphRun`]、把图片摆成 `img` 元素、把读者的偏好推给
//! 排版状态。**两端一份实现**：桌面以前那套「`TextView` + 虚拟列表」与移动这套自绘字形
//! 摆的是同一份 `Layout`，留两份就意味着同一本书在两台上可能各排各的行；而只有这一份拿得到
//! 字形级的控制（选择高亮、ruby、竖排都从 `paint` 那一层长出来）。
//!
//! 三种模式只差 [`Viewport`] 的那三个数，切片由核心分片器给出：
//!
//! - **连续滚动**：一片装完全部，位置是 `scroll_y` 这个像素数。手机上默认这一档。
//! - **分页**：一片 = 一个视口高、一页一栏，位置是页号。
//! - **双页对开**：一片 = 一栏、一页两栏，位置还是页号（一号就是「一张纸」）。
//!
//! 宽度装不下两栏时双页自动退回分页（判据在 [`chrome::PageMode::effective`]，按宽度而不是
//! 按 target），所以横屏的平板真能双页，竖屏手机不会长出两条窄柱子。
//!
//! 滚动**不自己做惯量**：`gpui-mobile` 的平台层已经把触摸翻译成了 `ScrollWheel` 事件
//! （`gpui-mobile/src/android/window.rs:1316` 起，用 `momentum::VelocityTracker` 估速度、
//! 用 `MomentumScroller` 在松手后每帧派发一次衰减增量），视图只要把事件里的 dy 累加到
//! `scroll_y`。在这里再套一个 `MomentumScroller` 会把同一份惯量算两遍，滚起来直接翻倍。

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use folio_core::layout::fragment::column_offset;
use folio_core::layout::{FontKey, LineFragment};
use folio_core::store::Store;
use folio_core::{Chapter, DocMeta, FlowDoc, Format, Locator, Reader};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{
    AnyElement, App, Context, Hsla, InteractiveElement, IntoElement, ParentElement, Render,
    ScrollDelta, ScrollWheelEvent, SharedString, Styled, TestSupportExt as _, Window, div, img, px,
};

use super::chrome::{self, PageMode, Reserve};
use super::paint::{GlyphRun, GlyphsElement};
use crate::ui::host::device_family;
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::document::{Document, Viewport, chapter_xhtml, open};
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
    fn advance(&mut self, delta: isize, prefs: &Reader) -> bool {
        let target = self.chapter as isize + delta;
        if target < 0 || target as usize >= self.chapters.len() {
            return false;
        }
        self.chapter = target as usize;
        self.show_chapter(prefs);
        true
    }

    /// 跳到第 `index` 章。越界与「已经在这章」都返回 `false` 且不动任何东西 ——
    /// 目录里点当前章不该把读者已经滚到的位置甩回章首。
    fn goto(&mut self, index: usize, prefs: &Reader) -> bool {
        if index >= self.chapters.len() || index == self.chapter {
            return false;
        }
        self.chapter = index;
        self.show_chapter(prefs);
        true
    }

    /// 当前章的标题，没有章节表时是空串。
    fn chapter_title(&self) -> &str {
        self.chapters
            .get(self.chapter)
            .map(|chapter| chapter.title.as_str())
            .unwrap_or_default()
    }

    fn title(&self) -> &SharedString {
        &self.title
    }

    fn document(&self) -> &Document {
        &self.document
    }

    fn document_mut(&mut self) -> &mut Document {
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

/// 上一帧真正摆上屏的那几个数。
///
/// 存下来是因为翻页、进度、落库都发生在**渲染之外**（滚轮事件、滑条、点目录），那时手上
/// 没有 `Window`，重新推一遍「现在几栏、共几页」就要再量一次版心，而版心是渲染量出来的。
#[derive(Clone, Copy, Debug)]
struct LaidOut {
    mode: PageMode,
    columns: usize,
    pages: usize,
    /// 一个视口高，分页时页号的换算要用。
    viewport: f32,
}

impl Default for LaidOut {
    fn default() -> Self {
        Self {
            mode: PageMode::Continuous,
            columns: 1,
            pages: 1,
            viewport: 1.0,
        }
    }
}

pub struct ReaderView {
    source: Source,
    /// 上一次同步下来的读者偏好。与 [`ConfigHolder`] 里的不一致就推给排版状态。
    prefs: Reader,
    /// 排版实际用的正文家族：偏好里那个在设备上解析不出来时换成设备有的那个。
    family: SharedString,
    /// 上次读到的偏好家族名。挑设备字体要枚举全部字族，只在名字真变了时做。
    wanted_family: String,
    /// 读者**选**的那一档。装不下双页时实际生效的是分页，见 [`ReaderView::mode`]。
    mode: PageMode,
    laid_out: LaidOut,
    /// 已滚过的像素（内容坐标，向下为正）。只在连续滚动里有意义。
    scroll_y: f32,
    /// 分页 / 双页的当前页号（一张纸一号，两栏算同一号）。
    page: usize,
    /// 滚轮攒下的增量：分页模式下翻一页要攒够半屏，不然一次惯性滚动能翻过去三页。
    wheel_accum: f32,
    /// 上一帧的内容总高。与 `laid_out.viewport` 一起决定连续滚动的可滚上限。
    content_height: f32,
    /// 外壳在不属于正文那一侧占掉的量（桌面 = 侧栏宽，移动 = 底栏高）。
    reserve: Reserve,
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
            mode: PageMode::default(),
            laid_out: LaidOut::default(),
            scroll_y: 0.0,
            page: 0,
            wheel_accum: 0.0,
            content_height: 0.0,
            reserve: Reserve::ZERO,
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
        Ok(Self::with_book(
            &meta,
            flow,
            prefs.clone(),
            store,
            book_id,
            locator,
            cx,
        ))
    }

    /// 从已解析好的文档建视图，不碰书库。
    ///
    /// 公开是因为**宿主的测试要能不开文件就打开一本书**：坐标、分页、退档那几条门读的是
    /// 这条链子，而链子上最贵的一环（解 ZIP、读盘）对它们没有任何影响。
    pub fn from_flow(
        meta: &DocMeta,
        flow: FlowDoc,
        prefs: Reader,
        locator: Locator,
        cx: &App,
    ) -> Self {
        Self::with_book(meta, flow, prefs, None, None, locator, cx)
    }

    /// 上面两条共用的那段：挑字体、建 `Book`、挂上待恢复的位置。
    fn with_book(
        meta: &DocMeta,
        flow: FlowDoc,
        prefs: Reader,
        store: Option<Rc<Store>>,
        book_id: Option<i64>,
        locator: Locator,
        cx: &App,
    ) -> Self {
        let family = device_family(&prefs.font_family, cx);
        let mut effective = prefs.clone();
        effective.font_family = family.clone();
        let wanted_mode = book_id
            .and_then(|id| store.as_ref()?.position(id).ok().flatten())
            .map_or(PageMode::Continuous, |(mode, _)| {
                PageMode::from_storage_name(&mode)
            });
        let book = Book::new(meta, flow, effective.clone(), locator);
        Self {
            source: Source::Book(Box::new(book)),
            wanted_family: prefs.font_family.clone(),
            prefs: effective,
            family: family.into(),
            mode: wanted_mode,
            laid_out: LaidOut::default(),
            scroll_y: 0.0,
            page: 0,
            wheel_accum: 0.0,
            content_height: 0.0,
            reserve: Reserve::ZERO,
            header_reserve: 0.0,
            footer_reserve: 0.0,
            // 章已经由 `Book::new` 定好了；章内偏移留到第一帧排版做完再落地。
            pending_restore: (locator.offset > 0).then_some(locator),
            saved_offset: locator.offset,
            store,
            book_id,
        }
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
            let (mode, locator) = store
                .position(book_id)
                .ok()
                .flatten()
                .unwrap_or((String::new(), Locator::start()));
            Some((
                DocMeta {
                    title: row.title,
                    authors: row.authors,
                    format,
                    size: row.size,
                },
                flow,
                mode,
                locator,
            ))
        });
        let Some((meta, flow, mode, locator)) = opened else {
            log::error!("ReaderView: 书库里第 {book_id} 本读不出来（章节没入库或格式不认识）");
            return;
        };
        // `self.prefs` 里的家族已经是这台设备解析得出来的那个，不再挑一遍。
        let book = Book::new(&meta, flow, self.prefs.clone(), locator);
        // 上次是以哪一档读的，这次还用那一档：进度存的是「第几个字」，与模式无关，
        // 但读者认得的是那**一张纸**的样子。
        self.mode = PageMode::from_storage_name(&mode);
        self.source = Source::Book(Box::new(book));
        self.book_id = Some(book_id);
        self.scroll_y = 0.0;
        self.page = 0;
        self.wheel_accum = 0.0;
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

    /// 「章标题 · 章内位置」，给顶栏与状态栏用。
    pub fn position_label(&self, cx: &App) -> String {
        let title = self.chapter_title();
        let position = self.chapter_position(cx);
        match (title.is_empty(), position.is_empty()) {
            (false, false) => format!("{title} · {position}"),
            (false, true) => title,
            (true, false) => position,
            (true, true) => String::new(),
        }
    }

    /// 外壳报告不属于正文的那条边占掉多少（桌面占宽、移动占高）。
    /// 没变就不 notify，避免父子互相触发。
    pub fn set_reserve(&mut self, reserve: Reserve, cx: &mut Context<Self>) {
        if (self.reserve.width - reserve.width).abs() > 0.5
            || (self.reserve.height - reserve.height).abs() > 0.5
        {
            self.reserve = reserve;
            cx.notify();
        }
    }

    /// 告知阅读页自己那两条带子的高度（页眉的章名、页脚的进度）。
    ///
    /// 与 [`ReaderView::set_reserve`] 分成两个入口是因为**归属不同**：那条由外壳每帧报告
    /// （导航栏收起时是 0），这两条由阅读页自己定。合成一个数的话，外壳那一句
    /// 「现在导航栏占 0」会把阅读页自己让出来的两条带一起冲掉。
    pub fn set_chrome_bands(&mut self, header: f32, footer: f32, cx: &mut Context<Self>) {
        if (self.header_reserve - header).abs() > 0.5 || (self.footer_reserve - footer).abs() > 0.5
        {
            self.header_reserve = header;
            self.footer_reserve = footer;
            cx.notify();
        }
    }

    /// 上一帧实际生效的模式（双页装不下时是分页）。进度、翻页、落库都照它走。
    pub fn mode(&self) -> PageMode {
        self.laid_out.mode
    }

    /// 读者**选**的那一档。按钮的选中态读它，不然在手机上点「双页」会立刻弹回去。
    pub fn wanted_mode(&self) -> PageMode {
        self.mode
    }

    /// 本章的页数；连续滚动时这个数没有意义（进度按像素走）。
    pub fn page_count(&self) -> usize {
        self.laid_out.pages
    }

    /// 当前页号。
    pub fn page(&self) -> usize {
        self.page
    }

    /// 本章还能往下滚多少像素。视口还没量过（第一帧之前）时是 0。
    fn max_scroll(&self) -> f32 {
        (self.content_height - self.laid_out.viewport).max(0.0)
    }

    /// 本章已读比例。进度条与页脚都念它。
    pub fn progress(&self) -> f32 {
        if self.laid_out.mode.is_continuous() {
            let max = self.max_scroll();
            if max <= 0.0 {
                0.0
            } else {
                (self.scroll_y / max).clamp(0.0, 1.0)
            }
        } else {
            let last = self.laid_out.pages.saturating_sub(1);
            if last == 0 {
                0.0
            } else {
                (self.page as f32 / last as f32).clamp(0.0, 1.0)
            }
        }
    }

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
    fn set_scroll_y(&mut self, target: f32, cx: &mut Context<Self>) {
        let clamped = target.clamp(0.0, self.max_scroll());
        if (clamped - self.scroll_y).abs() < 0.5 {
            return;
        }
        self.scroll_y = clamped;
        self.persist_if_moved();
        cx.notify();
    }

    /// 翻到第 `target` 页（越界夹到最后一页）。页号变了才落库、才重画。
    fn set_page(&mut self, target: usize, cx: &mut Context<Self>) {
        let target = target.min(self.laid_out.pages.saturating_sub(1));
        if target == self.page {
            return;
        }
        self.page = target;
        self.wheel_accum = 0.0;
        self.persist_if_moved();
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
        self.page = 0;
        self.wheel_accum = 0.0;
        self.content_height = 0.0;
        self.pending_restore = None;
        self.persist_position();
        cx.notify();
    }

    /// 章节数（兜底那一行没有章，是 0）。目录面板的列表长度。
    pub fn chapter_count(&self) -> usize {
        match &self.source {
            Source::Book(book) => book.chapters.len(),
            Source::Sample(_) => 0,
        }
    }

    /// 当前章的下标。
    pub fn chapter_index(&self) -> usize {
        match &self.source {
            Source::Book(book) => book.chapter,
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
        if book.chapters.is_empty() {
            return String::new();
        }
        tf(
            cx,
            "reader.chapter_position",
            &[
                ("index", &(book.chapter + 1)),
                ("total", &book.chapters.len()),
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
        let target = book.chapter as isize + delta;
        (0..book.chapters.len() as isize).contains(&target)
    }

    /// 换一章：作废滚动位置并把新位置落库。
    ///
    /// 换章后 `scroll_y` 与页号都归零，于是落库的章内偏移也就是新章的开头 —— 这是对的，
    /// 不是妥协：读者要求的就是从新的一章开始读。
    pub fn advance_chapter(&mut self, delta: isize, cx: &mut Context<Self>) {
        let prefs = self.prefs.clone();
        let moved = match &mut self.source {
            Source::Book(book) => book.advance(delta, &prefs),
            Source::Sample(_) => false,
        };
        if !moved {
            return;
        }
        self.scroll_y = 0.0;
        self.page = 0;
        self.wheel_accum = 0.0;
        self.content_height = 0.0;
        self.pending_restore = None;
        self.persist_position();
        cx.notify();
    }

    // ---- 给状态栏与宿主测试读的排版结果 ----

    /// 本章可见文本的字数（锚点坐标系的长度）。
    pub fn text_length(&self) -> usize {
        match &self.source {
            Source::Book(book) => book.document.text_map().chars(),
            Source::Sample(_) => 0,
        }
    }

    /// 排版出来的行盒。
    pub fn lines(&self) -> Vec<folio_core::layout::LineBox> {
        match &self.source {
            Source::Book(book) => book.document.lines().to_vec(),
            Source::Sample(_) => Vec::new(),
        }
    }

    /// 每行的可见文本，与 [`ReaderView::lines`] 一一对应。
    pub fn line_texts(&self) -> Vec<String> {
        match &self.source {
            Source::Book(book) => book.document.line_texts(),
            Source::Sample(_) => Vec::new(),
        }
    }

    /// 恢复请求是否还挂着（false 表示已落地或被放弃）。
    pub fn has_pending_restore(&self) -> bool {
        self.pending_restore.is_some()
    }

    /// 跳到章内第 `offset` 个字所在的那一屏。目录跳转、书签、恢复进度共用这一个入口。
    pub fn jump_to_offset(&mut self, offset: usize, cx: &mut Context<Self>) {
        let Source::Book(book) = &self.source else {
            return;
        };
        let line = book.document.line_at_offset(offset);
        if self.laid_out.mode.is_continuous() {
            self.scroll_y = book.document.y_at_offset(offset);
        } else {
            self.page = book
                .document()
                .page_of_line(line)
                .min(self.laid_out.pages.saturating_sub(1));
        }
        self.persist_position();
        cx.notify();
    }

    /// 第 `page` 页第 `column` 栏覆盖的行区间（排版行下标）。
    ///
    /// 渲染按它挑行，测试按它钉「双页的左右两栏首尾相接」—— 这一条以前只能上真机猜，
    /// 现在两端同一份代码，宿主就测得住。
    pub fn sheet_range(&self, page: usize, column: usize) -> Option<Range<usize>> {
        // 必须先挡这一条：单栏时「片下标 = page」，`column == 1` 会指到**下一页**的第一栏，
        // 看着像有内容、其实是隔壁那张纸。
        if column >= self.laid_out.columns {
            return None;
        }
        let Source::Book(book) = &self.source else {
            return None;
        };
        book.document
            .slice_of(page, column, self.laid_out.columns)
            .map(|slice| slice.lines.clone())
    }

    /// 当前屏幕上顶那一条的可见文本偏移。
    ///
    /// 连续滚动读视口顶；分页 / 双页读这一页第一栏的首行 —— 空片（作者强制断点补出来的）
    /// 跳过，全空才给章首。三种模式收敛到同一个数，进度与批注才在同一套坐标上。
    fn current_offset(&self) -> Option<usize> {
        let Source::Book(book) = &self.source else {
            return None;
        };
        if self.laid_out.mode.is_continuous() {
            return Some(book.document.offset_at_y(self.scroll_y));
        }
        let columns = self.laid_out.columns.max(1);
        for column in 0..columns {
            let Some(slice) = book.document.slice_of(self.page, column, columns) else {
                break;
            };
            if slice.empty {
                continue;
            }
            return Some(book.document.coordinate_of_line(slice.lines.start));
        }
        Some(0)
    }

    /// 字偏移跨过了一行才写盘：进度不是「滚到第几个像素」而是「读到第几个字」，
    /// 同一个字里再怎么滑都不必写盘。杀进程回来还在同一行，靠的就是这一步。
    fn persist_if_moved(&mut self) {
        if self
            .current_offset()
            .is_some_and(|offset| offset != self.saved_offset)
        {
            self.persist_position();
        }
    }

    /// 当前这一章的位置写进书库。没有 `book_id`（库打不开）时静默跳过 ——
    /// 进度落盘失败不该让阅读停下来。
    pub fn persist_position(&mut self) {
        let (Some(store), Some(id)) = (&self.store, self.book_id) else {
            return;
        };
        let locator = Locator {
            chapter: self.chapter_index() as u32,
            offset: self.current_offset().unwrap_or_default(),
        };
        self.saved_offset = locator.offset;
        let mode = self.laid_out.mode.storage_name();
        if let Err(err) = store.set_position(id, mode, &locator) {
            log::warn!("persist_position: 写入失败 {err}");
        }
    }

    /// 平台层算好的滚动增量落到正文。
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

    /// 读者在设置里改了字号、行距、缩进或字体，在这一帧同步进来。
    ///
    /// 每帧读一次 Global 再比较，比给这个视图挂一条订阅更合适：偏好本来就是每帧都要用
    /// 的输入，而订阅者一多，就容易出现「在 `render` 里改状态」那一类被禁止的写法。
    fn sync_prefs(&mut self, cx: &App) {
        let wanted = ConfigHolder::reader(cx);
        if wanted.font_family != self.wanted_family {
            // 偏好换了家族名：重挑一次设备真能解析出来的那一个。
            self.wanted_family = wanted.font_family.clone();
            self.family = device_family(&wanted.font_family, cx).into();
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

    /// 把 `[cull.start, cull.end]` 这段排版 y 覆盖到的行摆进 `runs` / `images`。
    ///
    /// 几何全部来自排版层，这里只加三个偏移：左边的 `x`（这一栏的版心在窗口里的位置）、
    /// 向上的 `origin_y`（这一片的起点）、以及 `top` —— 页眉那条带子占掉的高度，正文从它
    /// 下面开始画。行内每个 run 用自己的宽度累加定 x；基线一律
    /// `line.y + line.baseline − origin_y + top`：gpui 自己的 `paint_line` 先把 run 的
    /// line-height 盒顶摆到 `baseline − (盒高 + ascent − descent)/2`，再由它把基线落回盒内
    /// 同一个偏移，两头抵消，结果就是这一条线。这里自己画字形，所以直接用它。
    ///
    /// 自由函数而不是方法：渲染时要一边可变借用 `self.source`（拿 `&mut Document` 排版）
    /// 一边读 `self.scroll_y` / `self.page`，写成 `&self` 方法会直接撞借用检查器。
    #[allow(clippy::too_many_arguments)]
    fn place_lines(
        document: &Document,
        family: &SharedString,
        lines: Range<usize>,
        cull: Range<f32>,
        origin_y: f32,
        x: f32,
        top: f32,
        foreground: Hsla,
        known: &[String],
        runs: &mut Vec<GlyphRun>,
        images: &mut Vec<VisibleImage>,
    ) {
        let layout = document.layout();
        for ix in lines {
            let Some(line) = layout.lines.get(ix) else {
                continue;
            };
            if line.y + line.height < cull.start || line.y > cull.end {
                continue;
            }
            // 行文本住在所属块的那个归一化串里（`LineRun::norm` 是它的字节区间）。
            let Some(block) = layout.block_of_line(ix) else {
                continue;
            };
            let mut x = x + line.x;
            let baseline = line.y + line.baseline - origin_y + top;
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
                                gpui_font(&FontKey::clone(&run.style.font), family, known),
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
    }
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_prefs(cx);
        let viewport_size = window.viewport_size();
        // 有效视口 = 窗口高 − 外壳占的高 − 页眉 − 页脚。可见行筛选、滚动上限、
        // `on_scroll` 的夹范围全都从这里取，所以底部那一截内容仍然滚得进来，
        // 也不会滚出多余空白。浮层（顶栏/底栏/面板）压在正文**之上**，不参与这个数：
        // 点一下屏幕字不该跳走。
        let viewport_height = (f32::from(viewport_size.height)
            - self.reserve.height
            - self.header_reserve
            - self.footer_reserve)
            .max(1.0);
        let gap = chrome::spread_gap(self.prefs.margin_x);
        // 退档判据与排版必须量同一个版心宽，所以用 `Document` 的那个式子算，不在这里另定一套。
        let usable = Document::content_width(window, &self.prefs, self.reserve.width);
        let mode = self.mode.effective(usable, gap);
        let columns = mode.columns();

        let foreground = cx.theme().foreground;
        // 这台设备注册的字族名：`gpui_font` 靠它把书里不存在的 `font-family` 落回偏好字体
        // （`resolve_font` 解析不到是 panic，不是回退）。
        let known = cx.text_system().all_font_names();
        let family = self.family.clone();
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
                    &family,
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
                self.laid_out.viewport = viewport_height;
            }
            Source::Book(book) => {
                let column_width = book.document_mut().ensure_viewport(
                    window,
                    cx,
                    Viewport {
                        reserve: self.reserve.width,
                        columns,
                        // 连续滚动 = 一片高 0：核心那边把 0 当「不分片」的哨兵值。
                        height: if mode.is_continuous() {
                            0.0
                        } else {
                            viewport_height
                        },
                        gap,
                    },
                );
                let pages = if mode.is_continuous() {
                    1
                } else {
                    book.document().page_count()
                };
                self.laid_out = LaidOut {
                    mode,
                    columns,
                    pages,
                    viewport: viewport_height,
                };

                // 上次读到第几个字 → 该摆在哪一像素 / 哪一页。只有排版做完了才映射得出，
                // 所以这一步在排版之后而不是构造时。
                if let Some(locator) = self.pending_restore.take() {
                    let line = book.document().line_at_offset(locator.offset);
                    if mode.is_continuous() {
                        self.scroll_y = book.document().y_at_offset(locator.offset);
                    } else {
                        self.page = book
                            .document()
                            .page_of_line(line)
                            .min(pages.saturating_sub(1));
                    }
                }
                // 内容高度 / 视口任一变化后重新夹一次落点（转屏、换字号都在这一刻生效）。
                if mode.is_continuous() {
                    self.content_height = book.document().height();
                    let max_scroll = (self.content_height - viewport_height).max(0.0);
                    self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
                } else {
                    self.page = self.page.min(pages.saturating_sub(1));
                }

                let mut runs = Vec::new();
                let mut images = Vec::new();
                if mode.is_continuous() {
                    // 连续滚动：整章一条，靠 `scroll_y` 挑出可见行。
                    ReaderView::place_lines(
                        book.document(),
                        &family,
                        0..book.document().line_count(),
                        self.scroll_y..(self.scroll_y + viewport_height),
                        self.scroll_y,
                        self.prefs.margin_x,
                        self.header_reserve,
                        foreground,
                        &known,
                        &mut runs,
                        &mut images,
                    );
                } else {
                    // 分页 / 双页：一页取几片、每片一栏，摆在各自的栏位上。
                    let slices = book.document().slices();
                    for index in 0..columns {
                        let Some(slice) = slices.get(self.page * columns + index) else {
                            continue;
                        };
                        if slice.empty {
                            continue;
                        }
                        let x =
                            self.prefs.margin_x + column_offset(column_width, gap, slice.column);
                        ReaderView::place_lines(
                            book.document(),
                            &family,
                            slice.lines.clone(),
                            slice.origin_y..(slice.origin_y + slice.height + 1.0),
                            slice.origin_y,
                            x,
                            self.header_reserve,
                            foreground,
                            &known,
                            &mut runs,
                            &mut images,
                        );
                    }
                }
                element.extend(runs);
                for image in images {
                    column = column.child(image_child(image));
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
            // 宿主的测试要按 id 把滚轮事件投到这里。非 `test-support` 构建里这一层是透明的
            // （`Observed` 原样转交内部元素），所以真机上不多花一个节点。
            .test_support()
            .size_full()
            .overflow_hidden()
            .on_scroll_wheel(listener)
            .child(column)
    }
}

/// 一片里的一张图：绝对摆位交给 gpui 的 `img`。
fn image_child(image: VisibleImage) -> AnyElement {
    div()
        .absolute()
        .left(px(image.x))
        .top(px(image.top))
        .w(px(image.width))
        .h(px(image.height))
        .child(img(image.image).size_full())
        .into_any_element()
}

/// 冷启动时依次试探的文档位置。
///
/// 首选**应用专属外部目录** `/sdcard/Android/data/<包名>/files/`：它的属主就是本应用，
/// 所以不需要任何存储权限，`adb push` 也写得进去。`/sdcard/Download/` 排在后面而不是
/// 前面 —— 那里的文件属主是媒体提供者（另一个 uid），scoped storage 下我们的进程读到
/// 的是 EACCES，放对了文件名也不会出现画面。`FOLIO_MOBILE_DOC` 留给调试时指任意路径。
/// 桌面上这些路径都不存在，`boot` 于是直接落到那一行兜底文本 —— 桌面的书从书架点开，
/// 走的是 `open_book`，不是这条路。
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

#[cfg(test)]
mod tests {
    //! M1–M4 的验收门，现在挂在**统一的那一份**视图上。
    //!
    //! 这批用例原来在 `desktop.rs`（虚拟列表那一套）里，`mobile.rs` 那边一条都没有 —— 于是
    //! 「手机上正文渲染成立」这件事靠的是真机截图，而桌面上那条回路红过也没人知道。
    //! 两端一份实现之后，这些断言测的就是真机上跑的那条代码路径。
    //!
    //! 用生产视图 `ReaderView`，不在测试里另搭近似视图；滚动走真实滚轮事件，
    //! 因为直接写 `scroll_y` 会绕过要测的那段行为。
    //! 导入点名而不 `use super::*`：`gpui_kit` 根把 `gpui` 整个转了出来，里面有一个
    //! 叫 `test` 的属性宏，glob 会让下面的 `#[test]` 递归展开成它。

    use std::rc::Rc;

    use super::{PageMode, ReaderView};
    use crate::ui::shared::config_model::ConfigHolder;
    use folio_core::store::Store;
    use folio_core::{Chapter, Config, DocMeta, FlowDoc, Format, Locator, Reader};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, ScrollDelta, TestAppContext, point, px, size,
    };

    /// 一段足够长的中文，保证它会折行 —— 不折行的话「缩进改变了布局」这种断言无从成立。
    const PARA: &str = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。";

    fn flow() -> FlowDoc {
        let source = (0..80)
            .map(|i| format!("{PARA}（第 {i} 段）\n\n"))
            .collect::<String>();
        FlowDoc {
            chapters: vec![Chapter {
                title: "第一章".into(),
                source,
                spine_href: String::new(),
            }],
            ..Default::default()
        }
    }

    fn meta(format: &str) -> DocMeta {
        DocMeta {
            title: "测试书".into(),
            authors: Vec::new(),
            format: Format::parse(format).expect("格式名要认得"),
            size: 0,
        }
    }

    fn prefs(size: f32) -> Reader {
        Reader {
            font_size: size,
            ..Reader::default()
        }
    }

    /// 开一扇窗、装好配置、把视图摆上去。`width` 决定这一帧能不能长出双页 ——
    /// 退档判据按宽度而不是按 target，所以窗口尺寸就是测试里唯一要控制的变量。
    fn open(
        cx: &mut TestAppContext,
        flow: &FlowDoc,
        format: &str,
        prefs: Reader,
        locator: Locator,
        width: f32,
    ) -> (Entity<ReaderView>, AnyWindowHandle) {
        open_with_store(cx, flow, format, prefs, locator, width, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn open_with_store(
        cx: &mut TestAppContext,
        flow: &FlowDoc,
        format: &str,
        prefs: Reader,
        locator: Locator,
        width: f32,
        store: Option<Rc<Store>>,
        book_id: Option<i64>,
    ) -> (Entity<ReaderView>, AnyWindowHandle) {
        let meta = meta(format);
        let mut view = None;
        let handle = cx.open_window(size(px(width), px(520.)), |window, cx| {
            // 偏好必须**装进配置**而不是只交给构造函数：视图每帧从 `ConfigHolder`
            // 同步一次偏好（读者在设置页改字号就是这条路），装默认配置就是把这里传的
            // `prefs` 当场冲掉 —— 而画面照样出得来，只有几何不对，最难查的那种红。
            let config = Config {
                reader: prefs.clone(),
                ..Default::default()
            };
            ConfigHolder::install(config, cx);
            let built =
                ReaderView::with_book(&meta, flow.clone(), prefs, store, book_id, locator, cx);
            let created = cx.new(|_| built);
            view = Some(created.clone());
            Root::new(created, window, cx)
        });
        let view = view.expect("ReaderView 应该被创建");
        settle(cx, handle.into());
        (view, handle.into())
    }

    /// 两轮「跑空事件 → 渲染一帧」。一轮不够：首帧只是把恢复请求挂上，
    /// 而 `run_until_parked` 只跑事件不画帧。框架自己的 `settle()` 也是这个形状。
    fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("渲染应成功");
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("第二帧应成功");
        cx.run_until_parked();
    }

    /// 真实滚轮事件投进正文。gpui 的约定是「向下滚为负」，所以往下滚给负的 dy。
    fn scroll_down(cx: &mut TestAppContext, handle: AnyWindowHandle, pixels: f32) {
        cx.update_window(handle, |_, window, cx| {
            window.scroll(
                "folio-reader",
                ScrollDelta::Pixels(point(px(0.0), px(-pixels))),
                cx,
            );
        })
        .expect("滚动应送达阅读视图");
        cx.run_until_parked();
    }

    fn progress_of(view: &Entity<ReaderView>, cx: &mut TestAppContext) -> f32 {
        view.read_with(cx, |v, _| v.progress())
    }

    fn offset_of(view: &Entity<ReaderView>, cx: &mut TestAppContext) -> usize {
        view.read_with(cx, |v, _| v.current_offset().unwrap_or_default())
    }

    /// 合成书：仓库里那本三章的 `minimal-zh.epub`。
    fn epub_flow() -> FlowDoc {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/minimal-zh.epub");
        let (_, content) = folio_core::open(&path).expect("合成 EPUB 应能打开");
        let folio_core::Content::Reflow(flow) = content;
        flow
    }

    #[gpui_kit::test]
    fn wheel_scrolling_moves_the_reading_position(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);

        assert_eq!(
            progress_of(&view, cx),
            0.0,
            "刚打开时应在章首，不该已有位移"
        );

        scroll_down(cx, handle, 900.0);
        let progress = progress_of(&view, cx);
        assert!(
            progress > 0.05,
            "真实滚轮事件应产生可测位移，实际比例 {progress:.3}"
        );
    }

    #[gpui_kit::test]
    fn locator_matches_the_scrolled_ratio(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let total = flow.chapters[0].char_len();
        assert!(total > 1000, "语料要够长才滚得动，实际 {total} 字");

        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
        assert_eq!(offset_of(&view, cx), 0, "未滚动时偏移应为 0");

        scroll_down(cx, handle, 900.0);
        let progress = progress_of(&view, cx);
        let offset = offset_of(&view, cx);

        assert!(offset > 0, "滚动后偏移应前进");
        assert!(offset < total, "偏移不应越出章尾");
        // 正文全是等高段落，所以「滚过的比例」与「读到的字数比例」该吻合。
        // 留一档余量：首行缩进与段距让行顶与字数不是严格线性。
        let ratio = offset as f32 / total as f32;
        assert!(
            (ratio - progress).abs() < 0.1,
            "偏移↔比例 换算不一致：offset {offset} / {total} = {ratio:.3}，progress = {progress:.3}"
        );
    }

    /// 「重启回到原处」里属于我们自己的那一半：位置必须能无损穿过 SQLite 回来，
    /// 连**当时那一档摆法**一起回来（进度存的是字偏移，模式只是它的读数方式）。
    #[gpui_kit::test]
    fn position_survives_a_restart_through_the_store(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);

        scroll_down(cx, handle, 900.0);
        let locator = Locator {
            chapter: 0,
            offset: offset_of(&view, cx),
        };
        assert!(locator.offset > 0, "滚动后应能算出字符偏移");

        let store = Rc::new(Store::in_memory().expect("内存库"));
        let id = store
            .add_book(std::path::Path::new("memory://fixture"), &meta("markdown"))
            .expect("入库");
        store.save_flow(id, &flow).expect("章节入库");
        store
            .set_position(id, PageMode::Continuous.storage_name(), &locator)
            .expect("写入进度");

        let (mode, restored) = store.position(id).expect("读回进度").expect("进度应存在");
        assert_eq!(mode, "continuous");
        assert_eq!(restored, locator, "进度穿过 SQLite 之后必须逐字符一致");

        // 用读回来的进度重开：恢复请求应在排版做完那一帧被消费掉，偏移落回同一个字。
        let (reopened, _) = open_with_store(
            cx,
            &flow,
            "markdown",
            prefs(16.0),
            restored,
            460.0,
            Some(store),
            Some(id),
        );
        assert!(
            !reopened.read_with(cx, |v, _| v.has_pending_restore()),
            "解析完成后恢复请求应被消费掉，而不是永远挂着"
        );
        assert_eq!(offset_of(&reopened, cx), restored.offset);
        assert!(progress_of(&reopened, cx) > 0.0, "重开之后正文真的滚出去了");
    }

    /// M2 的验收门：中文排版基础三件事必须由排版引擎产出。上一版在桌面上这些是
    /// `TextView` 画的，测到的是框架的行为；现在两端都是自绘，测的是引擎自己的行盒。
    #[gpui_kit::test]
    fn the_engine_indents_centers_and_spaces_lines(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = epub_flow();
        assert_eq!(flow.chapters.len(), 3, "spine 三章");

        let (view, _handle) = open(
            cx,
            &flow,
            "epub",
            Reader {
                font_size: 20.0,
                line_height: 1.8,
                ..Reader::default()
            },
            Locator::start(),
            460.0,
        );
        let lines = view.read_with(cx, |v, _| v.lines());
        let texts = view.read_with(cx, |v, _| v.line_texts());
        assert!(!lines.is_empty(), "引擎应产出行盒");

        // 第一章首个行盒是标题：UA 的 h1 = 2em 之外，书自己的 CSS 写了 1.8em 且居中。
        let title = &lines[0];
        assert_eq!(texts[0], "第一章 蛇结");
        assert!(
            (title.dominant_font_size() - 36.0).abs() < 0.5,
            "h1 = 1.8em × 20px，实际 {}",
            title.dominant_font_size()
        );
        assert!(
            title.x > 20.0,
            "居中标题应离开版心左缘，实际 x = {}",
            title.x
        );

        let body = lines
            .iter()
            .find(|l| l.dominant_font_size() == 20.0 && l.x > 0.0)
            .expect("应有一个首行被缩进的正文行");
        assert!(
            (body.x - 40.0).abs() < 1.0,
            "2em 缩进在 20px 字号下应是 40px，实际 {}",
            body.x
        );
        assert!(
            title.height > body.height * 1.5,
            "行盒高应随字号走，标题 {} vs 正文 {}",
            title.height,
            body.height
        );
        assert!(
            (body.height - 36.0).abs() < 0.5,
            "正文行高 = 20 × 1.8，实际 {}",
            body.height
        );

        // 长段落必须真的折了行：没有任何一行容得下整段原文，才算折过。
        let longest_paragraph = flow.chapters[0]
            .source
            .split('<')
            .map(|s| s.chars().filter(|c| !c.is_whitespace()).count())
            .max()
            .unwrap_or(0);
        let longest_line = texts.iter().map(|t| t.chars().count()).max().unwrap_or(0);
        assert!(
            longest_line < longest_paragraph && longest_line > 4,
            "段落应被折行：最长段 {longest_paragraph} 字，最长行只有 {longest_line} 字"
        );
        for pair in lines.windows(2) {
            assert!(
                pair[1].y >= pair[0].y + pair[0].height - 0.5,
                "行盒不能重叠：{} + {} > {}",
                pair[0].y,
                pair[0].height,
                pair[1].y
            );
        }
    }

    /// M3 的验收：一个段落里的局部字重、斜体与颜色必须各自成立。
    /// 这一条测的是**真实字体栈**上的多 run 度量与摆放，mock 测不到。
    #[gpui_kit::test]
    fn inline_styles_survive_into_runs(cx: &mut TestAppContext) {
        use folio_core::css::computed::Color;

        cx.update(gpui_kit::init);
        let flow = epub_flow();
        let (view, _handle) = open(
            cx,
            &flow,
            "epub",
            Reader::default(),
            Locator::start(),
            460.0,
        );
        let lines = view.read_with(cx, |v, _| v.lines());

        let rich = lines
            .iter()
            .find(|l| l.text_runs().count() >= 4)
            .expect("带 em/strong/span 的那一行应被切成多个 run");
        assert!(
            rich.text_runs().any(|r| r.style.font.weight >= 700),
            "strong 应带粗体"
        );
        assert!(rich.text_runs().any(|r| r.style.font.italic), "em 应带斜体");
        assert!(
            rich.text_runs()
                .any(|r| r.style.color == Some(Color(0xC0, 0x39, 0x2B))),
            "span.red 的局部颜色要落到 run 上"
        );
        assert!(
            rich.text_runs().all(|r| r.width > 0.0),
            "每个 run 都要有实际绘制宽度"
        );
        let sum: f32 = rich.text_runs().map(|r| r.width).sum();
        assert!(
            (sum - rich.width).abs() < 0.5,
            "行宽应是各 run 宽度之和：{sum} vs {}",
            rich.width
        );
    }

    /// M3-E3 验收：EPUB 章里的内嵌图要经排版引擎变成图片片段，`src` 按本章目录解析到
    /// 容器路径，且可见文本里不留哨兵字符。
    #[gpui_kit::test]
    fn images_flow_through_the_engine_as_fragments(cx: &mut TestAppContext) {
        use folio_core::format::ImageRes;

        cx.update(gpui_kit::init);
        // 一张 1×1 的合法 PNG：解码由 gpui 负责，这里只要能过 header。
        const PNG_1X1: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let flow = FlowDoc {
            chapters: vec![Chapter {
                title: "图".into(),
                source: "<p>前<img src=\"../Images/pic.png\" alt=\"图\">后</p>".into(),
                spine_href: "OEBPS/Text/chap.xhtml".into(),
            }],
            images: vec![ImageRes {
                href: "OEBPS/Images/pic.png".into(),
                media_type: "image/png".into(),
                width: 40,
                height: 20,
                bytes: PNG_1X1.to_vec(),
            }],
            ..Default::default()
        };
        let (view, _handle) = open(
            cx,
            &flow,
            "epub",
            Reader::default(),
            Locator::start(),
            460.0,
        );
        let lines = view.read_with(cx, |v, _| v.lines());
        let texts = view.read_with(cx, |v, _| v.line_texts());

        assert_eq!(texts[0], "前后", "行内图不产出字符，哨兵要被去掉");
        let img = lines[0]
            .images()
            .next()
            .expect("../Images/pic.png 应解析成 OEBPS/Images/pic.png 并命中");
        assert_eq!(img.href, "OEBPS/Images/pic.png");
        assert_eq!((img.width, img.height), (40.0, 20.0));
    }

    /// M4 的门：坐标 → 行 → 落点 → 再反解回坐标，必须回到同一个字。
    /// 这一条与「进度落在第几个字上」整套设计同源，所以它红了就是锚点坐标系红了。
    #[gpui_kit::test]
    fn the_engine_progress_is_a_character_offset_that_comes_back(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = epub_flow();
        // 字号故意给到 60：这本合成书的第一章很短，默认字号下整章只有几行、比窗口还短，
        // 那一档下没有可滚的范围，测试就成了测空集。
        let prefs = prefs(60.0);
        let (view, handle) = open(cx, &flow, "epub", prefs.clone(), Locator::start(), 460.0);
        let lines = view.read_with(cx, |v, _| v.lines().len());
        assert!(lines > 4, "大字号下第一章该排出多行，实际 {lines}");
        // 跳转目标取「本章中点」。写死一个坐标的话，CI 上字体不同 → 行高不同 →
        // 可能落在第一行里，那条测试就变成测空集了。
        let total = view.read_with(cx, |v, _| v.text_length());
        assert!(total > 40, "第一章该有几十个字可跳，实际 {total}");
        let target = total / 2;
        // 章首那一行的坐标不是 0：pretty-print 过的 XHTML 里 `<body>` 与 `<p>` 之间的
        // 换行本身是文本节点，也进坐标系。
        let start = offset_of(&view, cx);
        assert!(start < 8, "刚打开该在章首附近，实际坐标 {start}");

        view.update(cx, |v, cx| v.jump_to_offset(target, cx));
        settle(cx, handle);

        let saved = offset_of(&view, cx);
        assert!(
            saved > start,
            "跳到中点坐标 {target} 之后章内偏移该往前走，实际 {start} → {saved}"
        );
        assert!(
            saved <= target,
            "落点该是覆盖 {target} 那一行的行首，不该越过它：{saved}"
        );
        assert!(
            target - saved < target / 4,
            "落点离目标 {target} 太远：{saved}"
        );
        let progress = progress_of(&view, cx);
        assert!(
            progress > 0.0 && progress < 1.0,
            "滚动比例该在 (0,1) 之间，实际 {progress}"
        );

        // 用同一个坐标重开：落回的行反解出来的偏移必须还是它。
        let (reopened, _) = open(
            cx,
            &flow,
            "epub",
            prefs,
            Locator {
                chapter: 0,
                offset: saved,
            },
            460.0,
        );
        assert!(
            !reopened.read_with(cx, |v, _| v.has_pending_restore()),
            "恢复请求该在排版做完的那一帧被消费掉"
        );
        assert_eq!(
            offset_of(&reopened, cx),
            saved,
            "坐标 → 行顶 → 再反解回坐标，必须回到同一个字"
        );
        assert!(
            progress_of(&reopened, cx) > 0.0,
            "重开之后正文真的滚出去了，不是只记了个数"
        );
    }

    /// 双页成立的那两条判据：一页确实是**两栏**，而且两栏首尾相接 —— 右栏的第一行
    /// 紧跟左栏的最后一行，读者按「先左后右」读不会跳行。
    #[gpui_kit::test]
    fn a_spread_is_two_contiguous_columns_of_one_sheet(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 1200.0);
        assert_eq!(
            view.read_with(cx, |v, _| v.mode()),
            PageMode::Continuous,
            "默认该是连续滚动，不是替读者决定"
        );

        view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
        settle(cx, handle);
        assert_eq!(
            view.read_with(cx, |v, _| v.mode()),
            PageMode::Spread,
            "1200 宽的窗口该放得下两栏"
        );
        let pages = view.read_with(cx, |v, _| v.page_count());
        assert!(pages > 2, "长一章双页也该有多页，实际 {pages}");

        let left = view
            .read_with(cx, |v, _| v.sheet_range(0, 0))
            .expect("第 0 页左栏应有内容");
        let right = view
            .read_with(cx, |v, _| v.sheet_range(0, 1))
            .expect("第 0 页右栏应有内容");
        assert!(!left.is_empty() && !right.is_empty(), "两栏都要有行");
        assert_eq!(
            left.end, right.start,
            "右栏该从左栏断的那一行接着排，不能重叠也不能留缝"
        );

        // 一页一号：翻一页 = 翻一张纸（两栏一起走），偏移该往前走并且不超过两栏的行数。
        let before = offset_of(&view, cx);
        view.update(cx, |v, cx| v.scroll_page(1, cx));
        settle(cx, handle);
        assert_eq!(view.read_with(cx, |v, _| v.page()), 1);
        let after = offset_of(&view, cx);
        assert!(after > before, "翻一张纸该往前走，实际 {before} → {after}");
    }

    /// 窄窗口点「双页」不该得到两条窄柱子：按宽度退回分页。
    /// 这条判据按宽度而不是按 target，所以横屏平板能真双页、竖屏手机自动单页。
    #[gpui_kit::test]
    fn a_narrow_window_falls_back_from_spread_to_one_column(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
        view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
        settle(cx, handle);

        assert_eq!(
            view.read_with(cx, |v, _| v.mode()),
            PageMode::Paged,
            "460 宽放不下两栏，该退回分页"
        );
        // 读者**选**的仍然是双页：按钮的选中态读它，不然一退档那格就自己弹回去了。
        assert_eq!(view.read_with(cx, |v, _| v.wanted_mode()), PageMode::Spread);
        // 只有一栏，所以第 1 栏不存在。
        assert!(
            view.read_with(cx, |v, _| v.sheet_range(0, 1)).is_none(),
            "退档之后不该还剩一条空栏"
        );
    }

    /// 换摆法不换位置：连续滚到某个字，切双页（宽窗）再切回来，还在同一个字上。
    /// 进度是「第几个字」，模式只是它的读数方式 —— 这条要是红了，读者的位置就会在
    /// 点一下「双页」时飘掉。
    #[gpui_kit::test]
    fn switching_modes_keeps_the_same_character(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let flow = flow();
        let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 1200.0);
        scroll_down(cx, handle, 1200.0);
        let wanted = offset_of(&view, cx);
        assert!(wanted > 0, "先滚到一个字上，实际 {wanted}");

        view.update(cx, |v, cx| v.set_mode(PageMode::Spread, cx));
        settle(cx, handle);
        assert_eq!(view.read_with(cx, |v, _| v.mode()), PageMode::Spread);
        let in_spread = offset_of(&view, cx);
        // 落点是「覆盖那个字的那张纸的开头」，所以它只能等于或早于原位置，
        // 且不能早过一页之内的距离。
        assert!(
            in_spread <= wanted,
            "落位不该越过原来那一行：{wanted} → {in_spread}"
        );

        view.update(cx, |v, cx| v.set_mode(PageMode::Continuous, cx));
        settle(cx, handle);
        assert_eq!(
            offset_of(&view, cx),
            in_spread,
            "切回连续滚动该落在双页那一屏的开头那个字上"
        );
    }
}
