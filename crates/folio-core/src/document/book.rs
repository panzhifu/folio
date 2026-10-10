//! provider 解析出来的那本书 —— [`Document`] 的通用实现。
//!
//! 只有这一个实现：格式差异已经被 provider 抹平成 [`Content`]，这里不再分格式。
//! 将来若要造一份「不是从文件来的」文档（测试替身、内置示例），也实现同一个 trait。

use std::path::{Path, PathBuf};

use super::{ContentRef, Document, unsupported};
use crate::Result;
use crate::format::{self, Content, ContentKind, DocMeta, DocumentProvider, FontBytes};

pub struct BookDocument {
    /// 留住 provider：图片与字体的字节要按 href 现取，只有它知道容器怎么开。
    ///
    /// `None` = **没有容器可谈**：内容来自书库或测试替身（[`BookDocument::from_content`]）。
    /// 那时取字节一律回空 —— 与「容器丢了」是同一个表现（那块地方空着，正文照排）。
    provider: Option<Box<dyn DocumentProvider>>,
    path: Option<PathBuf>,
    meta: DocMeta,
    content: Content,
}

impl BookDocument {
    /// 从文件读一本：选 provider → 读成 [`Content`] → 校验结构完整性。
    pub fn open(path: &Path) -> Result<Self> {
        let provider = format::provider_for(path).ok_or_else(|| unsupported(path))?;
        tracing::debug!(
            "document: {} 交给 {} provider",
            path.display(),
            provider.format().as_str()
        );
        let (meta, content) = provider.read(path)?;
        // 校验只写日志，不阻塞阅读。
        crate::validate::check(&content, path);
        Ok(Self {
            provider: Some(provider),
            path: Some(path.to_path_buf()),
            meta,
            content,
        })
    }

    /// 已经有内容、没有容器。书库读回的书、测试替身、将来的内置示例走这一条：
    /// 正文齐全，但 `image_bytes` / `fonts` 回空。
    pub fn from_content(meta: DocMeta, content: Content) -> Self {
        // 校验只写日志，不阻塞阅读。路径用书名占位（没有文件）。
        crate::validate::check(&content, Path::new(&meta.title));
        Self {
            provider: None,
            path: None,
            meta,
            content,
        }
    }

    /// 内容来自书库、字节仍从**原文件**现取。
    ///
    /// 这是「按主键打开」那条路：章文本早在落库时就进了 SQLite，重解一遍 ZIP 只为拿
    /// 文本是白付；但图片与内嵌字体刻意不入库，得回到容器里读。
    ///
    /// 文件不在了**不是错误**：`provider_for` 只按扩展名认领，真正的读取失败会在
    /// 取字节时回空 —— 表现就是没有插图、没有补缺字的那一份，正文照排。
    pub fn with_container(meta: DocMeta, content: Content, path: &Path) -> Self {
        Self {
            provider: format::provider_for(path),
            path: Some(path.to_path_buf()),
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

    fn image_bytes(&self, href: &str) -> Option<Vec<u8>> {
        let (provider, path) = self.container()?;
        provider.image_bytes(path, href)
    }

    fn fonts(&self, wanted: &[String]) -> Vec<FontBytes> {
        match self.container() {
            Some((provider, path)) => provider.fonts(path, wanted),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    //! 门面：一个容器文件走完「选 provider → 读 → 统一接口」，以及可重排/定版式
    //! 两条路在 trait 上的表现差别。

    use std::io::Write as _;
    use std::path::Path;

    use crate::document::{BookDocument, ContentRef, Document, LocatorKind, metadata};
    use crate::format::{ContentKind, Format};

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
