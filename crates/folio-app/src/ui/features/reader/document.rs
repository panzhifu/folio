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

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use folio_core::Reader;
use folio_core::content::anchor::TextMap;
use folio_core::content::format::css::computed::{ComputedStyle, StyledDom, TextAlign, cascade};
use folio_core::content::format::css::{Origin, RuleSet};
use folio_core::content::dom::{Dom, NodeId};
use folio_core::content::hit::Hit;
use folio_core::layout::fragment::{Fragment, FragmentOptions, column_width, fragments};
use folio_core::layout::{
    BlockCache, Layout, LayoutKey, LayoutOptions, LineBox, content_hash, layout_with_blocks,
    quantize_px,
};
use folio_core::infra::task::{TaskError, TaskEvent, TaskHandle, TaskManager};
use gpui_kit::{App, TextSystem, Window, WindowTextSystem};

use super::measure::GpuiMeasure;
use crate::ui::features::reader::book_fonts::BookFonts;
use crate::ui::shared::images::ChapterImages;

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

/// 一章的**解析产物**：与读者偏好无关，所以改字号、拖窗口都不必重新解析。
///
/// 缓存挂在产物而不是挂在「取回来的章文本」上，理由见 `plans/阅读管线.md` §五：
/// 文本本来就存在库里，重复付的钱在解析与可见文本坐标表上。`Arc` 是因为它同时被界面
/// （命中、锚点换算）与在飞的排版作业拿着，而这两份必须是同一章。
struct ChapterArtifacts {
    /// 建它时那段源文本的哈希（与 [`LayoutKey::chapter_hash`] 同源，见 `layout::content_hash`）。
    ///
    /// 复用前对一遍：对不上就是换章了，`NodeId` 指的是上一章，绝不能拿去排版。
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
    source: String,
    /// 作者层样式表的**原文**（书自己那份 CSS）。键里要有它的哈希，而 `RuleSet` 已经是
    /// 解析后的结构 —— 哈希原文与哈希结构是两件事，「作者改了一个字」是原文那件事。
    /// 章里挂的 `<style>` 不在这里：它长在 `source` 里，`chapter_hash` 已经盖住它。
    author_css: String,
    /// 本章的解析产物（DOM + 可见文本坐标），按章内容跨代次复用。
    ///
    /// 为什么留着它有两层：① 锚点换算要用（`LineRun::orig` 是**节点内**的字节区间，
    /// 没有节点文本就换不成可见文本坐标）；② 解析只做一次 —— 排版可以每帧重做
    /// （宽度、字号变了），但 HTML 解析不该跟着重做，它比层叠贵。
    artifacts: Arc<ChapterArtifacts>,
    rules: RuleSet,
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
    /// 层叠结果。分片要读块级的 `break-*` 与 orphans/widows，而这些只在层叠之后存在，
    /// 所以它跟排版结果同批算、同批存，不在分片时重来一遍。
    styles: Option<StyledDom>,
    /// 上次分片时的 `(排版键, 一片高)`。排版键与切片之间唯一的差别就是那片高：
    /// 栏宽与栏数都已经在键里了，字号也在（换字号会改行数，行变了切点就变了）。
    fragmented: Option<(LayoutKey, u32)>,
    /// 本章切出来的片。`Rc` 是因为渲染每帧都要拿一份而不必搬动整张表。
    slices: Rc<Vec<Fragment>>,
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
    source: String,
    /// 界面手上那份解析产物。键的 `chapter_hash` 对得上就直接用，对不上才重新解析 ——
    /// 「改一次字号不许重解析」那条计数断言（`folio_core::infra::metrics::Metric::DomParses`）
    /// 钉的就是这一格。
    artifacts: Arc<ChapterArtifacts>,
    /// 书自己的样式（作者层）。章里的 `<style>` 不在这里 —— 它随 DOM 解析就地收。
    rules: RuleSet,
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

/// 作业体：解析（按需）→ 层叠 → 排版。测试里被同步调用，线上在后台线程里跑 —— 同一份代码。
fn typeset_job(mut input: TypesetInput) -> Result<Typeset, TaskError> {
    // 第 3 级（解析）与可见文本坐标：按章内容复用。`chapter_hash` 对得上就说明手上这份
    // DOM 正是从这段文本来的，里面每个 `NodeId` 都还作数 —— 换字号、拖窗口都不必重解析。
    let artifacts = if input.artifacts.key == input.key.chapter_hash {
        input.artifacts
    } else {
        Arc::new(ChapterArtifacts::build(&input.source))
    };
    let mut rules = input.rules;
    rules.extend(Document::chapter_style_rules(&artifacts.dom));
    // 读者层的段间距。放在这里而不是 `Document::new`：那一份 `rules` 跟着作者 CSS 存，
    // 偏好一改就会让「章的样式」看起来也变了。
    rules.extend(Document::reader_rules(&input.prefs));
    let styles = cascade(&artifacts.dom, &rules, &Document::root_base(&input.prefs));
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

impl Document {
    pub fn new(
        source: String,
        css: &str,
        prefs: Reader,
        images: Arc<ChapterImages>,
        faces: Arc<BookFonts>,
    ) -> Self {
        let mut rules = RuleSet::default();
        // 书自己的 CSS 是作者层；读者偏好通过根样式进来（`root_base`），
        // 带 `!important` 的读者样式表留给 M5 的 reader.css。
        rules.extend(RuleSet::parse(css, Origin::Author));
        // 建好才交出去：`text_map()` 与锚点换算在第一次排版回来之前也要能用。
        let artifacts = Arc::new(ChapterArtifacts::build(&source));
        Self {
            source,
            author_css: css.to_string(),
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
            styles: None,
            fragmented: None,
            slices: Rc::new(Vec::new()),
        }
    }

    /// 这本书的内嵌字体。绘制路径与度量共用它，两边不可能拿不同的别名表。
    pub fn fonts(&self) -> &BookFonts {
        &self.faces
    }

    /// 章节自己的 `<style>` 块 → 作者层规则。
    ///
    /// 样式表**文件**按位置清单回容器取（见 `folio_core::content::document::Document::stylesheet`），
    /// 而**写在章里的** `<style>` 随正文一起到达 —— 解析 DOM 时就地收进来，作者把
    /// 样式写在哪一份里，层叠结果都不该有差别（EasyPub 之类工具的封面页就是这么写的）。
    fn chapter_style_rules(dom: &Dom) -> RuleSet {
        let mut css = String::new();
        for (id, _) in dom.elements() {
            if dom.tag_name(id) != Some("style") {
                continue;
            }
            for &child in &dom.get(id).children {
                css.push_str(&dom.get(child).text);
                css.push('\n');
            }
        }
        let mut rules = RuleSet::default();
        if !css.trim().is_empty() {
            rules.extend(RuleSet::parse(&css, Origin::Author));
        }
        rules
    }

    /// 读者偏好里**只能用 CSS 表达**的那几项 → 读者层规则。
    ///
    /// 段间距走这一层而不是根样式：`margin` 不是继承属性，挂在根上穿不到 `<p>`。
    /// 读者层在层叠里位于作者层之下（`Origin::Reader` < `Origin::Author`，与浏览器一致），
    /// 所以书自己写了 `p { margin-bottom: 2em }` 时仍按书的；没有作者声明时，
    /// `css/ua.rs` 给 `<p>` 的那一对 1em 才是兜底。
    ///
    /// 它与章文本无关，所以**不进 `ChapterArtifacts`**（那一份按 `chapter_hash` 复用，
    /// 掺进偏好就等于把偏好烘进了缓存），每次作业现拼一条。
    fn reader_rules(prefs: &Reader) -> RuleSet {
        let gap = prefs.paragraph_gap_em;
        // 固定三位小数：滑条的步长到不了第四位，而写全精度会让同一段距在不同机器上
        // 拼出两条字面不同的串（键里的作者样式哈希不管这一格，但日志与调试要对得上）。
        RuleSet::parse(
            &format!("p {{ margin-top: {gap:.3}em; margin-bottom: {gap:.3}em }}"),
            Origin::Reader,
        )
    }

    /// 换读者的偏好。**这里不推进任何代数**：排版键是从输入现算的，下一帧
    /// [`Self::ensure_viewport`] 一算就发现 `prefs_hash` 那格变了，作业自然派出去。
    /// 只有切片必须当场作废 —— 字号改了行数就改，切点跟着改。
    pub fn set_prefs(&mut self, prefs: Reader) {
        self.prefs = prefs;
        // 「想要哪把键」只有 `ensure_viewport` 知道（键要含栏宽与栏数），所以这里只能把它
        // 清成「还不知道」。不清的话 `applied` 仍等于旧的 `desired`，`is_current()` 会答
        // 「跟上了」，而恢复进度与批注回显等的就是这一句。
        self.desired = None;
        self.drop_fragments();
    }

    /// 换章。源文本没变就保留上一次的排版结果，来回翻同一章不必重算。
    ///
    /// 解析与排版都搬进了后台作业（见 [`Self::request_typeset`]），这里只作废：
    /// 旧章的产物与行盒留着，新结果回来之前屏幕上还是上一章（一帧的功夫）——
    /// 锚点换算在这段时间里指着旧章，不会指到半个新章上。
    pub fn set_chapter(&mut self, source: String) {
        if source == self.source {
            return;
        }
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
    pub fn layout_key(&self, width: f32, columns: usize) -> LayoutKey {
        LayoutKey::compose(
            &self.source,
            &self.author_css,
            &self.prefs,
            width,
            columns,
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

    /// 作废切片缓存。排版一作废就必须跟着作废它 —— 切点读的是行盒。
    fn drop_fragments(&mut self) {
        self.fragmented = None;
        self.slices = Rc::new(Vec::new());
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

    /// 某个 DOM 节点层叠完的计算样式。排版没做过（`styles` 还没建）时是 `None`。
    ///
    /// 绘制层用它取块级装饰（背景色、边框）—— 那些是「盒子长什么样」的信息，
    /// 只有层叠完才知道。
    pub fn style_of(&self, node: NodeId) -> Option<&ComputedStyle> {
        self.styles.as_ref()?.get(node)
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
        self.layout.height
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
                TextAlign::Left
            },
            text_indent: prefs.font_size * prefs.indent_em.max(0.0),
            ..ComputedStyle::default()
        }
    }

    /// 请求一次排版（不阻塞）：键没变就什么都不做，变了就把层叠 + 排版丢给后台队列
    /// （解析按需 —— 产物对得上就不重解析，见 [`typeset_job`]），渲染帧照常用上一份结果画。
    /// 测试里同步跑同一段作业体，结果当帧可断言 —— 覆盖的代码与线上同一条。
    fn request_typeset(
        &mut self,
        key: LayoutKey,
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
            source: self.source.clone(),
            artifacts: self.artifacts.clone(),
            rules: self.rules.clone(),
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
                    chapter_hash = key.chapter_hash,
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
                chapter_hash = key.chapter_hash,
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
        let columns = viewport.columns.max(1);
        let total = Self::content_width(window, &self.prefs, viewport.reserve);
        let width = column_width(total, columns, viewport.gap).max(1.0);
        // 键在这里算，算是「这一帧想要什么」的唯一表述：排版、切片、绘制三级都从它出发。
        let key = self.layout_key(width, columns);
        let width = key.width as f32;
        // 片高同样量化一档，且**切片用的就是量化后的那个数**：排版按 A 宽折行、切片按 B 高切、
        // 绘制按 C 摆，三方都以为自己对，其实是三方各对（`quantize_px` 那条纪律的另一半）。
        let slice_height = quantize_px(viewport.height.max(0.0));
        let height = slice_height as f32;
        self.desired = Some(key);
        // 先收上一帧的作业成果，再看要不要派新活 —— 顺序反了会把刚回来的结果当旧的丢掉。
        self.poll_typeset();
        self.request_typeset(key, cx.text_system().clone(), window.text_system().clone());
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
    fn dom(&self) -> &Dom {
        &self.artifacts.dom
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

    /// 一段字符区间在排版里的矩形（排版坐标，每行一条）。
    #[allow(dead_code)] // M6 选中接线的入口（见 plans/M6-selection.md）
    pub fn selection_rects(
        &self,
        window: &Window,
        cx: &App,
        range: std::ops::Range<usize>,
    ) -> Vec<folio_core::content::hit::SelectionRect> {
        folio_core::content::hit::rects_for_range(
            self.layout(),
            self.dom(),
            self.text_map(),
            &self.measure(window, cx),
            range,
        )
    }

    /// 长按落点吸到一个词上（纯坐标表运算，不碰度量）。
    #[allow(dead_code)] // M6 长按选词的入口（见 plans/M6-selection.md）
    pub fn snap_word(&self, offset: usize) -> std::ops::Range<usize> {
        folio_core::content::hit::snap_word(self.text_map(), offset)
    }

    /// 长按所在段落的字符区间（「全选本段」）。
    pub fn paragraph_range(
        &self,
        window: &Window,
        cx: &App,
        offset: usize,
    ) -> Option<std::ops::Range<usize>> {
        let _ = (window, cx);
        folio_core::content::hit::paragraph_range(self.layout(), self.dom(), self.text_map(), offset)
    }

    #[allow(dead_code)] // 上面三个 M6 入口共用
    fn measure(&self, window: &Window, cx: &App) -> GpuiMeasure {
        GpuiMeasure::new(
            cx.text_system().clone(),
            window.text_system().clone(),
            self.prefs.font_family.clone(),
            self.prefs.mono_font_family.clone(),
            self.faces.clone(),
        )
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
    pub fn line_at_offset(&self, offset: usize) -> usize {
        folio_core::content::anchor::line_at_offset(self.layout(), self.dom(), self.text_map(), offset)
    }

    /// 要把 `offset` 那一行摆上屏，滚动位置该是多少（行顶的绝对 y）。
    ///
    /// 排版还没做过（`layout` 是空的）时给 0，等于回到章首 —— 这是唯一不骗人的兜底。
    pub fn y_at_offset(&self, offset: usize) -> f32 {
        let layout = self.layout();
        let line = folio_core::content::anchor::line_at_offset(layout, self.dom(), self.text_map(), offset);
        layout
            .lines
            .get(line)
            .map(|box_| box_.y)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests;
