//! RTF（`.rtf`）：一个 ASCII 文件把正文与格式指令混写在一起。
//!
//! RTF 既没有「章」这个结构，也没有目录，所以这一族的适配器做三件事：
//!
//! 1. **读成段**（[`parse`]）：字节流 → 段表。码页解码、`\ucN` 后面那串回退字节、
//!    `*\目标` 跳过、`\binN` 的裸字节都在这里。
//! 2. **归成章**（[`split_chapters`]）：RTF 的标题只是「字号大一点的段落」，所以章是**判出来**的。
//! 3. **登记图片**（[`Parser::finish_pict`]）：`\pict` 里那串十六进制解出来，按合成 href
//!    记账，字节等要画时回文件现取 —— 与 FB2 的 `<binary>` 同一套「不入库」的规矩。
//!
//! 为什么不引现成的 RTF 解析器：它们给的都是 AST，而 RTF 真正的工作量在语义那一层
//! （码页、回退字节、跳过目标、分章判据），一处都省不下来。词法本身只是一个字节状态机，
//! 而它必须**对畸形输入永不失败** —— 这一份文件的立场是「坏一处不整本打不开」
//! （同 [`super::fb2`] 模块头那条）。把一个外部 AST 接到这条立场上，比自己写更费。
//!
//! 这一版明确不做的，每一条都是看得见的取舍，不是漏掉的：
//!
//! - **字号、颜色、字体名一律不进输出**：重排读者的正文版式归读者的偏好管，作者的
//!   10.5pt 与读者的 22pt 是同一件事的两种说法，听读者的才对。字号只留作分章的判据。
//! - **对齐、缩进、表格线同样不留**；表格降级成「每格一段」，与 FB2 把 `table` 映射成
//!   `div` 同一立场（表格线不是阅读的必需品）。
//! - **WMF / EMF / 位图那些插图丢掉**：`image` 没有这两个解码器，登记进去的表现是静默
//!   画空（见 [`super::shrink_image`] 里 gpui 那条例），不如不登记。
//! - **`\footnote` / `\header` 那一类整段跳过**：注文史混进正文会让每段后面凭空多出一堆
//!   字，与 FB2 丢掉 `<body name="notes">` 同一条理由。脚注渲染是另一件事。
//! - **不猜码页与声明不符的文件**：`\ansicpg` 写什么就按什么解。「按 cp1252 解不动就改按
//!   GBK 试」那条看着能救真书，实际是掷硬币 —— GBK 几乎吃得下任意字节对，被「救」回来的
//!   cp1252 文件会变成一本满是汉字的乱码书，而屏幕上完全看不出发生过什么。
//!   [`a_wrong_declared_codepage_is_followed_as_declared`] 那条测试钉的就是这条立场。

use std::collections::HashMap;
use std::path::Path;

use super::{
    Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, FlowDoc, Format, ImageRes,
    extension_of, image_dims, stem_of,
};
use crate::Result;
use crate::infra::error::IoResultExt as _;

/// 标题判据里「这一段的字数已经不像标题了」的上限。
const MAX_HEADING_CHARS: usize = 60;

/// 「整段粗体」那一判据的更紧上限：抓「楔子」「尾声」，不抓一句被加粗的话。
const MAX_BOLD_HEADING_CHARS: usize = 20;

/// 退化成书名候选的那一行的长度上限（再长就是正文的第一段，不是书名）。
const MAX_TITLE_CHARS: usize = 40;

pub struct RtfAdapter;

impl DocumentProvider for RtfAdapter {
    fn format(&self) -> Format {
        Format::Rtf
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(extension_of(path).as_deref(), Some("rtf"))
    }

    /// 只读索引：章表（标题 + 序号 + 字数）、没有封面、图片只登元数据。
    ///
    /// RTF 是**单文件**，标题与正文出自同一趟扫描，所以这一趟省不下读盘，省下的是
    /// 把每一段的 XHTML 拼出来那部分 —— 与 [`super::fb2`] 同一句话。
    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, false)
    }

    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, true)
    }

    /// 单文件容器里的第 N 章：整本重扫一遍，取第 N 章。
    ///
    /// 分章判据是确定的（同一份字节每次判出同一套章），所以库里那个序号每次都指到
    /// 同一章 —— 这条前提正是 [`ChapterLocator::Section`] 能成立的根据。
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
        let (_, content) = read_with(path, true)?;
        let Content::Reflow(flow) = content else {
            return Err(crate::Error::ChapterUnavailable {
                path: path.to_path_buf(),
                index,
                at: format!("{at:?}"),
            });
        };
        flow.chapters
            .get(index)
            .map(|chapter| chapter.source().to_string())
            .ok_or_else(|| crate::Error::ChapterUnavailable {
                path: path.to_path_buf(),
                index,
                at: format!("{at:?}"),
            })
    }

    /// 图片字节按合成 href 现解一次：`\pict` 那串十六进制就在文件里，与 EPUB 每次重开
    /// 容器、FB2 每次重解 base64 同形，字节不常驻。
    fn image_bytes(&self, path: &Path, href: &str) -> Option<Vec<u8>> {
        let raw = std::fs::read(path).ok()?;
        parse(&raw)
            .images
            .into_iter()
            .find(|pict| pict.href == href)
            .map(|pict| pict.bytes)
    }
}

fn read_with(path: &Path, with_text: bool) -> Result<(DocMeta, Content)> {
    let raw = std::fs::read(path).io_path(path)?;
    let doc = parse(&raw);
    let title = book_title(&doc, path);
    let mut chapters = split_chapters(&doc, &title);
    if chapters.is_empty() {
        // 结构怪到一段正文都没抽出来时仍然给一章空的：让「打开了一本没内容的书」与
        // 「打不开这本书」在界面上能分开。
        chapters.push(Chapter::inline(title.clone(), "<body></body>"));
    }
    let images: Vec<ImageRes> = doc
        .images
        .iter()
        .map(|pict| ImageRes {
            href: pict.href.clone(),
            source: pict.href.clone(),
            media_type: pict.media_type.to_string(),
            width: pict.width,
            height: pict.height,
        })
        .collect();
    let meta = DocMeta {
        title,
        authors: doc.authors.clone(),
        format: Format::Rtf,
        size: raw.len() as u64,
    };
    // 索引那一趟：章表只留标题与序号，**但字数要留下来**（与 [`super::fb2`] 同一条口径 ——
    // 这一趟正文已经在手上，数一遍是白拿的）。
    let chapters = if with_text {
        chapters
    } else {
        chapters
            .into_iter()
            .enumerate()
            .map(|(index, chapter)| {
                let chars = chapter.char_len();
                Chapter::section(chapter.title.clone(), index).with_char_len(chars)
            })
            .collect()
    };
    let flow = FlowDoc {
        chapters,
        // RTF 的作者样式不进输出（模块头第一条），所以没有样式表可存。
        css: String::new(),
        css_hrefs: Vec::new(),
        // 目录由章表来（与 FB2 同一做法）。
        toc: Vec::new(),
        // 翻页方向只有 EPUB 的 spine 那一格读得出来，其余格式按左翻。
        progression_rtl: false,
        // RTF 没有「封面」这个结构：`\info` 里不含图，第一张插图也不猜成封面 ——
        // 猜错的表现是书架上一本书挂着别的东西的图，比没封面难看得多。
        cover: None,
        images,
        cover_ref: None,
    };
    tracing::debug!(
        "rtf: {} 读到 {} 章、{} 张插图、书名{}",
        path.display(),
        flow.chapters.len(),
        flow.images.len(),
        if doc.title.is_some() {
            "取自 \\info"
        } else {
            "靠回落"
        },
    );
    Ok((meta, Content::Reflow(flow)))
}

/// 一趟扫描的产物。
struct RtfDoc {
    paras: Vec<Para>,
    /// `\stylesheet` 里读到的样式号 → 样式名。分章判据要用样式名。
    styles: HashMap<usize, String>,
    images: Vec<Pict>,
    /// `\info` 里的 `\title`；`None` 时由 [`book_title`] 决定退到哪一层。
    title: Option<String>,
    authors: Vec<String>,
}

/// 一个读完的段。块级标记（`<p>` 还是 `<hN>`）到分章那一步才定，所以这里存的是**段内**标记。
#[derive(Debug)]
struct Para {
    /// 段内的 XHTML：文字已转义、行内标记已配平、插图已换成 `<img>`。
    inner: String,
    /// 同一段的纯可见文字（行内标记之外那一截）。分章判据与章名都用它。
    text: String,
    /// 这一段里出现过文字的**最大**字号（半磅）；`0` = 这一路没写过 `\fs`。
    fs: u16,
    /// 文字是否全是粗体。
    all_bold: bool,
    /// 这一段的样式号（`\sN`），用来查 `\stylesheet` 里的样式名。
    style: Option<usize>,
}

/// `\pict` 解出来的一张图。
#[derive(Debug)]
struct Pict {
    /// 合成路径：`pict-<序号>.<扩展名>`。序号 = 这一张在文件里出现的顺序，而分章与
    /// 取字节跑的是同一趟扫描，所以两边数出来的号一定是同一个。
    href: String,
    media_type: &'static str,
    bytes: Vec<u8>,
    width: u32,
    height: u32,
}

/// 行内格式。只留能变成 XHTML 行内标记的那几个 —— 颜色与字体名按模块头那条不进输出。
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct CharFlags {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    sub: bool,
    sup: bool,
}

impl CharFlags {
    /// 套上这组行内标记。嵌套顺序写死（粗→斜→下划线→删除→上下标），产物才稳定，
    /// 测试才钉得住同一份输出。
    fn wrap(&self, text: &str) -> String {
        let mut out = text.to_string();
        if self.bold {
            out = format!("<strong>{out}</strong>");
        }
        if self.italic {
            out = format!("<em>{out}</em>");
        }
        if self.underline {
            out = format!("<u>{out}</u>");
        }
        if self.strike {
            out = format!("<s>{out}</s>");
        }
        if self.sup {
            out = format!("<sup>{out}</sup>");
        }
        if self.sub {
            out = format!("<sub>{out}</sub>");
        }
        out
    }
}

/// 一个组的文字流向。
///
/// 子组**继承**父组的流向（见 [`Frame::child`]）。这一条是 RTF「跳过目标」不漏水的根据：
/// `{\*\datastore …}` 里任何一层子组的字节都不许落到正文去。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dest {
    /// 正文。
    Body,
    /// 丢掉：`*\` 标的目标，以及 `\colortbl`、`\fonttbl`、`\info` 这些容器本身。
    ///
    /// `\colortbl` 必须在丢掉那一列，而且不能只靠 `*\`：Word 写的是
    /// `{\colortbl ;\red0\green0\blue0;}`，那两个分号是**裸文本**，漏进正文就是
    /// 每段前面凭空多一串分号。
    Skip,
    /// `\stylesheet` 的容器本身（它自己没有文字，文字在子组里）。
    Stylesheet,
    /// `\stylesheet` 的一条：捕获的文字是样式名。
    StyleName,
    /// `\info` 的容器。
    Info,
    /// `\info` 里被认出来的那一项（`\title` / `\author`）：捕获的文字是它的值。
    InfoValue,
    /// `\pict`：这一组的字节是图片数据。
    Pict,
}

/// 组的现场。字符格式按组继承、在组尾还原 —— 这是 RTF 的一条基本规则，
/// 所以它们住在帧上而不是全局变量上。
struct Frame {
    dest: Dest,
    flags: CharFlags,
    /// 字号（半磅）；`0` = 这一路没写过 `\fs`。只用于分章判据。
    fs: u16,
    /// 段落样式号（`\sN`）。放在帧上而不是全局，正是为了让 `{\listtext\pard …}`
    /// 那种嵌套组里的 `\pard` 碰不到外层正在积累的段落。
    style: Option<usize>,
    /// `StyleName` 帧：这一条样式的号。
    style_id: Option<usize>,
    /// `InfoValue` 帧：这一项的键。
    info_key: Option<&'static str>,
    /// 捕获中的文字（样式名或 `\info` 的值）。
    capture: String,
    /// `Pict` 帧：攒着的图片字节。
    pict: Option<PictBuf>,
}

impl Frame {
    fn root() -> Self {
        Self {
            dest: Dest::Body,
            flags: CharFlags::default(),
            fs: 0,
            style: None,
            style_id: None,
            info_key: None,
            capture: String::new(),
            pict: None,
        }
    }

    fn child(&self) -> Self {
        let dest = match self.dest {
            // `\stylesheet` 的每个直接子组是一条样式。
            Dest::Stylesheet => Dest::StyleName,
            // 图片组里的子组只有数据，没有正文。
            Dest::Pict => Dest::Skip,
            other => other,
        };
        Self {
            dest,
            flags: self.flags,
            fs: self.fs,
            style: self.style,
            style_id: None,
            info_key: self.info_key,
            capture: String::new(),
            pict: None,
        }
    }
}

/// `\pict` 那一组攒下来的图片数据。
#[derive(Default)]
struct PictBuf {
    data: Vec<u8>,
    /// 组里读到的类型声明；`None` = 没写（那就靠魔数嗅一次）。
    kind: Option<PictKind>,
}

enum PictKind {
    Png,
    Jpeg,
    /// WMF / EMF / Mac PICT / DIB 那一族：`image` 没有解码器。
    Unsupported,
}

impl PictBuf {
    /// 解出一张**画得动**的图：`(扩展名, mime, 字节)`。
    ///
    /// 数据为空、认不出类型、或格式压根没有解码器的都回 `None`：登记一张解不动的图
    /// 等于在版面上留一块说不清原因的空白，不如不登记。
    fn resolve(self) -> Option<(&'static str, &'static str, Vec<u8>)> {
        if self.data.is_empty() {
            return None;
        }
        let kind = self.kind.or_else(|| sniff_kind(&self.data))?;
        match kind {
            PictKind::Png => Some(("png", "image/png", self.data)),
            PictKind::Jpeg => Some(("jpg", "image/jpeg", self.data)),
            PictKind::Unsupported => None,
        }
    }
}

fn sniff_kind(data: &[u8]) -> Option<PictKind> {
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some(PictKind::Png)
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(PictKind::Jpeg)
    } else {
        None
    }
}

/// 一个正在积累的段。
#[derive(Default)]
struct ParaBuf {
    /// 段内的 XHTML。
    inner: String,
    /// 可见文字（未转义）。
    text: String,
    /// 按当前行内格式攒着、还没收口的那一截。
    run: String,
    flags: CharFlags,
    /// 当前行内的字号（半磅），由字符帧带来。
    fs: u16,
    max_fs: u16,
    chars: usize,
    bold_chars: usize,
}

impl ParaBuf {
    /// 行内格式变了：先把攒着的那一截按**旧**格式收口，再记下新格式。
    ///
    /// 这一步不能省：一个 `<strong>` 跨过了格式变更还继续往后攒，它的闭合标签就会跑到
    /// 下一段里去，而 html5ever 会把标签挪到它认为能放的地方 —— 表现是加粗漏到别的段落上。
    fn set_run(&mut self, flags: CharFlags, fs: u16) {
        if flags != self.flags {
            self.flush_run();
            self.flags = flags;
        }
        self.fs = fs;
    }

    fn push(&mut self, s: &str) {
        for ch in s.chars() {
            // **一段还没有任何内容时，ASCII 空白不算内容。** RTF 的换行是行尾（主循环里
            // 丢掉了），但空格是正文：把目标签好的 RTF 里那些用来缩进的普通空格
            // 原样收下，每个段落前面就凭空多出一串空白。
            //
            // 只跳 ASCII 的空格与制表：中文书用全角空格（U+3000）与 `\~`（U+00A0）写的
            // **首行缩进是作者的意思**，跟着一起跳掉就把读者的段落缩进抹平了。
            if self.text.is_empty() && self.inner.is_empty() && matches!(ch, ' ' | '\t' | '\u{b}') {
                continue;
            }
            escape_into(&mut self.run, ch);
            self.text.push(ch);
            self.chars += 1;
            if self.flags.bold {
                self.bold_chars += 1;
            }
        }
        if self.fs > self.max_fs {
            self.max_fs = self.fs;
        }
    }

    /// 段内硬换行（RTF 的 `\line`）。
    fn br(&mut self) {
        self.flush_run();
        self.inner.push_str("<br/>");
    }

    /// 插图。`<img>` 必须落在行内标记之外，所以先收口。
    fn image(&mut self, href: &str) {
        self.flush_run();
        self.inner.push_str(&format!("<img src=\"{href}\"/>"));
    }

    fn flush_run(&mut self) {
        if self.run.is_empty() {
            return;
        }
        let run = std::mem::take(&mut self.run);
        self.inner.push_str(&self.flags.wrap(&run));
    }

    /// 收口成一个段。**只有真有过可见文字或插图的段才算数**：`{\pard\par}` 这种空段在
    /// RTF 里随处可见（列表项后面、段落尾、分节符旁边），带空格的缩进段也一样 ——
    /// 留下它们会让目录长出一串空章，正文里也多出看不见内容的空块。
    fn finish(&mut self, style: Option<usize>) -> Option<Para> {
        self.flush_run();
        let has_image = self.inner.contains("<img");
        if self.text.trim().is_empty() && !has_image {
            self.reset();
            return None;
        }
        let para = Para {
            inner: std::mem::take(&mut self.inner),
            text: std::mem::take(&mut self.text),
            fs: self.max_fs,
            all_bold: self.chars > 0 && self.bold_chars == self.chars,
            style,
        };
        self.reset();
        Some(para)
    }

    fn reset(&mut self) {
        self.max_fs = 0;
        self.chars = 0;
        self.bold_chars = 0;
        self.fs = 0;
        self.flags = CharFlags::default();
    }
}

/// 字节流 → 段表。**这一层对任何输入都不报错**：读到哪算哪，能收多少收多少
/// （模块头那条「坏一处不整本打不开」）。
fn parse(src: &[u8]) -> RtfDoc {
    Parser::new(src).run()
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    frames: Vec<Frame>,
    para: ParaBuf,
    paras: Vec<Para>,
    styles: HashMap<usize, String>,
    images: Vec<Pict>,
    title: Option<String>,
    authors: Vec<String>,
    /// 等码页解码的那串字节（`\'hh`，以及文件里直接冒出来的高位字节）。
    pending: Vec<u8>,
    /// `\uN` 之后要扔掉多少个回退字节。
    fallback: usize,
    /// `\ucN`：一个 `\uN` 后面跟几个回退字符。RTF 的默认是 1。
    uc: usize,
    codepage: &'static encoding_rs::Encoding,
    /// 等低代理项配对的 `\uN` 高位代理。
    high_surrogate: Option<u16>,
}

impl<'a> Parser<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            pos: 0,
            frames: vec![Frame::root()],
            para: ParaBuf::default(),
            paras: Vec::new(),
            styles: HashMap::new(),
            images: Vec::new(),
            title: None,
            authors: Vec::new(),
            pending: Vec::new(),
            fallback: 0,
            uc: 1,
            codepage: encoding_rs::WINDOWS_1252,
            high_surrogate: None,
        }
    }

    fn frame(&self) -> &Frame {
        self.frames.last().expect("根组永远在栈底")
    }

    fn frame_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("根组永远在栈底")
    }

    fn dest(&self) -> Dest {
        self.frame().dest
    }

    /// 当前是否走在正文那条路上（`\par`、`<br/>`、`<img>` 只在正文里生效）。
    fn in_body(&self) -> bool {
        self.dest() == Dest::Body
    }

    fn run(mut self) -> RtfDoc {
        while self.pos < self.src.len() {
            let b = self.src[self.pos];
            match b {
                b'{' => {
                    self.pos += 1;
                    self.open_group();
                }
                b'}' => {
                    self.pos += 1;
                    self.close_group();
                }
                b'\\' => self.control(),
                // RTF 源码里的 CR / LF 是**行尾**，不是内容（文件常常每若干个 `\'hh`
                // 就折一行）。把它们当文字会让每两行之间凭空多一个换行。
                b'\r' | b'\n' => self.pos += 1,
                0x20..=0x7f => {
                    self.pos += 1;
                    // `\uN` 的回退字符也可能是裸 ASCII（Word 常写 `\u233?` 那一形）。
                    if self.fallback > 0 {
                        self.fallback -= 1;
                    } else {
                        // **先把攒着的码页字节解出来**：一个汉字写成 `\'d6\'d0`，后面紧跟
                        // 一个裸 ASCII 字母时，若不先解这串字节，字母就会排到汉字**前面** ——
                        // 屏幕上就是「a如果」这种顺序错乱的正文。
                        self.flush_pending();
                        self.emit_char(b as char);
                    }
                }
                _ => {
                    self.pos += 1;
                    // NUL 只会来自 `\binN` 数错了那一类坏文件。收下它就得到一个谁也
                    // 解释不了的空白方框，丢掉它后面剩下的内容还读得动。
                    if b != 0 {
                        self.byte(b);
                    }
                }
            }
        }
        self.finish_doc()
    }

    /// `\` 开头：控制字（`\par`）或控制符号（`\{`、`\'hh`、`\*`、`\~`……）。
    fn control(&mut self) {
        let src = self.src;
        let start = self.pos + 1;
        let Some(&first) = src.get(start) else {
            // 文件在一个反斜杠处断了。
            self.pos = src.len();
            return;
        };
        if !first.is_ascii_lowercase() {
            // 控制符号：单个字符。规范的分工是「小写字母开头是控制字，其余是控制符号」，
            // 所以 `\PAR` 是「控制符号 P」加正文 `AR` —— 按这一条读才不会把 `\plainx`
            // 读成一个并不存在的控制字 `plainx`。
            self.pos = start + 1;
            if first == b'\'' {
                // `\'hh`：两个十六进制位 = 一个**码页字节**，不是一个字符。
                let hi = src.get(start + 1).copied().and_then(hex_value);
                let lo = src.get(start + 2).copied().and_then(hex_value);
                self.pos = start + 3;
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    self.byte(hi << 4 | lo);
                }
                return;
            }
            self.flush_pending();
            self.symbol(first);
            return;
        }
        let mut i = start;
        while src.get(i).is_some_and(u8::is_ascii_lowercase) {
            i += 1;
        }
        let name = std::str::from_utf8(&src[start..i]).unwrap_or("");
        self.pos = i;
        // 参数：可选的负号 + 一串数字（`\u-10133` 是负写法，`\fs21` 是正写法）。
        let mut negative = false;
        if src.get(self.pos) == Some(&b'-') {
            negative = true;
            self.pos += 1;
        }
        let mut param: i64 = 0;
        let mut has_param = false;
        while let Some(&c) = src.get(self.pos) {
            if !c.is_ascii_digit() {
                break;
            }
            param = param.saturating_mul(10).saturating_add(i64::from(c - b'0'));
            has_param = true;
            self.pos += 1;
        }
        if negative {
            param = -param;
        }
        // 控制字后面**那一个**空格是分隔符，不属于内容（这就是 `\par ` 不吃掉一个空格的原因）。
        if src.get(self.pos) == Some(&b' ') {
            self.pos += 1;
        }
        self.flush_pending();
        self.word(name, param, has_param);
    }

    fn symbol(&mut self, s: u8) {
        self.high_surrogate = None;
        match s {
            // `\*` 说的是「整个这一组我不认识，跳过」。它写在组的第一位。
            b'*' => {
                // 正在捕获样式名 / `\info` 值的那一组不许被降级：`{\title{\*\falt …}}`
                // 里该丢的是**子组**，把父组一起降级会把书名读没。
                if !matches!(self.dest(), Dest::StyleName | Dest::InfoValue) {
                    let frame = self.frame_mut();
                    frame.dest = Dest::Skip;
                    frame.pict = None;
                }
            }
            b'{' => self.emit_str("{"),
            b'}' => self.emit_str("}"),
            b'\\' => self.emit_str("\\"),
            b'~' => self.emit_str("\u{a0}"),
            // 可选连字符是一条软断行提示，屏幕上没有它的对应物。
            b'-' => {}
            b'_' => self.emit_str("\u{2011}"),
            // `\` + 空格 / 制表 / 换行：RTF 里它们是保留写法，什么都不产。
            b' ' | b'\t' | b'\r' | b'\n' => {}
            // 认不得的控制符号：丢标记、留着内容（内容在它前后，不在它里面）。
            _ => {}
        }
    }

    fn word(&mut self, name: &str, param: i64, has_param: bool) {
        if name != "u" {
            self.high_surrogate = None;
        }
        // 开关型的控制字（`\b0`、`\i0`）：没写参数就是开。
        let on = !has_param || param != 0;
        match name {
            // —— 码页与 `\u` 回退 ——
            "ansi" | "pc" | "mac" => {
                // `\mac` 按 cp1252 解：Mac Roman 不在图上已有的那份表里，
                // 认不出的字节变成替换字符而不是丢内容，这是两害相权。
                self.codepage = encoding_rs::WINDOWS_1252;
            }
            "ansicpg" if has_param => {
                self.codepage = encoding_for_codepage(param);
            }
            "uc" if has_param => {
                // 那个上限是防呆：`\uc` 写成一千不会更对，只会把后面的正文吃掉。
                self.uc = param.clamp(0, 8) as usize;
            }
            "u" if has_param => self.unicode(param),
            // `\binN` 后面是**裸字节**（可以含 NUL）。不吃掉整 N 个，后面的整个文件都会
            // 被当控制字读 —— 症状是书读到一半没了，而屏幕上没有任何一处说它坏了。
            "bin" => {
                let n = param.max(0) as usize;
                self.pos = (self.pos + n).min(self.src.len());
                self.pict_unsupported("bin");
            }

            // —— 行内格式 ——
            "b" => self.update_flags(|flags| flags.bold = on),
            "i" => self.update_flags(|flags| flags.italic = on),
            "ul" | "ulw" | "uld" | "uldbw" | "ulh" => {
                self.update_flags(|flags| flags.underline = true)
            }
            "ulnone" => self.update_flags(|flags| flags.underline = false),
            "strike" => self.update_flags(|flags| flags.strike = on),
            "sub" => self.update_flags(|flags| flags.sub = true),
            "nosub" => self.update_flags(|flags| flags.sub = false),
            "super" => self.update_flags(|flags| flags.sup = true),
            "nosuper" => self.update_flags(|flags| flags.sup = false),
            "plain" => self.update_flags(|flags| *flags = CharFlags::default()),
            "fs" if has_param => {
                self.frame_mut().fs = param.clamp(0, u16::MAX as i64) as u16;
            }

            // —— 段落边界 ——
            "par" => {
                if self.in_body() {
                    self.end_para();
                }
            }
            "line" | "hardline" => {
                if self.in_body() {
                    self.para.br();
                }
            }
            // `\softline` 是「这里可以断行」，屏幕上没有它的对应物。
            "softline" => {}
            "tab" | "ptab" | "hptab" => self.emit_str("\t"),
            "sect" | "sectd" | "page" | "pagebb" | "col" => {
                if self.in_body() {
                    self.end_para();
                }
            }
            // 表格降级：每格一段（模块头那条立场）。
            "cell" | "row" | "nestcell" | "nestrow" => {
                if self.in_body() {
                    self.end_para();
                }
            }

            // —— 段落样式 ——
            "s" if has_param && param >= 0 => {
                let frame = self.frame_mut();
                if frame.dest == Dest::StyleName {
                    frame.style_id = Some(param as usize);
                } else {
                    frame.style = Some(param as usize);
                }
            }
            "pard" => {
                self.frame_mut().style = None;
            }

            // —— 目标（容器）的分派 ——
            "stylesheet" if self.in_body() => {
                self.frame_mut().dest = Dest::Stylesheet;
            }
            "info" if self.in_body() => {
                self.frame_mut().dest = Dest::Info;
            }
            "title" | "author" => {
                // 只认 `\info` 的直接子组里那一个：`\title` 在书签与域代码里也出现，
                // 那里的它不是书名。
                let parent_is_info = self
                    .frames
                    .iter()
                    .rev()
                    .nth(1)
                    .is_some_and(|parent| parent.dest == Dest::Info);
                if parent_is_info {
                    // 只存这两个字面量：`name` 是从字节流上借的切片，活不过这一趟扫描，
                    // 而 [`Frame::info_key`] 要的是能挂到组结束的那个键名。
                    let key = if name == "title" { "title" } else { "author" };
                    let frame = self.frame_mut();
                    frame.dest = Dest::InfoValue;
                    frame.info_key = Some(key);
                }
            }
            "pict" => {
                // 已经在被丢掉的目标里（`{\*\shppict{\pict …}}` 那一支）就不登记：
                // Word 同一张图会写新老两份，都收就画两遍。
                if self.in_body() {
                    let frame = self.frame_mut();
                    frame.dest = Dest::Pict;
                    frame.pict = Some(PictBuf::default());
                }
            }
            "pngblip" => self.pict_kind(PictKind::Png),
            "jpegblip" => self.pict_kind(PictKind::Jpeg),
            "wmetafile" | "pichmsg" | "emfblip" | "pmmetafile" | "macpict" | "dibitmap"
            | "bitmap" => self.pict_unsupported(name),
            // 注文史、页眉页脚、表那一族：整组丢掉（模块头列了理由）。
            "footnote" | "endnote" | "ftnsep" | "ftnsepc" | "ftncont" | "ftnaddr" | "ftnblk"
            | "header" | "headerl" | "headerr" | "headerf" | "footer" | "footerl" | "footerr"
            | "footerf" | "fonttbl" | "colortbl" | "generator" | "template" | "listtable"
            | "listoverridetable" | "themedata" => {
                if !matches!(self.dest(), Dest::StyleName | Dest::InfoValue) {
                    self.frame_mut().dest = Dest::Skip;
                }
            }

            // 剩下的：带参数的多半是属性（`\cf2`、`\picwgoal1440`、`\trowd`），
            // 不带参数的多半是一个命名字符。
            _ => self.other_word(name, param, has_param),
        }
    }

    /// 改字符格式：先在旧格式下收口，再改帧，最后把新格式交给段缓冲。
    fn update_flags(&mut self, change: impl FnOnce(&mut CharFlags)) {
        let (flags, fs) = {
            let frame = self.frame_mut();
            let mut flags = frame.flags;
            change(&mut flags);
            frame.flags = flags;
            (flags, frame.fs)
        };
        self.para.set_run(flags, fs);
    }

    fn pict_kind(&mut self, kind: PictKind) {
        if let Some(buf) = self.pict_buf_mut() {
            buf.kind = Some(kind);
        }
    }

    fn pict_unsupported(&mut self, name: &str) {
        let Some(buf) = self.pict_buf_mut() else {
            return;
        };
        buf.kind = Some(PictKind::Unsupported);
        tracing::debug!("rtf: 丢一张插图（{name}）：那种格式没有解码器");
    }

    /// 往内层找那个还在攒数据的 `\pict` 帧（图片数据也可能写在它的子组里）。
    fn pict_buf_mut(&mut self) -> Option<&mut PictBuf> {
        self.frames
            .iter_mut()
            .rev()
            .find_map(|frame| frame.pict.as_mut())
    }

    fn open_group(&mut self) {
        self.flush_pending();
        self.high_surrogate = None;
        let child = self.frame().child();
        self.frames.push(child);
    }

    fn close_group(&mut self) {
        self.flush_pending();
        self.high_surrogate = None;
        if self.frames.len() <= 1 {
            // 多余的 `}`：坏文件里的杂项括号，不能让它把根组弹掉。
            return;
        }
        let frame = self.frames.pop().expect("上面查过栈深");
        match frame.dest {
            Dest::StyleName => {
                // 样式组的尾巴上有一个分号（`{\s17 Heading 1;}`），它是标记不是名字的一部分。
                let raw = frame.capture.trim();
                let name = raw.strip_suffix(';').unwrap_or(raw).trim();
                if let Some(id) = frame.style_id
                    && !name.is_empty()
                {
                    self.styles.insert(id, name.to_string());
                }
            }
            Dest::InfoValue => {
                let value = frame.capture.trim();
                if value.is_empty() {
                    return;
                }
                match frame.info_key {
                    Some("author") => self.authors.push(value.to_string()),
                    Some("title") if self.title.is_none() => {
                        self.title = Some(value.to_string());
                    }
                    _ => {}
                }
            }
            Dest::Pict => {
                if let Some(buf) = frame.pict {
                    self.finish_pict(buf);
                }
            }
            _ => {}
        }
        // 组尾要把行内格式还原成父组那一套。已经写下的 `<strong>` 是事实，
        // 父组的格式只影响接下来攒的那一截。
        let (flags, fs) = {
            let frame = self.frame();
            (frame.flags, frame.fs)
        };
        self.para.set_run(flags, fs);
    }

    /// `\pict` 那一组读完了：解出图片、登记元数据、在正文里放一个 `<img>`。
    fn finish_pict(&mut self, buf: PictBuf) {
        let raw_len = buf.data.len();
        let Some((ext, media_type, bytes)) = buf.resolve() else {
            if raw_len > 0 {
                tracing::debug!("rtf: 丢一张插图（{raw_len} 字节）：认不出格式");
            }
            return;
        };
        let (width, height) = image_dims(&bytes);
        let href = format!("pict-{}.{}", self.images.len(), ext);
        self.images.push(Pict {
            href: href.clone(),
            media_type,
            bytes,
            width,
            height,
        });
        // `\info` / 样式名那一路上冒出来的图不进正文（它不属于任何段落）。
        if self.in_body() {
            self.para.image(&href);
        }
    }

    fn emit_char(&mut self, ch: char) {
        self.emit_after_bytes(ch.encode_utf8(&mut [0u8; 4]));
    }

    fn emit_str(&mut self, text: &str) {
        self.emit_after_bytes(text);
    }

    ///  emit 一个新文字之前，先把攒着的码页字节结算掉。
    ///
    /// 不这么做就会得到「a如果」这种顺序错乱的正文：一个汉字写成 `\'d6\'d0` 两串
    /// 十六进制，后面紧跟一个裸 ASCII 字母时，字母会先一步进正文，而那串字节要等
    /// 下一个边界才解出来。裸 ASCII 在 RTF 里到处是（英文、数字、标点、空格），
    /// 所以这不是罕见形态。
    fn emit_after_bytes(&mut self, text: &str) {
        if !self.pending.is_empty() {
            self.flush_pending();
        }
        self.emit(text);
    }

    /// 一段文字进哪儿：正文、样式名、`\info` 的值，还是丢掉。
    fn emit(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.dest() {
            Dest::Body => {
                let (flags, fs) = {
                    let frame = self.frame();
                    (frame.flags, frame.fs)
                };
                self.para.set_run(flags, fs);
                self.para.push(text);
            }
            Dest::StyleName | Dest::InfoValue => self.frame_mut().capture.push_str(text),
            Dest::Skip | Dest::Pict | Dest::Stylesheet | Dest::Info => {}
        }
    }

    /// 一个**码页字节**：`\uN` 的回退写法先扔掉，图片数据优先，其余进等解码的缓冲。
    fn byte(&mut self, b: u8) {
        // 这一条必须在最前面：`\'hh` 是直接调用进来的（不经过主循环那一支），
        // 而真书里的中文正文**全是** `\'hh` —— 少这一步就等于每个汉字后面多跟一个错字。
        if self.fallback > 0 {
            self.fallback -= 1;
            return;
        }
        if let Some(buf) = self.pict_buf_mut() {
            buf.data.push(b);
            return;
        }
        self.pending.push(b);
    }

    /// 把攒着的码页字节解成文字。
    ///
    /// 每一个「不是码页字节」的事件之前都要先解一次，这样解出来的文字**顺序**与它在
    /// 文件里的位置一致，也保证一个多字节汉字不会被组边界劈成两半各解一次
    /// （那会得到两个替换字符，而屏幕上的表现是书名那里少一个字）。
    fn flush_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.pending);
        let encoding = self.codepage;
        let (text, _, had_errors) = encoding.decode(&bytes);
        if had_errors {
            tracing::debug!(
                "rtf: 有 {} 个字节按文件声明的码页（{}）解不动，按替换字符收下",
                bytes.len(),
                encoding.name()
            );
        }
        self.emit(&text);
    }

    fn end_para(&mut self) {
        self.flush_pending();
        let style = self.frame().style;
        if let Some(para) = self.para.finish(style) {
            self.paras.push(para);
        }
    }

    /// 一个 `\uN`：一个 Unicode 码点。紧跟其后的 `\ucN` 个字节是它的回退写法，要扔掉。
    fn unicode(&mut self, param: i64) {
        self.fallback = self.uc;
        // `\uN` 的参数是一个 **16 位**数，按有符号写（`0xFFFF` 写成 `-1`）。
        let unit = param as u16;
        let code = match unit {
            // 代理对：高位要等下一个 `\uN` 来配，中间的回退字节不许打断这个等待。
            0xd800..=0xdbff => {
                self.high_surrogate = Some(unit);
                return;
            }
            0xdc00..=0xdfff => match self.high_surrogate.take() {
                Some(high) => {
                    0x1_0000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(unit) - 0xdc00)
                }
                // 孤零零一个低位代理：这文件坏了。丢掉它，别把已经读到的内容一起带崩。
                None => return,
            },
            other => u32::from(other),
        };
        if let Some(ch) = char::from_u32(code) {
            self.emit_char(ch);
        }
    }

    /// 剩下的那一堆控制字。
    ///
    /// 带参数的未知控制字（`\cf2`、`\trowd`、`\picwgoal1440`……）什么都不产，只有
    /// RTF 1.9 的 `\Nnnnnn`（32 位码点）例外；不带参数的那一半多半是一个命名字符。
    fn other_word(&mut self, name: &str, param: i64, has_param: bool) {
        if has_param {
            if name == "N" {
                self.fallback = self.uc;
                if let Some(ch) = char::from_u32(param.max(0) as u32) {
                    self.emit_char(ch);
                }
            }
            return;
        }
        let ch = match name {
            "emdash" => '—',
            "endash" => '–',
            "bullet" => '•',
            "ldblquote" | "ldquote" => '“',
            "rdblquote" | "rdquote" => '”',
            "lquote" => '‘',
            "rquote" => '’',
            "ellipsis" => '…',
            "degree" => '°',
            // 方向标记与零宽连接符在屏幕上没有对应物（写一个空格进去会让每段多一个空格）。
            "ltrmark" | "rtlmark" | "ltrpar" | "rtpar" | "zwj" | "zwnj" => return,
            // 认不得的名字：丢标记、留着内容（同控制符号那一条）。
            _ => return,
        };
        self.emit_char(ch);
    }

    fn finish_doc(mut self) -> RtfDoc {
        // 文件在半截处断了（缺闭合括号）时，读到的内容不能跟着一起丢：把还在积累的
        // 那一段收口，栈里没关的那些目标组就让它去。
        self.end_para();
        RtfDoc {
            paras: self.paras,
            styles: self.styles,
            images: self.images,
            title: self.title,
            authors: self.authors,
        }
    }
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Windows 码页号 → 解码器。
///
/// 只列真书里会出现的那几个，认不出的**一律按 cp1252**（RTF 的 `\ansi` 在 PC 上的默认）。
/// 在这里回 `None` 就得决定「解不出怎么办」，而那一档最坏的下场是丢正文；
/// 按 cp1252 解的下场最多是几个替换字符，后面的内容照读。
fn encoding_for_codepage(cp: i64) -> &'static encoding_rs::Encoding {
    use encoding_rs as e;
    match cp {
        // 简体：GB2312 / GBK（都写作 936）与 CP937 归到 WHATWG 那一支 gbk，
        // 它的解码器能吃下 GB18030 的字节。
        936 | 937 => e::GBK,
        54936 => e::GB18030,
        950 => e::BIG5,
        932 => e::SHIFT_JIS,
        949 => e::EUC_KR,
        65001 => e::UTF_8,
        // WHATWG 那张表里没有 ISO-8859-1 这一支（`latin1` / `iso-8859-1` 这些标签都
        // 折到 windows-1252），所以 28591 落到最后那条默认，与上面 `\ansi` 同一档。
        1250 => e::WINDOWS_1250,
        1251 => e::WINDOWS_1251,
        1253 => e::WINDOWS_1253,
        1254 => e::WINDOWS_1254,
        1255 => e::WINDOWS_1255,
        1256 => e::WINDOWS_1256,
        1257 => e::WINDOWS_1257,
        1258 => e::WINDOWS_1258,
        _ => e::WINDOWS_1252,
    }
}

fn escape_into(out: &mut String, ch: char) {
    match ch {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        _ => out.push(ch),
    }
}

/// 书名：`\info` 里的 `\title` 优先（Word 与 WPS 存 RTF 都会写），
/// 退到**不是标题的**第一段文字（RTF 书的第一行常常就是书名），再退到文件名。
///
/// 「不是标题的」那个限定是必要的：一个没有 `\info` 的文件若以「第一章 ××」开头，
/// 把章名当书名会让书架上一整排书都叫第一章。
fn book_title(doc: &RtfDoc, path: &Path) -> String {
    if let Some(title) = &doc.title
        && !title.trim().is_empty()
    {
        return title.trim().to_string();
    }
    let body_fs = body_font_size(&doc.paras);
    if let Some(first) = doc.paras.first()
        && heading_level(first, body_fs, &doc.styles).is_none()
    {
        let text = first.text.trim();
        if !text.is_empty() && text.chars().count() <= MAX_TITLE_CHARS {
            return text.to_string();
        }
    }
    stem_of(path)
}

/// 全书正文的字号（半磅）= 按字符数加权的众数。`0` = 文件里根本没写过 `\fs`。
///
/// 用众数而不是「出现过的最小值」：正文是全书字符的大头，标题只占几个字 ——
/// 拿任何一个别的口径当基准，都可能把基准定到标题那一档去，于是每一段都像标题。
fn body_font_size(paras: &[Para]) -> u16 {
    let mut hist: HashMap<u16, usize> = HashMap::new();
    for para in paras {
        if para.fs > 0 && !para.text.is_empty() {
            *hist.entry(para.fs).or_default() += para.text.chars().count();
        }
    }
    hist.into_iter()
        .max_by_key(|(_, chars)| *chars)
        .map_or(0, |(fs, _)| fs)
}

/// 这一段是不是标题、是的话第几级。判据都不要求文件写过任何「章节」结构。
///
/// 先过两道门：**够短**（[`MAX_HEADING_CHARS`]）且**不以任何句末标点收尾**。
/// 后者专门挡「一本书的第一页有一句放大加粗的话」那形态 —— 它字号够大、又不以句号结尾，
/// 没有这道门就会变成整本书的第一章。
///
/// 1. `\stylesheet` 里这一段用的样式是标题类样式（`Heading 1` / `标题 2` / `题名`）；
/// 2. 文字本身长成章名（`第一章`、`第 12 节`、`Chapter 3`、`Part II`）；
/// 3. 字号比正文众数大五分之一以上；
/// 4. 整段粗体、不超过 [`MAX_BOLD_HEADING_CHARS`] 字、且不小于正文字号 ——
///    抓「楔子」「尾声」这种没有数词的小标题。
fn heading_level(para: &Para, body_fs: u16, styles: &HashMap<usize, String>) -> Option<u8> {
    let chars = para.text.chars().count();
    if chars == 0 || chars > MAX_HEADING_CHARS || para.inner.contains("<img") {
        return None;
    }
    if ends_sentence(&para.text) {
        return None;
    }
    if let Some(style) = para.style
        && let Some(name) = styles.get(&style)
        && let Some(level) = heading_in_style(name)
    {
        return Some(level);
    }
    if let Some(level) = heading_in_text(&para.text) {
        return Some(level);
    }
    if body_fs == 0 {
        // 全书没写过字号：只剩「整段粗体且很短」这一条还能用，否则一律不分章。
        return if para.all_bold && chars <= MAX_BOLD_HEADING_CHARS {
            Some(1)
        } else {
            None
        };
    }
    if para.fs >= body_fs.saturating_mul(6) / 5 {
        return Some(1);
    }
    if para.all_bold && chars <= MAX_BOLD_HEADING_CHARS && para.fs >= body_fs {
        return Some(1);
    }
    None
}

/// 句末标点：以它们收尾的那一段是一句话，不是标题。
fn ends_sentence(text: &str) -> bool {
    text.trim_end()
        .chars()
        .next_back()
        .is_some_and(|c| "。！？；，、,.!?;:…".contains(c))
}

/// 样式名说这是标题。Word 的中英文写法都认。
fn heading_in_style(name: &str) -> Option<u8> {
    let lower = name.trim().to_ascii_lowercase();
    const MARKS: [&str; 6] = ["heading", "标题", "title", "题名", "书名", "章标题"];
    if !MARKS.iter().any(|mark| lower.contains(mark)) {
        return None;
    }
    Some(trailing_level(&lower))
}

/// 文字本身长成章名。层级：卷 / 部 / 篇 / 辑 / 册 算一级，章 / 节 / 回 / 集 / 话 /
/// 幕 / 场 / 折 算二级（两级都开一章，只是标题的字号不同）。
fn heading_in_text(text: &str) -> Option<u8> {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix('第') {
        let chars: Vec<char> = rest.chars().collect();
        let mut i = 0;
        while i < chars.len()
            && (chars[i].is_ascii_digit()
                || chars[i].is_whitespace()
                || "零一二三四五六七八九十百千万两〇".contains(chars[i]))
        {
            i += 1;
        }
        // 数词必须有，而且后面得真的跟着一个单位字（「第三，然后是别的」不是）。
        if i == 0 || i >= chars.len() {
            return None;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let level = match chars.get(i)? {
            '卷' | '部' | '篇' | '辑' | '册' => 1,
            '章' | '节' | '回' | '集' | '话' | '幕' | '场' | '折' => 2,
            _ => return None,
        };
        // 单位后面跟什么都允许（「第一章 楔子」与「第一章楔子」都是章名）：
        // 一句话被误判的那两种形态由长度上限与句末标点去挡（见 [`heading_level`]）。
        return Some(level);
    }
    // 英文那一族：`Chapter 12` / `Part II` / `Book 3`。
    let first = trimmed.split_whitespace().next()?;
    let rest = trimmed[first.len()..].trim_start();
    if rest.is_empty() || rest.chars().count() > MAX_HEADING_CHARS {
        return None;
    }
    matches!(
        first.to_ascii_lowercase().as_str(),
        "chapter" | "part" | "book" | "volume" | "canto"
    )
    .then_some(1)
}

/// 一串文字末尾那段数字；没有就是 1。
fn trailing_level(text: &str) -> u8 {
    let digits: String = text
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse::<u8>().map_or(1, |d| d.clamp(1, 6))
}

/// 段表 → 章表。产物已经是引擎就绪的 XHTML（这条契约由 `format::contract` 那份测试钉着）。
fn split_chapters(doc: &RtfDoc, title: &str) -> Vec<Chapter> {
    let body_fs = body_font_size(&doc.paras);
    let mut chapters: Vec<Chapter> = Vec::new();
    let mut buf = String::new();
    let mut current: Option<String> = None;
    for para in &doc.paras {
        match heading_level(para, body_fs, &doc.styles) {
            Some(level) => {
                // 首个标题之前那截叫「前言」（与 Markdown 同一说法）。
                flush(&mut chapters, &mut buf, current.take(), "前言");
                current = Some(para.text.trim().to_string());
                block(&mut buf, para, Some(level));
            }
            None => block(&mut buf, para, None),
        }
    }
    // 全书一个标题都没有时，那一章的名字就是书名本身。
    flush(&mut chapters, &mut buf, current.take(), title);
    chapters
}

/// 一段落成一块：标题用 `<hN>`，正文用 `<p>`。
fn block(buf: &mut String, para: &Para, heading: Option<u8>) {
    let tag = match heading {
        Some(level) => format!("h{level}"),
        None => "p".to_string(),
    };
    buf.push('<');
    buf.push_str(&tag);
    buf.push('>');
    buf.push_str(&para.inner);
    buf.push_str(&format!("</{tag}>\n"));
}

/// 收尾一章。**空的不收**：一个标题后面既没文字也没图，就不要在目录里长出一页空白。
fn flush(
    chapters: &mut Vec<Chapter>,
    buf: &mut String,
    title: Option<String>,
    default_title: &str,
) {
    if buf.trim().is_empty() {
        buf.clear();
        return;
    }
    let body = std::mem::take(buf);
    let name = title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| default_title.to_string());
    // 包一层 `<body>`：排版层从 body 往下找正文（与 `fb2::flush` 同一做法）。
    chapters.push(Chapter::inline(name, format!("<body>{body}</body>")));
}

#[cfg(test)]
mod tests {
    //! RTF：码页、`\ucN` 回退、跳过目标、`\bin` 不错位、分章判据、插图按需现取。
    //!
    //! 每一条都拿**真文件里的那串字节**喂（走公开的 `parse` / provider，不碰内部函数），
    //! 因为要钉的正是「一趟扫描读回来的东西」。

    use std::path::{Path, PathBuf};

    use super::{
        RtfAdapter, body_font_size, ends_sentence, heading_in_style, heading_in_text,
        heading_level, parse,
    };
    use crate::content::format::{Content, DocMeta, DocumentProvider, FlowDoc, Format};

    fn write_rtf(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    /// 测试里只写正文，外层 `{\rtf1\ansi\ansicpg65001 …}` 由这里补 —— 因为下面这些串
    /// 里的中文是直接写进源码的 UTF-8 字节，那正是 `\ansicpg65001` 那一形态。
    /// 码页专项的测试自己声明 `\ansicpg936`（后写的声明赢）。
    fn doc(inner: &str) -> super::RtfDoc {
        parse(format!("{{\\rtf1\\ansi\\ansicpg65001 {inner}}}").as_bytes())
    }

    /// 各段的可见文字，一段一格。
    fn texts(inner: &str) -> Vec<String> {
        doc(inner)
            .paras
            .iter()
            .map(|para| para.text.clone())
            .collect()
    }

    fn flow(path: &Path) -> (DocMeta, FlowDoc) {
        let (meta, content) = RtfAdapter.read(path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("RTF 该读成可重排");
        };
        (meta, flow)
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([9, 9, 9, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    /// 字节 → RTF 的十六进制写法（`\'hh`）。真文件里的图片数据就是这么写的。
    fn hex_literal(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|b| format!("\\'{b:02x}"))
            .collect::<String>()
    }

    #[test]
    fn it_claims_rtf_and_reports_the_format() {
        assert_eq!(RtfAdapter.format(), Format::Rtf);
        assert!(RtfAdapter.supports(Path::new("a.rtf")));
        assert!(RtfAdapter.supports(Path::new("A.RTF")));
        assert!(!RtfAdapter.supports(Path::new("a.epub")));
    }

    /// **中文书最常见的那一形态**：`\ansicpg936` + GBK 双字节的 `\'hh`。
    /// 这一条断了就是一整本中文书读不出来。
    #[test]
    fn gbk_hex_escapes_decode_by_the_declared_codepage() {
        // 中文 = D6D0 CEC4（GBK 的「文」是 CEC4，不是 CEC8 —— 后者是「稳」）。
        let parsed = parse(r"{\rtf1\ansi\ansicpg936\pard\fs21 \'d6\'d0\'ce\'c4\par}".as_bytes());
        assert_eq!(parsed.paras[0].text, "中文", "{:?}", parsed.paras[0].text);
    }

    /// `\uN` 是主写法，后面那 `\ucN` 个字节是回退，**不许同时收两次**。
    #[test]
    fn unicode_escapes_take_the_codepoint_and_drop_the_fallback() {
        let parsed = parse(
            r"{\rtf1\ansi\ansicpg936\uc2 \pard\fs21 \u20013\'d6\'d0\u25991\'ce\'c4\par}".as_bytes(),
        );
        assert_eq!(parsed.paras[0].text, "中文", "{:?}", parsed.paras[0].text);
    }

    /// `\uc1` 的回退字符也可能是裸 ASCII（Word 写给非 ASCII 标点常用那一形）。
    #[test]
    fn a_single_ascii_fallback_char_is_dropped_too() {
        let parsed = parse(r"{\rtf1\ansi\uc1 \pard \u233?\u232?\par}".as_bytes());
        assert_eq!(parsed.paras[0].text, "éè", "{:?}", parsed.paras[0].text);
    }

    /// 代理对（emoji、CJK 扩展 B）：两个 `\uN` 合起来才是一个字。
    #[test]
    fn a_surrogate_pair_becomes_one_character() {
        // U+20BB7「𠮷」：高位 0xD842 写作 -10174，低位 0xDFB7 写作 -8265。
        let parsed = parse(r"{\rtf1\ansi\uc0 \pard \u-10174\u-8265\par}".as_bytes());
        assert_eq!(
            parsed.paras[0].text, "\u{20bb7}",
            "{:?}",
            parsed.paras[0].text
        );
    }

    /// 不认识的目标一律跳过，**包括那些不带 `*` 的**：`{\colortbl ;…;}` 里的分号是裸文本。
    ///
    /// 样本写成一行是**必要的**，不是为了好看：RTF 源码里的换行是行尾（丢掉），
    /// 而**空格是正文**——多行缩进的那一串空格会老实落到第一个段落前面。
    #[test]
    fn unknown_and_known_destinations_leak_nothing_into_the_body() {
        let inner = concat!(
            r"{\fonttbl{\f0\fnil SimSun;}}",
            r"{\colortbl ;\red0\green0\blue0;}",
            r"{\*\datastore 01 05 00 00}",
            r"{\header 这是页眉不该出现}",
            r"{\footnote 这是注文不该出现}",
            r"{\stylesheet{\s1\snext0 heading 1;}}",
            r"\pard\fs21 正文\par",
        );
        let parsed = doc(inner);
        assert_eq!(parsed.paras.len(), 1, "{:?}", parsed.paras);
        assert_eq!(parsed.paras[0].text, "正文", "{:?}", parsed.paras[0].text);
        // `\stylesheet` 的样式名是要留的：分章判据靠它。
        assert_eq!(parsed.styles.get(&1).map(String::as_str), Some("heading 1"));
    }

    /// **`\binN` 后面是裸字节**（可以含 NUL，也可以长得像 `{` 与 `}`）。不吃掉整 N 个，
    /// 后面整本都会读歪 —— 症状是书读到一半没了，而屏幕上没有任何一处说它坏了。
    #[test]
    fn bin_bytes_are_consumed_exactly_and_never_leak_into_text() {
        let head = r"{\rtf1\ansi\ansicpg65001\pard\fs21 正文一{\pict\wmetafile8\bin5";
        let tail = r"}正文二\par}";
        let mut body = head.as_bytes().to_vec();
        body.extend_from_slice(&[0x00, 0x01, b'}', b'{', 0x00]);
        body.extend_from_slice(tail.as_bytes());
        let parsed = parse(&body);
        let collected: Vec<&str> = parsed.paras.iter().map(|para| para.text.as_str()).collect();
        assert_eq!(collected, vec!["正文一正文二"], "{collected:?}");
        assert!(parsed.images.is_empty(), "WMF 解不动，一张都不该登记");
    }

    /// 一个坏文件不许让整个打不开：读到哪算哪。
    #[test]
    fn a_truncated_file_still_yields_what_it_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_rtf(
            dir.path(),
            "半个.rtf",
            r"{\rtf1\ansi\ansicpg65001\pard\fs21 第一段\par 读到一半",
        );
        let (meta, flow) = flow(&path);
        // 书名回落的第一档是**第一个不是标题的段落**（真 RTF 的第一行常常就是书名），
        // 文件名只在那一档也拿不到时才用（见 [`an_empty_document_is_one_empty_chapter_not_an_error`]）。
        assert_eq!(meta.title, "第一段");
        assert_eq!(flow.chapters.len(), 1);
        assert!(flow.chapters[0].source().contains("读到一半"));
    }

    /// 空文件、只有文件头那种：给一章空的，而不是报错，也不是零章。
    /// 这一段一个字符都没有，所以书名只能退到文件名。
    #[test]
    fn an_empty_document_is_one_empty_chapter_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        for (name, stem, body) in [
            ("a.rtf", "a", r"{\rtf1}"),
            ("b.rtf", "b", ""),
            ("c.rtf", "c", r"{\rtf1\ansi"),
            ("d.rtf", "d", r"{\rtf1\ansi\pard\par}"),
        ] {
            let path = write_rtf(dir.path(), name, body);
            let (meta, flow) = flow(&path);
            assert_eq!(flow.chapters.len(), 1, "{name}");
            assert_eq!(flow.chapters[0].source(), "<body></body>", "{name}");
            assert_eq!(meta.title, stem, "{name}：一个字符都没有时退到文件名");
        }
    }

    #[test]
    fn paragraphs_and_hard_breaks_and_tabs() {
        assert_eq!(
            texts(r"\pard 第一\par 第二\line 还是第二\par 第三\par"),
            vec!["第一", "第二还是第二", "第三"]
        );
        // `\line` 是段内硬换行 → `<br/>`；`\tab` 是一个真的制表字符。
        let parsed = doc(r"\pard 甲\line 乙\par");
        assert!(parsed.paras[0].inner.contains("<br/>"));
        assert_eq!(texts(r"\pard 一\tab 二\par"), vec!["一\t二"]);
    }

    /// `{\pard\par}` 那种空段（真文件里每段后面都拖一个）不许变成章。
    #[test]
    fn empty_paragraphs_are_dropped() {
        assert_eq!(texts(r"\pard 正文\par{\pard\par}{\pard\par}"), vec!["正文"]);
    }

    #[test]
    fn inline_formatting_becomes_xhtml_tags_and_nests_cleanly() {
        let parsed = doc(r"\pard 前\b 粗\i0\b0 后\i 斜\i0\par");
        assert_eq!(parsed.paras.len(), 1);
        let inner = &parsed.paras[0].inner;
        assert_eq!(inner, "前<strong>粗</strong>后<em>斜</em>", "{inner}");
        assert_eq!(
            doc(r"\pard\i 斜体一段\par").paras[0].inner,
            "<em>斜体一段</em>"
        );
    }

    /// 字符格式按**组**还原，不按段：真文件里每一段都写在自己的组里
    /// （`{\pard … \par}`），所以上一段的粗体不该漏到下一段。
    /// 反过来，同一个组里连着两段没有 `\b0` 时，粗体照规矩继续 —— 那不是 bug。
    #[test]
    fn character_formatting_is_restored_at_the_group_boundary() {
        let grouped = doc(r"{\pard\b 甲\par}{\pard 乙\par}");
        assert_eq!(grouped.paras[1].inner, "乙", "{:?}", grouped.paras[1].inner);
        let same_group = doc(r"\pard\b 甲\par 乙\par");
        assert_eq!(same_group.paras[0].inner, "<strong>甲</strong>");
        assert_eq!(same_group.paras[1].inner, "<strong>乙</strong>");
    }

    /// 书名与作者从 `\info` 来 —— 这是 RTF 唯一一处正经的元数据。
    #[test]
    fn metadata_comes_from_the_info_destination() {
        let inner = r"{\info{\title Test Book}{\author Leo Tolstoy}{\author Second Author}}
                     \pard\fs21 正文\par";
        let parsed = doc(inner);
        assert_eq!(parsed.title.as_deref(), Some("Test Book"));
        assert_eq!(parsed.authors, vec!["Leo Tolstoy", "Second Author"]);
    }

    #[test]
    fn the_body_font_size_is_the_char_weighted_mode() {
        let parsed =
            doc(r"\pard\fs28 大标题\par\pard\fs21 正文很长的一段\par\pard\fs21 又一段\par");
        assert_eq!(body_font_size(&parsed.paras), 21);
        // 全书没写过 `\fs`：回 0，字号那条判据随之不生效。
        assert_eq!(body_font_size(&doc(r"\pard 正文\par").paras), 0);
    }

    /// 判据得能举出**反例**，否则「按字号分章」会变成「按心情分章」。
    #[test]
    fn heading_rules_and_their_counterexamples() {
        assert_eq!(heading_in_text("第一章 楔子"), Some(2));
        assert_eq!(heading_in_text("第 12 节"), Some(2));
        assert_eq!(heading_in_text("第二百五十回"), Some(2));
        assert_eq!(heading_in_text("第一章这里没有空格也是章名"), Some(2));
        assert_eq!(heading_in_text("Chapter 3"), Some(1));
        // 反例：没有「第」前缀的不猜；数词后面不是单位字的不算；「规章制度」里的
        // 「章」不是章名。
        assert_eq!(heading_in_text("卷下"), None);
        assert_eq!(heading_in_text("第三，然后是别的"), None);
        assert_eq!(heading_in_text("规章制度很重要"), None);
        assert_eq!(heading_in_text("Chapter"), None, "后面没有编号");

        assert_eq!(heading_in_style("heading 2"), Some(2));
        assert_eq!(heading_in_style("标题 3"), Some(3));
        assert_eq!(heading_in_style("Normal"), None);

        // 句末标点那一道门：放大加粗的**一句正文**不是标题。
        assert!(ends_sentence("这是第一句话。"));
        assert!(ends_sentence("走开！"));
        assert!(!ends_sentence("第一章"));
    }

    /// 「字号大 + 没有句末标点」的一句正文仍然是章；以句号收尾的那一句不是。
    #[test]
    fn a_big_sentence_ending_with_a_period_is_not_a_chapter() {
        let parsed = doc(r"\pard\fs40 这是开篇的一句。\par\pard\fs21 正文\par");
        let body = body_font_size(&parsed.paras);
        assert_eq!(
            heading_level(&parsed.paras[0], body, &parsed.styles),
            None,
            "以句号收尾的一整句不该变成章"
        );
    }

    /// 真书里那三形态：样式标题、`第X章`、放大字号；外加「前言」那一章。
    #[test]
    fn chapters_split_on_the_three_heading_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let body = r"{\rtf1\ansi\ansicpg65001\deff0
             {\stylesheet{\s17\snext0 heading 1;}}
             \paperw11906\deflang1033
             \pard\qc\fs21 楔子\par
             {\pard\qc\s17\fs28 名称\par}
             \pard\fs21 正文一\par
             {\pard\fs36 第二章阅读\par}
             \pard\fs21 正文二\par}";
        let path = write_rtf(dir.path(), "书.rtf", body);
        let (meta, flow) = flow(&path);
        // 「楔子」字号没变也不粗 → 不是标题，它是「前言」那一章的内容，也是书名回落的来源。
        assert_eq!(meta.title, "楔子");
        let titles: Vec<&str> = flow
            .chapters
            .iter()
            .map(|chapter| chapter.title.as_str())
            .collect();
        assert_eq!(titles, vec!["前言", "名称", "第二章阅读"], "{titles:?}");
        // 样式标题（s17 → heading 1）认出来了，标题留在本章开头。
        assert!(
            flow.chapters[1].source().starts_with("<body><h1>名称</h1>"),
            "{}",
            flow.chapters[1].source()
        );
        // 正文一用的是正文字号、也不粗，所以它归在前一章里，没有变成一章。
        assert!(flow.chapters[1].source().contains("<p>正文一</p>"));
        // 字号标题（fs36 ≥ 21×6/5）与「第二章」那条文字判据是同一段，任一条命中即可；
        // 层级由文字判据给（章/节/回 = 2，卷/部/篇 = 1），所以这里是 `<h2>` 而不是 `<h1>`。
        assert!(
            flow.chapters[2].source().contains("<h2>第二章阅读</h2>"),
            "{}",
            flow.chapters[2].source()
        );
        assert!(flow.chapters[2].source().contains("<p>正文二</p>"));

        // 索引那一趟：正文不在手上，但字数要留着（全局进度那条分母）。
        let (_, indexed) = RtfAdapter.index(&path).unwrap();
        let Content::Reflow(indexed) = indexed else {
            panic!("RTF 该读成可重排");
        };
        assert_eq!(indexed.chapters.len(), flow.chapters.len());
        for chapter in &indexed.chapters {
            assert!(chapter.locator().is_some(), "{}", chapter.title);
            assert!(chapter.char_len().is_some(), "{}", chapter.title);
        }
        assert!(indexed.chapters[0].source().is_empty());
    }

    /// 全书一个标题都没有：整篇一章，章名就是书名（与单文件 HTML 同一档）。
    #[test]
    fn a_document_without_any_heading_is_one_chapter() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_rtf(
            dir.path(),
            "散文.rtf",
            r"{\rtf1\ansi\ansicpg65001\pard\fs21 第一段\par 第二段\par}",
        );
        let (meta, flow) = flow(&path);
        assert_eq!(meta.title, "第一段");
        assert_eq!(flow.chapters.len(), 1);
        assert_eq!(flow.chapters[0].title, "第一段");
        assert!(flow.chapters[0].source().contains("<p>第二段</p>"));
    }

    /// 插图：`\pict` 的十六进制解出来，登记成元数据，`<img>` 进正文，字节按 href 现取。
    #[test]
    fn embedded_pngs_are_registered_and_read_on_demand() {
        let dir = tempfile::tempdir().unwrap();
        let image = png(30, 10);
        let body = format!(
            "{{\\rtf1\\ansi\\pard\\fs21 前{{\\pict\\pngblip\\picwgoal1440 {}}}后\\par}}",
            hex_literal(&image)
        );
        let path = write_rtf(dir.path(), "a.rtf", &body);
        let (_, flow) = flow(&path);
        assert_eq!(flow.images.len(), 1);
        assert_eq!(flow.images[0].href, "pict-0.png");
        assert_eq!(flow.images[0].media_type, "image/png");
        assert_eq!((flow.images[0].width, flow.images[0].height), (30, 10));
        assert!(
            flow.chapters[0]
                .source()
                .contains("<img src=\"pict-0.png\"/>"),
            "{}",
            flow.chapters[0].source()
        );
        assert_eq!(
            RtfAdapter.image_bytes(&path, "pict-0.png").unwrap(),
            image,
            "按 href 现取要拿回原样的字节"
        );
        assert!(RtfAdapter.image_bytes(&path, "pict-9.png").is_none());
    }

    /// 没写 `\pngblip` 的按魔数认；JPEG 也照样登记。
    #[test]
    fn picture_type_is_sniffed_from_the_magic_when_the_group_omits_it() {
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
        let body = format!(
            "{{\\rtf1\\ansi\\pard 一段{{\\pict {}}}\\par}}",
            hex_literal(&jpeg)
        );
        let parsed = parse(body.as_bytes());
        assert_eq!(parsed.images.len(), 1);
        assert_eq!(parsed.images[0].media_type, "image/jpeg");
        assert_eq!(parsed.images[0].href, "pict-0.jpg");
    }

    /// Word 同一张图写新老两份（`{\*\shppict{\pict…}}` 与 `{\nonshppict{\pict…}}`），
    /// 只该收一张。
    #[test]
    fn the_legacy_picture_copy_is_not_collected_twice() {
        let hex = hex_literal(&png(4, 4));
        // 拼串用 `concat` 而不是 `format!`：RTF 那一大堆花括号在 `format!` 里每个都要
        // 写两遍，而少写一个的下场是「unmatched `}`」这种与语义毫无关系的编译错误。
        let body = [
            r"{\rtf1\ansi\ansicpg65001{\*\shppict{\pict\jpegblip ",
            &hex,
            r"}}{\nonshppict{\pict\pngblip ",
            &hex,
            r"}}\pard 正文\par}",
        ]
        .concat();
        let parsed = parse(body.as_bytes());
        assert_eq!(parsed.images.len(), 1, "{:?}", parsed.images);
        assert_eq!(parsed.images[0].href, "pict-0.png");
        assert_eq!(parsed.paras[0].text, "正文", "{:?}", parsed.paras[0].text);
    }

    /// 正文里的尖括号必须转义，否则 html5ever 会把它当标签吃掉一段。
    #[test]
    fn angle_brackets_in_the_text_are_escaped() {
        let parsed = doc(r"\pard 如果 a<b 且 c>d\par");
        assert!(
            parsed.paras[0].inner.contains("&lt;"),
            "{}",
            parsed.paras[0].inner
        );
        assert_eq!(parsed.paras[0].text, "如果 a<b 且 c>d");
    }

    /// 坏到极点的那些输入：不许 panic。
    #[test]
    fn malformed_bytes_never_panic() {
        for body in [
            r"{\rtf1\ansi\pard \'z\par",
            r"{\rtf1\ansi\pard \'4\par 正文\par}",
            r"{\rtf1\ansi\pard\fs\par 正文\par}}}}",
            r"{\rtf1\ansi \u-1\u-2\u9999999999999999999999\par}",
            r"{\rtf1\ansi\bin99999999 正文\par}",
            r"{\rtf1\ansicpg999999\pard 正文\par}",
            r"{\rtf1\ansicpg936\pard 正文\par}",
            r"{\rtf1\ansi\pict",
            r"{\rtf1\ansi\ansicpg936\pard \'d6\par}",
            "}]",
            "{",
            "\\",
        ] {
            let _ = parse(body.as_bytes());
        }
    }

    /// 一个坏的 `\'` 不许把后面的正文一起带走。
    ///
    /// 只要求「后面的还在」：`\'` 后面那两个字符按规矩被当十六进制位吃掉了，所以第二个
    /// 逃生的开头会错位 —— 一个坏文件里的**局部**错位，不是丢整段。真实诉求从来是
    /// 「别把整本读没」，那一条这里钉得住。
    #[test]
    fn a_bad_escape_does_not_lose_the_rest() {
        let parsed = parse(r"{\rtf1\ansi\ansicpg65001\pard \'z\'4 正文\par}".as_bytes());
        assert_eq!(parsed.paras.len(), 1, "{:?}", parsed.paras);
        assert!(
            parsed.paras[0].text.contains("正文"),
            "{:?}",
            parsed.paras[0].text
        );
    }

    /// 分章不许丢字：各章的可见文本拼起来（去掉空白）必须等于全书段落文字拼起来。
    ///
    /// 这条断了的表现是读者翻过某一章发现少了一段，而目录里看不出任何异常 ——
    /// 与 `format::contract` 里「包装不增删一个可见字符」是同一条不变量的另一头：
    /// 那边管标记包装，这边管**切分**。
    ///
    /// 这一条只看文字：行内 `<img>` 在 `TextMap` 里占一个字符位，而段落文字里没有它，
    /// 插图的去向由 [`embedded_pngs_are_registered_and_read_on_demand`] 那条钉。
    #[test]
    fn chapter_split_loses_no_visible_text() {
        use crate::content::anchor::TextMap;
        use crate::content::dom::Dom;

        fn visible(xhtml: &str) -> String {
            let dom = Dom::parse(xhtml.as_bytes());
            let body = dom
                .elements()
                .find(|(id, _)| dom.tag_name(*id) == Some("body"))
                .map_or(dom.document, |(id, _)| id);
            TextMap::build(&dom, body).text().to_string()
        }
        fn without_whitespace(text: &str) -> String {
            text.chars().filter(|c| !c.is_whitespace()).collect()
        }

        let body = r"{\rtf1\ansi\ansicpg65001
             {\stylesheet{\s17\snext0 heading 1;}}
             \pard\fs21 开篇的一段\par
             {\pard\s17\fs28 第一章\par}
             \pard\fs21\b 粗\b0 与\i 斜\i0 混着的一段\line 段内还换了一行\par
             \pard\fs21 又一段\par
             \pard\fs36 第二章\par
             \pard 收尾的一段\par}";
        let dir = tempfile::tempdir().unwrap();
        let path = write_rtf(dir.path(), "对拍.rtf", body);
        let (_, flow) = flow(&path);
        let joined: String = flow
            .chapters
            .iter()
            .map(|chapter| visible(chapter.source()))
            .collect();
        let expected: String = parse(body.as_bytes())
            .paras
            .iter()
            .map(|para| para.text.as_str())
            .collect();
        assert!(flow.chapters.len() >= 3, "这份样本该切出几章");
        assert_eq!(
            without_whitespace(&joined),
            without_whitespace(&expected),
            "分章改了内容"
        );
    }

    /// `\ansicpg` 声明与字节不符时**照声明**解 —— 这条钉住模块头那条「不猜码页」的立场，
    /// 免得将来有人顺手加一条「解不动就换 GBK」的掷硬币规则。
    #[test]
    fn a_wrong_declared_codepage_is_followed_as_declared() {
        let parsed = parse(r"{\rtf1\ansi\ansicpg1252\pard \'d6\'d0\par}".as_bytes());
        // cp1252 里 0xD6 是 Ö、0xD0 是 Ð：得到两个拉丁字母，而不是偷偷换一张表去猜「中文」。
        assert_eq!(parsed.paras[0].text, "ÖÐ", "{:?}", parsed.paras[0].text);
    }

    /// 表格降级成每格一段：格线不是阅读的必需品，内容才是。
    #[test]
    fn a_table_degrades_to_one_paragraph_per_cell() {
        let parsed = doc(r"\pard\trowd\intbl 左格\cell 右格\cell\row\pard 表格之后\par");
        let collected: Vec<&str> = parsed.paras.iter().map(|para| para.text.as_str()).collect();
        assert_eq!(collected, vec!["左格", "右格", "表格之后"], "{collected:?}");
    }

    /// 列表项前面那个 `{\pntext …\tab}` 是**要显示**的标记，不当目标跳过。
    #[test]
    fn a_list_marker_stays_in_the_text() {
        let parsed = doc(r"\uc0 \pard\fs21 {\pntext\u183\tab}第一项\par");
        assert_eq!(
            parsed.paras[0].text, "·\t第一项",
            "{:?}",
            parsed.paras[0].text
        );
    }

    /// `\uN` 写给 `\info` 里的书名也要能收（书名那一串常常正是 `\u` 形态）。
    #[test]
    fn info_values_written_with_unicode_escapes_are_captured() {
        let parsed = parse(
            r"{\rtf1\ansi\ansicpg936{\info{\uc2 \title \u20013\'d6\'d0\u25991\'ce\'c4}}\pard 正文\par}".as_bytes(),
        );
        assert_eq!(parsed.title.as_deref(), Some("中文"));
    }
}
