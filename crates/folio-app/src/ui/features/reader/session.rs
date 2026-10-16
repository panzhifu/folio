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

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use folio_core::content::annotation::ResolvedAnnotation;
use folio_core::content::document::Document as OpenedDocument;
use folio_core::store::{Annotation, Store};
use folio_core::{DocMeta, Format, Locator, Reader, StaleReason};
use gpui_kit::{App, Bounds, Context, Entity, Pixels, Point, SharedString, Task};

use super::book::Book;
use super::chrome::{PageMode, Reserve};
use super::fixed::FixedBook;
use super::measure::FontCache;
use crate::ui::host::device_family;
use crate::ui::shared::config_model::ConfigHolder;

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
    /// 一本打开的定版式书（CBZ / 逐页 SVG 漫画）。页是既定的，没有排版链 —— 见
    /// [`FixedBook`] 与 `content::format::Content` 的两族划分。
    Fixed(Box<FixedBook>),
}

/// 一个成立的选区：挂在**某一章**的章内字符坐标上。
///
/// 跨章滚动之后屏上可能同时摆着两章的行，选区属于它成立时的那一章 —— 复制、
/// 高亮、把手都按它自己的章取数据，不跟锚章走。它的章被裁出拼接窗口（滚远了）
/// 时选区一并丢（渲染帧里 `span_doc` 查不到就清）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Selection {
    pub(super) chapter: usize,
    pub(super) range: Range<usize>,
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
    /// 这一档**不是**读者选的：库里那本书没存过模式（第一次读它，或从没换过档）。
    ///
    /// 挂着这面旗时渲染帧每帧按当前窗口宽重算默认档（[`chrome::default_mode`]），
    /// 落库也不写这一档 —— 否则「在平板上第一次打开」会把双页钉死，回到手机上
    /// `effective` 只能把它退成分页，而不是它本来该是的样子。
    pub(super) mode_undecided: bool,
    pub(super) laid_out: LaidOut,
    /// 正在走的那一次翻页动画（见 [`Turn`]）。`None` = 画面定住了。
    pub(super) turn: Option<Turn>,
    /// 每次起动画加一的时钟键，供 [`Turn::seq`] 用。
    pub(super) turn_seq: u64,
    /// 已滚过的像素（内容坐标，向下为正）。只在连续滚动里有意义。
    pub(super) scroll_y: f32,
    /// 这一帧**画在**哪一个全局 y。动画期间它落后于 [`ReaderSession::scroll_y`]，
    /// 而绘制与命中两处都必须读它而不是 `scroll_y`：用后者命中的话，补间走完之前
    /// 按在某个字上的手指会算到隔壁那一行去（起点也从这里取，见 [`Turn::scroll`]）。
    pub(super) scroll_painted: f32,
    /// 分页 / 双页这一帧的当前页偏了多少（平移翻页期间非零，定住时是 `(0, 0)`）。
    ///
    /// 由渲染帧按当前页写入，命中换算（`modules/selection.rs` 的 `to_layout`）减掉同一个
    /// 数 —— 绘制把位移烘进了 run 的坐标，命中不减就会在快速连点两下时按到新页的空地上。
    pub(super) page_shift: (f32, f32),
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
    /// 那一趟恢复要落到的**章内锚点**（目录条目的 `id`）。
    ///
    /// 为什么不直接换算成偏移存进 [`Self::pending_restore`]：偏移表是排版产物
    /// （`Document::id_offsets`），挂请求那一刻目标章通常还没排版。所以带着 `id`
    /// 等一等，落地那一帧再换算。
    ///
    /// 与 `pending_restore` 同生共死：挂新请求必须把它清掉，否则上一次没落地的
    /// 锚点会跟着下一次恢复跳错地方。清它的活儿收在 [`Self::pend_restore`] 里。
    pub(super) pending_anchor: Option<String>,
    /// 到过的章（`chapters.idx`）。目录那一屏按它把来过的画成浅色。
    ///
    /// 开书时从 `chapter_read` 读一次，之后只增。这张集合同时是「要不要再写一笔」的
    /// 去重位（`HashSet::insert` 返回是否新入），所以每章最多打一次库，
    /// 而不是每跨过一行一次。
    pub(super) read: HashSet<usize>,
    /// 上一次落库的位置。滚动的落库按它去重：偏移只在**跨过一行**时才变，    /// 所以「变了才写」等于「每行至多写一次」，比每帧一次或每次触摸一次都便宜，
    /// 也不需要定时器。章号与偏移一起比 —— 跨进下一章的第一个字偏移可能还是 0，
    /// 只比偏移会漏掉那一次落库。
    pub(super) saved_chapter: usize,
    pub(super) saved_offset: usize,
    /// 书库。移动端的导入还没有界面，文件是 `adb push` 进应用目录的，
    /// 但位置与书目照常入库，所以杀掉进程回来还在同一章。
    pub(super) store: Option<Arc<Store>>,
    pub(super) book_id: Option<i64>,
    /// `FontKey → Font` 的解析缓存。渲染每帧对每条可见 run 都要一次，缓存后只做哈希查表
    /// （见 [`FontCache`]：body / mono / faces / 设备表变了必须 `clear` 或 `reset`）。
    pub(super) font_cache: FontCache,
    /// 书里的图与 GPU 图集之间那层**会回收**的缓存（见 [`super::image_atlas`]）。
    ///
    /// 挂在会话而不是挂在书上：它管的是「这一屏画过的像素」，而一屏同时只有一本书；
    /// 换书时显式 `clear`（`open_book` 里那一趟），会话被扔掉时 `observe_release`
    /// 把剩下的格子全部放掉 —— 少了这两条里的任何一条，读过的图就永久压在显存里。
    pub(super) atlas: gpui_kit::Entity<super::image_atlas::ImageAtlas>,

    // ---- 选中（M6）：状态在这里，交互逻辑在 modules/selection.rs，绘制在 paint.rs / view.rs ----
    /// 当前选中（某章的章内可见文本坐标，含头不含尾）。滚动不动它，重排不丢它，
    /// 它所属的章被裁出窗口时丢。
    pub(super) selection: Option<Selection>,
    /// 就地菜单条开合。拖动期间收起（让位），成选即开。
    pub(super) selection_menu_open: bool,
    /// 指针拖选的临时态：按下锚点 + 起始位置 + 是否已拖过阈值。
    pub(super) mouse_drag: Option<MouseDrag>,
    /// 长按扩展的锚点：长按吸成词后手指继续拖，活动端绕它走。
    /// 连同它所属的章一起记（跨章滚动后坐标按章隔离）。
    pub(super) long_press_anchor: Option<(usize, usize)>,
    /// 把手拖动：哪一端 + 手指→插入符的偏移。
    pub(super) edge_drag: Option<EdgeDrag>,
    /// 拖选/把手拖动松手后吃掉紧跟着的那一下 click（否则 `page::on_body_tap`
    /// 会把刚选的清掉、还会翻页）。
    pub(super) suppress_tap: bool,
    /// 浮层（顶栏/底栏）开着。开着时指针让位——那一击的语义是收浮层。
    pub(super) overlay_open: bool,
    /// 正文元素这一帧画在窗口里的位置。事件坐标 → 排版坐标的原点，
    /// 由 `GlyphsElement::paint` 每帧写入（gpui 事件只有窗口坐标）。
    pub(super) content_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// 本帧两端把手的命中区（窗口坐标，44px）。paint 每帧写入，事件时读。
    pub(super) handle_hits: HandleHits,
    /// 本帧菜单条的位置（窗口坐标）。视图每帧写入；压在它上面的一击属于菜单，
    /// 不算「点别处清除」。
    pub(super) menu_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// 图片放大查看那一层的状态，由**阅读页**持有（它画在最上层，要盖住顶栏底栏与
    /// 那两块弹层）。会话只拿句柄：正文插图的那一击与定版式那一次的长按都发生在
    /// 会话的渲染里，得有个地方把「要看这张图」写出去。没挂上（单独的会话测试）时
    /// 图片不可点，行为与这一层不存在时相同。
    pub(super) image_viewer: Option<Entity<super::image_viewer::ImageViewer>>,
    /// 整本书的那一列图：图片查看器左右滑走的、计数念的都在这列里。
    ///
    /// 开书时算一次（换书 = `open_book` 重算），之后只读。它**不是**排版产物：
    /// 可重排那一支来自库里的每章清单（`chapters.images_json`），定版式那一支来自页表，
    /// 两支都不要求「那一章已经排好」—— 这正是它存在的理由：读者要在一章还没走到之前
    /// 就知道一共有几张、这一张排第几。
    ///
    /// 每一条目建列时就解过一次 `src`（解不出的不收），所以列里的每张图都拿得出版本号
    /// 对得上的 `DecodeRequest`。
    pub(super) image_sequence: Arc<[super::image_viewer::ImageSlot]>,
    /// 脚注弹层的状态：点中一条脚注引用时写进来（注文 + 标记在屏上的位置）。
    /// 弹层长在**阅读页**上（见 `page::ReadingPage::footnote_popover`），这一格是它的
    /// 数据源；会话只负责「这一击落在脚注上、注文是这句」。
    pub(super) footnote: Option<Footnote>,

    // ---- 批注（M7）：状态在这里，读写在 modules/annotations.rs，绘制在 view.rs ----
    /// 这本书在库里的全部批注，**原样**（还没解锚）。开书时读一次，写入时追加一条。
    pub(super) annotations: Vec<Annotation>,
    /// 按章解好锚的批注：章下标 →（排版代数，解出来的区间）。
    ///
    /// 缓存键里那个代数是必需的：解锚读的是「这一章的可见文本坐标」，换章、改字号、
    /// 换窗口宽都会让它进位，而进位之后旧区间指的就是别的字了。
    pub(super) annotation_spans: RefCell<AnnotationSpans>,

    // ---- 阅读时长（统计页的数据源）：状态在这里，判断在 modules/reading_time.rs ----
    /// 这一段还没结账的阅读从哪一刻起算（Unix 秒）。0 = 此刻没在读任何书。
    pub(super) reading_segment_start: i64,
    /// 最后一次「人还在」的证据落在哪一刻。宽限线按它算，见 `modules/reading_time`。
    pub(super) reading_last_activity: i64,
    /// 那条每来一次就问「到点结了没有」的时钟。换书时它已经挂着就不重挂；
    /// 关书时把它 drop 掉（`Task` 一 drop，计时就取消）。
    pub(super) reading_clock: Option<Task<()>>,
}

/// 按章缓存的解锚结果：章下标 →（这批结果出自哪一代排版，区间表）。
///
/// 起别名是因为它出现在三处签名里（会话字段、解算入口、渲染帧），摊开写每次都得
/// 把四层嵌套重抄一遍，clippy 也嫌复杂。
pub(super) type AnnotationSpans = HashMap<usize, (u64, Rc<Vec<ResolvedAnnotation>>)>;

/// 选区的一端：起点（首个选中字符之前）或终点（末个选中字符之后）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SelectionEdge {
    Start,
    End,
}

impl SelectionEdge {
    /// 被拖动时按住不动的另一端。
    pub(super) fn opposite(self) -> Self {
        match self {
            SelectionEdge::Start => SelectionEdge::End,
            SelectionEdge::End => SelectionEdge::Start,
        }
    }
}

/// 指针拖选的临时态。
pub(super) struct MouseDrag {
    /// 按下落点所属的章。整手拖选都锁在这一章里：跨章拖选是罕见场景，
    /// v1 直接不允许 —— 拖过章界的手势就停在章界上。
    pub chapter: usize,
    /// 按下落点的章内字符偏移。
    pub anchor: usize,
    /// 按下的窗口位置，阈值判据的基准。
    pub origin: Point<Pixels>,
    /// 是否已经拖过阈值。只有拖过的松手才算「拖选成了一次」，才拦 click。
    pub dragged: bool,
}

/// 把手拖动的临时态。
pub(super) struct EdgeDrag {
    pub edge: SelectionEdge,
    /// 起拖时记下的「手指 → 插入符」偏移：圆钮长在行的上/下侧，
    /// 手指不压在文字上，后续每帧用它还原「手指指的是哪个字」。
    pub offset: Point<Pixels>,
}

/// 一条被点开的脚注：注文 + 那条引用在屏上的位置（弹层从这一点长出去）。
///
/// `text` 是**解析出来的注文**（`<aside epub:type="footnote">` 一类的子树文本或
/// 标记图上的 `alt`），`anchor` 是标记在窗口坐标里的位置 —— 弹层要贴着读者点的那一处。
#[derive(Clone, Debug)]
pub struct Footnote {
    pub text: String,
    pub anchor: Point<Pixels>,
}

/// 把手命中区：`(哪一端, 命中盒)` 的每帧快照。paint 每帧整表重写，事件侧只读。
type HandleHits = Rc<RefCell<Vec<(SelectionEdge, Bounds<Pixels>)>>>;

/// 拖选成立要越过的位移（逻辑像素）。更小的位移是手抖，不是选择——
/// 普通点击（含触摸合成的按下/抬起）不该留下高亮。
pub(super) const DRAG_THRESHOLD_PX: f32 = 4.0;

/// 兜底用的那行字。中英混排，顺带把 fallback 路径也验一遍。
const SAMPLE_LINE: &str = "Folio — 中文正文与 Latin 混排的一行";

impl ReaderSession {
    /// 冷启动：只读偏好、挑设备字体，**自己不找书**。
    ///
    /// 它以前会在几个硬编码路径里翻 `folio-sample.*` 并直接打开，那是书架还没有的时候
    /// 的临时办法。现在书从书架点开（[`Self::open_book`]），所以这里一律进
    /// [`Self::sample`] 那一屏，等外壳把读者点的那本换上来。
    ///
    /// **删掉那条自动打开不是清理，是修一个真 bug**：`/sdcard/Download` 也在它翻的
    /// 那几个路径里，而「全盘扫描」(`scan_roots(all_files = true)`) 扫的正是
    /// `/storage/emulated/0` —— 于是同一个文件被两处认领：阅读器把它当启动占位读，
    /// 导入管线把它当一本书收进库。实测症状就是「书架顶上有一本叫『版心样本』的书，
    /// 删掉之后下次全盘扫描又回来」。
    ///
    /// 兜底那一行仍然要留（`Source::Sample`）：它是「起来了但还没打开书」这一屏，
    /// 没有它就得让外壳保证永远不先构造阅读页，而那是个更脆的约定。
    /// 书库句柄照样带上 —— 外壳紧接着就用 `open_book` 换书，句柄丢了那条路读不出东西。
    pub fn boot(cx: &mut App) -> Self {
        let prefs = ConfigHolder::reader(cx);
        // 书库没装不是 fatal：照样能读，只是进度与偏好落不了盘，
        // 而「读了但没记住」比「直接退出」更可接受。
        let store = crate::ui::shared::app_store::handle(cx);
        tracing::debug!("ReaderSession: 启动进占位屏，等外壳按书架点选换书");
        Self::sample(&prefs, store, cx)
    }

    /// 只有那一行兜底文本时用。书库句柄**照样带上**：这一屏不是终点，
    /// 外壳紧接着就用 [`Self::open_book`] 把读者点的那本换上来，
    /// 句柄丢了那条路就永远读不出东西（只报「章节没入库」）。
    fn sample(prefs: &Reader, store: Option<Arc<Store>>, cx: &mut App) -> Self {
        let family = device_family(&prefs.font_family, cx);
        let mut effective = prefs.clone();
        effective.font_family = family.clone();
        Self {
            source: Source::Sample(SAMPLE_LINE.into()),
            wanted_family: prefs.font_family.clone(),
            prefs: effective,
            family: family.into(),
            mode: PageMode::default(),
            mode_undecided: false,
            laid_out: LaidOut::default(),
            scroll_y: 0.0,
            scroll_painted: 0.0,
            page_shift: (0.0, 0.0),
            turn: None,
            turn_seq: 0,
            page: 0,
            wheel_accum: 0.0,
            content_height: 0.0,
            reserve: Reserve::ZERO,
            header_reserve: 0.0,
            footer_reserve: 0.0,
            pending_restore: None,
            pending_anchor: None,
            read: HashSet::new(),
            saved_chapter: 0,
            saved_offset: 0,
            store,
            book_id: None,
            font_cache: FontCache::new(cx.text_system().all_font_names()),
            atlas: super::image_atlas::ImageAtlas::entity(cx),
            selection: None,
            selection_menu_open: false,
            mouse_drag: None,
            long_press_anchor: None,
            edge_drag: None,
            suppress_tap: false,
            overlay_open: false,
            content_bounds: Rc::default(),
            handle_hits: Rc::default(),
            menu_bounds: Rc::default(),
            image_viewer: None,
            image_sequence: super::image_viewer::empty_image_sequence(),
            footnote: None,
            annotations: Vec::new(),
            annotation_spans: RefCell::default(),
            // 时长不在这里起算：构造的那一刻读者还没真的翻开任何东西。
            // 起点由 `open_book` 里的 `begin_reading` 定，兜底那一屏则永远不进账。
            reading_segment_start: 0,
            reading_last_activity: 0,
            reading_clock: None,
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
        store: Option<Arc<Store>>,
        cx: &mut App,
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
        store: Option<Arc<Store>>,
        book_id: Option<i64>,
        locator: Locator,
        cx: &mut App,
    ) -> Self {
        let family = device_family(&prefs.font_family, cx);
        let mut effective = prefs.clone();
        effective.font_family = family.clone();
        // 库里存过档位就用读者的选择。没存过（第一次读这本书，或读了但从没换过档）
        // 先按连续滚动摆着，等渲染帧量到窗口宽再定默认档 —— 构造时手上没有窗口，
        // 而「装不装得下两栏」只有量出来的宽能回答。
        let stored_mode = book_id
            .and_then(|id| store.as_ref()?.position(id).ok().flatten())
            .map(|(mode, _)| mode);
        let mode_undecided = stored_mode.as_deref().is_none_or(|name| name.is_empty());
        let wanted_mode = stored_mode
            .as_deref()
            .map(PageMode::from_storage_name)
            .unwrap_or(PageMode::Continuous);
        let book = Book::new(doc, effective.clone(), locator, cx);
        let mut session = Self {
            source: Source::Book(Box::new(book)),
            wanted_family: prefs.font_family.clone(),
            prefs: effective,
            family: family.into(),
            mode: wanted_mode,
            mode_undecided,
            laid_out: LaidOut::default(),
            scroll_y: 0.0,
            scroll_painted: 0.0,
            page_shift: (0.0, 0.0),
            turn: None,
            turn_seq: 0,
            page: 0,
            wheel_accum: 0.0,
            content_height: 0.0,
            reserve: Reserve::ZERO,
            header_reserve: 0.0,
            footer_reserve: 0.0,
            // 章已经由 `Book::new` 定好了；章内偏移留到第一帧排版做完再落地。
            // 连续模式的落点是**全局 y**（章基线 + 章内行顶）：哪怕偏移是 0 也要挂
            // 请求，把「这一章的开头」摆到它拼出来的位置 —— 上一章可能正占着上方。
            pending_restore: (wanted_mode.is_continuous() || locator.offset > 0).then_some(locator),
            pending_anchor: None,
            read: read_at_open(book_id, store.as_ref()),
            saved_chapter: locator.chapter as usize,
            saved_offset: locator.offset,
            store,
            book_id,
            // 设备字体表在这里读一次；换书（可能注册了新内嵌字体）时由 `open_book` reset。
            font_cache: FontCache::new(cx.text_system().all_font_names()),
            atlas: super::image_atlas::ImageAtlas::entity(cx),
            selection: None,
            selection_menu_open: false,
            mouse_drag: None,
            long_press_anchor: None,
            edge_drag: None,
            suppress_tap: false,
            overlay_open: false,
            content_bounds: Rc::default(),
            handle_hits: Rc::default(),
            menu_bounds: Rc::default(),
            image_viewer: None,
            image_sequence: super::image_viewer::empty_image_sequence(),
            footnote: None,
            annotations: Vec::new(),
            annotation_spans: RefCell::default(),
            // 时长不在这里起算：构造的那一刻读者还没真的翻开任何东西。
            // 起点由 `open_book` 里的 `begin_reading` 定，兜底那一屏则永远不进账。
            reading_segment_start: 0,
            reading_last_activity: 0,
            reading_clock: None,
        };
        // 屏上要有读者划过的痕迹，就得先知道库里有哪些。开书这一条路（`with_document`）
        // 与换书那一条（`open_book`）都要过一遍，漏一处就是「重开书批注不见了」。
        session.load_annotations();
        // 整本书那一列图同理：两条路都要建，漏一条的症状是图片查看器那一列空的。
        session.rebuild_image_sequence();
        session
    }

    /// 从书库打开一本书。
    ///
    /// 按主键读库而不是拿 `books.path` 重新解析一遍：库里存的是**索引**
    /// （章表 + 每章在容器里的位置、CSS、目录、封面、图片元数据），正文本身按位置
    /// 回容器现取（见 `plans/text-out-of-db.md`）。这样开书不必等一次全量解析，
    /// 而库也不会与书的内容同量级。
    ///
    /// 源文件不在了的表现是「这一章取不到」而不是「书没了」：条目、进度、作者与
    /// 封面都还在，读者能看见是哪一本出了问题。
    ///
    /// 读不到的一切情况都只写日志、不动当前那一屏：读者正在读的书不该因为点错一本
    /// 坏记录而变成空白页。
    pub fn open_book(&mut self, book_id: i64, cx: &mut Context<Self>) {
        // 正文只在源文件里，所以「文件还是那份文件吗」是能不能读的前提。
        // 三种不可信各自处置：文件不在 → 连数据一起删（读者看见的是这本书从架上消失，
        // 而不是点开一片空白）；文件换了 / 索引是旧算法算的 → **投一趟后台重扫**，
        // 这一屏照旧用现有索引开，位置与批注都还在（`book_id` 没变）。
        if let Some(store) = self.store.clone() {
            match store.stale_reason(book_id) {
                Ok(Some(StaleReason::Missing)) => {
                    tracing::warn!(book_id, "ReaderSession: 源文件不在了，连数据一起删");
                    if let Err(err) = store.remove_book(book_id) {
                        tracing::error!(book_id, error = %err, "ReaderSession: 删书失败");
                    }
                    return;
                }
                Ok(Some(reason)) => {
                    tracing::info!(book_id, reason = ?reason, "ReaderSession: 索引要重扫，已交后台");
                    let pool = crate::ui::shared::services::pool();
                    // 不等：重扫必然把整本书重读一遍（要读完全书才知道的数全在导入那趟算），
                    // 而旧索引只是粗一点、不是打不开 —— 三处兜底见 `folio_core::schedule_reindex`
                    // 的文档。新索引下次开书才生效，正看着的这一屏不重建。
                    if folio_core::schedule_reindex(&pool, store, book_id).is_none() {
                        // 号没领到：这本的重扫正在飞，或今天这个进程里试过且失败了。
                        tracing::debug!(book_id, "ReaderSession: 重扫已在飞或已抑制，这一趟不再投");
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(book_id, error = %err, "ReaderSession: 查文件状态失败")
                }
            }
        }
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
                    ..DocMeta::default()
                },
                content,
                row.path,
                mode,
                locator,
            ))
        });
        let Some((meta, content, path, mode, locator)) = opened else {
            tracing::error!(
                book_id,
                "ReaderSession: 书库里读不出来（章节没入库或格式不认识）"
            );
            return;
        };
        // 库里只有章节位置，正文按位置回容器取；图片与内嵌字体同样不入库，也现取。
        let doc: std::sync::Arc<dyn OpenedDocument> = std::sync::Arc::from(
            folio_core::content::document::with_container(meta, content, Path::new(&path)),
        );
        // 定版式（CBZ / 逐页 SVG 漫画）与可重排走两条完全不同的阅读链：前者没有章、
        // 没有排版，页是既定的（见 `content::format::Content` 的两族划分）。
        self.source = if doc.is_fixed() {
            Source::Fixed(Box::new(FixedBook::new(doc, locator)))
        } else {
            // `self.prefs` 里的家族已经是这台设备解析得出来的那个，不再挑一遍。
            // `Book::new` 可能刚把书里的内嵌字体注册进设备，字体表要重读一份，
            // 缓存按新表重建（否则旧 FontKey 会命中到上一本书解析出的 Font）。
            let book = Book::new(doc, self.prefs.clone(), locator, cx);
            Source::Book(Box::new(book))
        };
        // 上一本读过的图整份放掉：`source` 是原地换的，会话实体还活着，所以
        // `observe_release` 那一条不会替我们做这件事。
        self.atlas.update(cx, |atlas, cx| atlas.clear(cx));
        self.font_cache.reset(cx.text_system().all_font_names());
        // 换了坐标系（章内字符偏移是按章算的）：旧选中指向的是上一章的字，必须丢。
        self.clear_selection();
        // 换书那一刻屏上不该还浮着上一本的脚注。
        self.footnote = None;
        // 上次是以哪一档读的，这次还用那一档：进度存的是「第几个字」，与模式无关，
        // 但读者认得的是那**一张纸**的样子。没存过就当读者还没选，
        // 交给渲染帧按窗口宽定默认档。
        self.mode = PageMode::from_storage_name(&mode);
        self.mode_undecided = mode.is_empty();
        // 上一本那一段必须先结账：`settle_reading` 按 `self.book_id` 归属，
        // 顺序写反了就会把上一本读的时间记到这一本头上（两本的账一起错）。
        self.settle_reading();
        self.book_id = Some(book_id);
        // 从这一刻起为这一本计时。
        self.begin_reading(cx);
        // 换了一本书，屏上的划线也得跟着换：批注是按 `book_id` 存的。
        self.load_annotations();
        // 换书时那一层若还开着，先关掉：查看器手里带的是**上一本**折出来的那一列，
        // 留着它就能继续左右滑到上一本的图上去 —— 计数、分享、定位原文全指着别人的书。
        self.close_image_viewer(cx);
        // 图的那一列也按新的那一本重折一次（兜底那一行没有列，`Sample` 折成空）。
        self.rebuild_image_sequence();
        self.scroll_y = 0.0;
        self.page = 0;
        self.wheel_accum = 0.0;
        self.content_height = 0.0;
        self.saved_chapter = locator.chapter as usize;
        self.saved_offset = locator.offset;
        // 章内偏移还是要等第一帧排版做完才能落地。连续模式哪怕偏移是 0 也挂请求：
        // 落点是全局 y，章首未必拼在 0 的位置。
        self.pend_restore((self.mode.is_continuous() || locator.offset > 0).then_some(locator));
        cx.notify();
    }

    /// 挂（或作废）一趟还没落地的恢复请求，并清掉还没落地的锚点。
    ///
    /// 除了带锚点的那一跳（`modules::toc::ReaderSession::restore_to_entry`，它俩必须
    /// 一起写）之外，这是**唯一**该写 [`Self::pending_restore`] 的地方：留着上一次
    /// 没落地的锚点，下一次恢复就会跳到那一跳的地方去。
    pub(super) fn pend_restore(&mut self, locator: Option<Locator>) {
        self.pending_anchor = None;
        self.pending_restore = locator;
    }
    /// 当前那本书的只读借用；兜底那一行与定版式书都没有「可重排的书」，返回 `None`。
    pub(super) fn book(&self) -> Option<&Book> {
        match &self.source {
            Source::Book(book) => Some(book),
            Source::Sample(_) | Source::Fixed(_) => None,
        }
    }

    /// 整本的翻页方向：`true` = 从右往左翻。
    ///
    /// 只有 EPUB 的 `<spine page-progression-direction="rtl">` 会给 `true`（其余格式那一格
    /// 恒为 `false`，见 `FlowDoc::progression_rtl`）。没有书（兜底那一行）按左翻。
    /// 这一问的是**手**的事：点哪一侧算下一页、箭头按钮按下去往哪走 —— 与正文往哪边流
    /// （CSS 的 `direction`，管排版）是两件事，各读各的。
    pub(super) fn progression_rtl(&self) -> bool {
        self.book().is_some_and(|book| book.progression_rtl())
    }

    /// 当前那本书的可变借用；兜底那一行与定版式书都没有「可重排的书」，返回 `None`。
    pub(super) fn book_mut(&mut self) -> Option<&mut Book> {
        match &mut self.source {
            Source::Book(book) => Some(book),
            Source::Sample(_) | Source::Fixed(_) => None,
        }
    }

    /// 当前那本定版式书的只读借用；可重排 / 兜底那一行返回 `None`。
    pub(super) fn fixed(&self) -> Option<&FixedBook> {
        match &self.source {
            Source::Fixed(book) => Some(book),
            _ => None,
        }
    }

    /// 当前那本定版式书的可变借用；可重排 / 兜底那一行返回 `None`。
    pub(super) fn fixed_mut(&mut self) -> Option<&mut FixedBook> {
        match &mut self.source {
            Source::Fixed(book) => Some(book),
            _ => None,
        }
    }

    /// 这一屏是不是定版式（页是既定的，翻页不走排版链）。
    pub(super) fn is_fixed(&self) -> bool {
        matches!(self.source, Source::Fixed(_))
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
            Source::Fixed(book) => book.title().clone(),
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

    /// 本章的页数；连续滚动时这个数没有意义（进度按像素走）。定版式读整本页数。
    pub fn page_count(&self) -> usize {
        if let Some(fixed) = self.fixed() {
            return fixed.page_count();
        }
        self.laid_out.pages
    }

    /// 当前页号。定版式读页表里的页号。
    pub fn page(&self) -> usize {
        if let Some(fixed) = self.fixed() {
            return fixed.page();
        }
        self.page
    }

    /// 本章还能往下滚多少像素。视口还没量过（第一帧之前）时是 0。
    pub(super) fn max_scroll(&self) -> f32 {
        (self.content_height - self.laid_out.viewport).max(0.0)
    }

    /// 本章已读比例。进度条与页脚都念它。
    ///
    /// 连续滚动读**视口顶那一章**的章内比例：进度条是那一章的刻度，跨章滚动时
    /// 每进一章归零重来。落库存的「第几个字」与此无关，两种口径互不干扰。
    pub fn progress(&self) -> f32 {
        // 定版式的进度按页号走（与可重排「第几个字」是两套口径，见 `LocatorKind::Page`）。
        if let Some(fixed) = self.fixed() {
            let last = fixed.page_count().saturating_sub(1);
            return if last == 0 {
                0.0
            } else {
                (fixed.page() as f32 / last as f32).clamp(0.0, 1.0)
            };
        }
        if self.laid_out.mode.is_continuous() {
            let Some(book) = self.book() else {
                return 0.0;
            };
            let gap = super::book::Book::chapter_gap(&self.prefs);
            let Some(chapter) = book.chapter_at_y(self.scroll_y, gap) else {
                return 0.0;
            };
            let base = book.base_of(chapter, gap).unwrap_or(0.0);
            let height = book
                .span_doc(chapter)
                .map(|doc| doc.height())
                .unwrap_or(0.0);
            let local_max = (height - self.laid_out.viewport).max(0.0);
            if local_max <= 0.0 {
                return 0.0;
            }
            return ((self.scroll_y - base) / local_max).clamp(0.0, 1.0);
        }
        let last = self.laid_out.pages.saturating_sub(1);
        if last == 0 {
            0.0
        } else {
            (self.page as f32 / last as f32).clamp(0.0, 1.0)
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
            // 重排会把每一页的片高与行盒换掉：正在平移的那一张是按旧行盒摆出来的，
            // 让它继续走完只会把两张对不上的纸叠在屏幕上。
            self.cancel_turn();
            if let Some(book) = self.book_mut() {
                // 拼接窗口里的每一章都吃同一份偏好：一起作废，一起重排。
                book.set_prefs_on_all(effective);
            }
        }
    }
}

/// 正在走的那一次翻页动画。
///
/// 记的是**出场那一张**而不是两张各自的位置：进场那一张永远是 `self.page` / `scroll_y`，
/// 那两个数本身就是权威，再抄一份进动画状态就多出两处会各自漂移的坐标。
///
/// 时钟不在这里存：那口表由 gpui 的 `Presence` 按 `seq` 那把键持有（见
/// `view.rs` 的 `turn_frame`），它自己续帧、自己按系统的减弱动态效果归零。
/// 在这里再存一个 `Instant`，就有了两套「走了多久」，而其中一套不会被测试时钟推着走。
#[derive(Clone, Copy)]
pub(super) struct Turn {
    /// 这一趟的时钟键。每次起动画换新键：复用同一个键会把上一趟**还没走完**的状态
    /// 接到这一趟上，读者看见的是第二次翻页只滑了一半就到位。
    pub seq: u64,
    /// 分页 / 双页：出场那一页的页号。连续滚动没有「页」，那一臂看 [`Self::scroll`]。
    pub leaving: Option<usize>,
    /// 连续滚动：补间的起点（起那一刻**画出来的**那一个数）与终点。
    ///
    /// 起点不用 `scroll_y` 而用画出来的那个：读者连点两下时，第二趟要从他眼前那一屏
    /// 起步，否则画面先跳回上一趟的终点再走，看着像卡了一下。
    pub scroll: Option<(f32, f32)>,
    /// `true` = 往后走（下一页 / 往下滚）。接反了不报错，画面是反着走的，
    /// 所以方向的判据收在 [`chrome::slide_shifts`] 那一格里钉住。
    pub forward: bool,
}

/// 开书时读一次「到过的章」。
///
/// 没有 `book_id`（`with_document` 直连的测试、兜底那一行）或库读不动时给**空集**：
/// 表现是目录里全都还没来过，而不是弹一个错误 —— 进度读不出来才是事故，
/// 浅色画不出来只是少个提示。
fn read_at_open(book_id: Option<i64>, store: Option<&Arc<Store>>) -> HashSet<usize> {
    let (Some(id), Some(store)) = (book_id, store) else {
        return HashSet::new();
    };
    store.read_chapters(id).unwrap_or_default()
}

/// 把目录那一跳带来的章内锚点 `id` 换算成章内偏移。
///
/// 只有渲染帧做得了这一步：偏移表（`Document::id_offsets`）是**排版产物**，挂恢复
/// 请求的那一刻目标章往往还没排版，所以请求里先带着 `id` 等一等。
///
/// 表里没有这个 `id`（书里的目录指向一个不存在的锚点，或锚点挂在一节没有正文的元素上）
/// 就**留在请求原本的偏移上**，也就是章首 —— 宁可跳到本章开头，也不要凭猜落在正文中间。
///
/// 做成自由函数而不是方法：调用点在 `match &mut self.source` 的分支里，那时 `self.source`
/// 已被可变借用，任何 `&mut self` 的方法都过不去；而这里的锚点与请求是两个独立字段。
pub(super) fn land_at_anchor(anchor: Option<String>, locator: Locator, book: &Book) -> Locator {
    let Some(fragment) = anchor else {
        return locator;
    };
    match book.document().id_offsets().get(&fragment).copied() {
        Some(offset) => Locator { offset, ..locator },
        None => locator,
    }
}
