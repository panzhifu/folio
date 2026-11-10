//! 章表与书目：格式枚举、章、位置、目录项与图片元数据的**类型**。
//!
//! 从 folio-format 搬来的那一半「词汇」：格式差异（21 个 provider 怎么读文件）留在
//! folio-format，这里只留各家共同交出的形状。与解析绑定的行为也留在那边 ——
//! `copy_extension` 问 provider 注册表、`Chapter::inline` 顺手数一遍字数，都在
//! folio-format 的扩展 trait（[`folio_format::ext`]）里；这里只有构造与访问。

use serde::{Deserialize, Serialize};

/// 本章正文里**登记过**的一张图片引用（`index()` 那一趟要落进 `chapters.images_json`
/// 的那一份）。类型住在模型里（[`Chapter::images`] 的载荷），扫描引擎（HTML tokenizer）
/// 留在 folio-format 的 `scan`。
///
/// 存在的理由是一件排版给不出来的事：**整本书**那一列图要在读者走到它之前就知道
/// 「一共有几张、这一张在第几章第几个字」。排版只认识「已经排好的那一章」，
/// 而这一列要的是清单 —— 导入那一趟已经在逐章跑扫描（folio-format 的 `ChapterScan`），
/// 顺手记下来是一趟扫描的价钱，换掉的是「为下一张图把一章排一遍」。
///
/// `src` 是**未解析**的那一条原样引用（`<img src="images/a.png">` 里那一段），
/// 与界面侧图片表解析出的 `href` 不同：后者要按本章目录解一遍、还要对得上那本书的
/// 图片表。这里存原样是因为扫描器不认容器 —— 而**同一句解析只做一次**才不会出现
/// 「列里画得出、正文里画不出」那种两本账。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageUse {
    /// 作者写下的那一条引用（相对本章目录）。
    pub src: String,
    /// 这一处前面已经数到的可见字符数 —— 与 `Locator.offset` 同一个轴。
    ///
    /// 指到图片**之前**那一个字：图片自己在那个轴上不占一格（它是 U+FFFC 一个替换元素，
    /// 由排版插在文本流里，不由这里的字数器数进来）。差这一个字落在同一个行盒里，
    /// 「定位原文」要的就是那一行。
    pub offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Format {
    Markdown,
    /// `Default` 走这一档：它只服务 `DocMeta { ..Default::default() }` 那种填法，
    /// 真读到一本书的 provider 一定会显式写自己的格式。挑纯文本是因为它最保守 ——
    /// 谁忘了写，症状是「按 TXT 打开」，不是把一本没读出来的书当 EPUB 去解析容器。
    #[default]
    Text,
    Epub,
    /// FictionBook 2：一种 XML 单文件的电子书格式。
    Fb2,
    /// 单个 HTML / XHTML 文件。
    Html,
    /// Comic Book Zip：一个 zip，里面按页排着图片。定版式。
    Cbz,
    /// Rich Text Format：一个 ASCII 文件把正文与格式指令写在一起。可重排。
    Rtf,
    /// Microsoft Compiled HTML Help：一个 ITSF 容器，里面是一组 HTML topic。
    ///
    /// 「一组 HTML + 那份 hhc 的读法」是格式，「ITSF + LZX」是它用的容器 ——
    /// 与 [`Format::Epub`] 躺在 ZIP 上是同一刀分法（`plans/导入管线.md` §十：
    /// 容器家族比格式少）。
    Chm,
    /// Mobipocket / Kindle：PalmDB 容器里的一条正文流（KF7），或 SKEL/FRAG 拼出的多个
    /// 文件（KF8）。`.mobi` / `.azw` / `.azw3` / `.prc` 都归这一族。可重排。
    Mobi,
    /// PDF：页是既定的画布，**内容流里没有「行」这个事实**，要栅格化才有像素。定版式。
    ///
    /// 与 [`Format::Cbz`] 同在这一族而分歧只有一处：漫画的一页本身就是张图（按 `href`
    /// 去容器取字节），PDF 的一页是「要算出来的一张图」，所以页表里那条是
    /// `PageSource::Render` 而不是 `Image`。
    Pdf,
    /// WordprocessingML（`.docx`）：ZIP + `word/document.xml`。可重排。
    Docx,
    /// Word 97-2003（`.doc`）：OLE2 复合文件 + `WordDocument` 流。可重排。
    ///
    /// 与 [`Format::Docx`] 是同一族的两代容器：那边的正文是 XML，这边的正文是
    /// 「字符流 + piece table 把它映射到字节区间」。归约到的形状是同一串 XHTML。
    Doc,
    /// OpenDocument Text（`.odt`）：ZIP + `content.xml`。可重排。
    Odt,
    /// PalmDoc 纯文本（`.pdb`）：PalmDB 容器里的压缩文本记录。可重排。
    Pdb,
    /// AZW4（Kindle Print Replica）：PalmDB 容器里装着 PDF。定版式。
    ///
    /// 与 [`Format::Pdf`] 共用同一套页表与栅格化，差别只在「PDF 字节从哪儿来」——
    /// 这里要先从 PalmDB 的记录里取出来。
    Azw4,
    /// CB7：一个 7z，里面按页排着图片。定版式，与 [`Format::Cbz`] 同族。
    Cb7,
    /// CBR：一个 RAR，里面按页排着图片。定版式，与 [`Format::Cbz`] 同族。
    Cbr,
    /// CBT：一个 tar，里面按页排着图片。定版式，与 [`Format::Cbz`] 同族。
    Cbt,
    /// DjVu：扫描书/漫画，页是既定的画布，要渲染才有像素。定版式。
    Djvu,
    /// Calibre HTMLZ：一个 ZIP，里面是 `index.html`、章节 HTML 与图片。可重排。
    Htmlz,
    /// MHTML（`.mht` / `.mhtml`）：一个 MIME 存档里的一段 HTML 与它带的图片、样式。可重排。
    ///
    /// 与 [`Format::Htmlz`] 同一族（几份 HTML + 一堆字节），容器换成了「文本存档里几段
    /// 带头的部分」，所以键要从 `Content-Location` 折出来而不是取自条目名。
    Mhtml,
}

/// 21 个格式的键名与 media type 只在这里列一遍；`as_str` / `mime` / `parse`
/// 三个方法都由这张表生成。用宏而不是运行时查表，是为了让 `as_str` 与 `mime`
/// 保持**穷尽 match** —— 加一个 `Format` 变体却忘了加表行，编译期就红，
/// 不会拖到「分享出去打不开」才发现。
macro_rules! format_methods {
    ($( $variant:ident => $key:literal, $mime:literal );* $(;)?) => {
        impl Format {
            /// 全部变体，由表生成；契约测试拿它遍历，确认每个键名与 mime 都在。
            #[cfg(test)]
            pub(crate) const ALL: &'static [Self] = &[ $( Self::$variant, )* ];

            pub fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $key, )* }
            }

            /// 对外交出这本书时报的 media type —— 系统的分享面板按它筛选能收件的应用。
            ///
            /// 与落地副本的扩展名（它问 provider 注册表，在 folio-format 的
            /// `FormatExt::copy_extension`）不同，这张对照只有这一份：注册表管的是
            /// 「哪个 provider 来读这个扩展名」，不是「对面把这个文件当成什么」，
            /// 两者各写一张表反而会漂。已注册的类型优先（EPUB、RTF、HTML、CHM、PDF），
            /// 没注册的用事实标准那个（FB2、MOBI、CBZ）。
            ///
            /// 返回值不会是空串：Android 的 `GpuiSystemUi.shareImage` 把空 mime 退成 `image/*`，
            /// 一本 `.epub` 顶着 `image/*` 出去，读者看到的是相册那一排应用，一个也打不开书。
            pub fn mime(self) -> &'static str {
                match self { $( Self::$variant => $mime, )* }
            }

            pub fn parse(s: &str) -> Option<Format> {
                match s { $( $key => Some(Self::$variant), )* _ => None }
            }
        }
    };
}

format_methods! {
    Markdown => "markdown", "text/markdown";
    Text => "text", "text/plain";
    Epub => "epub", "application/epub+zip";
    Fb2 => "fb2", "application/x-fictionbook+xml";
    Html => "html", "text/html";
    Cbz => "cbz", "application/vnd.comicbook+zip";
    Rtf => "rtf", "application/rtf";
    Chm => "chm", "application/vnd.ms-htmlhelp";
    Mobi => "mobi", "application/x-mobipocket-ebook";
    Pdf => "pdf", "application/pdf";
    Docx => "docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
    // `.doc` 与 `.dot`（模板）在分享面板里是同一个类型：Windows 登记的就是这一条，
    // 报一条认不出的 MIME 会让收件列表整个空掉。
    Doc => "doc", "application/msword";
    Odt => "odt", "application/vnd.oasis.opendocument.text";
    Pdb => "pdb", "application/vnd.palm";
    // AZW4 在系统分享面板里仍是 Kindle 那一族；它不是 PDF 文件本身。
    Azw4 => "azw4", "application/x-mobipocket-ebook";
    Cb7 => "cb7", "application/x-7z-compressed";
    Cbr => "cbr", "application/vnd.comicbook-rar";
    Cbt => "cbt", "application/vnd.comicbook+tar";
    // IANA 注册的是 image/vnd.djvu；`image/x-djvu` 是历史别名，不另报。
    Djvu => "djvu", "image/vnd.djvu";
    Htmlz => "htmlz", "application/zip";
    // `multipart/related` 才是这一族自己声明的形状，报 `message/rfc822` 会被邮件客户端抢走。
    Mhtml => "mhtml", "multipart/related";
}

/// 一本书的书目信息。**字段按 Dublin Core 的术语集命名与取舍**（DCMI Terms / ISO 15839），
/// 因为各家电子书格式的书目区就是它：EPUB 的 OPF 直接写 `dc:*`，MOBI 的 EXTH 标签在
/// libmobi 的表里逐条注着对应哪一条 `dc:*`，FB2 的 `<book-info>` 也是同一套语义。
///
/// 空串与空表都表示「这本书没说」，不表示「说不清」—— 界面上是**有值才显示那一行**，
/// 所以空值不该长成一行空白。哪些 DC 项没收，写在下面各自的注释上。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocMeta {
    /// `dc:title`。缺了就用文件名兜底，所以这一条不会是空串。
    pub title: String,
    /// `dc:creator`。
    pub authors: Vec<String>,
    /// `dc:contributor`：译者、编者、绘者。没有按 `opf:role` 筛 —— 书里写 role 的比例
    /// 很低，筛了等于把「有个人但没说他是谁」那一批整个丢掉。
    pub contributors: Vec<String>,
    /// `dc:subject`：关键词 / 主题词。
    pub subjects: Vec<String>,
    /// `dc:description`：内容简介。EPUB 允许这一格带标签，provider 剥成纯文本再递出来
    /// （界面这一行是标签行，不是正文区）。
    pub description: String,
    /// `dc:publisher`。
    pub publisher: String,
    /// `dc:date`：出版日期。**原样存字符串** —— 书里写的粒度从「2023」到
    /// 「2023-05-01T00:00:00Z」都有，折成日期类型会把作者给的那一档精度抹掉。
    pub published: String,
    /// `dc:language`：BCP 47 原样（`zh-CN` / `en` / `ja`）。
    pub language: String,
    /// `dc:identifier` 里那条 ISBN。认三种写法：`urn:isbn:…`、
    /// `opf:scheme="ISBN"` 那一族、以及 `<meta property="dcterms:identifier" scheme="ISBN">`。
    /// 其余 identifier（uuid / DOI / Goodreads id）不进这一格：读者要认的是封底那条码。
    pub isbn: String,
    /// `dc:type`（`text` / `dcbook:poetry` 那一类）。
    pub dctype: String,
    /// `dc:rights`。
    pub rights: String,
    /// 系列。DC 十五项里没有「系列」这一项，规范载体是 `dc:relation` 配
    /// `<meta property="belongs-to-collection">`（序号在 `refines` 上来的 `group-position`），
    /// Calibre 导出则另写一对 `calibre:series` / `calibre:series_index`。两条都认，
    /// 先碰到的那条赢。
    pub series: Option<Series>,
    /// 我们自己的格式枚举。**不是** `dc:format`：那一格书里写的是 MIME（`text/html` 之类），
    /// 而界面这一行要的是「EPUB / MOBI」那一档。两个语义不共用一个字段。
    ///
    /// `dc:source` 与 `dc:coverage` 没收：电子书里几乎不出现，出现了也没有对应的界面去处。
    pub format: Format,
    /// 文件字节数（`dc:format` 的 `extent` 语义落在这一格）。
    pub size: u64,
}

/// 一本书属于哪个系列的第几本。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Series {
    pub name: String,
    /// `group-position` / `calibre:series_index`。**存字符串**：Calibre 允许 1.5 这种
    /// 插在中间的位置，折成数字就得在显示时决定「1.5」还是「1.50」，原样递出去最诚实。
    pub position: String,
}

/// 可重排文档的一章。
///
/// **正文的契约：永远是引擎就绪的 XHTML 片段。** 这是格式差异的终点 —— provider
/// 负责把自家格式归约成它（EPUB / FB2 / HTML 本来就产出 XHTML，Markdown / TXT 经
/// folio-format 的 `xhtml::from_plain_text` 包一层），引擎只管吃。所以界面上不存在
/// 「这个格式要不要先包一下」这种分支，`Format` 也不参与排版链。
///
/// 阅读位置不在这套坐标里：阅读位置（folio-core 的 `Locator`）的 `offset` 是**章内
/// 可见文本偏移**（folio-format 的 `TextMap` 那把尺），进度、锚点、选中都用它；
/// 正文怎么包不影响它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    pub title: String,
    /// 容器内的条目名（EPUB 的 spine 项）。单文件格式是空串。
    ///
    /// 两个用处：相对资源（`<img src>`）的基准目录，以及 EPUB 那一路的
    /// [`ChapterLocator::Entry`]。**不落库** —— 位置列里已经有了，见
    /// `plans/text-out-of-db.md`。
    pub spine_href: String,
    /// 正文在哪儿。**这一格取代了「一个 `String` 加一个平行数组」**：
    /// 之前 `source` 空串 + 章位置两个东西拼出来「正文在不在手上」，还得靠约定保证等长；
    /// 现在它是类型里的一个字段。
    pub text: ChapterText,
    /// **规范化可见字数**，`index()` 那一趟算出来、落进 `chapters.char_len`。
    ///
    /// `None` = 这一趟没算过（旧库读回来的行、或 provider 只读了清单）。
    /// 与 `Some(0)` 分开：前者是「不知道」，后者是「这一章真的是空的」，
    /// 合起来会让进度条把没算过的章当成空章（见 folio-format 的
    /// `ChapterExt::char_len`）。
    pub char_len: Option<usize>,
    /// 这一章正文里图片引用的**文档序清单**（`<img src>` 那一族，见 [`ImageUse`]）。
    ///
    /// 与 `char_len` 同一趟扫出来、同一条口径，随章节表落库。它是「整本书一共有几张图、
    /// 这一张在第几章第几个字」的唯一来源 —— 排版只认识已经排好的那一章，而查看器要的是
    /// 还没走到的那几章。空 = 这一章没有图，**或**这一版还没重扫过这本书
    /// （老库读回来是空表，界面按「这本书没有图」处理，见 `chapters.images_json`）。
    pub images: Vec<ImageUse>,
}

/// 一章的正文在哪儿。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChapterText {
    /// 正文就在手上（provider 刚读完、测试替身、`from_content`）。
    Inline(String),
    /// 正文在容器里，按位置现取。
    At(ChapterLocator),
}

impl ChapterText {
    /// 正文在手上时给那一份；在容器里时 `None`。
    pub fn inline(&self) -> Option<&str> {
        match self {
            Self::Inline(text) => Some(text),
            Self::At(_) => None,
        }
    }
}

impl Chapter {
    /// 容器里的一章，正文按条目名现取（EPUB 的 spine 项）。
    pub fn entry(title: impl Into<String>, href: impl Into<String>) -> Self {
        let href = href.into();
        Self {
            title: title.into(),
            spine_href: href.clone(),
            text: ChapterText::At(ChapterLocator::Entry(href)),
            // 正文不在手上，字数与图片清单要等 `index()` 那一趟读过才算得出。
            char_len: None,
            images: Vec::new(),
        }
    }

    /// 单文件容器里的第 N 章，正文重读一遍现分。
    pub fn section(title: impl Into<String>, index: usize) -> Self {
        Self {
            title: title.into(),
            spine_href: String::new(),
            text: ChapterText::At(ChapterLocator::Section(index)),
            char_len: None,
            images: Vec::new(),
        }
    }

    /// 补上条目名（相对资源的基准目录）。
    pub fn with_spine_href(mut self, href: impl Into<String>) -> Self {
        self.spine_href = href.into();
        self
    }

    /// 补上规范化可见字数（`index()` 那一趟算出来的那一格）。
    pub fn with_char_len(mut self, chars: Option<usize>) -> Self {
        self.char_len = chars;
        self
    }

    /// 把另一章已经扫出来的图片清单搬过来（`Chapter::section` 那一类「重写成只留位置」的路）。
    pub fn with_images(mut self, images: Vec<ImageUse>) -> Self {
        self.images = images;
        self
    }

    /// 正文；不在手上时是空串。要区分「空章」与「正文在别处」，看 [`Chapter::text`]。
    pub fn source(&self) -> &str {
        match &self.text {
            ChapterText::Inline(text) => text,
            ChapterText::At(_) => "",
        }
    }

    /// 正文的位置；在手上时是 `None`（还没算过它该在哪儿）。
    pub fn locator(&self) -> Option<&ChapterLocator> {
        match &self.text {
            ChapterText::At(at) => Some(at),
            ChapterText::Inline(_) => None,
        }
    }
}

/// 一章正文在容器里的位置 —— 库里只存这个，不存正文。
///
/// 判据与图片同一条（见 `plans/images-lazy.md`）：**库里只放「算出来贵、且原文件
/// 给不了」的东西**。正文原文件给得了 —— 重开一次容器就回来了 —— 而一本二十万字的
/// 中文书入库是 0.4 MB（还要再加一倍以上的全文索引），库于是与内容同量级。
///
/// 三种位置对应两类容器：
///
/// - [`ChapterLocator::Entry`]：容器里能按名字取一个条目。EPUB 的 spine 项、CBZ 的一页。
///   取一章 = 解一个 zip 条目，最便宜的一种。
/// - [`ChapterLocator::Section`]：单文件容器（TXT / MD / HTML / FB2）里的第 N 段。
///   重新分一次章就能拿回它 —— 需要 provider 的分章规则稳定，见 [`ChapterLocator::Section`]
///   自己的说明。
/// - [`ChapterLocator::Inline`]：正文就在手上（测试、`from_content`、没有可寻址容器
///   的格式）。这一档是**降级**，不是常态：能算出前两种时不该落回它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChapterLocator {
    /// 容器内条目名（与 [`TocEntry::href`] 同形）。
    Entry(String),
    /// 单文件容器里的第几章（0 起）。
    ///
    /// 存序号而不是字节区间：分章规则（Markdown 的 `#`、FB2 的 `<section>`）一旦
    /// 调整，字节区间就指到别处，而序号只会让「第几章」这个说法本身失效 —— 两者都
    /// 要重扫，但后者坏得看得见。
    Section(usize),
    /// 正文随文档一起来（那就是说没有可寻址的容器）。
    Inline(String),
}

impl Default for ChapterLocator {
    fn default() -> Self {
        Self::Inline(String::new())
    }
}

impl ChapterLocator {
    /// 这一档的正文是否就在位置上。
    pub fn is_inline(&self) -> bool {
        matches!(self, Self::Inline(_))
    }
}

/// 一章该按哪种位置存。
///
/// 名单式容器（EPUB 的 spine 项、CHM 的 topic 条目）按 [`Chapter::spine_href`] 认条目；
/// 其余是单文件，按序号认。CBZ 是定版式，没有章，走不到这里。
///
/// 已经算过位置的（从库里读回来又存回去）原样保留 —— 重算一遍只会多一次分支，
/// 而两种来源算出来的结果本来就该一致。
pub fn locator_for(format: Format, chapter: &Chapter, index: usize) -> ChapterLocator {
    if let Some(at) = chapter.locator() {
        return at.clone();
    }
    match format {
        Format::Epub | Format::Chm | Format::Htmlz | Format::Mhtml
            if !chapter.spine_href.is_empty() =>
        {
            ChapterLocator::Entry(chapter.spine_href.clone())
        }
        Format::Text
        | Format::Markdown
        | Format::Html
        | Format::Fb2
        | Format::Rtf
        | Format::Mobi
        | Format::Docx
        | Format::Doc
        | Format::Odt
        | Format::Pdb => ChapterLocator::Section(index),
        // 没有可寻址的位置（EPUB 少了 href、定版式那几族、测试替身）：只能说正文在手上。
        _ => ChapterLocator::Inline(chapter.source().to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TocEntry {
    pub title: String,
    /// 解析后的绝对容器路径（已百分号解码、去 fragment），与 `Chapter::spine_href` 同形。
    pub href: String,
    /// 同一个文件里的落点，也就是 `href` 里 `#` 后面那一截（已百分号解码）。`None` = 章首。
    ///
    /// 单独立一个字段而不是并回 `href`：`href` 与 `Chapter::spine_href` 同形是「这条目录
    /// 对得上哪一章」的比对键，带上 `#` 就比不中了，而 nav 里多条指向同一个章文件的不同
    /// 小节恰恰是靠这一截互相区分的。
    ///
    /// `#[serde(default)]`：库里已有的 `toc_json` 没有这一项，旧书解出来是 `None`，
    /// 表现就是「跳到章首」，与它被加进来之前的行为一致。
    #[serde(default)]
    pub fragment: Option<String>,
    /// **定版式**的落点：第几页。可重排的那一路永远是 `None`。
    ///
    /// 有 `href` 为什么还要这一格：`href` 是「容器里哪一条」的比对键（与
    /// `Chapter::spine_href` 同形），而 PDF / 漫画的目录条对不上任何一条容器路径 ——
    /// 它要说的是「第 7 页」。把页号编码进 `href` 那根字符串会让比对键变成两义，
    /// 所以另开一格，两格互斥。
    ///
    /// `#[serde(default)]`：库里已有的 `toc_json` 没有这一项，老书解出来是 `None`
    /// = 「按 href 跳」，与它被加进来之前逐字节一致。
    #[serde(default)]
    pub page: Option<usize>,
    pub children: Vec<TocEntry>,
}

/// 一本书里的一张图片的**元数据**。`href` 是解析后的**容器绝对路径**（与
/// `Chapter::spine_href` 同形），排版与绘制都用它当键；`width`/`height` 是解码前的
/// 固有像素尺寸，读取时算好，布局期只查表。SVG 等取不到尺寸的格式为 0，由排版层降级。
///
/// 与 [`Resource`]（API 4 资源清单）的分工，两者**并存而不是重复**：这里记的是
/// **排版元数据**（占位尺寸 + 降级路），布局热路径按它查表摆图；`Resource` 是
/// **按类消费**的容器目录（清单里还有 CSS / 字体 / 音视频），不参与摆位。所以它
/// 删不得 —— `Resource` 没有 width/height，清单替代不了它；它也不并入 `Resource`
/// （字段语义不同：像素尺寸 vs 字节长度），真要折成清单项走 [`ImageRes::as_resource`]。
///
/// **字节不在这里，也不进数据库**（学 KOReader 的 `NodeImageProxy`）：真要画时按
/// 这一条去容器里现取（folio-core 的 `Document::image_bytes`）。原图随时能从书里
/// 再拿一次，就不该在库里留第二份 —— 一本 200 MB 的漫画，图片一入库就等于把书抄了一遍。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRes {
    /// **查询键**：正文 `<img src>` 按本章目录解出来的那一条。排版与绘制都拿它问表。
    pub href: String,
    /// 字节在容器里**真实**的那一条目名。
    ///
    /// 与 `href` 只在一种情况下不同：作者把这张图声明成我们解不动的格式（EPUB3 里最常
    /// 见的是逐页 SVG 与带 `image/svg+xml` 的封面），并在 manifest 上挂了 `fallback=`
    /// 指着同一张图的位图版本（folio-format 的 `format::epub::decodable_fallback`）。
    /// 那时**键不能换** —— 它是正文里那串 `src` 解出来的，而正文一个字都不许改
    /// （`plans/Locator.md`：锚点坐标就建在源文本上）—— 要换的只是「去哪儿读字节」，
    /// 那一格就是这里。
    ///
    /// 其余格式与没有降级路的条目都填 `href.clone()`。库里空串读回时同样算「与 `href`
    /// 同一条」（`store::content` 那一趟），所以这一格不是一份新的事实，是旧事实的显式化。
    pub source: String,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
}

impl ImageRes {
    /// 折成清单里的一项（`doc/format.md` API 4 的 `ResourceKind::Image`）。
    /// `size` 这一层给不出 —— 它记的是**像素尺寸**，清单那一格要的是**字节长度**；
    /// 想要字节数去问 provider 的 `resources()`（EPUB 从 zip 中央目录现量）。
    pub fn as_resource(&self) -> Resource {
        Resource::new(self.href.clone(), ResourceKind::Image)
    }
}

/// 一本书里的一项**资源**（`doc/format.md` API 4）：容器里一条可寻址的字节，按
/// 种类登记 —— 不再只有图片那一张表。
///
/// 类型一步写全（8 档），但**消费先只认 Css / Image / Text 三类**：Font 已在用
/// （内嵌字体子集化），Video / Audio 只登记，JavaScript 只登记、永不执行。
/// `href` 与 [`Chapter::spine_href`] / [`ImageRes::href`] 同形（容器绝对路径），
/// 是 `resource_bytes` 的查询键；`size` 是容器里那条字节的长度，取不到时是 `None`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    pub href: String,
    pub kind: ResourceKind,
    pub size: Option<u64>,
}

impl Resource {
    pub fn new(href: impl Into<String>, kind: ResourceKind) -> Self {
        Self {
            href: href.into(),
            kind,
            size: None,
        }
    }

    /// 带容器字节数的那一种（EPUB 的 manifest 能从 zip 条目直接量出来）。
    pub fn with_size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }
}

/// 资源的种类（[`Resource`]）。serde 键名是库里 / 清单里看得见的那一格。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceKind {
    Css,
    Image,
    /// 书内夹带的纯文本（txt 附件之类），当正文一样取字节。
    Text,
    Font,
    Video,
    Audio,
    /// 只登记、永不执行。
    JavaScript,
    #[default]
    Other,
}

impl ResourceKind {
    /// 从 manifest / 容器给的 media type 折成种类。`;` 之后的参数不参与判定；
    /// 认不出的一律 [`ResourceKind::Other`]，**不猜** —— 登记错一类，将来按类
    /// 消费时就静默拿错字节。XHTML / HTML 条目是「章」，不算资源消费类，落 Other。
    pub fn from_media_type(media_type: &str) -> Self {
        let base = media_type.split(';').next().unwrap_or("").trim();
        match base {
            "text/css" => Self::Css,
            "text/plain" => Self::Text,
            "text/javascript" | "application/javascript" => Self::JavaScript,
            _ => match base.split('/').next().unwrap_or("") {
                "image" => Self::Image,
                "audio" => Self::Audio,
                "video" => Self::Video,
                "font" => Self::Font,
                _ if base.starts_with("application/x-font")
                    || base.starts_with("application/font") =>
                {
                    Self::Font
                }
                _ => Self::Other,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Format, Resource, ResourceKind};

    /// media type → 种类的映射：manifest 给什么就折成什么，认不出的落 Other 不猜。
    #[test]
    fn media_types_map_to_resource_kinds() {
        use ResourceKind::*;
        assert_eq!(ResourceKind::from_media_type("text/css"), Css);
        assert_eq!(ResourceKind::from_media_type("image/png"), Image);
        assert_eq!(ResourceKind::from_media_type("image/svg+xml"), Image);
        assert_eq!(
            ResourceKind::from_media_type("image/jpeg; charset=binary"),
            Image
        );
        assert_eq!(ResourceKind::from_media_type("font/woff2"), Font);
        assert_eq!(
            ResourceKind::from_media_type("application/x-font-opentype"),
            Font
        );
        assert_eq!(ResourceKind::from_media_type("audio/mpeg"), Audio);
        assert_eq!(ResourceKind::from_media_type("video/mp4"), Video);
        assert_eq!(ResourceKind::from_media_type("text/javascript"), JavaScript);
        assert_eq!(ResourceKind::from_media_type("text/plain"), Text);
        // XHTML / HTML 条目是「章」，不是资源消费类。
        assert_eq!(
            ResourceKind::from_media_type("application/xhtml+xml"),
            Other
        );
        assert_eq!(
            ResourceKind::from_media_type("application/octet-stream"),
            Other
        );
    }

    /// serde 往返：kind 的键名小写，`size` 可缺席。
    #[test]
    fn resources_round_trip_through_json() {
        let resource = Resource::new("OEBPS/styles/main.css", ResourceKind::Css).with_size(2048);
        let json = serde_json::to_string(&resource).unwrap();
        assert!(json.contains("\"kind\":\"css\""), "{json}");
        let back: Resource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, resource);
        let no_size: Resource = serde_json::from_str(r#"{"href":"a.png","kind":"image"}"#).unwrap();
        assert_eq!(no_size.kind, ResourceKind::Image);
        assert_eq!(no_size.size, None);
    }

    /// 键名与 media type 是持久化 / 分享出去的契约：三张 match 合成一张表之后，
    /// 这条门负责钉住「每个变体的键名往返得回来、mime 非空、变体数对得上」。
    #[test]
    fn every_format_key_round_trips_and_has_a_mime() {
        assert_eq!(Format::ALL.len(), 21, "加格式时表与 enum 要一起加");
        for format in Format::ALL {
            assert_eq!(
                Format::parse(format.as_str()),
                Some(*format),
                "{format:?} 的键名读不回来"
            );
            assert!(!format.mime().is_empty(), "{format:?} 没有 media type");
        }
    }

    /// 表里那几个「不是变体名简单拼出来」的键逐字钉死，防止改表时顺手改错。
    #[test]
    fn special_format_keys_and_mimes_stay_put() {
        assert_eq!(Format::Epub.mime(), "application/epub+zip");
        assert_eq!(Format::Doc.mime(), "application/msword");
        assert_eq!(Format::Azw4.mime(), "application/x-mobipocket-ebook");
        assert_eq!(Format::Djvu.mime(), "image/vnd.djvu");
        assert_eq!(Format::Mhtml.mime(), "multipart/related");
        assert_eq!(
            Format::Docx.mime(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
        assert_eq!(Format::parse("mhtml"), Some(Format::Mhtml));
        assert_eq!(Format::parse("不在表里"), None);
        assert_eq!(Format::default(), Format::Text);
    }
}
