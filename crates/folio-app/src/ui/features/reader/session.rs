//! 阅读会话：一本书的一屏——切片、摆行、滚动与翻页、位置落盘的状态机与命令入口。
//!
//! 这是 `reader` 这一纵切的中枢：它持有 [`Book`]（当前打开的书与本章排版状态）与
//! 一屏自己的状态（模式、滚动/页号、外壳报的占位），对外给出一排命令
//! （`set_mode` / `scroll_page` / `goto_chapter` / `persist_position` …），渲染在
//! [`crate::ui::features::reader::view`]（`impl Render for ReaderSession`）。
//!
//! 命令按职责分在 [`crate::ui::features::reader::modules`] 下：`navigation`（翻页/滚动/
//! 模式）、`position`（进度 ↔ 坐标、恢复、落库）、`toc`（章节表）。它们都写
//! `impl ReaderSession`，靠 `pub(super)` 字段打进同一个实体 —— 一个会话一个实体，
//! 不为拆文件而拆实体。

use std::path::{Path, PathBuf};
use std::rc::Rc;

use folio_core::document::Document as OpenedDocument;
use folio_core::store::Store;
use folio_core::{DocMeta, Format, Locator, Reader};
use gpui_kit::{App, Context, SharedString};

use super::book::Book;
use super::chrome::{PageMode, Reserve};
use super::measure::FontCache;
use crate::ui::host::device_family;
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::library::PACKAGE;

/// 视图现在拿的是什么内容。
pub enum Source {
    /// 兜底：一行硬编码文本。找不到任何文档时用它，为的是冷启动失败也能看见画面，
    /// 而不是一片黑 —— 「装上了但读不到书」和「根本没起来」在 logcat 里很难分开。
    Sample(SharedString),
    /// 一本打开的书。
    ///
    /// 装箱是因为两臂差着数量级：`Book` 带着章节表与排版结果，而兜底那一行只要 24。
    /// 不装箱的话，整个 `Source` 连同它外面的会话都按大的那臂算，每次赋值都在搬一整本书。
    Book(Box<Book>),
}

/// 上一帧真正摆上屏的那几个数。
///
/// 存下来是因为翻页、进度、落库都发生在**渲染之外**（滚轮事件、滑条、点目录），那时手上
/// 没有 `Window`，重新推一遍「现在几栏、共几页」就要再量一次版心，而版心是渲染量出来的。
#[derive(Clone, Copy, Debug)]
pub(super) struct LaidOut {
    pub mode: PageMode,
    pub columns: usize,
    pub pages: usize,
    /// 一个视口高，分页时页号的换算要用。
    pub viewport: f32,
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

pub struct ReaderSession {
    pub(super) source: Source,
    /// 上一次同步下来的读者偏好。与 [`ConfigHolder`] 里的不一致就推给排版状态。
    pub(super) prefs: Reader,
    /// 排版实际用的正文家族：偏好里那个在设备上解析不出来时换成设备有的那个。
    pub(super) family: SharedString,
    /// 上次读到的偏好家族名。挑设备字体要枚举全部字族，只在名字真变了时做。
    pub(super) wanted_family: String,
    /// 读者**选**的那一档。装不下双页时实际生效的是分页，见 [`ReaderSession::mode`]。
    pub(super) mode: PageMode,
    pub(super) laid_out: LaidOut,
    /// 已滚过的像素（内容坐标，向下为正）。只在连续滚动里有意义。
    pub(super) scroll_y: f32,
    /// 分页 / 双页的当前页号（一张纸一号，两栏算同一号）。
    pub(super) page: usize,
    /// 滚轮攒下的增量：分页模式下翻一页要攒够半屏，不然一次惯性滚动能翻过去三页。
    pub(super) wheel_accum: f32,
    /// 上一帧的内容总高。与 `laid_out.viewport` 一起决定连续滚动的可滚上限。
    pub(super) content_height: f32,
    /// 外壳在不属于正文那一侧占掉的量（桌面 = 侧栏宽，移动 = 底栏高）。
    pub(super) reserve: Reserve,
    /// 阅读页顶部那行小字（章名）占掉的高度。正文从它下面开始画，
    /// 滚动上限也按它扣 —— 可见行筛选、`max_scroll`、`on_scroll` 的夹范围必须同一个数。
    pub(super) header_reserve: f32,
    /// 阅读页底部那行小字（进度）占掉的高度。与 `header_reserve` 同理。
    pub(super) footer_reserve: f32,
    /// 还没落地的位置。恢复必须等排版做过一次才能做（坐标 → 行顶的映射要有行盒），
    /// 而构造时手上没有窗口，所以先记下来，第一帧渲染时取走。
    pub(super) pending_restore: Option<Locator>,
    /// 上一次落库的章内偏移。滚动的落库按它去重：偏移只在**跨过一行**时才变，
    /// 所以「变了才写」等于「每行至多写一次」，比每帧一次或每次触摸一次都便宜，
    /// 也不需要定时器。
    pub(super) saved_offset: usize,
    /// 书库。移动端的导入还没有界面，文件是 `adb push` 进应用目录的，
    /// 但位置与书目照常入库，所以杀掉进程回来还在同一章。
    pub(super) store: Option<Rc<Store>>,
    pub(super) book_id: Option<i64>,
    /// `FontKey → Font` 的解析缓存。渲染每帧对每条可见 run 都要一次，缓存后只做哈希查表
    /// （见 [`FontCache`]：body / mono / faces / 设备表变了必须 `clear` 或 `reset`）。
    pub(super) font_cache: FontCache,
}

/// 兜底用的那行字。中英混排，顺带把 fallback 路径也验一遍。
const SAMPLE_LINE: &str = "Folio — 中文正文与 Latin 混排的一行";

impl ReaderSession {
    /// 冷启动：读偏好、挑设备字体，找得到文档就打开书，否则退回那一行兜底文本。
    pub fn boot(cx: &App) -> Self {
        let prefs = ConfigHolder::reader(cx);
        let found = candidate_paths().into_iter().find(|path| path.is_file());
        let Some(path) = found else {
            log::info!("ReaderSession: 没有可读文档，进入兜底的单行文本");
            return Self::sample(&prefs, cx);
        };
        // 书库没装不是 fatal：照样能读，只是进度与偏好落不了盘，
        // 而「读了但没记住」比「直接退出」更可接受。
        let store = crate::ui::shared::app_store::handle(cx);
        match Self::open_document(&path, &prefs, store, cx) {
            Ok(view) => {
                log::info!("ReaderSession: 打开 {path:?}");
                view
            }
            Err(err) => {
                log::error!("ReaderSession: 打开 {path:?} 失败：{err:#}");
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
            font_cache: FontCache::new(cx.text_system().all_font_names()),
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
        let doc = super::document::open(path)?;
        let book_id = store.as_ref().and_then(|store| {
            let id = store.add_book(path, doc.meta()).ok()?;
            let _ = store.save_document(id, doc.as_ref());
            let _ = store.touch_opened(id);
            Some(id)
        });
        // 恢复失败就当没读过：回到章首，而不是停在某个对不上的位置上。
        let locator = book_id
            .and_then(|id| store.as_ref()?.position(id).ok().flatten())
            .map(|(_, locator)| locator)
            .unwrap_or_else(Locator::start);
        Ok(Self::with_document(
            doc,
            prefs.clone(),
            store,
            book_id,
            locator,
            cx,
        ))
    }

    /// 从一份已打开的文档建会话，不碰书库。
    ///
    /// 公开是因为**宿主的测试要能不开文件就打开一本书**：坐标、分页、退档那几条门读的是
    /// 这条链子，而链子上最贵的一环（解 ZIP、读盘）对它们没有任何影响。
    pub fn with_document(
        doc: std::sync::Arc<dyn OpenedDocument>,
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
        let book = Book::new(doc, effective.clone(), locator, cx);
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
            // 设备字体表在这里读一次；换书（可能注册了新内嵌字体）时由 `open_book` reset。
            font_cache: FontCache::new(cx.text_system().all_font_names()),
        }
    }

    /// 从书库打开一本书。
    ///
    /// 按主键读库而不是按 `books.path` 去开那个文件：导入时那份副本躺在系统给文件
    /// 选择器的临时目录里，随时会被清掉，而正文、图片、CSS 早在 `save_document` 时就
    /// 进了 SQLite。书架点开一张卡片要的是「这本书还在」这个保证，不是「那个文件还在」。
    ///
    /// 读不到的一切情况都只写日志、不动当前那一屏：读者正在读的书不该因为点错一本
    /// 坏记录而变成空白页。
    pub fn open_book(&mut self, book_id: i64, cx: &mut Context<Self>) {
        let opened = self.store.as_ref().and_then(|store| {
            let row = store.book(book_id).ok().flatten()?;
            let content = store.load_content(book_id).ok().flatten()?;
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
                content,
                row.path,
                mode,
                locator,
            ))
        });
        let Some((meta, content, path, mode, locator)) = opened else {
            log::error!("ReaderSession: 书库里第 {book_id} 本读不出来（章节没入库或格式不认识）");
            return;
        };
        // 章文本早在落库时就进了库，但图片与内嵌字体刻意不入库 —— 回到原文件现取。
        let doc: std::sync::Arc<dyn OpenedDocument> = std::sync::Arc::from(
            folio_core::document::with_container(meta, content, Path::new(&path)),
        );
        // `self.prefs` 里的家族已经是这台设备解析得出来的那个，不再挑一遍。
        let book = Book::new(doc, self.prefs.clone(), locator, cx);
        // 换了一本书：`Book::new` 可能刚把书里的内嵌字体注册进设备，字体表要重读一份，
        // 缓存按新表重建（否则旧 FontKey 会命中到上一本书解析出的 Font）。
        self.font_cache.reset(cx.text_system().all_font_names());
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

    /// 当前那本书的只读借用；兜底那一行没有书，返回 `None`。
    pub(super) fn book(&self) -> Option<&Book> {
        match &self.source {
            Source::Book(book) => Some(book),
            Source::Sample(_) => None,
        }
    }

    /// 当前那本书的可变借用；兜底那一行没有书，返回 `None`。
    pub(super) fn book_mut(&mut self) -> Option<&mut Book> {
        match &mut self.source {
            Source::Book(book) => Some(book),
            Source::Sample(_) => None,
        }
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
    /// 与 [`ReaderSession::set_reserve`] 分成两个入口是因为**归属不同**：那条由外壳每帧报告
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
    pub(super) fn max_scroll(&self) -> f32 {
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

    // ---- 给状态栏与宿主测试读的排版结果 ----

    /// 本章可见文本的字数（锚点坐标系的长度）。
    pub fn text_length(&self) -> usize {
        self.book()
            .map(|book| book.document().text_map().chars())
            .unwrap_or(0)
    }

    /// 排版出来的行盒。
    pub fn lines(&self) -> Vec<folio_core::layout::LineBox> {
        self.book()
            .map(|book| book.document().lines().to_vec())
            .unwrap_or_default()
    }

    /// 每行的可见文本，与 [`ReaderSession::lines`] 一一对应。
    pub fn line_texts(&self) -> Vec<String> {
        self.book()
            .map(|book| book.document().line_texts())
            .unwrap_or_default()
    }

    /// 恢复请求是否还挂着（false 表示已落地或被放弃）。
    pub fn has_pending_restore(&self) -> bool {
        self.pending_restore.is_some()
    }

    /// 第 `page` 页第 `column` 栏覆盖的行区间（排版行下标）。
    ///
    /// 渲染按它挑行，测试按它钉「双页的左右两栏首尾相接」—— 这一条以前只能上真机猜，
    /// 现在两端同一份代码，宿主就测得住。
    pub fn sheet_range(&self, page: usize, column: usize) -> Option<std::ops::Range<usize>> {
        // 必须先挡这一条：单栏时「片下标 = page」，`column == 1` 会指到**下一页**的第一栏，
        // 看着像有内容、其实是隔壁那张纸。
        if column >= self.laid_out.columns {
            return None;
        }
        let book = self.book()?;
        book.document()
            .slice_of(page, column, self.laid_out.columns)
            .map(|slice| slice.lines.clone())
    }

    /// 读者在设置里改了字号、行距、缩进或字体，在这一帧同步进来。
    ///
    /// 每帧读一次 Global 再比较，比给这个会话挂一条订阅更合适：偏好本来就是每帧都要用
    /// 的输入，而订阅者一多，就容易出现「在 `render` 里改状态」那一类被禁止的写法。
    pub(super) fn sync_prefs(&mut self, cx: &App) {
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
            // body / mono 不入 `FontKey`，换了必须让缓存失效，否则同一个 key 会命中旧字体。
            self.font_cache.clear();
            if let Some(book) = self.book_mut() {
                book.document_mut().set_prefs(effective);
            }
        }
    }
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
