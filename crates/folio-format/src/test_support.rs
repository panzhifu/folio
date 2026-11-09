//! 测试用的「文档门面」替身：provider 索引 + 按需取章 + 共享容器。
//!
//! 正式的门面在 `folio_core::content::document`；这一层只给格式自己的测试用，
//! 免得它们为了取一章正文把整个 core 拖回来。三件事与正式门面同形：
//! **索引不带正文**、**共享容器只开一次**、**`href` 的降级路走 `ImageRes::source`**。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::container::Container;
use crate::content::FlowDoc;
use crate::{Content, DocMeta, DocumentProvider, Error, FontBytes, FontRef, ImageRes, Result};

pub(crate) struct Doc {
    provider: Box<dyn DocumentProvider>,
    path: PathBuf,
    meta: DocMeta,
    content: Content,
    container: OnceLock<Container>,
}

impl Doc {
    pub(crate) fn try_open(path: &Path) -> Result<Self> {
        let provider = crate::provider_for(path)
            .ok_or_else(|| Error::UnsupportedFormat(path.display().to_string()))?;
        let (meta, content) = provider.index(path)?;
        Ok(Self {
            provider,
            path: path.to_path_buf(),
            meta,
            content,
            container: OnceLock::new(),
        })
    }

    pub(crate) fn open(path: &Path) -> Self {
        Self::try_open(path).unwrap_or_else(|err| panic!("{} 读不动：{err}", path.display()))
    }

    pub(crate) fn meta(&self) -> &DocMeta {
        &self.meta
    }

    pub(crate) fn content(&self) -> &Content {
        &self.content
    }

    pub(crate) fn flow(&self) -> Option<&FlowDoc> {
        match &self.content {
            Content::Reflow(flow) => Some(flow),
            Content::Fixed(_) => None,
        }
    }

    pub(crate) fn images(&self) -> &[ImageRes] {
        match &self.content {
            Content::Reflow(flow) => &flow.images,
            Content::Fixed(fixed) => &fixed.images,
        }
    }

    fn shared_container(&self) -> &Container {
        self.container.get_or_init(|| Container::new(&self.path))
    }

    /// 正文里那串 `src` 解出来的键 → 真正去读的条目名（降级路）。
    fn image_at<'a>(&'a self, href: &'a str) -> &'a str {
        self.images()
            .iter()
            .find(|img| img.href == href && img.source != img.href)
            .map_or(href, |img| img.source.as_str())
    }

    pub(crate) fn image_bytes(&self, href: &str) -> Option<Vec<u8>> {
        let at = self.image_at(href);
        if let Some(bytes) = self.shared_container().read(at) {
            return Some(bytes);
        }
        self.provider.image_bytes(&self.path, at)
    }

    pub(crate) fn render_page(&self, page: usize, edge: u32) -> Option<Vec<u8>> {
        self.provider.render_page(&self.path, page, edge)
    }

    pub(crate) fn chapter_source(&self, index: usize) -> Result<String> {
        let flow = self.flow().ok_or_else(|| Error::ChapterUnavailable {
            path: self.path.clone(),
            index,
            at: "定版式没有章".to_string(),
        })?;
        let chapter = flow
            .chapters
            .get(index)
            .ok_or_else(|| Error::ChapterUnavailable {
                path: self.path.clone(),
                index,
                at: format!("只有 {} 章", flow.chapters.len()),
            })?;
        match chapter.locator() {
            Some(at) => self.provider.chapter_source(&self.path, at),
            None => Ok(chapter.source().to_string()),
        }
    }

    pub(crate) fn stylesheet(&self) -> String {
        let Some(flow) = self.flow() else {
            return String::new();
        };
        if !flow.css.is_empty() {
            return flow.css.clone();
        }
        let mut css = String::new();
        for href in &flow.css_hrefs {
            let text = self
                .shared_container()
                .read_text(href)
                .or_else(|| self.provider.entry_text(&self.path, href));
            let Some(text) = text else {
                continue;
            };
            css.push_str(&crate::absolutize_css_urls(&text, &crate::dir_of(href)));
            css.push('\n');
        }
        css
    }

    pub(crate) fn font_charset(&self) -> &[u32] {
        self.flow().map_or(&[], |flow| flow.font_charset.as_slice())
    }

    pub(crate) fn fonts(&self, wanted: &[String]) -> Vec<FontBytes> {
        self.provider.fonts(&self.path, wanted)
    }

    pub(crate) fn font_refs(&self, wanted: &[String]) -> Option<Vec<FontRef>> {
        self.provider.font_refs(&self.path, wanted)
    }

    pub(crate) fn font_refs_in(&self, wanted: &[String]) -> Option<Vec<FontRef>> {
        self.provider
            .font_refs_in(&self.path, self.shared_container(), wanted)
    }
}
