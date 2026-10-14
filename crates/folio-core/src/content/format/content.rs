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
    /// 整本的翻页方向：`true` = 从右往左翻（阿拉伯语、希伯来语、日文竖排那类）。
    ///
    /// 来源只有 EPUB 的 `<spine page-progression-direction="rtl">` —— 它说的是**翻书**，
    /// 与文字往哪边流（CSS 的 `direction`，来自 `dir` 属性）是两件事：前者管读者的手
    /// （点屏幕哪一侧是「下一页」、箭头按钮按下去往哪走），后者管排版。两件事各有各的
    /// 缺省，所以这里单独一格，不许从 `direction` 推。
    ///
    /// **`default` 与缺席都折成 `false`**，这是已知的一条限制：规范要求那种情况下由
    /// 「内容自己的方向」定，而我们不在开书时回头去嗅第一章的首强字符（开书只读索引）。
    /// 后果是一本**没写这个属性**的 RTL 书：正文照常右起（`dir="rtl"` 那条路是通的），
    /// 点按区却还按左翻书 —— 读者能立刻感觉到，而它比「正文也读反」轻得多。
    /// 属性显式写了 `rtl` 的书不受影响。
    pub progression_rtl: bool,
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
    /// 这一页**其余**的图（多图页 / 对开跨页）。主图在 [`PageSource`] 里，这里是附加的。
    ///
    /// `#[serde(default)]`：老库的 `fixed_json` 没有这一项，解出来是空 —— 表现就是
    /// 「单图页」，与它被加进来之前逐字节一致，不需要整库重扫。
    #[serde(default)]
    pub extra_images: Vec<PageImage>,
}

/// 定版式一页上的一张图（非主图）。尺寸为原始像素；`0` 表示不知道。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageImage {
    pub href: String,
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

    /// 这一页的**全部**图（主图在前、附加图在后）。多图页 / 跨页用它。
    pub fn image_hrefs(&self) -> Vec<&str> {
        let mut out = Vec::with_capacity(1 + self.extra_images.len());
        if let PageSource::Image { href } = &self.source {
            out.push(href.as_str());
        }
        out.extend(self.extra_images.iter().map(|image| image.href.as_str()));
        out
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

    use crate::content::format::content::{
        Content, ContentKind, FixedDoc, FixedPage, PageImage, PageSource,
    };

    fn page(href: &str) -> FixedPage {
        FixedPage {
            source: PageSource::Image {
                href: href.to_string(),
            },
            width: 800,
            height: 1200,
            extra_images: Vec::new(),
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
            extra_images: Vec::new(),
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
                extra_images: vec![PageImage {
                    href: "b.jpg".into(),
                    width: 512,
                    height: 768,
                }],
            },
        ];
        let json = serde_json::to_string(&pages).unwrap();
        let back: Vec<FixedPage> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, pages);
        assert!(json.contains("\"kind\":\"image\""), "{json}");
        assert!(json.contains("\"kind\":\"render\""), "{json}");
    }

    /// 附加图那一列要能原样回来，并且**主图在前**（`image_hrefs` 的顺序就是绘制的顺序）。
    #[test]
    fn a_multi_image_page_keeps_every_image_in_order() {
        let page = FixedPage {
            source: PageSource::Image {
                href: "left.jpg".into(),
            },
            width: 1600,
            height: 1200,
            extra_images: vec![PageImage {
                href: "right.jpg".into(),
                width: 800,
                height: 1200,
            }],
        };
        assert_eq!(page.image_href(), Some("left.jpg"));
        assert_eq!(page.image_hrefs(), vec!["left.jpg", "right.jpg"]);

        let json = serde_json::to_string(&[page.clone()]).unwrap();
        let back: Vec<FixedPage> = serde_json::from_str(&json).unwrap();
        assert_eq!(back[0].image_hrefs(), vec!["left.jpg", "right.jpg"]);
    }

    /// `extra_images` 靠 `#[serde(default)]` 兼容老库：`fixed_json` 里没有这一项的那一版
    /// 写下的行，读回来必须是一页「单图」，而不是解不开整本 —— 这一条不成立就得整库重扫。
    #[test]
    fn a_page_written_before_the_extra_column_reads_back_as_single_image() {
        let legacy = r#"[{"source":{"kind":"image","href":"a.jpg"},"width":800,"height":1200}]"#;
        let back: Vec<FixedPage> = serde_json::from_str(legacy).unwrap();
        assert_eq!(back.len(), 1);
        assert!(
            back[0].extra_images.is_empty(),
            "老库那一版没有附加图，读回来就是单图"
        );
        assert_eq!(back[0].image_hrefs(), vec!["a.jpg"]);
    }
}
