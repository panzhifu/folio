//! OpenDocument Text（`.odt`）：一个 ZIP 里的 `content.xml`。
//!
//! 与 DOCX 同一类：不是电子书格式，但容器与正文结构足够接近 EPUB，所以只做一件事 ——
//! **把 ODF 的正文归约成引擎就绪的 XHTML**。分章、书目、图片全部走现有链路。
//!
//! 一期承诺：
//!
//! - `text:h`（按 `text:outline-level`）起章，`text:p` 段落
//! - `text:span` 的粗体 / 斜体按 `styles.xml` 里的 `fo:font-weight` / `fo:font-style` 认
//! - `text:s` / `text:tab` / `text:line-break` / `text:a` / `draw:image`
//! - `text:list` / `text:list-item` → `<ul>` / `<ol>`，记数取 `text:list-style` 里的
//!   `style:num-format` 与 `text:bullet-char`（层号 = `<text:list>` 的嵌套深度）
//! - `text:note` → 上标锚 + `text:note-body` 摆回**它所在的那一章**末尾
//!
//! 复杂样式、分栏、浮动框降级成普通段落。**不丢字**是底线。
//!
//! `text:continue-numbering`（第二个列表接着前一个的号往下数）不读：归约产物里每一个
//! `<ol>` 都从头数，代价是那种「接上一串号」的列表屏上是 1、2、3 而纸上是 4、5、6。

use std::collections::HashMap;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use super::zip::Zip;
use super::{
    Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, FlowDoc, Format, ImageRes,
    ListWriter, Marker, image_dims, media_type_of, resolve_href, stem_of,
};
// 归约产物的转义与「本章引用了哪几张登记过的图」只有一份实现（见 `super::escape_text`
// 与 `super::chapter_images` 那两条注释）。
use super::{chapter_images, escape_attr, escape_into};
use crate::Result;

pub struct OdtAdapter;

impl DocumentProvider for OdtAdapter {
    fn format(&self) -> Format {
        Format::Odt
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(super::extension_of(path).as_deref(), Some("odt"))
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let book = read_book(path)?;
        let chapters = book
            .chapters
            .iter()
            .enumerate()
            .map(|(ix, (title, body))| {
                Chapter::section(title.clone(), ix)
                    .with_char_len(super::visible_chars(body))
                    .with_images(chapter_images(body, &book.images))
            })
            .collect();
        Ok((
            book.meta,
            Content::Reflow(FlowDoc {
                chapters,
                toc: Vec::new(),
                images: book.images,
                ..Default::default()
            }),
        ))
    }

    fn chapter_source(&self, path: &Path, at: &ChapterLocator) -> Result<String> {
        let index = match at {
            ChapterLocator::Section(index) => *index,
            other => {
                return Err(crate::Error::ChapterUnavailable {
                    path: path.to_path_buf(),
                    index: 0,
                    at: format!("{other:?}"),
                });
            }
        };
        let book = read_book(path)?;
        book.chapters
            .get(index)
            .map(|(_, body)| body.clone())
            .ok_or_else(|| crate::Error::ChapterUnavailable {
                path: path.to_path_buf(),
                index,
                at: format!("{at:?}"),
            })
    }

    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let book = read_book(path)?;
        let chapters = book
            .chapters
            .into_iter()
            .map(|(title, body)| Chapter::inline(title, body))
            .collect();
        Ok((
            book.meta,
            Content::Reflow(FlowDoc {
                chapters,
                toc: Vec::new(),
                images: book.images,
                ..Default::default()
            }),
        ))
    }

    fn read_texts(&self, path: &Path) -> Option<Vec<String>> {
        read_book(path)
            .ok()
            .map(|book| book.chapters.into_iter().map(|(_, body)| body).collect())
    }
}

struct Book {
    meta: DocMeta,
    chapters: Vec<(String, String)>,
    images: Vec<ImageRes>,
}

fn read_book(path: &Path) -> Result<Book> {
    let mut zip = Zip::open(path)?;
    let meta = parse_meta(
        zip.read_text("meta.xml").as_deref().unwrap_or(""),
        &stem_of(path),
        std::fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
    );
    let styles = parse_styles(zip.read_text("styles.xml").as_deref().unwrap_or(""));
    let xml = zip
        .read_text("content.xml")
        .ok_or_else(|| unsupported(path, "ZIP 里没有 content.xml"))?;
    // 列表样式两处都要扫（见 [`parse_list_styles`]），正文的引用在 `content.xml` 里。
    let mut list_styles = parse_list_styles(zip.read_text("styles.xml").as_deref().unwrap_or(""));
    list_styles.extend(parse_list_styles(&xml));

    let mut builder = Builder::new(meta.title.clone(), &styles, list_styles);
    builder.parse(&xml);
    builder.finish();
    if builder
        .chapters
        .iter()
        .all(|(_, body)| body.trim().is_empty())
    {
        return Err(unsupported(path, "content.xml 里没有正文"));
    }
    let images = builder.images(&mut zip);
    Ok(Book {
        meta,
        chapters: builder.chapters,
        images,
    })
}

/// `meta.xml`：书名、作者、语言、简介。
fn parse_meta(xml: &str, stem: &str, size: u64) -> DocMeta {
    let mut title = None;
    let mut initial_creator = None;
    let mut creator = None;
    let mut language = String::new();
    let mut description = String::new();

    let mut reader = Reader::from_str(xml);
    let mut want: Option<&str> = None;
    let mut buf = String::new();
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) => {
                let name = local_name(e.name().as_ref());
                want = match name.as_str() {
                    "title" => Some("title"),
                    "initial-creator" => Some("initial-creator"),
                    "creator" => Some("creator"),
                    "language" => Some("language"),
                    "description" => Some("description"),
                    _ => None,
                };
                buf.clear();
            }
            Event::Text(e) if want.is_some() => super::xml_piece(&mut buf, e.as_ref(), false),
            Event::GeneralRef(e) if want.is_some() => super::xml_piece(&mut buf, e.as_ref(), true),
            Event::End(e) => {
                let name = local_name(e.name().as_ref());
                if want == Some(name.as_str()) {
                    let value = buf.trim().to_string();
                    match name.as_str() {
                        "title" if !value.is_empty() => title = Some(value),
                        "initial-creator" if !value.is_empty() => initial_creator = Some(value),
                        "creator" if !value.is_empty() => creator = Some(value),
                        "language" => language = value,
                        "description" => description = value,
                        _ => {}
                    }
                }
                want = None;
            }
            Event::Eof => break,
            _ => {}
        }
    }

    let authors = initial_creator.or(creator).into_iter().collect();
    DocMeta {
        title: title.unwrap_or_else(|| stem.to_string()),
        authors,
        language,
        description,
        format: Format::Odt,
        size,
        ..DocMeta::default()
    }
}

#[derive(Default, Clone, Copy)]
struct TextStyle {
    bold: bool,
    italic: bool,
}

/// `styles.xml` 里 `style:family="text"` 的粗体 / 斜体。
///
/// 只认文本样式；段落样式里的字体、缩进交给排版默认值，避免把 Word/Writer 的
/// 页面几何带进重排引擎。
fn parse_styles(xml: &str) -> HashMap<String, TextStyle> {
    let mut out = HashMap::new();
    let mut reader = Reader::from_str(xml);
    let mut current: Option<String> = None;
    let mut text_style = TextStyle::default();
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) | Event::Empty(e) => {
                let name = local_name(e.name().as_ref());
                if name == "style" && attr(&e, "family").as_deref() == Some("text") {
                    current = attr(&e, "name");
                    text_style = TextStyle::default();
                } else if current.is_some() && name == "text-properties" {
                    text_style.bold = attr(&e, "font-weight")
                        .is_some_and(|value| value.eq_ignore_ascii_case("bold") || value == "700");
                    text_style.italic = attr(&e, "font-style")
                        .is_some_and(|value| value.eq_ignore_ascii_case("italic"));
                }
            }
            Event::End(e) => {
                if local_name(e.name().as_ref()) == "style"
                    && let Some(name) = current.take()
                {
                    out.insert(name, text_style);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// 列表样式：`(样式名, 层级)` → 那一层画什么号。
///
/// 定义在**两处**都可能出现：`styles.xml` 的 `office:styles`（作者命名的 `L1` / `L2`）与
/// `content.xml` 的 `office:automatic-styles`（LibreOffice 给每一段本地列表现造的样式）。
/// 两处各扫一遍合进同一张表 —— 扫的时候不看它的父容器：两种容器下的 `text:list-style`
/// 形状是同一个，多一层判据只是多一处会漏的地方。
///
/// 三格记数都来自真样本：`liste-courses.odt` 的 `text:bullet-char` 是 `•` `◦` `▪` 三层，
/// `welcome.odt`（LibreOffice 写的）的 `style:num-format` 是 `A` 与 `1`。
fn parse_list_styles(xml: &str) -> HashMap<(String, u32), Marker> {
    let mut out = HashMap::new();
    let mut reader = Reader::from_str(xml);
    let mut style: Option<String> = None;
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) | Event::Empty(e) => {
                let name = local_name(e.name().as_ref());
                if name == "list-style" {
                    style = attr(&e, "name");
                    continue;
                }
                let Some(style_name) = style.as_deref() else {
                    continue;
                };
                // `text:level` 是 1 起的；缺省按第一层（生产者的写法里没有缺这一格的真样本）。
                let level = attr(&e, "level")
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or(1);
                let marker = match name.as_str() {
                    "list-level-style-bullet" => {
                        marker_of_bullet(&attr(&e, "bullet-char").unwrap_or_default())
                    }
                    "list-level-style-number" => {
                        marker_of_num_format(&attr(&e, "num-format").unwrap_or_default())
                    }
                    // 图片型的项目符号：那张符号在本文件里，画不出来就当圆点 ——
                    // 这一格确确实实是一个符号而不是号，不能降级成十进制。
                    "list-level-style-image" => Marker::Disc,
                    _ => continue,
                };
                out.insert((style_name.to_string(), level), marker);
            }
            Event::End(e) => {
                if local_name(e.name().as_ref()) == "list-style" {
                    style = None;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// `style:num-format` → [`Marker`]。
///
/// 那一格按 ODF 是一个「格式串」：`1` 是十进制、`a` / `A` / `i` / `I` 是那四种拉丁与
/// 罗马写法，其余（真见过 `-`）是**作者自己指定的字面符号** —— 字面符号画不出来，
/// 按圆点走：它要的是一个符号，不是一个号。
fn marker_of_num_format(value: &str) -> Marker {
    match value {
        "1" => Marker::Ordered(None),
        "a" => Marker::Ordered(Some("lower-alpha")),
        "A" => Marker::Ordered(Some("upper-alpha")),
        "i" => Marker::Ordered(Some("lower-roman")),
        "I" => Marker::Ordered(Some("upper-roman")),
        "none" => Marker::Nothing,
        _ => Marker::Disc,
    }
}

/// `text:bullet-char` → 哪一个符号。表外（`➢` 那类自定义字符）落圆点：
/// 引擎的标记由 `layout::marker` 按类型折，自定义那一格画不出来，而形状上它就是个项目符号。
fn marker_of_bullet(char: &str) -> Marker {
    match char {
        "◦" | "○" => Marker::Circle,
        "▪" | "■" => Marker::Square,
        _ => Marker::Disc,
    }
}

struct Builder {
    meta_title: String,
    styles: HashMap<String, TextStyle>,
    /// 列表样式那张表（见 [`parse_list_styles`]）。
    list_styles: HashMap<(String, u32), Marker>,
    chapters: Vec<(String, String)>,
    current_title: Option<String>,
    body: String,
    para: Option<String>,
    /// 当前段落是 `text:h` 时记下层级；`text:p` 是 `None`。
    heading: Option<usize>,
    span: Vec<TextStyle>,
    link_open: bool,
    used_images: Vec<String>,
    /// 正开着的那几层列表（层号就取它的深度：`<text:list>` 是嵌套元素，深度就是层级）。
    lists: ListWriter,
    /// 正在收的那一条注的锚 id（`None` = 不在注里）。
    note: Option<String>,
    /// 注体里攒起来的段落（`End("text:note")` 时拼成一条注文）。
    note_parts: Vec<String>,
    /// `text:note-citation` 那一格：它就是屏上要摆的号（LibreOffice 自己算好了）。
    citation: String,
    citation_open: bool,
    /// `End("text:p")` 落到注体而不是正文时，外层那一段先寄存在这里。
    saved_para: Option<String>,
    /// `id → 注体` 与本章引用序（注体要摆回**这一章**末尾，弹层只在本章找 id）。
    note_bodies: HashMap<String, String>,
    cited: Vec<String>,
}

impl Builder {
    fn new(
        meta_title: String,
        styles: &HashMap<String, TextStyle>,
        list_styles: HashMap<(String, u32), Marker>,
    ) -> Self {
        Self {
            meta_title,
            styles: styles.clone(),
            list_styles,
            chapters: Vec::new(),
            current_title: None,
            body: String::new(),
            para: None,
            heading: None,
            span: Vec::new(),
            link_open: false,
            used_images: Vec::new(),
            lists: ListWriter::default(),
            note: None,
            note_parts: Vec::new(),
            citation: String::new(),
            citation_open: false,
            saved_para: None,
            note_bodies: HashMap::new(),
            cited: Vec::new(),
        }
    }

    fn parse(&mut self, xml: &str) {
        let mut reader = Reader::from_str(xml);
        while let Ok(event) = reader.read_event() {
            match event {
                Event::Start(e) => {
                    let name = local_name(e.name().as_ref());
                    match name.as_str() {
                        "h" => {
                            self.flush_paragraph();
                            self.para = Some(String::new());
                            self.heading = Some(
                                attr(&e, "outline-level")
                                    .and_then(|value| value.parse::<usize>().ok())
                                    .unwrap_or(1)
                                    .clamp(1, 6),
                            );
                        }
                        "p" => {
                            self.flush_paragraph();
                            self.para = Some(String::new());
                            self.heading = None;
                        }
                        "span" => self.span.push(self.style_of(&e)),
                        "image" => {
                            if let Some(href) = attr(&e, "href") {
                                self.emit_image(&href);
                            }
                        }
                        "a" => {
                            if let Some(href) = attr(&e, "href") {
                                self.push_html(&format!("<a href=\"{}\">", escape_attr(&href)));
                                self.link_open = true;
                            }
                        }
                        "list" => self.open_list(&e),
                        "note" => self.start_note(&e),
                        "note-citation" => {
                            self.citation_open = true;
                            self.citation.clear();
                        }
                        _ => {}
                    }
                }
                Event::Empty(e) => {
                    let name = local_name(e.name().as_ref());
                    match name.as_str() {
                        "s" => {
                            let count = attr(&e, "c")
                                .and_then(|value| value.parse::<usize>().ok())
                                .unwrap_or(1);
                            self.push_html(&" ".repeat(count));
                        }
                        "tab" => self.push_html(" "),
                        "line-break" => self.push_html("<br/>"),
                        "image" => {
                            if let Some(href) = attr(&e, "href") {
                                self.emit_image(&href);
                            }
                        }
                        _ => {}
                    }
                }
                Event::Text(e) => self.push_text(e.as_ref(), false),
                // `&amp;` 在 `Text` 之外单独成一个事件，只吃 `Text` 会把这一个字符丢掉
                // （见 `super::push_xml_piece` 的说明）。
                Event::GeneralRef(e) => self.push_text(e.as_ref(), true),
                Event::End(e) => {
                    let name = local_name(e.name().as_ref());
                    match name.as_str() {
                        "h" | "p" => self.flush_paragraph(),
                        "span" => {
                            self.span.pop();
                        }
                        "a" if self.link_open => {
                            self.push_html("</a>");
                            self.link_open = false;
                        }
                        "list" => self.lists.pop(&mut self.body),
                        "note-citation" => self.citation_open = false,
                        "note" => self.finish_note(),
                        _ => {}
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
    }

    fn style_of(&self, start: &BytesStart<'_>) -> TextStyle {
        attr(start, "style-name")
            .and_then(|name| self.styles.get(&name).copied())
            .unwrap_or_default()
    }

    fn emit_image(&mut self, href: &str) {
        let href = resolve_href("", href);
        if !self.used_images.iter().any(|known| known == &href) {
            self.used_images.push(href.clone());
        }
        self.push_html(&format!("<img src=\"{}\"/>", escape_attr(&href)));
    }

    /// 一段文字 → 引擎就绪的那一段：先折实体、再转义、最后按当前 span 的样式包。
    fn push_text(&mut self, raw: &str, is_reference: bool) {
        // 引用那一格（`text:note-citation`）也在收字，而它没有段落 —— 外层那一段此刻是寄存着的。
        if self.para.is_none() && !self.citation_open {
            return;
        }
        let mut text = String::new();
        super::xml_piece(&mut text, raw, is_reference);
        let mut escaped = String::new();
        escape_into(&mut escaped, &text);
        let style = self.span.last().copied().unwrap_or_default();
        if style.italic {
            escaped = format!("<em>{escaped}</em>");
        }
        if style.bold {
            escaped = format!("<strong>{escaped}</strong>");
        }
        self.push_html(&escaped);
    }

    fn push_html(&mut self, html: &str) {
        if self.citation_open {
            self.citation.push_str(html);
            return;
        }
        if let Some(para) = self.para.as_mut() {
            para.push_str(html);
        }
    }

    /// 开一层列表：层级取 `<text:list>` 的**嵌套深度**（1 起），记数按这一层的样式。
    ///
    /// `text:style-name` 是空串（LibreOffice 真这么写：`welcome.odt` 里那两个注解列表就是
    /// `text:style-name=""`）或样式里没定义那一层 → 圆点：ODF 的默认列表样式画的是一个
    /// 符号而不是号，这里没有别的依据可取。
    fn open_list(&mut self, start: &BytesStart<'_>) {
        let style = attr(start, "style-name").unwrap_or_default();
        let level = self.lists.depth() as u32 + 1;
        let marker = self
            .list_styles
            .get(&(style.clone(), level))
            .copied()
            .unwrap_or(Marker::Disc);
        self.lists.push(&style, marker, &mut self.body);
    }

    /// `text:note` 是**内联**的：引用点就在这句话中间，注体也写在这里（与 DOCX 那种
    /// 「注体在另一份部件里」正相反）。所以进来先把外层那一段寄存起来，出去时再把上标锚
    /// 接回它原来那个位置 —— 否则注体里的那个 `text:p` 会把外层段落提前 flush 掉。
    fn start_note(&mut self, start: &BytesStart<'_>) {
        let id = attr(start, "id").unwrap_or_default();
        self.saved_para = self.para.take();
        self.heading = None;
        self.note = Some(if id.is_empty() {
            // 没有 `text:id` 就没有可指的锚，按出现序造一个：宁可自造，也不要一个
            // 空的 `href="#"`（点开来跳到本章开头）。
            format!("note-{}", self.cited.len() + 1)
        } else {
            escape_attr(&id)
        });
        self.note_parts.clear();
        self.citation.clear();
        self.citation_open = false;
    }

    fn finish_note(&mut self) {
        let Some(id) = self.note.take() else {
            return;
        };
        self.citation_open = false;
        let body = self.note_parts.join(" ");
        self.note_parts.clear();
        self.para = Some(self.saved_para.take().unwrap_or_default());
        if body.is_empty() {
            // 注体是空的：什么都不写。留一个点开没有内容的上标，看着就是界面坏了。
            return;
        }
        // 号优先用 `text:note-citation` 自己写的那串（Writer 已经算好，含 `a.` 这种写法），
        // 那一格空着才按出现序数一个号兜底。
        let cited = self.cited.len() + 1;
        let label = match super::visible_text(&self.citation).trim() {
            "" => cited.to_string(),
            text => text.to_string(),
        };
        self.note_bodies.insert(id.clone(), body);
        self.cited.push(id.clone());
        // 属性名写 `epub:type`：弹层那头认的就是它（`chapter_layout` 的
        // `footnote_anchor_is_ref` / `is_footnote_body`），与 EPUB 书共用一条解析链。
        self.push_html(&format!(
            "<sup><a epub:type=\"noteref\" href=\"#{id}\">{label}</a></sup>"
        ));
    }

    /// 关掉所有开着的列表层。换标题、换章都要先走这一步，否则 `</ul>` 会掉进下一章。
    fn close_lists(&mut self) {
        self.lists.close_all(&mut self.body);
    }

    fn flush_paragraph(&mut self) {
        let Some(para) = self.para.take() else {
            return;
        };
        let heading = self.heading.take();
        let html = para.trim().to_string();
        if html.is_empty() {
            return;
        }
        // 注体里的段落不进正文：攒起来，等 `text:note` 收尾时拼成一条注文。
        if self.note.is_some() {
            self.note_parts.push(html);
            return;
        }
        match heading {
            Some(1) => {
                let title = super::visible_text(&html).trim().to_string();
                if title.is_empty() {
                    return;
                }
                self.flush_chapter();
                self.current_title = Some(title);
                self.body.push_str(&format!("<h1>{html}</h1>\n"));
            }
            Some(level) => {
                self.close_lists();
                self.body
                    .push_str(&format!("<h{level}>{html}</h{level}>\n"));
            }
            None => {
                if self.lists.depth() > 0 {
                    // 列表里的一项：`<text:p>` 不再包 `<p>`，直接写成 `<li>`。
                    // 包一层就多一个块，圆点与那一行之间会多出一个段前距。
                    self.lists.item(&html, &mut self.body);
                    return;
                }
                self.body.push_str(&format!("<p>{html}</p>"));
                self.body.push('\n');
            }
        }
    }

    fn flush_chapter(&mut self) {
        self.close_lists();
        // 引用过的注摆在这一章末尾：弹层那一条链只在本章里找 `id`。
        for id in std::mem::take(&mut self.cited) {
            let Some(html) = self.note_bodies.get(&id) else {
                continue;
            };
            self.body.push_str(&format!(
                "<aside epub:type=\"footnote\" id=\"{id}\">{html}</aside>\n"
            ));
        }
        let body = std::mem::take(&mut self.body);
        if body.trim().is_empty() {
            return;
        }
        let title = self
            .current_title
            .take()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| self.meta_title.clone());
        self.chapters.push((title, body));
    }

    fn finish(&mut self) {
        self.flush_paragraph();
        self.flush_chapter();
        if self.chapters.is_empty() {
            self.chapters.push((self.meta_title.clone(), String::new()));
        }
    }

    fn images(&self, zip: &mut Zip) -> Vec<ImageRes> {
        let mut out = Vec::new();
        for href in &self.used_images {
            let Some(bytes) = zip.read(href) else {
                continue;
            };
            let ext = super::extension_of(Path::new(href)).unwrap_or_default();
            let (width, height) = image_dims(&bytes);
            out.push(ImageRes {
                href: href.clone(),
                source: href.clone(),
                media_type: media_type_of(&ext).to_string(),
                width,
                height,
            });
        }
        out
    }
}

fn local_name(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_string()
}

fn attr(start: &BytesStart<'_>, want: &str) -> Option<String> {
    for attribute in start.attributes().with_checks(false).flatten() {
        let key = attribute.key.as_ref();
        if local_name(key) == want {
            return Some(
                quick_xml::escape::unescape(&attribute.value)
                    .map(|value| value.into_owned())
                    .unwrap_or_else(|_| attribute.value.to_string()),
            );
        }
    }
    None
}

fn unsupported(path: &Path, why: &str) -> crate::Error {
    crate::Error::UnsupportedFormat(format!(
        "{}：{why}",
        path.file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::Path;

    use super::OdtAdapter;
    use crate::content::format::{Content, DocumentProvider, Format};

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([1, 2, 3, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    fn odt(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("书.odt");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            ("mimetype", b"application/vnd.oasis.opendocument.text".to_vec()),
            (
                "meta.xml",
                r#"<office:document-meta xmlns:office="o" xmlns:dc="d" xmlns:meta="m"><office:meta><dc:title>样书</dc:title><meta:initial-creator>作者乙</meta:initial-creator><dc:language>zh-CN</dc:language><dc:description>简介</dc:description></office:meta></office:document-meta>"#
                    .as_bytes()
                    .to_vec(),
            ),
            (
                "styles.xml",
                br#"<office:document-styles xmlns:office="o" xmlns:style="s" xmlns:fo="f"><office:styles><style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold" fo:font-style="italic"/></style:style></office:styles></office:document-styles>"#
                    .to_vec(),
            ),
            (
                "content.xml",
                r#"<office:document-content xmlns:office="o" xmlns:text="t" xmlns:draw="d" xmlns:xlink="x"><office:body><office:text>
                    <text:h text:outline-level="1">第一章</text:h>
                    <text:p>正文甲 <text:span text:style-name="T1">粗斜</text:span><text:s text:c="2"/>尾巴</text:p>
                    <text:p><draw:image xlink:href="Pictures/pic.png"/></text:p>
                    <text:h text:outline-level="1">第二章</text:h>
                    <text:p>正文乙</text:p>
                </office:text></office:body></office:document-content>"#
                    .as_bytes()
                    .to_vec(),
            ),
            ("Pictures/pic.png", png(20, 10)),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(&body).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    /// 与 `docx` 那一条同一件事的另一处：实体引用在 quick-xml 0.42 里**单独成一个事件**，
    /// 只吃 `Event::Text` 的循环会把 `&` 整颗丢掉，而 ODT 的正文与 `meta.xml` 两处各自攒文本。
    #[test]
    fn entity_references_are_characters_not_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("实体.odt");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            ("mimetype", b"application/vnd.oasis.opendocument.text".to_vec()),
            (
                "meta.xml",
                r#"<office:document-meta xmlns:office="o" xmlns:dc="d"><office:meta><dc:title>R&amp;D 年报</dc:title></office:meta></office:document-meta>"#
                    .as_bytes()
                    .to_vec(),
            ),
            (
                "content.xml",
                r#"<office:document-content xmlns:office="o" xmlns:text="t"><office:body><office:text>
                    <text:p>正文里 Tom&#38;Jerry 与 R&amp;D</text:p>
                </office:text></office:body></office:document-content>"#
                    .as_bytes()
                    .to_vec(),
            ),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(&body).unwrap();
        }
        zip.finish().unwrap();

        let (meta, content) = OdtAdapter.read(&path).unwrap();
        assert_eq!(meta.title, "R&D 年报", "meta.xml 的实体");
        let Content::Reflow(flow) = content else {
            panic!("ODT 该读成可重排");
        };
        // 同一口径：查**可见文本**（引擎就绪的那一份里 `&` 该是 `&amp;`）。
        let seen = super::super::visible_text(flow.chapters[0].source());
        assert_eq!(seen.trim(), "正文里 Tom&Jerry 与 R&D", "两种实体都要落地");
    }

    #[test]
    fn headings_become_chapters_and_text_styles_are_mapped() {
        let dir = tempfile::tempdir().unwrap();
        let path = odt(dir.path());
        let (meta, content) = OdtAdapter.read(&path).unwrap();
        assert_eq!(meta.format, Format::Odt);
        assert_eq!(meta.title, "样书");
        assert_eq!(meta.authors, vec!["作者乙"]);
        assert_eq!(meta.description, "简介");
        let Content::Reflow(flow) = content else {
            panic!("ODT 该读成可重排");
        };
        assert_eq!(flow.chapters.len(), 2);
        assert_eq!(flow.chapters[0].title, "第一章");
        assert!(
            flow.chapters[0]
                .source()
                .contains("<strong><em>粗斜</em></strong>")
        );
        assert!(
            flow.chapters[0].source().contains(" 尾巴"),
            "text:s 要变成两个空格"
        );
        assert_eq!(flow.images.len(), 1);
        assert_eq!(flow.images[0].href, "Pictures/pic.png");
        assert_eq!((flow.images[0].width, flow.images[0].height), (20, 10));
    }

    /// 一份部件完全由调用方摆的 odt（列表与注那两族要的是 `styles.xml` 与 `content.xml`
    /// **两份部件之间的引用**，[`odt`] 那个写死的夹具扩不动）。
    fn odt_parts(dir: &Path, name: &str, parts: &[(&str, &str)]) -> std::path::PathBuf {
        let path = dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("mimetype", opts).unwrap();
        zip.write_all(b"application/vnd.oasis.opendocument.text")
            .unwrap();
        for (part, xml) in parts {
            zip.start_file(*part, opts).unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    /// 把一份 odt 读成「每一章的正文」拼起来的那一串（门里查的是归约产物本身的形状）。
    fn body_of(path: &Path) -> String {
        let (_, content) = OdtAdapter.read(path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("ODT 该读成可重排");
        };
        flow.chapters
            .iter()
            .map(|chapter| chapter.source().to_string())
            .collect()
    }

    /// ODT 的列表**形状在 markup 里（嵌套的 `text:list`），记数在样式里（`text:list-style`）**：
    /// 两半各在一处，缺任一半都画不出号。这一条门同时钉四种记数各自落到哪一截 XHTML ——
    /// 真样本里那四种都见过（`liste-courses.odt` 的 `•` `◦` `▪` 三层与 `welcome.odt` 的
    /// `style:num-format="1"` / `"A"`）。
    #[test]
    fn list_levels_come_from_the_style_and_the_nesting() {
        let dir = tempfile::tempdir().unwrap();
        let styles = r#"<office:document-styles xmlns:office="o" xmlns:style="s" xmlns:text="t">
            <office:styles><text:list-style style:name="L1">
                <text:list-level-style-bullet text:level="1" text:bullet-char="•"/>
                <text:list-level-style-number text:level="2" style:num-format="1"/>
                <text:list-level-style-bullet text:level="3" text:bullet-char="◦"/>
                <text:list-level-style-bullet text:level="4" text:bullet-char="▪"/>
            </text:list-style></office:styles>
        </office:document-styles>"#;
        // 四层套下去再回到第一层加一项：`text:list` 是嵌套在 `text:list-item` **里面**的，
        // 层号就取这个深度；「戊」与「甲」同层，靠的是关掉里面那三层。
        let content = r#"<office:document-content xmlns:office="o" xmlns:text="t"><office:body><office:text>
            <text:list text:style-name="L1"><text:list-item><text:p>甲</text:p>
            <text:list text:style-name="L1"><text:list-item><text:p>乙</text:p>
            <text:list text:style-name="L1"><text:list-item><text:p>丙</text:p>
            <text:list text:style-name="L1"><text:list-item><text:p>丁</text:p></text:list-item></text:list>
            </text:list-item></text:list>
            </text:list-item></text:list>
            </text:list-item><text:list-item><text:p>戊</text:p></text:list-item></text:list>
            <text:p>列表之后的一段</text:p>
        </office:text></office:body></office:document-content>"#;
        let path = odt_parts(
            dir.path(),
            "列表.odt",
            &[("styles.xml", styles), ("content.xml", content)],
        );
        let body = body_of(&path);
        assert!(
            body.contains(concat!(
                "<ul>\n<li>甲<ol>\n<li>乙<ul style=\"list-style-type: circle\">\n",
                "<li>丙<ul style=\"list-style-type: square\">\n<li>丁</li>\n</ul>\n",
                "</li>\n</ul>\n</li>\n</ol>\n</li>\n<li>戊</li>\n</ul>"
            )),
            "四层各自的记数，与退层后回到第一层那一项：{body}"
        );
        assert!(
            body.contains("</ul>\n<p>列表之后的一段</p>"),
            "普通段落之前把列表关掉：{body}"
        );
        assert!(!body.contains("<p>甲"), "项不许再包一层段落：{body}");
        let seen = super::super::visible_text(&body);
        for want in ["甲", "乙", "丙", "丁", "戊", "列表之后的一段"] {
            assert!(seen.contains(want), "可见文本少了 {want}：{seen}");
        }
    }

    /// 样式名是空串、或那一层在样式里没有定义 → 圆点，不编一个号。
    ///
    /// 空样式名不是想象：LibreOffice 写的 `welcome.odt` 里那两个注解列表就是
    /// `text:list text:style-name=""`，而它们画的是项目符号。
    #[test]
    fn an_unstyled_list_is_a_bullet_not_an_invented_number() {
        let dir = tempfile::tempdir().unwrap();
        let styles = r#"<office:document-styles xmlns:office="o" xmlns:style="s" xmlns:text="t">
            <office:styles><text:list-style style:name="L1">
                <text:list-level-style-number text:level="1" style:num-format="1"/>
            </text:list-style></office:styles>
        </office:document-styles>"#;
        let content = r#"<office:document-content xmlns:office="o" xmlns:text="t"><office:body><office:text>
            <text:list text:style-name=""><text:list-item><text:p>没有样式名</text:p></text:list-item></text:list>
            <text:list text:style-name="L1"><text:list-item><text:p>一层有定义</text:p>
            <text:list text:style-name="L1"><text:list-item><text:p>二层没定义</text:p></text:list-item></text:list>
            </text:list-item></text:list>
            <text:list text:style-name="没有这一份样式"><text:list-item><text:p>样式不存在</text:p></text:list-item></text:list>
        </office:text></office:body></office:document-content>"#;
        let path = odt_parts(
            dir.path(),
            "认不出.odt",
            &[("styles.xml", styles), ("content.xml", content)],
        );
        let body = body_of(&path);
        assert!(
            body.contains("<ol>\n<li>一层有定义"),
            "样式说了十进制就写 `<ol>`：{body}"
        );
        assert_eq!(
            body.matches("<ul>").count(),
            3,
            "认不出的一律圆点（空样式名、层没定义、样式不存在）：{body}"
        );
        assert!(
            body.contains("<ul>\n<li>二层没定义"),
            "第二层没定义时不许把上一层的号续下来：{body}"
        );
    }

    /// `text:note` 是**内联**的：引用点在句子中间，注体也写在这里（与 DOCX 的
    /// 「注体在另一份部件里」正相反），所以要把外层那段寄存起来再续回去。
    ///
    /// 四条各自钉一个真会犯的错：外层段落被注体里那个 `text:p` 提前 flush（症状是
    /// 「正文」与「尾巴」被劈成两段）、号取 `text:note-citation` 而不是自己数、
    /// 注体摆回**它所在的那一章**末尾（弹层只在本章找 id）、以及注体为空时
    /// 一个字都不写（留一个点开没内容的上标看着就是界面坏了）。
    #[test]
    fn notes_are_anchored_in_place_and_their_bodies_land_at_the_chapter_tail() {
        let dir = tempfile::tempdir().unwrap();
        let content = r#"<office:document-content xmlns:office="o" xmlns:text="t"><office:body><office:text>
            <text:h text:outline-level="1">第一章</text:h>
            <text:p>正文<text:note text:id="ftn0" text:note-class="footnote"><text:note-citation>1</text:note-citation><text:note-body><text:p>注文甲</text:p><text:p>第二段</text:p></text:note-body></text:note>尾巴</text:p>
            <text:p>第二处<text:note text:id="etn0" text:note-class="endnote"><text:note-citation>i.</text:note-citation><text:note-body><text:p>尾注丙与 R&amp;D</text:p></text:note-body></text:note>。</text:p>
            <text:p>空注体<text:note text:id="ftn9" text:note-class="footnote"><text:note-citation>9</text:note-citation><text:note-body></text:note-body></text:note>。</text:p>
            <text:h text:outline-level="1">第二章</text:h>
            <text:p>干净的正文</text:p>
        </office:text></office:body></office:document-content>"#;
        let path = odt_parts(dir.path(), "注.odt", &[("content.xml", content)]);
        let (_, content) = OdtAdapter.read(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("ODT 该读成可重排");
        };
        assert_eq!(flow.chapters.len(), 2);
        let first = &flow.chapters[0].source();
        let second = &flow.chapters[1].source();
        assert!(
            first.contains(
                "<p>正文<sup><a epub:type=\"noteref\" href=\"#ftn0\">1</a></sup>尾巴</p>"
            ),
            "外层那段不能被注体劈开，引用落成原位的上标：{first}"
        );
        assert!(
            first.contains(r##"<sup><a epub:type="noteref" href="#etn0">i.</a></sup>"##),
            "号取 `text:note-citation` 自己写的那串（含 `.`）：{first}"
        );
        assert!(
            first.contains(r#"<aside epub:type="footnote" id="ftn0">注文甲 第二段</aside>"#),
            "两段注文用空格接住，摆回本章末尾：{first}"
        );
        assert!(
            first.contains(r#"<aside epub:type="footnote" id="etn0">尾注丙与 R&amp;D</aside>"#),
            "尾注同样摆回本章，实体要折回来：{first}"
        );
        assert!(
            !first.contains("#ftn9"),
            "注体为空的一条不许留下点开没有内容的上标：{first}"
        );
        assert!(
            !second.contains("aside"),
            "注文不许掉到没引用它的第二章：{second}"
        );
        // 引用与注文都要还能读得下去：标记那一个字符不算正文，但号要在。
        let seen = super::super::visible_text(first);
        assert!(seen.contains("正文1尾巴"), "号就在「正文」后面：{seen}");
    }

    /// 真 ODF 样本那一遍：`tests/fixtures/local/odt/` 里那几份（怎么取见那份 README）。
    ///
    /// 两份各自钉一件事：
    /// - `welcome.odt`（LibreOffice 写的）：正文里有 4 个 `text:list`，样式 `L3` 的第一层是
    ///   `text:bullet-char="➢"`（表外的自定义符号 → 圆点），`L4` 的第一层是
    ///   `style:num-format="1"`（十进制）—— 两族形状都要出现，且一个字都不许少。
    /// - `liste-courses.odt`：3 项的清单，样式 `L1` 前两层是 `•` / `◦`。这一份的正文在
    ///   `<text:list>` 里只有第一项是字面文本，另两项是模板变量，所以只钉形状不钉字数。
    ///
    /// **这一批样本里没有任何一份带 `text:note`**：注那一路由合成夹具那条门钉，
    /// 形状按 ODF 的 `text:note` / `text:note-citation` / `text:note-body` 三格写，
    /// 但「真生产者怎么摆」在这一条上没有被量过。
    ///
    /// 跑法：`cargo test -p folio-core --lib -- --ignored the_real_odf_documents`
    #[test]
    #[ignore = "要仓库外的真 ODF 样本：见 tests/fixtures/README.md 的 odt 那一节"]
    fn the_real_odf_documents_keep_their_lists() {
        let dir = Path::new("../../tests/fixtures/local/odt");
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(".odt"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        // 空目录会让下面每一条断言都**空转地通过**，所以先把它当一条门。
        assert!(!names.is_empty(), "{} 里一份样本都没有", dir.display());

        let welcome = body_of(&dir.join("welcome.odt"));
        assert!(
            welcome.contains("<ul>") && welcome.contains("<ol>"),
            "➢ 那一档落圆点、num-format=1 那一档落十进制：{welcome}"
        );
        assert!(
            welcome.contains("<li>"),
            "正文里那 8 个 `text:list-item` 要落成项：{welcome}"
        );
        assert!(
            welcome.contains("Go to"),
            "列表里的字一个都不许少：{welcome}"
        );

        let courses = body_of(&dir.join("liste-courses.odt"));
        assert_eq!(
            courses.matches("<li>").count(),
            3,
            "那一份就是 3 个 `text:list-item`：{courses}"
        );
        assert!(
            !courses.contains("<ol>"),
            "样式里全是 bullet，不许凭空出号：{courses}"
        );

        // 每一份都要读得动，且读完还有可见文本（列表与注这两刀不许把任何一本读空）。
        for name in &names {
            let body = body_of(&dir.join(name));
            assert!(
                super::super::visible_chars(&body).is_some_and(|count| count > 0),
                "{name} 的可见文本是空的"
            );
        }
    }

    #[test]
    fn a_zip_without_content_xml_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.odt");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("mimetype", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"application/vnd.oasis.opendocument.text")
            .unwrap();
        zip.finish().unwrap();
        assert!(OdtAdapter.read(&path).is_err());
    }
}
