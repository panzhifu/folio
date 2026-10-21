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
    /// 封面**落盘之后**的描述（`covers/<hash>.<ext>` 那两个格）。
    ///
    /// 与 [`FlowDoc::cover_ref`] 同一件事、同一来源：由导入管线在写文件成功之后填，
    /// provider 只交字节。定版式以前没有这一格，于是 `land_cover` 把整族挡在门外，
    /// **首页那张图算完就丢**（漫画一直是这样，PDF 接上封面之后也会一样）——
    /// 现在这一族与可重排走同一条落盘路，界面不再问格式。
    pub cover_ref: Option<crate::content::format::cover::CoverRef>,
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
    /// 交给栅格化引擎的一页（PDF / AZW4 / DjVu）：**一页是算出来的一张图**，
    /// 所以页表里没有容器路径可存，只有引擎那一侧的页号。取像素走
    /// [`crate::content::format::DocumentProvider::render_page`]，
    /// 画多大由调用方定档（见 `format::pdf::RENDER_EDGE`）。
    Render { index: u32 },
}

/// 页内的一个盒。**原点在这一页可见框的左上角、y 朝下**；单位是**这一页自己的那一套**
/// （PDF 是 pt，DjVu 是它的原生像素）—— 消费方要的从来是「占这一页的多大比例」，
/// 而那由 [`PageText::width`] / [`PageText::height`] 与页图表幅的比给，不必知道是哪一套。
///
/// 不复用 `content::hit::SelectionRect`（那是排版坐标里的选中盒），也不把栅格引擎自己的
/// `Rect` 递到这一层：格式差异到 [`Content`] 为止，界面不该 import 某个引擎。
///
/// **y 在这里就已经翻过一次**：实测两个引擎的盒子都是 y 朝上 —— PDF 那一族连单位都不是
/// 像素而是 pt（基线在 `bbox.y`，`height` 恰等于字号、往 y 增方向长；拿 Ghostscript 写的
/// 已知位置量出来的），DjVu 的文字层在 remap 之前同样朝上。而栅格出来的页图与屏幕都是 y
/// 朝下。翻这一刀只该有一处做：两处各翻一次就会有一次翻反，症状是色块画在字的上下对称位
/// 置 —— 屏上看得出偏，却说不清为什么偏。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PageRect {
    /// 点在不在这一格里。边界算在里（与排版那一侧同一口径：点在字框的右边缘上，
    /// 仍然该选中这个字而不是隔壁那个）。
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }

    /// 点到这个盒的最短距离的平方。不在任何盒里时用它找最近的那个字
    /// （点在行空的页边距里、或在两张图之间，都该收到最近的一格而不是判「没命中」）。
    pub fn distance_sq(&self, x: f32, y: f32) -> f32 {
        let dx = (x - self.x).max(self.x + self.width - x).max(0.0);
        let dy = (y - self.y).max(self.y + self.height - y).max(0.0);
        dx * dx + dy * dy
    }

    /// 两格并成一格（选中按行出色块时，一行里的字并成一条）。
    pub fn union(&self, other: &Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            width: (self.x + self.width).max(other.x + other.width) - x,
            height: (self.y + self.height).max(other.y + other.height) - y,
        }
    }
}

/// 文字层里的一个可见字符。
///
/// **下标就是页内偏移**：`TextAnchor.start/end` 与 `Locator.offset` 在定版式上数的就是
/// 这个序，而它只数字、不数换行（一整页是一个扁平的序，跨行不塞哨兵 —— 要换行是
/// [`PageText::slice`] 那一头的事）。摊平的序就是阅读序：PDF 那一族 `extract_text_lines`
/// 实测按 y 从上到下排好，本层不再自己排。
///
/// `line` 挂在字上而不是并成一张「行表」：出色块要按行并、收边要不跨行，两处问的都是
/// 同一批字。两张平行数组靠约定等长，与「一份事实抄两处」是同一个坑。
///
/// 引擎给的**词编号不在这里**（PDF 那一族实测给得出）：收边按字符类别走
/// [`crate::content::hit::CharClass`]，与排版那一侧同一份判据 —— 那个编号本身不可信
/// （一份中文 PDF 里最长的一条「词」是 46 个字的一整句，而一个英文手机号被切成四「词」）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageChar {
    pub ch: char,
    pub rect: PageRect,
    /// 第几行（页内从 0 起，阅读序）。
    pub line: u32,
}

/// 一页的文字层。
///
/// **不入库、按页现取**：实测每页 0.3 ms（疏页）到 10 ms（dev profile 的密页，release
/// 更低），与「字节不入库」「章文不入库」同一立场 —— 为一次选中付一整本书的文字层是
/// 白付，而存起来就要为它开一列、跟着 `INDEX_VERSION` 走，栅格引擎升一版就得整库重扫。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PageText {
    /// 这一页**可见框**的尺寸，单位与 [`PageRect`] 的坐标同一套。[`PageRect`] 就是相对它的
    /// 左上角，而栅格出来的图是同一个框、只差一个比例 —— 绘制侧要的那把尺就是
    /// `这一格的宽 / 页表里那对像素的宽`，所以这一格是**唯一**要说清单位在哪里的地方。
    pub width: f32,
    pub height: f32,
    pub chars: Vec<PageChar>,
}

impl PageText {
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn char_count(&self) -> usize {
        self.chars.len()
    }

    /// 夹到 `0..=len`。选中区间是从两处命中拼出来的，越界比 panic 便宜。
    fn clamp(&self, index: usize) -> usize {
        index.min(self.chars.len())
    }

    /// 一段偏移的原文。**跨行处插一个换行**：复制出去要还是那一行一行的样子，
    /// 而批注的原文快照也存这一串（与排版那一侧同一条口径：快照带断行才认得出诗）。
    pub fn slice(&self, range: std::ops::Range<usize>) -> String {
        let start = self.clamp(range.start);
        let end = self.clamp(range.end).max(start);
        let mut out = String::new();
        let mut line: Option<u32> = None;
        for item in &self.chars[start..end] {
            if line.is_some_and(|previous| previous != item.line) {
                out.push('\n');
            }
            line = Some(item.line);
            out.push(item.ch);
        }
        out
    }

    /// 整页原文（不插换行，纯字序）。偏移空间就是它，所以 [`Self::slice`] 与它对得上。
    pub fn text(&self) -> String {
        self.chars.iter().map(|item| item.ch).collect()
    }

    /// 点落在哪一格 → 插入点偏移（`0..=len`）。
    ///
    /// 两条规则照抄排版那一侧的 `content::hit::hit_test`（同一条观感，两个引擎不该各有一套）：
    /// 落在字框里时按**前进量的中点**判这一格之前还是之后（点在字的右半边算下一个字之前，
    /// 与文本编辑器一致）；不在任何字框里时收到**最近的那个字**，页边距与图之间的空白
    /// 都该给一个插入点，而不是判「没命中」。
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        let mut inside: Option<(usize, f32)> = None;
        let mut nearest: Option<(usize, f32)> = None;
        for (index, item) in self.chars.iter().enumerate() {
            if item.rect.contains(x, y) {
                // 同一行里叠着的盒（着重号、上下标）取**中点离得最近**的那一个。
                let mid = item.rect.x + item.rect.width / 2.0;
                let gap = (x - mid).abs();
                if inside.is_none_or(|(_, best_gap)| gap < best_gap) {
                    inside = Some((index, gap));
                }
                continue;
            }
            let distance = item.rect.distance_sq(x, y);
            if nearest.is_none_or(|(_, best_distance)| distance < best_distance) {
                nearest = Some((index, distance));
            }
        }
        if let Some((index, _)) = inside {
            let rect = self.chars[index].rect;
            let after = x >= rect.x + rect.width / 2.0;
            return Some(if after { index + 1 } else { index });
        }
        let (index, _) = nearest?;
        let rect = self.chars[index].rect;
        // 收边：点在字框右边或下边之后 → 插在这一格之后，否则之前。
        let after = x > rect.x + rect.width || y > rect.y + rect.height;
        Some(if after { index + 1 } else { index })
    }

    /// 从 `index` 那一格向外收成一个选中单元（**收边的判据与排版那一侧同一份**，
    /// 见 [`crate::content::hit::snap_word`]：拉丁词字符连成一段、空白连成一段，
    /// 中文 / 标点 / 换行单字自为界）。
    ///
    /// 不用引擎给的词编号：一份中文 PDF 里最长的一条「词」实测是 46 个字的一整句，
    /// 按它吸附等于**点一下选中一整句**，而同一趟里一个英文手机号被切成四「词」。
    /// 收边不跨行 —— 跨行的色块会画到下一行去，而那在一页纸上没有对应的字。
    pub fn snap(&self, index: usize) -> std::ops::Range<usize> {
        let total = self.chars.len();
        if total == 0 {
            return 0..0;
        }
        // 越界夹进来：拖到页外再长按不该 panic（与 `snap_word` 同一条）。
        let index = index.min(total - 1);
        let class = crate::content::hit::CharClass::of(self.chars[index].ch);
        if !class.is_connectable() {
            return index..index + 1;
        }
        let line = self.chars[index].line;
        let same = |at: usize| {
            self.chars[at].line == line
                && crate::content::hit::CharClass::of(self.chars[at].ch) == class
        };
        let mut start = index;
        while start > 0 && same(start - 1) {
            start -= 1;
        }
        let mut end = index + 1;
        while end < total && same(end) {
            end += 1;
        }
        start..end
    }

    /// 一段选中按行并成的色块（一帧画一次）。空行不出现，越界的夹掉。
    pub fn rects(&self, range: std::ops::Range<usize>) -> Vec<PageRect> {
        let start = self.clamp(range.start);
        let end = self.clamp(range.end).max(start);
        let mut out: Vec<(u32, PageRect)> = Vec::new();
        for item in &self.chars[start..end] {
            match out.last_mut() {
                // 同一行的字按阅读序连着来，所以「只看最后一格」就够，不用查表。
                Some((line, box_)) if *line == item.line => *box_ = box_.union(&item.rect),
                _ => out.push((item.line, item.rect)),
            }
        }
        out.into_iter().map(|(_, box_)| box_).collect()
    }
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
        Content, ContentKind, FixedDoc, FixedPage, PageChar, PageImage, PageRect, PageSource,
        PageText,
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

        let json = serde_json::to_string(std::slice::from_ref(&page)).unwrap();
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

    /// 手摆一页两行的文字层：第一行 `ab cd`（两个词，中间那个缝是一个空格格），
    /// 第二行四个汉字（**每词一字**，那是中文 PDF 里唯一可靠的收边粒度，理由见
    /// [`PageText::snap`] 那条实测）。
    fn two_lines() -> PageText {
        let mut chars: Vec<PageChar> = Vec::new();
        // 第 0 行 y=10，第 1 行 y=30；每格 10pt 宽。
        for (ch, line) in [
            ('a', 0u32),
            ('b', 0),
            (' ', 0),
            ('c', 0),
            ('d', 0),
            ('中', 1),
            ('文', 1),
            ('字', 1),
            ('词', 1),
        ] {
            // 每行自己数自己的列：一格 10pt 宽。
            let column = chars.iter().filter(|item| item.line == line).count();
            chars.push(PageChar {
                ch,
                rect: PageRect {
                    x: column as f32 * 10.0,
                    y: 10.0 + line as f32 * 20.0,
                    width: 10.0,
                    height: 12.0,
                },
                line,
            });
        }
        PageText {
            width: 100.0,
            height: 60.0,
            chars,
        }
    }

    /// 命中的两条规则与排版那一侧同一条口径（`content::hit::hit_test`）：
    /// 落在字格里时按**前进量的中点**判这一格之前还是之后，落在字格之外时收到**最近的那个字**
    /// （页边距、行间空白都该给一个插入点，判「没命中」会让拖动收不住边）。
    #[test]
    fn hitting_a_page_uses_the_char_midpoint_like_the_typesetter_does() {
        let page = two_lines();
        // 「a」占 0..10：点在 4（左半边）→ 插在它之前，点在 6（右半边）→ 在它之后。
        assert_eq!(page.hit(4.0, 14.0), Some(0));
        assert_eq!(page.hit(6.0, 14.0), Some(1));
        // 行末之后与行首之前：给插入点，不是 None。
        assert_eq!(page.hit(46.0, 14.0), Some(5));
        assert_eq!(page.hit(-30.0, 14.0), Some(0));
        // 第二行「中」：字序里它是第 5 格，所以行内左半边给 5、右半边给 6。
        assert_eq!(page.hit(4.0, 34.0), Some(5));
        assert_eq!(page.hit(6.0, 34.0), Some(6));
        // 页底下很远的地方仍收到最近的那一字（第二行末尾之后），不是判空。
        assert_eq!(page.hit(50.0, 200.0), Some(9));
        // 一个字符都没有的一页：没有可插的位置。
        assert_eq!(PageText::default().hit(10.0, 10.0), None);
    }

    /// **拉丁连成词、中文与空白单字自为界** —— 这一份判据不重写，与排版那一侧共用
    /// `content::hit::CharClass`。中文不按引擎「词」的理由是实测：一份中文 PDF 里最长的
    /// 一条「词」是 46 个字的一整句，按它吸附等于点一下就选中一整句。
    #[test]
    fn a_latin_tap_snaps_to_the_word_while_a_cjk_tap_snaps_to_one_char() {
        let page = two_lines();
        // 「c」往后连上「d」，但前面那个空格是另一类，所以收 `cd` 两格。
        assert_eq!(page.snap(3), 3..5);
        // 「b」往前连上「a」：整词 `ab`，到那个空格为止。
        assert_eq!(page.snap(1), 0..2);
        // 空格自己一段（收边不吞掉两侧的词）。
        assert_eq!(page.snap(2), 2..3);
        // 中文：一个字就是一格，绝不扩到整行。
        assert_eq!(page.snap(5), 5..6);
        assert_eq!(page.snap(7), 7..8);
        // 空页与越界都不 panic。
        assert_eq!(PageText::default().snap(4), 0..0);
        assert_eq!(page.snap(999), 8..9);
    }

    /// 收边不许跨行：第二行开头那一格往前收，不能把第一行的尾巴也卷进来。
    #[test]
    fn snapping_stays_inside_its_own_line() {
        let mut page = two_lines();
        // 把第二行也改成拉丁，跨行的字母才是真会连出去的那种形状。
        for item in page.chars.iter_mut().filter(|item| item.line == 1) {
            item.ch = 'x';
        }
        assert_eq!(page.snap(5), 5..9, "第二行四个 x 连成一段，不碰第一行");
        assert_eq!(page.snap(8), 5..9);
    }

    /// 选中按行出**一条**色块（一帧画一次，与浏览器一样按行断），复制跨行断行，
    /// 而偏移空间本身不塞换行 —— 否则锚点数的序与字序就错开了。
    #[test]
    fn a_selection_draws_one_box_per_line_and_copies_with_line_breaks() {
        let page = two_lines();
        let all = 0..page.char_count();
        assert_eq!(page.text(), "ab cd中文字词", "摊平不插换行");
        assert_eq!(page.slice(all.clone()), "ab cd\n中文字词");
        // 只勾一行里的一段：一条色块，宽到那一段的右沿。
        let part = 0..2;
        let rects = page.rects(part.clone());
        assert_eq!(rects.len(), 1);
        assert_eq!(
            (rects[0].x, rects[0].width, rects[0].height),
            (0.0, 20.0, 12.0)
        );
        // 跨两行 = 两条，各按各的行。
        let across = page.rects(3..7);
        assert_eq!(across.len(), 2);
        assert_eq!((across[0].y, across[1].y), (10.0, 30.0));
        // 空区间与越界的区间都不该 panic，也不该画东西。
        assert!(page.rects(4..4).is_empty());
        assert_eq!(page.slice(6..999), "文字词");
    }
}
