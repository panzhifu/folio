//! 统一的文档接口 —— KOReader `document/document.lua` 那一层在 Rust 里的对应物。
//!
//! 格式差别到 [`ContentKind`] 的两个家族为止：往上一律是 [`Document`]，界面不再问
//! 「这是什么格式」，只问「可重排还是定版式」。「怎么解析」在
//! [`crate::content::format::provider`]，这里管「读者看到什么」。
//!
//! 与 KOReader 的差别只有一处：那边 `Document` 子类同时持有引擎与页缓存、还负责渲染；
//! 这里 core 不渲染 —— [`Document`] 只给出内容与按需取字节的入口，排版与绘制在引擎
//! 与界面。这条边界也是它能在 `cargo test` 里被全量覆盖的原因。

mod book;

pub use book::BookDocument;

use std::path::Path;

use crate::Result;
use crate::content::format::{
    Content, ContentKind, DocMeta, FixedDoc, FlowDoc, FontBytes, Format, ImageRes, ParseBudget,
    TocEntry,
};

/// 阅读位置的坐标系。可重排是「章 + 字符偏移」，定版式是「页码」。
///
/// 这一条决定进度怎么存、怎么恢复：把页码存进字符偏移的书里，改一次字号就全废了。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocatorKind {
    Char,
    Page,
}

/// 借出的内容视图。
///
/// 用视图而不是 `&Content`：调用方关心的是「可重排的那部分」或「定版式的那部分」，
/// 给它一个还要自己 match 的枚举没有意义。
#[derive(Debug, Clone, Copy)]
pub enum ContentRef<'a> {
    Reflow(&'a FlowDoc),
    Fixed(&'a FixedDoc),
}

/// 一本已解析、可阅读的书。
pub trait Document: Send + Sync {
    fn meta(&self) -> &DocMeta;

    /// 内容家族。界面靠它分流：可重排走排版引擎，定版式走页面绘制。
    fn kind(&self) -> ContentKind;

    fn content(&self) -> ContentRef<'_>;

    /// 容器里某个 `href` 的字节（图片，将来也可能是别的资源）。
    /// 读不到是 `None`，不是错误 —— 那块地方空着就好。
    fn image_bytes(&self, href: &str) -> Option<Vec<u8>>;

    /// 这本书的作者样式表（拼接后的 CSS 文本）。
    ///
    /// 索引路径下库里只有位置清单（[`crate::content::format::FlowDoc::css_hrefs`]），
    /// 这里按清单回容器现取：每份用它**自己所在目录**做 `url()` 绝对化，按文档序
    /// 拼接（顺序即层叠顺序）。某一份读不到就跳过并记日志 —— 与图片缺失同一个
    /// 立场：少一份样式，正文照排。调用方（阅读会话）每书取一次。
    fn stylesheet(&self) -> String;

    /// `@font-face` 要的那几份内嵌字体。默认没有。
    fn fonts(&self, _wanted: &[String]) -> Vec<FontBytes> {
        Vec::new()
    }

    fn format(&self) -> Format {
        self.meta().format
    }

    fn locator_kind(&self) -> LocatorKind {
        match self.kind() {
            ContentKind::Reflow => LocatorKind::Char,
            ContentKind::Fixed => LocatorKind::Page,
        }
    }

    fn is_fixed(&self) -> bool {
        self.kind() == ContentKind::Fixed
    }

    // 内容里本来就有的三项给个直通，省得每个调用点都 match 一遍。
    fn toc(&self) -> &[TocEntry] {
        match self.content() {
            ContentRef::Reflow(flow) => &flow.toc,
            ContentRef::Fixed(fixed) => &fixed.toc,
        }
    }

    /// 封面落盘之后的描述（`covers/<hash>.<ext>` 那两格）。
    ///
    /// **由导入管线填，不是 provider 填**：provider 只交字节，它不知道书库在哪儿。
    /// 默认 `None` = 没有封面文件，读那一侧退回 `books.cover` 那个 BLOB。
    fn cover_ref(&self) -> Option<&crate::content::format::cover::CoverRef> {
        None
    }

    /// 填上封面描述。只有可重排那一支实现它 —— 定版式的封面是首页缩略图，
    /// 走另一条路（`save_fixed`）。
    fn set_cover_ref(&mut self, _cover: Option<crate::content::format::cover::CoverRef>) {}

    fn cover(&self) -> Option<&[u8]> {
        match self.content() {
            ContentRef::Reflow(flow) => flow.cover.as_deref(),
            ContentRef::Fixed(fixed) => fixed.cover.as_deref(),
        }
    }

    fn images(&self) -> &[ImageRes] {
        match self.content() {
            ContentRef::Reflow(flow) => &flow.images,
            ContentRef::Fixed(fixed) => &fixed.images,
        }
    }

    fn flow(&self) -> Option<&FlowDoc> {
        match self.content() {
            ContentRef::Reflow(flow) => Some(flow),
            ContentRef::Fixed(_) => None,
        }
    }

    fn fixed(&self) -> Option<&FixedDoc> {
        match self.content() {
            ContentRef::Fixed(fixed) => Some(fixed),
            ContentRef::Reflow(_) => None,
        }
    }

    /// 定版式的页数。可重排**没有**「页数」这个事实（页是排版结果），返回 `None` ——
    /// 那是界面按当前视口算出来的读数，不该由文档来回答。
    fn page_count(&self) -> Option<usize> {
        self.fixed().map(FixedDoc::page_count)
    }

    /// 第 `index` 章的**引擎就绪 XHTML**，需要时才去容器里取。
    ///
    /// 库里只存位置不存正文（见 [`crate::content::format::ChapterLocator`] 与
    /// `plans/text-out-of-db.md`），所以这是「读一本已经打开的书」唯一的正文入口：
    ///
    /// - 位置是 [`ChapterLocator::Inline`]：正文就在手上（测试、旧库兜底），直接给。
    /// - 位置是 [`ChapterLocator::Entry`]：让 provider 解一个 zip 条目（EPUB）。
    /// - 位置是 [`ChapterLocator::Section`]：单文件格式，重读一遍再取第 N 章。
    ///
    /// **不做缓存**：调用方（阅读会话）按章缓存，因为只有它知道哪些章还会被翻到。
    /// 取不到时是 [`crate::Error::ChapterUnavailable`]，不是空串 —— 那个区别决定了
    /// 界面要说「这一章是空的」还是「源文件不在了」。
    fn chapter_source(&self, index: usize) -> Result<String>;
}

/// 打开一本书，返回统一接口。
///
/// **`Content` 的两个家族都从这里出去**，所以调用方拿到的东西与格式无关；
/// 分流靠 `kind()`（`Reflow` / `Fixed`），不靠扩展名。
///
/// 这是 app 侧唯一的「文件 → 文档」入口。低一层还有两个入口用于**内容已经在手上**的
/// 场合（书库读回、测试替身），见 [`from_content`] / [`with_container`]。
pub fn open(path: &Path) -> Result<Box<dyn Document>> {
    Ok(Box::new(BookDocument::open(path)?))
}

/// 同 [`open`]，但把这一本内部的解析并行度交给调用方分（[`ParseBudget`]）。
///
/// 只有导入一批书时该用它：那时「同时有几本在飞」是外层知道的数，每本内部几路必须由它
/// 一并算出，两层相乘才不超过这台机器。单本打开（读者点开了一个文件）用 [`open`] 就对。
pub fn open_with(path: &Path, budget: ParseBudget) -> Result<Box<dyn Document>> {
    Ok(Box::new(BookDocument::open_with(path, budget)?))
}

/// 从已经解析好的内容建一份文档，**没有容器**。
///
/// 书库读回（`store.load_content`）、宿主测试的合成书、将来的内置示例都走这一条：
/// 正文与坐标齐全，而 `image_bytes` / `fonts` 回空 —— 与「容器丢了」是同一个表现。
pub fn from_content(meta: DocMeta, content: Content) -> Box<dyn Document> {
    Box::new(BookDocument::from_content(meta, content))
}

/// 内容来自书库（**只有索引**），正文、图片与字体的字节都从**原文件**现取。
///
/// 「按主键打开」那条路用它：库里存的是章节表、CSS、目录、封面与图片元数据，
/// 而正经的正文、图片字节、内嵌字体都要回到容器里读 —— 前者按
/// [`format::ChapterLocator`]，后两者按 `href`（见 `format::ImageRes` /
/// `format::FontBytes`）。
pub fn with_container(meta: DocMeta, content: Content, path: &Path) -> Box<dyn Document> {
    Box::new(BookDocument::with_container(meta, content, path))
}

/// 只取书目元信息，不建正文模型 —— 书库列书目时用这一条。
pub fn metadata(path: &Path) -> Result<DocMeta> {
    let provider = crate::content::format::provider_for(path).ok_or_else(|| unsupported(path))?;
    provider.metadata(path)
}

/// 「一个 provider 都没认上」是导入失败里最常见的一种，留一句日志：
/// 读者与开发者否则看到的都是同一句「不支持的格式」。
pub(crate) fn unsupported(path: &Path) -> crate::Error {
    tracing::warn!("document: 没有 provider 认得 {}", path.display());
    crate::Error::UnsupportedFormat(
        path.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
    )
}
