//! 搬进 folio-model 的类型上、与本层（扫描 / 注册表 / DOM）绑定的行为。
//!
//! 类型只有数据（那是 folio-model 的边界），而这些方法要么问 provider 注册表、
//! 要么跑一遍标记扫描、要么建一张坐标表 —— 都是行为，所以作为扩展 trait 留在
//! folio-format。调用方导入对应 trait 后，写法与从前一致。

use folio_model::format::{Chapter, Format};

use crate::content::{FlowDoc, PageText};
use crate::scan::ChapterSummary;

pub trait ChapterExt {
    /// 正文就在手上的那一章。
    fn inline(title: impl Into<String>, source: impl Into<String>) -> Chapter;

    /// 正文在**手上**时的规范化可见字数；不在手上、或这一趟没算过，都是 `None`。
    fn char_len(&self) -> Option<usize>;

    /// 补上扫描那一趟的两样数：字数与图片清单。
    fn with_scan(self, summary: &ChapterSummary) -> Chapter;
}

impl ChapterExt for Chapter {
    fn inline(title: impl Into<String>, source: impl Into<String>) -> Chapter {
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
        let summary = (!source.is_empty()).then(|| crate::scan::chapter_summary(&source));
        Chapter {
            title: title.into(),
            spine_href: String::new(),
            text: folio_model::format::ChapterText::Inline(source),
            char_len: summary.as_ref().map(|s| s.visible_chars),
            images: summary.map(|s| s.images).unwrap_or_default(),
        }
    }

    fn char_len(&self) -> Option<usize> {
        self.char_len.or_else(|| {
            self.text
                .inline()
                .filter(|text| !text.is_empty())
                .and_then(crate::visible_chars)
        })
    }

    fn with_scan(mut self, summary: &ChapterSummary) -> Chapter {
        self.char_len = Some(summary.visible_chars);
        self.images = summary.images.clone();
        self
    }
}

pub trait FormatExt {
    /// 落地副本该用的扩展名（不带点、小写）。
    fn copy_extension(self) -> &'static str;
}

impl FormatExt for Format {
    fn copy_extension(self) -> &'static str {
        // **问的是 provider 注册表**，不是在这里手写一张对照表：副本存成
        // `<哈希前缀>.<这一格>`，而下一次按扩展名认领文件的正是那张表 —— 两处各写一份
        // 就会漂，而漂的症狀最狠：导进去一本书，它自己的副本再也认不出来
        // （实测过一次，`Format::as_str` 给的是 `text` / `markdown`，注册表认的是
        // `txt` / `md`）。
        //
        // 认法：拿注册表全部被认领的扩展名逐个探，取第一个「provider 认下它，且它
        // 报出的格式就是自己」的那个。剩下没有命中的（例如还没写 provider 的格式）
        // 退回 `as_str`，让日志里至少有个说得清的名字（**注册表里 21 种格式都有 provider，
        // 所以这一档现在只兜「忘了往 `supported_extensions` 里加」这种漂移**）。
        crate::provider::supported_extensions()
            .iter()
            .copied()
            .find(|ext| {
                let probe = std::path::Path::new("probe").with_extension(ext);
                crate::provider::provider_for(&probe).is_some_and(|p| p.format() == self)
            })
            .unwrap_or_else(|| self.as_str())
    }
}

pub trait PageTextExt {
    /// 这一页的**锚点坐标系**：[`PageText::text`] 那一串包成一张 [`crate::text_map::TextMap`]。
    fn text_map(&self) -> crate::text_map::TextMap;
}

impl PageTextExt for PageText {
    fn text_map(&self) -> crate::text_map::TextMap {
        // 定版式没有 DOM，所以这张表没有节点路径 —— 批注与进度因此走的是同一把尺、
        // 只是少一级找回办法（代价写在
        // [`TextMap::from_visible_text`](crate::text_map::TextMap::from_visible_text)）。
        // 存在的理由只有一条：「第几个字」这件事的算法（截取、上下文库、子串搜索）全在
        // 锚点那一层，而那一层的入口要的是 `TextMap`。在这里转一次形，界面就不必自己
        // 决定「这一页的坐标系该数什么」—— 那个决定只该有人做一次。
        //
        // 每次现建，不缓存：一页一份 `String` 与一张空表，而调用它的是「选中/落批注」
        // 那种一帧最多一次的事件，不是绘制热路径。
        crate::text_map::TextMap::from_visible_text(self.text())
    }
}

pub trait FlowDocExt {
    /// 全书**规范化可见字数** —— 全局字符进度的分母（`plans/Locator.md` 第二节）。
    fn char_len(&self) -> Option<usize>;

    /// 已知道字数的那几章之和 + 还有几章不知道 —— 进度条在「部分章没算过」时
    /// 用的降级读数。
    fn known_char_len(&self) -> (usize, usize);
}

impl FlowDocExt for FlowDoc {
    fn char_len(&self) -> Option<usize> {
        // 只要有一章的字数不知道，整本就回 `None`：**不把未知折成 0**。
        // 折了会让分母偏小、百分比偏大，而症状是「读到一半进度条已经满了」——
        // 比不显示百分比更糟，因为它是个安静的错数。
        //
        // 老库升上来的行（v14 之前）与只读了清单的 provider 都会走到 `None`；
        // 前者的处置是让下一次 `reindex` 把它填上。
        let mut total = 0usize;
        for chapter in &self.chapters {
            total = total.checked_add(chapter.char_len()?)?;
        }
        Some(total)
    }

    fn known_char_len(&self) -> (usize, usize) {
        // 第二项非零就说明这个百分比是**下界**。
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
