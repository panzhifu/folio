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
