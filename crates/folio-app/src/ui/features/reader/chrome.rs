//! 阅读页浮层的决策部分：点在哪一区、浮层开到哪一档、正文按哪种模式摆、进度怎么念。
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

/// 设置面板里字号滑条的范围。
///
/// 设置页那一项允许 9–72（那里是数字输入，宽范围不碍事），这一条是**阅读时顺手调**的
/// 滑条，只铺常用的一段；超出这段的字号仍然在设置页里可调，滑条只是把它顶到端点显示。
pub const FONT_SIZE_RANGE: (f32, f32) = (12.0, 40.0);

/// 亮度滑条的范围。**上界不写数就是「跟随系统」**（见 `Appearance::brightness`），
/// 下界引用配置那一格的同一个数，不在界面里抄第二份。
pub const BRIGHTNESS_RANGE: (f32, f32) =
    (folio_core::infra::config::Appearance::BRIGHTNESS_MIN, 1.0);

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
    /// 连续滚动：一章一条到底的正文。手机上默认这一档。
    #[default]
    Continuous,
    /// 整页翻，一页一栏。双页装不下时退回的就是它。
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

    /// 读回的模式名认不出来就当连续滚动：旧库里的名字、手改过的配置都不该让书打不开。
    pub fn from_storage_name(name: &str) -> Self {
        match name {
            "spread" => Self::Spread,
            "page" => Self::Paged,
            _ => Self::Continuous,
        }
    }

    /// 点一下切到哪一档：连续 ⇄ 双页。
    ///
    /// 三档循环会把「分页」做成一格谁都不会点的中间态：读者要的是「一直滚」或
    /// 「像书一样翻」，分页只在装不下双页时作为退路自动出现。
    pub fn next(self) -> Self {
        match self {
            Self::Continuous => Self::Spread,
            Self::Spread | Self::Paged => Self::Continuous,
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

/// 读者从没为这本书选过档位时，按窗口宽给一个默认：**装得下两栏就双页**，装不下就
/// 连续滚动。平板与桌面上那条侧栏让出来之后（见 `ui::shell::immersive_nav`），
/// 1024 宽的横屏正是靠这一条长出双页的。
///
/// 装不下时给 `Continuous` 而不是 `Paged`：分页是「一屏一页、要动手翻」，那是读者
/// 主动选的读法；从没选过的人默认拿到最不需要学的那一种。
pub fn default_mode(total: f32, gap: f32) -> PageMode {
    if spread_fits(total, gap) {
        PageMode::Spread
    } else {
        PageMode::Continuous
    }
}

/// 平移翻页里**两张纸各沿轴走多远**，按「这一趟走了多少」`progress` 算（0 = 刚起步，
/// 1 = 走完），给回 `(出场页, 进场页)`。沿哪根轴由调用方定 —— 这一层只管一个数，
/// 分成 `x` / `y` 是绘制侧的事（档位来自 `folio_core::PageTurn`）。
///
/// 两张**同速**、并且始终相距 `dist`，读起来才是一条纸带整体平移；两张走同样的量而
/// 间距会变，就变成了两张卡片各自飞。进场那张起步时在阅读方向的前方外 `dist` 处
/// （`progress` = 0 ⇒ 位移 = `±dist`），走完回到原位（`progress` = 1 ⇒ 位移 = 0）；
/// 出场那张正好相反，走完走满 `dist` 出画。
///
/// 方向接反了**不报错**，画面是「下一页从左边滑进来」—— 所以这一句抽成纯函数、
/// 并且在这里钉住方向，与 `ui::shell::nav_offset` 那一条同一取舍：宿主测试只有
/// 在这一层才摸得到位移。
pub fn slide_shifts(progress: f32, dist: f32, forward: bool) -> (f32, f32) {
    let dir = if forward { 1.0 } else { -1.0 };
    (-dir * dist * progress, dir * dist * (1.0 - progress))
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

/// 底栏那排入口展开的是哪块面板。
///
/// 目录不在这里了：它盖的是整块窗口，换成了一块底部弹层（见 `page::toc_sheet_element`），
/// 开合与停在哪一档归 `BottomSheetState` 管。这一档只管**长在底栏卡片里**的那点东西。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Panel {
    /// 只有底栏本身（状态行 + 进度条 + 入口）。
    #[default]
    None,
    /// 阅读设置：亮度、字号，底下一行是翻页方式与「更多设置」。
    ///
    /// 原来那一格「排版」（字号 + 行距）并到这里：底栏那颗「设置」展开的就是这一块，
    /// 而行距不在这一屏 —— 它是换一本书才调一次的东西，住在完整设置里，靠这一屏右边
    /// 那颗按钮压进去。
    Reading,
}

/// 阅读页浮层：整层的开合，加上底栏上面那块面板的档位。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Chrome {
    open: bool,
    panel: Panel,
}

impl Chrome {
    /// 浮层整层是不是露出来的。
    pub fn is_open(self) -> bool {
        self.open
    }

    /// 当前展开的面板。
    pub fn panel(self) -> Panel {
        self.panel
    }

    /// 正文上的一次点击。
    ///
    /// 浮层开着的时候**任何一区都只收浮层**：面板正挡着下半屏，这时候翻屏等于让读者
    /// 去猜字跑到哪去了；先把界面收干净，第二下再翻，是他看得见的两步。
    pub fn handle_body_tap(&mut self, zone: Zone) -> Effect {
        if self.open {
            self.dismiss();
            return Effect::DismissChrome;
        }
        match zone {
            Zone::PrevPage => Effect::PrevPage,
            Zone::NextPage => Effect::NextPage,
            Zone::ToggleChrome => {
                self.open = true;
                Effect::OpenChrome
            }
        }
    }

    /// 收起最上面一层。返回 `true` 表示这次「返回」被浮层消费了，不该再去 pop 页面。
    ///
    /// 面板先于整层：读者从目录跳完字回来，第二下点击就该回到干净态，
    /// 而不是把顶栏底栏再关一次、让他以为点丢了。
    pub fn dismiss(&mut self) -> bool {
        if self.panel != Panel::None {
            self.panel = Panel::None;
            return true;
        }
        if self.open {
            self.open = false;
            return true;
        }
        false
    }

    /// 点底栏的一个入口：已经是这一档就收起面板（再点一次是「不要了」），否则切过去。
    /// 整层保持露出 —— 切面板不该让顶栏闪一下。
    pub fn select(&mut self, panel: Panel) {
        self.open = true;
        self.panel = if self.panel == panel {
            Panel::None
        } else {
            panel
        };
    }
}

/// 进度念成人话。
///
/// 一本 1225 章的书，前半小时按整数百分比念都是 `0%` —— 读者以为进度没动。
/// 所以一成以下保留一位小数；而真·没读的时候不写「0.0%」，那和「读了一点点」
/// 是两件事，别用一个小数位把它们并成一件。
pub fn percent_label(ratio: f32) -> String {
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
        BRIGHTNESS_RANGE, Chrome, Effect, MIN_COLUMN_WIDTH, PageMode, Panel, Zone, default_mode,
        dim_alpha, percent_label, slide_shifts, spread_fits, spread_gap, tween, zone,
    };
    use folio_core::infra::config::Appearance;

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
    fn a_tap_collapses_the_overlay_before_it_pages() {
        let mut chrome = Chrome::default();
        // 干净态：点中间开浮层，点两边翻屏。
        assert_eq!(
            chrome.handle_body_tap(Zone::ToggleChrome),
            Effect::OpenChrome
        );
        assert!(chrome.is_open());
        // 浮层开着（还带着面板）时点哪一区都只收，不翻屏：字不该在读者眼皮底下跑走。
        // 两层（面板 + 整层）要两下才收干净，收的过程中一区都不产生翻页。
        chrome.select(Panel::Reading);
        for zone in [Zone::PrevPage, Zone::NextPage] {
            assert_eq!(chrome.handle_body_tap(zone), Effect::DismissChrome);
        }
        assert!(!chrome.is_open(), "两下之后该收干净");
        // 收干净之后才轮得到翻屏。
        assert_eq!(chrome.handle_body_tap(Zone::NextPage), Effect::NextPage);
        assert_eq!(chrome.handle_body_tap(Zone::PrevPage), Effect::PrevPage);
    }

    #[test]
    fn dismissing_peels_the_panel_before_the_whole_layer() {
        let mut chrome = Chrome::default();
        chrome.select(Panel::Reading);
        assert!(chrome.dismiss(), "第一下该被面板消费");
        assert_eq!(chrome.panel(), Panel::None);
        assert!(chrome.is_open(), "面板收了，底栏还在");
        assert!(chrome.dismiss(), "第二下收整层");
        assert!(!chrome.is_open());
        assert!(!chrome.dismiss(), "已经干净了，这一下该交还给系统返回");
    }

    #[test]
    fn picking_the_same_panel_twice_closes_it_without_hiding_the_bar() {
        let mut chrome = Chrome::default();
        chrome.select(Panel::Reading);
        assert_eq!(chrome.panel(), Panel::Reading);
        chrome.select(Panel::Reading);
        assert_eq!(chrome.panel(), Panel::None, "再点同一格是「不要了」");
        assert!(chrome.is_open(), "但顶栏底栏不该跟着闪一下");
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

    /// 默认档的那条线画在哪：装得下两栏给双页，装不下给**连续滚动**而不是分页。
    /// 分页是「一屏一页、要动手翻」，那是读者主动选的读法，不该由默认给。
    #[test]
    fn the_default_mode_follows_the_width_without_ever_defaulting_to_paged() {
        let gap = spread_gap(24.0);
        assert_eq!(default_mode(1200.0, gap), PageMode::Spread);
        assert_eq!(default_mode(400.0, gap), PageMode::Continuous);
        // 临界点与 spread_fits 同一条线，不多让也不早让。
        assert_eq!(
            default_mode(MIN_COLUMN_WIDTH * 2.0 + gap, gap),
            PageMode::Spread
        );
        assert_eq!(
            default_mode(MIN_COLUMN_WIDTH * 2.0 + gap - 1.0, gap),
            PageMode::Continuous
        );
        // 默认档永远不会算出 Paged —— 退档是读者选了双页之后的事，不是默认的事。
        for width in [0.0, 100.0, 400.0, 900.0, 1200.0, 4000.0] {
            assert_ne!(
                default_mode(width, gap),
                PageMode::Paged,
                "{width} 算出了分页"
            );
        }
    }

    #[test]
    fn the_mode_cycle_and_the_stored_name_agree() {
        assert_eq!(PageMode::Continuous.next(), PageMode::Spread);
        assert_eq!(PageMode::Spread.next(), PageMode::Continuous);
        // 退档后的分页再点一次也回连续，不会绕进「三档循环」出不来。
        assert_eq!(PageMode::Paged.next(), PageMode::Continuous);
        for mode in [PageMode::Continuous, PageMode::Paged, PageMode::Spread] {
            assert_eq!(
                PageMode::from_storage_name(mode.storage_name()),
                mode,
                "{} 存出去再读回来必须是同一档",
                mode.storage_name()
            );
        }
        // 认不出的名字（旧库、手改的 JSON）当连续滚动，不是 panic、不是双页。
        assert_eq!(PageMode::from_storage_name(""), PageMode::Continuous);
        assert_eq!(
            PageMode::from_storage_name("paginated"),
            PageMode::Continuous
        );
    }

    #[test]
    fn the_progress_label_distinguishes_nobody_from_a_little() {
        assert_eq!(percent_label(0.0), "0%", "没读与读了一点点是两件事");
        assert_eq!(percent_label(0.005), "0.5%", "一成以下保留一位小数");
        assert_eq!(percent_label(0.12), "12%");
        assert_eq!(percent_label(1.0), "100%");
        // 越界的比例（滚动位置刚从上一个模式残留下来时会遇到）夹住而不是念出 130%。
        assert_eq!(percent_label(1.7), "100%");
        assert_eq!(percent_label(-0.3), "0%");
    }

    /// 平移翻页的两张纸各走多远。接反了不报错，画面是「下一页从左边滑进来」，
    /// 而位移只有宿主这一层摸得到 —— 所以钉在这里。
    #[test]
    fn the_two_pages_slide_apart_at_the_same_speed() {
        let dist = 400.0;

        // 起步：出场那张还在原位，进场那张停在阅读方向的前方外一整个 `dist`。
        assert_eq!(slide_shifts(0.0, dist, true), (0.0, dist));
        // 走完：进场那张落回原位，出场那张走满 `dist` 出画。
        assert_eq!(slide_shifts(1.0, dist, true), (-dist, 0.0));

        // 中途两张始终相距 `dist`（同速 = 一条纸带），间距变了就是两张卡片各自飞。
        for step in 0..=10 {
            let progress = step as f32 / 10.0;
            let (leaving, incoming) = slide_shifts(progress, dist, true);
            assert_eq!(
                incoming - leaving,
                dist,
                "走了 {progress} 之后两张不再相距 {dist}"
            );
        }

        // 进场那张**单调靠近**原位：读数越大越往右 = 往回退着走，方向接反的现场。
        let mut last = dist;
        for step in 0..=10 {
            let progress = step as f32 / 10.0;
            let (_, incoming) = slide_shifts(progress, dist, true);
            assert!(
                incoming <= last,
                "往后翻时进场那一页在 {progress} 反而更往右了"
            );
            last = incoming;
        }

        // 往前翻整对符号反过来：进场从左边来，出场往右边退。
        assert_eq!(slide_shifts(0.0, dist, false), (0.0, -dist));
        assert_eq!(slide_shifts(1.0, dist, false), (dist, 0.0));

        // 零距离（窗口还没量过）不该长出 NaN 或半张纸。
        assert_eq!(slide_shifts(0.5, 0.0, true), (0.0, 0.0));
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
}
