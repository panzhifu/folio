//! 导航栏：一级目的地 + 按窗口宽度自适应的呈现。
//!
//! 分档照 Material 3 的 window size classes：**Compact**（<600dp）用底部导航栏、
//! **Medium**（600–840dp）用图标栏（rail）、**Expanded**（≥840dp）用展开侧栏。
//! 后两者都包 gpui-kit 的 `Sidebar`（`collapsed` 时它自己只画图标），
//! 底栏没有现成组件，自绘（`bottom_bar`）。
//!
//! 桌面端窗口可以被拖到很窄（最小 520px），但鼠标桌面不该长出底部导航栏，
//! 所以 [`SizeClass::of`] 在桌面把结果夹到至少 `Medium`。

mod bottom_bar;
mod side_nav;

pub use bottom_bar::BAR_HEIGHT as BOTTOM_BAR_HEIGHT;

use gpui_kit::assets::IconName;
use gpui_kit::{App, IntoElement, Window};

use side_nav::side_nav;

/// 侧栏展开宽度（逻辑像素）。阅读页按它预留版心，所以这个数只能有一份。
pub const SIDEBAR_WIDTH: f32 = 260.0;
/// 图标栏 / 侧栏折叠后的宽度，与 gpui-kit `Sidebar` 的 `COLLAPSED_WIDTH` 对齐。
pub const RAIL_WIDTH: f32 = 48.0;

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

/// 窗口宽度分档（Material 3 window size classes 的三档简化）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SizeClass {
    /// < 600 逻辑像素：手机。
    Compact,
    /// 600–840：平板竖屏 / 窄窗口。
    Medium,
    /// ≥ 840：桌面 / 平板横屏。
    Expanded,
}

impl SizeClass {
    /// 按**当前窗口宽度**分档。
    ///
    /// 桌面端夹到至少 `Medium`：鼠标桌面即便窗口很窄也不该出现底部导航栏，
    /// 那里更接近「平板竖屏」而不是「手机」。
    pub fn of(window: &Window) -> Self {
        let width = f32::from(window.viewport_size().width);
        let class = if width < 600.0 {
            SizeClass::Compact
        } else if width < 840.0 {
            SizeClass::Medium
        } else {
            SizeClass::Expanded
        };
        #[cfg(not(mobile))]
        {
            if class == SizeClass::Compact {
                return SizeClass::Medium;
            }
        }
        class
    }

    pub fn is_compact(self) -> bool {
        self == SizeClass::Compact
    }
}

/// 按 size class 选一种导航栏。`current` 是选中态，`on_select` 在点击时收到目的地。
///
/// 返回的元素会撑满它在布局里拿到的盒子（side nav 自带宽度；bottom bar 高度由
/// [`bottom_bar::BAR_HEIGHT`] 定），调用方在外层决定它占哪条边。
pub fn navigation<F>(
    cx: &App,
    current: Destination,
    size: SizeClass,
    on_select: F,
) -> impl IntoElement
where
    F: Fn(Destination, &mut Window, &mut App) + Clone + 'static,
{
    match size {
        SizeClass::Compact => bottom_bar::bottom_bar(cx, current, on_select).into_any_element(),
        SizeClass::Medium => side_nav(cx, current, true, on_select).into_any_element(),
        SizeClass::Expanded => side_nav(cx, current, false, on_select).into_any_element(),
    }
}
