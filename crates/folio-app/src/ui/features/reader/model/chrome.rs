//! 阅读页浮层的决策部分：点在哪一区、顶栏底栏露不露、正文按哪种模式摆、进度怎么念。
//!
//! 这里只有**判断**，一个 gpui 元素都没有 —— 于是两端都编译，宿主的 `cargo test`
//! 就能把点击分区、状态机与模式退档整条跑一遍，不必上真机猜。渲染在 `page`。
//!
//! 顶栏与底栏是**压在正文上**的浮层，不推动正文重排：读者点一下屏幕，字不该跳走。
//! 正文让出的只有干净态那两行小字的高度（[`HEADER_HEIGHT`] / [`FOOTER_HEIGHT`]）。

use folio_core::layout::fragment::column_width;

/// 顶栏高（逻辑像素，不含系统状态栏）。
pub const TOP_BAR_HEIGHT: f32 = 44.0;
/// 干净态页眉那一行小字的高度。正文从它下面开始排，章名才不会压在字上。
pub const HEADER_HEIGHT: f32 = 28.0;
/// 干净态页脚那一行小字的高度。
pub const FOOTER_HEIGHT: f32 = 28.0;

/// 设置卡片里字号滑条的范围。
///
/// 设置屏那一项允许 `Reader::FONT_SIZE_MIN`–`Reader::FONT_SIZE_MAX`（那里是数字输入，
/// 宽范围不碍事），这一条是**阅读时顺手调**的滑条，只铺常用的一段；超出这段的字号仍然在设置屏里
/// 可调，滑条只是把它顶到端点显示。
pub const FONT_SIZE_RANGE: (f32, f32) = (12.0, 40.0);

/// 亮度滑条的范围。**上界不写数就是「跟随系统」**（见 `Appearance::brightness`），
/// 下界引用配置那一格的同一个数，不在界面里抄第二份。
pub const BRIGHTNESS_RANGE: (f32, f32) =
    (folio_core::infra::config::Appearance::BRIGHTNESS_MIN, 1.0);

/// 翻页时长滑条的范围，单位毫秒。两端直接引用配置那两个常数，不在界面抄第二份 ——
/// 抄了就有了第二套「最慢/最快」，滑条推到顶与文件里那个数未必是同一个东西。
///
/// 往右是**更慢**（时长变长），因为配的是时长而不是速度：两者互为倒数，取速度就得在界面上
/// 反着摆才符合「往右 = 慢一点」的直觉，读数还要再换算一次。
pub const TURN_DURATION_RANGE: (f32, f32) = (
    folio_core::infra::config::Appearance::TURN_DURATION_MIN,
    folio_core::infra::config::Appearance::TURN_DURATION_MAX,
);

/// 行距滑条的两端：与设置屏那格数字输入、与 `Reader::clamped` 夹的是同一段。
pub const LINE_HEIGHT_RANGE: (f32, f32) = (
    folio_core::infra::config::Reader::LINE_HEIGHT_MIN,
    folio_core::infra::config::Reader::LINE_HEIGHT_MAX,
);

/// 段距滑条的两端，单位是字号倍。`0` 是合法档：不空行，靠首行缩进分段。
pub const PARAGRAPH_GAP_RANGE: (f32, f32) = (
    folio_core::infra::config::Reader::PARAGRAPH_GAP_MIN,
    folio_core::infra::config::Reader::PARAGRAPH_GAP_MAX,
);

/// 首行缩进滑条的两端，单位是字符。`0` 是合法档：不缩进。
pub const INDENT_RANGE: (f32, f32) = (
    folio_core::infra::config::Reader::INDENT_MIN,
    folio_core::infra::config::Reader::INDENT_MAX,
);

/// 版心（正文栏最大宽度）滑条的两端，单位像素。下界那个 `0` 是「随窗口铺满」的约定值，
/// 不是「栏宽 0 像素」—— 滑条走到顶就是把这一格关掉，与设置屏输入框填 0 同一件事。
pub const MAX_MEASURE_RANGE: (f32, f32) = (
    folio_core::infra::config::Reader::MAX_MEASURE_MIN,
    folio_core::infra::config::Reader::MAX_MEASURE_MAX,
);

/// 左右页边距滑条的两端，单位像素。双页的中缝拿的就是这一格（[`spread_gap`]），
/// 所以读者在这张卡片上把留白推到 0，两页之间仍有那条下限。
pub const MARGIN_X_RANGE: (f32, f32) = (
    folio_core::infra::config::Reader::MARGIN_X_MIN,
    folio_core::infra::config::Reader::MARGIN_X_MAX,
);

/// 桌面上「压暗那一层」的透明度。`None`（跟随系统）与顶格都是 0 —— 不压。
///
/// 为什么桌面上要有这一层：背光不是窗口能改的东西（那是显示器的地盘，gpui 没有这个口），
/// 而读者要的「暗一点」在两端都得真做得到 —— Android 走 `screenBrightness`，桌面走应用自己
/// 盖的一层黑。同一个数，两种暗法，滑条与读数完全一样。
///
/// 上限 0.7 而不是 1.0：这一层盖的是应用自己画的字，压满就是黑屏，而「再暗一点」从来
/// 不是「看不见」。
pub fn dim_alpha(brightness: Option<f32>) -> f32 {
    match brightness {
        None => 0.0,
        Some(value) => (1.0 - value).clamp(0.0, 0.7),
    }
}

/// 双页对开时一栏最少要多宽（逻辑像素）。低于这个宽度，一行只容得下十几个字，
/// 「两页」就变成了两条窄柱子，读者的眼球要在柱子间来回跳 —— 那是比单页更差的阅读。
pub const MIN_COLUMN_WIDTH: f32 = 200.0;
/// 读者把页边距调到 0 时中缝的下限：两页之间总得有一条缝，否则页界靠猜。
pub const MIN_SPREAD_GAP: f32 = 24.0;

/// 外壳在不属于正文的那一侧占掉多少（逻辑像素）。
///
/// 分成两个轴是因为**导航栏在两端的对边不同**：手机上横躺着占高，桌面上竖着占宽。
/// 合成一个数的话另一端就得拿它去扣错的那一轴 —— 以前正是这样：桌面把它当宽扣、
/// 移动把它当高扣，同一个参数两种读法，读者看不出差别，但版心会少扣或多扣一条边。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reserve {
    /// 左侧占掉的宽度（侧栏 / 图标栏）。
    pub width: f32,
    /// 底部占掉的高度（导航栏）。
    pub height: f32,
}

impl Reserve {
    pub const ZERO: Self = Self {
        width: 0.0,
        height: 0.0,
    };
}

/// 正文的呈现模式。**三种模式是同一套分片代码的三个参数**，不是三条代码路
/// （见 `folio_core::layout::fragment`）：区别只有「一片多高」与「一页几栏」。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PageMode {
    /// 连续滚动：一章一条到底的正文。只有「翻页方式」里选了「上下滚动」才走到这里
    /// （见 [`reconcile`]），它不再是谁的默认值。
    Continuous,
    /// 整页翻，一页一栏。**默认档**：一本没动过设置的书就是无动画左右翻页。
    #[default]
    Paged,
    /// 双页对开：一页两栏。
    Spread,
}

impl PageMode {
    /// 一页几栏。
    pub fn columns(self) -> usize {
        match self {
            Self::Spread => 2,
            Self::Continuous | Self::Paged => 1,
        }
    }

    /// 是不是连续滚动（只有它是「一片高 0」，也就只有它靠滚动位置而不是页号定位）。
    pub fn is_continuous(self) -> bool {
        matches!(self, Self::Continuous)
    }

    /// 装不下两栏时退回整页翻。手机上点「双页」不会得到两条窄柱子，而是分页 ——
    /// 这一档由宽度决定、不由 target 决定，所以横屏的平板真能双页，竖屏手机不能。
    pub fn effective(self, total_width: f32, gap: f32) -> Self {
        if self == Self::Spread && !spread_fits(total_width, gap) {
            Self::Paged
        } else {
            self
        }
    }

    /// 落库用的模式名。进度本身存的是「第几个字」，与模式无关，所以换模式不丢位置；
    /// 这个名字记的是「这份进度当时长什么样」，重开一本书时照它把模式也带回来。
    pub fn storage_name(self) -> &'static str {
        match self {
            Self::Continuous => "continuous",
            Self::Paged => "page",
            Self::Spread => "spread",
        }
    }

    /// 读回的模式名认不出来就当默认档：旧库里的名字、手改过的配置都不该让书打不开。
    pub fn from_storage_name(name: &str) -> Self {
        match name {
            "continuous" => Self::Continuous,
            "spread" => Self::Spread,
            _ => Self::Paged,
        }
    }

    /// 点一下「双页」那颗切到哪一档：单页 ⇄ 双页。
    ///
    /// **连续滚动不在这一循环里**：那一档现在由「翻页方式」那条列表里唯一的入口决定
    /// （选「上下滚动」，见 [`reconcile`]），按钮不再顺手把读者从滚动里踢出来。从前它是
    /// 连续 ⇄ 双页，于是手机上那颗按钮同时管着两件不相干的事：既要决定滚还是翻，
    /// 又要决定一屏一页还是一屏两页。
    pub fn next(self) -> Self {
        match self {
            Self::Paged | Self::Continuous => Self::Spread,
            Self::Spread => Self::Paged,
        }
    }
}

/// 双页的中缝：取读者自己那条页边距，并给一条下限。
///
/// 为什么跟着 `margin_x` 走而不是定一个数：左右两页读起来该是「两页」，中缝比外面
/// 那条边距窄就会变成「一块被硬切开的正文」，页界靠猜。
pub fn spread_gap(margin_x: f32) -> f32 {
    margin_x.max(MIN_SPREAD_GAP)
}

/// 这宽度装不装得下双页。`total` 是**已扣掉左右页边距**的可用宽（[`column_width`] 只算栏）。
pub fn spread_fits(total: f32, gap: f32) -> bool {
    column_width(total, 2, gap) >= MIN_COLUMN_WIDTH
}

/// 读者从没为这本书选过档位时，按窗口宽给一个默认：**装得下两栏就双页**，装不下就整页翻。
/// 平板与桌面上那条侧栏让出来之后（见 `ui::shell::immersive_nav`），
/// 1024 宽的横屏正是靠这一条长出双页的。
///
/// **默认永不给 `Continuous`**：连续滚动是一种要读者主动选的读法（它就摆在「翻页方式」
/// 那条列表里，叫「上下滚动」），不是「还没选」时的替身。从前这里装不下两栏时回
/// Continuous，于是「翻页方式」里选的平移 / 覆盖 / 仿真折页在手机上永远不出场 ——
/// 两轴各存一份真相、又互相否决，读起来就是「选了不生效」。
pub fn default_mode(total: f32, gap: f32) -> PageMode {
    if spread_fits(total, gap) {
        PageMode::Spread
    } else {
        PageMode::Paged
    }
}

/// 定版式能选哪几张纸。仿真折页要一张**排版页**的捕获纹理（定版式那一页是位图，
/// 没有那条捕获链），所以仍然筛掉；其余四档（平移 / 上下平移 / 覆盖 / 上下滚动）
/// 与「不走动画」都讲得通 —— 「上下滚动」在定版式上也真的有一条纵向页栈可滚。
pub fn fixed_turn_applies(turn: folio_core::PageTurn) -> bool {
    turn != folio_core::PageTurn::Curl
}

/// 定版式实际按哪一档走。读者全局选的是折页时，定版式退到平移 ——
/// 不是「什么都不做」：同一本书换成流式阅读时那一档照旧生效，这里只换一个当场
/// 讲得通的形状。
pub fn fixed_turn_style(turn: folio_core::PageTurn) -> folio_core::PageTurn {
    if fixed_turn_applies(turn) {
        turn
    } else {
        folio_core::PageTurn::Slide
    }
}

/// 「翻页方式」里那一档与摆法之间**唯一**的那条耦合。
///
/// 摆法与动画档是两件事（双页可以配任何一种纸面动画），但有一对组合是自相矛盾的：
/// 连续滚动里没有「翻页」这个动作，而纸面动画在连续里永远不出场。这一格回答
/// 「选了这一档，摆法必须跟着去哪」，两个写入口（底栏那一格与设置屏那一格）因此共用
/// 同一条判据，而不是各筛一次 —— 设置屏那一格正是因为它不知道摆法、当初故意不筛，
/// 才让读者选到一个当场不出场的档。
///
/// `by_width` 是「这本书没选过摆法时该长成什么样」（[`default_mode`] 给出来的那个数）：
/// 从连续里走出去时没有别的依据可退，双页还是单页只有宽度答得了。
///
/// 只在**渲染帧**里解一次（见 `view::body`），不在写配置那一头解：设置屏改的是全局
/// `appearance.page_turn`，而摆法是按书存在会话里的，两头隔着一条边界。
pub fn reconcile(turn: folio_core::PageTurn, mode: PageMode, by_width: PageMode) -> PageMode {
    match turn {
        // 选了「上下滚动」：摆法就是连续，别管读者之前在看单页还是双页。
        folio_core::PageTurn::Scroll => PageMode::Continuous,
        // 选了任何一张纸的走法却还站在连续里：退到按宽度该长出的那一档。
        _ if mode == PageMode::Continuous => by_width,
        // 其余组合都讲得通，原样交回。
        _ => mode,
    }
}

/// 连续滚动那一屏跳的补间：起点与终点之间按走了的比例插值。
///
/// 起点是**上一帧画出来的**那一个数而不是 `scroll_y`：读者连点两下时，第二趟要从
/// 他眼前那一屏起步，否则画面先跳回上一趟的终点再走，看着像卡了一下。
pub fn tween(from: f32, to: f32, progress: f32) -> f32 {
    from + (to - from) * progress
}

/// 正文被点中的哪一区。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    /// 左三分之一：往回翻一屏。
    PrevPage,
    /// 右三分之一：往前翻一屏。
    NextPage,
    /// 中间：开合浮层。
    ToggleChrome,
}

/// 按横坐标把正文分成三区。`x` 与 `width` 都在窗口坐标里。
///
/// 宽度非正是**能发生的**：窗口正在销毁或还没量过。那种时候按中间那一档算，
/// 因为左右两档要拿宽度做除法，猜一个方向比猜另一个更没根据。
///
/// `rtl` 是**整本的翻页方向**（EPUB 的 `<spine page-progression-direction="rtl">`，
/// 落在 `FlowDoc::progression_rtl`）：从右往左读的书里点左侧是「往前翻」，点右侧是「往回翻」。
/// 三等分本身不动 —— 读者的拇指落在哪儿与语言无关，换的只是那两侧各自意味着什么。
/// 中间那一档（开合浮层）与方向无关，所以 `rtl` 不许影响它。
pub fn zone(x: f32, width: f32, rtl: bool) -> Zone {
    if width <= 0.0 {
        return Zone::ToggleChrome;
    }
    let (left, right) = if rtl {
        (Zone::NextPage, Zone::PrevPage)
    } else {
        (Zone::PrevPage, Zone::NextPage)
    };
    if x < width / 3.0 {
        left
    } else if x > width * 2.0 / 3.0 {
        right
    } else {
        Zone::ToggleChrome
    }
}

/// PC 上的一颗方向键。
///
/// 按**物理键**记而不是「上一页 / 下一页」：左右两颗在 RTL 的书里换手，上下不换 ——
/// 合成一个方向就把那一轴丢了（音量键那两条动作也是按物理键命名的，同一个理由）。
/// 判断只有 [`Arrow::turn`] 一条，宿主测试直接跑。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arrow {
    /// 左：从左往右读的书里往回翻，从右往左读的书里往前翻。
    Left,
    /// 右：与 [`Arrow::Left`] 相反。
    Right,
    /// 上：往回一屏。
    Up,
    /// 下：往前一屏。
    Down,
}

impl Arrow {
    /// 读书顺序里的 ±1（`+1` = 往后）。`rtl` 与 [`zone`] 那一支同源
    /// （`ReaderSession::progression_rtl`）：左右跟它换手，上下不换。
    pub fn turn(self, rtl: bool) -> isize {
        match self {
            Self::Left => {
                if rtl {
                    1
                } else {
                    -1
                }
            }
            Self::Right => {
                if rtl {
                    -1
                } else {
                    1
                }
            }
            Self::Up => -1,
            Self::Down => 1,
        }
    }
}

/// 一次正文点击应当产生的效果。调用方照它去动正文，状态机自己只碰浮层。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    /// 往回翻一屏。
    PrevPage,
    /// 往前翻一屏。
    NextPage,
    /// 浮层已经打开。
    OpenChrome,
    /// 浮层已经收起（收的是面板还是整层，状态机自己知道）。
    DismissChrome,
}

/// 阅读页浮层：顶栏与底栏这一层的开合。
///
/// 底栏里曾经还嵌着一块「阅读设置」面板，档位由这里的 `Panel` 记着；那块现在换成了一张
/// 底部弹层（见 `components::reading_sheet`），开合与停在哪一档归它自己的 `BottomSheetState`
/// 管 —— 与目录那一屏同一个形状。于是这一层只剩「露不露」一个布尔，浮层的收层顺序
/// （先收最上面那一层、最后才收这一条）在阅读页的 `dismiss_chrome` 里排。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Chrome {
    open: bool,
}

impl Chrome {
    /// 浮层整层是不是露出来的。
    pub fn is_open(self) -> bool {
        self.open
    }

    /// 正文上的一次点击。
    ///
    /// 浮层开着的时候，正文上的任何一击都只**收浮层**：左右两区不翻页。工具栏露头的那一刻
    /// 这一层就是模态的 —— 与压在它上面的图片查看器、两块弹层同一套读法（会话侧的
    /// `overlay_open` 也正是照这个语义让长按与拖选让位的：那一击的意思是「收浮层」，
    /// 不是「操作正文」）。左右两区照旧翻页的话，读者点中间想收浮层、点偏了一点却翻过
    /// 一页，工具栏还压在新的那一页上 —— 两种结果混在一根手指上，界限只有三分之一屏宽。
    /// 要继续读，收了浮层再点，一下就回正轨；这条边界靠「中间一区」划不清楚，就干脆不划。
    pub fn handle_body_tap(&mut self, zone: Zone) -> Effect {
        if self.open {
            self.dismiss();
            return Effect::DismissChrome;
        }
        match zone {
            Zone::ToggleChrome => {
                self.open = true;
                Effect::OpenChrome
            }
            Zone::PrevPage => Effect::PrevPage,
            Zone::NextPage => Effect::NextPage,
        }
    }

    /// 收起这一层。返回 `true` 表示这次「返回」被浮层消费了，不该再去 pop 页面。
    ///
    /// 压在这一层上面的还有图片查看器、笔记便签、脚注与两块弹层，它们的先后由调用方排
    /// （见 `view::reading_page::dismiss_chrome`）；这一格只回答「顶栏底栏还露着吗」。
    pub fn dismiss(&mut self) -> bool {
        if self.open {
            self.open = false;
            return true;
        }
        false
    }
}

/// 进度念成人话。
///
/// 一本 1225 章的书，前半小时按整数百分比念都是 `0%` —— 读者以为进度没动。
/// 所以一成以下保留一位小数；而真·没读的时候不写「0.0%」，那和「读了一点点」
/// 是两件事，别用一个小数位把它们并成一件。
pub fn percent_label(ratio: Option<f32>) -> String {
    // `None` = 这一本有章的字数还没算过，那一格**空着**。念 0% 是把「不知道」说成
    // 「还没开始读」，而这两件事在读者那里完全不同（与架上卡片同一个决定）。
    let Some(ratio) = ratio else {
        return String::new();
    };
    let percent = (ratio.clamp(0.0, 1.0) * 100.0 * 10.0).round() / 10.0;
    if percent >= 10.0 || percent == 0.0 {
        format!("{:.0}%", percent)
    } else {
        format!("{:.1}%", percent)
    }
}

#[cfg(test)]
mod tests {
    //! 这些是阅读页唯一「能在宿主上跑完」的那一层：分区、开合、模式退档、进度念法。
    //! 渲染那一侧要窗口和字体，这一侧只要数，所以真机上「点左下为什么没翻页」这类问题
    //! 应该先在这里红过一次。

    use super::{
        Arrow, BRIGHTNESS_RANGE, Chrome, Effect, INDENT_RANGE, LINE_HEIGHT_RANGE, MARGIN_X_RANGE,
        MAX_MEASURE_RANGE, MIN_COLUMN_WIDTH, PARAGRAPH_GAP_RANGE, PageMode, TURN_DURATION_RANGE,
        Zone, default_mode, dim_alpha, fixed_turn_applies, fixed_turn_style, percent_label,
        reconcile, spread_fits, spread_gap, tween, zone,
    };
    use folio_core::PageTurn;
    use folio_core::infra::config::{Appearance, Reader};

    /// 方向键：左右跟整本的翻页方向换手，上下不换。
    ///
    /// 换反了不报错，只是「RTL 的书里按右箭头往前翻」—— 与点屏幕左右分区那一条
    /// 口径对不上，而那一层只有键盘能摸到。
    #[test]
    fn the_arrow_keys_follow_the_book_direction_on_one_axis() {
        // 从左往右读：左 = 往回，右 = 往前。
        assert_eq!((Arrow::Left.turn(false), Arrow::Right.turn(false)), (-1, 1));
        // 从右往左读：左右对调。
        assert_eq!((Arrow::Left.turn(true), Arrow::Right.turn(true)), (1, -1));
        // 上下与翻页方向无关：RTL 也只换左右那一轴。
        for rtl in [false, true] {
            assert_eq!((Arrow::Up.turn(rtl), Arrow::Down.turn(rtl)), (-1, 1));
        }
    }

    #[test]
    fn the_body_is_split_into_three_zones() {
        // 三分之一 / 三分之二是拇指去够左右两页的距离，不是精确刻度；这里钉的是边界归属。
        assert_eq!(zone(0.0, 300.0, false), Zone::PrevPage);
        assert_eq!(zone(99.0, 300.0, false), Zone::PrevPage);
        assert_eq!(zone(150.0, 300.0, false), Zone::ToggleChrome);
        assert_eq!(zone(201.0, 300.0, false), Zone::NextPage);
        // 从右往左翻的书：左右两档对调，中间那一档不动。
        assert_eq!(zone(0.0, 300.0, true), Zone::NextPage);
        assert_eq!(zone(99.0, 300.0, true), Zone::NextPage);
        assert_eq!(zone(150.0, 300.0, true), Zone::ToggleChrome);
        assert_eq!(zone(201.0, 300.0, true), Zone::PrevPage);
        // 宽度非正是能发生的（窗口正在销毁或还没量过），那种时候按中间那档算。
        assert_eq!(zone(10.0, 0.0, false), Zone::ToggleChrome);
        assert_eq!(zone(10.0, -1.0, false), Zone::ToggleChrome);
    }

    #[test]
    fn a_body_tap_while_the_overlay_is_up_only_dismisses_it() {
        let mut chrome = Chrome::default();
        // 干净态：点中间开浮层，点两边翻屏。
        assert_eq!(
            chrome.handle_body_tap(Zone::ToggleChrome),
            Effect::OpenChrome
        );
        assert!(chrome.is_open());
        // 浮层开着时三区都是「收」：左右两区不翻页 —— 工具栏露头之后这一层是模态的，
        // 否则「想收浮层、点偏了一点翻过一页」两种结果混在同一根手指上。
        assert_eq!(
            chrome.handle_body_tap(Zone::PrevPage),
            Effect::DismissChrome
        );
        assert!(!chrome.is_open(), "左右那一区也该把整层收掉");
        chrome.handle_body_tap(Zone::ToggleChrome);
        assert!(chrome.is_open());
        assert_eq!(
            chrome.handle_body_tap(Zone::NextPage),
            Effect::DismissChrome
        );
        assert!(!chrome.is_open());
        // 收干净之后回到干净态的规则：两边翻屏、中间开浮层。
        assert_eq!(chrome.handle_body_tap(Zone::PrevPage), Effect::PrevPage);
        assert!(!chrome.is_open(), "翻页不该顺手把浮层打开");
        assert_eq!(chrome.handle_body_tap(Zone::NextPage), Effect::NextPage);
        assert_eq!(
            chrome.handle_body_tap(Zone::ToggleChrome),
            Effect::OpenChrome
        );
    }

    /// 这一层收一次就干净，第二下交还给系统返回。
    ///
    /// 压在它上面的那几层（图片查看器、便签、脚注、两块弹层）各自吃掉自己那一下，
    /// 顺序排在阅读页的 `dismiss_chrome` 里 —— 那一头有窗口与实体，只有真机与宿主测得动；
    /// 这一格钉的是**最下面那层**不许偷偷吃掉本该 pop 的返回。
    #[test]
    fn the_layer_is_gone_after_one_tap_and_the_next_is_handed_back() {
        let mut chrome = Chrome::default();
        assert!(!chrome.dismiss(), "干净态这一下不该被浮层消费");
        chrome.handle_body_tap(Zone::ToggleChrome);
        assert!(chrome.dismiss(), "顶栏底栏还露着，这一下该收它");
        assert!(!chrome.is_open());
        assert!(!chrome.dismiss(), "已经干净了，这一下该交还给系统返回");
    }

    #[test]
    fn spread_yields_to_paged_when_two_columns_do_not_fit() {
        // 手机上那点宽度（减掉 48×2 页边距后约 190）装不下两栏 → 退回整页翻。
        assert_eq!(
            PageMode::Spread.effective(190.0, spread_gap(48.0)),
            PageMode::Paged
        );
        // 桌面窗口铺开就装得下。
        assert_eq!(
            PageMode::Spread.effective(900.0, spread_gap(48.0)),
            PageMode::Spread
        );
        // 连续滚动与分页不改变档：退档只可能发生在使用户要双页的那一步。
        assert_eq!(
            PageMode::Continuous.effective(0.0, 0.0),
            PageMode::Continuous
        );
        assert_eq!(PageMode::Paged.effective(0.0, 0.0), PageMode::Paged);

        // 判据是「一栏至少多宽」，所以正好卡在临界点上。
        let gap = 24.0;
        assert!(spread_fits(MIN_COLUMN_WIDTH * 2.0 + gap, gap));
        assert!(!spread_fits(MIN_COLUMN_WIDTH * 2.0 + gap - 1.0, gap));
        // 页边距调到 0 也留一条缝，别让两页贴成一条。
        assert_eq!(spread_gap(0.0), 24.0);
        assert_eq!(spread_gap(64.0), 64.0);
    }

    /// 默认档的那条线画在哪：装得下两栏给双页，装不下给**分页**，任何宽度都不给连续。
    ///
    /// 这条守的是「选了不生效」那一半的根：从前装不下两栏时默认回 Continuous，于是
    /// 手机上「翻页方式」里那四档纸面动画全都在一个永不出场的摆法里等着，读者在设置屏
    /// （那一格不按摆法筛）选到仿真翻页，屏上什么都不动。
    #[test]
    fn the_default_mode_follows_the_width_and_never_lands_in_continuous() {
        let gap = spread_gap(24.0);
        assert_eq!(default_mode(1200.0, gap), PageMode::Spread);
        assert_eq!(default_mode(400.0, gap), PageMode::Paged);
        // 临界点与 spread_fits 同一条线，不多让也不早让。
        assert_eq!(
            default_mode(MIN_COLUMN_WIDTH * 2.0 + gap, gap),
            PageMode::Spread
        );
        assert_eq!(
            default_mode(MIN_COLUMN_WIDTH * 2.0 + gap - 1.0, gap),
            PageMode::Paged
        );
        for width in [0.0, 100.0, 400.0, 900.0, 1200.0, 4000.0] {
            assert_ne!(
                default_mode(width, gap),
                PageMode::Continuous,
                "{width} 的默认档算出了连续滚动"
            );
        }
    }

    /// 一条列表 + 一颗「双页」按钮，合起来必须**永远**给出一个自洽的摆法：
    /// 连续滚动当且仅当选了「上下滚动」。这条是 [`reconcile`] 的全部契约，也是
    /// 读者那句「现在不是根据选择的翻页效果进行翻页」的正面。把 `reconcile` 整个
    /// 退回成「原样交回 self.mode」，这一条在 (Slide, Continuous) 那一格当场红。
    #[test]
    fn every_turn_entry_lands_on_a_mode_that_can_run_it() {
        let by_width = PageMode::Spread;
        for turn in PageTurn::ALL.iter().copied() {
            for started in [PageMode::Continuous, PageMode::Paged, PageMode::Spread] {
                let mode = reconcile(turn, started, by_width);
                assert_eq!(
                    mode == PageMode::Continuous,
                    turn == PageTurn::Scroll,
                    "{turn:?} 从 {started:?} 出发落进了 {mode:?}：连续滚动与纸面动画同时成立"
                );
                // 双页那颗按钮不该把读者送回连续里（那一档只有列表管得着）。
                assert_ne!(
                    mode.next(),
                    PageMode::Continuous,
                    "{turn:?} + {started:?} 落进 {mode:?} 之后点双页会掉回连续"
                );
            }
        }
        // 装不下两栏的那本（默认档是单页）从连续里走出去时，落的是单页不是双页。
        assert_eq!(
            reconcile(PageTurn::Curl, PageMode::Continuous, PageMode::Paged),
            PageMode::Paged
        );
    }

    #[test]
    fn the_spread_button_only_cycles_between_single_and_double() {
        assert_eq!(PageMode::Paged.next(), PageMode::Spread);
        assert_eq!(PageMode::Spread.next(), PageMode::Paged);
        for mode in [PageMode::Continuous, PageMode::Paged, PageMode::Spread] {
            assert_eq!(
                PageMode::from_storage_name(mode.storage_name()),
                mode,
                "{} 存出去再读回来必须是同一档",
                mode.storage_name()
            );
        }
        // 认不出的名字（旧库、手改的 JSON）当默认档，不是 panic、不是双页。
        assert_eq!(PageMode::from_storage_name(""), PageMode::Paged);
        assert_eq!(PageMode::from_storage_name("paginated"), PageMode::Paged);
    }

    /// 定版式只排除折页；读者选中的折页退到平移，界面上两个可选集与 `turn()` 实际
    /// 用的那一档因此同源。上下滚动是**真的可走**的一档（连续竖滚）。
    #[test]
    fn a_fixed_book_never_takes_a_curl_but_takes_a_scroll() {
        for turn in PageTurn::ALL.iter().copied() {
            let wanted = turn != PageTurn::Curl;
            assert_eq!(
                fixed_turn_applies(turn),
                wanted,
                "{turn:?} 在定版式上该不该出现"
            );
            let used = fixed_turn_style(turn);
            assert!(
                fixed_turn_applies(used),
                "{turn:?} 退到了 {used:?}，仍然不是定版式能走的档"
            );
            if wanted {
                assert_eq!(used, turn, "讲得通的档不该被换掉");
            } else {
                assert_eq!(used, PageTurn::Slide, "{turn:?} 该退到平移");
            }
        }
    }

    #[test]
    fn the_progress_label_distinguishes_nobody_from_a_little() {
        assert_eq!(percent_label(Some(0.0)), "0%", "没读与读了一点点是两件事");
        assert_eq!(percent_label(Some(0.005)), "0.5%", "一成以下保留一位小数");
        assert_eq!(percent_label(Some(0.12)), "12%");
        assert_eq!(percent_label(Some(1.0)), "100%");
        // 越界的比例（滚动位置刚从上一个模式残留下来时会遇到）夹住而不是念出 130%。
        assert_eq!(percent_label(Some(1.7)), "100%");
        assert_eq!(percent_label(Some(-0.3)), "0%");
        // 「这一本的字数还没算过」念**空**，不念 0%：0% 是一个读者会当真的读数，
        // 而这里的实情是「不知道」。与架上卡片那一条同一个决定。
        assert_eq!(percent_label(None), "");
    }

    /// 连续滚动那一屏跳的补间。起点用「画出来的那一个数」，不是 `scroll_y`。
    #[test]
    fn the_scroll_tween_runs_between_the_painted_positions() {
        assert_eq!(tween(100.0, 500.0, 0.0), 100.0, "刚起步还在原地");
        assert_eq!(tween(100.0, 500.0, 1.0), 500.0, "走完落在终点");
        assert_eq!(tween(100.0, 500.0, 0.5), 300.0);
        // 往回翻是同一句式的负方向，不需要另一条规则。
        assert_eq!(tween(500.0, 100.0, 0.5), 300.0);
        // 终点就是起点（夹在章尾、一个字没挪）时不该抖出一个非零的数。
        assert_eq!(tween(240.0, 240.0, 0.37), 240.0);
    }

    /// 桌面上那一层压暗的透明度。**两端都不许压**：「跟随系统」与滑条推到顶是同一件事，
    /// 顶格还留着黑层的话，读者没有一档能把界面调回原样。
    #[test]
    fn the_dim_layer_is_absent_at_both_ends_of_the_scale() {
        assert_eq!(dim_alpha(None), 0.0, "跟随系统 = 不压");
        assert_eq!(dim_alpha(Some(1.0)), 0.0, "顶格 = 不压");
        // 中间按「少掉的那一成」压。数取二进制里精确的那几个，浮点上的等式才钉得住
        // （`1.0 - 0.6` 在 f32 里是 0.39999998，等式会红在数上而不是红在逻辑上）。
        assert_eq!(dim_alpha(Some(0.5)), 0.5);
        assert_eq!(dim_alpha(Some(0.75)), 0.25);
        // 下限再暗也不压满：这一层盖的是应用自己画的字，压满就是黑屏。
        assert_eq!(dim_alpha(Some(0.05)), 0.7);
        assert_eq!(
            dim_alpha(Some(0.0)),
            0.7,
            "越界的 0 也夹住，画面不该整个消失"
        );
    }

    /// 亮度滑条的两端。**上界必须正好是 1.0** —— 那一格归成「跟随系统」，推不到顶读者就
    /// 关不掉这一层；下界引用配置那一格的同一个数，不在界面里抄第二份。
    #[test]
    fn the_brightness_slider_reaches_the_follow_the_system_end() {
        assert_eq!(BRIGHTNESS_RANGE.1, 1.0, "推不到顶就归不成「跟随系统」");
        assert_eq!(BRIGHTNESS_RANGE.0, Appearance::BRIGHTNESS_MIN);
        assert!(BRIGHTNESS_RANGE.0 > 0.0, "0 是黑屏，不是一个档位");
    }

    /// 卡片里每条滑条的两端必须就是配置夹的那两端。
    ///
    /// 这条不是重复断言一遍常数：它走的是 `Reader::clamped` / `Appearance::normalized`
    /// 那两条真路径。有人在界面里把端点抄成一个字面量，滑条就能拖到一个「写了但存不进
    /// 配置」的位置 —— 屏幕上数变了，落盘那一刻被夹回去，下一帧又缩回来。
    #[test]
    fn the_card_sliders_stop_where_the_config_clamps() {
        // 上界：滑条推到顶写进去的那个数，配置夹完不许再动。
        let top = Reader {
            line_height: LINE_HEIGHT_RANGE.1 + 1.0,
            paragraph_gap_em: PARAGRAPH_GAP_RANGE.1 + 1.0,
            indent_em: INDENT_RANGE.1 + 1.0,
            max_measure: MAX_MEASURE_RANGE.1 + 1.0,
            margin_x: MARGIN_X_RANGE.1 + 1.0,
            ..Reader::default()
        }
        .clamped();
        assert_eq!(
            top.line_height, LINE_HEIGHT_RANGE.1,
            "行距滑条顶不到配置的上界"
        );
        assert_eq!(top.paragraph_gap_em, PARAGRAPH_GAP_RANGE.1);
        assert_eq!(top.indent_em, INDENT_RANGE.1);
        assert_eq!(top.max_measure, MAX_MEASURE_RANGE.1);
        assert_eq!(top.margin_x, MARGIN_X_RANGE.1);
        // 下界同理。版心那一格故意填负数：那个 `0` 是「随窗口铺满」的约定值，夹完还得是 0，
        // 而不是被哪一条「至少留一点」的顺手夹挤成一像素。
        let bottom = Reader {
            line_height: LINE_HEIGHT_RANGE.0 - 1.0,
            paragraph_gap_em: PARAGRAPH_GAP_RANGE.0 - 1.0,
            indent_em: INDENT_RANGE.0 - 1.0,
            max_measure: -1.0,
            margin_x: MARGIN_X_RANGE.0 - 1.0,
            ..Reader::default()
        }
        .clamped();
        assert_eq!(bottom.line_height, LINE_HEIGHT_RANGE.0);
        assert_eq!(bottom.paragraph_gap_em, PARAGRAPH_GAP_RANGE.0);
        assert_eq!(bottom.indent_em, INDENT_RANGE.0);
        assert_eq!(bottom.max_measure, MAX_MEASURE_RANGE.0);
        assert_eq!(bottom.margin_x, MARGIN_X_RANGE.0);
        // 翻页时长同理：两端引用配置的常数，界面不许有第二套「最慢/最快」。
        let slow = Appearance {
            turn_duration: TURN_DURATION_RANGE.1 + 1.0,
            ..Appearance::default()
        }
        .normalized();
        assert_eq!(slow.turn_duration, TURN_DURATION_RANGE.1);
        let fast = Appearance {
            turn_duration: TURN_DURATION_RANGE.0 - 1.0,
            ..Appearance::default()
        }
        .normalized();
        assert_eq!(fast.turn_duration, TURN_DURATION_RANGE.0);
    }
}
