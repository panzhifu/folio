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
//! 一层半透明 tint、描边与柔影；嵌入档是不透明列 + 右侧发丝线。模糊只有走 wgpu 渲染器的
//! 那两端（Android / Linux 桌面）真做，Metal 与 DirectX 跳过玻璃那一层，屏上只剩 tint。
//!
//! 悬浮底栏**不占布局行**：它绝对定位压在内容上，内容自己带尾部留白（[`clearance`]），
//! 于是一边滚一边从栏底下穿过去。侧栏两档仍然占位（[`reserved_width`]）—— 压上去会
//! 挡住书架第一列，而华为的悬浮侧栏本来就可以滑出收起，我们没做那个手势。
//!
//! 桌面窗口可以拖到很窄（最小 520px），但鼠标桌面不该长出底部导航栏：
//! [`SizeClass::of`] 在桌面把结果夹到至少 `Medium`。

mod bottom_bar;
mod side_nav;
mod surface;

use gpui_kit::assets::IconName;
use gpui_kit::{App, IntoElement, Window};

use crate::ui::host;

use side_nav::side_nav;

/// 侧栏宽（逻辑像素），三档只有这一个数 —— 华为的「侧边页签」控件就是 96vp。
///
/// 它同时是内容让出去的那一条：Medium 档多两倍 [`NAV_MARGIN`]（卡片悬在列里），
/// Expanded 档就贴边（见 [`reserved_width`]）。
pub const NAV_WIDTH: f32 = 96.0;
/// 悬浮片离窗口边的那一圈（左右下同宽）。
pub const NAV_MARGIN: f32 = 12.0;
/// 悬浮片的圆角，等于底栏高的一半 —— 一条真正的胶囊，不是「圆角矩形」。
pub const NAV_RADIUS: f32 = 28.0;
/// 底栏高（逻辑像素）。华为底部页签是 56vp：图标 24 + 一行小字 + 上下各 6。
pub const BAR_HEIGHT: f32 = 56.0;

/// 导航栏在一档里**占掉**的宽度：Compact 是浮的（0），Medium 是卡片加两侧留白，
/// Expanded 是那一条 96 宽的列。
///
/// 书架排几列、统计那张热力图的格子多大，都从这一个式子取数 —— 两处各写一份的话，
/// 改一侧忘另一侧只会表现为「格子宽得莫名其妙」。
pub fn reserved_width(size: SizeClass) -> f32 {
    match size {
        SizeClass::Compact => 0.0,
        SizeClass::Medium => NAV_WIDTH + NAV_MARGIN * 2.0,
        SizeClass::Expanded => NAV_WIDTH,
    }
}

/// 底栏下沿离窗口底多少：手势条那一条 + 一圈留白。
pub fn bar_offset(safe_bottom: f32) -> f32 {
    NAV_MARGIN + safe_bottom
}

/// 内容尾部要空出来的一段：底栏高 + 上下两圈留白 + 手势条。
///
/// 这一段**加在滚动内容里**而不是外壳的容器上：加在容器上等于把视口切短，内容永远
/// 到不了栏底下，那就不叫悬浮了。滚到底时最后一排正好停在栏的上沿。
pub fn clearance(safe_bottom: f32) -> f32 {
    BAR_HEIGHT + NAV_MARGIN * 2.0 + safe_bottom
}

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

/// 窗口宽度分档（华为那两道线的三档）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SizeClass {
    /// < 600 逻辑像素：手机。底部页签。
    Compact,
    /// 600–840：平板竖屏 / 窄窗口。悬浮侧栏卡片。
    Medium,
    /// ≥ 840：桌面宽窗口。嵌入侧栏。
    ///
    /// 触屏端走不到这一档：[`SizeClass::of`] 把它夹回悬浮卡片，所以这条整列只在桌面出现。
    Expanded,
}

impl SizeClass {
    /// 按**当前窗口宽度**分档。
    ///
    /// 桌面端夹到至少 `Medium`：鼠标桌面即便窗口很窄也不该出现底部导航栏，
    /// 那里更接近「平板竖屏」而不是「手机」。
    ///
    /// 触屏端反过来夹到至多 `Medium`：那条 ≥840 的嵌入整列是华为给鼠标桌面的那一档，
    /// 平板横屏不该长出它。量在真机上：11 寸平板（DBY-W09）横屏全屏是 2560 物理 px
    /// 配 400dpi（scale 2.5）= **1024 逻辑 px**，正好越过 840 那条线，于是横屏是整列、
    /// 竖屏（640）才是悬浮卡片 —— 同一台设备转个身换一种导航，读者会读成 bug。
    /// 触屏端一律悬浮，两端各自只剩一个判据。
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
        #[cfg(mobile)]
        {
            if class == SizeClass::Expanded {
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
/// 返回的元素会撑满它在布局里拿到的盒子（侧栏自带宽度；底栏是绝对定位的胶囊，不吃行高），
/// 调用方在外层决定它占哪条边。`safe` 是宿主的系统 inset：底栏靠 `bottom` 那一条离开
/// 手势条，嵌入侧栏靠 `top` 那一条让开状态栏 / 挖孔。
pub fn navigation<F>(
    cx: &mut App,
    current: Destination,
    size: SizeClass,
    safe: &host::Insets,
    on_select: F,
) -> impl IntoElement
where
    F: Fn(Destination, &mut Window, &mut App) + Clone + 'static,
{
    match size {
        SizeClass::Compact => {
            bottom_bar::bottom_bar(cx, current, bar_offset(safe.bottom), on_select)
                .into_any_element()
        }
        SizeClass::Medium => side_nav(cx, current, true, safe.top, on_select).into_any_element(),
        SizeClass::Expanded => side_nav(cx, current, false, safe.top, on_select).into_any_element(),
    }
}
