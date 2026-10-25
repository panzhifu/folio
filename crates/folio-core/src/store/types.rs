//! 存储层对外的数据类型。
//!
//! 这里只有「一行的形状」，没有一条 SQL —— 查询在各自的主题模块里
//! （`books` / `content` / `annotations` / `collections` / `reading`）。
//!
//! 唯一值得解释的是 [`ShelfItem`] 为什么不是 `BookRow` 加一列：见它自己的文档。

use serde::{Deserialize, Serialize};

use crate::content::anchor::TextAnchor;

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

/// 一本书的完整书目（Dublin Core 那一组 + 我们自己的格式与大小）。
///
/// 与 [`BookRow`] 分开是因为用途不同：架面一次要读几百行，只要标题、作者与进度；
/// 这一份是「信息屏」按 id 单查一条，里面有简介与主题词那种长文本。把长文本塞进
/// `BookRow` 等于每次上架都把全库的简介读进内存 —— 与封面字节出库（`books.cover`
/// 在 v16 删掉）是同一条理由，也是同一个方向。
///
/// 空串与空 `Vec` 都是「这本书没说」。界面**有值才显示那一行**，所以空值不该占一格。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BookMeta {
    pub id: i64,
    pub title: String,
    pub authors: Vec<String>,
    /// `dc:contributor`。
    pub contributors: Vec<String>,
    /// `dc:subject`。
    pub subjects: Vec<String>,
    pub description: String,
    pub publisher: String,
    /// `dc:date`，原样字符串（书里给的精度不一）。
    pub published: String,
    pub language: String,
    pub isbn: String,
    pub dctype: String,
    pub rights: String,
    pub series: Option<crate::content::format::Series>,
    pub format: String,
    pub size: u64,
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
    /// 封面在 `covers/` 里的描述（哈希 + MIME）。`None` = 老库那一档，封面只在
    /// [`ShelfItem::cover`] 那个 BLOB 里。
    pub cover_ref: Option<crate::content::format::cover::CoverRef>,
    /// **书库根**（副本所在的那一层）。缩略图缓存目录挂在它底下
    /// （`<书库>/.thumbs/<尺寸>/`），而界面手上只有副本路径，所以这一格由库里
    /// 那一列 `books.path` 反推出来（`store::book_root`）一起带出来 ——
    /// 界面不该自己再推一遍（那是第二处真相）。
    pub cover_root: Option<std::path::PathBuf>,
}

/// 界面那五色划线的合法取值，按菜单里圆点从左到右摆：红 / 紫 / 蓝 / 绿 / 黄。
/// 存的是**名字**而不是 ARGB：配色属于主题，把颜色值写死进标注，将来换主题就成了
/// 修不了的数据问题。
///
/// 顺序就是圆点的顺序，改它要连 `Palette`（`folio-app` 的 `reader/modules/annotations`）
/// 那份数组一起改；但**每个名字对应的主题色不许动** —— 库里存的是名字、画的时候按名字
/// 查回颜色，名字一换色，读者已有的批注就跟着变色。
pub const ANNOTATION_COLORS: &[&str] = &["ruby", "rose", "sky", "lime", "amber"];

/// 划线怎么画。三档，按菜单里左边那条胶囊从左到右摆：底色带 / 字下实线 / 字下波浪。
///
/// 与 [`ANNOTATION_COLORS`] 同一条存法：**存名字而不是存画法**（不存线宽、不存透明度、
/// 也不存路径点位）。那三样都属于绘制，将来调一个数就该跟着那一档走，而不是回头改库。
///
/// 这一档同时是全局偏好里那一格（`infra::config::Appearance::highlight_style`，
/// 「新划的一条默认用哪种」）—— 两件事共用一个枚举是本意的：偏好问的就是「下一条款式」，
/// 而库里那条记的就是「这一条款式」，中间不该隔一层翻译。它**不**进 `Reader` 那一节：
/// `Reader` 整份是排版失效键，一条痕迹怎么画与一行字折在哪里毫无关系（同 `PageTurn`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnnotationStyle {
    /// 铺在字底的一条色带（整行行盒那块矩形，压透明度）。默认档。
    #[default]
    Highlight,
    /// 字下那条 2 像素实线 —— v21 之前库里只有这一种画法，所以它是老行的读回值
    /// （见 `store::schema` 的 v21 那条）。
    Line,
    /// 字下那条波浪线。与 [`Self::Line`] 同一个落点（基线下 `descent × 0.618`），
    /// 只是那条线自己起伏。
    Wavy,
}

impl AnnotationStyle {
    /// 全部三档，按界面该出现的顺序。加一档忘了在这里挂，界面上就永远选不到它。
    pub const ALL: &'static [Self] = &[Self::Highlight, Self::Line, Self::Wavy];

    /// 库里与配置文件用的键名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Highlight => "highlight",
            Self::Line => "line",
            Self::Wavy => "wavy",
        }
    }

    /// 认不出的值回落 [`Self::default`]：手改坏一个键不该让整节配置退回默认，而「没有有效
    /// 值」这一格全书只该有一个答案 —— 与 [`PageTurn::parse`](crate::PageTurn::parse) 同一条理由。
    pub fn parse(value: &str) -> Self {
        match value {
            "line" => Self::Line,
            "wavy" => Self::Wavy,
            // "highlight" 与它认不出的那些写法一起落这里：默认就是它。
            _ => Self::default(),
        }
    }
}

/// 自己实现序列化，理由与 [`Mode`](crate::Mode)、[`PageTurn`](crate::PageTurn) 那两条一样：
/// derive 碰到认不出的字符串会报错，而报错的代价是**整节** `Appearance` 退回默认。
impl Serialize for AnnotationStyle {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AnnotationStyle {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Ok(Self::parse(&text))
    }
}

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
    /// 这一条怎么画。库里那一列是名字，读回来已是 [`AnnotationStyle`]
    /// （认不出的写法由存储层兜成默认档，与颜色同一口径）。
    pub style: AnnotationStyle,
    /// 锚点找不回来了。界面要显式标出来，而不是把批注画在猜出来的位置上。
    pub stale: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 一条书签：某一章里的一个位置。独立于 [`Annotation`] —— 批注锚的是「选中的那段字」，
/// 书签锚的是「正在读的那一行」；它没有颜色、画法与笔记，生存方式只有钉下、跳回、拔掉。
///
/// `anchor` 与批注同一把尺（[`TextAnchor`]）：可重排书锚住当时那一行（带指纹，重排、
/// 改字号之后找得回）；定版式那一族是点锚点 —— [`Bookmark::chapter`] 就是页号，
/// 它自己就是完整的地址。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub id: i64,
    pub book_id: i64,
    /// 章在 `FlowDoc::chapters` 里的下标；定版式那一族是页号。表里没有第二个地方记它，
    /// 与批注同一条口径（v13 摘掉 `chapter_id` 的理由）。
    pub chapter: u32,
    pub anchor: TextAnchor,
    pub created_at: i64,
}

/// 跨书列表里的一行批注：批注本体 + 它属于哪本书的书名。
///
/// 带着书名而不是只给 `book_id` 让界面再查一次：「笔记」那一屏按书分组，这一列本来
/// 就在这条 `JOIN books` 里，多一次往返只是白费（与 [`ReadingRank::title`] 同一条理由）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationCard {
    pub annotation: Annotation,
    pub book_title: String,
}

/// 跨书列表里的一行书签。与 [`AnnotationCard`] 同一条理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkCard {
    pub bookmark: Bookmark,
    pub book_title: String,
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

impl Collection {
    /// 这个组该不该摆成架面顶部那一张组卡 —— **一本都没有就不摆**：一张点进去是空白的卡
    /// 占的那一格正压着「我刚在读的那一组在哪儿」。
    ///
    /// 判据挂在类型上而不是写进每一个读它的地方：组卡与「按组清点」吃的是同一份
    /// `book_count`，口径摆在一处才不会各写一份。库里那一行**不跟着删** —— 读者移出最后一本
    /// 时未必想连组一起去掉，而他下次给别的书挂同一个组名，`create_collection` 照旧复用
    /// 那一行（同名即复用）。
    ///
    /// 「按组清点」那份数据形状（[`Store::collection_groups`](crate::Store::collection_groups)）
    /// 不归这条判据管：那一串是每组一串书，不是架面顶部那一排组卡。
    pub fn is_shown(&self) -> bool {
        self.book_count > 0
    }
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
