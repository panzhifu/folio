//! 阅读页浮层的决策部分：点在哪一区、浮层开到哪一档、各条带占多高、进度怎么念。
//!
//! 这里只有**判断**，一个 gpui 元素都没有 —— 于是两端都编译，宿主的 `cargo test`
//! 就能把点击分区与状态机整条跑一遍，不必上真机猜。渲染在 `page_mobile`。
//!
//! 顶栏与底栏是**压在正文上**的浮层，不推动正文重排：读者点一下屏幕，字不该跳走。
//! 正文让出的只有干净态那两行小字的高度（[`HEADER_HEIGHT`] / [`FOOTER_HEIGHT`]）。

/// 顶栏高（逻辑像素，不含系统状态栏）。
pub const TOP_BAR_HEIGHT: f32 = 44.0;
/// 干净态页眉那一行小字的高度。正文从它下面开始排，章名才不会压在字上。
pub const HEADER_HEIGHT: f32 = 28.0;
/// 干净态页脚那一行小字的高度。
pub const FOOTER_HEIGHT: f32 = 28.0;
/// 底栏高：状态行 + 进度条 + 一排入口。
pub const BOTTOM_BAR_HEIGHT: f32 = 128.0;
/// 底栏上方展开的面板高（目录列表 / 排版滑条共用一档）。
pub const PANEL_HEIGHT: f32 = 320.0;

/// 排版面板里字号滑条的范围。
///
/// 设置页那一项允许 9–72（那里是数字输入，宽范围不碍事），这一条是**阅读时顺手调**的
/// 滑条，只铺常用的一段；超出这段的字号仍然在设置页里可调，滑条只是把它顶到端点显示。
pub const FONT_SIZE_RANGE: (f32, f32) = (12.0, 40.0);
/// 排版面板里行距滑条的范围，同样是设置页 1.0–4.0 里常用的那一段。
pub const LINE_HEIGHT_RANGE: (f32, f32) = (1.2, 2.4);

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
pub fn zone(x: f32, width: f32) -> Zone {
    if width <= 0.0 {
        return Zone::ToggleChrome;
    }
    if x < width / 3.0 {
        Zone::PrevPage
    } else if x > width * 2.0 / 3.0 {
        Zone::NextPage
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
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Panel {
    /// 只有底栏本身（状态行 + 进度条 + 入口）。
    #[default]
    None,
    /// 章节表。
    Toc,
    /// 字号与行距。
    Typography,
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

    /// 浮层一共压住多高（逻辑像素）。
    ///
    /// 正文的可滚范围**不**按它算（浮层压字不重排），但浮层里那些条带自己的高度、
    /// 以及「面板展开时列表有多高」都从这里派生，所以这个数只能有一份。
    pub fn covered_height(self) -> f32 {
        if !self.open {
            return 0.0;
        }
        TOP_BAR_HEIGHT
            + BOTTOM_BAR_HEIGHT
            + if self.panel == Panel::None {
                0.0
            } else {
                PANEL_HEIGHT
            }
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
