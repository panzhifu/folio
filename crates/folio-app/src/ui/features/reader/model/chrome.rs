//! 阅读页浮层的决策部分：点在哪一区、顶栏底栏露不露、正文按哪种模式摆、进度怎么念。
//!
//! 这里只有**判断**，一个 gpui 元素都没有 —— 于是两端都编译，宿主的 `cargo test`
//! 就能把点击分区、状态机与模式退档整条跑一遍，不必上真机猜。渲染在 `page`。
//!
//! 顶栏与底栏是**压在正文上**的浮层，不推动正文重排：读者点一下屏幕，字不该跳走。
//! 正文让出的只有干净态那两行小字的高度（[`HEADER_HEIGHT`] / [`FOOTER_HEIGHT`]）。

use folio_core::content::format::css::PageMargins;
use folio_core::infra::config::{Appearance, Mode, PageTurn};

// 摆法模式与它的纯函数帮手住在 `folio_core::reader`（§3.6 下沉，章界与退档规则
// 在 core 离线可测）；app 里 `chrome::PageMode` 等旧路径照旧可用。
// `MIN_COLUMN_WIDTH` / `MIN_SPREAD_GAP` / `spread_fits` 只有 core 自己用，不在此列。
pub use folio_core::reader::{PageMode, default_mode, reconcile, spread_gap};

/// 这一帧**实际**用哪一档翻页动效。
///
/// 水墨那一档把「一张纸在走」的几档（平移 / 上移 / 覆盖 / 折页）落成当场落定，读者
/// 选的那一格**不写回配置** —— 退出水墨之后原来选的那一档照旧生效。`Scroll` 原样：
/// 它是摆法（连续滚动），不是翻页动效。
pub fn effective_turn(appearance: &Appearance) -> PageTurn {
    if appearance.reading_mode.unwrap_or(appearance.mode) == Mode::Ink {
        appearance.page_turn.still()
    } else {
        appearance.page_turn
    }
}

/// 顶栏高（逻辑像素，不含系统状态栏）。
pub const TOP_BAR_HEIGHT: f32 = 44.0;
/// 干净态页眉那一行小字的高度。正文从它下面开始排，章名才不会压在字上。
pub const HEADER_HEIGHT: f32 = 28.0;
/// 干净态页脚那一行小字的高度。
pub const FOOTER_HEIGHT: f32 = 28.0;

/// 读者页边距与书的 `@page` 合成后的上下边距（像素）。
///
/// 读者的「上下页边距」拖离 0 就是覆盖：上下两个值都用读者的那个，书里写多少都不再
/// 影响。滑条为 0 时用书的 `@page`（没写就是 0）。百分比按 `basis_width`（页框宽）折，
/// 与 CSS 的百分比外边距同一条基准。
///
/// 两端都夹进 `Reader::MARGIN_Y_MIN..=MARGIN_Y_MAX`：手改配置与坏书都可能写出把整页
/// 吃掉的值。
pub fn effective_page_margins(
    reader_margin_y: f32,
    book: PageMargins,
    basis_width: f32,
) -> (f32, f32) {
    use folio_core::infra::config::Reader;
    let clamp = |value: f32| value.clamp(Reader::MARGIN_Y_MIN, Reader::MARGIN_Y_MAX);
    if reader_margin_y > 0.0 {
        let margin = clamp(reader_margin_y);
        (margin, margin)
    } else {
        let (top, bottom) = book.vertical_px(basis_width);
        (clamp(top), clamp(bottom))
    }
}

/// 页眉 / 页脚两条带在页边距之后的高度。上下可以不同：`@page` 允许只写一边。
///
/// 正文让出的高与带子自己的高必须由这一个函数算出来 —— 两处各算一次，读者把边距调大
/// 之后两者会差一截，滚动时那半行字就从缝里露出来。
pub fn bands_with_vertical_margins(top: f32, bottom: f32) -> (f32, f32) {
    (
        HEADER_HEIGHT + top.max(0.0),
        FOOTER_HEIGHT + bottom.max(0.0),
    )
}

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

/// 上下页边距滑条的两端，单位像素。它不改折行，只改一页装多少行。
pub const MARGIN_Y_RANGE: (f32, f32) = (
    folio_core::infra::config::Reader::MARGIN_Y_MIN,
    folio_core::infra::config::Reader::MARGIN_Y_MAX,
);

/// 字距滑条的两端，单位是字号倍。下界留一点负值：排版层本来就支持收紧。
pub const LETTER_SPACING_RANGE: (f32, f32) = (
    folio_core::infra::config::Reader::LETTER_SPACING_MIN,
    folio_core::infra::config::Reader::LETTER_SPACING_MAX,
);

/// 一格的两个步长。`slide` 是滑条自己的落档（手指拖到哪就停在这一格的倍数上），
/// `nudge` 是 `−` / `+` 一颗按下去走的跨度。
///
/// 两者必须差一个量级：按 `slide` 走满程要划 56–200 下，那是在骗读者说这格能调准；
/// 而 ± 只走 `slide` 就是把滑条的细档当成按钮该干的活。这一族定的原则是**全量程靠 ±
/// 落在 10–30 下**，细的那一档仍然留给滑条。
///
/// `nudge` 必须是 `slide` 的整数倍：不然按钮会把值停在格子之外，随后一次拖动先「跳」
/// 一下才回到格上。步长是数据，写在量程旁边而不是渲染代码里 —— 与上面那族 `*_RANGE`
/// 同一条立场（抄第二份就有了第二个「一档多大」）。
#[derive(Clone, Copy, Debug)]
pub struct SliderSteps {
    /// 滑条自身的落档。
    pub slide: f32,
    /// `−` / `+` 一颗按下去走的跨度。
    pub nudge: f32,
}

/// 字号：常用的一段里 ± 一档一像素，满程 28 下。
pub const FONT_SIZE_STEPS: SliderSteps = SliderSteps {
    slide: 0.5,
    nudge: 1.0,
};
/// 行距：± 一档 0.1 倍字号；按能用的那一段（1.2–2.2）算只有 10 下。
pub const LINE_HEIGHT_STEPS: SliderSteps = SliderSteps {
    slide: 0.05,
    nudge: 0.1,
};
/// 段距：± 一档 0.2 个字号倍。
pub const PARAGRAPH_GAP_STEPS: SliderSteps = SliderSteps {
    slide: 0.05,
    nudge: 0.2,
};
/// 字距：满程本来就短（0.25 的范围），± 一档 0.02。
pub const LETTER_SPACING_STEPS: SliderSteps = SliderSteps {
    slide: 0.01,
    nudge: 0.02,
};
/// 首行缩进：± 一档半字符，满程 12 下。
pub const INDENT_STEPS: SliderSteps = SliderSteps {
    slide: 0.25,
    nudge: 0.5,
};
/// 版心：这一格满程 200 档，± 已经走到 100 像素一档仍要 40 下 —— 量程本身偏大，
/// 见 `doc/排版设置操控.md` §三 那一条（它该改成档而不是像素）。
pub const MAX_MEASURE_STEPS: SliderSteps = SliderSteps {
    slide: 20.0,
    nudge: 100.0,
};
/// 左右页边距：± 一档 16 像素。
pub const MARGIN_X_STEPS: SliderSteps = SliderSteps {
    slide: 8.0,
    nudge: 16.0,
};
/// 上下页边距：与左右同一档。
pub const MARGIN_Y_STEPS: SliderSteps = SliderSteps {
    slide: 8.0,
    nudge: 16.0,
};
/// 翻页时长：两三百毫秒的差别肉眼分不出 5 ms，± 一档 50 ms。
pub const TURN_DURATION_STEPS: SliderSteps = SliderSteps {
    slide: 10.0,
    nudge: 50.0,
};
/// 背光：满程 95 个细档，± 一档十分之一屏亮度。
pub const BRIGHTNESS_STEPS: SliderSteps = SliderSteps {
    slide: 0.01,
    nudge: 0.1,
};

/// 自动前进：满程 38 个细档（2..40 页/分），± 一档就是一分钟一页。
pub const AUTO_ADVANCE_RANGE: (f32, f32) = (2.0, 40.0);
pub const AUTO_ADVANCE_STEPS: SliderSteps = SliderSteps {
    slide: 1.0,
    nudge: 1.0,
};

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

/// 外壳在不属于正文的那一侧占掉多少（逻辑像素）。
///
/// 只剩**宽**：上下两条系统边由 [`PageInsets`](folio_core::reader::PageInsets)
/// 管，`PageBox` 从那里算。桌面侧栏眼下是压在正文上的浮层，这一格实际为 0；留着它是
/// 给“真的让宽的侧栏”一个口子。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reserve {
    /// 左侧占掉的宽度（侧栏 / 图标栏）。
    pub width: f32,
}

impl Reserve {
    pub const ZERO: Self = Self { width: 0.0 };
}

/// 自动阅读这一趟用哪种推进方式。**与书的摆法脱钩**：这是读者在自动工具栏里挑的，
/// 不是书自己那一档（翻页方式对摆法的否决权见 [`reconcile`]）。
///
/// 两档各要一个参照系：滚动要连续一列上的 `scroll_y`（按真实 dt 推像素），翻页要按页号
/// 一页一张地走。所以开自动那一瞬摆法跟着这一档切过去，退出时由
/// `ReaderSession::auto_saved_layout` 原样还回去 —— 自动不该把读者原本的读法顶掉。
///
/// 不落库：它是「这一趟怎么走」，不是书或读者的长期偏好（速度那一格才落库）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AutoReadMode {
    /// 按「页/分」离散翻页。既有那一档，也是默认 —— 第一击直接开始时读法不跳。
    #[default]
    Page,
    /// 连续匀速滚动：一「页」= 一屏高，掉拍就少滚一点，不追赶。
    Scroll,
}

impl AutoReadMode {
    /// 这一档要求正文站在哪个摆法上。
    ///
    /// `Page` 沿用读者原来的分页 / 双页（不把双页压成单页）；只有原本就是连续滚动的
    /// 才退到整页翻。`Scroll` 一律切到连续 —— 分页档是按页切好的，没有全局 `scroll_y`
    /// 可推，不换摆法就只是名义上有一种模式。
    pub fn layout(self, current: PageMode) -> PageMode {
        match self {
            Self::Scroll => PageMode::Continuous,
            Self::Page => {
                if current.is_continuous() {
                    PageMode::Paged
                } else {
                    current
                }
            }
        }
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

    use super::AutoReadMode;
    use super::{
        Arrow, BRIGHTNESS_RANGE, Chrome, Effect, FOOTER_HEIGHT, HEADER_HEIGHT, INDENT_RANGE,
        LETTER_SPACING_RANGE, LINE_HEIGHT_RANGE, MARGIN_X_RANGE, MARGIN_Y_RANGE, MAX_MEASURE_RANGE,
        PARAGRAPH_GAP_RANGE, PageMode, TURN_DURATION_RANGE, Zone, bands_with_vertical_margins,
        dim_alpha, effective_page_margins, effective_turn, percent_label, tween, zone,
    };
    use folio_core::PageTurn;
    use folio_core::content::format::css::PageMargins;
    use folio_core::content::format::css::computed::Length;
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

    /// 自动阅读两档各要一个参照系：滚动非连续不可，翻页不把读者原本的双页压成单页，
    /// 只有原本就是连续滚动的才退到整页翻（那里没有页号可翻）。
    #[test]
    fn auto_read_modes_ask_for_their_own_layout() {
        assert_eq!(
            AutoReadMode::Scroll.layout(PageMode::Paged),
            PageMode::Continuous
        );
        assert_eq!(
            AutoReadMode::Scroll.layout(PageMode::Spread),
            PageMode::Continuous,
            "滚动档一律切连续：分页档按页切好，没有全局 scroll_y 可推"
        );
        assert_eq!(
            AutoReadMode::Scroll.layout(PageMode::Continuous),
            PageMode::Continuous
        );
        assert_eq!(AutoReadMode::Page.layout(PageMode::Paged), PageMode::Paged);
        assert_eq!(
            AutoReadMode::Page.layout(PageMode::Spread),
            PageMode::Spread,
            "翻页档沿用读者原本的双页，不压成单页"
        );
        assert_eq!(
            AutoReadMode::Page.layout(PageMode::Continuous),
            PageMode::Paged,
            "原本连续的退到整页翻，翻页档才翻得动"
        );
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
            letter_spacing_em: LETTER_SPACING_RANGE.1 + 1.0,
            max_measure: MAX_MEASURE_RANGE.1 + 1.0,
            margin_x: MARGIN_X_RANGE.1 + 1.0,
            margin_y: MARGIN_Y_RANGE.1 + 1.0,
            ..Reader::default()
        }
        .clamped();
        assert_eq!(
            top.line_height, LINE_HEIGHT_RANGE.1,
            "行距滑条顶不到配置的上界"
        );
        assert_eq!(top.paragraph_gap_em, PARAGRAPH_GAP_RANGE.1);
        assert_eq!(top.indent_em, INDENT_RANGE.1);
        assert_eq!(top.letter_spacing_em, LETTER_SPACING_RANGE.1);
        assert_eq!(top.max_measure, MAX_MEASURE_RANGE.1);
        assert_eq!(top.margin_x, MARGIN_X_RANGE.1);
        assert_eq!(top.margin_y, MARGIN_Y_RANGE.1);
        // 下界同理。版心那一格故意填负数：那个 `0` 是「随窗口铺满」的约定值，夹完还得是 0，
        // 而不是被哪一条「至少留一点」的顺手夹挤成一像素。
        let bottom = Reader {
            line_height: LINE_HEIGHT_RANGE.0 - 1.0,
            paragraph_gap_em: PARAGRAPH_GAP_RANGE.0 - 1.0,
            indent_em: INDENT_RANGE.0 - 1.0,
            letter_spacing_em: LETTER_SPACING_RANGE.0 - 1.0,
            max_measure: -1.0,
            margin_x: MARGIN_X_RANGE.0 - 1.0,
            margin_y: MARGIN_Y_RANGE.0 - 1.0,
            ..Reader::default()
        }
        .clamped();
        assert_eq!(bottom.line_height, LINE_HEIGHT_RANGE.0);
        assert_eq!(bottom.paragraph_gap_em, PARAGRAPH_GAP_RANGE.0);
        assert_eq!(bottom.indent_em, INDENT_RANGE.0);
        assert_eq!(bottom.letter_spacing_em, LETTER_SPACING_RANGE.0);
        assert_eq!(bottom.max_measure, MAX_MEASURE_RANGE.0);
        assert_eq!(bottom.margin_x, MARGIN_X_RANGE.0);
        assert_eq!(bottom.margin_y, MARGIN_Y_RANGE.0);
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

    /// 书的 `@page` 是默认值；读者把「上下页边距」拖离 0 就整个覆盖，上下两边都用
    /// 读者那一个数。没书（TXT / 定版式）时与这一格出现之前一样。
    #[test]
    fn book_page_margins_are_the_default_and_the_reader_overrides_them() {
        let book = PageMargins {
            top: Some(Length::Px(12.0)),
            bottom: Some(Length::Px(6.0)),
            ..PageMargins::default()
        };
        assert_eq!(effective_page_margins(0.0, book, 600.0), (12.0, 6.0));
        assert_eq!(
            bands_with_vertical_margins(12.0, 6.0),
            (HEADER_HEIGHT + 12.0, FOOTER_HEIGHT + 6.0),
            "两条带按各自那一边长高"
        );
        assert_eq!(
            effective_page_margins(30.0, book, 600.0),
            (30.0, 30.0),
            "读者一拖，书里那两条都不再影响"
        );
        assert_eq!(
            effective_page_margins(0.0, PageMargins::default(), 600.0),
            (0.0, 0.0),
            "没写 @page 的书不凭空多出边距"
        );

        let percent = PageMargins {
            top: Some(Length::Percent(0.1)),
            bottom: Some(Length::Px(999.0)),
            ..PageMargins::default()
        };
        assert_eq!(
            effective_page_margins(0.0, percent, 600.0),
            (60.0, Reader::MARGIN_Y_MAX),
            "百分比按页框宽折；越界值夹到配置上界"
        );
        assert_eq!(
            effective_page_margins(-5.0, book, 600.0),
            (12.0, 6.0),
            "负的读者值等于没设，仍然跟书"
        );
        assert_eq!(
            effective_page_margins(999.0, book, 600.0),
            (Reader::MARGIN_Y_MAX, Reader::MARGIN_Y_MAX)
        );
    }

    /// 水墨只把「一张纸在走」的几档落成当场落定，`Scroll`（连续滚动）与 `None` 原样。
    ///
    /// 这一条钉的是**别把摆法也一起停掉**：水墨下读者选着上下滚动时，那一档既是动效
    /// 也是摆法，落成 `None` 会把他踢回分页 —— 位置还在，翻页方式却变了。
    #[test]
    fn ink_stills_paper_turns_but_keeps_scrolling() {
        use folio_core::infra::config::{Appearance, Mode};

        let mut appearance = Appearance {
            mode: Mode::Light,
            page_turn: PageTurn::Curl,
            ..Default::default()
        };
        assert_eq!(
            effective_turn(&appearance),
            PageTurn::Curl,
            "正常档照读者选的走"
        );

        appearance.mode = Mode::Ink;
        for turn in [
            PageTurn::Slide,
            PageTurn::SlideUp,
            PageTurn::Cover,
            PageTurn::Curl,
        ] {
            appearance.page_turn = turn;
            assert_eq!(
                effective_turn(&appearance),
                PageTurn::None,
                "{turn:?} 在墨水下该当场落定"
            );
        }
        appearance.page_turn = PageTurn::Scroll;
        assert_eq!(
            effective_turn(&appearance),
            PageTurn::Scroll,
            "上下滚动是摆法，墨水不该把它停掉"
        );

        // 书内单独钉的阅读模式优先于软件全局，与正文那一层的折法同一条。
        appearance.mode = Mode::Light;
        appearance.reading_mode = Some(Mode::Ink);
        appearance.page_turn = PageTurn::Slide;
        assert_eq!(effective_turn(&appearance), PageTurn::None);
    }
}
