//! 存储层对外的数据类型。
//!
//! 这里只有「一行的形状」，没有一条 SQL —— 查询在各自的主题模块里
//! （`books` / `content` / `annotations` / `collections` / `reading`）。
//!
//! 唯一值得解释的是 [`ShelfItem`] 为什么不是 `BookRow` 加一列：见它自己的文档。

use serde::{Deserialize, Serialize};

use crate::anchor::TextAnchor;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookRow {
    pub id: i64,
    pub path: String,
    pub title: String,
    pub authors: Vec<String>,
    pub format: String,
    pub size: u64,
    pub last_opened_at: Option<i64>,
}

/// 书架上的一张卡片：`BookRow` 那些项再加封面字节。
///
/// 单开一个类型而不是往 `BookRow` 上加 `cover`，是因为封面是这排里唯一大的东西
/// （一本几百 KB 到几 MB）。桌面端的侧栏把整个书库握在手里当标签页列表用，
/// 那个用途不需要封面字节，也就没必要为它把每本书的字节都读进内存。
/// 没有 `Eq`，因为 [`ShelfItem::progress`] 是 `f32`（`NaN` 之下 `Eq` 会撒谎）。
/// 书架上的比较是给人看的排序与断言，`PartialEq` 够用。
#[derive(Debug, Clone, PartialEq)]
pub struct ShelfItem {
    pub id: i64,
    pub title: String,
    pub authors: Vec<String>,
    pub format: String,
    /// 封面原始字节，未解码。没封面（Markdown / TXT，或 EPUB 里没声明）时是 `None`，
    /// 界面要画一块占位而不是空的一格。
    pub cover: Option<Vec<u8>>,
    pub last_opened_at: Option<i64>,
    /// **已读比例**（0.0–1.0），书架那颗进度条读它。
    ///
    /// 它是**数字数**算出来的，不是排版几何：`(前面各章字数之和 + 章内偏移) ÷ 全书总字数`
    /// （`plans/Locator.md` 第二节）。两条好处都由这一点买来：**不用打开书**就能显示，
    /// 而且两台屏宽不同的设备显示的是同一个数。
    ///
    /// `None` 有三种来路，界面都该按「没有进度可显示」处理，而不是当成 0%：
    /// 从没打开过（`positions` 里没有这一行）、全书字数还没算过（v14 之前入库的行，
    /// 或 provider 那一趟只读了清单）、或从没落过库的书。
    pub progress: Option<f32>,
}

/// 界面那四色高亮的合法取值。存的是**名字**而不是 ARGB：配色属于主题，
/// 把颜色值写死进标注，将来换主题就成了修不了的数据问题。
pub const ANNOTATION_COLORS: &[&str] = &["amber", "rose", "sky", "lime"];

/// 一条标注：坐标（含找回它所需的指纹）+ 读者写的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub id: i64,
    pub book_id: i64,
    /// 章在 `FlowDoc::chapters` 里的下标。表里另有 `chapter_id` 外键，但那是索引与
    /// 级删用的；**坐标属于哪一章以这里为准**，因为它和锚点是一起写进去的。
    pub chapter: u32,
    pub anchor: TextAnchor,
    pub note: String,
    pub color: String,
    /// 锚点找不回来了。界面要显式标出来，而不是把批注画在猜出来的位置上。
    pub stale: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 一个集合（分组）。名字是用户输入的、跨书存在的东西，所以它跟着库走而不是配置。
///
/// `book_count` 是列表页要显示的那个数。带上它而不是让调用方为每个集合再查一次，
/// 是因为「N 本」本来就在这条 `LEFT JOIN ... GROUP BY` 里算得出来，多一次往返只是白费。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub id: i64,
    pub name: String,
    pub book_count: usize,
}

/// 一次阅读会话。时间都是 Unix 秒。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadingSession {
    pub id: i64,
    pub book_id: i64,
    pub started_at: i64,
    pub ended_at: i64,
    pub seconds: i64,
    /// 会话期间净前进的字符数。翻回前面会让它变小甚至变负 —— 它记的是「位置移动」，
    /// 不是「翻了多少」，所以显示时按 `max(0)` 的语义读。
    pub chars: i64,
}

/// 一本书的阅读累计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BookReading {
    pub book_id: i64,
    pub seconds: i64,
    pub sessions: i64,
    pub chars: i64,
    /// 最后一次会话的结束时间。没读过是 `None`。
    pub last_read_at: Option<i64>,
}

/// 读过的书的合计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadingTotals {
    pub seconds: i64,
    pub sessions: i64,
    /// 至少读过一次的书有几本。
    pub books: i64,
    pub chars: i64,
}

/// 一天（读者本地日）的合计。`day` 是 `YYYY-MM-DD`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyReading {
    pub day: String,
    pub seconds: i64,
    pub chars: i64,
}

/// 一段时间窗口（读者本地日）的全部读数，一次查询算完。
///
/// 为什么不是一个一个数各查一遍：这一屏上的每个数都必须来自**同一批会话行**。分开查
/// 除了多打几次库，还会让「今天 + 本周 = 合计」这种关系在两次查询之间被新写入的会话
/// 打断 —— 界面正画到一半，读者切了个屏回来,那一栏的加总就对不上图。
///
/// 派生的那几个数（连续天数、活跃天数）都是 [`DailyReading`] 这串**补过零**的桶上的
/// 纯算术，所以它们与图共用同一条数据通路，不可能出现「图上有那根柱子，天数却没算它」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingSummary {
    /// 窗口内每天一条，**含没有记录的天（补零）**，最后一条是今天。x 轴要连续。
    pub days: Vec<DailyReading>,
    pub seconds: i64,
    pub sessions: i64,
    /// 窗口内读过的书有几本。
    pub books: i64,
    /// 有读的那些天有几天（= `days` 里 `seconds > 0` 的条数）。
    pub active_days: usize,
    /// 连着读的天数。**今天还没读时从昨天起算**，否则每天早晨这个数都会归零一次。
    pub streak: usize,
    /// 窗口内最长的一段连续。
    pub longest_streak: usize,
}

impl ReadingSummary {
    /// 今天（窗口最后那一格）的时长。窗口为空时是 0。
    pub fn seconds_at_today(&self) -> i64 {
        self.days.last().map_or(0, |day| day.seconds)
    }

    /// 有读的那些天的日均。
    ///
    /// 除以**活跃天数**而不是窗口天数：「近 14 天日均」要回答的是「他坐下来读的时候
    /// 一次读多久」，把没读的那几天摊进去会把这个数一路压低到没有意义。
    pub fn average_per_active_day(&self) -> i64 {
        if self.active_days == 0 {
            0
        } else {
            self.seconds / self.active_days as i64
        }
    }
}

/// 一本书在某个窗口内的排行（「时间花在哪些书上」那一条）。
///
/// 带着 `title` 而不是只给 `book_id` 让界面再查一次书：那一列本来就在这条
/// `JOIN books` 里，多一次往返只是白费（与 `Collection` 的 `book_count` 同一条理由）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingRank {
    pub book_id: i64,
    pub title: String,
    pub seconds: i64,
    pub sessions: i64,
}
