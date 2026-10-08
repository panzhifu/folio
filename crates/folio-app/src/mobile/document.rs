//! 一条文档路径 → `folio_core::layout::Layout`。
//!
//! 这一段是移动端的「外壳」：folio-core 负责 format → dom → css → layout，
//! 这里只补上那一步 —— 把纯文本文档变成排版层认得的 XHTML。

use std::path::Path;

use folio_core::Reader;
use folio_core::css::computed::{ComputedStyle, TextAlign, cascade};
use folio_core::css::{Origin, RuleSet};
use folio_core::dom::{Dom, NodeId};
use folio_core::layout::{Layout, LayoutOptions, NoImages, layout as typeset};
use folio_core::{Chapter, Content, DocMeta, FlowDoc, Format};
use gpui::{App, Window};

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
/// **规范化后的纯文本**（见 `folio-core/src/format/markdown.rs`：它只做行尾统一、
/// 压空行、按 `#` 分章，从不生成 HTML；桌面端把这两类交给 gpui-kit 的 TextView，
/// 移动端走自研引擎，所以要自己做一次最小规范化）：
/// 空行分段、段内换行 → `<br/>`、ATX 标题 → `<hN>`，文字全部 XML 转义。
/// 段落顺序与字符内容都不改动原文，锚点坐标系（`Chapter::source` 的字节偏移）
/// 因此仍然成立。
pub fn chapter_xhtml(chapter: &Chapter, format: Format) -> String {
    if format == Format::Epub {
        return chapter.source.clone();
    }
    let mut out = String::with_capacity(chapter.source.len() + 64);
    for block in chapter.source.split("\n\n") {
        let lines: Vec<&str> = block.lines().collect();
        if lines.iter().all(|l| l.trim().is_empty()) {
            continue;
        }
        // 1–6 个 `#` 加空格才是 ATX 标题，其余一律当正文。
        if let Some((level, text)) = lines
            .first()
            .and_then(|first| atx_heading(first.trim_start()))
        {
            // 标题独占一行：整块只有它是标题，正文留给后面的段。
            let head = format!("<h{level}>");
            out.push_str(&head);
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
    let mut first = true;
    for line in text.lines() {
        if !first {
            out.push_str("<br/>");
        }
        first = false;
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
    (!text.is_empty()).then(|| (hashes as u8, text))
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
/// 与 `folio-app/src/engine.rs` 的 `body_of` 同一套降级。
fn body_of(dom: &Dom) -> NodeId {
    dom.elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document)
}

/// 一章的排版状态：源文本 + 书自己的 CSS + 读者偏好，缓存上一次的 `Layout`。
///
/// 与桌面端 `Engine` 的分工一致 —— 只是这里没有虚拟列表，绘制层直接读 `lines()`
/// 并按 `scroll_y` 自己挑可见行。
pub struct Page {
    xhtml: String,
    rules: RuleSet,
    prefs: Reader,
    /// 上次排版时的 (内容宽度, 字号, 行距)，三者没变就不重排。
    laid_out: Option<(f32, f32, f32)>,
    layout: Layout,
}

impl Page {
    pub fn new(xhtml: String, css: &str, prefs: Reader) -> Self {
        let mut rules = RuleSet::default();
        // 书自己的 CSS 是作者层；读者偏好通过根样式进来（`root_base`）。
        rules.extend(RuleSet::parse(css, Origin::Author));
        Self {
            xhtml,
            rules,
            prefs,
            laid_out: None,
            layout: Layout::default(),
        }
    }

    pub fn set_prefs(&mut self, prefs: Reader) {
        self.prefs = prefs;
    }

    pub fn prefs(&self) -> &Reader {
        &self.prefs
    }

    pub fn lines(&self) -> &[folio_core::layout::LineBox] {
        &self.layout.lines
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// 版心宽度 = 视口宽 − 左右页边距。
    ///
    /// **`max_measure == 0` 是「不设上限、随窗口铺满」的哨兵值**（见
    /// `folio_core::Reader::max_measure` 的注释），不是真的 0 像素上限 ——
    /// 当成上限用 `usable.min(0)` 就会塌成 1px 版心、一行只剩一个字。
    /// 桌面端 `folio-app/src/engine.rs` 的 `content_width` 是同一份语义，
    /// 差别只在移动端没有侧栏，所以 `pane_reserve` 恒为 0。
    pub fn content_width(window: &Window, prefs: &Reader) -> f32 {
        let viewport = f32::from(window.viewport_size().width);
        let usable = viewport - prefs.margin_x * 2.0;
        let capped = if prefs.max_measure > 0.0 {
            usable.min(prefs.max_measure)
        } else {
            usable
        };
        capped.max(1.0)
    }

    /// 根样式 = 读者偏好。书自己的 CSS 能覆盖它（作者层高于读者普通层，
    /// 与浏览器一致），读者的 `!important` 终究能压回来。照抄桌面端 `root_base`。
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

    /// 度量要用窗口级文本系统，所以这一步留在渲染帧里做（与桌面端同一取舍：
    /// 挪进 `background_spawn` 是 M4 的事）。
    pub fn ensure_layout(&mut self, window: &mut Window, cx: &mut App) {
        let width = Self::content_width(window, &self.prefs);
        let key = (width, self.prefs.font_size, self.prefs.line_height);
        if self.laid_out == Some(key) {
            return;
        }
        let measure = crate::mobile::measure::MobileMeasure::new(
            cx.text_system().clone(),
            window.text_system().clone(),
            self.prefs.font_family.clone(),
        );
        let dom = Dom::parse(self.xhtml.as_bytes());
        let styles = cascade(&dom, &self.rules, &Self::root_base(&self.prefs));
        self.layout = typeset(
            &dom,
            &styles,
            body_of(&dom),
            LayoutOptions { width },
            &measure,
            &NoImages,
        );
        self.laid_out = Some(key);
    }
}
