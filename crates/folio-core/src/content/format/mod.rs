use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::content::scan::ImageUse;

pub mod cbz;
/// 字节 → 文本的那一层（BOM / 声明 / 容器报的码页）。见模块头那四档顺序。
pub(crate) mod charset;
/// CHM（Microsoft Compiled HTML Help）：一个 ITSF 容器里的一组 HTML topic。
pub mod chm;
pub mod container;
pub mod content;
pub mod cover;
pub mod css;
pub mod epub;
pub mod fb2;
pub mod html;
pub mod markdown;
pub(crate) mod mobi;
#[allow(dead_code)] // 装配那一步（KF7/KF8 的 provider）由下一轮接上，这一层先被它用。
pub(crate) mod palmdb;
pub mod provider;
pub mod rtf;
pub(crate) mod sniff;
/// SVG 栅格化：把矢量资源（SVG 封面、逐页漫画、正文内联 SVG）画成位图。
pub mod svg;
pub mod thumb;
pub mod woff;
pub mod xhtml;
pub(crate) mod zip;

pub use content::{Content, ContentKind, FixedDoc, FixedPage, FlowDoc, PageImage, PageSource};
pub use provider::{
    DocumentProvider, ParseBudget, looks_like_a_book, provider_for, providers,
    supported_extensions, supports_extension,
};

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
        }
    }

    /// 落地副本该用的扩展名（不带点、小写）。
    ///
    /// **问的是 provider 注册表**，不是在这里手写一张对照表：副本存成
    /// `<哈希前缀>.<这一格>`，而下一次按扩展名认领文件的正是那张表 —— 两处各写一份
    /// 就会漂，而漂的症狀最狠：导进去一本书，它自己的副本再也认不出来
    /// （实测过一次，`Format::as_str` 给的是 `text` / `markdown`，注册表认的是
    /// `txt` / `md`）。
    ///
    /// 认法：拿注册表全部被认领的扩展名逐个探，取第一个「provider 认下它，且它
    /// 报出的格式就是自己」的那个。剩下没有命中的（例如还没写 provider 的格式）
    /// 退回 `as_str`，让日志里至少有个说得清的名字。
    pub fn copy_extension(self) -> &'static str {
        crate::content::format::provider::supported_extensions()
            .iter()
            .copied()
            .find(|ext| {
                let probe = std::path::Path::new("probe").with_extension(ext);
                crate::content::format::provider::provider_for(&probe)
                    .is_some_and(|p| p.format() == self)
            })
            .unwrap_or_else(|| self.as_str())
    }

    /// 对外交出这本书时报的 media type —— 系统的分享面板按它筛选能收件的应用。
    ///
    /// 与 [`Format::copy_extension`]（它问 provider 注册表）不同，这张对照只有这一份：
    /// 注册表管的是「哪个 provider 来读这个扩展名」，不是「对面把这个文件当成什么」，
    /// 两者各写一张表反而会漂。已注册的类型优先（EPUB、RTF、HTML、CHM），没注册的用
    /// 事实标准那个（FB2、MOBI、CBZ）。
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
            _ => None,
        }
    }
}

/// 索引方案的版本。**分章规则、位置方案、或定版式的页表算法一改就要加一。**
///
/// 库里的位置（[`ChapterLocator`]）是「拿什么规则算出来的」这件事的承诺：
/// `Section(3)` 只在「按今天这套规则分章」的前提下指到第三章。Markdown 的标题层级、
/// FB2 的 section 嵌套、EPUB 的 spine 过滤，任一改动都会让老库的序号指到别处 ——
/// 那时版本号对不上，[`crate::store::Store::stale_reason`] 会说「该重扫」。
///
/// 定版式文档（CBZ / 将来的 PDF）没有章表，但页表同样是「按某套算法产出的索引」，
/// 所以**两条落库路径都要盖这一格** —— 不盖的话定版式书会被永远判成「索引旧了」，
/// 每次开书都白重扫一遍。
///
/// v2：CSS 出库 —— `books.css` 列从拼接文本改成位置清单（`CssStored` 信封），
/// 老库靠重扫改写成新格式；重扫前旧文本当内联样式兜底。
///
/// v3：EPUB 的 spine 过滤改了两处（见 `format::epub::KEEP_NONLINEAR` 与
/// `format::epub::unreadable_chapter`）：`linear="no"` 的那几节不再是章，读不动但是
/// 内容的那几节改成留一章说明。**两边的章数与章号都会动**，而库里的位置记的就是章号 ——
/// 不 bump 的话老库的进度会安静地指到隔壁那一章。代价是整库每本重扫一次，由
/// `stale_reason` 认成 `OldIndex`、开书时投递，不在升级那一刻付。
/// v4：书目按 Dublin Core 补齐（`store` 的 v19 迁移那 11 列）。老行取默认值 = 全空，
/// 而「全空」在信息屏上与「这本书没有简介」长得一样 —— 所以要重扫一次把它们填上。
/// 开书时由 `stale_reason` 认成 `OldIndex` 投递，不在升级那一刻整库付。
/// v5：EPUB2 的 NCX 目录**不再拍平**（`format::epub::parse_ncx` 按 `navPoint` 的层读成树）。
/// 目录是落库的（`books.toc_json`），所以这一格改了以后老库里那份还是平的那一份 ——
/// 症状不是「跳错地方」而是**界面上看不出层级、也没有可折的分支**：`toc_tree::build` 拿到的
/// 每条都是叶子，`branch` 全假，缩进与 chevron 都不会出现。真机上已经撞过一次（基督山伯爵
/// 那本 124 个 `navPoint`、最深 2 层，装上新代码后目录面板仍是平的，就是这个原因）。
/// 老行不重扫不会读错东西，只是永远停在旧形状，所以按同一口径 bump。
/// v6：EPUB 把**封面页**收成第一章（`<guide type="cover">` 指的那一页，即使标了
/// `linear="no"`；见 `format::epub::KEEP_NONLINEAR`）。这一本多一章、后面每章序号都往后
/// 顶一格 —— 与 v3 同一类改动，不 bump 老库的进度就会安静地指到隔壁那一章。
/// v7：**每章的图片引用清单入库**（`store` 的 v20 迁移那一列）。老行读回来是空表，
/// 而「这本书没有图」与「这一版还没扫过图」在界面上是同一件事：图片查看器那一列
/// 会一张都不剩，读者只会以为这本书是纯文字书。所以要重扫一次把它填上。
pub const INDEX_VERSION: i64 = 7;

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
/// [`xhtml::from_plain_text`] 包一层），引擎只管吃。所以界面上不存在
/// 「这个格式要不要先包一下」这种分支，`Format` 也不参与排版链。
///
/// 阅读位置不在这套坐标里：`crate::Locator` 的 `offset` 是**章内可见文本偏移**
/// （`crate::content::anchor::TextMap`），进度、锚点、选中都用它；正文怎么包不影响它。
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
    /// 之前 `source` 空串 + `FlowDoc::chapter_locators[i]` 两个东西拼出来「正文在不在
    /// 手上」，还得靠约定保证等长；现在它是类型里的一个字段。
    pub text: ChapterText,
    /// **规范化可见字数**，`index()` 那一趟算出来、落进 `chapters.char_len`。
    ///
    /// `None` = 这一趟没算过（旧库读回来的行、或 provider 只读了清单）。
    /// 与 `Some(0)` 分开：前者是「不知道」，后者是「这一章真的是空的」，
    /// 合起来会让进度条把没算过的章当成空章（见 [`Chapter::char_len`]）。
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
    /// 正文就在手上的那一章。
    pub fn inline(title: impl Into<String>, source: impl Into<String>) -> Self {
        let source = source.into();
        // 正文在手就顺手把字数定下来：所有调用方要的都是同一个数，而在这里算一次，
        // 比让每个读的地方各算一次便宜，也让 `char_len` 一建出来就是确定的。
        // 走 `content::scan` 而不是 `visible_chars`：同一个数（逐章对拍守着），
        // 但它一遍扫完不建 DOM —— 导入里 `with_text` 那一档本来就已经算过一遍。
        //
        // **空串不算**，它是「正文不在手上」在这套类型里的哨兵：`locator_for`
        // 在「没有可寻址位置」那一档写的正是 `Inline(chapter.source())`，而正文不在
        // 手上时 `source()` 就是空串；`load_flow_in` 对认不出来的位置也回落到
        // `ChapterLocator::default()`（同样是 `Inline("")`）。拿它去数会得到
        // `Some(0)`，于是「库里没记字数」被读成「这一章是空的」—— 分母偏小、
        // 百分比偏大，而且没有任何报错。真正的空章走 `Some(0)`，
        // 它的标记（`<p></p>` 之类）不会退化成空串。
        let summary = (!source.is_empty()).then(|| crate::content::scan::chapter_summary(&source));
        Self {
            title: title.into(),
            spine_href: String::new(),
            text: ChapterText::Inline(source),
            char_len: summary.as_ref().map(|s| s.visible_chars),
            images: summary.map(|s| s.images).unwrap_or_default(),
        }
    }

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

    /// 补上扫描那一趟的两样数：字数与图片清单。
    ///
    /// 合成一句而不是 `with_char_len` + `with_images` 两句：这两格**必须同源**，
    /// 而分开两句写的下场是漏一句 —— 漏了 `with_images` 的那一章在「整本书那一列图」里
    /// 一个身影都没有，正文里那张图照样画得出，只有左右滑到那儿会跳过它。
    /// 同一趟扫描出来的两样数，一次交出去。
    pub fn with_scan(mut self, summary: &crate::content::scan::ChapterSummary) -> Self {
        self.char_len = Some(summary.visible_chars);
        self.images = summary.images.clone();
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

    /// 正文在**手上**时的规范化可见字数；不在手上、或这一趟没算过，都是 `None`。
    ///
    /// 这一格是 [`visible_chars`] 的结果，随 `index()` 一起产出并落进
    /// `chapters.char_len`。它与位置坐标系**同一个口径**：`TextMap::chars()`
    /// 数的就是 `Locator.offset` 的推进轴，所以「前面各章字数之和 + 章内偏移」
    /// 是一个可以相减的全局字符进度（见 `plans/Locator.md` 第二节）。
    ///
    /// `None` 与 `Some(0)` 是两件事：前者是「还没算」（旧库、或这一趟只读了清单），
    /// 后者是「这一章真的是空的」。合成一个就会让进度条把没算过的章当成空章。
    ///
    /// 空串那一支**不算**：`ChapterLocator::default()` 是 `Inline("")`，而
    /// `load_flow_in` 对认不出来的位置正是回落到它（「位置不明」那一档）。
    /// 拿它去数会得到 0，于是「库没记字数」被读成「这一章是空的」——
    /// 分母偏小、百分比偏大，而且没有任何报错。真正的空章走 `Some(0)`
    /// （`Chapter::inline` 建的时候就把它数下来了）。
    pub fn char_len(&self) -> Option<usize> {
        self.char_len.or_else(|| {
            self.text
                .inline()
                .filter(|text| !text.is_empty())
                .and_then(visible_chars)
        })
    }
}

/// 一段引擎就绪的 XHTML 里**可见文本**的字符数 —— 与 [`crate::content::anchor::TextMap::chars`]
/// 同一个口径（空白折叠、行内图占一个字符位）。
///
/// 存在的理由：`chapters.char_len` 要的是「读者能数出来的字数」，而
/// `source().chars().count()` 数的是**标记**——同一段正文标点多几个 `<span>`，
/// 字数就变了。而进度百分比要跨设备显示同一个数、还要能前后相减，所以它必须
/// 与 `Locator.offset` 走同一把尺。
///
/// 解析不了时回 `None`（坏 XHTML、空片段）：**绝不用字节数估**。字节↔字符的比例
/// 在各章之间不均匀（中英混排、标签密度），估出来的百分比会不单调 ——
/// 读者往下读而进度条后退，是产品上不可接受的（`plans/导入管线.md` §八 第 3 条）。
pub fn visible_chars(xhtml: &str) -> Option<usize> {
    Some(text_map(xhtml).chars())
}

/// 一段引擎就绪的 XHTML 的**可见文本**本身 —— 与 [`visible_chars`] 同一个 `TextMap`，
/// 一个数、一条串。
///
/// 全文搜索在这条串上找命中，于是命中的字符偏移天生就是 [`crate::Locator::offset`]
/// 那一把尺（见 [`crate::search`]）。分成两处实现就会有一处漂移，症状是「搜到『猫』、
/// 跳过去屏上是『狗』」。
pub fn visible_text(xhtml: &str) -> String {
    text_map(xhtml).text().to_string()
}

/// 两条路共用的那一次解析：折出这一章的可见文本与它的坐标表。
fn text_map(xhtml: &str) -> crate::content::anchor::TextMap {
    let dom = crate::content::dom::Dom::parse(xhtml.as_bytes());
    // 没有 `<body>` 的片段（FB2 / Markdown 的包装产物都带，但裸片段不带）
    // 就整棵当正文根 —— `TextMap::build` 自己会跳过 `<script>` / `<style>`。
    let body = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map_or(dom.document, |(id, _)| id);
    crate::content::anchor::TextMap::build(&dom, body)
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
        Format::Epub | Format::Chm if !chapter.spine_href.is_empty() => {
            ChapterLocator::Entry(chapter.spine_href.clone())
        }
        Format::Text
        | Format::Markdown
        | Format::Html
        | Format::Fb2
        | Format::Rtf
        | Format::Mobi => ChapterLocator::Section(index),
        // 没有可寻址的位置（EPUB 少了 href、将来的格式、测试替身）：只能说正文在手上。
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
    pub children: Vec<TocEntry>,
}

/// 一本书里的一张图片的**元数据**。`href` 是解析后的**容器绝对路径**（与
/// `Chapter::spine_href` 同形），排版与绘制都用它当键；`width`/`height` 是解码前的
/// 固有像素尺寸，读取时算好，布局期只查表。SVG 等取不到尺寸的格式为 0，由排版层降级。
///
/// **字节不在这里，也不进数据库**（学 KOReader 的 `NodeImageProxy`）：真要画时按
/// [`Self::source`] 去容器里现取，见 [`image_bytes`]。原图随时能从书里再拿一次，就不该在库里
/// 留第二份 —— 一本 200 MB 的漫画，图片一入库就等于把书抄了一遍。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRes {
    /// **查询键**：正文 `<img src>` 按本章目录解出来的那一条。排版与绘制都拿它问表。
    pub href: String,
    /// 字节在容器里**真实**的那一条目名。
    ///
    /// 与 `href` 只在一种情况下不同：作者把这张图声明成我们解不动的格式（EPUB3 里最常
    /// 见的是逐页 SVG 与带 `image/svg+xml` 的封面），并在 manifest 上挂了 `fallback=`
    /// 指着同一张图的位图版本（`format::epub::decodable_fallback`）。那时**键不能换** ——
    /// 它是正文里那串 `src` 解出来的，而正文一个字都不许改（`plans/Locator.md`：锚点坐标
    /// 就建在源文本上）—— 要换的只是「去哪儿读字节」，那一格就是这里。
    ///
    /// 其余格式与没有降级路的条目都填 `href.clone()`。库里空串读回时同样算「与 `href`
    /// 同一条」（`store::content` 那一趟），所以这一格不是一份新的事实，是旧事实的显式化。
    pub source: String,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
}

/// 这本书里取到的一份字体字节，按容器绝对路径索引。
///
/// **不进 [`FlowDoc`]、也不进 SQLite**：字体是**锦上添花**的那一层 —— 容器不在时的
/// 表现是「回到今天的样子」（读者字体照常排，只是没有补缺字的那一份），
/// 而不是少一页正文。于是 `fonts_at` 每次开书去容器现读一次，几个小文件、一次解压，
/// 不常驻。（正文也不在库里，但它有 [`ChapterLocator`] 那条位置线，
/// 见 `plans/text-out-of-db.md`；字体没有，只能每次现读。）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontBytes {
    /// 容器内的规范路径，与 `@font-face` 的 `src` 改写后同形（见 `css::FontSrc::Url`）。
    pub href: String,
    /// manifest 声明的 media-type，供调用方打日志（WOFF 已在这里解回 sfnt，
    /// 所以 `bytes` 一律是 TTF/OTF 那一层的字节）。
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// 从字节头猜一张图的 mime。
///
/// 存在的理由是 `books.cover` 那一列**只有字节、没有媒体类型**：封面是从容器里按
/// OPF 的 cover id 捞出来的，那条记录不带 manifest 的 `media-type`。而 gpui 的
/// `Image::from_bytes` 必须显式给格式（它按格式挑解码器，给错了解不出来，
/// 一片空白且不报错），所以只能在画之前按 magic bytes 反推。
///
/// 只认根 `Cargo.toml` 给 `image` 开的那几种 feature；猜不出或没编译进来的格式返回
/// `None`，由调用方决定降级成什么占位，而不是假装它是 PNG 去试一次。
pub fn guess_media_type(bytes: &[u8]) -> Option<&'static str> {
    let guessed = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .format()?;
    decodable_bitmap_mime(guessed)
}

// `COVER_SPEC_VERSION` 在 v16 作废：它记的是「这份封面字节是按哪一版规格生成的」，
// 用来让 `stale_reason` 认出该重生成的封面。封面改成 `covers/` 里的文件之后，
// 换规格只要重新生成缩略图（`format::thumb::purge` + 重新生成），
// 不需要「按规格重扫全书」这个概念了。

/// 一张位图缩到长边不超过 `max_edge`，再编码回一个紧凑格式。
///
/// 返回 `Some((bytes, mime))` **只在真的缩过**时：已经够小、解不出来、或本来就
/// 不该按位图缩的（SVG）都返回 `None`，调用方保留原字节。
///
/// 为什么这件事存在：gpui 的 `img` 把位图放进 sprite atlas，而单张 atlas 纹理的
/// 上限是设备的 `max_texture_dimension_2d`（GLES 上常见 4096，部分只有 2048），
/// 并且 tile 用的是**图像原始像素**。一张 1200×1600 的插图本身不大，但在
/// devicePixelRatio 2.8 的手机上仍按 1200×1600 进 atlas —— 一旦超过上限，
/// `paint_image` 只写一条日志、图就静默消失。KOReader 的 `ImageWidget` 也是
/// 先把位图按显示尺寸缩一遍再缓存（`frontend/ui/widget/imagewidget.lua` 的
/// `scale_factor` 与 `ImageCache`），这里取同一个思路。
///
/// 编码格式跟着透明通道走：有 alpha 的走 PNG（无损），没有的走 JPEG（照片缩完
/// 仍然小一个量级）。两者都在 workspace 已开的 `image` feature 里。
pub fn shrink_image(bytes: &[u8], max_edge: u32) -> Option<(Vec<u8>, &'static str)> {
    use image::ImageDecoder as _;

    // 这是一笔**贵**事（整份解码 + 重编码），所以数它一次：模块头那条纪律允许
    // 「本身就贵的事件」进计数器，而这一格的口径是「阅读时不该发生」。
    crate::infra::metrics::Metric::ImageShrinks.count();

    if max_edge == 0 {
        return None;
    }
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    // 动画 GIF 不缩：`ImageReader` 只解第一帧，缩完就把动画丢了。书里的 GIF 一般是
    // 小装饰图，本来也过不了 `max_edge` 那道门，这里只是把「大 GIF」这条路显式让开。
    if reader.format() == Some(image::ImageFormat::Gif) {
        return None;
    }
    // 先拿 decoder 把 EXIF 方向读出来再解码：`from_decoder` 不应用方向，
    // 不自己补一下的话，手机上拍的竖图缩完会横着。gpui 自己那条解码路
    // （`decode_static_image_from_decoder`）也是这么做的，两边口径一致。
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().ok()?;
    let mut image = image::DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);

    let (width, height) = (image.width(), image.height());
    let longest = width.max(height);
    // **不管原图多大都重编一遍**：封面的问题不是「太大」而是「按原格式存」。
    // 一张 800×1200 的 PNG 封面就有 400 KB，而书架上显示几十像素宽 —— 它过不了
    // 「超过 max_edge 才缩」那道门，于是原样进了库。缩到显示尺寸 + 一律 JPEG，
    // 才是这一列真正该占的地方。
    // `thumbnail` 而不是自己算目标尺寸再 `resize_exact`：它按长宽比缩进 `max_edge` 的方框，
    // 取整口径与手算那一条一致（1535×2200 → 357×512，538×720 → 383×512），少了一段会算错
    // 的算术 —— 手算算错就是把封面拉变形。
    //
    // 换掉的不只是那段算术，还有滤波：`resize_exact(Triangle)` 是**卷积滤波**，而
    // `thumbnail` 走 `imageops::thumbnail` 那个**整数块平均**采样器（每个源像素恰好贡献给
    // 一个目标像素，边缘用线性补），官方文档自己标了「新尺寸接近旧尺寸时可能有锯齿」。
    // 快是真快：实测 1535×2200 那一张 127.8 ms → 44.3 ms，538×720 那一张 26.5 ms → 7.7 ms
    // （两条臂交替跑 7 轮取中位）。代价是缩放质量换了档：小比例缩放（1.4×）那一头最容易看出。
    // 与 `thumb::encode_from` 同一道滤波也算一条理由：库里那份和磁盘那两档不该长得不一样。
    let image = if longest > max_edge {
        image.thumbnail(max_edge, max_edge)
    } else {
        image
    };

    // 带 alpha 的封面先铺一层白底再编 JPEG：JPEG 没有 alpha，直接丢掉会让透明区
    // 变成黑的。书封极少靠透明表达什么，白底是印刷品的默认。
    let rgb = image.to_rgb8();
    let mut out = Vec::new();
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::Cursor::new(&mut out), 82);
    encoder.encode_image(&rgb).ok()?;
    // 重编之后反而更大的（小图、或者已经高度优化的 JPEG）就退回原图。
    if out.len() >= bytes.len() {
        return None;
    }
    Some((out, "image/jpeg"))
}

// 内容家族（可重排 / 定版式）与 provider 注册表各自成文件，见 `content` / `provider`。
// 这里把它们再导出，旧路径 `format::{FlowDoc, Content, FormatAdapter, adapters}` 全线保留：
// 调用方不需要知道内部拆成了几块。

/// 扩展名的小写形式（不带点）。认不出返回 `None`。
pub(crate) fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// 文件名去掉扩展名，作为无名之书的兜底标题。
pub(crate) fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "未命名".to_string())
}

/// 位图的固有像素尺寸。只读 header，不解像素；取不到尺寸（SVG / 未知格式）时返回
/// `(0, 0)`，由排版的降级规则兜底。
pub(crate) fn image_dims(bytes: &[u8]) -> (u32, u32) {
    match image_dims_from_head(bytes) {
        HeadDims::Sized(dims) => dims,
        // 整条字节都在手上了，两种「拿不到」都是拿不到。
        _ => (0, 0),
    }
}

/// 按**一段字节**（可以只是解压流的前缀）读尺寸的结果。
///
/// 分三档而不是 `Option`，因为「没读出尺寸」有两种相反的下一步：认不出这是位图
/// （SVG、非图像、没编解码器的格式）再读整条也拿不到，别白花一趟解压；而认得出是
/// 位图、只是尺寸头排在更后面（带大 EXIF 缩略图的 JPEG）才值得多读一趟。
/// 唯一按这三档决定下一步的是 [`epub::parse_image`]（见那里）。
#[derive(Debug)]
pub(crate) enum HeadDims {
    /// 尺寸定出来了。
    Sized((u32, u32)),
    /// 这不是（我们解得动的）位图：不要再为它读更多字节。
    NotABitmap,
    /// 是位图，但这段字节不够长，尺寸头在更后面。
    NeedsMore,
}

/// 工作区给 `image` 真开了解码器的那几种位图，以及各自的 mime。
///
/// 一张表三处用：猜 mime（[`guess_media_type`]）、判断「这格式值不值得为尺寸多读一趟」
/// （[`image_dims_from_head`]）、以及按**作者声明的 media-type** 判这张图要不要找降级路
/// （[`is_decodable_bitmap`]）。加一种格式只改这里，三处不会分叉。
const DECODABLE_BITMAPS: [(image::ImageFormat, &str); 5] = [
    (image::ImageFormat::Png, "image/png"),
    (image::ImageFormat::Jpeg, "image/jpeg"),
    (image::ImageFormat::WebP, "image/webp"),
    (image::ImageFormat::Gif, "image/gif"),
    (image::ImageFormat::Bmp, "image/bmp"),
];

/// 容器 / manifest **声明**的 media-type 是不是我们解得动的位图。
///
/// 与 [`image_dims_from_head`] 那条按字节头判的是同一张表，差别只在问的是作者写下的那一
/// 串：字节还没解压（甚至不值得解压）就先决定「这一份要不要换成降级的那一份」，问的正是
/// 声明。参数串（`image/jpeg;charset=`）与大小写都归到表那一边 —— 出版书里 `IMAGE/PNG`
/// 与带参数的写法都真的存在。
///
/// **`false` 不等于「这本书坏了」**：SVG 是完全合法的图，只是我们这一版没有栅格化器，
/// 所以调用方拿它去找作者备好的那一份，找不到就照旧留一块空。
pub(crate) fn is_decodable_bitmap(media_type: &str) -> bool {
    let mime = media_type
        .split(';')
        .next()
        .unwrap_or(media_type)
        .trim()
        .to_ascii_lowercase();
    DECODABLE_BITMAPS.iter().any(|(_, known)| *known == mime)
}

/// 认得出的格式 → 它的 mime；`image` 里没编解码器的（TIFF、AVIF 那类）回 `None`，
/// 因为对着 magic bytes 猜得出格式却没有解码器，等于拿不到尺寸。
fn decodable_bitmap_mime(format: image::ImageFormat) -> Option<&'static str> {
    DECODABLE_BITMAPS
        .iter()
        .find(|(known, _)| *known == format)
        .map(|(_, mime)| *mime)
}

/// 只按这段字节读尺寸，并按 [`HeadDims`] 说明「读不出来」是哪一种。
///
/// 只喂前缀时 `into_dimensions()` 会在字节用完处失败 —— 那正是 `NeedsMore` 的来路，
/// 所以调用方给的字节越短，越要把这三档当回事。
pub(crate) fn image_dims_from_head(bytes: &[u8]) -> HeadDims {
    let Ok(reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()
    else {
        return HeadDims::NotABitmap;
    };
    // 认不出 magic（None）或认得出但没解码器：都归到「不是解得动的位图」。
    let recognizable = reader
        .format()
        .is_some_and(|format| decodable_bitmap_mime(format).is_some());
    if !recognizable {
        return HeadDims::NotABitmap;
    }
    match reader.into_dimensions() {
        Ok((width, height)) => HeadDims::Sized((width, height)),
        Err(_) => HeadDims::NeedsMore,
    }
}

/// 扩展名 → 图片 mime。用在「容器没声明 media-type」的场合（HTML 的 `<img>`、FB2 的
/// `<binary>` 缺失 content-type）；认不出给 `application/octet-stream` 而不是猜 PNG。
pub(crate) fn media_type_of(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

pub fn detect(path: &Path) -> Option<Format> {
    provider_for(path).map(|provider| provider.format())
}

/// 把 `href` 相对 `base_dir`（通常是 OPF 或某章所在的目录）解析成容器内的规范路径。
/// 去掉 `#fragment`，做百分号解码，折叠 `./` 与 `../`。
///
/// 这是 epub 适配器与 app 侧图片 resolver 共用的同一份实现 —— 两边各写一次
/// 迟早会在「文件名里有空格 / 百分号编码」这类书上分叉。
pub fn resolve_href(base_dir: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href).trim();
    let decoded = percent_decode(href);
    let merged = if decoded.starts_with('/') {
        decoded.trim_start_matches('/').to_string()
    } else if base_dir.is_empty() {
        decoded
    } else {
        format!("{base_dir}{decoded}")
    };
    let mut stack: Vec<&str> = Vec::new();
    for seg in merged.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    stack.join("/")
}

/// 取 `href` 里 `#` 后面那一截并百分号解码；没有 `#`、截出来是空的、或根本不是元素 id
/// 的都是 `None`（= 跳章首）。
///
/// 必须在 [`resolve_href`] 之前取：那条的第一件事就是把 `#` 整条剥掉。解码也走同一份
/// 实现 —— 两处口径不一致的话，锚点里带百分号编码的书会在「路径解开了、锚点解不开」
/// 这种最难查的地方分叉。
///
/// 只认 XHTML 的 `id`。`epubcfi(...)` 是另一套定位语法、OEB 17 的 `$` 是历史遗留，
/// 这里都不解析：认不出时读者的表现是落到章首，而不是跳到一个错的位置上。
pub fn fragment_of(href: &str) -> Option<String> {
    // fragment 是**第一个** `#` 之后的全部内容（它自己也可以含 `#`）。
    let raw = href.split_once('#')?.1.trim();
    if raw.is_empty() || raw.starts_with("epubcfi(") {
        return None;
    }
    // `#a,xpointer(...)` 这种复合片段里只有第一段是元素 id。
    let id = raw.split(',').next().unwrap_or(raw);
    (!id.is_empty()).then(|| percent_decode(id))
}

/// 路径所在目录，带结尾 `/`；没有目录时是空串。
pub fn dir_of(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => String::new(),
    }
}

pub(crate) fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(n) = u8::from_str_radix(&input[i + 1..i + 3], 16)
        {
            out.push(n);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    // href 按 UTF-8 百分号编码；解出来的字节流不一定落在字符边界上，
    // 那种情况保留替换字符而不是整本读失败。
    String::from_utf8_lossy(&out).into_owned()
}

/// 把一份 CSS 里的 `url(…)` 按**那份 CSS 自己的目录**解成容器绝对路径。
///
/// 正文出库之后样式表按清单回容器现取（见 `crate::content::document::Document::stylesheet`），
/// 每一份都要在拼接前进这一步 —— 拼完就再也不知道某条 `url(Fonts/a.otf)` 原本写在
/// `OEBPS/Styles/` 还是 `OEBPS/` 下面。而 `@font-face` 的 `src` 要拿去查容器里的
/// 条目 —— 差一层目录的表现是「书里明明有字体，就是读不到」，且屏幕上看不出原因。
///
/// 输出**一律带引号**：解出来的路径常含空格（真书里的 `body italic.otf`），
/// 不加引号那是非法 CSS，解析器会在空格处断开。
pub(crate) fn absolutize_css_urls(css: &str, base_dir: &str) -> String {
    // 先清注释：注释里的 `url(x)` 不是声明，改写它等于往 CSS 里塞假路径。
    let text = crate::content::format::css::strip_comments(css);
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let Some(start) = find_url_function(rest) else {
            out.push_str(rest);
            return out;
        };
        // `rest[start..start + 4]` 就是 `url(`，`open` 是它之后第一位。
        let open = start + 4;
        let Some(close) = find_close_paren(&rest[open..]) else {
            // 括号不闭合：剩下的原样抄走，不猜。
            out.push_str(rest);
            return out;
        };
        let token_end = open + close + 1; // 整个 `url(…)` 函数 token 的结束（不含）
        let inner = rest[open..open + close]
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .trim();
        // `url(` 之前的部分先抄走；`url(` 本身**不能**跟着抄，否则改写会变成
        // `url(url("解出来的路径")` —— 括号一下就不配对了。
        out.push_str(&rest[..start]);
        if inner.is_empty() || crate::content::format::css::is_remote(inner) {
            // 远程与空的都原样留着：一期不取外部字体，也不假装取过。
            out.push_str(&rest[start..token_end]);
        } else {
            out.push_str("url(\"");
            out.push_str(&resolve_href(base_dir, inner));
            out.push_str("\")");
        }
        rest = &rest[token_end..];
    }
    out
}

/// 下一个 `url(` 函数 token 的起始字节偏移。
///
/// 要判前一个字符：`dataurl(` / `my-url(` 里都含 `url(`，按子串找会把它们当函数改写。
fn find_url_function(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i + 4 <= bytes.len() {
        if bytes[i..i + 4].eq_ignore_ascii_case(b"url(")
            && (i == 0 || !is_function_char(bytes[i - 1]))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 能构成一个标识符的字符：命中的话它前面那串就不是独立的 `url` 函数名。
fn is_function_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b >= 0x80
}

/// 从 `text` 开头找到配对 `)` 的位置（不含）。引号里的括号跳过。
fn find_close_paren(text: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            ')' => return Some(i),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod contract;

#[cfg(test)]
mod tests;
