//! WordprocessingML（`.docx`）：一个 ZIP 里的 `word/document.xml`。
//!
//! DOCX 不是电子书格式，但它与 EPUB 共享同一个容器家族：ZIP + XML + `media/` 里的
//! 图片。所以这里不另写一套阅读器，只做一件事 —— **把 WordprocessingML 归约成
//! 引擎就绪的 XHTML**，之后的排版、锚点、选中全部走现有链路。
//!
//! ## 归约到什么程度
//!
//! 一期只承诺读者真正会看的那几样：
//!
//! - `w:p` → `<p>`；`Title`/`Heading1` 起新章，`Heading2+` 写成 `<h2>`/`<h3>`
//! - `w:rPr` 的粗体 / 斜体 / 下划线 → `<strong>` / `<em>` / `<u>`
//! - `w:br` / `w:tab` → `<br/>` / 空格
//! - `<a:blip r:embed>` → `<img src="word/media/..."/>`（按 `document.xml.rels` 解）
//! - `w:numPr` + `word/numbering.xml` → `<ul>` / `<ol>`（嵌套与记数按 `w:ilvl` 与 `w:numFmt`）
//! - `w:footnoteReference` / `w:endnoteReference` → 上标锚，注体从 `word/footnotes.xml`
//!   与 `word/endnotes.xml` 取、摆回**引用它的那一章**末尾（脚注弹层的那条契约）
//! - 表格按行摊平，单元格用 `" | "` 连接
//!
//! 分栏、浮动图、域代码一律降级成普通段落 —— **不丢字**是底线，版式还原不是这一期的目标。
//!
//! 两样**故意没接**，各自的症状与理由写在用它的地方：样式表里靠 `w:style` 间接触发的
//! 编号（两个生产者都写在段落自己身上，见 `Para::num`），以及 `w:numRestart` 那种
//! 每节从头再数的编号。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use super::zip::Zip;
use super::{
    Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, FlowDoc, Format, ImageRes,
    ListWriter, Marker, image_dims, media_type_of, resolve_href, stem_of,
};
use crate::Result;

pub struct DocxAdapter;

impl DocumentProvider for DocxAdapter {
    fn format(&self) -> Format {
        Format::Docx
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(super::extension_of(path).as_deref(), Some("docx"))
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
                // 目录不在这里造：DOCX 的标题层级已经落成章表，界面的「没有目录就退回章节表」
                // 那条兜底给出的正是同一份东西，另造一份只会多一处漂移。
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

/// 一次解完：书目、章表、图片。
fn read_book(path: &Path) -> Result<Book> {
    let mut zip = Zip::open(path)?;
    let core = parse_core(zip.read_text("docProps/core.xml").as_deref().unwrap_or(""));
    let rels = parse_rels(
        zip.read_text("word/_rels/document.xml.rels")
            .as_deref()
            .unwrap_or(""),
    );
    let xml = zip
        .read_text("word/document.xml")
        .ok_or_else(|| unsupported(path, "ZIP 里没有 word/document.xml"))?;
    // 列表与脚注各是**另一份部件**：正文里只有引用（`w:numPr` 的两个数、
    // `w:footnoteReference` 的 id），定义与注体在别处。缺文件不是错误 —— 那只是这本没有。
    let numbering = parse_numbering(zip.read_text("word/numbering.xml").as_deref().unwrap_or(""));
    let mut notes = parse_notes(
        zip.read_text("word/footnotes.xml").as_deref().unwrap_or(""),
        "fn",
    );
    notes.extend(parse_notes(
        zip.read_text("word/endnotes.xml").as_deref().unwrap_or(""),
        "en",
    ));

    let stem = stem_of(path);
    let title = core
        .title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(stem);
    let meta = DocMeta {
        title: title.clone(),
        authors: core.creators,
        language: core.language,
        description: core.description,
        format: Format::Docx,
        size: std::fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
        ..DocMeta::default()
    };

    let mut builder = Builder::new(title, numbering, notes);
    builder.parse(&xml, &rels);
    builder.finish();
    if builder
        .chapters
        .iter()
        .all(|(_, body)| body.trim().is_empty())
    {
        return Err(unsupported(path, "word/document.xml 里没有正文"));
    }
    let images = builder.images(&mut zip, &rels);
    Ok(Book {
        meta,
        chapters: builder.chapters,
        images,
    })
}

#[derive(Default)]
struct Core {
    title: Option<String>,
    creators: Vec<String>,
    language: String,
    description: String,
}

/// `docProps/core.xml`：`dc:title` / `dc:creator` / `dc:language` / `dc:description`。
fn parse_core(xml: &str) -> Core {
    let mut out = Core::default();
    let mut reader = Reader::from_str(xml);
    let mut want: Option<&str> = None;
    let mut buf = String::new();
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) => {
                let name = local_name(e.name().as_ref());
                want = match name.as_str() {
                    "title" => Some("title"),
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
                        "title" if !value.is_empty() && out.title.is_none() => {
                            out.title = Some(value);
                        }
                        "creator" if !value.is_empty() => out.creators.push(value),
                        "language" => out.language = value,
                        "description" => out.description = value,
                        _ => {}
                    }
                }
                want = None;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

#[derive(Clone, Debug)]
struct Rel {
    target: String,
    external: bool,
}

/// `word/_rels/document.xml.rels`：`Id → Target`。
fn parse_rels(xml: &str) -> HashMap<String, Rel> {
    let mut out = HashMap::new();
    let mut reader = Reader::from_str(xml);
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) | Event::Empty(e) => {
                if local_name(e.name().as_ref()) != "Relationship" {
                    continue;
                }
                let Some(id) = attr(&e, "Id") else {
                    continue;
                };
                let Some(target) = attr(&e, "Target") else {
                    continue;
                };
                let external = attr(&e, "TargetMode")
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("External"));
                out.insert(id, Rel { target, external });
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// `word/numbering.xml`：一份编号定义。
///
/// 正文里的 `w:numPr` 只交两个数（`w:ilvl` 层号与 `w:numId` 实例号），**记数的方式在这份
/// 文件里**：`w:num` 说实例指向哪个抽象定义，`w:abstractNum` 里那一层的 `w:numFmt` 才是
/// 「decimal 还是 bullet」。两张表都要，跳一层就认不出号。
///
/// `w:lvlText` 那种模板（真样本 `numbering.docx` 里同时有 `Section %1.` 与 `%1` 两种写法）
/// **不接**：标记由 `layout::marker` 按记数折出来，模板画不出来 —— 取的是同一条 `w:lvl`
/// 里的 `w:numFmt`。代价是「Section 1.」变成「1.」，读得懂，只是少了那几个字。
#[derive(Default)]
struct Numbering {
    /// `w:num` 的 `numId → w:abstractNumId`。
    instances: HashMap<String, u32>,
    /// `(abstractNumId, ilvl) → 这一层的记数`。
    levels: HashMap<(u32, u32), Marker>,
    /// `(numId, ilvl) → 这一层的记数`：`w:lvlOverride` 覆盖抽象定义那一格
    /// （真样本 `override.docx` 就是这一种：同一个抽象定义被第三个实例改写了第一层）。
    overrides: HashMap<(String, u32), Marker>,
}

impl Numbering {
    /// 这一项该画什么号。
    ///
    /// 认不出来的那几档（`numId` 在 numbering.xml 里没有、层号超出定义、整份文件缺失）
    /// 一律 [`Marker::Nothing`]：缩进留着、号不画。凭空给一个圆点或一个「1.」是编造，
    /// 而编出来的号会被读者当成书里真有这个东西。
    fn marker(&self, num_id: &str, ilvl: u32) -> Marker {
        if let Some(marker) = self.overrides.get(&(num_id.to_owned(), ilvl)) {
            return *marker;
        }
        let Some(abstract_id) = self.instances.get(num_id).copied() else {
            return Marker::Nothing;
        };
        self.levels
            .get(&(abstract_id, ilvl))
            .copied()
            .unwrap_or(Marker::Nothing)
    }
}

fn parse_numbering(xml: &str) -> Numbering {
    let mut out = Numbering::default();
    let mut reader = Reader::from_str(xml);
    // 「现在正在读的那几格」。这份文件只有三层嵌套，用几个局部变量比建一棵树便宜。
    let mut current_abstract: Option<u32> = None;
    let mut current_instance: Option<String> = None;
    let mut overriding = false;
    let mut level: Option<u32> = None;
    let mut fmt: Option<Marker> = None;
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) | Event::Empty(e) => {
                let name = local_name(e.name().as_ref());
                match name.as_str() {
                    "abstractNum" => {
                        current_abstract =
                            attr(&e, "abstractNumId").and_then(|value| value.parse().ok());
                    }
                    "num" => current_instance = attr(&e, "numId"),
                    "lvlOverride" => overriding = true,
                    "lvl" => {
                        level = attr(&e, "ilvl").and_then(|value| value.parse().ok());
                        fmt = None;
                    }
                    // 这两格在生产者笔下是空元素，但 `<w:numFmt w:val="x"></w:numFmt>`
                    // 也合法，所以 Start 与 Empty 两条都收。
                    "numFmt" => {
                        if let Some(value) = attr(&e, "val") {
                            fmt = Some(marker_of_num_fmt(&value));
                        }
                    }
                    "abstractNumId" => {
                        if let (Some(instance), Some(value)) =
                            (current_instance.as_deref(), attr(&e, "val"))
                            && let Ok(abstract_id) = value.parse::<u32>()
                        {
                            out.instances.insert(instance.to_string(), abstract_id);
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                let name = local_name(e.name().as_ref());
                match name.as_str() {
                    "abstractNum" => current_abstract = None,
                    "num" => current_instance = None,
                    "lvlOverride" => overriding = false,
                    "lvl" => {
                        // 层号读不到就丢掉这一格：没有 ilvl 的 `w:lvl` 对不上任何段落。
                        if let Some(ilvl) = level.take() {
                            let marker = fmt.take().unwrap_or(Marker::Nothing);
                            match (overriding, current_instance.as_deref(), current_abstract) {
                                (true, Some(instance), _) => {
                                    out.overrides.insert((instance.to_string(), ilvl), marker);
                                }
                                (false, _, Some(abstract_id)) => {
                                    out.levels.insert((abstract_id, ilvl), marker);
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// `w:numFmt` 那一格 → [`Marker`]。
///
/// 下划线与连字符都先抹掉再比（两种写法在生产者笔下都见过：`chineseCountingThousand`
/// 与 `chinese_counting_thousand`）。表外的一律 `Ordered(None)`（十进制），理由写在
/// [`Marker`] 的注释里。
fn marker_of_num_fmt(value: &str) -> Marker {
    let key: String = value
        .chars()
        .filter(|c| *c != '-' && *c != '_')
        .collect::<String>()
        .to_ascii_lowercase();
    match key.as_str() {
        "bullet" => Marker::Disc,
        "none" => Marker::Nothing,
        "lowerletter" => Marker::Ordered(Some("lower-alpha")),
        "upperletter" => Marker::Ordered(Some("upper-alpha")),
        "lowerroman" => Marker::Ordered(Some("lower-roman")),
        "upperroman" => Marker::Ordered(Some("upper-roman")),
        "chinesecounting" | "chinesecountingthousand" | "chineselegalsimplified" => {
            Marker::Ordered(Some("cjk-ideographic"))
        }
        _ => Marker::Ordered(None),
    }
}

/// `word/footnotes.xml` 与 `word/endnotes.xml`：`前缀+w:id` → 注体的 XHTML。
///
/// 那三种挂着 `w:type` 的格子（`separator` / `continuationSeparator` / `continuationNotice`）
/// 不是注文，是 Word 画页底那条线用的，必须跳过。**判的是 `w:type` 在不在，不是 id 的
/// 正负或大小**：真样本 `without_numid.docx` 里那三种正好占着 id `-1` / `0` / `1`，而
/// `footnotes.docx` 里那条真注文的 id 也是 `1` —— 照 id 判会把第一条注文当分隔符丢掉。
///
/// 注体里不带的两样：字符样式（注文本来就是小字，弹层按正文排）与图片（`w:blip` 的引用
/// 在 `document.xml.rels`，注体那一份另有 rels）。段与段之间用空格接，与表格格子同一条口径。
fn parse_notes(xml: &str, prefix: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut reader = Reader::from_str(xml);
    // 正在收的那一条的键（`None` = 不在注体里）与它是不是那三种图形格子。
    let mut key: Option<String> = None;
    let mut skip = false;
    let mut body = String::new();
    let mut in_text = false;
    let mut text = String::new();
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) => {
                let name = local_name(e.name().as_ref());
                match name.as_str() {
                    "footnote" | "endnote" => {
                        match attr(&e, "id") {
                            Some(id) if attr(&e, "type").is_none() => {
                                key = Some(format!("{prefix}{}", escape_attr(&id)));
                            }
                            _ => {
                                key = None;
                                skip = true;
                            }
                        }
                        body.clear();
                    }
                    "p" => {
                        if key.is_some() && !body.is_empty() {
                            body.push(' ');
                        }
                    }
                    "t" if key.is_some() && !skip => {
                        in_text = true;
                        text.clear();
                    }
                    _ => {}
                }
            }
            Event::Empty(e) => {
                // `w:br` 在注体里是真的换行（诗的注文整条就是靠它分行）。
                if key.is_some() && !skip && matches!(local_name(e.name().as_ref()).as_str(), "br")
                {
                    body.push_str("<br/>");
                }
            }
            Event::Text(e) if in_text => super::xml_piece(&mut text, e.as_ref(), false),
            Event::GeneralRef(e) if in_text => super::xml_piece(&mut text, e.as_ref(), true),
            Event::End(e) => {
                let name = local_name(e.name().as_ref());
                match name.as_str() {
                    "t" => {
                        in_text = false;
                        escape_into(&mut body, &std::mem::take(&mut text));
                    }
                    "footnote" | "endnote" => {
                        let html = body.trim().to_string();
                        body.clear();
                        in_text = false;
                        text.clear();
                        let was_skipped = std::mem::replace(&mut skip, false);
                        match (key.take(), was_skipped) {
                            (Some(key), false) if !html.is_empty() => {
                                out.insert(key, html);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// 段落 / run 的累加状态。解析器只认这一层需要的格子，不建完整 DOM。
struct Builder {
    meta_title: String,
    chapters: Vec<(String, String)>,
    current_title: Option<String>,
    body: String,
    para: Option<Para>,
    run: RunStyle,
    in_text: bool,
    text: String,
    link_open: bool,
    in_cell: bool,
    cell: String,
    row: Vec<String>,
    used_images: HashSet<String>,
    image_order: Vec<String>,
    /// `word/numbering.xml` 那两张表（见 [`parse_numbering`]）。
    numbering: Numbering,
    /// `w:id → 注体`，脚注与尾注共用一张表（键带 `fn` / `en` 前缀，所以两种的 id
    /// 各自从 1 起也不撞）。
    notes: HashMap<String, String>,
    /// 本章引用过、还没摆出去的注：`(键, 屏上的号)`，按引用序。
    cited: Vec<(String, u32)>,
    /// 全文连续的编号：屏上的号与纸上的号按出现序数出来（`w:numRestart` 不读，理由写在
    /// [`Builder::emit_note`]）。
    footnote_count: u32,
    endnote_count: u32,
    /// 正开着的那几层列表。
    lists: ListWriter,
    /// 正在读的 `w:numPr` 里那两格（层号与实例号）；两个都齐了才算一项。
    pending_num: Option<(Option<u32>, Option<String>)>,
}

struct Para {
    style: Option<String>,
    html: String,
    /// 这一段挂在第几层的哪个编号实例上（`w:ilvl` + `w:numId`）。
    ///
    /// 两个生产者（Word 与 LibreOffice 导出的 docx）都把 `numPr` **写在段落自己身上**，
    /// 所以没去 `word/styles.xml` 找样式间接的那一层：真样本里那几份的 `List*` 样式只是
    /// 出厂定义，正文一个都没引用。真撞上「只有样式编号」的书，症状是列表读成普通段落
    /// —— 字一个不少，只是没号。
    num: Option<(u32, String)>,
}

#[derive(Default, Clone, Copy)]
struct RunStyle {
    bold: bool,
    italic: bool,
    underline: bool,
}

impl Builder {
    fn new(meta_title: String, numbering: Numbering, notes: HashMap<String, String>) -> Self {
        Self {
            meta_title,
            chapters: Vec::new(),
            current_title: None,
            body: String::new(),
            para: None,
            run: RunStyle::default(),
            in_text: false,
            text: String::new(),
            link_open: false,
            in_cell: false,
            cell: String::new(),
            row: Vec::new(),
            used_images: HashSet::new(),
            image_order: Vec::new(),
            numbering,
            notes,
            cited: Vec::new(),
            footnote_count: 0,
            endnote_count: 0,
            lists: ListWriter::default(),
            pending_num: None,
        }
    }

    fn parse(&mut self, xml: &str, rels: &HashMap<String, Rel>) {
        let mut reader = Reader::from_str(xml);
        while let Ok(event) = reader.read_event() {
            match event {
                Event::Start(e) => {
                    let name = local_name(e.name().as_ref());
                    match name.as_str() {
                        "p" => {
                            self.para.get_or_insert_with(|| Para {
                                style: None,
                                html: String::new(),
                                num: None,
                            });
                            self.run = RunStyle::default();
                        }
                        "numPr" => self.pending_num = Some((None, None)),
                        "ilvl" | "numId" => self.read_num_pr(&name, &e),
                        "footnoteReference" | "endnoteReference" => {
                            self.emit_note(name == "endnoteReference", &e)
                        }
                        "r" => self.run = RunStyle::default(),
                        "b" => self.run.bold = style_on(&e),
                        "i" => self.run.italic = style_on(&e),
                        "u" => self.run.underline = style_on(&e),
                        "t" => {
                            self.in_text = true;
                            self.text.clear();
                        }
                        "br" => self.push_html("<br/>"),
                        "tab" => self.push_html(" "),
                        "blip" => {
                            if let Some(id) = attr(&e, "embed") {
                                self.emit_image(&id, rels);
                            }
                        }
                        "hyperlink" => {
                            if let Some(target) = attr(&e, "id")
                                .and_then(|id| rels.get(&id))
                                .filter(|rel| rel.external)
                                .map(|rel| rel.target.clone())
                            {
                                self.push_html(&format!("<a href=\"{}\">", escape_attr(&target)));
                                self.link_open = true;
                            }
                        }
                        "tc" => {
                            self.in_cell = true;
                            self.cell.clear();
                        }
                        "tr" => self.row.clear(),
                        _ => {}
                    }
                }
                Event::Empty(e) => {
                    let name = local_name(e.name().as_ref());
                    match name.as_str() {
                        "pStyle" => {
                            if let Some(para) = self.para.as_mut() {
                                para.style = attr(&e, "val");
                            }
                        }
                        "b" => self.run.bold = style_on(&e),
                        "i" => self.run.italic = style_on(&e),
                        "u" => self.run.underline = style_on(&e),
                        "br" => self.push_html("<br/>"),
                        "tab" => self.push_html(" "),
                        "blip" => {
                            if let Some(id) = attr(&e, "embed") {
                                self.emit_image(&id, rels);
                            }
                        }
                        "ilvl" | "numId" => self.read_num_pr(&name, &e),
                        "footnoteReference" | "endnoteReference" => {
                            self.emit_note(name == "endnoteReference", &e)
                        }
                        _ => {}
                    }
                }
                // `&amp;` 在 0.42 里**单独成一个事件**，只吃 `Text` 的写法会把这一个字符
                // 整颗丢掉，而 `w:t` 里 `R&D` 这类写法很常见（见 `super::xml_piece`）。
                Event::Text(e) if self.in_text => {
                    super::xml_piece(&mut self.text, e.as_ref(), false)
                }
                Event::GeneralRef(e) if self.in_text => {
                    super::xml_piece(&mut self.text, e.as_ref(), true)
                }
                Event::End(e) => {
                    let name = local_name(e.name().as_ref());
                    match name.as_str() {
                        "t" => {
                            self.in_text = false;
                            let text = std::mem::take(&mut self.text);
                            self.push_run(&text);
                        }
                        "r" => self.run = RunStyle::default(),
                        "hyperlink" if self.link_open => {
                            self.push_html("</a>");
                            self.link_open = false;
                        }
                        "p" => self.finish_paragraph(),
                        "numPr" => self.commit_num_pr(),
                        "tc" if self.in_cell => {
                            let cell = self.cell.trim().to_string();
                            if !cell.is_empty() {
                                self.row.push(cell);
                            }
                            self.in_cell = false;
                        }
                        "tr" if !self.row.is_empty() => {
                            let row = std::mem::take(&mut self.row);
                            self.push_block(&format!("<p>{}</p>", row.join(" | ")));
                        }
                        _ => {}
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
    }

    fn push_run(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut escaped = String::new();
        escape_into(&mut escaped, text);
        if self.run.italic {
            escaped = format!("<em>{escaped}</em>");
        }
        if self.run.bold {
            escaped = format!("<strong>{escaped}</strong>");
        }
        if self.run.underline {
            escaped = format!("<u>{escaped}</u>");
        }
        self.push_html(&escaped);
    }

    fn emit_image(&mut self, id: &str, rels: &HashMap<String, Rel>) {
        let Some(rel) = rels.get(id).filter(|rel| !rel.external) else {
            return;
        };
        let href = resolve_rel(&rel.target);
        if self.used_images.insert(href.clone()) {
            self.image_order.push(href.clone());
        }
        self.push_html(&format!("<img src=\"{}\"/>", escape_attr(&href)));
    }

    fn push_html(&mut self, html: &str) {
        if let Some(para) = self.para.as_mut() {
            para.html.push_str(html);
        }
    }

    fn push_block(&mut self, html: &str) {
        if self.in_cell {
            self.cell.push_str(html);
        } else {
            self.body.push_str(html);
            self.body.push('\n');
        }
    }

    fn finish_paragraph(&mut self) {
        let Some(para) = self.para.take() else {
            return;
        };
        let html = para.html.trim().to_string();
        if html.is_empty() {
            return;
        }
        // 格子里的段落**不再包 `<p>`**：行那一格已经把整行包成一个 `<p>`（`" | "` 拼格子），
        // 段落再套一层就成了 `<p><p>…</p></p>`。真样本上第一次撞见是在 Word 的表格里，
        // 而 html5ever 见到 `<p>` 里开 `<p>` 会当场把外层关掉 —— 屏上表现为那一行被劈成两段。
        // 一个格子里几段就用空格接：这一族本来就没有行盒（`<table>` 未认），
        // 拼接口径要的是「一行读得下去」，不是保住段落边界。
        if self.in_cell {
            if !self.cell.is_empty() {
                self.cell.push(' ');
            }
            self.cell.push_str(&html);
            return;
        }
        let style = para.style.as_deref().unwrap_or_default();
        if is_title(style) {
            let title = super::visible_text(&html).trim().to_string();
            if !title.is_empty() {
                self.flush_chapter();
                self.current_title = Some(title.clone());
                self.body
                    .push_str(&format!("<h1>{}</h1>\n", escape_text(&title)));
                return;
            }
        }
        if let Some(level) = heading_level(style)
            && level > 1
        {
            self.close_lists();
            self.push_block(&format!("<h{level}>{html}</h{level}>"));
            return;
        }
        // 列表那一条要先判：Word 允许给列表项挂样式（`ListParagraph`），若先走
        // 标题那两格或普通段落那一支，这一项就再也不是项了。
        if let Some((ilvl, num_id)) = para.num {
            let marker = self.numbering.marker(&num_id, ilvl);
            self.lists
                .write(ilvl as usize, &num_id, marker, &html, &mut self.body);
            return;
        }
        self.close_lists();
        self.push_block(&format!("<p>{html}</p>"));
    }

    /// `w:numPr` 里读到一半的两格（`w:ilvl` + `w:numId`）落到当前段落上。
    fn read_num_pr(&mut self, name: &str, start: &BytesStart<'_>) {
        let Some((level, id)) = self.pending_num.as_mut() else {
            return;
        };
        let value = attr(start, "val");
        match name {
            "ilvl" => *level = value.and_then(|value| value.parse().ok()),
            _ => *id = value,
        }
    }

    /// 把 `w:numPr` 收完的那两格挂到段落上。**两格都齐**才算一项：
    /// 只有 `w:ilvl`（缺 `w:numId`，即「继承但这一份里没有」）不是列表，
    /// 而 `without_numid.docx` 那份真样本就叫这个名字。
    fn commit_num_pr(&mut self) {
        let Some((level, id)) = self.pending_num.take() else {
            return;
        };
        match (level, id) {
            (Some(ilvl), Some(num_id)) if !num_id.is_empty() => {
                if let Some(para) = self.para.as_mut() {
                    para.num = Some((ilvl, num_id));
                }
            }
            _ => {}
        }
    }

    /// 正文里那一个引用标记 → 上标 + 锚；注体留在 `notes` 里，等这一章落定时摆到末尾。
    ///
    /// 号按**文档出现序**数（Word 默认全文连续编号）。`w:numRestart`（每节从头再数）不读：
    /// 那几份真样本里没有这一格，而猜错的症状是「屏上的号与纸上的号对不上」——
    /// 连续数出来的号至少自洽，锚也照样指到那一条注。
    ///
    /// `notes` 里没有这一格（注体缺失，或那三种图形格子）就**一个字都不写**：
    /// 留一个点开来没有内容的上标，比少一个标记更像一个错。
    fn emit_note(&mut self, endnote: bool, start: &BytesStart<'_>) {
        let Some(raw) = attr(start, "id") else {
            return;
        };
        let id = format!("{}{}", if endnote { "en" } else { "fn" }, escape_attr(&raw));
        if !self.notes.contains_key(&id) {
            return;
        }
        // 同一条注被引用第二次（Word 的「交叉引用脚注」就是这么写的）：号不变，
        // 注体也只摆一份 —— 一个 `id` 在同一章里出现两次，弹层解到的就是后那一个。
        let already = self
            .cited
            .iter()
            .find(|(cited, _)| *cited == id)
            .map(|(_, label)| *label);
        let label = match already {
            Some(label) => label,
            None => {
                let next = if endnote {
                    self.endnote_count += 1;
                    self.endnote_count
                } else {
                    self.footnote_count += 1;
                    self.footnote_count
                };
                self.cited.push((id.clone(), next));
                next
            }
        };
        // 属性名写的是 `epub:type`：脚注弹层那一头认的就是它（`chapter_layout` 的
        // `footnote_anchor_is_ref` / `is_footnote_body`），与 EPUB 书共用一条解析链。
        self.push_html(&format!(
            "<sup><a epub:type=\"noteref\" href=\"#{id}\">{label}</a></sup>"
        ));
    }

    /// 关掉所有开着的列表层。换普通段落、换标题、换章都要先走这一步，
    /// 否则 `</ul>` 会掉进下一章的正文里。
    fn close_lists(&mut self) {
        self.lists.close_all(&mut self.body);
    }

    fn flush_chapter(&mut self) {
        self.close_lists();
        // 引用过的注摆在这一章末尾：弹层那一条链只在本章里找 `id`
        // （`footnote_body_text`），摆到别章去就等于没有。
        for (id, _) in std::mem::take(&mut self.cited) {
            let Some(html) = self.notes.get(&id) else {
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
        self.flush_chapter();
        if self.chapters.is_empty() {
            // 整篇没有标题、也没有段落：给一个空章，调用方会把它当「读不出内容」拒掉。
            self.chapters.push((self.meta_title.clone(), String::new()));
        }
    }

    /// 把 `r:embed` 用到的关系解析成真正的图片元数据。读不出字节的那几张不登记。
    fn images(&self, zip: &mut Zip, rels: &HashMap<String, Rel>) -> Vec<ImageRes> {
        let mut out = Vec::new();
        for id in &self.image_order {
            let Some(rel) = rels
                .values()
                .find(|rel| !rel.external && resolve_rel(&rel.target) == id.as_str())
            else {
                continue;
            };
            let href = resolve_rel(&rel.target);
            let Some(bytes) = zip.read(&href) else {
                continue;
            };
            let ext = super::extension_of(Path::new(&href)).unwrap_or_default();
            let (width, height) = image_dims(&bytes);
            out.push(ImageRes {
                href: href.clone(),
                source: href,
                media_type: media_type_of(&ext).to_string(),
                width,
                height,
            });
        }
        out
    }
}

fn chapter_images(body: &str, known: &[ImageRes]) -> Vec<crate::content::scan::ImageUse> {
    let known: HashSet<&str> = known.iter().map(|image| image.href.as_str()).collect();
    crate::content::scan::chapter_summary(body)
        .images
        .into_iter()
        .filter(|image| known.contains(image.src.as_str()))
        .collect()
}

fn resolve_rel(target: &str) -> String {
    let target = target.trim();
    if target.starts_with('/') {
        resolve_href("", target)
    } else {
        // `document.xml.rels` 的 Target 相对 `word/`。
        resolve_href("word/", target)
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

/// `<w:b w:val="0"/>` 是「关」，缺省的 `<w:b/>` 是「开」。
fn style_on(start: &BytesStart<'_>) -> bool {
    attr(start, "val").is_none_or(|value| {
        !(value.eq_ignore_ascii_case("0")
            || value.eq_ignore_ascii_case("false")
            || value.eq_ignore_ascii_case("none"))
    })
}

fn is_title(style: &str) -> bool {
    style.eq_ignore_ascii_case("Title") || heading_level(style) == Some(1)
}

fn heading_level(style: &str) -> Option<usize> {
    let lower = style.to_ascii_lowercase();
    let digits = lower.strip_prefix("heading")?.trim();
    digits.parse().ok()
}

fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text);
    out
}

fn escape_into(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

fn escape_attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text);
    out.replace('"', "&quot;")
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

    use super::{DocxAdapter, chapter_images, read_book};
    use crate::content::format::{Content, DocumentProvider, Format};

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([9, 8, 7, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    fn docx(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("书.docx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            (
                "[Content_Types].xml",
                br#"<Types><Default Extension="xml" ContentType="application/xml"/></Types>"#
                    .to_vec(),
            ),
            (
                "docProps/core.xml",
                r#"<cp:coreProperties xmlns:cp="x" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>样书</dc:title><dc:creator>作者甲</dc:creator><dc:language>zh-CN</dc:language></cp:coreProperties>"#
                    .as_bytes()
                    .to_vec(),
            ),
            (
                "word/_rels/document.xml.rels",
                br#"<Relationships xmlns="x"><Relationship Id="rId1" Type="image" Target="media/pic.png"/></Relationships>"#
                    .to_vec(),
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="x" xmlns:a="a" xmlns:r="r"><w:body>
                    <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>第一章</w:t></w:r></w:p>
                    <w:p><w:r><w:rPr><w:b/><w:i/></w:rPr><w:t>正文甲</w:t></w:r><w:r><w:t xml:space="preserve"> 正文乙</w:t></w:r></w:p>
                    <w:p><w:r><w:drawing><a:blip r:embed="rId1"/></w:drawing></w:r></w:p>
                    <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>第二章</w:t></w:r></w:p>
                    <w:p><w:r><w:t>正文丙</w:t></w:r></w:p>
                </w:body></w:document>"#
                    .as_bytes()
                    .to_vec(),
            ),
            ("word/media/pic.png", png(40, 30)),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(&body).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    /// 一份带表格的 docx：形状照 Word 写出来的样子（格子里是完整的 `<w:p>`，
    /// 段落带 `Heading1` 样式的是**表格题注**那一类，不是章）。
    fn docx_with_table(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("表格.docx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            (
                "[Content_Types].xml",
                br#"<Types><Default Extension="xml" ContentType="application/xml"/></Types>"#
                    .to_vec(),
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="x"><w:body>
                    <w:p><w:r><w:t>前言</w:t></w:r></w:p>
                    <w:tbl>
                      <w:tr>
                        <w:tc><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>甲</w:t></w:r></w:p></w:tc>
                        <w:tc><w:p><w:r><w:t>乙</w:t></w:r></w:p><w:p><w:r><w:t>乙二</w:t></w:r></w:p></w:tc>
                      </w:tr>
                      <w:tr>
                        <w:tc><w:p><w:r><w:t>丙</w:t></w:r></w:p></w:tc>
                        <w:tc><w:p><w:r><w:t>丁</w:t></w:r></w:p></w:tc>
                      </w:tr>
                    </w:tbl>
                </w:body></w:document>"#
                    .as_bytes()
                    .to_vec(),
            ),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(&body).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    /// `w:t` 里的实体引用单独成一个事件，只吃 `Text` 的写法会把 `&` 整颗丢掉，
    /// 而那一颗是**正文的字符**（`R&D`、`Tom & Jerry` 这类写法在真文档里常见）。
    /// 书目那一格同一件事：`docProps/core.xml` 里的 `&amp;` 也走同一条路。
    #[test]
    fn entity_references_are_characters_not_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("实体.docx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            (
                "[Content_Types].xml",
                br#"<Types><Default Extension="xml" ContentType="application/xml"/></Types>"#
                    .to_vec(),
            ),
            (
                "docProps/core.xml",
                r#"<cp:coreProperties xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>R&amp;D 年报</dc:title></cp:coreProperties>"#
                    .as_bytes()
                    .to_vec(),
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="x"><w:body>
                    <w:p><w:r><w:t>R&amp;D 与 Tom&#38;Jerry</w:t></w:r></w:p>
                </w:body></w:document>"#
                    .as_bytes()
                    .to_vec(),
            ),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(&body).unwrap();
        }
        zip.finish().unwrap();

        let (meta, content) = DocxAdapter.read(&path).unwrap();
        assert_eq!(meta.title, "R&D 年报", "core.xml 的实体");
        let Content::Reflow(flow) = content else {
            panic!("DOCX 该读成可重排");
        };
        // 断言打在**可见文本**上：引擎就绪的那一份是 XHTML，`&` 本来就该写作 `&amp;`，
        // 要查的是「这一个字符没有被丢掉」，而不是「串里没有转义符」。
        let seen = super::super::visible_text(flow.chapters[0].source());
        assert_eq!(seen.trim(), "R&D 与 Tom&Jerry", "正文里的两种实体都要落地");
    }

    #[test]
    fn headings_become_chapters_and_runs_keep_formatting() {
        let dir = tempfile::tempdir().unwrap();
        let path = docx(dir.path());
        let (meta, content) = DocxAdapter.read(&path).unwrap();
        assert_eq!(meta.format, Format::Docx);
        assert_eq!(meta.title, "样书");
        assert_eq!(meta.authors, vec!["作者甲"]);
        let Content::Reflow(flow) = content else {
            panic!("DOCX 该读成可重排");
        };
        assert_eq!(flow.chapters.len(), 2, "两个 Heading1 各起一章");
        assert_eq!(flow.chapters[0].title, "第一章");
        assert!(
            flow.chapters[0]
                .source()
                .contains("<strong><em>正文甲</em></strong>")
        );
        assert!(flow.chapters[0].source().contains(" 正文乙"), "空格要保留");
        assert!(flow.chapters[1].source().contains("正文丙"));
        assert_eq!(flow.images.len(), 1);
        assert_eq!(flow.images[0].href, "word/media/pic.png");
        assert_eq!((flow.images[0].width, flow.images[0].height), (40, 30));
        assert_eq!(
            chapter_images(flow.chapters[0].source(), &flow.images).len(),
            1
        );
    }

    /// Word 的表格：每个格子里都是**一整段**（`<w:tc><w:p>…`），而一行拼成一个 `<p>`。
    ///
    /// 这条门是被真样本叫出来的（`sct-inner-content.docx` / `blk-inner-content.docx` 都是
    /// Word 写的表格）：格子内那一段以前也过一次 `finish_paragraph`，于是行外面套 `<p>`、
    /// 里面又有一个 `<p>`，交出去的是 `<p><p>T2</p></p>`。html5ever 见到 `<p>` 里开 `<p>`
    /// 会当场关掉外层 —— 表现是那一行被劈成两段，而表格本来就没有行盒，这一刀只亏不赚。
    /// 顺带钉第二条：格子里挂着 `Heading1` 样式也不许起一章。
    #[test]
    fn table_cells_do_not_nest_paragraphs_or_start_chapters() {
        let dir = tempfile::tempdir().unwrap();
        let path = docx_with_table(dir.path());
        let (_, content) = DocxAdapter.read(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("DOCX 该读成可重排");
        };
        assert_eq!(flow.chapters.len(), 1, "格子里那个 Heading1 不算一章");
        let body = flow.chapters[0].source();
        assert!(!body.contains("<p><p"), "行里不许再套段落：{body}");
        assert!(body.contains("<p>甲 | 乙 乙二</p>"), "一行读成一行：{body}");
        assert!(body.contains("丙 | 丁"), "第二行：{body}");
    }

    #[test]
    fn the_index_keeps_text_out_and_positions_by_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = docx(dir.path());
        let (_, content) = DocxAdapter.index(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("DOCX 该读成可重排");
        };
        assert_eq!(flow.chapters.len(), 2);
        assert!(
            flow.chapters
                .iter()
                .all(|chapter| chapter.source().is_empty())
        );
        assert_eq!(
            flow.chapters[1].char_len(),
            Some(
                super::super::visible_chars(
                    &DocxAdapter
                        .chapter_source(&path, flow.chapters[1].locator().unwrap())
                        .unwrap()
                )
                .unwrap()
            )
        );
    }

    /// 一份部件完全由调用方摆的 docx（列表、脚注那两族要的是**多份部件之间的引用**，
    /// [`docx`] 那个写死的夹具扩不动）。
    fn docx_parts(dir: &Path, name: &str, parts: &[(&str, &str)]) -> std::path::PathBuf {
        let path = dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(
            br#"<Types><Default Extension="xml" ContentType="application/xml"/></Types>"#,
        )
        .unwrap();
        for (part, xml) in parts {
            zip.start_file(*part, opts).unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    /// 列表：`w:numPr` 交的是**层号 + 实例号**，记数在 `word/numbering.xml` 那一格。
    ///
    /// 形状照真样本 `numbering.docx` 写（那份有 `decimal` / `lowerRoman` / `none` 三种抽象
    /// 定义，正文里 5 个段落挂 `w:ilvl` + `w:numId`）。这里钉四件事：
    /// `<ol>` / `<ul>` 各归各的、`w:ilvl` 1 落成真嵌套（子列表长在那个还开着的 `<li>` 里）、
    /// 同一层换记数就关掉重开（号跟着从头数，Word 换 `numId` 就是这个意思）、
    /// 以及 `w:numFmt` 那一格落到 `list-style-type` 上而不是被吞掉。
    #[test]
    fn numbered_lists_come_out_nested_and_renumbered() {
        let dir = tempfile::tempdir().unwrap();
        let numbering = r#"<w:numbering xmlns:w="x">
            <w:abstractNum w:abstractNumId="1">
              <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/></w:lvl>
              <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerRoman"/></w:lvl>
            </w:abstractNum>
            <w:abstractNum w:abstractNumId="2">
              <w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl>
            </w:abstractNum>
            <w:num w:numId="1"><w:abstractNumId w:val="1"/></w:num>
            <w:num w:numId="2"><w:abstractNumId w:val="2"/></w:num>
        </w:numbering>"#;
        let item = |ilvl: &str, num_id: &str, text: &str| {
            format!(
                "<w:p><w:pPr><w:numPr><w:ilvl w:val=\"{ilvl}\"/><w:numId w:val=\"{num_id}\"/></w:numPr></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"
            )
        };
        let document = format!(
            r#"<w:document xmlns:w="x"><w:body>{}{}{}<w:p><w:r><w:t>收尾的普通段落</w:t></w:r></w:p></w:body></w:document>"#,
            item("0", "1", "甲"),
            item("1", "1", "乙"),
            item("0", "2", "丙"),
        );
        let path = docx_parts(
            dir.path(),
            "列表.docx",
            &[
                ("word/numbering.xml", numbering),
                ("word/document.xml", &document),
            ],
        );
        let (_, content) = DocxAdapter.read(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("DOCX 该读成可重排");
        };
        let body = flow.chapters[0].source();
        assert!(
            body.contains(
                "<ol>\n<li>甲<ol style=\"list-style-type: lower-roman\">\n<li>乙</li>\n</ol>"
            ),
            "层号 1 要落成真嵌套，记数按 numbering.xml：{body}"
        );
        assert!(
            body.contains("</ol>\n</li>\n</ol>\n<ul>\n<li>丙"),
            "同一层换了记数就关掉重开（号跟着从头数）：{body}"
        );
        assert!(
            body.contains("</ul>\n<p>收尾的普通段落</p>"),
            "普通段落之前必须把列表关掉，否则 `</ul>` 会掉进下一段：{body}"
        );
        assert!(!body.contains("<p>甲"), "项不许再包一层段落：{body}");
        // 字一个都不能少（这一族的底线）。
        let seen = super::super::visible_text(body);
        for want in ["甲", "乙", "丙", "收尾的普通段落"] {
            assert!(seen.contains(want), "可见文本少了 {want}：{seen}");
        }
    }

    /// 认不出记数时**不编一个号**：缩进留着、号不画。
    ///
    /// 三种认不出的写法都真存在（`without_numid.docx` 那份样本就叫这个名字）：`numId`
    /// 在 numbering.xml 里没有、层号超出定义的范围、以及整份 `numbering.xml` 缺失。
    /// 另一条底线是「只有 `w:ilvl` 没有 `w:numId`」根本不算列表。
    #[test]
    fn an_unresolvable_numbering_is_an_indent_not_an_invented_number() {
        let dir = tempfile::tempdir().unwrap();
        let numbering = r#"<w:numbering xmlns:w="x">
            <w:abstractNum w:abstractNumId="1">
              <w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl>
            </w:abstractNum>
            <w:num w:numId="1"><w:abstractNumId w:val="1"/></w:num>
        </w:numbering>"#;
        let document = r#"<w:document xmlns:w="x"><w:body>
            <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="7"/></w:numPr></w:pPr><w:r><w:t>没有这个实例</w:t></w:r></w:p>
            <w:p><w:pPr><w:numPr><w:ilvl w:val="4"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>层号超出定义</w:t></w:r></w:p>
            <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/></w:numPr></w:pPr><w:r><w:t>缺 numId</w:t></w:r></w:p>
        </w:body></w:document>"#;
        let path = docx_parts(
            dir.path(),
            "认不出.docx",
            &[
                ("word/numbering.xml", numbering),
                ("word/document.xml", document),
            ],
        );
        let (_, content) = DocxAdapter.read(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("DOCX 该读成可重排");
        };
        let body = flow.chapters[0].source();
        assert!(
            body.contains(r#"<ul style="list-style-type: none">"#),
            "认不出的那一档只留缩进：{body}"
        );
        assert!(!body.contains("<ol>"), "不该凭空出一串号：{body}");
        assert!(
            body.contains("<p>缺 numId</p>"),
            "只有 ilvl 没有 numId 不是列表：{body}"
        );
    }

    /// 脚注与尾注：正文里落一个可点的上标锚，注体摆回**引用它的那一章**末尾。
    ///
    /// 钉的是弹层那条链（`chapter_layout` 的 `footnote_anchor_is_ref` / `is_footnote_body`）
    /// 在 docx 这一路也成立，四格各自对应一个真会犯的错：
    /// 那三种 `w:type` 格子不是注文（照 id 判会把第一条真注文当分隔符丢掉）、
    /// `w:footnoteRef` 是注里那个标记自己、不能当注体的字、
    /// 段与段之间要接住（诗的注文整条靠 `w:br` 分行）、
    /// 以及**注体缺失时一个字都不写**（留一个点开没内容的上标看着就是坏了）。
    #[test]
    fn footnotes_are_anchored_and_their_bodies_land_in_the_referencing_chapter() {
        let dir = tempfile::tempdir().unwrap();
        let footnotes = r#"<w:footnotes xmlns:w="x">
            <w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:t>—</w:t></w:r></w:p></w:footnote>
            <w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:t>=</w:t></w:r></w:p></w:footnote>
            <w:footnote w:id="1">
              <w:p><w:r><w:footnoteRef/><w:t xml:space="preserve">注文甲与 R&amp;D</w:t></w:r></w:p>
              <w:p><w:r><w:t>第二段</w:t></w:r><w:r><w:br/><w:t>换行</w:t></w:r></w:p>
            </w:footnote>
        </w:footnotes>"#;
        let endnotes = r#"<w:endnotes xmlns:w="x">
            <w:endnote w:type="separator" w:id="-1"><w:p><w:r><w:t>—</w:t></w:r></w:p></w:endnote>
            <w:endnote w:id="1"><w:p><w:r><w:t>尾注丙</w:t></w:r></w:p></w:endnote>
        </w:endnotes>"#;
        let reference = |kind: &str, id: &str| format!("<w:r><w:{kind} w:id=\"{id}\"/></w:r>");
        let document = format!(
            concat!(
                r#"<w:document xmlns:w="x"><w:body>"#,
                r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>第一章</w:t></w:r></w:p>"#,
                r#"<w:p><w:r><w:t>正文</w:t></w:r>{}<w:r><w:t>尾巴</w:t></w:r></w:p>"#,
                r#"<w:p><w:r><w:t>又一处</w:t></w:r>{}<w:r><w:t>。</w:t></w:r></w:p>"#,
                r#"<w:p><w:r><w:t>引用不到的一条</w:t></w:r>{}<w:r><w:t>。</w:t></w:r></w:p>"#,
                r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>第二章</w:t></w:r></w:p>"#,
                r#"<w:p><w:r><w:t>干净的正文</w:t></w:r></w:p>"#,
                r#"</w:body></w:document>"#
            ),
            reference("footnoteReference", "1"),
            reference("endnoteReference", "1"),
            reference("footnoteReference", "9"),
        );
        let path = docx_parts(
            dir.path(),
            "脚注.docx",
            &[
                ("word/footnotes.xml", footnotes),
                ("word/endnotes.xml", endnotes),
                ("word/document.xml", &document),
            ],
        );
        let (_, content) = DocxAdapter.read(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("DOCX 该读成可重排");
        };
        assert_eq!(flow.chapters.len(), 2);
        let first = &flow.chapters[0].source();
        let second = &flow.chapters[1].source();
        assert!(
            first.contains(r##"<sup><a epub:type="noteref" href="#fn1">1</a></sup>"##),
            "引用落成可点的上标：{first}"
        );
        assert!(
            first.contains(r##"<sup><a epub:type="noteref" href="#en1">1</a></sup>"##),
            "尾注各数自己的号（两种的 id 都从 1 起也不撞）：{first}"
        );
        assert!(
            first.contains(
                r#"<aside epub:type="footnote" id="fn1">注文甲与 R&amp;D 第二段<br/>换行</aside>"#
            ),
            "注体留在本章末尾，段间用空格接、实体折回来：{first}"
        );
        assert!(
            first.contains(r#"<aside epub:type="footnote" id="en1">尾注丙</aside>"#),
            "尾注的注体同样摆回本章：{first}"
        );
        assert_eq!(
            first.matches("<aside").count(),
            2,
            "注体只有这两条：那三种 `w:type` 格子是页底那条线，不是注文：{first}"
        );
        assert!(
            !first.contains("#fn9"),
            "注体缺失的一条不该留一个点开没内容的上标：{first}"
        );
        assert!(
            !second.contains("aside"),
            "注体不能掉到没引用它的那一章：{second}"
        );
    }

    /// 同一张注被引用第二次（Word 的「交叉引用脚注」）：号不变，注体只摆一份。
    ///
    /// 两个 `id` 相同的 `aside` 落在同一章里，`footnote_body_text` 解到的是**后那一个**，
    /// 而屏上看不出任何异常 —— 所以这一条要在归约那一步就断掉。
    #[test]
    fn a_repeated_reference_keeps_one_number_and_one_body() {
        let dir = tempfile::tempdir().unwrap();
        let footnotes = r#"<w:footnotes xmlns:w="x">
            <w:footnote w:id="2"><w:p><w:r><w:t>重复引用的注文</w:t></w:r></w:p></w:footnote>
        </w:footnotes>"#;
        let document = r#"<w:document xmlns:w="x"><w:body>
            <w:p><w:r><w:t>第一处</w:t></w:r><w:r><w:footnoteReference w:id="2"/></w:r></w:p>
            <w:p><w:r><w:t>第二处</w:t></w:r><w:r><w:footnoteReference w:id="2"/></w:r></w:p>
            <w:p><w:r><w:t>另一条</w:t></w:r><w:r><w:footnoteReference w:id="3"/></w:r></w:p>
        </w:body></w:document>"#;
        let path = docx_parts(
            dir.path(),
            "重引.docx",
            &[
                ("word/footnotes.xml", footnotes),
                ("word/document.xml", document),
            ],
        );
        let (_, content) = DocxAdapter.read(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("DOCX 该读成可重排");
        };
        let body = flow.chapters[0].source();
        assert_eq!(
            body.matches(r##"href="#fn2">1</a></sup>"##).count(),
            2,
            "两处都指同一个号：{body}"
        );
        assert_eq!(
            body.matches(r#"<aside epub:type="footnote" id="fn2">"#)
                .count(),
            1,
            "一章里同一个 id 只能有一份注体：{body}"
        );
        assert!(
            !body.contains("#fn3"),
            "没有注体的那一条（这份文件只有一条注）一个字都不写：{body}"
        );
    }

    /// 真 Word 样本上把列表与脚注这两刀各量一遍（怎么取这些文件见 `tests/fixtures/README.md`）。
    ///
    /// 四个样本各自钉一件事：
    /// - `numbering.docx`（Word 写的）：正文 5 个段落挂 `w:numPr`，抽象定义里有
    ///   `decimal` / `decimalEnclosedCircle` / `lowerRoman` / `none` 四种。用到的前三中
    ///   要各自落成形状，且**两份十进制实例（`numId` 1 与 2）不能连成一串号** ——
    ///   Word 的号按实例从头数。`none` 那一档这份文件定义了却没段落引用，所以它由
    ///   合成夹具那条门钉（`an_unresolvable_numbering_is_an_indent_not_an_invented_number`）。
    /// - `override.docx`（Word 写的）：同一个抽象定义被 `w:lvlOverride` 改写了一层，
    ///   号要按改写后的那一格走。
    /// - `footnotes.docx`（Word 写的）：一条真注文 + 正文里一个 `w:footnoteReference`。
    /// - `without_numid.docx`（Word 写的）：`footnotes.xml` 里**只有** separator /
    ///   continuationSeparator / continuationNotice 那三种图形格子，其中一条的 id 是 `1`
    ///   —— 与 `footnotes.docx` 里那条真注文的 id 一模一样。这一条就是「判 `w:type`
    ///   而不是判 id」那句话说的事，在真样本上量出来。
    ///
    /// 跑法：`cargo test -p folio-core --lib -- --ignored the_real_word_documents`
    #[test]
    #[ignore = "要仓库外的真 Word 样本：见 tests/fixtures/README.md 的 docx 那一节"]
    fn the_real_word_documents_keep_their_lists_and_notes() {
        let dir = Path::new("../../tests/fixtures/local/docx");
        let body_of = |name: &str| -> String {
            let path = dir.join(name);
            let (_, content) = DocxAdapter
                .read(&path)
                .unwrap_or_else(|err| panic!("{} 读不出来：{err}", path.display()));
            let Content::Reflow(flow) = content else {
                panic!("DOCX 该读成可重排：{name}");
            };
            flow.chapters
                .iter()
                .map(|chapter| chapter.source().to_string())
                .collect()
        };
        let lists = body_of("numbering.docx");
        assert!(lists.contains("<ol>"), "decimal 那一档：{lists}");
        assert!(
            lists.contains(r#"<ol style="list-style-type: lower-roman">"#),
            "lowerRoman 那一档要落到 list-style-type：{lists}"
        );
        // `numId` 1 是一项、`numId` 2 是两项，两者都是 `decimal`。合成一条就成了
        // 1 / 2、3，而 Word 的纸面上是 1 / 1、2。
        assert_eq!(
            lists.matches("<ol>").count(),
            2,
            "换了编号实例就换一个列表（号跟着从头数）：{lists}"
        );
        let overrides = body_of("override.docx");
        assert!(
            overrides.contains("<li>"),
            "lvlOverride 那一份也归约成列表：{overrides}"
        );

        let notes = body_of("footnotes.docx");
        assert!(
            notes.contains(r##"<sup><a epub:type="noteref" href="#fn1">1</a></sup>"##),
            "真样本里的引用要落成可点的上标：{notes}"
        );
        assert!(
            notes.contains(r#"<aside epub:type="footnote" id="fn1">World</aside>"#),
            "注体摆回引用它的那一章：{notes}"
        );

        let only_separators = body_of("without_numid.docx");
        assert!(
            !only_separators.contains("noteref") && !only_separators.contains("<aside"),
            "那三种 `w:type` 格子（其中一条 id 就是 1）一条都不许被当成注文摆出来"
        );

        // LibreOffice 写的那一份：没有列表也没有注，归约后不该凭空多出形状来。
        let libre = body_of("hello.docx");
        assert!(
            !libre.contains("<li>") && !libre.contains("<aside"),
            "没有的东西不凭空造：{libre}"
        );
    }

    /// 真 Word 文件那一遍：`tests/fixtures/local/docx/` 里那四份（怎么取见那份 README）。
    /// 合成夹具能摆出形状，**生产者怎么写**只有真文件说得准 —— 这一遍盯三条普遍不变量，
    /// 外加那条被真样本叫出来的「格子里不再套段落」。
    ///
    /// 跑法：`cargo test -p folio-core --lib -- --ignored the_real_word_documents`
    #[test]
    #[ignore = "要仓库外的真 Word 样本：见 tests/fixtures/README.md 的 docx 那一节"]
    fn the_real_word_documents_keep_their_shapes() {
        let dir = Path::new("../../tests/fixtures/local/docx");
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(".docx"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        // 空目录会让下面每一条断言都**空转地通过**，所以先把它当一条门。
        assert!(!names.is_empty(), "{} 里一份样本都没有", dir.display());

        for name in &names {
            let path = dir.join(name);
            let (meta, content) = DocxAdapter.read(&path).unwrap_or_else(|err| {
                panic!("{} 读不出来：{err}", path.display());
            });
            let Content::Reflow(flow) = content else {
                panic!("DOCX 该读成可重排：{}", path.display());
            };
            assert!(!flow.chapters.is_empty(), "{name} 一章都没有");
            assert!(!meta.title.is_empty(), "{name} 书名是空的");
            let body: String = flow
                .chapters
                .iter()
                .map(|chapter| chapter.source().to_string())
                .collect();
            assert!(!body.contains("<p><p"), "{name} 里段落套段落");
            assert!(
                super::super::visible_chars(&body).is_some_and(|n| n > 0),
                "{name} 可见文本是空的"
            );
        }

        // 图片那条链（`r:embed` → rels → `word/media/…`）在真文件上走得到字节。
        let images = DocxAdapter
            .index(&dir.join("having-images.docx"))
            .unwrap()
            .1;
        let Content::Reflow(flow) = images else {
            panic!("该读成可重排");
        };
        assert_eq!(flow.images.len(), 3, "四条引用、三份文件，登记按文件去重");
    }

    #[test]
    fn a_zip_without_document_xml_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.docx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "[Content_Types].xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"<Types/>").unwrap();
        zip.finish().unwrap();
        assert!(read_book(&path).is_err());
    }
}
