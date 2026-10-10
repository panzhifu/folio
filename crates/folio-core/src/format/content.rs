//! 文档内容的**两种家族**：可重排与定版式。
//!
//! 这是 KOReader `document.info.has_pages` 那一刀在 Folio 的对应物：格式解析的差别
//! 到 [`Content`] 为止，往上（[`crate::document::Document`]）只有这两族。
//!
//! - [`FlowDoc`]（可重排）：行由排版引擎按视口算出，页是结果不是事实。
//!   EPUB / FB2 / HTML / Markdown / TXT 都归它。
//! - [`FixedDoc`]（定版式）：页是既定的，缩放与裁剪只是显示变换。
//!   CBZ 归它；将来的 PDF / DJVU 也是这一族。

use serde::{Deserialize, Serialize};

use super::{Chapter, ImageRes, TocEntry};

/// 可重排文档。`css` 是全书作者样式按 manifest 顺序拼起来的一份，
/// `toc` 与 `cover` 来自 OPF；这三项都是**文档级**数据，所以和章节分开存。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlowDoc {
    pub chapters: Vec<Chapter>,
    pub css: String,
    pub toc: Vec<TocEntry>,
    pub cover: Option<Vec<u8>>,
    /// 全书图片，按容器路径索引。封面若也在 manifest 里会一并收录。
    pub images: Vec<ImageRes>,
}

impl FlowDoc {
    pub fn char_len(&self) -> usize {
        self.chapters.iter().map(Chapter::char_len).sum()
    }
}

/// 定版式文档。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FixedDoc {
    /// 阅读顺序的页表。
    pub pages: Vec<FixedPage>,
    pub toc: Vec<TocEntry>,
    pub cover: Option<Vec<u8>>,
    /// 全书图片资源。整页漫画也在其中 —— 与 [`FlowDoc::images`] 同义。
    pub images: Vec<ImageRes>,
    /// 首屏默认画布尺寸。单页各自的尺寸以 [`FixedPage`] 为准：
    /// 漫画包里的页面大小不一定一致，按整本一个数缩放会有的页被切掉。
    pub width: u32,
    pub height: u32,
}

impl FixedDoc {
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

/// 定版式文档的一页。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixedPage {
    pub source: PageSource,
    /// 这一页的原始像素尺寸；`0` 表示不知道，绘制侧按容器宽度兜底。
    pub width: u32,
    pub height: u32,
}

impl FixedPage {
    /// 这一页要画的图在容器里的路径；渲染型（PDF）没有，返回 `None`。
    pub fn image_href(&self) -> Option<&str> {
        match &self.source {
            PageSource::Image { href } => Some(href),
            PageSource::Render { .. } => None,
        }
    }
}

/// 一页的内容从哪来。
///
/// `serde` 的 tag 写在里面：这一列要落进 SQLite，形状变了要能看出来是哪一版写的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PageSource {
    /// 一张图：字节按 `href` 去容器里现取（见 [`crate::document::Document::image_bytes`]）。
    Image { href: String },
    /// 交给栅格化引擎的一页（PDF / DJVU）。一期还没有 provider 产出它，
    /// 留着是因为「定版式」这一族迟早要装下它，而那时不该再改一次磁盘形状。
    Render { index: u32 },
}

/// 文档内容。格式差异到此为止。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Reflow(FlowDoc),
    Fixed(FixedDoc),
}

/// 内容家族。与 [`Content`] 的两个分支一一对应，给界面做分流的判据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Reflow,
    Fixed,
}

impl Content {
    pub fn kind(&self) -> ContentKind {
        match self {
            Content::Reflow(_) => ContentKind::Reflow,
            Content::Fixed(_) => ContentKind::Fixed,
        }
    }

    pub fn as_flow(&self) -> Option<&FlowDoc> {
        match self {
            Content::Reflow(flow) => Some(flow),
            Content::Fixed(_) => None,
        }
    }

    pub fn as_fixed(&self) -> Option<&FixedDoc> {
        match self {
            Content::Fixed(fixed) => Some(fixed),
            Content::Reflow(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    //! 内容家族的判据与页表取值，以及 `FixedPage` 落 JSON 的往返。

    use crate::format::content::{Content, ContentKind, FixedDoc, FixedPage, PageSource};

    fn page(href: &str) -> FixedPage {
        FixedPage {
            source: PageSource::Image {
                href: href.to_string(),
            },
            width: 800,
            height: 1200,
        }
    }

    #[test]
    fn content_kind_follows_the_branch() {
        let flow = Content::Reflow(Default::default());
        assert_eq!(flow.kind(), ContentKind::Reflow);
        assert!(flow.as_flow().is_some());
        assert!(flow.as_fixed().is_none());

        let fixed = Content::Fixed(FixedDoc {
            pages: vec![page("1.jpg")],
            ..Default::default()
        });
        assert_eq!(fixed.kind(), ContentKind::Fixed);
        assert!(fixed.as_fixed().is_some());
        assert_eq!(fixed.as_fixed().unwrap().page_count(), 1);
    }

    #[test]
    fn a_page_knows_its_image_href() {
        assert_eq!(page("p/2.png").image_href(), Some("p/2.png"));
        let render = FixedPage {
            source: PageSource::Render { index: 7 },
            width: 0,
            height: 0,
        };
        assert_eq!(render.image_href(), None);
    }

    /// 页表要能原样落进 SQLite 那一列再读回来（store 的 v7 依赖这一条）。
    #[test]
    fn pages_round_trip_through_json() {
        let pages = vec![
            page("a.jpg"),
            FixedPage {
                source: PageSource::Render { index: 3 },
                width: 1024,
                height: 768,
            },
        ];
        let json = serde_json::to_string(&pages).unwrap();
        let back: Vec<FixedPage> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, pages);
        assert!(json.contains("\"kind\":\"image\""), "{json}");
        assert!(json.contains("\"kind\":\"render\""), "{json}");
    }
}
