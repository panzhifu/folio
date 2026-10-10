//! 「一章的 XHTML → `folio_core::layout::Layout`」这一段，桌面与移动共用。
//!
//! 分成两件事：**怎么排、切成几片**在这里，**怎么摆上屏**在视图层
//! （`ui/features/reader`，两端一份实现）。这样同一本书在两端得到的是同一份
//! `Layout` 与同一套分片，屏幕上看到的行只可能因为视口尺寸不同而不同，不会因为
//! 「两端各实现了一遍层叠与断行」而不同。
//!
//! 入口的 `Chapter::source` **一定是引擎就绪的 XHTML**（契约见 `format::Chapter`）。
//! 纯文本的包装已经在 provider 里做完（`folio_core::format::xhtml`），所以这里
//! 没有「这个格式要不要先包一下」的分支 —— `Format` 不参与排版链。

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use folio_core::Reader;
use folio_core::anchor::TextMap;
use folio_core::css::computed::{ComputedStyle, StyledDom, TextAlign, cascade};
use folio_core::css::{Origin, RuleSet};
use folio_core::dom::{Dom, NodeId};
use folio_core::hit::Hit;
use folio_core::layout::fragment::{Fragment, FragmentOptions, column_width, fragments};
use folio_core::layout::{Layout, LayoutOptions, LineBox, layout as typeset};
use gpui_kit::{App, Window};

use super::measure::GpuiMeasure;
use crate::ui::features::reader::book_fonts::BookFonts;
use crate::ui::shared::images::ChapterImages;

/// 打开一个文档，返回统一接口。
///
/// 走 `folio_core::document::open`：`Content` 的两个家族都从那里出去，界面这一层只按
/// `kind()` 分流，不看扩展名，也不再有第二个「按格式查 provider」的入口。
///
/// 返回 `Arc` 而不是 `Box`：`Book` / `FixedBook` 与绘制那条链（图片字节来源）要共用
/// 同一份文档，而字节按需取用的是它手里那个 provider。
pub fn open(path: &Path) -> folio_core::Result<Arc<dyn folio_core::document::Document>> {
    Ok(Arc::from(folio_core::document::open(path)?))
}

/// 章节源文本 → 交给排版层的 XHTML 片段。
/// `<body>` 不在（纯片段或畸形 XHTML）时退回文档根：排版层只认「从这里往下找内容」。
pub fn body_of(dom: &Dom) -> NodeId {
    dom.elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document)
}

/// 一章的排版状态：源文本 + 书自己的 CSS + 读者偏好 + 本章图片索引，
/// 缓存上一次的 `Layout` 与由它切出的片。
///
/// 重排只在**排版输入真的变了**之后发生（版心宽度或任何一项读者偏好），所以逐帧调用
/// [`Document::ensure_layout`] 是安全的：滚动、点击、别的窗口改标题都不触发重算。
/// 分片同理，多出来的两个键是「一片高」与「一页几栏」（见 [`Document::ensure_viewport`]）——
/// 连续滚动 / 分页 / 双页对开三种模式在核心层是同一套代码，差别只有这两个数。
pub struct Document {
    source: String,
    /// 本章解析好的 DOM，与 `source` 同生命周期。
    ///
    /// 为什么留着它有两层：① 锚点换算要用（`LineRun::orig` 是**节点内**的字节区间，
    /// 没有节点文本就换不成可见文本坐标）；② 解析只做一次 —— 排版可以每帧重做
    /// （宽度、字号变了），但 HTML 解析不该跟着重做，它比层叠贵。
    dom: Dom,
    /// 本章的可见文本坐标表。锚点与进度都存在这套坐标里，它不随行盒变化。
    map: TextMap,
    rules: RuleSet,
    prefs: Reader,
    /// 本章的图片索引：排版期查固有尺寸，绘制期取可渲染的 `Image`。
    images: Arc<ChapterImages>,
    /// 整本书的内嵌字体（别名表 + 已经注册进设备的家族名）。换章不换这一份，
    /// 所以由 `Book` 建好灌进来，而不是每章自己注册一遍。
    faces: Arc<BookFonts>,
    /// 上次排版时的 `(内容宽度, 读者偏好)`。
    ///
    /// 记的是**整份偏好**而不是字号与行距：首行缩进、段距、是否两端对齐同样进排版，
    /// 只按字号与行距判断就会让「改了缩进但不重排」这种毛病留在缓存里。
    laid_out: Option<(f32, Reader)>,
    layout: Layout,
    /// 层叠结果。分片要读块级的 `break-*` 与 orphans/widows，而这些只在层叠之后存在，
    /// 所以它跟排版结果同批算、同批存，不在分片时重来一遍。
    styles: Option<StyledDom>,
    /// 上次分片时的 `(栏宽, 读者偏好, 一片高, 一页几栏)`。宽与偏好也是它的键：
    /// 换字号会改变行数，行变了切点就变了。
    fragmented: Option<(f32, Reader, f32, usize)>,
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
        let (dom, map) = Self::parse_chapter(&source);
        Self {
            source,
            dom,
            map,
            rules,
            prefs,
            images,
            faces,
            laid_out: None,
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

    /// 源文本 → DOM + 可见文本坐标。两者必须一起换，否则坐标指的就是上一章的文本。
    fn parse_chapter(source: &str) -> (Dom, TextMap) {
        let dom = Dom::parse(source.as_bytes());
        let map = TextMap::build(&dom, body_of(&dom));
        (dom, map)
    }

    pub fn set_prefs(&mut self, prefs: Reader) {
        self.prefs = prefs;
        // 偏好同时进排版键与分片键，所以两边一起作废：留着旧切片的话，
        // 「换了字号但一页还是那么多行」这种毛病在屏幕上看不出来，只会在进度条上飘。
        self.drop_fragments();
    }

    /// 换章。源文本没变就保留上一次的排版结果，来回翻同一章不必重算。
    pub fn set_chapter(&mut self, source: String) {
        if source != self.source {
            let (dom, map) = Self::parse_chapter(&source);
            self.source = source;
            self.dom = dom;
            self.map = map;
            self.laid_out = None;
            self.drop_fragments();
        }
    }

    /// 换章时图片索引也要跟着换：`<img src>` 的相对路径基准目录随章变化。
    pub fn set_images(&mut self, images: Arc<ChapterImages>) {
        self.images = images;
        self.laid_out = None;
        self.drop_fragments();
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

    /// 排版一章。度量要用**窗口级**文本系统（`layout_line` 只挂在它上面），
    /// 所以这一步留在渲染帧里做；挪进 `background_spawn` 属 M4。
    #[allow(dead_code)] // 测试与 M4 的后台化接线用
    pub fn ensure_layout(&mut self, window: &mut Window, cx: &mut App, pane_reserve: f32) {
        let width = Self::content_width(window, &self.prefs, pane_reserve);
        self.typeset_to(window, cx, width);
    }

    /// 按给定宽度排版一章。宽度没变、偏好没变就直接复用上一次的结果。
    fn typeset_to(&mut self, window: &mut Window, cx: &mut App, width: f32) {
        // 逐帧比引用而不是先 `prefs.clone()` 再比：`Reader` 里有两个 `String`，
        // 而这一条几乎每帧都要问一次（滚动、点选、改标题都重画）。只有真重排时才克隆。
        if let Some((laid_width, laid_prefs)) = &self.laid_out
            && *laid_width == width
            && *laid_prefs == self.prefs
        {
            return;
        }
        let measure = GpuiMeasure::new(
            cx.text_system().clone(),
            window.text_system().clone(),
            self.prefs.font_family.clone(),
            self.prefs.mono_font_family.clone(),
            self.faces.clone(),
        );
        let styles = cascade(&self.dom, &self.rules, &Self::root_base(&self.prefs));
        // 解析不做在这里（`dom` 是字段）；换章才重解析，见 `parse_chapter`。
        let body = body_of(&self.dom);
        self.layout = typeset(
            &self.dom,
            &styles,
            body,
            LayoutOptions {
                width,
                // 完整排版：界面要行盒来摆字，不能用只算高的轻量档。
                light: false,
            },
            &measure,
            self.images.as_ref(),
        );
        self.styles = Some(styles);
        self.laid_out = Some((width, self.prefs.clone()));
        // 行盒换了，切点也就换了 —— 切片必须跟着作废，否则分页翻到的是上一页的行。
        self.drop_fragments();
    }

    /// 排版 + 分片，返回**栏宽**（绘制要按它摆每栏的左边界）。
    ///
    /// 一次把整章切完而不是逐页现算：`Fragmentainer` 的游标是为「翻到第 500 页」准备的，
    /// 而这里一片一章最多几千行，一次切完仍然只在 `(栏宽, 偏好, 片高, 栏数)` 变了时发生，
    /// 换来的是进度条随手就有总页数。
    pub fn ensure_viewport(
        &mut self,
        window: &mut Window,
        cx: &mut App,
        viewport: Viewport,
    ) -> f32 {
        let columns = viewport.columns.max(1);
        let total = Self::content_width(window, &self.prefs, viewport.reserve);
        let width = column_width(total, columns, viewport.gap).max(1.0);
        self.typeset_to(window, cx, width);
        let height = viewport.height.max(0.0);
        // 同上：先比引用，缓存命中就不克隆 `prefs`。
        let cached = self.fragmented.as_ref().is_some_and(|(w, p, h, c)| {
            *w == width && *p == self.prefs && *h == height && *c == columns
        });
        if cached {
            return width;
        }
        let options = FragmentOptions { height, columns };
        let slices = match &self.styles {
            Some(styles) => fragments(&self.layout, styles, options),
            // 没排过版就没有行盒可切（第一帧之前）。
            None => Vec::new(),
        };
        self.fragmented = Some((width, self.prefs.clone(), height, columns));
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
    // 所以这三个方法放在共用层，实现全在 `folio_core::anchor`。

    /// 本章的可见文本坐标表（批注与书签在上面建锚点）。
    pub fn text_map(&self) -> &TextMap {
        &self.map
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
        let mut out: HashMap<String, usize> = HashMap::new();
        for (node, _) in self.dom.elements() {
            let Some(id) = self.dom.id_attr(node) else {
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
            if let Some(chars) = self.map.node_chars(node) {
                return Some(chars.start);
            }
            // 逆序压栈，弹出来的就是文档序。
            let children = &self.dom.get(node).children;
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
        folio_core::hit::hit_test(
            self.layout(),
            &self.dom,
            &self.map,
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
    ) -> Vec<folio_core::hit::SelectionRect> {
        folio_core::hit::rects_for_range(
            self.layout(),
            &self.dom,
            &self.map,
            &self.measure(window, cx),
            range,
        )
    }

    /// 长按落点吸到一个词上（纯坐标表运算，不碰度量）。
    #[allow(dead_code)] // M6 长按选词的入口（见 plans/M6-selection.md）
    pub fn snap_word(&self, offset: usize) -> std::ops::Range<usize> {
        folio_core::hit::snap_word(&self.map, offset)
    }

    /// 长按所在段落的字符区间（「全选本段」）。
    pub fn paragraph_range(
        &self,
        window: &Window,
        cx: &App,
        offset: usize,
    ) -> Option<std::ops::Range<usize>> {
        let _ = (window, cx);
        folio_core::hit::paragraph_range(self.layout(), &self.dom, &self.map, offset)
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
        let line = folio_core::anchor::line_at_y(self.layout(), y);
        folio_core::anchor::line_offset(self.layout(), &self.dom, &self.map, line).unwrap_or(0)
    }

    /// 第 `line` 行的行首字符偏移。越界给 0（没有行就没有坐标）。
    ///
    /// 桌面端用它做「列表滚过多少像素 → 第几行 → 第几个字」的最后一跳：
    /// 行号是从虚拟列表自己的坐标空间反解出来的，不能再拿布局绝对 `y` 去换算。
    pub fn coordinate_of_line(&self, line: usize) -> usize {
        folio_core::anchor::line_offset(self.layout(), &self.dom, &self.map, line).unwrap_or(0)
    }

    /// 可见文本偏移落在第几行（行盒下标）。
    pub fn line_at_offset(&self, offset: usize) -> usize {
        folio_core::anchor::line_at_offset(self.layout(), &self.dom, &self.map, offset)
    }

    /// 要把 `offset` 那一行摆上屏，滚动位置该是多少（行顶的绝对 y）。
    ///
    /// 排版还没做过（`layout` 是空的）时给 0，等于回到章首 —— 这是唯一不骗人的兜底。
    pub fn y_at_offset(&self, offset: usize) -> f32 {
        let layout = self.layout();
        let line = folio_core::anchor::line_at_offset(layout, &self.dom, &self.map, offset);
        layout
            .lines
            .get(line)
            .map(|box_| box_.y)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    //! 本章的排版状态（本文件）与 `measure.rs` 在宿主上跑的用例。
    //!
    //! 为什么这些算移动端的测试：移动 UI 本身只在 `cfg(mobile)` 下编译，宿主的
    //! `cargo test` 碰不到那一侧的视图。但「同一本书在两端排出一样的行」这件事
    //! 完全落在这一层 —— 源文本包装、层叠、版心宽度、断行与缓存都在这里，
    //! 移动端只是把它们摆上屏的那层薄壳。所以这一层被测住，移动端的排版就不是没测过。
    //!
    //! 反过来讲也很清楚：这里**测不到**的是触摸滚动、章节按钮的点击、主题在 Android 上
    //! 解析到浅还是深 —— 那几件只有真开窗口、真上机器才算验证。

    use std::sync::Arc;

    use crate::ui::features::reader::book_fonts::BookFonts;
    use crate::ui::features::reader::document::{Document, body_of};
    use crate::ui::shared::images::ChapterImages;
    use folio_core::Reader;
    use folio_core::layout::LineFragment;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Context, Render, Styled, TestAppContext, div, px, size,
    };

    /// 一个只占满窗口的空视图。这些用例要的是**窗口**（度量与视口都挂在窗口上），
    /// 界面内容无关，所以给它最小的那一副。
    struct Blank;

    impl Render for Blank {
        fn render(
            &mut self,
            _window: &mut gpui_kit::Window,
            _cx: &mut Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            div().size_full()
        }
    }

    /// 开一个指定尺寸的窗口，返回它的句柄。版心宽度的用例靠它给视口。
    fn window(cx: &mut TestAppContext, width: f32, height: f32) -> AnyWindowHandle {
        cx.open_window(size(px(width), px(height)), |_, _| Blank)
            .into()
    }

    /// 一段足够长的中文，保证它会折行 —— 不折行的话「缩进改变了布局」这种断言无从成立。
    const LONG: &str = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。";

    fn prefs() -> Reader {
        Reader::default().clamped()
    }

    /// 把版心相关的两项设成「不参与这段」的值：`margin_x` 归零、上限关掉。
    /// 留白与上限本身另有用例，这里只关心缩进与折行。
    fn bare_prefs() -> Reader {
        let mut prefs = prefs();
        prefs.margin_x = 0.0;
        prefs.max_measure = 0.0;
        prefs
    }

    /// 在窗口里把一段 XHTML 排完，返回排好的 [`Document`]。
    ///
    /// 返回整个文档而不是行坐标，是因为要断言的东西有一半在文本里（哪一行是什么、
    /// 有没有留下一个画不出来的图片位）。
    fn layout_of(cx: &mut TestAppContext, xhtml: &str, prefs: &Reader, width: f32) -> Document {
        let handle = window(cx, width, 900.0);
        let mut document = Document::new(
            xhtml.to_string(),
            "",
            prefs.clone(),
            Arc::new(ChapterImages::empty()),
            // 这些用例钉的是排版与坐标，不是字体：空别名表 = 没有内嵌字体，
            // 与没有这个功能时逐字节同一条路径。
            Arc::new(BookFonts::empty()),
        );
        cx.update_window(handle, |_, win, cx| {
            document.ensure_layout(win, cx, 0.0);
        })
        .expect("窗口应当可排版");
        document
    }

    /// `max_measure == 0` 是「不设上限、随窗口铺满」的**哨兵值**，不是 0 像素的上限。
    /// 当成上限去夹就会塌成 1px 版心、一行只剩一个字 —— 全屏阅读时整个正文会变成一列竖字。
    #[gpui_kit::test]
    fn a_zero_max_measure_means_no_cap(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let handle = window(cx, 1000.0, 600.0);
        let mut uncapped = prefs();
        uncapped.margin_x = 48.0;
        uncapped.max_measure = 0.0;
        let mut capped = uncapped.clone();
        capped.max_measure = 400.0;

        let (free, limited) = cx
            .update_window(handle, |_, win, _| {
                (
                    Document::content_width(win, &uncapped, 0.0),
                    Document::content_width(win, &capped, 0.0),
                )
            })
            .expect("窗口应当可读");
        assert_eq!(free, 1000.0 - 48.0 * 2.0, "不设上限时版心随视口铺满");
        assert_eq!(limited, 400.0, "设了上限就按上限");
    }

    /// 侧栏那种「不属于阅读栏的宽度」必须从版心里扣掉，否则引擎按整窗宽折行、
    /// 屏幕按窄栏裁剪，居中的标题会被推到栏外。移动端没有侧栏，所以传 0 ——
    /// 同一个函数、同一个语义，这里也替那条路径把式子钉住。
    #[gpui_kit::test]
    fn the_pane_reserve_is_subtracted_from_the_measure(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let handle = window(cx, 1000.0, 600.0);
        let prefs = bare_prefs();
        let width = cx
            .update_window(handle, |_, win, _| {
                Document::content_width(win, &prefs, 260.0)
            })
            .expect("窗口应当可读");
        assert_eq!(width, 740.0, "侧栏占位要从视口宽里扣掉");
    }

    /// **缓存的键必须覆盖所有排版输入**。
    ///
    /// 这一条是回归门：缓存键曾经只记 `(宽度, 字号, 行距)`，于是读者在设置里改首行缩进或段距
    /// 时，`ensure_layout` 认定几何参数没变而直接复用旧布局 —— 界面上看不到任何报错，
    /// 只是那一项设置怎么改都不生效。现在键里带整份偏好，这条断言才会成立。
    #[gpui_kit::test]
    fn an_indent_change_reflows_the_document(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let handle = window(cx, 600.0, 900.0);
        let mut prefs = bare_prefs();
        prefs.indent_em = 0.0;
        let mut document = Document::new(
            format!("<p>{}</p>", LONG.repeat(6)),
            "",
            prefs.clone(),
            Arc::new(ChapterImages::empty()),
            // 这些用例钉的是排版与坐标，不是字体：空别名表 = 没有内嵌字体，
            // 与没有这个功能时逐字节同一条路径。
            Arc::new(BookFonts::empty()),
        );

        // 第一次排版：无缩进。
        cx.update_window(handle, |_, win, cx| document.ensure_layout(win, cx, 0.0))
            .expect("第一次排版应当成功");
        let flat = document.lines().first().map(|line| line.x).unwrap_or(0.0);

        // 同一个文档、同一个窗口，**只**改首行缩进：宽度、字号、行距都没变。
        // 缓存键只记那三项时这里会直接复用上面的布局，第一行的左缘一动不动。
        prefs.indent_em = 2.0;
        document.set_prefs(prefs);
        cx.update_window(handle, |_, win, cx| document.ensure_layout(win, cx, 0.0))
            .expect("第二次排版应当成功");
        let indented = document.lines().first().map(|line| line.x).unwrap_or(0.0);

        assert!(
            indented > flat,
            "首行缩进改了必须重排：不缩进时第一行左缘 {flat}，缩进两字后 {indented}"
        );
    }

    /// **解不出资源的 `<img>` 整张跳过**，不塌成空盒、也不留一块画不出来的空白。
    ///
    /// 这条对移动端尤其要紧：那一侧的图片现在按引擎给的位真画
    /// （`view.rs` 用 gpui 的 `img` 元素），所以一个解不出来的 `src`
    /// 若还留着位子，读者看到的就是一块空洞，还会以为正文排错了。
    /// 跳过之后行里的文本必须依然完整。
    #[gpui_kit::test]
    fn an_unresolvable_image_is_skipped_not_left_as_a_gap(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let document = layout_of(
            cx,
            "<p>前<img src=\"missing.png\" width=\"120\" height=\"80\">后</p>",
            &bare_prefs(),
            600.0,
        );
        let line = document.lines().first().expect("至少要有一行");
        assert!(
            !line
                .fragments
                .iter()
                .any(|fragment| matches!(fragment, LineFragment::Image(_))),
            "解不出资源的 `<img>` 该整张跳过，不该留一个画不出来的位"
        );
        assert_eq!(
            document.line_texts().concat(),
            "前后",
            "跳过之后行里的文字仍然是完整的"
        );
    }

    /// 排版层要找的那个根：html5ever 会替片段补出 `<body>`，所以裸片段也有一个 body 可用；
    /// 取到的必须是那个元素而不是文档根，否则 `<body>` 自己的 padding 与样式不生效。
    #[test]
    fn body_of_picks_the_body_element() {
        for source in [
            "<html><body><p>有 body</p></body></html>",
            "<p>只有正文</p>",
            "",
        ] {
            let dom = folio_core::dom::Dom::parse(source.as_bytes());
            let body = body_of(&dom);
            assert_eq!(
                dom.tag_name(body),
                Some("body"),
                "{source:?} 该排到 `<body>` 下面，实际取到了别的节点"
            );
        }
    }

    /// 进度坐标的往返：行顶像素 → 「第几个字」→ 行号，在**同一份排版**里必须是同一行；
    /// 而换了版心宽与字号之后，它必须还落在同一段里。
    ///
    /// 这就是开发方案 M5 那句验收（改字号 + 缩窗口 → 锚点命中）在共用层的落地检查。
    /// 三段文本各带一个段号标记，所以「还在同一段」是可判定的，而不是靠感觉像。
    #[gpui_kit::test]
    fn the_progress_coordinate_survives_a_relayout(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let xhtml = (0..3)
            .map(|i| format!("<p>第{i}段{LONG}</p>"))
            .collect::<Vec<_>>()
            .join("");
        let base = layout_of(cx, &xhtml, &bare_prefs(), 600.0);
        let texts = base.line_texts();
        // 找第二段的第一行：它的文本一定含「第1段」，且行顶像素可反解成坐标。
        let target = (0..texts.len())
            .find(|&k| texts[k].starts_with("第1段"))
            .expect("三段都该排出来");
        let y = base.lines()[target].y;

        let offset = base.offset_at_y(y);
        assert!(offset > 0, "第二段不可能从第 0 个字开始");
        assert_eq!(
            base.line_at_offset(offset),
            target,
            "同一份排版里，像素 → 坐标 → 行号必须回到原行"
        );
        // 反向也成立：坐标 → 行顶像素，得到的就是那一行的行顶。
        assert_eq!(base.y_at_offset(offset), y);

        for (width, size) in [(300.0, 16.0), (900.0, 24.0), (420.0, 30.0)] {
            let mut prefs = bare_prefs();
            prefs.font_size = size;
            let moved = layout_of(cx, &xhtml, &prefs, width);
            let landed = moved.line_at_offset(offset);
            let landed_text = &moved.line_texts()[landed];
            assert!(
                landed_text.contains("第1段"),
                "宽 {width}、字号 {size}：坐标 {offset} 落到了「{landed_text}」，不在第二段里"
            );
        }
    }

    /// 排版还没做过时这套换算不能给出假坐标：一个全新的 `Document` 没有行盒，
    /// 任何偏移都只能映射到「章首」，否则恢复进度会把人送到看不见的地方。
    #[test]
    fn an_unlaid_out_document_maps_everything_to_the_chapter_start() {
        let document = Document::new(
            format!("<p>{LONG}</p>"),
            "",
            prefs(),
            Arc::new(ChapterImages::empty()),
            // 这些用例钉的是排版与坐标，不是字体：空别名表 = 没有内嵌字体，
            // 与没有这个功能时逐字节同一条路径。
            Arc::new(BookFonts::empty()),
        );
        assert_eq!(document.line_count(), 0, "没排过版就没有行盒");
        assert_eq!(document.offset_at_y(4000.0), 0);
        assert_eq!(document.y_at_offset(1234), 0.0);
        assert_eq!(document.line_at_offset(1234), 0);
        // 坐标表不依赖排版，构造就有 —— 批注可以在排版之前建锚点。
        assert_eq!(document.text_map().text(), LONG);
    }
}
