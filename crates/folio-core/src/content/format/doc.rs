//! Word 97-2003（`.doc`）：一个 OLE2 复合文件里的 `WordDocument` 流。
//!
//! `.doc` 不是电子书格式，但它与 EPUB / DOCX 共享同一件事：**壳与正文是两层**。壳是
//! OLE2 的复合文件（`D0CF11E0` 那一族，`.xls` / `.ppt` / `.lit` 都住在这种壳里），正文是
//! `WordDocument` 流里那条字符流 —— 而字符流的位置**不写在文件里**：FIB 把它指到另一条流
//! （`0Table` / `1Table`，按 FIB 的一个比特选）里的 piece table，压缩的那几段还按文档语言
//! 选码页（简体 cp936 / 繁体 cp950 / 日 cp932 / 韩 cp949）。那一层不自己读，理由与实测的
//! 账写在根 `Cargo.toml` 的 `rwml` 那一格；这一格只管**把交回的模型归约成引擎就绪的
//! XHTML** —— 归约到与 [`super::docx`] **同一串形状**，之后的排版、锚点、选中、脚注弹层
//! 全部走现有链路，界面上不存在「这是 `.doc` 还是 `.docx`」的分支。
//!
//! ## 归约到什么程度
//!
//! 一期只承诺读者真正会看的那几样：
//!
//! - 标题层级：`heading_level` 1（以及 `Title` 样式）起新章，2..=6 写成 `<h2>`…`<h6>`
//!   —— 与 DOCX 同一条规则、同一份章表形状
//! - run 的粗体 / 斜体 / 下划线 / 删除线 / 上下标 → `<strong>` / `<em>` / `<u>` / `<s>` /
//!   `<sup>` / `<sub>`；隐藏文字（`fVanish`）**不写**：屏上看不见的字不进可见文本那把尺
//! - 列表：`level` 落成真嵌套、`ordered` 分 `<ol>` / `<ul>`，走 [`ListWriter`] 那台
//!   与 DOCX / ODT 共用的状态机
//! - `HYPERLINK` 域的结果 → `<a href>`（其余域只留结果那几个字）
//! - 图片按文档序登记成合成 href（`img-N.<ext>`），**字节不进库**：画的时候按
//!   [`DocumentProvider::image_bytes`] 回容器现取，与 RTF 的 `\pict` 同一条路
//! - 表格按行摊平、格子用 `" | "` 连接（行盒这一族引擎没有，与 DOCX 同一刀）
//!
//! 分栏、页眉页脚、文本框、图表一律不摆 —— 它们与正文**不在同一个子文档**，见
//! [`main_blocks`] 那一条。
//!
//! ## 三样故意没接
//!
//! - **书目**：Title / Author / Keywords 存在 OLE2 的 `\05SummaryInformation` 那一格，
//!   而 `rwml` 的二进制读法不碰它（实测那三份真样本的 `core_properties()` 全空）。症状是
//!   书名回落到文件名、作者那一行不显示（界面上「没有值就不显示那一行」），不是显示错东西。
//! - **脚注与尾注**：注体在另一个子文档，而 `rwml` 只交出**区域**、不交出「哪一条注挂在
//!   正文的哪一个字上」（它自己的注释就写着 `.doc` 那一支的 note id 是合成的）。锚接不上的
//!   注摆出来是错的注，所以宁可少那一段注文。
//! - **Word 6/95**（`wIdent` 写 `0x0170` 的那一代）：[`ole2_shape`] 与 `rwml` 的 FIB 检查
//!   同一判据，都不认 —— 症状是一次说得清的拒绝，而不是半本读出来的怪字。

use std::collections::HashMap;
use std::io::Read as _;
use std::path::Path;

use rwml::{
    Block, Cell, DocModel, FieldRole, Image as WordImage, ListInfo, ParaProps, Paragraph, Row, Run,
    SourceRegionKind, Table, VertAlign,
};

use super::{
    Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, FlowDoc, Format, ImageRes,
    ListWriter, Marker, chapter_images, escape_attr, escape_into, escape_text, extension_of,
    guess_media_type, image_dims, stem_of, visible_chars, visible_text,
};
use crate::Result;
use crate::infra::error::IoResultExt as _;

pub struct DocAdapter;

impl DocumentProvider for DocAdapter {
    fn format(&self) -> Format {
        Format::Doc
    }

    /// `.dot`（Word 97 那一代的模板）与 `.doc` 是同一个容器、同一套流，所以同一个 provider。
    fn supports(&self, path: &Path) -> bool {
        matches!(extension_of(path).as_deref(), Some("doc") | Some("dot"))
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let book = read_book(path)?;
        let images: Vec<ImageRes> = book.images.iter().map(Picture::to_res).collect();
        let chapters = book
            .chapters
            .iter()
            .enumerate()
            .map(|(ix, (title, body))| {
                Chapter::section(title.clone(), ix)
                    .with_char_len(visible_chars(body))
                    .with_images(chapter_images(body, &images))
            })
            .collect();
        Ok((
            book.meta,
            Content::Reflow(FlowDoc {
                chapters,
                // 目录不在这里造：标题层级已经落成章表，界面那条「没有目录就退回章节表」的
                // 兜底给出的正是同一份东西（与 [`super::docx`] 同一条理由）。
                toc: Vec::new(),
                images,
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
                images: book.images.iter().map(Picture::to_res).collect(),
                ..Default::default()
            }),
        ))
    }

    fn read_texts(&self, path: &Path) -> Option<Vec<String>> {
        read_book(path)
            .ok()
            .map(|book| book.chapters.into_iter().map(|(_, body)| body).collect())
    }

    /// 图片字节按合成 href 现取一次：那一串字节在容器的 `Data` 流里，与 RTF 每次重开文件
    /// 解一遍 `\pict` 是同一笔账（见 `format::rtf` 的 `image_bytes`）。
    fn image_bytes(&self, path: &Path, href: &str) -> Option<Vec<u8>> {
        let book = read_book(path).ok()?;
        book.images
            .into_iter()
            .find(|picture| picture.href == href)
            .map(|picture| picture.bytes)
    }
}

struct Book {
    meta: DocMeta,
    chapters: Vec<(String, String)>,
    /// 登记上了的图（元数据 + 字节）。`index` / `read` 只取元数据那一半，字节留给
    /// [`DocumentProvider::image_bytes`]。
    images: Vec<Picture>,
}

struct Picture {
    href: String,
    media_type: String,
    width: u32,
    height: u32,
    bytes: Vec<u8>,
}

impl Picture {
    fn to_res(&self) -> ImageRes {
        ImageRes {
            href: self.href.clone(),
            source: self.href.clone(),
            media_type: self.media_type.clone(),
            width: self.width,
            height: self.height,
        }
    }
}

/// 一次解完：书目、章表、图片。
///
/// `rwml::Document::open` 只吃整份字节（它没有按路径的入口），所以这一格是**整本读进内存**
/// 一次。`.doc` 的体积由正文与内嵌图决定，一本带几十张图的旧文档几十 MB 是真存在的；
/// 可重排那一族本来就要正文在手（没有「按页现取」那条路），这一笔与它要付的是同一族。
fn read_book(path: &Path) -> Result<Book> {
    let bytes = std::fs::read(path).io_path(path)?;
    let document =
        rwml::Document::open(&bytes).map_err(|err| unsupported(path, &err.to_string()))?;
    let model = document.model();
    let title = stem_of(path);

    let mut builder = Builder::new(title.clone());
    builder.blocks(&main_blocks(&model));
    builder.finish();
    if builder
        .chapters
        .iter()
        .all(|(_, body)| body.trim().is_empty())
    {
        return Err(unsupported(path, "WordDocument 流里没有正文"));
    }

    let meta = DocMeta {
        title,
        format: Format::Doc,
        size: bytes.len() as u64,
        ..DocMeta::default()
    };
    Ok(Book {
        meta,
        chapters: builder.chapters,
        images: builder.images,
    })
}

/// 只取**正文那一段**块。
///
/// Word 把脚注、尾注、页眉页脚、批注、文本框的正文存在同一条字符流的**其他子文档**里，
/// `rwml` 按子文档给模型分了区域（`SourceRegionKind`）。整份照读的症状不是「少字」而是
/// **顺序错**：注体与页眉那些字会插在正文中间，而 `chapters.char_len` 与 `Locator::offset`
/// 数的都是这一串 —— 进度与锚点一起偏，而屏上看不出异常。
///
/// `regions` 为空 = 这份模型没有子文档地图（`rwml` 自己合成的模型就是这种），整份就是正文。
///
/// 取区间走 `rwml` 自己的那两个入口（`source_regions` / `source_region_blocks`）而不是
/// 自己 `start..end`：后一对**按模型长度钳过**，而自己切片要在「区间倒挂」那一档多加一道门
/// —— 那份区间是从文件里读出来的，倒挂不是不可能。
fn main_blocks(model: &DocModel) -> Vec<&Block> {
    if model.regions.is_empty() {
        return model.blocks.iter().collect();
    }
    model
        .source_regions(SourceRegionKind::Main)
        .flat_map(|region| model.source_region_blocks(region))
        .collect()
}

/// 一份 `.doc` 的归约状态。
///
/// 与 [`super::docx`] 的 `Builder` 是同一台机器的两个前端：那边吃 quick-xml 的事件流，
/// 这边吃 `rwml` 交回的模型，写出去的 XHTML 逐字同形（章的切法、表格摊平、列表那几层）。
/// 所以这里既不吃 XML 也不建 DOM —— `rwml` 已经把 FIB / piece table / PAPX-CHPX 折成了
/// 段落与 run，这一层只负责把它们摆成引擎要的那一串。
struct Builder {
    meta_title: String,
    chapters: Vec<(String, String)>,
    current_title: Option<String>,
    body: String,
    lists: ListWriter,
    /// 编号列表的**实例序**（见 [`Builder::list_marker`]）。
    instance: usize,
    /// 上一个编号项的号（`None` = 上一段不是编号列表，或那个号念不出数）。
    last_number: Option<u32>,
    images: Vec<Picture>,
    /// 字节 → `images` 里的下标。同一张图被引用两次（插入一张图再复制一份就是这么写的）
    /// 只登记一次：两处引用指同一个 href，图片查看器那一列也不会长出两个影子。
    image_of: HashMap<Vec<u8>, usize>,
}

impl Builder {
    fn new(meta_title: String) -> Self {
        Self {
            meta_title,
            chapters: Vec::new(),
            current_title: None,
            body: String::new(),
            lists: ListWriter::default(),
            instance: 0,
            last_number: None,
            images: Vec::new(),
            image_of: HashMap::new(),
        }
    }

    fn blocks(&mut self, blocks: &[&Block]) {
        for block in blocks {
            match block {
                Block::Paragraph(paragraph) => self.paragraph(paragraph),
                Block::Image(image) => {
                    let html = self.image_tag(image);
                    if !html.is_empty() {
                        self.leave_list();
                        self.push_block(&format!("<p>{html}</p>"));
                    }
                }
                Block::Table(table) => self.table(table),
                // 分页符与分节符是**版式**事实：章已经由标题层级切了，在这里再切一次会让
                // 一章变成几十章（Word 每节都写一条），而章号是落库的进度坐标。
                Block::PageBreak | Block::SectionBreak(_) => {}
                // 图表的「正文」是类别与数值那一串，不是段落（与 DOCX 那一族同一处置）。
                Block::Chart(_) => {}
            }
        }
    }

    fn paragraph(&mut self, paragraph: &Paragraph) {
        let html = self.runs(&paragraph.runs);
        if html.trim().is_empty() {
            self.last_number = None;
            return;
        }
        let level = heading_level(&paragraph.props);
        if level == Some(1) {
            let title = visible_text(&html).trim().to_string();
            if !title.is_empty() {
                self.last_number = None;
                self.flush_chapter();
                self.current_title = Some(title.clone());
                self.body
                    .push_str(&format!("<h1>{}</h1>\n", escape_text(&title)));
                return;
            }
        }
        if let Some(level) = level.filter(|level| *level > 1) {
            self.leave_list();
            self.push_block(&format!("<h{level}>{html}</h{level}>"));
            return;
        }
        // 列表那一条要先判（与 docx 同一条顺序上的道理：挂了列表样式的段落先按项处理）。
        if let Some(list) = &paragraph.props.list {
            let (id, marker) = self.list_marker(list);
            self.lists
                .write(list.level as usize, &id, marker, &html, &mut self.body);
            return;
        }
        self.leave_list();
        self.push_block(&format!("<p>{html}</p>"));
    }

    /// 这一段该写进哪一串号、画什么号。
    ///
    /// `rwml` 交回的 [`ListInfo`] 只有层号、`ordered`、以及**已经算好的那个号的样子**
    /// （`label`：`"1."` / `"•"` / `"а)"`）。没有 DOCX 那个 `numId` 可以当实例身份，所以
    /// 「这是新的一串」只能从号自己看出来：**回到 1，或比上一个号小**。
    /// 念不出数的那一档（label 是罗马数字或自定义符号）并到上一串里 —— 症状是两串连成一串
    /// 数下去，而不是把一串劈成两串，后者更难看穿。
    fn list_marker(&mut self, list: &ListInfo) -> (String, Marker) {
        if !list.ordered {
            self.last_number = None;
            return ("bullet".to_string(), bullet_marker(&list.label));
        }
        let number = leading_number(&list.label);
        let restarts = match (number, self.last_number) {
            (Some(number), Some(last)) => number <= last,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => true,
        };
        if restarts {
            self.instance += 1;
        }
        self.last_number = number;
        (format!("numbered-{}", self.instance), Marker::Ordered(None))
    }

    /// 若干段的行内 HTML（字符样式、链接、图片都折在这里）。
    fn runs(&mut self, runs: &[Run]) -> String {
        let mut out = String::new();
        for run in runs {
            // 隐藏文字（`fVanish`）不写：那一族字屏上看不见，而 `char_len` 与锚点数的
            // 是**可见**文本。写进去等于让看不见的那几个字参与进度分母。
            if run.props.hidden {
                continue;
            }
            if let Some(image) = &run.image {
                out.push_str(&self.image_tag(image));
            }
            if run.text.is_empty() {
                continue;
            }
            let mut text = String::new();
            escape_into(&mut text, &run.text);
            // 包的顺序与 [`super::docx`] 逐字一致：`em` 在最里、`strong` 在外、`u` 再外。
            // 两个前端写出同一串 XHTML，排版那一层与测试门才不必认两种形状。
            if run.props.italic {
                text = format!("<em>{text}</em>");
            }
            if run.props.bold {
                text = format!("<strong>{text}</strong>");
            }
            if run.props.underline {
                text = format!("<u>{text}</u>");
            }
            if run.props.strike {
                text = format!("<s>{text}</s>");
            }
            match run.props.vert_align {
                VertAlign::Super => text = format!("<sup>{text}</sup>"),
                VertAlign::Sub => text = format!("<sub>{text}</sub>"),
                VertAlign::Baseline => {}
            }
            if let FieldRole::Hyperlink { url } = &run.field {
                text = format!("<a href=\"{}\">{text}</a>", escape_attr(url));
            }
            out.push_str(&text);
        }
        out
    }

    /// 一张图 → `<img src>`，并把它登记进 `images`。
    ///
    /// 交不出字节的那种占位（`rwml` 认得这一张而容器里没有它的数据）与解不动的格式
    /// （WMF / EMF 那一族 —— 我们只有位图与 SVG 两条栅格路）**都照样写 `<img>`**：
    /// 正文里那一格要留着，由排版按「这张图取不到」降级。悄悄删掉一个 `<img>` 会让这一章的
    /// 可见文本与锚点一起挪位，而那比画不出这张图难查得多。
    fn image_tag(&mut self, image: &WordImage) -> String {
        let Some(bytes) = image.bytes.as_ref() else {
            return String::new();
        };
        // 声明优先、按字节头兜底：`.doc` 里那张图的 mime 是 Word 自己写的，写错的不少
        // （`image/jpg` 那一族），而字节头不会说谎。
        let media_type = image
            .mime
            .as_deref()
            .filter(|mime| !mime.trim().is_empty() && !mime.starts_with("application"))
            .map(|mime| {
                mime.split(';')
                    .next()
                    .unwrap_or(mime)
                    .trim()
                    .to_ascii_lowercase()
            })
            .or_else(|| guess_media_type(bytes).map(str::to_string))
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let href = match self.image_of.get(bytes) {
            Some(ix) => self.images[*ix].href.clone(),
            None => {
                let href = format!(
                    "img-{}.{}",
                    self.images.len(),
                    super::cover::extension(&media_type)
                );
                let (width, height) = image_dims(bytes);
                self.image_of.insert(bytes.clone(), self.images.len());
                self.images.push(Picture {
                    href: href.clone(),
                    media_type,
                    width,
                    height,
                    bytes: bytes.clone(),
                });
                href
            }
        };
        format!("<img src=\"{}\"/>", escape_attr(&href))
    }

    /// 表格摊平：一行一个 `<p>`，格子用 `" | "` 接。
    fn table(&mut self, table: &Table) {
        self.leave_list();
        for row in &table.rows {
            if let Some(line) = self.row(row) {
                self.push_block(&format!("<p>{line}</p>"));
            }
        }
    }

    /// 一行摊成格子串（`None` = 这一行一个字都没有）。
    ///
    /// 表格与**格子里的嵌套表格**共用这一份：空格子不占一格这条规矩在两处各写一遍，
    /// 就会有一处留下 `" |  | "` 那种空格子 —— 而它的症状是那一行读起来凭空断了一格。
    fn row(&mut self, row: &Row) -> Option<String> {
        let cells: Vec<String> = row
            .cells
            .iter()
            .map(|cell| self.cell(cell))
            .filter(|cell| !cell.is_empty())
            .collect();
        (!cells.is_empty()).then(|| cells.join(" | "))
    }

    /// 一个格子：里面的段落用空格接住，嵌套的表格按行摊进同一格。
    ///
    /// 与 [`super::docx`] 的格子同一条口径：**格子里不再包 `<p>`** —— 外层那一行已经包成
    /// 一个 `<p>`，再套一层会被 html5ever 当场关掉，症状是那一行被劈成两段。
    fn cell(&mut self, cell: &Cell) -> String {
        let mut parts: Vec<String> = Vec::new();
        for block in &cell.blocks {
            let html = match block {
                Block::Paragraph(paragraph) => self.runs(&paragraph.runs),
                Block::Image(image) => self.image_tag(image),
                Block::Table(table) => table
                    .rows
                    .iter()
                    .filter_map(|row| self.row(row))
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => String::new(),
            };
            if !html.trim().is_empty() {
                parts.push(html);
            }
        }
        parts.join(" ")
    }

    /// 换普通段落、换标题、换章之前：把列表关掉、把编号那串断掉。
    fn leave_list(&mut self) {
        self.last_number = None;
        self.lists.close_all(&mut self.body);
    }

    fn push_block(&mut self, html: &str) {
        self.body.push_str(html);
        self.body.push('\n');
    }

    fn flush_chapter(&mut self) {
        self.leave_list();
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
            // 整篇一个标题都没有、也没有正文：给一章空的，调用方按「读不出内容」拒掉。
            self.chapters.push((self.meta_title.clone(), String::new()));
        }
    }
}

/// 这一段的标题层级（`None` = 正文）。
///
/// `rwml` 已经把「样式名认不认得出」与「大纲级」折成一格 `heading_level`，但 `Title`
/// 不在里面（它没有大纲级）。与 [`super::docx`] 的 `is_title` 同一判据：`Title` 那一段
/// **起新章**而不是摆成一个 `<h1>`，因为一本书的 Title 只有一个、它就是要单独占一页的东西。
fn heading_level(props: &ParaProps) -> Option<usize> {
    let named_title = [&props.style_id, &props.style_name]
        .into_iter()
        .flatten()
        .any(|style| style.eq_ignore_ascii_case("Title") || style.eq_ignore_ascii_case("标题"));
    if named_title {
        return Some(1);
    }
    props.heading_level.map(usize::from)
}

/// 圆点 / 空心圆 / 方块三档，认不出的一律圆点。
///
/// `label` 是 Word 已经算好的那一个符号，与 ODT 的 `text:bullet-char` 是同一件事的两种存法；
/// 表外的自定义符号（`➢` 那一族折成圆点）那条口径从 [`super::odt`] 量出来以后沿用。
fn bullet_marker(label: &str) -> Marker {
    match label.chars().next() {
        Some('◦') | Some('○') => Marker::Circle,
        Some('▪') | Some('‣') | Some('■') => Marker::Square,
        _ => Marker::Disc,
    }
}

/// `"12."` 那种 label 开头的十进制号；念不出数（`"iv)"`、`"•"`、空）回 `None`。
fn leading_number(label: &str) -> Option<u32> {
    let digits: String = label.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn unsupported(path: &Path, why: &str) -> crate::Error {
    crate::Error::UnsupportedFormat(format!(
        "{}：{why}",
        path.file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    ))
}

/// OLE2 那一支的判定结果。
///
/// 与 [`super::sniff::ArchiveShape`] 同一个形状、同一个理由：**「开不出容器」与「这不是书」
/// 对读者是两句话**（后者建议换个文件，前者值得把文件留下来查），并成一个 `bool` 就说不清。
pub(crate) enum Ole2 {
    /// 目录里有 `WordDocument`，而它的 `wIdent` 是 Word 97-2003 那一条。
    Word,
    /// 开得出目录，但那条流不在、或者开头不对：`.xls` / `.ppt` / `.lit` / Word 6.0 全在这一档。
    NotWord,
    /// 头八字节是 OLE2 而目录读不出来（截断、坏了）。
    Corrupt,
}

/// [`Ole2`] 那三档怎么分：**只读目录与流开头那两个字节**，不碰正文。
///
/// 为什么不拿 `rwml::Document::open` 的成败当判据（CHM 那一支就是那么判的）：那一问只接受
/// **整份字节**，用它判一次等于每本候选书先把正文解一遍。实测一份 9 KB 的最小书 283 µs，
/// 而一本带图的 `.doc` 有几十 MB —— 判定层排在哈希那一趟之前，不该付这一笔。
///
/// 判据与 `rwml` 自己那道 FIB 检查**同一条**（`wIdent == 0xA5EC`，见它的 `fib::parse`），
/// 所以不会漂成「判定收下了、provider 说这不是 Word」。剩下的分歧只在「FIB 之后读挂了」，
/// 那一档由 provider 的 `Err` 说话（这本坏了 / 这一版读不动），不是判定层能替它决定的。
pub(crate) fn ole2_shape(path: &Path) -> Ole2 {
    let Ok(mut container) = cfb::open(path) else {
        return Ole2::Corrupt;
    };
    let Ok(mut stream) = container.open_stream("WordDocument") else {
        return Ole2::NotWord;
    };
    let mut head = [0u8; 2];
    if stream.read_exact(&mut head).is_err() {
        return Ole2::NotWord;
    }
    // [MS-DOC] FibBase.wIdent：Word 6.0 / 7.0 / 97-2003 写 0xA5EC（小端 → `EC A5`）。
    if u16::from_le_bytes(head) != 0xA5EC {
        return Ole2::NotWord;
    }
    Ole2::Word
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{DocAdapter, Ole2, bullet_marker, leading_number, main_blocks, ole2_shape};
    use crate::content::format::{
        Chapter, ChapterLocator, Content, DocumentProvider, FlowDoc, Format, Marker,
    };
    use rwml::{
        Block, Cell, CharProps, DocModel, FieldRole, Image as WordImage, ListInfo, ParaProps,
        Paragraph, Row, Run, SourceRegion, SourceRegionKind, Table, VertAlign,
    };

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([9, 8, 7, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    /// 一份手搭的 `.doc` 模型：`rwml` 那层的类型全是 `pub`，所以**归约这一层可以不吃文件**
    /// 就把形状钉死（二进制那一层的解码由 `the_real_word_documents_*` 那两条真样本门钉，
    /// 那三份是别人生产的文件，我们自己合成不出来）。
    fn model(blocks: Vec<Block>) -> DocModel {
        DocModel {
            blocks,
            ..DocModel::default()
        }
    }

    fn run(text: &str, props: CharProps) -> Run {
        Run {
            text: text.into(),
            props,
            ..Run::default()
        }
    }

    fn para(text: &str, props: ParaProps) -> Block {
        Block::Paragraph(Paragraph {
            props,
            runs: vec![run(text, CharProps::default())],
        })
    }

    fn heading(text: &str, level: u8) -> Block {
        para(
            text,
            ParaProps {
                heading_level: Some(level),
                ..ParaProps::default()
            },
        )
    }

    fn item(text: &str, level: u8, ordered: bool, label: &str) -> Block {
        para(
            text,
            ParaProps {
                list: Some(ListInfo {
                    level,
                    ordered,
                    label: label.to_string(),
                }),
                ..ParaProps::default()
            },
        )
    }

    fn picture(bytes: &[u8], mime: Option<&str>) -> Block {
        Block::Paragraph(Paragraph {
            props: ParaProps::default(),
            runs: vec![Run {
                text: String::new(),
                props: CharProps::default(),
                image: Some(WordImage {
                    bytes: Some(bytes.to_vec()),
                    mime: mime.map(str::to_string),
                    ..WordImage::default()
                }),
                ..Run::default()
            }],
        })
    }

    fn cell(blocks: Vec<Block>) -> Cell {
        Cell {
            blocks,
            ..Cell::default()
        }
    }

    /// 归约一趟的全部产出（章表 + 图片表），与 `index` / `read` 走的是同一个 `Builder`。
    fn reduce(model: &DocModel) -> FlowDoc {
        let mut builder = super::Builder::new("样书".to_string());
        builder.blocks(&main_blocks(model));
        builder.finish();
        let chapters = builder
            .chapters
            .iter()
            .map(|(title, body)| Chapter::inline(title.clone(), body.clone()))
            .collect();
        let images = builder.images.iter().map(super::Picture::to_res).collect();
        FlowDoc {
            chapters,
            images,
            ..Default::default()
        }
    }

    /// `Title` 与 `heading 1` 各起一章，2 级以上写成 `<h2>`…，正文包成 `<p>`。
    #[test]
    fn title_and_first_heading_start_chapters_and_deeper_ones_stay_inside() {
        let flow = reduce(&model(vec![
            para(
                "书名页",
                ParaProps {
                    style_name: Some("Title".to_string()),
                    ..ParaProps::default()
                },
            ),
            heading("第一章", 1),
            para("正文甲", ParaProps::default()),
            heading("小节", 2),
            para("正文乙", ParaProps::default()),
            heading("第二章", 1),
            para("正文丙", ParaProps::default()),
        ]));
        let titles: Vec<&str> = flow.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(
            titles,
            ["书名页", "第一章", "第二章"],
            "Title 与两个一级标题各起一章"
        );
        let first = &flow.chapters[1].source();
        assert!(first.starts_with("<h1>第一章</h1>"), "{first}");
        assert!(first.contains("<h2>小节</h2>"), "二级留在章内：{first}");
        assert!(first.contains("<p>正文甲</p>"), "{first}");
        assert!(flow.chapters[2].source().contains("<p>正文丙</p>"));
        assert!(
            !flow.chapters[2].source().contains("小节"),
            "上一节的标题不许漏到下一站：{}",
            flow.chapters[2].source()
        );
    }

    /// run 的五种字符形状与链接：包出去的顺序与 [`docx`](super::super::docx) 逐字一致。
    ///
    /// 这条门的存在理由是那两个前端必须交同一串 XHTML：排版那一层与 `visible_chars`
    /// 那把尺都只认一种形状，而「`<strong><em>` 还是 `<em><strong>`」这种分歧在屏上
    /// 看不出来，只在字数与锚点上偏。
    #[test]
    fn runs_carry_their_formatting_in_one_shared_shape() {
        let paragraph = Paragraph {
            props: ParaProps::default(),
            runs: vec![
                run(
                    "粗",
                    CharProps {
                        bold: true,
                        ..CharProps::default()
                    },
                ),
                run(
                    "粗斜",
                    CharProps {
                        bold: true,
                        italic: true,
                        ..CharProps::default()
                    },
                ),
                run(
                    "划掉",
                    CharProps {
                        strike: true,
                        ..CharProps::default()
                    },
                ),
                run(
                    "上标",
                    CharProps {
                        vert_align: VertAlign::Super,
                        ..CharProps::default()
                    },
                ),
                run(
                    "下标",
                    CharProps {
                        vert_align: VertAlign::Sub,
                        ..CharProps::default()
                    },
                ),
                Run {
                    text: "链接".into(),
                    props: CharProps::default(),
                    field: FieldRole::Hyperlink {
                        url: "https://example.com/a&b".into(),
                    },
                    ..Run::default()
                },
                run(
                    "看不见",
                    CharProps {
                        hidden: true,
                        ..CharProps::default()
                    },
                ),
                run(
                    "与&实体",
                    CharProps {
                        underline: true,
                        ..CharProps::default()
                    },
                ),
            ],
        };
        let flow = reduce(&model(vec![Block::Paragraph(paragraph)]));
        let body = flow.chapters[0].source();
        assert!(body.contains("<strong>粗</strong>"), "{body}");
        assert!(body.contains("<strong><em>粗斜</em></strong>"), "{body}");
        assert!(body.contains("<s>划掉</s>"), "{body}");
        assert!(body.contains("<sup>上标</sup>"), "{body}");
        assert!(body.contains("<sub>下标</sub>"), "{body}");
        assert!(
            body.contains(r#"<a href="https://example.com/a&amp;b">链接</a>"#),
            "{body}"
        );
        assert!(body.contains("<u>与&amp;实体</u>"), "实体要折回来：{body}");
        assert!(
            !body.contains("看不见"),
            "隐藏文字不进可见文本那把尺：{body}"
        );
    }

    /// 列表：层号落成真嵌套，`ordered` 分 `<ol>` / `<ul>`，**号回到 1 就换一串**。
    #[test]
    fn lists_nest_and_restart_when_the_number_wraps_back() {
        let flow = reduce(&model(vec![
            item("甲", 0, true, "1."),
            item("乙", 1, false, "•"),
            item("丙", 0, true, "1."),
            item("丁", 0, false, "◦"),
            para("收尾的普通段落", ParaProps::default()),
        ]));
        let body = flow.chapters[0].source();
        assert!(
            body.contains("<ol>\n<li>甲<ul>\n<li>乙</li>\n</ul>\n</li>\n</ol>\n<ol>\n<li>丙"),
            "层号 1 落成真嵌套、换串就关掉重开：{body}"
        );
        assert_eq!(body.matches("<ol>").count(), 2, "两串号两个 `<ol>`：{body}");
        assert!(
            body.contains("</ol>\n<ul style=\"list-style-type: circle\">\n<li>丁"),
            "空心圆那一档认得出来：{body}"
        );
        assert!(
            body.contains("</ul>\n<p>收尾的普通段落</p>"),
            "普通段落之前把列表关掉：{body}"
        );
        assert!(!body.contains("<p>甲"), "项不许再包一层段落：{body}");
    }

    /// 认不出号的那一档（罗马数字、自定义符号）并到上一串，而不是把一串劈成两串。
    #[test]
    fn an_unreadable_label_continues_the_current_run() {
        assert_eq!(leading_number("12."), Some(12));
        assert_eq!(leading_number("iv)"), None);
        assert_eq!(leading_number(""), None);
        assert_eq!(bullet_marker("➢"), Marker::Disc);
        assert_eq!(bullet_marker("▪"), Marker::Square);
    }

    /// 表格按行摊平，格子里**不再套段落**（与 DOCX 那一族同一口径，理由在 [`Builder::cell`]）。
    #[test]
    fn table_rows_flatten_and_cells_do_not_nest_paragraphs() {
        let nested = Block::Table(Table {
            rows: vec![Row {
                cells: vec![cell(vec![para("内层", ParaProps::default())]), cell(vec![])],
            }],
            ..Table::default()
        });
        let table = Block::Table(Table {
            rows: vec![
                Row {
                    cells: vec![
                        cell(vec![para("甲", ParaProps::default())]),
                        cell(vec![
                            para("乙一", ParaProps::default()),
                            para("乙二", ParaProps::default()),
                        ]),
                    ],
                },
                Row {
                    cells: vec![
                        cell(vec![para("丙", ParaProps::default()), nested]),
                        cell(vec![para("丁", ParaProps::default())]),
                    ],
                },
            ],
            ..Table::default()
        });
        let flow = reduce(&model(vec![table]));
        let body = flow.chapters[0].source();
        assert!(!body.contains("<p><p"), "行里不许再套段落：{body}");
        assert!(
            body.contains("<p>甲 | 乙一 乙二</p>"),
            "一格里的两段用空格接：{body}"
        );
        assert!(
            body.contains("<p>丙 内层 | 丁</p>"),
            "嵌套表格摊进格子、空格子不占一格：{body}"
        );
    }

    /// 图片：按文档序登记成合成 href，**同一张字节只登记一次**，声明与字节头两处认 mime。
    #[test]
    fn images_get_one_href_per_bytes_and_a_media_type_that_matches_the_bytes() {
        let bytes = png(40, 30);
        let other = png(8, 6);
        let flow = reduce(&model(vec![
            // mime 声明成 `application/…`（Word 的占位写法）时按字节头兜底。
            picture(&bytes, Some("application/octet-stream")),
            picture(&other, Some("image/png")),
            picture(&bytes, Some("image/png")),
        ]));
        assert_eq!(flow.images.len(), 2, "三条引用、两份字节，登记按字节去重");
        assert_eq!(flow.images[0].href, "img-0.png", "mime 从字节头认出来");
        assert_eq!((flow.images[0].width, flow.images[0].height), (40, 30));
        assert_eq!((flow.images[1].width, flow.images[1].height), (8, 6));
        let body = flow.chapters[0].source();
        assert_eq!(
            body.matches(r#"<img src="img-0.png"/>"#).count(),
            2,
            "同一张图的两次引用指同一个 href：{body}"
        );
    }

    /// 脚注区与正文区在**同一份 blocks** 里：只吃 `Main`，其余不摆。
    ///
    /// 这条钉的是「顺序错」那一档最坏的形状：注体的字若混进正文，`char_len` 与
    /// `Locator::offset` 一起偏，而屏上看不出任何异常。
    #[test]
    fn only_the_main_subdocument_is_read() {
        let mut model = model(vec![
            para("正文", ParaProps::default()),
            para("注体不该出现在正文里", ParaProps::default()),
        ]);
        let region = |kind, block_start, block_end| SourceRegion {
            kind,
            source_story_index: None,
            block_start,
            block_end,
            source_start_cp: 0,
            source_len_cp: 0,
            text_start: 0,
            text_len: 0,
        };
        model.regions = vec![
            region(SourceRegionKind::Main, 0, 1),
            region(SourceRegionKind::Footnote, 1, 2),
        ];
        let flow = reduce(&model);
        let body = flow.chapters[0].source();
        assert!(body.contains("正文"), "{body}");
        assert!(
            !body.contains("注体"),
            "注体在别的子文档，不摆进正文：{body}"
        );
    }

    /// 判定层那一支的三档：Word、不是 Word 的壳、开不动的壳。
    ///
    /// `Workbook` 那条流的名字是从 `.xls` 的目录抄来的形状 —— 这一族**共用同一个容器**，
    /// 所以判据只能是「有没有那条流」，不能是「是不是 `.doc` 结尾」。
    #[test]
    fn the_ole2_branch_tells_word_from_the_rest_of_the_family() {
        let dir = tempfile::tempdir().unwrap();

        let other = dir.path().join("表.xls");
        let file = std::fs::File::create(&other).unwrap();
        let mut cf = cfb::CompoundFile::create(file).unwrap();
        cf.create_stream("Workbook").unwrap();
        cf.flush().unwrap();
        drop(cf);
        assert!(matches!(ole2_shape(&other), Ole2::NotWord));

        // 头八字节像 OLE2 而目录指针指着外面。
        let corrupt = dir.path().join("坏.doc");
        std::fs::write(&corrupt, [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]).unwrap();
        assert!(matches!(ole2_shape(&corrupt), Ole2::Corrupt));
    }

    /// 真样本：`tests/fixtures/oss/doc/` 那三份（LibreOffice 按「MS Word 97」导出的，
    /// 来源与许可见那一份 `README.md`）。这一遍钉的是**整条链**：OLE2 容器 → FIB 与
    /// piece table → 模型 → 归约，其中没有一环是我们自己写的。
    #[test]
    fn the_real_word_documents_open_and_keep_their_text() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/doc");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|entry| entry.ok())
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.ends_with(".doc"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        // 空目录会让下面每一条断言**空转地通过**，所以先把它当一条门。
        assert!(!names.is_empty(), "{} 里一份样本都没有", dir.display());

        for name in &names {
            let path = dir.join(name);
            assert!(
                matches!(ole2_shape(&path), Ole2::Word),
                "{name} 该被判定层认成 Word"
            );
            let (meta, content) = DocAdapter
                .read(&path)
                .unwrap_or_else(|err| panic!("{} 读不出来：{err}", path.display()));
            let Content::Reflow(flow) = content else {
                panic!(".doc 该读成可重排：{name}");
            };
            assert!(!flow.chapters.is_empty(), "{name} 一章都没有");
            assert!(!meta.title.is_empty(), "{name} 书名是空的");
            assert_eq!(meta.format, Format::Doc, "{name}");
            let body: String = flow
                .chapters
                .iter()
                .map(|chapter| chapter.source().to_string())
                .collect();
            assert!(!body.contains("<p><p"), "{name} 里段落套段落");
            assert!(
                super::visible_chars(&body).is_some_and(|chars| chars > 0),
                "{name} 可见文本是空的"
            );
        }

        // 那份真表格：两格的字都读出来、摆成两行（**别人生产的文件**给的形状）。
        let path = dir.join("nested_tables.doc");
        let (_, content) = DocAdapter.read(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!(".doc 该读成可重排");
        };
        let body = flow.chapters[0].source();
        assert!(
            body.contains("<p>Outer cell text</p>") && body.contains("<p>Inner cell text</p>"),
            "真样本的两格各摊成一行：{body}"
        );
    }

    /// 真样本走一遍**索引**那一路：正文出库、章号落位，而 `chapter_source` 取回的那一串
    /// 与 `read` 那一路**逐字节相同**（进度与锚点建在这一致性上，`provider.rs` 那条契约）。
    #[test]
    fn the_real_word_documents_index_without_the_text() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/doc");
        let path = dir.join("floating_wrap_policy.doc");
        let (meta, content) = DocAdapter.index(&path).unwrap();
        assert_eq!(meta.title, "floating_wrap_policy", "没有书目就回文件名");
        assert!(meta.authors.is_empty(), "rwml 不读 SummaryInformation");
        let Content::Reflow(flow) = content else {
            panic!(".doc 该读成可重排");
        };
        assert_eq!(flow.chapters.len(), 1);
        assert!(flow.chapters[0].source().is_empty(), "索引那一趟不交正文");
        assert!(
            matches!(flow.chapters[0].locator(), Some(ChapterLocator::Section(0))),
            "单文件容器按章号落位"
        );
        assert!(
            flow.chapters[0].char_len().is_some_and(|chars| chars > 0),
            "字数在索引那一趟就算出来"
        );

        let same = DocAdapter
            .chapter_source(&path, flow.chapters[0].locator().unwrap())
            .unwrap();
        let texts = DocAdapter.read_texts(&path).unwrap();
        let (_, full) = DocAdapter.read(&path).unwrap();
        let Content::Reflow(full_flow) = full else {
            panic!(".doc 该读成可重排");
        };
        assert_eq!(same, full_flow.chapters[0].source());
        assert_eq!(texts, vec![same], "`read_texts` 与 `read` 的正文逐字相同");
    }

    /// 一份穿着 `.doc` 扩展名的裸文本：扩展名归它认领，而开不出 `WordDocument` 就是不认。
    #[test]
    fn a_bare_file_wearing_the_extension_is_rejected_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("假.doc");
        std::fs::write(&path, "其实是一段纯文本\n").unwrap();
        assert!(DocAdapter.supports(&path), "扩展名归它认领");
        assert!(DocAdapter.index(&path).is_err(), "壳不对就不猜它是哪种书");
    }

    /// `.dot` 模板与 `.doc` 是同一个容器、同一个 provider。
    #[test]
    fn a_template_is_claimed_by_the_same_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("模板.dot");
        std::fs::write(&path, b"x").unwrap();
        assert!(DocAdapter.supports(&path));
    }
}
