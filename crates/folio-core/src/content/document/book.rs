//! provider 解析出来的那本书 —— [`Document`] 的通用实现。
//!
//! 只有这一个实现：格式差异已经被 provider 抹平成 [`Content`]，这里不再分格式。
//! 将来若要造一份「不是从文件来的」文档（测试替身、内置示例），也实现同一个 trait。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{ContentRef, Document, unsupported};
use crate::Result;
use crate::content::format::{
    self, ChapterLocator, ChapterText, Content, ContentKind, DocMeta, DocumentProvider, FontBytes,
};

pub struct BookDocument {
    /// 留住 provider：图片与字体的字节要按 href 现取，只有它知道容器怎么开。
    ///
    /// `None` = **没有容器可谈**：内容来自书库或测试替身（[`BookDocument::from_content`]）。
    /// 那时取字节一律回空 —— 与「容器丢了」是同一个表现（那块地方空着，正文照排）。
    provider: Option<Box<dyn DocumentProvider>>,
    path: Option<PathBuf>,
    /// **开一次的容器句柄**（`plans/导入管线.md` §十）。
    ///
    /// 与 `provider` 并存而不是取代它：`Container` 管的是「按条目名取字节」这件
    /// 与格式无关的事，而封面怎么认、字体清单怎么读、`@font-face` 指哪个条目
    /// 都是格式知识，仍归 provider。取字节时容器先试，试不到（单文件格式、
    /// 名字不是条目名）再退回 provider —— 两条路都在，谁先谁后见
    /// [`BookDocument::image_bytes`]。
    ///
    /// 懒开：绝大多数书在这次会话里根本不会被取字节，所以 `OnceLock` 里的
    /// `Container` 要等第一次取字节才真的 `Zip::open`（见那个模块的头）。
    container: std::sync::OnceLock<crate::content::format::container::Container>,
    /// `href` → 字节真实所在的那条目，**只收降级过的那几张**。
    ///
    /// 存在理由是 EPUB3 的 `<item fallback="…">`：正文里那串 `<img src>` 指的是这一版解不动
    /// 的那一份（逐页 SVG 是主流那个），而字节躺在同一个容器的另一条目里 —— 那一格就是
    /// [`ImageRes::source`](crate::content::format::ImageRes::source)。键仍是
    /// `<img src>` 解出来的那一条，因为正文一个字都不许改（锚点坐标建在源文本上）。
    ///
    /// 第一次取字节时才从图片元数据里筛：绝大多数书一张降级路都没挂，那时它就是一张空表。
    image_sources: std::sync::OnceLock<HashMap<String, String>>,
    meta: DocMeta,
    content: Content,
}

impl BookDocument {
    /// 从文件读一本：选 provider → 读成 [`Content`] → 校验结构完整性。
    ///
    /// 走的是 [`DocumentProvider::index`]：**只读索引，不解正文**。正文不在库里
    /// （见 `plans/text-out-of-db.md`），导入时解出来再丢掉等于白付一次全量解压 ——
    /// 一本 1000 多章的 EPUB 就是白解 1000 多个 zip 条目。
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, format::ParseBudget::Auto)
    }

    /// 同 [`open`](Self::open)，但把这一本**内部**的解析并行度交给调用方分。
    ///
    /// 谁需要它：导入一批书的那一路。一批同时导 M 本、每本内部 I 路，真正抢核的是
    /// M × I，而只有外层知道 M —— 于是它把 I 算好传下来（`ingest::import::split_budget`），
    /// 而不是让两层各自按整机核数猜。读者打开一本书用的是 [`open`]（那时 `Auto` 就是
    /// 整机那一档：它是唯一在跑的那本）。
    pub fn open_with(path: &Path, budget: format::ParseBudget) -> Result<Self> {
        let provider = format::provider_for(path).ok_or_else(|| unsupported(path))?;
        tracing::debug!(
            "document: {} 交给 {} provider（只读索引）",
            path.display(),
            provider.format().as_str()
        );
        let (meta, content) = provider.index_with(path, budget)?;
        // 校验只写日志，不阻塞阅读。
        crate::ingest::validate::check(&content, path);
        Ok(Self {
            provider: Some(provider),
            path: Some(path.to_path_buf()),
            container: std::sync::OnceLock::new(),
            image_sources: std::sync::OnceLock::new(),
            meta,
            content,
        })
    }

    /// 已经有内容、没有容器。书库读回的书、测试替身、将来的内置示例走这一条：
    /// 正文齐全，但 `image_bytes` / `fonts` 回空。
    pub fn from_content(meta: DocMeta, content: Content) -> Self {
        // 校验只写日志，不阻塞阅读。路径用书名占位（没有文件）。
        crate::ingest::validate::check(&content, Path::new(&meta.title));
        Self {
            provider: None,
            path: None,
            container: std::sync::OnceLock::new(),
            image_sources: std::sync::OnceLock::new(),
            meta,
            content,
        }
    }

    /// 内容来自书库、字节仍从**原文件**现取。
    ///
    /// 这是「按主键打开」那条路：库里只有章节位置（[`ChapterLocator`]），正文按
    /// 位置回容器现取（[`Document::chapter_source`]）；图片与内嵌字体同样刻意
    /// 不入库，按 `href` 现取。
    ///
    /// 文件不在了**不是错误**：`provider_for` 只按扩展名认领，真正的读取失败会在
    /// 取字节时回空 —— 表现就是没有插图、没有补缺字的那一份，正文照排。
    pub fn with_container(meta: DocMeta, content: Content, path: &Path) -> Self {
        Self {
            provider: format::provider_for(path),
            path: Some(path.to_path_buf()),
            container: std::sync::OnceLock::new(),
            image_sources: std::sync::OnceLock::new(),
            meta,
            content,
        }
    }

    /// 这本书是从哪个文件来的；内容来自内存时是 `None`。
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// 取字节的两条路共用的一段：没有 provider 或没有路径就是没有字节。
impl BookDocument {
    fn container(&self) -> Option<(&dyn DocumentProvider, &Path)> {
        Some((self.provider.as_deref()?, self.path.as_deref()?))
    }

    /// 那个**开一次的容器句柄**；没有文件可谈时是 `None`。
    ///
    /// 第一次调用把它建起来（但**不**开文件 —— `Container` 自己懒开），之后每次
    /// 取字节都命中同一个句柄。
    fn shared(&self) -> Option<&crate::content::format::container::Container> {
        let path = self.path.as_deref()?;
        Some(
            self.container
                .get_or_init(|| crate::content::format::container::Container::new(path)),
        )
    }

    /// 这条 `href` 的字节该**去哪儿读**。
    ///
    /// 绝大多数时候答案就是它自己；只有作者挂了降级路（EPUB3 的 `<item fallback="…">`，
    /// 见 [`ImageRes`](crate::content::format::ImageRes)）的那几张换了去处。换的地方收在
    /// 这一处，于是下面取字节那两条路（容器先试、退回 provider）都不必各自再知道
    /// 「格式可以声明一份读不动的东西」这件事 —— 那是 provider 层的知识，不是这一层的。
    fn image_at<'a>(&'a self, href: &'a str) -> &'a str {
        let map = self.image_sources.get_or_init(|| {
            self.images()
                .iter()
                .filter(|img| img.source != img.href)
                .map(|img| (img.href.clone(), img.source.clone()))
                .collect()
        });
        map.get(href).map(String::as_str).unwrap_or(href)
    }
}

impl Document for BookDocument {
    fn meta(&self) -> &DocMeta {
        &self.meta
    }

    fn kind(&self) -> ContentKind {
        self.content.kind()
    }

    fn content(&self) -> ContentRef<'_> {
        match &self.content {
            Content::Reflow(flow) => ContentRef::Reflow(flow),
            Content::Fixed(fixed) => ContentRef::Fixed(fixed),
        }
    }

    fn cover_ref(&self) -> Option<&crate::content::format::cover::CoverRef> {
        match &self.content {
            Content::Reflow(flow) => flow.cover_ref.as_ref(),
            // 定版式的封面是首页缩略图，存在 `save_fixed` 那条路上，没有文件描述。
            Content::Fixed(_) => None,
        }
    }

    fn set_cover_ref(&mut self, cover: Option<crate::content::format::cover::CoverRef>) {
        // 只对可重排生效：定版式没有「从容器里抽出来的封面」这个概念。
        if let Content::Reflow(flow) = &mut self.content {
            flow.cover_ref = cover;
        }
    }

    fn image_bytes(&self, href: &str) -> Option<Vec<u8>> {
        // 先问「去哪儿读」：只有挂了降级路的那几张与 `href` 不同（见 [`Self::image_at`]）。
        // 界面上那串 `<img src>` 解出来的仍是 `href`，它当查询键用着才对。
        let at = self.image_at(href);
        // 容器先试：`at` 就是归一化条目名（资源表里存的那一形），命中就免掉
        // 「重新打开 + 重扫中央目录」那两笔 —— 这是滚动取图的热路径。
        if let Some(bytes) = self.shared().and_then(|container| container.read(at)) {
            return Some(bytes);
        }
        // 退回 provider：单文件格式（HTML 的图是磁盘上挨着它的另一个文件）、
        // 或者 `at` 不是条目名（FB2 用 `#id` 指 `<binary>`）。
        let (provider, path) = self.container()?;
        provider.image_bytes(path, at)
    }

    fn stylesheet(&self) -> String {
        let ContentRef::Reflow(flow) = self.content() else {
            // 定版式没有作者样式表。
            return String::new();
        };
        if !flow.css.is_empty() {
            // 文本就在手上（`read` 全文路径、`from_content`、旧库兜底）—— 直接用。
            return flow.css.clone();
        }
        if flow.css_hrefs.is_empty() {
            return String::new();
        }
        let Some((provider, path)) = self.container() else {
            // 清单在、容器不在：与取不到章正文同一表现 —— 没有作者样式，正文照排。
            tracing::warn!(
                "document: 作者样式 {} 份取不到（容器不在）",
                flow.css_hrefs.len()
            );
            return String::new();
        };
        let mut css = String::new();
        for href in &flow.css_hrefs {
            // 与取图同一条：容器先试（几份样式表共用一次打开），再退回 provider。
            let text = self
                .shared()
                .and_then(|container| container.read_text(href))
                .or_else(|| provider.entry_text(path, href));
            let Some(text) = text else {
                tracing::debug!("document: 样式表 {href} 不在容器里，跳过");
                continue;
            };
            // 每份用它自己的目录解 `url()` —— 拼接之前做，基准才不会串。
            css.push_str(&crate::content::format::absolutize_css_urls(
                &text,
                &crate::content::format::dir_of(href),
            ));
            css.push('\n');
        }
        css
    }

    fn fonts(&self, wanted: &[String]) -> Vec<FontBytes> {
        match self.container() {
            Some((provider, path)) => provider.fonts(path, wanted),
            None => Vec::new(),
        }
    }

    fn chapter_source(&self, index: usize) -> Result<String> {
        let ContentRef::Reflow(flow) = self.content() else {
            // 定版式没有「章」，也没有正文要取 —— 页面自己就是内容。
            return Ok(String::new());
        };
        let Some(chapter) = flow.chapters.get(index) else {
            return Ok(String::new());
        };
        let at = chapter.locator().cloned().unwrap_or_default();
        // 正文已经在手上（测试替身、`from_content`）。
        if let ChapterText::Inline(text) = &chapter.text {
            return Ok(text.clone());
        }
        if let ChapterLocator::Inline(text) = &at {
            return Ok(text.clone());
        }
        // 剩下两种都要容器。没有容器（内容从库里读回、而正文又没跟着回来）就是
        // 「这本书读不了」——不是空章，得让上面看得见。
        let (path, provider) = match self.container() {
            Some((provider, path)) => (path, provider),
            None => {
                return Err(crate::Error::ChapterUnavailable {
                    path: self.path.clone().unwrap_or_default(),
                    index,
                    at: format!("{at:?}"),
                });
            }
        };
        match &at {
            ChapterLocator::Entry(entry) => {
                // 容器先试：翻章要读一整章正文，而「重开一次容器」在这里是纯亏
                // （条目名索引在同一本书里不变）。命中就免掉那次打开。
                if let Some(text) = self
                    .shared()
                    .and_then(|container| container.read_text(entry))
                {
                    return Ok(text);
                }
                provider
                    .entry_text(path, entry)
                    .ok_or_else(|| crate::Error::ChapterUnavailable {
                        path: path.to_path_buf(),
                        index,
                        at: entry.clone(),
                    })
            }
            // 单文件容器：provider 重读一遍再取第 N 章 —— 只有它知道怎么分章。
            ChapterLocator::Section(_) => provider.chapter_source(path, &at).map_err(|err| {
                let _ = err;
                crate::Error::ChapterUnavailable {
                    path: path.to_path_buf(),
                    index,
                    at: format!("{at:?}"),
                }
            }),
            ChapterLocator::Inline(text) => Ok(text.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    //! 门面：一个容器文件走完「选 provider → 读 → 统一接口」，以及可重排/定版式
    //! 两条路在 trait 上的表现差别。

    use std::io::Write as _;
    use std::path::Path;

    use crate::content::document::{BookDocument, ContentRef, Document, LocatorKind, metadata};
    use crate::content::format::{ChapterLocator, Content, ContentKind, Format};

    /// 把「正文在手上」的内容变成**库里那一份**：每章只留位置。
    ///
    /// 落库走的是 `Store::save_flow_in`，这里只复现它的那一句 `locator_for`
    /// —— 测试不想为了一个形状去拉一份 SQLite。
    fn at_locators(
        flow: &crate::content::format::FlowDoc,
        format: crate::content::format::Format,
    ) -> crate::content::format::FlowDoc {
        let chapters = flow
            .chapters
            .iter()
            .enumerate()
            .map(|(ix, ch)| crate::content::format::Chapter {
                title: ch.title.clone(),
                spine_href: ch.spine_href.clone(),
                // 字数不跟着位置走掉：它是索引那一趟的产物，落库之后仍要读得回来。
                char_len: ch.char_len(),
                text: crate::content::format::ChapterText::At(crate::content::format::locator_for(
                    format, ch, ix,
                )),
            })
            .collect();
        crate::content::format::FlowDoc {
            chapters,
            ..flow.clone()
        }
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([1, 2, 3, 255]));
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    fn cbz(dir: &Path, pages: &[(&str, u32, u32)]) -> std::path::PathBuf {
        let path = dir.join("comic.cbz");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, w, h) in pages {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(&png(*w, *h)).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn a_reflow_container_reads_as_a_flow_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "a.md",
            "# 标题\n\n正文第一段。\n\n# 第二章\n\n第二段。\n".as_bytes(),
        );
        let book = BookDocument::open(&path).unwrap();
        assert_eq!(book.format(), Format::Markdown);
        assert_eq!(book.kind(), ContentKind::Reflow);
        assert_eq!(book.locator_kind(), LocatorKind::Char);
        assert!(!book.is_fixed());
        assert!(book.fixed().is_none());
        assert!(book.page_count().is_none(), "可重排没有页数这个事实");
        let ContentRef::Reflow(flow) = book.content() else {
            panic!("该读成可重排");
        };
        assert!(!flow.chapters.is_empty());
    }

    /// **正文出库之后的主要回路**：库里的章表只有位置，正文要能按位置从容器里回来。
    ///
    /// 这条链子就是打开一本书真正走的那一条：`document::open`（导入时）→ 落库 →
    /// `with_container`（开书时）→ `chapter_source`。中间任何一环把位置算错，
    /// 读者看到的就是「翻到第二章是空的」。
    #[test]
    fn a_lazily_loaded_chapter_comes_back_from_the_container() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "a.md",
            "# 标题\n\n正文第一段。\n\n# 第二章\n\n第二段。\n".as_bytes(),
        );
        // 导入时：读到内容，把正文换成位置。
        let opened = BookDocument::open(&path).unwrap();
        let (meta, content) = (
            opened.meta().clone(),
            match opened.content() {
                ContentRef::Reflow(flow) => flow.clone(),
                ContentRef::Fixed(_) => panic!("该读成可重排"),
            },
        );
        // `open` 给的是索引：每一章有标题与位置，正文按位置现取。
        let full: Vec<String> = (0..content.chapters.len())
            .map(|ix| opened.chapter_source(ix).unwrap())
            .collect();
        assert!(content.chapters.iter().all(|c| c.locator().is_some()));

        // 开书时的那一份：只有位置 + 容器（`save_flow_in` 存出来的就是这个形状）。
        let stored = at_locators(&content, meta.format);
        assert!(
            stored
                .chapters
                .iter()
                .all(|c| matches!(c.locator(), Some(ChapterLocator::Section(_)))),
            "单文件格式该按序号存位置"
        );
        let book = BookDocument::with_container(meta, Content::Reflow(stored), &path);
        for (ix, want) in full.iter().enumerate() {
            assert_eq!(&book.chapter_source(ix).unwrap(), want, "第 {ix} 章");
        }
        assert!(book.chapter_source(999).unwrap().is_empty(), "越界给空");
    }

    /// EPUB 走的是**另一条**取章路：按容器条目名解一个 zip 条目（`Entry`），
    /// 而不是重读整本。这条拿仓库里那本合成 EPUB 跑完整条链。
    #[test]
    fn an_epub_chapter_comes_back_from_its_zip_entry() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/minimal-zh.epub");
        let opened = BookDocument::open(&path).unwrap();
        let (meta, content) = (
            opened.meta().clone(),
            match opened.content() {
                ContentRef::Reflow(flow) => flow.clone(),
                ContentRef::Fixed(_) => panic!("合成 EPUB 该读成可重排"),
            },
        );
        let full: Vec<String> = (0..content.chapters.len())
            .map(|ix| opened.chapter_source(ix).unwrap())
            .collect();
        // 开书时的那一份：只剩位置与容器。
        let stored = at_locators(&content, meta.format);
        assert!(
            stored
                .chapters
                .iter()
                .all(|c| matches!(c.locator(), Some(ChapterLocator::Entry(_)))),
            "EPUB 该按条目名存位置"
        );
        let book = BookDocument::with_container(meta, Content::Reflow(stored), &path);
        assert!(!full.is_empty());
        for (ix, want) in full.iter().enumerate() {
            assert_eq!(&book.chapter_source(ix).unwrap(), want, "第 {ix} 章");
        }
    }

    /// 容器不在了：要报 **`ChapterUnavailable`**，不是给一页空白。
    ///
    /// 这个区别决定界面说「这一章是空的」还是「源文件不在了」——
    /// 后者要引导读者重新导入或重新定位，不能静悄悄地画一页白。
    #[test]
    fn a_missing_container_is_an_error_not_an_empty_chapter() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "a.md", "# 一\n\n正文。\n".as_bytes());
        let opened = BookDocument::open(&path).unwrap();
        let (meta, content) = (
            opened.meta().clone(),
            match opened.content() {
                ContentRef::Reflow(flow) => flow.clone(),
                ContentRef::Fixed(_) => panic!("该读成可重排"),
            },
        );
        let content = at_locators(&content, meta.format);
        std::fs::remove_file(&path).unwrap();

        let book = BookDocument::with_container(meta, Content::Reflow(content), &path);
        assert!(book.chapter_source(0).is_err(), "文件没了不该当空章");
    }

    /// 样式表与正文同一立场：索引只给**位置清单**，打开时按清单回容器现取、
    /// 按文档序拼接。夹具里有两份 CSS，正好钉住「作者样式不是单文件」的形态 ——
    /// 库里读回的那一份（只有清单）要与全文路径拼出同一份文本。
    #[test]
    fn a_stylesheet_comes_back_from_its_container_by_href_list() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/minimal-zh.epub");
        let opened = BookDocument::open(&path).unwrap();
        let ContentRef::Reflow(flow) = opened.content() else {
            panic!("合成 EPUB 该读成可重排");
        };
        assert!(
            flow.css.is_empty(),
            "索引不该带样式文本，实际 {} 字节",
            flow.css.len()
        );
        assert_eq!(
            flow.css_hrefs.len(),
            2,
            "manifest 里的两份 CSS 都该在清单上：{:?}",
            flow.css_hrefs
        );
        let from_index = opened.stylesheet();
        assert!(!from_index.is_empty(), "两份样式都要从容器里取到");

        // 开书的那一份：库里读回来只有清单（save_flow_in 存出来的形状）。
        let (meta, content) = (opened.meta().clone(), flow.clone());
        let book = BookDocument::with_container(meta, Content::Reflow(content), &path);
        assert_eq!(
            book.stylesheet(),
            from_index,
            "按清单现取要与索引拼出同一份样式"
        );
    }

    /// 容器不在了：清单在手上也取不到样式 —— 回空而不是报错，与图片缺失同一表现。
    #[test]
    fn a_missing_container_leaves_the_stylesheet_empty() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/minimal-zh.epub");
        let opened = BookDocument::open(&path).unwrap();
        let (meta, content) = (
            opened.meta().clone(),
            match opened.content() {
                ContentRef::Reflow(flow) => flow.clone(),
                ContentRef::Fixed(_) => panic!("该读成可重排"),
            },
        );
        let book = BookDocument::with_container(
            meta,
            Content::Reflow(content),
            Path::new("/gone/missing.epub"),
        );
        assert_eq!(book.stylesheet(), "", "容器丢了就是没有作者样式，正文照排");
    }

    /// 定版式走的是同一个 trait，但坐标、页数、`ContentRef` 都换了一套 ——
    /// 这一条正是「统一接口 + 两种内容」成立与否的判据。
    #[test]
    fn a_fixed_container_reads_as_a_page_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = cbz(dir.path(), &[("1.png", 100, 200), ("2.png", 100, 200)]);
        let book = BookDocument::open(&path).unwrap();
        assert_eq!(book.format(), Format::Cbz);
        assert_eq!(book.kind(), ContentKind::Fixed);
        assert_eq!(book.locator_kind(), LocatorKind::Page);
        assert!(book.is_fixed());
        assert_eq!(book.page_count(), Some(2));
        assert!(book.flow().is_none());
        let ContentRef::Fixed(fixed) = book.content() else {
            panic!("该读成定版式");
        };
        assert_eq!(fixed.pages[0].image_href(), Some("1.png"));
        // 字节仍然按 href 现取。
        assert_eq!(book.image_bytes("1.png").unwrap(), png(100, 200));
        assert!(book.image_bytes("nope.png").is_none());
    }

    #[test]
    fn metadata_does_not_need_the_body_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "a.md", "# 书名\n\n正文。\n".as_bytes());
        let meta = metadata(&path).unwrap();
        assert_eq!(meta.title, "书名");
        assert_eq!(meta.format, Format::Markdown);
    }

    #[test]
    fn an_unknown_container_is_reported_not_opened() {
        let Err(err) = BookDocument::open(Path::new("/x/y.mobi")) else {
            panic!("没注册的容器不该打开");
        };
        assert!(matches!(err, crate::Error::UnsupportedFormat(_)), "{err}");
    }
}
