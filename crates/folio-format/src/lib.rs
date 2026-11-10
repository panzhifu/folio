//! Folio 的文件格式解析：字节 + 路径 → 统一的文档模型。
//!
//! 这一层只回答「这份文件是什么、怎么把它读成章表与正文」。它不认识数据库、
//! 网络、界面与读者偏好；需要落盘目录的地方由调用方注入（见 `comic_cache::set_root_resolver`）。
//!
//! 边界靠依赖图强制：这里没有 rusqlite / tokio / axum / ureq / notify / dirs。
//! 想往这里加之前，先问「后端解析能脱离 app 单独用」这条还成不成立。

use std::path::Path;

pub use crate::href::{
    absolutize_css_urls, asset_href, dir_of, fragment_of, percent_decode, resolve_href,
};

pub mod char_class;
pub mod dom;
pub mod error;
pub mod fixtures;
pub mod href;
pub mod metrics;
pub mod scan;
#[cfg(test)]
pub(crate) mod test_support;
pub mod text_map;

pub use error::{Error, ImportReject, IoResultExt, Result};

/// RAR / 7z / tar 的共同读取层：列条目、按名抽一条。ZIP 的随机寻址仍在 [`zip`]。
pub mod archive;
/// AZW4：PalmDB 容器里装着一份 PDF（Kindle Print Replica）。
pub mod azw4;
/// 7z 漫画包：与 CBZ 同一族（定版式、逐页图片），只是容器换成了 7z。
pub mod cb7;
/// RAR 漫画包：与 CBZ 同一族，容器换成 RAR（纯 Rust `rars`）。
pub mod cbr;
/// tar 漫画包：与 CBZ 同一族，容器换成 tar。
pub mod cbt;
pub mod cbz;
/// 字节 → 文本的那一层（BOM / 声明 / 容器报的码页）。见模块头那四档顺序。
pub(crate) mod charset;
/// CHM（Microsoft Compiled HTML Help）：一个 ITSF 容器里的一组 HTML topic。
pub mod chm;
/// 7z / RAR / tar 漫画包解到磁盘的那一层缓存（CB7 / CBR / CBT）。
///
/// 这一层公开出去的只有一格 —— [`comic_cache::drop_for`]：界面上「删书」那一步要连着
/// 带走它那一格解压产物，而只有这一层知道缓存目录按什么键摆。
pub mod comic_cache;
/// ComicInfo.xml —— 漫画包的书目便条（ComicRack 那一族写手共用）。四种容器共用这一份读法。
pub(crate) mod comicinfo;
pub mod container;
pub mod content;
pub mod css;
/// DjVu：定版式扫描书/漫画，页是既定的画布（纯 Rust `djvu-rs`）。
pub mod djvu;
/// Word 97-2003（`.doc`）：一个 OLE2 复合文件里的 `WordDocument` 流。
///
/// 与 [`docx`](super::docx) 是**同一个格式的两代容器**（OOXML 与 [MS-DOC]），所以两边
/// 归约到的形状、章的切法、表格摊平那一条口径都得住在一处 —— 见 [`Marker`] 与那三个
/// `escape_*`（它们从前在两个 provider 里各有一份逐字相同的副本）。
pub mod doc;
/// WordprocessingML（`.docx`）：一个 ZIP 里的 `word/document.xml`。
pub mod docx;
pub mod epub;
pub mod fb2;
pub mod html;
/// Calibre HTMLZ：一个 ZIP 里装着 index.html、章节 HTML 与图片。
pub mod htmlz;
pub mod markdown;
/// MHTML（`.mht` / `.mhtml`，RFC 2557）：一个 MIME 存档里的一段 HTML 与它自己带的资源。
///
/// 与 [`htmlz`](super::htmlz) 同一套坐标（几份 HTML + 一堆字节），差别只在容器：那边是
/// ZIP 的条目名，这边是 `Content-Location` 折出来的键。
pub mod mhtml;
pub(crate) mod mobi;
/// 纯文本（`.txt`）小说的分章与书目提取：规则表 + 引擎两份，见 `novel` 的模块说明。
pub mod novel;
/// OpenDocument Text（`.odt`）：一个 ZIP 里的 `content.xml`。
pub mod odt;
// 压着的不是「没人用的模块」——MOBI / AZW4 / PDB 与判定层都在用它 —— 而是那张记录表里
// 没人读的三格（`path()` 与每条记录的 `attributes` / `uid`）。它们留在类型里是为了
// 读文件时一次到位、也方便排障时打印，但别当成「已经接上了」：删掉任何一格都会红。
#[allow(dead_code)]
pub(crate) mod palmdb;
/// PDB / PalmDoc：PalmDB 容器里的纯文本记录。
pub mod pdb;
/// PDF：一页是既定的画布，字节要栅格化才有像素。定版式那一族的成员之一（同族的还有
/// 漫画包那四种与 DjVu）。
pub mod pdf;
pub mod provider;
pub mod rtf;
pub mod sniff;
/// SVG 栅格化：把矢量资源（SVG 封面、逐页漫画、正文内联 SVG）画成位图。
pub mod svg;
/// 纯文本那一族（`.txt`），以及它与 Markdown 共用的那条链（读成文本 → 规范化 →
/// 折成章表 → 包装）。共用件只在这一份里有一份。
pub mod text;
pub mod woff;
pub mod woff2;
pub mod xhtml;
pub(crate) mod zip;

pub use content::{
    Content, ContentKind, CoverRef, FixedDoc, FixedPage, FlowDoc, PageChar, PageImage, PageRect,
    PageRegion, PageSource, PageText,
};
pub use provider::{
    DocumentProvider, ParseBudget, Passphrase, looks_like_a_book, provider_for, providers,
    supported_extensions, supports_extension,
};

// 章表与书目的**类型**住进了 folio-model（公共词汇表：书库与排版引擎不再为了几个类型
// 依赖本 crate 的解析机器）。这里沿用旧路径；与扫描 / 注册表 / DOM 绑定的行为
// （`Chapter::inline`、`char_len`、`copy_extension`、`PageText::text_map`）作为扩展
// trait 住在 [`ext`]。
pub use folio_model::format::{
    Chapter, ChapterLocator, ChapterText, DocMeta, Format, ImageRes, Resource, ResourceKind,
    Series, TocEntry, locator_for,
};

pub mod ext;
pub use ext::{ChapterExt, FlowDocExt, FormatExt, PageTextExt};

/// 索引方案的版本。**分章规则、位置方案、或定版式的页表算法一改就要加一。**
///
/// 库里的位置（[`ChapterLocator`]）是「拿什么规则算出来的」这件事的承诺：
/// `Section(3)` 只在「按今天这套规则分章」的前提下指到第三章。Markdown 的标题层级、
/// FB2 的 section 嵌套、EPUB 的 spine 过滤，任一改动都会让老库的序号指到别处 ——
/// 那时版本号对不上，[`crate::store::Store::stale_reason`] 会说「该重扫」。
///
/// 定版式文档（漫画包 / PDF / AZW4 / DjVu）没有章表，但页表同样是「按某套算法产出的索引」，
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
/// v8：`.txt` **按规则表分章**（`content::format::novel`）。这一族的章从「一整篇一章」
/// 变成「几十到几百章」，而 `ChapterLocator::Section(序号)` 存的就是那个序号 —— 不 bump
/// 的话老库里每一本 TXT 的进度都会指到第一章的第一页，批注 relocate 也全部落在错的章上。
/// 代价按那条不兼容的口径付：整库重扫一次，开书时才投，不在升级那一刻。
/// v9：`.md` 的**正文过一次真的 CommonMark 解析**（`content::format::markdown::to_xhtml`）。
/// 章表一个字都没动（还是按 1–2 级 ATX 切），但**可见文本这把尺变了**：`**粗**` 那四个
/// 星号以前在屏上看得见、现在折成 `<strong>` 就没了，`- 列表项` 的短横、围栏的反引号、
/// `[文字](链接)` 的方括号同理。而 `chapters.char_len` 与 `crate::Locator::offset` 数的
/// 都是这份可见文本 —— 不 bump 的后果是老 .md 书的进度分母偏大、批注锚点整体偏后，
/// 且差值随每章标记密度不均匀，所以不是「挪一格」而是**每一章错得不一样**。
/// 同一条 v8 的口径付：整库重扫一次，开书时由 `stale_reason` 认成 `OldIndex` 投递。
/// v10：`.docx` / `.odt` 读出了**列表与脚注**（`format::docx` 的 `w:numPr` +
/// `word/numbering.xml`、`w:footnoteReference` + `word/footnotes.xml`，`format::odt` 的
/// `text:list` + `text:list-style`、`text:note`）。章表没动，动的还是**可见文本那把尺**：
/// 引用那一个上标把注的号（`1`、`i.`）注进了正文，而注体里那些原先整块没读的字现在才算进
/// 本章 —— `chapters.char_len` 与 `crate::Locator::offset` 数的都是这份文本，所以老库里
/// 带脚注的那几本同时错两处：进度分母偏小，批注锚点整体偏前。列表本身不改字数，但它与
/// 脚注是同一次归约加的，同一版里一起重扫只付一次。
///
/// v11：**CHM 与单文件 HTML 的作者样式真的进库了**。CHM 从前把 `FlowDoc::css` 写死成
/// 空串（容器里没有 manifest 可指，样式躺在每一页的 `<head>` 里没人去读），单文件 HTML
/// 只认内联 `<style>`、`<link rel="stylesheet">` 那一份整个丢掉。两条都不动章表、也不动
/// 可见文本那把尺（改的是 CSS 这一层，字数与锚点一根字节都没变），动的是**落库的 `css`
/// 列**：`BookDocument::stylesheet` 见 `flow.css` 非空就直接用它（先文本后清单），所以
/// 老库里那两本 CHM 与那些 `.html` 会一直拿着「空」或「半份」的样式，屏上不显示任何异常、
/// 报表上也不算落差 —— 书的样式有没有生效本来就不在任何计数器里。代价按 v8 那一条口径付：
/// 整库重扫一次，开书时由 `stale_reason` 认成 `OldIndex` 投递。
///
/// v12：**PDF 的图片页按源图密度封顶栅格档位**（`format::pdf::native_edge`）。页表
/// （`fixed_json` 里每页的 `width` / `height`）存的是「这一页画出来多大」，从前一律是
/// 2048 那一档，现在扫描件那一页是源图自己的密度（`hugG.pdf` 那一本 1232）。老行不重扫
/// 不会画错（读的那一趟按页表现画，页表说 2048 就还是 2048），只是继续付那一笔 ——
/// 一页 816 ms 对 106 ms、位图 11.3 MB 对 4.1 MB。代价按 v8 那一条口径付：整库重扫一次，
/// 开书时由 `stale_reason` 认成 `OldIndex` 投递。
///
/// v14：**ZIP 的归属按容器骨架重判**。两条一起：`sniff::zip_shape` 认
/// `META-INF/container.xml` + 任意 `.opf` 为 EPUB（不再只看 `mimetype` 是否第一条），
/// 且 `provider_for` 对真实文件先嗅容器、再退扩展名。老库里那些被误判成 HTMLZ 的
/// EPUB（真书《藩屏》：多看导出、无 mimetype 条目）会从「按文件名排 28 个 xhtml」
/// 变成「按 OPF spine 读」，章数、章序、章内文本与 locator 全变 —— 不 bump 的话
/// 进度与批注会落在错的章。同 v8 口径：整库重扫，开书时逐本付。
///
/// v15：**可重排 EPUB2 的 `<guide type="toc">` 目录页收成一章**（即使 spine 上标了
/// `linear="no"`；见 `format::epub::KEEP_NONLINEAR`）。Kindle 导出的书常把排好版的
/// 目录页当非线性内容，此前被整个滤掉，封面之后直接落到扉页（《搜神记》）。
/// 只有「有 guide toc、该页在 spine 里、且被标成非线性」的书多一章，后面章号整体
/// 后移一格；没命中的书章表逐项不变。同 v3 / v6 口径：不 bump 的话命中书的进度会
/// 安静地指到隔壁那一章，所以整库重扫一次，开书时逐本付。
///
/// **这一格从此只管引擎，不管数据**：改 `assets/novel-rules.*.json` 里的任何一条模式或
/// 阈值不必 bump —— [`rules_hash`] 那一格会说「规则表换了」，老库在开书时逐本重扫，
/// 不必为一套正则发一版。要 bump 的仍然是动那台引擎：四条否决门的编排、号怎么读、
/// 切点怎么落（`novel::detect`），因为那是「同一份表算出什么」变了。
pub const INDEX_VERSION: i64 = 15;

/// 这一格式的索引算自哪一份小说规则表 —— 交出**此刻生效**那一份的内容哈希。
///
/// `None` = 这个格式的索引与那张表无关（Markdown 按 `#` 切、EPUB 按 spine 切，表里那些
/// 模式一条都碰不到它们）。落库（`store::content`）与判失效（`store::stale_reason`）问的
/// 是同一个问题，所以答案只有这一处：两处各写一遍「哪些格式看规则表」，早晚会一处把
/// `.md` 算进去、另一处不算，症状是同一本书每次开都重扫、或永远不重扫。
///
/// 只有 `.txt` 那一条依赖，而它依赖的是**编译好且在用**的那一份：将来读者那一层
/// （`plans/纯文本小说.md` 步骤四）换掉生效表时，改的是 [`novel::rules`] 里「生效那份」
/// 怎么取，这一格不必动。
pub fn rules_hash(format: Format) -> Option<String> {
    match format {
        // PalmDoc 的正文是纯文本，分章与书目同样交给那张规则表。
        Format::Text | Format::Pdb => Some(novel::rules::effective().content_hash().to_string()),
        _ => None,
    }
}

/// 一段引擎就绪的 XHTML 里**可见文本**的字符数 —— 与 [`crate::text_map::TextMap::chars`]
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

/// 同一条可见文本，但**不建坐标表**：搜索只认字，不认「第 N 个字落在哪个节点」。
///
/// 与 [`visible_text`] 逐字节相同时才允许替换 —— 两边多跳过或漏跳过一处，命中偏移
/// 就会与锚点那把尺错开。这条等价性由 `search::tests` 里那条用例钉着。
///
/// 省下的是每个文本节点一次 `path.clone()` 与一张按 DOM 节点数分配的 `spans` 表：
/// 一本一千多章的书，一趟搜索每章都要付一次。
pub fn visible_text_only(xhtml: &str) -> String {
    let dom = crate::dom::Dom::parse(xhtml.as_bytes());
    crate::text_map::visible_text_only(&dom, body_of(&dom))
}

/// 两条路共用的那一次解析：折出这一章的可见文本与它的坐标表。
fn text_map(xhtml: &str) -> crate::text_map::TextMap {
    let dom = crate::dom::Dom::parse(xhtml.as_bytes());
    crate::text_map::TextMap::build(&dom, body_of(&dom))
}

/// 正文根：有 `<body>` 用 `<body>`，裸片段（FB2 / Markdown 的包装产物、片段本身）
/// 就整棵当正文 —— `TextMap` 与 `visible_text_only` 自己会跳过 `<script>` / `<style>`。
fn body_of(dom: &crate::dom::Dom) -> crate::dom::NodeId {
    dom.elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map_or(dom.document, |(id, _)| id)
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
    /// 容器中央目录记的解压后长度（WOFF 是解回 sfnt **之前**那一层的长度）。
    ///
    /// 与 `crc32` 一起当字体身份用：别名缓存不能拿 `href` 当键，否则两本书的
    /// `Fonts/kt.ttf` 会互相认错。EPUB 从 `Zip::range_at` 的现成元数据取，不读字节；
    /// MOBI 没有 ZIP 区间，只能对已读出的字节算。
    pub raw_len: u64,
    /// 容器中央目录记的 CRC32（MOBI 是对字节现算的）。见 [`Self::raw_len`]。
    pub crc32: u32,
}

/// 一份字体的**身份与位置**，不含字节。
///
/// EPUB 的 `font_refs` 从中央目录直接拿 `crc32 + raw_len`，于是别名缓存可以
/// 「先按身份查，命中就不解压」；`read_font` 只在未命中时读字节。没有 ZIP 区间的
/// provider 返回 `None`，调用方退回 [`FontBytes`] 那条一次读全的老路。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FontRef {
    /// 容器内的规范路径。
    pub href: String,
    /// manifest 声明的 media-type。
    pub media_type: String,
    /// 容器记的解压后长度（WOFF 是解回 sfnt 之前的长度）；MOBI 是原始 FONT
    /// 记录的长度 —— 那边没有中央目录，身份只能对原始记录现算。
    pub raw_len: u64,
    /// 容器记的 CRC32；MOBI 是对原始 FONT 记录现算的（不解压）。
    pub crc32: u32,
}

impl FontRef {
    pub fn from_bytes(font: &FontBytes) -> Self {
        Self {
            href: font.href.clone(),
            media_type: font.media_type.clone(),
            raw_len: font.raw_len,
            crc32: font.crc32,
        }
    }
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

/// 从 MIME 推文件扩展名。认不出的回 `bin` 而不是空串 —— 空串会让文件名以点结尾，
/// 那种名字在某些文件系统上直接创建失败。
///
/// 真书里 `image/jpg`、`IMAGE/PNG`、`image/octet-stream` 这类写错很常见，
/// 所以这里只做小写归一化与前缀忽略，不试图猜内容（猜错了会影响文件管理器双击打开）。
pub fn media_extension(mime: &str) -> &'static str {
    let m = mime.trim().to_ascii_lowercase();
    match m.as_str() {
        "image/jpeg" | "image/jpg" | "image/pjpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" | "image/x-ms-bmp" => "bmp",
        "image/svg+xml" => "svg",
        "image/avif" => "avif",
        _ => "bin",
    }
}

// `COVER_SPEC_VERSION` 在 v16 作废：它记的是「这份封面字节是按哪一版规格生成的」，
// 用来让 `stale_reason` 认出该重生成的封面。封面改成 `covers/` 里的文件之后，
// 换规格只要重新生成缩略图（`content::assets::thumb::purge` + 重新生成），
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
    // 「本身就贵的事件」进计数器。口径 2026-10-31 起放宽了一格：除了「超过上限的图
    // 进图集之前先缩小」，阅读时那一档**低清预览**也走这里
    // （`folio_app::ui::foundation::images::DecodeRequest::preview`）—— 那一档的意义就是
    // 花一次小的解码换「屏上不是白纸」，所以这一列不再是「阅读时不该发生」，
    // 而是「每发生一次都值不值」。
    crate::metrics::Metric::ImageShrinks.count();

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
    // 与 `content::assets::thumb::encode_from` 同一道滤波也算一条理由：库里那份和磁盘那两档不该长得不一样。
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

/// 把 quick-xml 报的一个文本片段攒进 `out`。
///
/// quick-xml 0.42 把实体引用（`&amp;`、`&#38;`）报成**独立的 `Event::GeneralRef`**，
/// 文本在实体处被切成两段（实测：`Text("一个城市的") + GeneralRef("#38") + Text("与它的灯。")`）。
/// 只吃 `Event::Text` 的解析器会把实体整颗丢掉 —— 而 `&` 在书名与作者名里是常见写法
/// （`Harry Potter & the Goblet of Fire`、`Tom & Jerry`、`R&D`），丢掉的症状是书目少一个字符、
/// 正文少一个字符，屏幕上最难查。所以这里 `Text` 原样攒、`GeneralRef` 还原成 `&名字;` 再攒，
/// 由**消费那一步统一 unescape**（各文件自己的 `text_of`）。
pub(crate) fn xml_piece(out: &mut String, raw: &str, is_reference: bool) {
    if is_reference {
        // quick-xml 交回来的是**去掉 `&` 与 `;`** 的那一截（`amp`、`#38`）。
        let wrapped = format!("&{raw};");
        out.push_str(&xml_unescape(&wrapped));
    } else {
        out.push_str(&xml_unescape(raw));
    }
}

/// `&amp;` → `&`。折不动（原文里有一个不该出现的裸 `&`）就原样留着。
pub(crate) fn xml_unescape(raw: &str) -> String {
    quick_xml::escape::unescape(raw)
        .map(|text| text.into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

/// 把**文档格式自己的字**写成 XHTML 的文本那一截。
///
/// 与 [`xml_unescape`] 是相反的一刀：那边把源文件里的实体折成字符，这一族把字符折成
/// 实体。住在 `mod.rs` 而不是各 provider 里，是因为**归约产物只有一个形状**：Word 两代
/// 容器（[`docx`](super::docx) 与 [`doc`](super::doc)）、ODF（[`odt`](super::odt)）都要把
/// 「一段不带标记的正文」包成 `<p>`，而 `&` 少折一次的症状是那一章从此解析不成 DOM
/// —— 字数、锚点、进度全部偏，且屏上看不出原因。三处各有一份逐字相同的副本已经漂过一次
/// 的教训不必再付一遍。
pub(crate) fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text);
    out
}

pub(crate) fn escape_into(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

/// 属性那一截：文本那三条再加一个引号。
pub(crate) fn escape_attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text);
    out.replace('"', "&quot;")
}

/// 列表该画的那一个号。
///
/// 它与 `css::computed::ListStyle` 不是同一件事：那一格是作者写在 CSS 里的词，这一格是
/// **文档格式自己的记数**折过来之后的形状（Word 的 `w:numFmt`、ODF 的 `style:num-format`
/// 与 `text:bullet-char`）。两个 provider 各折一次就会各漂一次，所以形状与「落成哪一截
/// XHTML」收在这一处，由 [`ListWriter`] 一起写。
///
/// 表外的记数（希伯来、阿拉伯、泰文、印度诸系统）折成 `Ordered(None)`：文档格式已经说了
/// 「这是一个编号列表」，而引擎没有那些记数 —— 画一个十进制号比画一个圆点更接近作者要的，
/// 也比静默丢掉这一格可读。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Marker {
    Disc,
    Circle,
    Square,
    /// 记数：`None` = 用引擎给 `<ol>` 的默认（十进制）；`Some` = `list-style-type` 那一词。
    Ordered(Option<&'static str>),
    /// 一个字符都不画。作者明确说了不要号（Word 的 `w:numFmt="none"`），缩进留着。
    Nothing,
}

impl Marker {
    /// 开这一层时写的那一截。`list-style-type` 只在**不是这一族的默认**时才写：
    /// `<ul>` 本来就是圆点、`<ol>` 本来就是十进制，多写一截只会让归约产物难以比对。
    fn open_tag(self) -> &'static str {
        match self {
            Marker::Disc => "<ul>",
            Marker::Circle => r#"<ul style="list-style-type: circle">"#,
            Marker::Square => r#"<ul style="list-style-type: square">"#,
            Marker::Ordered(None) => "<ol>",
            Marker::Ordered(Some("lower-alpha")) => r#"<ol style="list-style-type: lower-alpha">"#,
            Marker::Ordered(Some("upper-alpha")) => r#"<ol style="list-style-type: upper-alpha">"#,
            Marker::Ordered(Some("lower-roman")) => r#"<ol style="list-style-type: lower-roman">"#,
            Marker::Ordered(Some("upper-roman")) => r#"<ol style="list-style-type: upper-roman">"#,
            Marker::Ordered(Some("cjk-ideographic")) => {
                r#"<ol style="list-style-type: cjk-ideographic">"#
            }
            // `Ordered` 的 `Some` 只可能是上面这几个（两个 provider 都从这一张表来）。
            Marker::Ordered(Some(_)) => "<ol>",
            Marker::Nothing => r#"<ul style="list-style-type: none">"#,
        }
    }

    fn close_tag(self) -> &'static str {
        match self {
            Marker::Ordered(_) => "</ol>",
            _ => "</ul>",
        }
    }
}

/// 一层正开着的项目列表。
struct OpenList {
    /// 这一层是**哪一个**列表。DOCX 用 `w:numId`：两份编号实例就算记数相同也是两个列表，
    /// Word 的号跟着实例从头再数（真样本 `numbering.docx` 的 `numId` 1 与 2 都是
    /// `decimal`，写成一条就会把 1 / 1、2 数成 1 / 2、3）。
    id: String,
    marker: Marker,
    /// 这一层当前那个 `<li>` 还开着（`</li>` 还没写）。往里再开一层要靠它 ——
    /// 嵌套在 XHTML 里就是「子列表长在父项里面」。
    item_open: bool,
}

/// 正在往里写字的那几层列表（栈顶 = 当前层）。
///
/// 两族 driving 方式不同：**DOCX** 只在段落上挂一个 `w:ilvl`（层号是绝对的，可以跳级、
/// 也可以回头），所以它按层号定位；**ODT** 的 `<text:list>` 是嵌套元素，层由容器自己开合。
/// 两种驱动写的却是同一串 XHTML，所以这台状态机只有一份。
#[derive(Default)]
pub(crate) struct ListWriter {
    layers: Vec<OpenList>,
}

impl ListWriter {
    /// 有没有层开着，与开着几层（ODT 那一族按 `<text:list>` 的嵌套定层级，层号就取这个数）。
    pub(crate) fn depth(&self) -> usize {
        self.layers.len()
    }

    /// 往里再开一层。上一层那个 `<li>` 故意不关（见 [`OpenList::item_open`]）。
    pub(crate) fn push(&mut self, id: &str, marker: Marker, out: &mut String) {
        out.push_str(marker.open_tag());
        out.push('\n');
        self.layers.push(OpenList {
            id: id.to_string(),
            marker,
            item_open: false,
        });
    }

    /// 关掉栈顶那一层（连同它当前那个 `<li>`）。
    pub(crate) fn pop(&mut self, out: &mut String) {
        let Some(layer) = self.layers.pop() else {
            return;
        };
        if layer.item_open {
            out.push_str("</li>\n");
        }
        out.push_str(layer.marker.close_tag());
        out.push('\n');
    }

    /// 关掉所有开着的层。换普通段落、换章、整篇结束都要先走这一步，
    /// 否则 `</ul>` 会落到下一章的正文里。
    pub(crate) fn close_all(&mut self, out: &mut String) {
        while !self.layers.is_empty() {
            self.pop(out);
        }
    }

    /// 把 `html` 写成**第 `level` 层**（0 起）的一项：需要时开层、退层。
    ///
    /// 同一层换了记数**或换了列表实例**（`id`）就关掉再开：关掉重开的代价是号跟着从头
    /// 数，而那正是 Word 换 `numId` 时的行为。目标层与已有层之间缺的那几层用同一个
    /// 记数补上：`w:ilvl` 从 0 跳到 2 的文档真存在，中间那层没有别的依据可取。
    pub(crate) fn write(
        &mut self,
        level: usize,
        id: &str,
        marker: Marker,
        html: &str,
        out: &mut String,
    ) {
        while self.layers.len() > level + 1 {
            self.pop(out);
        }
        if self.layers.len() == level + 1
            && (self.layers[level].marker != marker || self.layers[level].id != id)
        {
            self.pop(out);
        }
        while self.layers.len() < level + 1 {
            self.push(id, marker, out);
        }
        let top = self
            .layers
            .last_mut()
            .expect("上面已经补齐到 level + 1 层了");
        if top.item_open {
            out.push_str("</li>\n");
        }
        top.item_open = true;
        out.push_str("<li>");
        out.push_str(html);
    }

    /// 往**当前那一层**添一项（调用方已经用 [`push`] 把这一层开了）。
    pub(crate) fn item(&mut self, html: &str, out: &mut String) {
        let Some(top) = self.layers.last_mut() else {
            // 没有层可写：这一格就当普通段落，别凭空吐出一个孤零零的 `<li>`。
            out.push_str("<p>");
            out.push_str(html);
            out.push_str("</p>\n");
            return;
        };
        if top.item_open {
            out.push_str("</li>\n");
        }
        top.item_open = true;
        out.push_str("<li>");
        out.push_str(html);
    }
}

/// 本章正文里**登记过**的图片引用（`index()` 那一趟要落进 `chapters.images_json` 的那一份）。
///
/// 过滤而不是照单全收：`<img src>` 解出来的一条引用可能指向**没登记上**的那一张（字节读不到、
/// 或这一版没有解码器）。留在清单里的症状不是「少一张图」而是「图片查看器里那一列多出一个
/// 点不开的格子」。`docx` / `odt` / `doc` 三个 provider 从前各有一份逐字相同的实现。
pub(crate) fn chapter_images(body: &str, known: &[ImageRes]) -> Vec<crate::scan::ImageUse> {
    let known: std::collections::HashSet<&str> =
        known.iter().map(|image| image.href.as_str()).collect();
    crate::scan::chapter_summary(body)
        .images
        .into_iter()
        .filter(|image| known.contains(image.src.as_str()))
        .collect()
}

/// 一段 base64 文本 → 字节。折行与空白一律跳过，解不出就是 `None`。
///
/// 存在的理由：`<binary>`（FB2）与 MHTML 的部分体都是「折成 76 列的 base64」，而解法里
/// 那条「先去掉所有空白再解」是要紧的一半 —— 不去掉，`STANDARD` 会在第一个换行处报错，
/// 症状是**每一张图都没有**。两处各写一遍迟早有一处忘了那一半。
pub(crate) fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &compact).ok()
}

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
const DECODABLE_BITMAPS: [(image::ImageFormat, &str); 6] = [
    (image::ImageFormat::Png, "image/png"),
    (image::ImageFormat::Jpeg, "image/jpeg"),
    (image::ImageFormat::WebP, "image/webp"),
    (image::ImageFormat::Gif, "image/gif"),
    (image::ImageFormat::Bmp, "image/bmp"),
    (image::ImageFormat::Tiff, "image/tiff"),
];

/// 容器 / manifest **声明**的 media-type 是不是我们解得动的位图。
///
/// 与 [`image_dims_from_head`] 那条按字节头判的是同一张表，差别只在问的是作者写下的那一
/// 串：字节还没解压（甚至不值得解压）就先决定「这一份要不要换成降级的那一份」，问的正是
/// 声明。参数串（`image/jpeg;charset=`）与大小写都归到表那一边 —— 出版书里 `IMAGE/PNG`
/// 与带参数的写法都真的存在。
///
/// **`false` 不等于「这本书坏了」**：它只说明 `image` 这一层没有解码器 —— SVG 就落在
/// 这一格里，而它由 [`svg`] 在真要画的时候栅格化，所以 EPUB 只是拿它决定「要不要沿
/// `fallback` 找一份位图」；找不到时那张 SVG 照样登记、照样画得出。拿它当登记门槛的
/// 地方（MHTML 那一条）会把声明成 SVG 的图跳过。
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
pub fn media_type_of(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

pub fn detect(path: &Path) -> Option<Format> {
    provider_for(path).map(|provider| provider.format())
}

#[cfg(test)]
mod contract;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod boundary_tests {
    /// 这一层只认字节与路径：清单里出现数据库、网络、语音、文件监听或字体子集化，
    /// 就说明依赖又漏了。逐行看键名而不是子串：`tts` 会撞进注释里的英文单词。
    #[test]
    fn the_manifest_stays_free_of_app_backends() {
        let manifest = include_str!("../Cargo.toml");
        let forbidden = [
            "rusqlite",
            "libsqlite3-sys",
            "tokio",
            "axum",
            "ureq",
            "notify",
            "dirs",
            "tts",
            "skera",
            "read-fonts",
            "write-fonts",
        ];
        for line in manifest.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let key = line.split(['=', '.', ' ']).next().unwrap_or("");
            assert!(
                !forbidden.contains(&key),
                "folio-format 的清单里出现了 `{key}`：这一层的边界是不认数据库 / 网络 / \
                 语音 / 文件监听 / 字体子集化"
            );
        }
    }
}
