//! 「一章的源文本 → `folio_core::layout::Layout`」这一段，桌面与移动共用。
//!
//! 分成两件事：**怎么排**在这里，**怎么摆上屏**留在各平台自己的视图层
//! （桌面 [`super::super::desktop::engine`] 把行盒接进虚拟列表，
//! 移动 `ui/mobile/reader_view.rs` 自己逐行绘制）。这样同一本书在两端得到的是同一份
//! `Layout`，屏幕上看到的行只可能因为视口宽度不同而不同，不会因为「两端各实现了一遍
//! 层叠与断行」而不同。
//!
//! [`chapter_xhtml`] 是这段链子的入口之一：Markdown / TXT 的章源文本是规范化纯文本，
//! 排版层只认 XHTML，所以先做一次最小包装（空行分段、段内换行 → `<br/>`、
//! ATX 标题 → `<hN>`，文字全部 XML 转义）。段落顺序与字符内容都不改动原文，
//! 所以这条链子不会让可见文本的坐标漂移（引擎路径的坐标见 `folio_core::anchor::TextMap`）。

use std::path::Path;
use std::sync::Arc;

use folio_core::Reader;
use folio_core::anchor::TextMap;
use folio_core::css::computed::{ComputedStyle, TextAlign, cascade};
use folio_core::css::{Origin, RuleSet};
use folio_core::dom::{Dom, NodeId};
use folio_core::layout::{Layout, LayoutOptions, LineBox, layout as typeset};
use folio_core::{Chapter, Content, DocMeta, FlowDoc, Format};
use gpui_kit::{App, Window};

use super::images::ChapterImages;
use super::measure::GpuiMeasure;

/// 打开一个文档。folio-core 的 `open` 按扩展名挑适配器（epub / md / txt）。
pub fn open(path: &Path) -> folio_core::Result<(DocMeta, FlowDoc)> {
    let (meta, content) = folio_core::open(path)?;
    match content {
        Content::Reflow(flow) => Ok((meta, flow)),
    }
}

/// 章节源文本 → 交给排版层的 XHTML 片段。
///
/// EPUB 的章节本来就是 XHTML，原样透传。Markdown / TXT 的 `Chapter::source` 是
/// **规范化后的纯文本**（`folio-core/src/format/markdown.rs` 只做行尾统一、压空行、
/// 按 `#` 分章，从不生成 HTML；桌面端把这两类交给 `gpui_kit::component::text::TextView`，
/// 移动端走自研引擎），所以要在这里补一次最小规范化。
pub fn chapter_xhtml(chapter: &Chapter, format: Format) -> String {
    if format == Format::Epub {
        return chapter.source.clone();
    }
    let mut out = String::with_capacity(chapter.source.len() + 64);
    for block in chapter.source.split("\n\n") {
        let lines: Vec<&str> = block.lines().collect();
        if lines.iter().all(|line| line.trim().is_empty()) {
            continue;
        }
        // 1–6 个 `#` 加空格才是 ATX 标题，其余一律当正文。
        if let Some((level, text)) = lines
            .first()
            .and_then(|first| atx_heading(first.trim_start()))
        {
            // 标题独占一行：整块只有它是标题，正文留给后面的段。
            out.push_str(&format!("<h{level}>"));
            escape(&mut out, text);
            out.push_str(&format!("</h{level}>"));
            let rest = &block[block.find('\n').map(|i| i + 1).unwrap_or(block.len())..];
            if !rest.trim().is_empty() {
                out.push('\n');
                paragraph(&mut out, rest);
            }
            out.push('\n');
            continue;
        }
        paragraph(&mut out, block);
        out.push('\n');
    }
    out
}

/// 一段正文：内部换行保留成 `<br/>`（`white-space: normal` 下 `\n` 仍是硬换行，
/// 见 `folio-core/src/layout/mod.rs` 的 `hard_lines`）。
fn paragraph(out: &mut String, text: &str) {
    out.push_str("<p>");
    for (ix, line) in text.lines().enumerate() {
        if ix > 0 {
            out.push_str("<br/>");
        }
        escape(out, line.trim());
    }
    out.push_str("</p>");
}

fn atx_heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim();
    (!text.is_empty()).then_some((hashes as u8, text))
}

fn escape(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

/// `<body>` 不在（纯片段或畸形 XHTML）时退回文档根：排版层只认「从这里往下找内容」。
pub fn body_of(dom: &Dom) -> NodeId {
    dom.elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document)
}

/// 一章的排版状态：源文本 + 书自己的 CSS + 读者偏好 + 本章图片索引，
/// 缓存上一次的 `Layout`。
///
/// 重排只在**排版输入真的变了**之后发生（版心宽度或任何一项读者偏好），所以逐帧调用
/// [`Document::ensure_layout`] 是安全的：滚动、点击、别的窗口改标题都不触发重算。
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
    /// 上次排版时的 `(内容宽度, 读者偏好)`。
    ///
    /// 记的是**整份偏好**而不是字号与行距：首行缩进、段距、是否两端对齐同样进排版，
    /// 只按字号与行距判断就会让「改了缩进但不重排」这种毛病留在缓存里。
    laid_out: Option<(f32, Reader)>,
    layout: Layout,
}

impl Document {
    pub fn new(source: String, css: &str, prefs: Reader, images: Arc<ChapterImages>) -> Self {
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
            laid_out: None,
            layout: Layout::default(),
        }
    }

    /// 源文本 → DOM + 可见文本坐标。两者必须一起换，否则坐标指的就是上一章的文本。
    fn parse_chapter(source: &str) -> (Dom, TextMap) {
        let dom = Dom::parse(source.as_bytes());
        let map = TextMap::build(&dom, body_of(&dom));
        (dom, map)
    }

    pub fn set_prefs(&mut self, prefs: Reader) {
        self.prefs = prefs;
    }

    /// 换章。源文本没变就保留上一次的排版结果，来回翻同一章不必重算。
    pub fn set_chapter(&mut self, source: String) {
        if source != self.source {
            let (dom, map) = Self::parse_chapter(&source);
            self.source = source;
            self.dom = dom;
            self.map = map;
            self.laid_out = None;
        }
    }

    /// 换章时图片索引也要跟着换：`<img src>` 的相对路径基准目录随章变化。
    pub fn set_images(&mut self, images: Arc<ChapterImages>) {
        self.images = images;
        self.laid_out = None;
    }

    pub fn prefs(&self) -> &Reader {
        &self.prefs
    }

    pub fn images(&self) -> &Arc<ChapterImages> {
        &self.images
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
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
    /// **`pane_reserve` 是外壳里不属于阅读栏的宽度**（桌面是侧栏，移动端没有侧栏所以传 0）。
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
    pub fn ensure_layout(&mut self, window: &mut Window, cx: &mut App, pane_reserve: f32) {
        let width = Self::content_width(window, &self.prefs, pane_reserve);
        let key = (width, self.prefs.clone());
        if self.laid_out.as_ref() == Some(&key) {
            return;
        }
        let measure = GpuiMeasure::new(
            cx.text_system().clone(),
            window.text_system().clone(),
            self.prefs.font_family.clone(),
        );
        let styles = cascade(&self.dom, &self.rules, &Self::root_base(&self.prefs));
        // 解析不做在这里（`dom` 是字段）；换章才重解析，见 `parse_chapter`。
        let body = body_of(&self.dom);
        self.layout = typeset(
            &self.dom,
            &styles,
            body,
            LayoutOptions { width },
            &measure,
            self.images.as_ref(),
        );
        self.laid_out = Some(key);
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
    //! 两端共用的排版地基（`ui/shared/document.rs` 与 `measure.rs`）在宿主上跑的用例。
    //!
    //! 为什么这些算移动端的测试：移动 UI 本身只在 `cfg(mobile)` 下编译，宿主的
    //! `cargo test` 碰不到那一侧的视图。但「同一本书在两端排出一样的行」这件事
    //! 完全落在这一层 —— 源文本包装、层叠、版心宽度、断行与缓存都在这里，
    //! 移动端只是把它们摆上屏的那层薄壳。所以这一层被测住，移动端的排版就不是没测过。
    //!
    //! 反过来讲也很清楚：这里**测不到**的是触摸滚动、章节按钮的点击、主题在 Android 上
    //! 解析到浅还是深 —— 那几件只有真开窗口、真上机器才算验证。

    use std::sync::Arc;

    use crate::ui::shared::document::{Document, body_of, chapter_xhtml};
    use crate::ui::shared::images::ChapterImages;
    use folio_core::layout::LineFragment;
    use folio_core::{Chapter, Format, Reader};
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
        );
        cx.update_window(handle, |_, win, cx| {
            document.ensure_layout(win, cx, 0.0);
        })
        .expect("窗口应当可排版");
        document
    }

    /// Markdown 的章源文本是规范化纯文本，进排版层之前要包成 XHTML。
    /// 这里钉住四件事：空行分段、段内换行成硬换行、ATX 标题成 `<hN>`、特殊字符转义。
    /// 少任何一条，移动端读 `.md` 就会把整篇排成一个段落，或把尖括号当标签吃掉。
    #[test]
    fn markdown_blocks_become_xhtml_without_losing_text() {
        let chapter = Chapter {
            title: "第一章".into(),
            source: "# 标题\n\n第一段\n还是第一段\n\n第二段\n\n<script>x</script>".into(),
            spine_href: String::new(),
        };
        let xhtml = chapter_xhtml(&chapter, Format::Markdown);

        assert!(
            xhtml.contains("<h1>标题</h1>"),
            "ATX 标题要成 `<h1>`：{xhtml}"
        );
        assert!(
            xhtml.contains("<p>第一段<br/>还是第一段</p>"),
            "段内换行要成硬换行：{xhtml}"
        );
        assert!(xhtml.contains("<p>第二段</p>"), "空行分段：{xhtml}");
        // 转义后不能剩下裸的 `<script>`，否则 html5ever 会真当标签解析，正文就少一段。
        assert!(
            !xhtml.contains("<script>"),
            "正文里的尖括号必须转义：{xhtml}"
        );
        assert!(xhtml.contains("&lt;script&gt;"));

        // EPUB 的章节本来就是 XHTML，一个字节都不该动。
        let epub = Chapter {
            title: String::new(),
            source: "<body><p>原样</p></body>".into(),
            spine_href: String::new(),
        };
        assert_eq!(chapter_xhtml(&epub, Format::Epub), epub.source);
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
    /// （`ui/mobile/reader_view.rs` 用 gpui 的 `img` 元素），所以一个解不出来的 `src`
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
        );
        assert_eq!(document.line_count(), 0, "没排过版就没有行盒");
        assert_eq!(document.offset_at_y(4000.0), 0);
        assert_eq!(document.y_at_offset(1234), 0.0);
        assert_eq!(document.line_at_offset(1234), 0);
        // 坐标表不依赖排版，构造就有 —— 批注可以在排版之前建锚点。
        assert_eq!(document.text_map().text(), LONG);
    }

    /// 桌面端虚拟列表的项高换算。`engine` 只在 `cfg(not(mobile))` 下编译，所以这一整块
    /// 跟着门控；它留在 `document` 的测试里而不搬去 `engine.rs`，是因为它复用上面对象
    /// 的 `layout_of` / `bare_prefs` 脚手架 —— 搬过去就得把那套「开窗口把一段 XHTML
    /// 排完」的东西抄第二份，两份几何会在不同的字体集上慢慢漂开。
    #[cfg(not(mobile))]
    mod list_metrics {
        use super::{LONG, bare_prefs, layout_of};
        use crate::ui::features::reader::engine::{item_top, line_at_item_top};
        use gpui_kit::TestAppContext;

        /// 桌面端那张虚拟列表的项高**不含块间空隙**，这是量出来的，不是推测：
        /// 三段各一行时最后一行的行底在 132 之上，而逐项高度之和比排版总高小一档以上。
        ///
        /// 写这条测试有两个目的：① `engine::item_top` / `line_at_item_top` 必须在
        /// **列表空间**里换算（用排版的绝对 y 会每段差一档 margin，恢复进度就偏半屏）；
        /// ② 段与段之间在桌面上少一档空白这件事必须留一道门 —— 谁去修渲染（把
        /// 「到下一行的推进量」当项高），这条会红，那时候要连 `line_element` 的
        /// `top` 一起改，不能只改一边。
        ///
        /// 断言只写**关系**不写具体像素：行盒高取 `max(line-height 盒, ascent+descent)`，
        /// 后者由解析出来的真字体给，本机与 CI 的字体集不同，钉死数字就是一条见机器就红的测试。
        #[gpui_kit::test]
        fn the_engine_list_counts_line_boxes_not_the_gaps_between_them(cx: &mut TestAppContext) {
            cx.update(gpui_kit::init);
            let xhtml = format!("<p>{LONG}</p><p>{LONG}</p><p>{LONG}</p>");
            let document = layout_of(cx, &xhtml, &bare_prefs(), 600.0);
            let lines = document.lines();
            assert_eq!(
                lines.len(),
                3,
                "27 个字 × 16px = 432px < 600px 版心，每段正好一行"
            );

            let boxes: f32 = lines.iter().map(|line| line.height).sum();
            // UA 表给 `<p>` 上下各 1em，所以第一段不是从 0 开始排。
            assert!(
                lines[0].y > 0.0,
                "首行行顶该在 UA 的段前距之下，实际 {}",
                lines[0].y
            );
            let last_bottom = lines[2].y + lines[2].height;
            assert!(
                last_bottom > boxes,
                "最后一行的行底（{last_bottom}）比项高之和（{boxes}）大 —— 差的就是列表里没有的那些档"
            );
            assert!(
                document.height() > boxes,
                "排版总高（{}）该大于项高之和（{boxes}），否则这两个空间就是一回事",
                document.height()
            );

            // 列表空间的项顶就是逐项累加，与绝对 y 无关。
            assert_eq!(item_top(&document, 0), 0.0);
            assert_eq!(item_top(&document, 1), lines[0].height);
            // 越界的行号**夹到最后一项的顶**，不是给出项高之和：调用方是「跳到第 k 行」，
            // k 超界时要停在最后一行能到的位置，而不是滚出内容末尾。
            assert_eq!(
                item_top(&document, 3),
                item_top(&document, 2),
                "行号越界就夹住，别滚出一屏空白"
            );
            for (line, row) in lines.iter().enumerate() {
                let top = item_top(&document, line);
                assert_eq!(line_at_item_top(&document, top), line, "项顶反解回自己");
                assert_eq!(
                    line_at_item_top(&document, top + row.height / 2.0),
                    line,
                    "落在项内部任意一点也还是那一项"
                );
            }
        }
    }
}
