//! 「一章的 XHTML → `folio_core::layout::Layout`」这一段，桌面与移动共用。
//!
//! 分成两件事：**怎么排、切成几片**在这里，**怎么摆上屏**在视图层
//! （`ui/features/reader`，两端一份实现）。这样同一本书在两端得到的是同一份
//! `Layout` 与同一套分片，屏幕上看到的行只可能因为视口尺寸不同而不同，不会因为
//! 「两端各实现了一遍层叠与断行」而不同。
//!
//! 入口的 `Chapter::source` **一定是引擎就绪的 XHTML**（契约见 `format::Chapter`）。
//! 纯文本的包装已经在 provider 里做完（`folio_core::content::format::xhtml`），所以这里
//! 没有「这个格式要不要先包一下」的分支 —— `Format` 不参与排版链。

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use folio_core::Reader;
use folio_core::content::anchor::{CharLines, TextMap};
use folio_core::content::document::Document as OpenedDocument;
use folio_core::content::dom::{Dom, NodeId};
use folio_core::content::format::css::computed::{
    Color, ComputedStyle, Length, StyledDom, TextAlign, WritingMode, cascade,
};
use folio_core::content::format::css::{Origin, RuleSet, chapter_style_rules};
use folio_core::content::hit::Hit;
use folio_core::infra::metrics::Metric;
use folio_core::infra::task::{TaskError, TaskEvent, TaskHandle, TaskManager};
use folio_core::layout::fragment::{Fragment, FragmentOptions, column_width, fragments};
use folio_core::layout::{
    BlockCache, DecorSeed, FontKey, Justify, Layout, LayoutInputs, LayoutKey, LayoutOptions,
    LineBox, LineFragment, content_hash, layout_with_blocks, quantize_px,
};
use gpui_kit::{App, LineLayout, SharedString, TextSystem, Window, WindowTextSystem};

use crate::ui::features::reader::model::book_fonts::BookFonts;
use crate::ui::features::reader::model::measure::GpuiMeasure;
use crate::ui::foundation::images::ChapterImages;

/// 打开一个文档，返回统一接口。
///
/// 走 `folio_core::content::document::open`：`Content` 的两个家族都从那里出去，界面这一层只按
/// `kind()` 分流，不看扩展名，也不再有第二个「按格式查 provider」的入口。
///
/// 返回 `Arc` 而不是 `Box`：`Book` / `FixedBook` 与绘制那条链（图片字节来源）要共用
/// 同一份文档，而字节按需取用的是它手里那个 provider。
pub fn open(path: &Path) -> folio_core::Result<Arc<dyn folio_core::content::document::Document>> {
    Ok(Arc::from(folio_core::content::document::open(path)?))
}

/// 章节源文本 → 交给排版层的 XHTML 片段。
/// `<body>` 不在（纯片段或畸形 XHTML）时退回文档根：排版层只认「从这里往下找内容」。
pub fn body_of(dom: &Dom) -> NodeId {
    dom.elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document)
}

/// 一章正文的**来路**。排版作业的第一步就是把它变成文本。
///
/// 两条来路都有真实用处，`Text` 那一支不是兼容垫：
///
/// - [`ChapterSource::Fetch`]：正文还躺在容器里，**由作业自己取**。这是这一格存在的全部
///   理由：取一章 = 一次解压 + 一次解码（实测 7–9.5 ms），2026-10-15 之前那笔钱在 UI 线程
///   的渲染帧里付（`Book::ensure_window_spans` → `make_span` → `chapter_text_at`）。
///   塞进同一个作业就是一次往返；把它拆成独立的一趟预取会是**两次**往返（文本回到界面
///   才能投排版作业），而排版队列本来就是单线程 FIFO、每个 `Document` 至多一个在飞作业。
/// - [`ChapterSource::Text`]：文本已经在手上 —— 测试替身、`ChapterLocator::Inline`
///   那种内联正文。这一支不走容器。
pub enum ChapterSource {
    Text(String),
    Fetch {
        /// 打开着的这份文档。`Document: Send + Sync`，所以能整份搬进后台线程 ——
        /// 这一条是 §十「一个步骤能进后台」五条判据里的第 1、2 条（签名不要窗口、
        /// 输入不可变），其余三条（取消检查点、结果整份搬回、失败只影响自己那一步）
        /// 由「一章是一个单元、作业带回一整份排版、取不到就当空章」天然满足。
        doc: Arc<dyn OpenedDocument>,
        chapter: usize,
    },
}

impl ChapterSource {
    /// 测试与内联正文那一条来路。
    pub fn text(source: impl Into<String>) -> Arc<Self> {
        Arc::new(Self::Text(source.into()))
    }

    /// 按位置回容器取的那一条来路。
    pub fn fetch(doc: Arc<dyn OpenedDocument>, chapter: usize) -> Arc<Self> {
        Arc::new(Self::Fetch { doc, chapter })
    }

    /// 这一章的身份 —— [`LayoutKey::chapter`](folio_core::layout::LayoutKey::chapter) 那一格。
    ///
    /// 两种来路给的是不同类型的值，而这正是各自唯一能给的：按位置取的那一路**界面手上
    /// 没有文本**，算不出内容哈希，只能给章号（一个开着的容器里第 N 章的内容不会变）；
    /// 正文已经在手上那一路没有章号可指，给内容哈希。
    ///
    /// 只在这里算一次，结果存在 `Document::chapter` 上 —— 键是每帧都要算的，
    /// 而内容哈希是 O(整章长度)。
    pub fn identity(&self) -> u64 {
        match self {
            Self::Text(source) => content_hash(source),
            Self::Fetch { chapter, .. } => *chapter as u64,
        }
    }

    /// 换章时判「还是不是同一章」。身份相同就当没换 —— 与从前那句
    /// `if source == self.source { return; }` 同一条意思，只是不再逐字节比整章文本。
    pub fn same_chapter(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

/// 一章的**解析产物**：与读者偏好无关，所以改字号、拖窗口都不必重新解析。
///
/// 缓存挂在产物而不是挂在「取回来的章文本」上，理由见 `plans/阅读管线.md` §五：
/// 文本本来就存在库里，重复付的钱在解析与可见文本坐标表上。`Arc` 是因为它同时被界面
/// （命中、锚点换算）与在飞的排版作业拿着，而这两份必须是同一章。
struct ChapterArtifacts {
    /// 建它时那段源文本的内容哈希（`layout::content_hash`，**由排版作业算** ——
    /// 界面线程在按位置取的那一路根本不持有正文）。
    ///
    /// 复用前对一遍：对不上就是换章了，`NodeId` 指的是上一章，绝不能拿去排版。
    /// 这一格与 [`LayoutKey::chapter`](folio_core::layout::LayoutKey::chapter) 是**两个问题**：
    /// 键那一格答「这是哪一章」（身份），这一格答「这份 DOM 是从哪段字节来的」（内容）。
    /// 同一个内容哈希也被批注那级缓存用着（[`Document::chapter_hash`]）。
    key: u64,
    dom: Dom,
    /// 本章的可见文本坐标表。锚点与进度都存在这套坐标里，它不随行盒变化。
    map: TextMap,
}

impl ChapterArtifacts {
    /// 源文本 → DOM + 可见文本坐标。两者必须一起建，否则坐标指的就是上一章的文本。
    fn build(source: &str) -> Self {
        let dom = Dom::parse(source.as_bytes());
        let map = TextMap::build(&dom, body_of(&dom));
        Self {
            key: content_hash(source),
            dom,
            map,
        }
    }
}

/// 一章的排版状态：源文本 + 书自己的 CSS + 读者偏好 + 本章图片索引，
/// 缓存上一次的 `Layout` 与由它切出的片。
///
/// 重排只在**排版输入真的变了**之后发生，而「真的变了」只有一个判据：
/// [`LayoutKey`](folio_core::layout::LayoutKey) 这把键换了。所以逐帧调用
/// [`Document::ensure_viewport`] 是安全的：滚动、点击、别的窗口改标题都不触发重算。
/// 过去这里有三套各自数「变了没有」的手搓键（`generation`、`applied` 那个元组、
/// `fragmented` 那个五元组），同一个失效问题在三处各答一遍；键统一之后，切片那一级
/// 只需再回答「我这把键配的那片高」—— 连续滚动 / 分页 / 双页对开三种模式在核心层
/// 是同一套代码，差别只有片高与栏数两个数。
pub struct Document {
    /// 这一章正文的来路。`Arc` 是因为它随作业搬进后台线程，而界面这一份还要留着
    /// 下次重投时再搬一遍 —— 从前这里是一段 `String`，每次提交作业都要整章克隆一份。
    source: Arc<ChapterSource>,
    /// 这一章的身份（[`ChapterSource::identity`]），换章时算一次存下来。
    ///
    /// 存下来而不是现算：这一格每帧进 [`LayoutKey`](folio_core::layout::LayoutKey)，
    /// 而内容哈希是 O(整章长度)。
    chapter: u64,
    /// 键里那两格**与帧无关**的输入哈希（作者样式表 + 读者偏好），建章时算一次。
    ///
    /// 从前这里存的是 `author_css: String`，而它存在的唯一理由就是每帧给它算一次哈希 ——
    /// 现在存算好的那两个数，原文就不必留着了（解析出来的规则在 [`Self::rules`] 里，
    /// 章内 `<style>` 由作业解析 DOM 时就地收，见 `folio_core::content::format::css::chapter_style_rules`）。
    ///
    /// `prefs` 那一格也在里面，所以 [`Self::set_prefs`] 是它唯一的重算时机。
    inputs: LayoutInputs,
    /// 本章的解析产物（DOM + 可见文本坐标），按章内容跨代次复用。
    ///
    /// 为什么留着它有两层：① 锚点换算要用（`LineRun::orig` 是**节点内**的字节区间，
    /// 没有节点文本就换不成可见文本坐标）；② 解析只做一次 —— 排版可以每帧重做
    /// （宽度、字号变了），但 HTML 解析不该跟着重做，它比层叠贵。
    artifacts: Arc<ChapterArtifacts>,
    /// 作者层规则。**整本书共用这一份**（`Arc`），由 `Book` 解析一次递进来。
    rules: Arc<RuleSet>,
    prefs: Reader,
    /// 本章的图片索引：排版期查固有尺寸，绘制期取可渲染的 `Image`。
    images: Arc<ChapterImages>,
    /// 本章图片索引的版本。`<img>` 的固有尺寸是排版输入，换索引就可能换几何，
    /// 而它既不在章文本里也不在样式里，只能单独数一格（键里的 `images_rev`）。
    images_rev: u32,
    /// 整本书的内嵌字体（别名表 + 已经注册进设备的家族名）。换章不换这一份，
    /// 所以由 `Book` 建好灌进来，而不是每章自己注册一遍。
    faces: Arc<BookFonts>,
    /// 块级排版缓存，随排版作业移进移出（队列单线程、在飞作业至多一个，所以不需要锁）。
    ///
    /// 它的键是「块节点 + 该块子树的样式哈希 + 宽度」，所以改字号只重排样式真的变了的那些块；
    /// **复用范围**（章文本、图片索引、字体集、度量口径）由 `BlockCache` 按
    /// [`context_rev`](LayoutKey::context_rev) 自己守：字体表换了而样式没换，旧块也不再复用
    /// —— 接线前那是这条缓存文档里写的「已知口径缺口」，现在那一格进了键。
    blocks: BlockCache,
    /// 手上这份排版是哪把键的产物。`None` = 还没排过（窗口第一帧之前）。
    applied: Option<LayoutKey>,
    /// 最近一次**想要**的键。作业带回它自己那把键，只有与这一格相等才采纳 ——
    /// 连着改三次字号，只有最后一代的结果会上屏（`plans/阅读管线.md` 已决第 12 条）。
    desired: Option<LayoutKey>,
    /// 在飞的排版作业：`(键, 句柄)`。至多一个 —— 新作业提交时旧句柄被丢弃
    /// （作业照跑完，结果没人收；队列单线程 FIFO，天然不会堆积）。
    pending: Option<(LayoutKey, TaskHandle<Typeset>)>,
    layout: Layout,
    /// 这一章的 CSS 自己有没有声明竖排。**纯样式事实**，与读者用哪种阅读模式无关。
    ///
    /// 为什么只能在排版之后才知道：轴写在书的 CSS 里，而 CSS 要到后台作业里才层叠。
    /// 于是第一遍按横排排（那一趟的折行宽就是栏宽），`apply_typeset` 读出问题、
    /// 把这一格订正，下一帧 `ensure_viewport` 用**列长**当折行宽重排一次。
    /// 横排的书一次就到位（这一格恒为 `false`，不会多派作业）；竖排的书每章多一次排版，
    /// 换来的是「界面不需要预先知道这本书是什么轴」。
    ///
    /// 生效还多一个条件：只有**分页**那一档真按竖排排（`paged && wants_vertical`），
    /// 理由记在 `folio_core::layout::LayoutOptions::allow_vertical` 上。
    wants_vertical: bool,
    /// 层叠结果。分片要读块级的 `break-*` 与 orphans/widows，而这些只在层叠之后存在，
    /// 所以它跟排版结果同批算、同批存，不在分片时重来一遍。
    styles: Option<StyledDom>,
    /// 「哪些块要画装饰」那张短表，与排版结果**同一批换**（[`Self::apply_typeset`]）。
    ///
    /// 为什么在这里而不是每帧现折：折一次要走**整章的块表**，而它的答案只跟这一代的
    /// 层叠结果有关 —— 从前那趟是每帧、链上每个章各一次（`plans/阅读管线.md` §十一
    /// 第 12 条）。`set_prefs` / `set_chapter` / `set_images` 都不动它：那一代排版还没
    /// 回来之前，屏上摆的仍是旧行盒与新表**相配**的那一份，清了反而会让这一帧没有底色。
    decorated: Rc<Vec<DecorSeed>>,
    /// 上次分片时的 `(排版键, 一片高)`。排版键与切片之间唯一的差别就是那片高：
    /// 栏宽与栏数都已经在键里了，字号也在（换字号会改行数，行变了切点就变了）。
    fragmented: Option<(LayoutKey, u32)>,
    /// 本章切出来的片。`Rc` 是因为渲染每帧都要拿一份而不必搬动整张表。
    slices: Rc<Vec<Fragment>>,
    /// **会话级复用的度量器**（`plans/阅读管线.md` §十一 差额表第 7 条）。
    ///
    /// 命中测试与选区矩形原来每次都现造一个 `GpuiMeasure`，而它的构造里有
    /// `texts.all_font_names()` —— 整张字体表枚举 + 排序 + 去重。拖选一段文字要
    /// 逐行求矩形，那一趟就是每次全字体表一遍。
    ///
    /// 构造参数（`prefs` 的两个字族、`faces`、两个文本系统）在一章之内都不变，
    /// 所以缓存一格就够；换章时整个 `Document` 会重造，缓存跟着走。
    measure: OnceLock<GpuiMeasure>,
    /// 已经 shaping 过的行：`(行号, 行内片段槽位)` → 那份几何。
    ///
    /// gpui 自己的 `LineLayoutCache` 只保两帧（`previous_frame` 与 `current_frame` 在
    /// `finish_frame` 里互换，`gpui-pre/text_system/line_layout.rs:586-600`），而邻章那些行
    /// 从「排版作业 shape 完」到「首次进屏」隔了远不止两帧 —— 所以进屏那一帧总要重 shape
    /// 一次，而且命中也要把整段文本哈希一遍。这一张表把它按章留住，绘制只搬运字形。
    ///
    /// **值里没有颜色**：存的是 `Arc<LineLayout>`（`font_id` + 字形 + 落点），不是
    /// `ShapedLine` —— 后者带 `decoration_runs`，那里面有主题色，烘进表就是「换了夜间
    /// 模式线还是旧色」那种看不出来的错。装饰线本来也是绘制层自己画的
    /// （见 `paint::paint_run` 里 `decoration_y` 那一段），`decoration_runs` 从没人读过。
    ///
    /// `RefCell` 而不是 `&mut self`：填表发生在渲染帧里，而那一刻手上是
    /// `Book::span_doc` 给的 `&Document`。`Document` 本来就因为 `slices: Rc<..>` 不是
    /// `Send`，加一格内部可变性不改变任何约束。
    shaped: RefCell<HashMap<(u32, u32), Arc<LineLayout>>>,
    /// 已经物化过的行：行号 → 那一行的片段单元。懒建——某一行的单元只在它第一次
    /// 进带时才建，所以驻留上界是「这一章进过屏的那些行」，随章进出 ±窗口一起释放。
    ///
    /// 失效只挂 [`Document::drop_fragments`]，与 shaping 表同一处、同一个信号。
    rows: RefCell<HashMap<u32, Rc<Vec<RowFragment>>>>,
    /// 「第几行覆盖哪些字符」那张索引，**第一次有人问时才折**（见 [`Document::char_lines`]）。
    ///
    /// 为什么懒建：折它要走一遍整章的行、每行逐片段问一次 `TextMap`，而用得到它的只有
    /// 批注回显与「字符偏移 → 行/像素」那几条查询。一本没划过字的书一分钱不必付。
    /// 为什么只折一次：那两条查询从前**每问一次走一遍整章**，一章 25 条批注实测
    /// 每帧 +1.7–2.2 ms（`plans/阅读管线.md` §十一 第 9 条）。
    ///
    /// 失效只挂 [`Document::drop_fragments`]：行号与字符区间都只在同一份排版里作数。
    char_lines: RefCell<Option<Rc<CharLines>>>,
}

/// [`Document::shaped`] 的条数上限。
///
/// 满了就**不再插**、退回 gpui 那两帧缓存，不做 LRU：一条 `Arc<LineLayout>` 连同它的
/// 字形表大约 1 KB 量级，4096 条封顶在几 MB，而驻留量随章进出 ±窗口自然释放。LRU 要为
/// 「千章书一路往下读」这种其实不会发生的场景付一套簿记 —— 表本身已经按章挂了。
const SHAPED_TABLE_CAP: usize = 4096;

/// 一行里的一个片段，值全是**章内局部量**——这是「行单元表」的一格。
///
/// 为什么要有这张表：`Scene::build` 从前每帧都从 `BlockBox::text` 现切一个 `SharedString`
/// （一次堆拷贝）再逐片段累加 x，于是「滚回走过的地方」也要把满屏的片段重新物化一遍
/// （实测一屏 62 个，`plans/阅读管线.md` §九）。物化一次就存一次，之后每帧只加三个平移数。
///
/// **文本与图片两种片段都在这里**，因为 x 只能有一个来源：要是图片还留在每帧的片段走里，
/// 「表里算的 x」与「走里累加的 x」就是两个真相，漏改一侧就是断行与绘制错位。
#[derive(Clone, Debug)]
pub enum RowFragment {
    /// 一段文本。`RowUnit::x` 是相对**行首**的偏移（不含行缩进、不含栏偏移）。
    Text(RowUnit),
    /// 一张图在行里的摆位（章内量）。
    ///
    /// **字节与固有尺寸不在这里**：那两样随图片索引变（`set_images` 会 bump `images_rev`
    /// 并清整张表），每帧现问 `ChapterImages` 才是对的。这里只存排版算好的框。
    Image(RowImage),
}

/// 一条文本片段物化一次之后的全部绘制输入。
///
/// 故意**不存**的两样，存了就是把失效问题从一处拆成两处：
///
/// - 解析好的 `gpui_kit::Font`：`FontCache` 已经有「换书 `reset`、改偏好 `clear`」那条
///   纪律（`reader/measure.rs`），表里再存一份就是第二个真相。这里存 `FontKey`，
///   每帧现解析——那是一次哈希查表加一次 `Arc` 克隆，不是这笔开销的主角。
/// - 主题色：`LayoutKey` 里没有主题，把 `run_color` 的结果烘进表就是「换了夜间模式
///   但线还是旧色」那种看不出来的错。这里存 CSS 那一格 `Option<Color>`，每帧现落回主题。
#[derive(Clone, Debug)]
pub struct RowUnit {
    /// 在**本行片段数组**里的槽位。连同行号一起构成 shaping 表的键
    /// （[`Document::shaped`]）。槽位连图片一起数，所以文本片段的槽位不会随图片增减漂移。
    pub slot: u32,
    /// 文本句柄。这一格就是这次缓存的主体：从前每帧从归一化串里切一个新的出来。
    pub text: SharedString,
    pub font: Arc<FontKey>,
    pub font_size: f32,
    /// 相对**行首**的 x（章内量）。
    pub x: f32,
    /// 基线偏移，px，正数往上。来自 `vertical-align`，绘制层把它从行的基线里减掉。
    pub baseline_shift: f32,
    /// 这一片的 inline box 高。绘制层用它定装饰线的纵向落点，而 shaping 里没有这个数。
    pub box_height: f32,
    pub underline: bool,
    pub line_through: bool,
    /// CSS 声明的文字色。`None` = 书里没写，每帧落到主题前景。
    pub color: Option<Color>,
    /// 这一段在不在一条 `<a href>` 里（`ComputedStyle::link`）。`color` 为 `None` 时绘制层
    /// 拿它落到主题的链接色 —— 出版工具的目录页普遍不写颜色，而那栏看着必须是能点的。
    pub link: bool,
    /// 字距（px），来自 `letter-spacing`。绘制时按字形所在字符序号右推 `ls × k`。
    pub letter_spacing: f32,
    /// 两端对齐：这一段起点已累计的右推量（px），以及这一行的摊法。绘制时字形再按
    /// **运行内**的隙序号右推 `justify.extra × 局部隙序号`，与 `hit`/选区同一句规则。
    /// `None` = 这一行不两端对齐。
    pub justify: Option<Justify>,
    pub justify_shift: f32,
}

/// 行内图片在行内的 x。
///
/// 文本 run 的 `x` 只是**原始前进量**，两端对齐的额外位移由每个 run 的
/// `justify_shift` 在绘制时推；图片没有 run 内逐字那一层，必须在这里把同一笔
/// 位移加上。漏掉它，图片会被前面已经摊开的字挤到左边 —— 脚注编号图看起来
/// 「盖在字上」正是这条路。
fn inline_image_x(x: f32, x_offset: f32, justify: Option<Justify>, gap_cursor: u32) -> f32 {
    x + x_offset + justify.map_or(0.0, |j| j.shift(gap_cursor))
}

/// 一行里一张图的摆位（章内量）。见 [`RowFragment::Image`] 为什么字节不进这一格。
#[derive(Clone, Debug)]
pub struct RowImage {
    pub x: f32,
    pub width: f32,
    pub height: f32,
    /// `vertical-align` 折出来的基线偏移（正数往上）。绘制时图顶要跟着抬。
    pub baseline_shift: f32,
    /// 容器路径。图片索引（`ChapterImages`）按它查字节与固有尺寸。
    pub href: String,
    /// 所属 DOM 节点。脚注注文按它解（`Document::footnote_note_for_node`）。
    pub node: NodeId,
}

/// 界面交给排版与分片的两个轴。**三种模式只差这里的三个数**：
///
/// | 模式 | `columns` | `height` |
/// |---|---|---|
/// | 连续滚动 | 1 | `0`（不分片，一片装完全部） |
/// | 分页 | 1 | 一个视口高 |
/// | 双页对开 | 2 | 一个视口高（一片 = 一栏 = 一页的一半） |
///
/// 折行发生在**栏宽**上，所以 `columns` 与 `gap` 影响排版，`height` 只影响切点：
/// 改页高不重排，改栏宽才重排。这两个数由同一份 [`Viewport`] 传进去，界面不可能
/// 把「按两栏排的版」切成「每页一栏」。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// 外壳里不属于阅读栏的**宽度**（桌面的侧栏 / 图标栏；手机上没有侧栏，是 0）。
    pub reserve: f32,
    /// 一页几栏。`0` 与 `1` 同义。
    pub columns: usize,
    /// 一栏可用的高（逻辑像素）。`0` = 不分片 = 连续滚动。
    pub height: f32,
    /// 栏间距，只在 `columns > 1` 时参与除法。
    pub gap: f32,
}

/// 一章的后台排版产出：层叠、行盒，一批算完一批换。
///
/// 全是 `Send` 的纯数据（core 那一侧不碰 gpui），所以能整份从作业线程搬回 UI 线程。
/// 分片**不在**这里面：它便宜且依赖「排好了才切」，留在渲染帧里同步做。
struct Typeset {
    /// 用完还给 `Document` 的块级缓存：下一代的排版在它上面接着复用。
    blocks: BlockCache,
    /// 作业算的是哪把键。上屏前与 [`Document::desired`] 对一遍，对不上就是陈旧的，丢掉。
    key: LayoutKey,
    /// 章的解析产物：可能是复用来的那一份（改字号、拖窗口都是这种），也可能是新解析的。
    artifacts: Arc<ChapterArtifacts>,
    styles: StyledDom,
    layout: Layout,
}

/// 作业的输入。度量要的两份文本系统以 `Arc` 搬进后台线程 —— spike① 验证过
/// `Arc<WindowTextSystem>` 可以 move 出去用，这正是排版能出渲染帧的前提。
struct TypesetInput {
    /// 从 `Document` 搬来的块级缓存，随结果搬回（见 [`Typeset::blocks`]）。
    blocks: BlockCache,
    /// 这一趟要算的那把键；排版宽度取键里量化后的 `width`，别在传一个未量化的数进来，
    /// 那样「键相同而版心差半像素」的两次排版会互相复用缓存。
    key: LayoutKey,
    /// 允许按书声明的竖排排吗。界面给的分页档 + 那一章 CSS 的合取，
    /// 与键里的 `width` 必须同批决定（横排喂栏宽、竖排喂列长）——
    /// 见 `folio_core::layout::LayoutOptions::allow_vertical`。
    allow_vertical: bool,
    /// 这一章正文的来路。`Arc` 克隆，不是整章文本克隆 —— 界面每投一次作业从前都要
    /// `self.source.clone()` 搬一整章过去，现在搬的是一个引用计数。
    source: Arc<ChapterSource>,
    /// 界面手上那份解析产物。它的内容哈希与本趟取到的正文对得上就直接用，对不上才
    /// 重新解析 ——「改一次字号不许重解析」那条计数断言
    /// （`folio_core::infra::metrics::Metric::DomParses`）钉的就是这一格。
    artifacts: Arc<ChapterArtifacts>,
    /// 书自己的样式（作者层）。章里的 `<style>` 不在这里 —— 它随 DOM 解析就地收。
    /// 交进来的是引用：深拷贝那一趟留在作业里跑（后台线程），不占 UI 线程。
    rules: Arc<RuleSet>,
    prefs: Reader,
    images: Arc<ChapterImages>,
    texts: Arc<TextSystem>,
    window_texts: Arc<WindowTextSystem>,
    body_family: String,
    mono_family: String,
    faces: Arc<BookFonts>,
}

/// 后台排版队列：整个进程一份、一条线程，与导入池同款
/// （[`folio_core::infra::task::TaskManager`]，`std` 实现、不依赖 gpui）。排版作业是
/// 毫秒级的活，FIFO 单线程就够；「只有还想要的那把键才上屏」由 [`Typeset::key`] 把关。
fn typeset_queue() -> &'static Arc<TaskManager> {
    static QUEUE: OnceLock<Arc<TaskManager>> = OnceLock::new();
    QUEUE.get_or_init(|| Arc::new(TaskManager::new(1)))
}

/// 作业体：**取正文（按需）→ 解析（按需）→ 层叠 → 排版**。测试里被同步调用，
/// 线上在后台线程里跑 —— 同一份代码。
fn typeset_job(mut input: TypesetInput) -> Result<Typeset, TaskError> {
    // 第 1 级（取正文）。`Fetch` 那一支到这一趟才回容器取：那一次解压 + 解码
    // （实测 7–9.5 ms / 章）就是这次改动从 UI 线程的渲染帧里挪走的东西。
    //
    // **取不到就当空章，不让整趟作业失败**：与从前 `Book::chapter_text_at` 那句降级同口径
    // （界面仍能把这一屏画出来，别的章还能读），而且返回错误会让 `Document::applied`
    // 永远设不上 —— 下一帧又重投一趟，一本读不出来的书就变成每帧一趟作业。
    let source = match input.source.as_ref() {
        ChapterSource::Text(text) => text.clone(),
        ChapterSource::Fetch { doc, chapter } => {
            doc.chapter_source(*chapter).unwrap_or_else(|err| {
                tracing::error!(chapter = chapter + 1, error = %err, "typeset: 取不到正文");
                String::new()
            })
        }
    };
    // 第 3 级（解析）与可见文本坐标：按章**内容**复用。内容哈希对得上就说明手上这份 DOM
    // 正是从这段文本来的，里面每个 `NodeId` 都还作数 —— 换字号、拖窗口都不必重解析。
    // 哈希在这里算而不是在界面算：`Fetch` 那一路的界面根本不持有正文。
    let content = content_hash(&source);
    let artifacts = if input.artifacts.key == content {
        input.artifacts
    } else {
        Arc::new(ChapterArtifacts::build(&source))
    };
    // 深拷贝从 UI 线程挪到这里：作业跑在排版队列上，而章里的 `<style>` 与读者层
    // 要往这份表里追加，所以只能在作业里改副本。
    //
    // 基准目录只有一个来源：`ChapterImages` 手里那一个（= 这一章在容器里的目录）。
    // 章内那块 `<style>` 与元素上的 `style="…"` 都按它折，两处不许各拿一份路径。
    let base_dir = input.images.base_dir().to_string();
    let mut rules = (*input.rules).clone();
    rules.extend(chapter_style_rules(&artifacts.dom, &base_dir));
    // 读者层的段间距。放在这里而不是 `Document::new`：那一份 `rules` 跟着作者 CSS 存，
    // 偏好一改就会让「章的样式」看起来也变了。
    rules.extend(Document::reader_rules(&input.prefs));
    let styles = cascade(
        &artifacts.dom,
        &rules,
        &Document::root_base(&input.prefs),
        &base_dir,
    );
    let measure = GpuiMeasure::new(
        input.texts,
        input.window_texts,
        input.body_family,
        input.mono_family,
        input.faces,
    );
    // 走带块级缓存的入口：与裸 `layout` 同一条主流程，只是把「样式没变的块」直接搬过来。
    let layout = layout_with_blocks(
        &mut input.blocks,
        &input.key,
        &artifacts.dom,
        &styles,
        body_of(&artifacts.dom),
        LayoutOptions {
            width: input.key.width as f32,
            // 完整排版：界面要行盒来摆字，不能用只算高的轻量档。
            light: false,
            // 与键里的 `width` 同批决定：竖排那一档喂的是列长，见 `ensure_viewport`。
            allow_vertical: input.allow_vertical,
            // 竖轴基准从键里取 —— 页高变了而排版不重做，楔块就会留着旧的一半。
            page_height: input.key.page_height.map(|h| h as f32),
        },
        &measure,
        input.images.as_ref(),
    );
    Ok(Typeset {
        blocks: input.blocks,
        key: input.key,
        artifacts,
        styles,
        layout,
    })
}

/// 两条窄化取交：字符索引给的「这段字落在哪几行」∩ 可见带给的「屏上是哪几行」。
///
/// 两边给的都是**左闭右开的连续行号**，而且各自都是超集（漏算的防线在
/// `rects_for_range_in` 里那道逐行交集判断，与 `Layout::lines_in_band` 在行盒叠放时
/// 直接给整区间），所以交集也只可能多扫、不可能漏矩形。
///
/// `char_lines` 是 `None` 表示那一章的行不按时序单调（`CharLines` 自己量出来的），
/// 这时只剩可见带那一头 —— 它总是可用。
fn narrowed_lines(char_lines: Option<Range<usize>>, band: Range<usize>) -> Range<usize> {
    let Some(char_lines) = char_lines else {
        return band;
    };
    let start = char_lines.start.max(band.start);
    let end = char_lines.end.min(band.end);
    // 两条带不相交（这一条批注根本不在这屏上）：给一个空区间，一行都不走。
    start..end.max(start)
}

/// 把一本书的作者样式表解析成一份**可以共享**的规则表。
///
/// 为什么要单独一个函数并数进计数器：那一整本 CSS 对每一章配的都是同一份规则，
/// 而从前 `Document::with_source` 每建一章就重解析一遍 —— 2026-10-15 真机拆开量到
/// **每进一章 1.5–1.9 ms**（《未来简史》23.6 KB / 323 条规则，`RuleSet::parse` 自己
/// 占 1.16 ms，release），跳一次章一次铺 4 章就是 7 ms 落在 UI 线程的一帧里。
/// `Book` 开书时调一次、把结果递给每一章；`Metric::RuleSetParses` 钉的就是
/// 「一本书 1 次，滚动 / 跳章 / 改偏好都不涨」。
pub fn author_rules(css: &str) -> Arc<RuleSet> {
    Metric::RuleSetParses.count();
    Arc::new(RuleSet::parse(css, Origin::Author))
}

impl Document {
    /// 正文**已经在手上**的一章：测试替身、内联正文（`ChapterLocator::Inline`）。
    ///
    /// 这一条自己解析 `css`。一本书会建好几个 `Document`，所以那条路走
    /// [`Document::new_with_rules`]（共用 [`author_rules`] 的那一份）。
    ///
    /// `#[cfg(test)]`：生产那一侧现在全都走 `new_with_rules`，这一条只剩测试在调；
    /// 挂着不给 lib 目标编，`cargo clippy --workspace --all-targets` 就报 never used。
    #[cfg(test)]
    pub fn new(
        source: String,
        css: &str,
        prefs: Reader,
        images: Arc<ChapterImages>,
        faces: Arc<BookFonts>,
    ) -> Self {
        Self::new_with_rules(source, css, author_rules(css), prefs, images, faces)
    }

    /// 同 [`Document::new`]，但作者层规则由调用方给（一本书共用一份）。
    pub fn new_with_rules(
        source: String,
        css: &str,
        rules: Arc<RuleSet>,
        prefs: Reader,
        images: Arc<ChapterImages>,
        faces: Arc<BookFonts>,
    ) -> Self {
        Self::with_source(
            ChapterSource::text(source),
            css,
            rules,
            prefs,
            images,
            faces,
        )
    }

    /// 正文**还要回容器取**的一章。
    ///
    /// 取的那一趟发生在排版作业里（[`typeset_job`]），所以构造这一刻界面手上没有正文，
    /// 解析产物先按空章建、等第一趟作业带回来。从前 `Book::make_span` 走的其实是同一条
    /// 形状 —— 先 `Document::new(String::new())` 再 `set_chapter(现取的那一段)`，
    /// 区别只在于那一趟取文本从 UI 线程挪进了后台。
    /// `#[cfg(test)]`：同上，生产走 `fetching_with_rules`，这一条只剩测试在调。
    #[cfg(test)]
    pub fn fetching(
        doc: Arc<dyn OpenedDocument>,
        chapter: usize,
        css: &str,
        prefs: Reader,
        images: Arc<ChapterImages>,
        faces: Arc<BookFonts>,
    ) -> Self {
        Self::fetching_with_rules(doc, chapter, css, author_rules(css), prefs, images, faces)
    }

    /// 同 [`Document::fetching`]，但作者层规则由调用方给（一本书共用一份）。
    /// 阅读链上每进一章走这一条。
    pub fn fetching_with_rules(
        doc: Arc<dyn OpenedDocument>,
        chapter: usize,
        css: &str,
        rules: Arc<RuleSet>,
        prefs: Reader,
        images: Arc<ChapterImages>,
        faces: Arc<BookFonts>,
    ) -> Self {
        Self::with_source(
            ChapterSource::fetch(doc, chapter),
            css,
            rules,
            prefs,
            images,
            faces,
        )
    }

    fn with_source(
        source: Arc<ChapterSource>,
        css: &str,
        rules: Arc<RuleSet>,
        prefs: Reader,
        images: Arc<ChapterImages>,
        faces: Arc<BookFonts>,
    ) -> Self {
        // 建好才交出去：`text_map()` 与锚点换算在第一次排版回来之前也要能用。
        // `Fetch` 那一路只能按空章建（正文还没取），第一趟作业回来后换成真的。
        let seed = match source.as_ref() {
            ChapterSource::Text(text) => text.as_str(),
            ChapterSource::Fetch { .. } => "",
        };
        // 身份在这里算一次存下来：它每帧进键，而内容哈希是 O(整章长度)。
        let chapter = source.identity();
        // 样式表与偏好这两格的哈希也在这里算一次：`layout_key()` 是每帧每个进链的章
        // 各调一次的地方，而那两样输入整个一章之内都不变（偏好只有 `set_prefs` 会动）。
        let inputs = LayoutInputs::compute(css, &prefs);
        let artifacts = Arc::new(ChapterArtifacts::build(seed));
        Self {
            source,
            chapter,
            inputs,
            artifacts,
            rules,
            prefs,
            images,
            images_rev: 0,
            faces,
            blocks: BlockCache::default(),
            applied: None,
            desired: None,
            pending: None,
            layout: Layout::default(),
            wants_vertical: false,
            styles: None,
            fragmented: None,
            decorated: Rc::new(Vec::new()),
            slices: Rc::new(Vec::new()),
            // 第一次真要命中/求矩形时才造（那也是唯一需要它的时刻）。
            measure: OnceLock::new(),
            shaped: RefCell::new(HashMap::new()),
            rows: RefCell::new(HashMap::new()),
            char_lines: RefCell::new(None),
        }
    }

    /// 这本书的内嵌字体。绘制路径与度量共用它，两边不可能拿不同的别名表。
    pub fn fonts(&self) -> &BookFonts {
        &self.faces
    }

    /// 读者偏好里**只能用 CSS 表达**的那几项 → 读者层规则。
    ///
    /// 段间距走这一层而不是根样式：`margin` 不是继承属性，挂在根上穿不到 `<p>`。
    /// 读者层在层叠里位于作者层之下（`Origin::Reader` < `Origin::Author`，与浏览器一致），
    /// 所以书自己写了 `p { margin-bottom: 2em }` 时仍按书的；没有作者声明时，
    /// `css/ua.rs` 给 `<p>` 的那一对 1em 才是兜底。
    ///
    /// 它与章文本无关，所以**不进 `ChapterArtifacts`**（那一份按内容哈希复用，
    /// 掺进偏好就等于把偏好烘进了缓存），每次作业现拼一条。
    ///
    /// 竖排那一格现在不从读者偏好来：`writing-mode` 只认书自己写的（层叠已接住，
    /// 排版层还没读，见 `css::computed::WritingMode`）。将来要读者强制竖排，落点是
    /// 这一层而不是 [`root_base`](Self::root_base) —— 根样式只是继承的种子，书在
    /// `<html>` 上写一条 `writing-mode: horizontal-tb` 就能把它压掉；读者层要赢过作者声明
    /// 得带 `!important`（`(Origin::Reader, true)` 折成 `CascadeLevel::ReaderImportant`，
    /// 那一档排在作者普通层之上，见 `css::mod` 的层叠级别表）。缓存那一级
    /// 不用动：`prefs_hash` 折的是**整份** `Reader`，加一格偏好自动换键。
    fn reader_rules(prefs: &Reader) -> RuleSet {
        let gap = prefs.paragraph_gap_em;
        // 固定三位小数：滑条的步长到不了第四位，而写全精度会让同一段距在不同机器上
        // 拼出两条字面不同的串（键里的作者样式哈希不管这一格，但日志与调试要对得上）。
        RuleSet::parse(
            &format!("p {{ margin-top: {gap:.3}em; margin-bottom: {gap:.3}em }}"),
            Origin::Reader,
        )
    }

    /// 换读者的偏好。**这里不推进任何代数**：下一帧 [`Self::ensure_viewport`] 一算键就发现
    /// `prefs_hash` 那格变了，作业自然派出去 —— 但那一格必须**在这里**重算：
    /// 键不再每帧从原文现哈希了（见 [`LayoutInputs`]），不在这儿刷新就永远停在旧值。
    /// 只有切片必须当场作废 —— 字号改了行数就改，切点跟着改。
    pub fn set_prefs(&mut self, prefs: Reader) {
        self.inputs.set_prefs(&prefs);
        self.prefs = prefs;
        // 「想要哪把键」只有 `ensure_viewport` 知道（键要含栏宽与栏数），所以这里只能把它
        // 清成「还不知道」。不清的话 `applied` 仍等于旧的 `desired`，`is_current()` 会答
        // 「跟上了」，而恢复进度与批注回显等的就是这一句。
        self.desired = None;
        self.drop_fragments();
    }

    /// 换章。同一章（身份对得上）就保留上一次的排版结果，来回翻同一章不必重算。
    ///
    /// 比的是**身份**而不是整段正文：`Fetch` 那一路这里根本没有文本可比，而比身份也不再
    /// 需要逐字节比一整章（从前那句 `source == self.source` 付的就是那一趟比较）。
    ///
    /// 解析与排版都在后台作业里（见 [`Self::request_typeset`]），这里只作废：
    /// 旧章的产物与行盒留着，新结果回来之前屏幕上还是上一章（一帧的功夫）——
    /// 锚点换算在这段时间里指着旧章，不会指到半个新章上。
    pub fn set_chapter(&mut self, source: Arc<ChapterSource>) {
        if self.source.same_chapter(&source) {
            return;
        }
        // 身份与来路**必须一起换**，否则键声称的章与作业取的章就不是同一章 ——
        // 那正是 `tests::the_key_and_the_fetched_chapter_are_the_same_chapter` 钉的坏法。
        self.chapter = source.identity();
        self.source = source;
        self.applied = None;
        self.desired = None;
        self.drop_fragments();
    }

    /// 换章时图片索引也要跟着换：`<img src>` 的相对路径基准目录随章变化。
    ///
    /// 图片索引单独进位：`<img>` 的固有尺寸是排版输入，而它既不在章文本里也不在样式里，
    /// 不换这一格就没人知道「同一段文字现在该占多大的洞」。
    pub fn set_images(&mut self, images: Arc<ChapterImages>) {
        self.images = images;
        self.images_rev += 1;
        self.applied = None;
        self.desired = None;
        self.drop_fragments();
    }

    /// **排版失效键的唯一构造点**（`plans/阅读管线.md` §四：一个键、一处构造、各级缓存共用）。
    ///
    /// 界面不许自己拼键、也不许只比其中几格：`width` 是**栏宽**（折行发生在它上头），
    /// 整窗宽不进键 —— 窗口变宽而栏宽被 `max_measure` 卡住时，排版本来就不该重做。
    ///
    /// 这一格**每帧每个进链的章各调一次**，所以它不做任何哈希：样式与偏好的哈希在
    /// [`LayoutInputs`] 里算好（建章与 `set_prefs` 时），章身份在 `self.chapter` 里算好
    /// （换章时）。这里只搬那几个数与一次量化。
    pub fn layout_key(&self, width: f32, columns: usize, page_height: Option<f32>) -> LayoutKey {
        LayoutKey::compose(
            self.inputs,
            self.chapter,
            width,
            columns,
            page_height,
            self.images_rev,
            self.faces.revision(),
        )
    }

    /// 排版结果是否已经跟上当前的输入。恢复阅读进度要等它：
    /// 「第几个字摆在哪一像素」只有当下那把键的排版才答得对。
    pub fn is_current(&self) -> bool {
        self.desired.is_some_and(|want| self.applied == Some(want))
    }

    /// 本章源文本的哈希 —— 「按章缓存一份解好锚的批注」那级的键。
    ///
    /// 从前的这里是排版代数，于是读者每动一次设置都要把整章批注重解一遍；而批注解出的
    /// 是**可见文本坐标**，它只跟章文本有关，与字号、栏宽都无关。换章之后这一格必然变，
    /// 因为它取的就是当前那份解析产物的键 —— 缓存与 `text_map()` 出自同一份 DOM，
    /// 这正是「不许拿上一章的坐标答本章的问题」那条。
    pub fn chapter_hash(&self) -> u64 {
        self.artifacts.key
    }

    /// 作废切片缓存、shaping 表、行单元表与那份行索引。排版一作废就必须跟着作废它们 ——
    /// 切点读的是行盒，而行号、片段槽位、字符区间这三个身份都只在同一份排版里作数。
    ///
    /// 这一处是**唯一的失效信号**：`set_chapter` / `set_prefs` / `set_images` /
    /// `apply_typeset` 四个都走它，所以三张缓存表都不必自己再判一次键。
    ///
    /// 索引清了不等于下一帧就重折：它是懒建的，没人问（没批注、没选区、没恢复落点）
    /// 就一直不折。而新一代排版落地前那一帧，屏上还是旧行盒 —— 那时重建的索引读的也是
    /// 同一份旧 `layout`，与它配得上，不存在「新索引配旧行盒」那种错配。
    fn drop_fragments(&mut self) {
        self.fragmented = None;
        self.slices = Rc::new(Vec::new());
        self.shaped.borrow_mut().clear();
        self.rows.borrow_mut().clear();
        *self.char_lines.borrow_mut() = None;
    }

    /// 那一代排版的「行 ↔ 字符区间」索引：第一次问时折，之后每次问都是同一份 `Rc`。
    ///
    /// 折一次 O(行数)，换来的是两条查询只碰与查询相交的那几行 —— 它们从前每问一次
    /// 就把整章的行走一遍，而批注回显是「每条批注 × 每帧 × 每个进链的章」问一次。
    /// 走 [`Document::drop_fragments`] 失效，所以只有换代那一回会真的重折。
    fn char_lines(&self) -> Rc<CharLines> {
        if let Some(built) = self.char_lines.borrow().as_ref() {
            return built.clone();
        }
        let built = Rc::new(CharLines::build(self.layout(), self.dom(), self.text_map()));
        *self.char_lines.borrow_mut() = Some(built.clone());
        built
    }

    /// 取一条已经 shape 好的行；表里没有就调 `shape` 现 shape 一次并存进去。
    ///
    /// `key` 是**章内局部身份**（行号, 行内片段槽位），由 `scene::Scene::build` 在摊 run
    /// 时按 enumerate 盖上；兜底那一行与测试里手搓的 run 不带身份（`None`），调用方跳过它。
    /// 表满之后（[`SHAPED_TABLE_CAP`]）不再插，那一趟就退回 gpui 的两帧缓存。
    ///
    /// `shape` 里要数 `Metric::ShapeLineCalls` —— 那一格的口径是「表未命中一次」，
    /// 不是「每条 run 一次」，所以计数必须留在闭包里而不是这里。
    pub fn shaped_line(
        &self,
        key: (u32, u32),
        shape: impl FnOnce() -> Arc<LineLayout>,
    ) -> Arc<LineLayout> {
        if let Some(hit) = self.shaped.borrow().get(&key) {
            return hit.clone();
        }
        let layout = shape();
        let mut table = self.shaped.borrow_mut();
        if table.len() < SHAPED_TABLE_CAP {
            table.insert(key, layout.clone());
        }
        layout
    }

    /// 第 `line` 行的片段单元；表里没有就现建一条，并把建出来的条数数进
    /// `Metric::SceneRuns`。
    ///
    /// 口径（2026-10-15 起）：**这一帧新建了几条单元**，不是「这一帧摆了几条 run」。
    /// 前者才是被缓存的那笔钱——一条单元的主体是那段从归一化串里切出来的 `SharedString`。
    ///
    /// 给回 `Rc` 而不是借用：调用方（`Scene::build`）此刻正借用着同一份 `Layout`，
    /// 把 `RefCell` 的借用留在外面会让两侧打架；抬一次引用计数就把借用当场收回去。
    pub fn row_units(&self, line: usize) -> Rc<Vec<RowFragment>> {
        let key = line as u32;
        if let Some(hit) = self.rows.borrow().get(&key) {
            return hit.clone();
        }
        let row = Rc::new(self.build_row(line));
        Metric::SceneRuns.add(row.len() as u64);
        self.rows.borrow_mut().insert(key, row.clone());
        row
    }

    /// 建一行的单元：把片段数组走一遍，文本在这里切出那个 `SharedString`（**一章一次**，
    /// 之后每帧只搬运句柄），图片在这里记下排版算好的框。
    ///
    /// `x` 在这里逐片段累加，相对**行首**；行缩进（`LineBox::x`）与栏偏移留在每帧那一侧，
    /// 因为它们里面含「这一片摆在窗口哪里」，不是章内量。
    fn build_row(&self, line: usize) -> Vec<RowFragment> {
        let Some(line_box) = self.layout.lines.get(line) else {
            return Vec::new();
        };
        // 行文本住在所属块的那个归一化串里（`LineRun::norm` 是它的字节区间）。
        let Some(block) = self.layout.block_of_line(line) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(line_box.fragments.len());
        let mut x = 0.0;
        // 两端对齐的隙游标：跨片段累计，给每个 run 记下它起点已摊到多少（`justify_shift`）。
        // 绘制时 run 内再按 `justify.extra × 局部隙序号` 逐字推 —— 与 `hit`/选区同一句规则。
        let justify = line_box.justify;
        let mut gap_cursor = 0u32;
        for (slot, fragment) in line_box.fragments.iter().enumerate() {
            let slot = slot as u32;
            match fragment {
                LineFragment::Text(run) => {
                    let text = block.text.get(run.norm.clone()).unwrap_or("");
                    if !text.is_empty() {
                        out.push(RowFragment::Text(RowUnit {
                            slot,
                            text: SharedString::new(text),
                            font: run.style.font.clone(),
                            font_size: run.style.font_size,
                            x,
                            baseline_shift: run.style.baseline_shift,
                            box_height: run.style.box_height(),
                            underline: run.style.underline,
                            line_through: run.style.line_through,
                            color: run.style.color,
                            link: run.style.link,
                            letter_spacing: run.style.letter_spacing,
                            justify,
                            justify_shift: justify.map_or(0.0, |j| j.shift(gap_cursor)),
                        }));
                    }
                    // 这一段点过的隙数（空格或字符，看摊法）；空片段也照数，与排版同源。
                    if let Some(j) = justify {
                        gap_cursor += text.chars().map(|ch| j.advance_cursor(ch)).sum::<u32>();
                    }
                    // 空片段不画，但照样占宽，否则后面所有 x 一起偏。
                    x += run.width;
                }
                LineFragment::Image(placed) => {
                    out.push(RowFragment::Image(RowImage {
                        x: inline_image_x(x, placed.x_offset, justify, gap_cursor),
                        width: placed.width,
                        height: placed.height,
                        baseline_shift: placed.baseline_shift,
                        href: placed.href.clone(),
                        node: placed.node,
                    }));
                    x += placed.advance;
                }
                // 列表标记：文字是它自己的（不在块文本里），位置在行首之外。
                // **不**把宽度加进 `x` —— 它挂在流动之外，正文一个字都不该挪。
                LineFragment::Marker(marker) => {
                    if !marker.text.is_empty() {
                        out.push(RowFragment::Text(RowUnit {
                            slot,
                            text: SharedString::new(marker.text.clone()),
                            font: marker.style.font.clone(),
                            font_size: marker.style.font_size,
                            x: x + marker.offset,
                            baseline_shift: 0.0,
                            box_height: marker.style.box_height(),
                            underline: false,
                            line_through: false,
                            color: marker.style.color,
                            // 列表标记不是链接的一部分：`<li><a>…` 里那个点/数字不该跟着染蓝。
                            link: false,
                            letter_spacing: marker.style.letter_spacing,
                            // 标记挂在流动之外，不参与两端对齐。
                            justify: None,
                            justify_shift: 0.0,
                        }));
                    }
                }
            }
        }
        out
    }

    #[allow(dead_code)] // 面板与测试读；渲染路径目前直接走字段
    pub fn prefs(&self) -> &Reader {
        &self.prefs
    }

    pub fn images(&self) -> &Arc<ChapterImages> {
        &self.images
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// 这一代的装饰短表（[`Self::decorated`]）。绘制侧按可见带裁它，不再走整章块表。
    pub fn decor_seeds(&self) -> &[DecorSeed] {
        &self.decorated
    }

    pub fn lines(&self) -> &[LineBox] {
        &self.layout.lines
    }

    /// 可见带 `[from_y, to_y)` 里那些行的下标区间 —— 绘制那一帧只该遍历这一段。
    ///
    /// 把 `0..行数` 整个交给场景层再逐行比 y，等于每帧付一次「整章行数」的钱，而其中
    /// 绝大多数行根本不在屏上（`plans/阅读管线.md` §十一 第 4 条）。区间是引擎在排版结果
    /// 自己那张索引上二分出来的，界面不算几何。
    pub fn visible_lines(&self, band: Range<f32>) -> Range<usize> {
        self.layout.lines_in_band(band.start, band.end)
    }

    /// `#[cfg(test)]`：只有测试拿它数行 —— 界面那一侧走 [`Document::visible_lines`] 的区间。
    #[cfg(test)]
    pub fn line_count(&self) -> usize {
        self.layout.lines.len()
    }

    /// 每行的可见文本。测试用它断言「哪一行是什么」，状态栏用它取当前行。
    pub fn line_texts(&self) -> Vec<String> {
        (0..self.layout.lines.len())
            .map(|ix| self.layout.line_text(ix).to_string())
            .collect()
    }

    /// 整章的内容总高（像素），滚动范围由它算。
    pub fn height(&self) -> f32 {
        // 沿**推进轴**的整章长度：横排就是 `layout.height`（一行行往下叠），竖排是
        // `layout.width`（一列列往左叠）。这一格喂的是「切片 / 进度 / 跳转」那一类
        // 「这本书还剩多少」的问句，它们问的从来都是沿阅读方向走了多远。
        if self.layout.vertical() {
            self.layout.width
        } else {
            self.layout.height
        }
    }

    /// 版心宽度 = 视口宽 − `pane_reserve` − 左右页边距。
    ///
    /// **`pane_reserve` 是外壳里不属于阅读栏的宽度**（桌面是侧栏 / 图标栏，移动端是 0：
    /// 那边的导航栏在底部，占的是高，扣在视口高上而不是这里）。
    /// 这一步量不到 flex 之后的真实栏宽，所以由外壳把占位报进来；版心必须 ≤ 阅读栏实际宽度，
    /// 否则引擎按宽列折行、屏幕按窄栏裁剪，居中的标题就会被推到栏外。
    ///
    /// `max_measure == 0` 是「不设上限、随窗口铺满」的**哨兵值**（见 `folio_core::Reader`），
    /// 不是真的 0 像素上限 —— 当成上限去 `usable.min(0)` 就会塌成 1px 版心、一行只剩一个字。
    pub fn content_width(window: &Window, prefs: &Reader, pane_reserve: f32) -> f32 {
        let viewport = f32::from(window.viewport_size().width);
        let usable = viewport - (pane_reserve + prefs.margin_x * 2.0);
        let capped = if prefs.max_measure > 0.0 {
            usable.min(prefs.max_measure)
        } else {
            usable
        };
        capped.max(1.0)
    }

    /// 根样式 = 读者偏好。书自己的 CSS 能覆盖它（作者层高于读者普通层，与浏览器一致），
    /// 读者的 `!important` 终究能压回来（M5）。
    fn root_base(prefs: &Reader) -> ComputedStyle {
        ComputedStyle {
            font_size: prefs.font_size,
            line_height: prefs.line_height,
            font_family: prefs.font_family.clone(),
            text_align: if prefs.justify {
                TextAlign::Justify
            } else {
                // 读者的「不两端对齐」要的是**贴起始边**，不是贴物理左边：阿拉伯语的书
                // 关掉两端对齐应该右对齐。CSS 的初值本来也是 `start`。
                TextAlign::Start
            },
            text_indent: Length::Px(prefs.font_size * prefs.indent_em.max(0.0)),
            ..ComputedStyle::default()
        }
    }

    /// 请求一次排版（不阻塞）：键没变就什么都不做，变了就把层叠 + 排版丢给后台队列
    /// （解析按需 —— 产物对得上就不重解析，见 [`typeset_job`]），渲染帧照常用上一份结果画。
    /// 测试里同步跑同一段作业体，结果当帧可断言 —— 覆盖的代码与线上同一条。
    fn request_typeset(
        &mut self,
        key: LayoutKey,
        allow_vertical: bool,
        texts: Arc<TextSystem>,
        window_texts: Arc<WindowTextSystem>,
    ) {
        if self.applied == Some(key) {
            return; // 已应用的这份就是想要的
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|(pending, _)| *pending == key)
        {
            return; // 同一把键的作业已经在飞
        }
        let input = TypesetInput {
            blocks: std::mem::take(&mut self.blocks),
            key,
            allow_vertical,
            source: self.source.clone(),
            artifacts: self.artifacts.clone(),
            rules: Arc::clone(&self.rules),
            prefs: self.prefs.clone(),
            images: self.images.clone(),
            texts,
            window_texts,
            body_family: self.prefs.font_family.clone(),
            mono_family: self.prefs.mono_font_family.clone(),
            faces: self.faces.clone(),
        };
        if cfg!(test) {
            match typeset_job(input) {
                Ok(out) => self.apply_typeset(out),
                Err(err) => tracing::error!(
                    chapter_id = key.chapter,
                    width = key.width,
                    error = %err,
                    "typeset: 排版失败"
                ),
            }
            return;
        }
        let handle = typeset_queue().submit(move |_| typeset_job(input));
        self.pending = Some((key, handle));
    }

    /// 收后台排版的结果（渲染帧经 [`Self::ensure_viewport`] 间接调用）。
    /// 只有**仍然想要**的那把键的结果才采纳；这期间章、偏好、宽度又变了，这一份就陈旧，
    /// 丢掉即可 —— 新的作业已经排在后面。
    fn poll_typeset(&mut self) {
        let Some((key, handle)) = self.pending.as_ref() else {
            return;
        };
        // 键是 Copy 的小结构，抄一份出来，后面要改 `self.pending`。
        let key = *key;
        let mut outcome: Option<Result<Typeset, TaskError>> = None;
        let mut done = false;
        while let Ok(event) = handle.events.try_recv() {
            match event {
                TaskEvent::Finished(result) => {
                    outcome = Some(result);
                    done = true;
                }
                TaskEvent::Cancelled => done = true,
                _ => {}
            }
        }
        if !done {
            return;
        }
        self.pending = None;
        match outcome {
            Some(Ok(out)) => {
                if out.key == key && self.desired == Some(key) {
                    self.apply_typeset(out);
                }
            }
            Some(Err(err)) => tracing::error!(
                chapter_id = key.chapter,
                width = key.width,
                error = %err,
                "typeset: 后台排版失败"
            ),
            None => {}
        }
    }

    /// 把一份后台产出换进来。行盒换了，切点也就换了 —— 切片必须跟着作废，
    /// 否则分页翻到的是上一页的行；重切在同一帧的 `ensure_viewport` 里同步做。
    fn apply_typeset(&mut self, out: Typeset) {
        // 缓存不论这一份结果采不采纳都要收回来：它是可再生的中间物，
        // 留着比扔掉省一次整章重排，而复用范围（章 / 字体 / 度量）变了它自己会清。
        self.blocks = out.blocks;
        self.artifacts = out.artifacts;
        // 装饰表与这一代排版结果同批折：`layout` 与 `styles` 一到，表就是它们的函数，
        // 之后每帧只裁短表（`Scene::build` 那条路）。
        self.decorated = Rc::new(out.layout.decor_seeds(&out.styles));
        // 轴是样式事实，只有层叠之后才看得见：这里读一次，下一帧的折行宽就喂对了。
        // 读的必须是**这一代**的样式表，而 `body` 与排版作业用的是同一个来路（`body_of`）。
        let wants = out
            .styles
            .get(body_of(&self.artifacts.dom))
            .is_some_and(|style| style.writing_mode != WritingMode::HorizontalTb);
        self.wants_vertical = wants;
        self.styles = Some(out.styles);
        self.layout = out.layout;
        self.applied = Some(out.key);
        self.drop_fragments();
    }

    /// 排版一章。度量要用**窗口级**文本系统（`layout_line` 只挂在它上面），
    /// 这两个 `Arc` 在这里拿，随作业搬进后台线程；这一步本身不再阻塞帧。
    #[allow(dead_code)] // 面板与测试读；渲染路径走 `ensure_viewport`
    pub fn ensure_layout(&mut self, window: &mut Window, cx: &mut App, pane_reserve: f32) {
        let _ = self.ensure_viewport(
            window,
            cx,
            Viewport {
                reserve: pane_reserve,
                columns: 1,
                height: 0.0,
                gap: 0.0,
            },
        );
    }

    /// 排版 + 分片，返回**栏宽**（绘制要按它摆每栏的左边界）。
    ///
    /// 一次把整章切完而不是逐页现算：`Fragmentainer` 的游标是为「翻到第 500 页」准备的，
    /// 而这里一片一章最多几千行，一次切完仍然只在 `(排版键, 片高)` 这一对变了时发生，
    /// 换来的是进度条随手就有总页数。排版本身在后台队列上跑（见 [`Self::request_typeset`]），
    /// 首帧拿不到行盒就先空着一帧 —— 结果回来，下一帧整章上屏。
    pub fn ensure_viewport(
        &mut self,
        window: &mut Window,
        cx: &mut App,
        viewport: Viewport,
    ) -> f32 {
        let paged = viewport.height > 0.0;
        // 竖排要三样同时成立：书自己声明了（那一格是排完才知道的）、读者在用分页、
        // 而且是**单栏**。双页对开那一档这里不当竖排：切片按 `columns=1` 切、界面按
        // `columns=2` 取片会互相取错页，而「一页两栏各装几列」本来就是竖排没接的那件事。
        // 连续滚动不转轴的理由记在 `LayoutOptions::allow_vertical`。
        let vertical = paged && viewport.columns.max(1) == 1 && self.wants_vertical;
        let columns = if vertical { 1 } else { viewport.columns.max(1) };
        let total = Self::content_width(window, &self.prefs, viewport.reserve);
        // 折行永远发生在**内联轴**上，切片永远沿**推进轴**切，两根轴在竖排里对调：
        // 横排喂栏宽、切片量页高；竖排喂列长（= 可用高）、切片量页宽。而「一页几栏」在
        // 竖排里就是「一页几列」，那是分片器按列厚塞出来的，不是界面给的数 —— 所以给 1。
        let (wrap, slice) = if vertical {
            (viewport.height.max(1.0), total)
        } else {
            (
                column_width(total, columns, viewport.gap).max(1.0),
                viewport.height,
            )
        };
        // 竖轴基准只有**横排 + 分页**那一档给得出：一片的高就是浏览器里 `body` 的那个
        // 包含块高，`html,body { height:100% }` 加空楔块那一族垂直居中写法靠它落数
        // （判据与为什么不顺手把根盒也定高，见 `LayoutOptions::page_height`）。
        // 滚动那一档 `viewport.height` 本身就是 0（上面 `paged` 就是照它判的）；竖排那一档
        // 转轴后百分比高对应的是列长，那条映射还没做 —— 两边都给 `None`，
        // 行为与这一格出现之前逐字相同。
        let page_height = if paged && !vertical {
            Some(slice)
        } else {
            None
        };
        // 键在这里算，算是「这一帧想要什么」的唯一表述：排版、切片、绘制三级都从它出发。
        let key = self.layout_key(wrap, columns, page_height);
        let width = key.width as f32;
        // 片长同样量化一档，且**切片用的就是量化后的那个数**：排版按 A 宽折行、切片按 B 高切、
        // 绘制按 C 摆，三方都以为自己对，其实是三方各对（`quantize_px` 那条纪律的另一半）。
        let slice_height = quantize_px(slice.max(0.0));
        let height = slice_height as f32;
        self.desired = Some(key);
        // 先收上一帧的作业成果，再看要不要派新活 —— 顺序反了会把刚回来的结果当旧的丢掉。
        self.poll_typeset();
        self.request_typeset(
            key,
            vertical,
            cx.text_system().clone(),
            window.text_system().clone(),
        );
        // 作业在飞就必须再要一帧：`poll_typeset` 只有渲染帧会调，而后台那一份完成时
        // 没有任何人知道。不主动要帧的话，「排版早就排好了、屏上却一个字都没有」会一直
        // 挂着，直到读者随手碰一下屏幕 —— 平板上尤其明显（没有键盘与鼠标带来的零散重绘）。
        if self.pending.is_some() {
            window.request_animation_frame();
        }

        // 排版在手（键对得上）才切片；否则沿用上一份（首帧可能还没有）。
        if self.applied != Some(key) {
            return width;
        }
        if self.fragmented == Some((key, slice_height)) {
            return width;
        }
        let options = FragmentOptions { height, columns };
        let slices = match &self.styles {
            Some(styles) => fragments(&self.layout, styles, options),
            // 没排过版就没有行盒可切（第一帧之前）。
            None => Vec::new(),
        };
        self.fragmented = Some((key, slice_height));
        self.slices = Rc::new(slices);
        width
    }

    /// 本章切出来的片，顺序是「页优先、页内按栏」。
    pub fn slices(&self) -> Rc<Vec<Fragment>> {
        self.slices.clone()
    }

    /// 一共几页。没分过片时算 1 —— 空的一页也得让读者翻得动、进度条也才有分母。
    pub fn page_count(&self) -> usize {
        self.slices.last().map_or(1, |last| last.page + 1)
    }

    /// 第 `page` 页第 `column` 栏。`columns` 由调用方给（界面才知道现在几栏），
    /// 越界返回 `None`：最后一页本来就可能不满。
    pub fn slice_of(&self, page: usize, column: usize, columns: usize) -> Option<&Fragment> {
        self.slices
            .get(page.saturating_mul(columns.max(1)) + column)
    }

    /// 第 `line` 行落在第几页。行号越界就给最后一页：恢复进度要的是「最接近的那一页」，
    /// 不是「翻不到的一页」。
    pub fn page_of_line(&self, line: usize) -> usize {
        self.slices
            .iter()
            .find(|slice| !slice.empty && line < slice.lines.end)
            .map_or_else(|| self.page_count() - 1, |slice| slice.page)
    }

    /// 第 `page` 页开头那一个可见字符的章内偏移。跨章滑动落地时用它落库；
    /// 页为空（还没切出来 / 末页之后）给 0。
    pub fn offset_of_page(&self, page: usize, columns: usize) -> usize {
        let Some(slice) = self.slice_of(page, 0, columns) else {
            return 0;
        };
        self.coordinate_of_line(slice.lines.start)
    }

    // ---- 进度与锚点：滚动位置 ↔ 可见文本偏移 ----
    //
    // 两端各自换算过一次就会各错一次：桌面读虚拟列表的像素偏移、移动读自己的
    // `scroll_y`，但「哪一行覆盖这个像素、这行的第一个字是第几个字」必须是同一个答案。
    // 所以这三个方法放在共用层，实现全在 `folio_core::content::anchor`。

    /// 本章的可见文本坐标表（批注与书签在上面建锚点）。
    ///
    /// 它与 [`Self::dom`] 必须出自同一份解析产物 —— 坐标表的区间是按那棵 DOM 的
    /// 文本节点算的，配错了就是把上一章的偏移安到本章的字上。两者都只从 `artifacts` 取。
    pub fn text_map(&self) -> &TextMap {
        &self.artifacts.map
    }

    /// 本章的 DOM。命中、锚点、目录落点都从它出发。
    pub fn dom(&self) -> &Dom {
        &self.artifacts.dom
    }

    /// 某个章内字符偏移处所在的那个链接的 `href`（不在链接里就是 `None`）。
    ///
    /// 从命中到的**文本节点**沿 `ancestors_including_self` 往上找最近的一个
    /// `<a>`：一个链接可能横跨若干嵌套的 inline（`<a><em>字</em></a>`），
    /// 只认「这点文字挂在哪条链上」而不是「这一格等于哪个元素」。
    /// 取最内层那一条 —— 嵌套链接（畸形，但真实书里有）取内层更贴近读者点的那个字。
    pub fn link_href_at(&self, offset: usize) -> Option<String> {
        let node = folio_core::content::hit::node_of(self.text_map(), offset)?;
        let dom = self.dom();
        for id in dom.ancestors_including_self(node) {
            if dom.tag_name(id) == Some("a")
                && let Some(href) = dom.attr(id, "href")
            {
                return Some(href.to_string());
            }
        }
        None
    }

    // ---- 脚注：一条引用（`<a epub:type="noteref">`）→ 注文 ----
    //
    // 判定与取文分两层：先看这一处挂在不在一条**脚注引用**上（`<a>` 自己标了
    // `noteref`，或它指向的元素是脚注正文），再看注文从哪儿取（目标元素的子树文本，
    // 取不到就退回标记图上的 `alt` / `zy-footnote`）。KDoc 里两种真实写法都覆盖：
    // 有的书 marker 里是一张 10px 的「注」图（`蛇结`），有的书是一串上标数字。

    /// 屏幕命中到的那个字符若在一条脚注引用里，返回它的注文。
    pub fn footnote_note_at(&self, offset: usize) -> Option<String> {
        let node = folio_core::content::hit::node_of(self.text_map(), offset)?;
        self.footnote_note_for_node(node)
    }

    /// 某个 DOM 节点（文字或图片）若在一条脚注引用里，返回它的注文。
    ///
    /// 标记是一张图时传的就是那张图的节点 —— 正文里的「注」图正是这么被点中的：
    /// 它在 `<a>` 里，但 `<a>` 自己没有一个可见文本字符可命中。
    pub fn footnote_note_for_node(&self, node: NodeId) -> Option<String> {
        let href = self.footnote_href_at_node(node)?;
        // 1) 目标元素就在本章：取它的子树文本。判它是不是脚注正文也在这一步做，
        //    所以「标了 noteref 但目标不是脚注」时不会误当成脚注。
        if let Some(text) = self.footnote_body_text_for_href(&href) {
            return Some(text);
        }
        // 2) 跨文件目标不在本章：退回标记自己的 `alt` / `zy-footnote`（老书把注文
        //    塞在标记图的这两个属性里，正文里另有 `<aside>`）。找不到就交给调用方
        //    走「跳到那条链接」的常规路。
        let dom = self.dom();
        for id in dom.ancestors_including_self(node) {
            for name in ["zy-footnote", "alt", "title"] {
                if let Some(v) = dom.attr(id, name)
                    && useful_marker_text(v)
                {
                    return Some(v.trim().to_string());
                }
            }
            if dom.tag_name(id) == Some("a") {
                break;
            }
        }
        None
    }

    /// 某个元素挂在脚注引用链上时，返回那条 `<a href>`（最内层优先）。
    fn footnote_href_at_node(&self, node: NodeId) -> Option<String> {
        let dom = self.dom();
        for id in dom.ancestors_including_self(node) {
            if dom.tag_name(id) == Some("a") {
                let href = dom.attr(id, "href")?;
                return footnote_anchor_is_ref(dom, id, self, href).then(|| href.to_string());
            }
        }
        None
    }

    /// `href`（`#id` 或 `某文件.xhtml#id`）指向的那个元素若是**本章**的脚注正文，
    /// 返回它的文本。
    ///
    /// 路径那一截**有意不查**：`Document` 只管一章，认不出「part0004.xhtml」是不是
    /// 自己这个文件。判定改由「这段 `#id` 在本章解出来的东西像不像注文」兜底
    /// （见 [`is_footnote_body`]）—— 《罗生门》的标记正是 `part0004.xhtml#m1`
    /// 这种带文件名的同章链接，只认纯 `#id` 会把它的脚注全漏掉，而普通目录链接
    /// （`part0004.xhtml#a005` 指向 `<h1>`）不会命中「注文」那一档。
    fn footnote_body_text_for_href(&self, href: &str) -> Option<String> {
        let fragment = folio_core::content::format::fragment_of(href)?;
        self.footnote_body_text(&fragment)
    }

    /// 本章里某个 `id` 处的脚注正文（`<aside epub:type="footnote">` / `<p class="note">`
    /// 一类）的文本。
    ///
    /// 文本优先取那个元素自己的子树；它只是个锚点、本身没字时（《罗生门》的
    /// `<a id="m1"></a>`）往最近的、有字的祖先块取 —— 那一整段才是注文。
    pub fn footnote_body_text(&self, id: &str) -> Option<String> {
        let dom = self.dom();
        for (node, _) in dom.elements() {
            if dom.id_attr(node) != Some(id) {
                continue;
            }
            if !is_footnote_body(dom, node) {
                return None;
            }
            for anc in dom.ancestors_including_self(node) {
                let text = dom.text_of_subtree(anc);
                let text = text.trim();
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            }
            return None;
        }
        None
    }

    /// 本章所有带 `id` 的元素 → 它第一个可见字符的偏移。目录跳转与「现在读到哪一条」
    /// 都查这张表。
    ///
    /// 一次走完 DOM 而不是逐个按 id 查：一章里挂着十几个小节锚点是常态，逐个查就是
    /// 十几趟全树遍历，而合成一张表只要一趟。
    ///
    /// 元素自己的 `node_chars` 一定是 `None`（坐标表只给文本节点建区间），所以往下找到
    /// 子树里第一个进了表的节点 —— 标题的文字就在它自己那棵子树的最前面。
    pub fn id_offsets(&self) -> HashMap<String, usize> {
        let dom = self.dom();
        let mut out: HashMap<String, usize> = HashMap::new();
        for (node, _) in dom.elements() {
            let Some(id) = dom.id_attr(node) else {
                continue;
            };
            // 规范要求 `id` 全章唯一。真出现重复就留文档序里在前的那个，而不是让后面
            // 冒出来的把落点改掉。
            if out.contains_key(id) {
                continue;
            }
            if let Some(start) = self.first_text_offset(node) {
                out.insert(id.to_string(), start);
            }
        }
        out
    }

    /// `root` 那棵子树里第一个进了可见文本坐标表的节点，它的区间起点。
    fn first_text_offset(&self, root: NodeId) -> Option<usize> {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if let Some(chars) = self.text_map().node_chars(node) {
                return Some(chars.start);
            }
            // 逆序压栈，弹出来的就是文档序。
            let children = &self.dom().get(node).children;
            stack.extend(children.iter().rev().copied());
        }
        None
    }

    /// 命中测试与选中矩形（M6 选中、M7 批注回显共用的三个薄包装）。
    ///
    /// 坐标与返回值都是**排版坐标**（行盒那一套），调用方负责换算
    /// （连续滚动减 `scroll_y`，分页/双页减片的 `origin_y` 与栏位 `x`）。
    /// 度量现建一个 `GpuiMeasure` —— 与排版用同一个构造参数，字符前进量同源。
    /// 排版还没做过（窗口第一帧之前）时 `lines` 是空的，命中给 `None`。
    #[allow(dead_code)] // M6 选中接线的入口，先立契约（见 plans/M6-selection.md）
    pub fn hit_test(&self, window: &Window, cx: &App, x: f32, y: f32) -> Option<Hit> {
        folio_core::content::hit::hit_test(
            self.layout(),
            self.dom(),
            self.text_map(),
            &self.measure(window, cx),
            x,
            y,
        )
    }

    /// 一段字符区间在排版里的矩形（排版坐标，每行一条），**不按可见带窄化**。
    ///
    /// 只走与这段区间**可能**相交的那几行：行索引二分出左闭右开的行号段，交点筛选留在
    /// `rects_for_range_in` 里那道「这一行与选区交集为空就跳过」，所以窄化只可能多扫、
    /// 不可能漏矩形。索引说这一章的行不按时序单调（`lines_for` 给 `None`）时退回整章扫，
    /// 与接线前逐字同一条口径。
    ///
    /// 每帧的批注回显与选区要的是带内那一条：[`Document::selection_rects_in_band`]。
    pub fn selection_rects(
        &self,
        window: &Window,
        cx: &App,
        range: std::ops::Range<usize>,
    ) -> Vec<folio_core::content::hit::SelectionRect> {
        let lines = narrowed_lines(
            self.char_lines().lines_for(range.clone()),
            0..self.layout().lines.len(),
        );
        folio_core::content::hit::rects_for_range_in(
            self.layout(),
            self.dom(),
            self.text_map(),
            &self.measure(window, cx),
            range,
            lines,
        )
    }

    /// 一段字符区间在**可见带内**的矩形（排版坐标）。绘制侧的批注下划线与选区走这一条。
    ///
    /// 两条窄化叠着用：字符索引给「这段字落在哪几行」，`Layout::lines_in_band` 给
    /// 「屏上是哪几行」，取它们的交。**为什么可见带也少不了**：一章的批注绝大多数不在
    /// 这一屏上，而那些条从前同样把整章的行走完、再由调用方按 y 丢掉 —— 「每条批注 ×
    /// 每帧」各一次（`plans/阅读管线.md` §十一 第 9 条）。
    ///
    /// 与「算完再按 y 滤」必须逐条等价：一条矩形就是它那一行的行盒 `y` 与 `height`
    /// （`rects_for_range_in` 末尾那条 `out.push`），而 `lines_in_band` 用的正是同一对
    /// 不等式。调用方那道 y 过滤**照旧留着**：它现在是重复的一道，不是唯一的一道。
    pub fn selection_rects_in_band(
        &self,
        window: &Window,
        cx: &App,
        range: std::ops::Range<usize>,
        band: std::ops::Range<f32>,
    ) -> Vec<folio_core::content::hit::SelectionRect> {
        let lines = narrowed_lines(
            self.char_lines().lines_for(range.clone()),
            self.visible_lines(band),
        );
        folio_core::content::hit::rects_for_range_in(
            self.layout(),
            self.dom(),
            self.text_map(),
            &self.measure(window, cx),
            range,
            lines,
        )
    }

    /// 长按落点吸到一个词上（纯坐标表运算，不碰度量）。
    #[allow(dead_code)] // M6 长按选词的入口（见 plans/M6-selection.md）
    pub fn snap_word(&self, offset: usize) -> std::ops::Range<usize> {
        folio_core::content::hit::snap_word(self.text_map(), offset)
    }

    /// 那个**会话级复用**的度量器（见 [`Document::measure`] 字段的说明）。
    ///
    /// 与排版作业里那个 `GpuiMeasure` 用同一组构造参数 —— 这一点是硬要求：
    /// 「按 A 算宽、按 B 画字」是同源性防线的反面，命中与绘制必须走同一个度量口径。
    #[allow(dead_code)] // 上面三个 M6 入口共用
    fn measure(&self, window: &Window, cx: &App) -> GpuiMeasure {
        self.measure
            .get_or_init(|| {
                GpuiMeasure::new(
                    cx.text_system().clone(),
                    window.text_system().clone(),
                    self.prefs.font_family.clone(),
                    self.prefs.mono_font_family.clone(),
                    self.faces.clone(),
                )
            })
            .clone()
    }

    /// 章内第 `y` 个像素那一行的行首字符偏移。滚动位置换成进度用这个。
    ///
    /// `y` 超过内容末尾时给末尾那一行的偏移，不是总字数 —— 读者停在最后一屏，
    /// 下次该回到最后一屏，而不是回到「章尾但看不见任何字」的地方。
    pub fn offset_at_y(&self, y: f32) -> usize {
        let line = folio_core::content::anchor::line_at_y(self.layout(), y);
        folio_core::content::anchor::line_offset(self.layout(), self.dom(), self.text_map(), line)
            .unwrap_or(0)
    }

    /// 第 `line` 行的行首字符偏移。越界给 0（没有行就没有坐标）。
    ///
    /// 桌面端用它做「列表滚过多少像素 → 第几行 → 第几个字」的最后一跳：
    /// 行号是从虚拟列表自己的坐标空间反解出来的，不能再拿布局绝对 `y` 去换算。
    pub fn coordinate_of_line(&self, line: usize) -> usize {
        folio_core::content::anchor::line_offset(self.layout(), self.dom(), self.text_map(), line)
            .unwrap_or(0)
    }

    /// 可见文本偏移落在第几行（行盒下标）。
    ///
    /// 走索引二分；索引不可用（那一章的行不按时序单调）时 `line_at_offset_indexed`
    /// 自己退回整表扫，两条口径是同一个函数保证的。
    pub fn line_at_offset(&self, offset: usize) -> usize {
        folio_core::content::anchor::line_at_offset_indexed(
            self.layout(),
            self.dom(),
            self.text_map(),
            offset,
            Some(&self.char_lines()),
        )
    }

    /// 要把 `offset` 那一行摆上屏，滚动位置该是多少（行顶的绝对 y）。
    ///
    /// 排版还没做过（`layout` 是空的）时给 0，等于回到章首 —— 这是唯一不骗人的兜底。
    pub fn y_at_offset(&self, offset: usize) -> f32 {
        let layout = self.layout();
        let line = folio_core::content::anchor::line_at_offset_indexed(
            layout,
            self.dom(),
            self.text_map(),
            offset,
            Some(&self.char_lines()),
        );
        layout
            .lines
            .get(line)
            .map(|box_| box_.y)
            .unwrap_or_default()
    }
}

/// `<a>` 是不是一条脚注引用：自己标了类型，或它指向的元素是**本章**的脚注正文。
///
/// 后一条是不可省的兜底：Z-Library / 多看这类工具转出来的书常常不写 `epub:type`，
/// 只把注文塞进一个 `id` 命名的 `<aside>`/`<div>`，标记 `<a href="#id">` 什么都不带。
fn footnote_anchor_is_ref(dom: &Dom, anchor: NodeId, doc: &Document, href: &str) -> bool {
    attr_has_word(dom, anchor, "epub:type", "noteref")
        || attr_has_word(dom, anchor, "role", "doc-noteref")
        || doc.footnote_body_text_for_href(href).is_some()
}

/// 某个元素是不是脚注正文：EPUB3 的 `epub:type="footnote"` / `role="doc-footnote"`，
/// 或它自己/最近一个祖先块的 `class` 像注文容器（`duokan-footnote-content`、
/// `footnote`、`note` —— 《罗生门》整章末尾那些 `<p class="note">` 就是）。
///
/// 有意**不**把裸的 `epub-footnote` 这种标记图类名算进来：那是标记自己，不是正文。
/// 也**不**按 id 命名猜（`m1` / `fn1` 这种没个准）。
fn is_footnote_body(dom: &Dom, node: NodeId) -> bool {
    for id in dom.ancestors_including_self(node) {
        if attr_has_word(dom, id, "epub:type", "footnote")
            || attr_has_word(dom, id, "role", "doc-footnote")
        {
            return true;
        }
        if dom
            .classes(id)
            .any(|c| c == "note" || c.contains("footnote"))
        {
            return true;
        }
    }
    false
}

/// 标记图上的 `alt` / `title` 是不是**真注文**。
///
/// 这类书（哈利波特附录那本就是这样）把脚注正文做成图片，标记图的 `alt` 只写
/// `"alt"` 或 `"注"`；把它当注文显示，读者看到的是一句「alt」。没有真文字就返回
/// `None`，让点击落回普通链接跳转，至少能滚到那张注文图本身。
fn useful_marker_text(value: &str) -> bool {
    let text = value.trim();
    if text.is_empty() {
        return false;
    }
    !matches!(
        text.to_ascii_lowercase().as_str(),
        "alt" | "image" | "img" | "note" | "footnote" | "注" | "图片" | "脚注"
    )
}

/// 属性值按空白分词后是否含 `want`（`epub:type` 可以写多个，空格分隔）。
fn attr_has_word(dom: &Dom, node: NodeId, name: &str, want: &str) -> bool {
    dom.attr(node, name)
        .is_some_and(|v| v.split_ascii_whitespace().any(|w| w == want))
}

#[cfg(test)]
mod tests;
