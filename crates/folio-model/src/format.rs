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

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Markdown => "markdown",
            Format::Text => "text",
            Format::Epub => "epub",
            Format::Fb2 => "fb2",
            Format::Html => "html",
            Format::Cbz => "cbz",
            Format::Rtf => "rtf",
            Format::Chm => "chm",
            Format::Mobi => "mobi",
            Format::Pdf => "pdf",
            Format::Docx => "docx",
            Format::Doc => "doc",
            Format::Odt => "odt",
            Format::Pdb => "pdb",
            Format::Azw4 => "azw4",
            Format::Cb7 => "cb7",
            Format::Cbr => "cbr",
            Format::Cbt => "cbt",
            Format::Djvu => "djvu",
            Format::Htmlz => "htmlz",
            Format::Mhtml => "mhtml",
        }
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
        match self {
            Format::Markdown => "text/markdown",
            Format::Text => "text/plain",
            Format::Epub => "application/epub+zip",
            Format::Fb2 => "application/x-fictionbook+xml",
            Format::Html => "text/html",
            Format::Cbz => "application/vnd.comicbook+zip",
            Format::Rtf => "application/rtf",
            Format::Chm => "application/vnd.ms-htmlhelp",
            Format::Mobi => "application/x-mobipocket-ebook",
            Format::Pdf => "application/pdf",
            Format::Docx => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            }
            // `.doc` 与 `.dot`（模板）在分享面板里是同一个类型：Windows 登记的就是这一条，
            // 而对面按它挑收件应用 —— 报一条认不出的 MIME，症状是收件列表整个空掉。
            Format::Doc => "application/msword",
            Format::Odt => "application/vnd.oasis.opendocument.text",
            Format::Pdb => "application/vnd.palm",
            // AZW4 在系统分享面板里仍是 Kindle 那一族；它不是 PDF 文件本身。
            Format::Azw4 => "application/x-mobipocket-ebook",
            Format::Cb7 => "application/x-7z-compressed",
            Format::Cbr => "application/vnd.comicbook-rar",
            Format::Cbt => "application/vnd.comicbook+tar",
            // IANA 注册的是 image/vnd.djvu；`image/x-djvu` 是历史别名，不另报。
            Format::Djvu => "image/vnd.djvu",
            Format::Htmlz => "application/zip",
            // 对面按它筛收件应用：`message/rfc822` 那一族会被邮件客户端抢走，
            // 而 `multipart/related` 才是这一族自己声明的形状。
            Format::Mhtml => "multipart/related",
        }
    }

    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "markdown" => Some(Format::Markdown),
            "text" => Some(Format::Text),
            "epub" => Some(Format::Epub),
            "fb2" => Some(Format::Fb2),
            "html" => Some(Format::Html),
            "cbz" => Some(Format::Cbz),
            "rtf" => Some(Format::Rtf),
            "chm" => Some(Format::Chm),
            "mobi" => Some(Format::Mobi),
            "pdf" => Some(Format::Pdf),
            "docx" => Some(Format::Docx),
            "doc" => Some(Format::Doc),
            "odt" => Some(Format::Odt),
            "pdb" => Some(Format::Pdb),
            "azw4" => Some(Format::Azw4),
            "cb7" => Some(Format::Cb7),
            "cbr" => Some(Format::Cbr),
            "cbt" => Some(Format::Cbt),
            "djvu" => Some(Format::Djvu),
            "htmlz" => Some(Format::Htmlz),
            "mhtml" => Some(Format::Mhtml),
            _ => None,
        }
    }
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

    pub fn as_inline(&self) -> Option<&str> {
        match self {
            Self::Inline(text) => Some(text),
            _ => None,
        }
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
