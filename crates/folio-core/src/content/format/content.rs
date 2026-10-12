//! 文档内容的**两种家族**：可重排与定版式。
//!
//! 这是 KOReader `document.info.has_pages` 那一刀在 Folio 的对应物：格式解析的差别
//! 到 [`Content`] 为止，往上（[`crate::content::document::Document`]）只有这两族。
//!
//! - [`FlowDoc`]（可重排）：行由排版引擎按视口算出，页是结果不是事实。
//!   EPUB / FB2 / HTML / Markdown / TXT 都归它。
//! - [`FixedDoc`]（定版式）：页是既定的，缩放与裁剪只是显示变换。
//!   CBZ 归它；将来的 PDF / DJVU 也是这一族。

use super::{Chapter, ImageRes, TocEntry};
use serde::{Deserialize, Serialize};

/// 可重排文档。`toc` 与 `cover` 来自 OPF；这两项是**文档级**数据，所以和章节分开存。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FlowDoc {
    pub chapters: Vec<Chapter>,
    /// 拼接好的作者样式表。**索引路径下为空**：正文出库之后 CSS 同样不入库，
    /// 存的是 [`Self::css_hrefs`]，打开时按清单回容器现取（见
    /// `crate::content::document::Document::stylesheet`）。全文路径（`read`）与
    /// 「文本已在手上」的场合（`from_content`、旧库兜底）才直接带文本。
    pub css: String,
    /// 作者样式表在**容器里**的位置清单（容器绝对路径，按文档序 —— 顺序即层叠
    /// 顺序）。索引路径填这里；单文件格式没有独立样式表，为空。
    pub css_hrefs: Vec<String>,
    pub toc: Vec<TocEntry>,
    pub cover: Option<Vec<u8>>,
    /// 全书图片，按容器路径索引。封面若也在 manifest 里会一并收录。
    pub images: Vec<ImageRes>,
    /// 封面**落盘之后**的描述：`covers/<hash>.<ext>` 那两个格。
    ///
    /// `None` 有两种来路，读那一侧都该退回 `cover` 那个 BLOB：
    /// 这本没有封面，或者有封面但没写进 `covers/`（磁盘满、没权限、还没接线）。
    /// 由导入管线在落盘成功之后填上 —— provider 只交字节，不知道书库在哪儿。
    /// （`FlowDoc` 不是 serde 类型：它一格一格映射到 SQL 列，见 `save_flow_in`。）
    pub cover_ref: Option<crate::content::format::cover::CoverRef>,
}

impl FlowDoc {
    /// 全书**规范化可见字数** —— 全局字符进度的分母（`plans/Locator.md` 第二节）。
    ///
    /// 只要有一章的字数不知道，整本就回 `None`：**不把未知折成 0**。
    /// 折了会让分母偏小、百分比偏大，而症状是「读到一半进度条已经满了」——
    /// 比不显示百分比更糟，因为它是个安静的错数。
    ///
    /// 老库升上来的行（v14 之前）与只读了清单的 provider 都会走到 `None`；
    /// 前者的处置是让下一次 `reindex` 把它填上。
    pub fn char_len(&self) -> Option<usize> {
        let mut total = 0usize;
        for chapter in &self.chapters {
            total = total.checked_add(chapter.char_len()?)?;
        }
        Some(total)
    }

    /// 已知道字数的那几章之和 + 还有几章不知道 —— 进度条在「部分章没算过」时
    /// 用的降级读数。第二项非零就说明这个百分比是**下界**。
    pub fn known_char_len(&self) -> (usize, usize) {
        let mut total = 0usize;
        let mut unknown = 0usize;
        for chapter in &self.chapters {
            match chapter.char_len() {
                Some(n) => total = total.saturating_add(n),
                None => unknown += 1,
            }
        }
        (total, unknown)
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
    /// 一张图：字节按 `href` 去容器里现取（见 [`crate::content::document::Document::image_bytes`]）。
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

    use crate::content::format::content::{Content, ContentKind, FixedDoc, FixedPage, PageSource};

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
