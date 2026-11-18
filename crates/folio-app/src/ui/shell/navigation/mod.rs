//! 导航栏：一级目的地 + 按窗口宽度自适应的呈现。
//!
//! 分档照华为（HarmonyOS 设计规范「响应式应用架构」）的两道线，而不是 Material 3 的
//! 三档尺寸类：
//!
//! * **< 600**：底部页签。一级入口少、动作都在屏内，底栏是拇指够得着的那一格。
//! * **600–840**：侧栏**悬浮**成一张 96 宽的卡片，竖向居中 —— 华为在这一档给的是
//!   「侧边导航栏收起态 / 悬浮显示」，与 ≥840 的嵌入列只差一张卡片和一列留白。
//! * **≥ 840**：侧边栏**嵌入**：96 宽、整列高、贴顶排，内容从它右边起。**只有桌面走到
//!   这一档** —— 触屏端把 ≥840 夹回悬浮卡片那一档，理由见 [`SizeClass::of`]。
//!
//! 三档共用同一套表面与入口格（[`surface`]）：悬浮档是一片**真背景模糊**的玻璃，上面再压
//! 一层半透明 tint 与描边；嵌入档是不透明列 + 右侧发丝线。模糊只有走 wgpu 渲染器的
//! 那两端（Android / Linux 桌面）真做，Metal 与 DirectX 跳过玻璃那一层，屏上只剩 tint。
//!
//! 悬浮底栏**不占布局行**：它绝对定位压在内容上，内容自己带尾部留白（[`clearance`]），
//! 于是一边滚一边从栏底下穿过去。侧栏两档仍然占位（[`reserved_width`]）—— 压上去会
//! 挡住书架第一列，而华为的悬浮侧栏本来就可以滑出收起，我们没做那个手势。
//!
//! 桌面窗口可以拖到很窄（最小 520px），但鼠标桌面不该长出底部导航栏：
//! [`SizeClass::of`] 在桌面把结果夹到至少 `Medium`。

mod bottom_bar;
mod fold;
mod selection_bar;
mod side_nav;
mod surface;

pub use crate::ui::foundation::navigation::SelectionAction;
pub(in crate::ui::shell) use fold::FoldTracker;
pub(in crate::ui::shell) use selection_bar::selection_bar;

use gpui_kit::assets::IconName;
use gpui_kit::{App, IntoElement, Window};

use crate::ui::host;

use side_nav::side_nav;

pub use crate::ui::foundation::navigation::{
    BAR_HEIGHT, NAV_MARGIN, NAV_RADIUS, NAV_WIDTH, SizeClass, bar_offset, clearance, reserved_width,
};

/// 一级目的地。两端**同一个枚举** —— 导航栏与路由都按它工作。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Destination {
    Shelf,
    Stats,
    Notes,
    Account,
}

impl Destination {
    /// 全部目的地，顺序即导航栏从左到右 / 从上到下。
    pub const ALL: [Destination; 4] = [
        Destination::Shelf,
        Destination::Stats,
        Destination::Notes,
        Destination::Account,
    ];

    /// 文案键（`nav.*`）。用变量传进来，所以 `ui/shared/l10n.rs` 的 tests 扫的是 `match` 里的字面量。
    pub fn label_key(self) -> &'static str {
        match self {
            Destination::Shelf => "nav.shelf",
            Destination::Stats => "nav.stats",
            Destination::Notes => "nav.notes",
            Destination::Account => "nav.account",
        }
    }

    /// 图标。用 Lucide 目录里**确实存在**的名字：写错的符号不会报错，只会画一片空白。
    pub fn icon(self) -> IconName {
        match self {
            Destination::Shelf => IconName::Library,
            Destination::Stats => IconName::ChartColumn,
            Destination::Notes => IconName::NotebookPen,
            Destination::Account => IconName::CircleUser,
        }
    }

    /// 点击元素 id。用 ASCII 而不是标签文字，让措辞可以改而不动元素树里的状态键。
    pub fn slug(self) -> &'static str {
        match self {
            Destination::Shelf => "nav-shelf",
            Destination::Stats => "nav-stats",
            Destination::Notes => "nav-notes",
            Destination::Account => "nav-account",
        }
    }

    /// 在 [`Destination::ALL`] 里的下标，用于路由表定位。
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|d| *d == self).unwrap_or(0)
    }
}

/// 按 size class 选一种导航栏。`current` 是选中态，`on_select` 在点击时收到目的地。
///
/// 返回的元素会撑满它在布局里拿到的盒子（侧栏自带宽度；底栏是绝对定位的胶囊，不吃行高），
/// 调用方在外层决定它占哪条边。`safe` 是宿主的系统 inset：底栏靠 `bottom` 那一条离开
/// 手势条，嵌入侧栏靠 `top` 那一条让开状态栏 / 挖孔。
///
/// `folded` = 内容正在往下读、导航该让开。判据在一处：外壳听内容那一路的滚动增量
/// （[`FoldTracker]`），所以**四个目的地同一档行为**，不再是只有架面会收。浮起来的那两档
/// （Compact 底栏、Medium 卡片）各自用弹簧把位移做出来；Expanded（桌面）不参与 ——
/// 那是常驻导航，鼠标滚轮又频繁。
pub fn navigation<F>(
    cx: &mut App,
    current: Destination,
    size: SizeClass,
    safe: &host::Insets,
    folded: bool,
    window: &mut Window,
    on_select: F,
) -> impl IntoElement
where
    F: Fn(Destination, &mut Window, &mut App) + Clone + 'static,
{
    match size {
        SizeClass::Compact => bottom_bar::bottom_bar(
            cx,
            current,
            bar_offset(safe.bottom),
            folded,
            window,
            on_select,
        )
        .into_any_element(),
        SizeClass::Medium => {
            side_nav(cx, current, true, safe.top, folded, window, on_select).into_any_element()
        }
        SizeClass::Expanded => {
            side_nav(cx, current, false, safe.top, false, window, on_select).into_any_element()
        }
    }
}
