//! 移动端外壳：底部导航栏与它下面的四个目的地。
//!
//! 桌面端的导航来自 gpui-kit 的 `Sidebar` / `TabBar`。移动端两边都没有可复用的：
//! `crates/folio-mobile` 链的 GPUI 是 crates.io 的 `gpui-pre` 快照，只有原语
//! （`div` / `InteractiveElement` / `ParentElement`）；`gpui-mobile` 自带一套
//! Material 组件（`gpui_mobile::components::material::NavigationBarBuilder`，
//! 就是底部导航栏），但它把 MD3 的紫色系配色写死在 `build()` 里，图标位用的是
//! emoji —— 而 emoji 要能画出来得先把字体推进应用 assets 并注册 AssetSource。
//! 所以这条栏用 `div` 原语拼，颜色沿用 `reader_view` 那组纸墨值。
//!
//! 点击在 Android 上成立，靠的是 gpui 自己的手势识别器：平台层只上报
//! `PlatformInput::Touch`（`gpui-mobile/src/android/window.rs:1490`），
//! 由 `Window::dispatch_recognized_touch_gesture` 把一次轻点合成
//! `RecognizedTouchGesture::Tap { down, up }` 再派发 MouseDown/MouseUp
//! （`gpui-pre-0.3.8/src/window.rs:5719`）。因此这里用 `on_click`（抬起才算），
//! 而不是 `on_mouse_down`（按下就算，误触率更高）。
//!
//! 文字一律用 `Styled::font` 显式指定这台设备真能解析出的家族：gpui 的默认文字
//! 样式挂的是桌面回退名，`TextSystem::resolve_font` 在全串解析不到时是 panic
//! （见 `reader_view::device_family`）。

use folio_core::layout::FontKey;
use gpui::{
    div, prelude::*, px, transparent_black, App, ClickEvent, Entity, Font, SharedString, Window,
};

use crate::measure::mobile_font;
use crate::reader_view::{device_family, ink, muted, paper, ReaderView};

/// 栏高（逻辑像素）。同一个数既用来定这条栏的高度，也在 `ShellView::boot` 里
/// 作为「底部被外壳占掉的高度」告诉正文，让正文的可滚范围据此缩短。
const BAR_HEIGHT: f32 = 64.0;
/// 选中标记的宽度与厚度：放在文字上方并**始终占位**，
/// 否则四格的文字会因选中状态切换而上下跳。
const INDICATOR_WIDTH: f32 = 22.0;
const INDICATOR_HEIGHT: f32 = 3.0;
const LABEL_SIZE: f32 = 13.0;
const PLACEHOLDER_TITLE_SIZE: f32 = 17.0;
const PLACEHOLDER_NOTE_SIZE: f32 = 13.0;

/// 四个目的地。顺序就是导航栏从左到右的顺序。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// 正文。一期移动端的书库（导入、多书列表）还没接，
    /// 所以这一格显示的是打开的那本书，而不是书架网格。
    Shelf,
    Stats,
    Notes,
    Account,
}

impl Tab {
    /// 导航栏上的文字。
    fn label(self) -> &'static str {
        match self {
            Tab::Shelf => "书架",
            Tab::Stats => "统计",
            Tab::Notes => "笔记",
            Tab::Account => "我的",
        }
    }

    /// 点击格子的元素 id。用 ASCII 而不是标签文字，
    /// 目的是让标签可以换措辞而不改动元素树里的状态键。
    fn slug(self) -> &'static str {
        match self {
            Tab::Shelf => "shelf",
            Tab::Stats => "stats",
            Tab::Notes => "notes",
            Tab::Account => "account",
        }
    }
}

/// 窗口根视图：上面是当前目的地的内容，下面固定一条导航栏。
pub struct ShellView {
    tab: Tab,
    reader: Entity<ReaderView>,
    /// 导航栏与空状态文字用的家族，来自本机真能解析出的那一个。
    family: SharedString,
}

impl ShellView {
    pub fn boot(cx: &mut App) -> Self {
        let family = SharedString::from(device_family(cx));
        let reader = cx.new(|cx| ReaderView::boot(cx));
        // 正文的有效视口比窗口矮一个栏高，否则最后 `BAR_HEIGHT` 的内容
        // 永远滚不到（可见行筛选和滚动上限都按窗口高算过）。
        reader.update(cx, |reader, _| reader.set_bottom_reserve(BAR_HEIGHT));
        Self {
            tab: Tab::Shelf,
            reader,
            family,
        }
    }

    /// 外壳自己的文字字体：泛族名一律换成设备家族，与度量/绘制同一个入口。
    fn chrome_font(&self, weight: u16) -> Font {
        mobile_font(
            &FontKey {
                family: String::new(),
                weight,
                italic: false,
            },
            &self.family,
        )
    }

    fn bottom_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = [Tab::Shelf, Tab::Stats, Tab::Notes, Tab::Account];
        let mut bar = div()
            .flex()
            .flex_row()
            .w_full()
            .h(px(BAR_HEIGHT))
            .bg(paper());

        for tab in tabs {
            let active = self.tab == tab;
            let fg = if active { ink() } else { muted() };
            let cell = div()
                .id(tab.slug())
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(6.0))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.tab = tab;
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(INDICATOR_WIDTH))
                        .h(px(INDICATOR_HEIGHT))
                        .rounded(px(INDICATOR_HEIGHT / 2.0))
                        .bg(if active {
                            ink()
                        } else {
                            transparent_black()
                        }),
                )
                .child(
                    div()
                        .font(self.chrome_font(400))
                        .text_size(px(LABEL_SIZE))
                        .text_color(fg)
                        .child(SharedString::from(tab.label())),
                );
            bar = bar.child(cell);
        }
        bar
    }

    /// 空状态：一行目的地名 + 一行「这里还没有数据」。
    /// 三格的内容（阅读时长、批注、账号）都还不在这台设备上，
    /// 所以显示缺什么，而不是摆一组假数字。
    fn placeholder(&self, title: &'static str, note: &'static str) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .child(
                div()
                    .font(self.chrome_font(600))
                    .text_size(px(PLACEHOLDER_TITLE_SIZE))
                    .text_color(ink())
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .font(self.chrome_font(400))
                    .text_size(px(PLACEHOLDER_NOTE_SIZE))
                    .text_color(muted())
                    .child(SharedString::from(note)),
            )
    }

    fn body(&self) -> impl IntoElement {
        let cell = div().flex_1().overflow_hidden();
        match self.tab {
            Tab::Shelf => cell.child(self.reader.clone()),
            Tab::Stats => cell.child(self.placeholder("统计", "还没有阅读时长记录")),
            Tab::Notes => cell.child(self.placeholder("笔记", "还没有添加过批注")),
            Tab::Account => cell.child(self.placeholder("我的", "还没有账号设置")),
        }
    }
}

impl Render for ShellView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("folio-shell")
            .size_full()
            .flex()
            .flex_col()
            .bg(paper())
            .child(self.body())
            // 分隔线：栏和正文同为纸色，靠一条发丝线分出边界。
            .child(div().h(px(0.5)).w_full().bg(muted()))
            .child(self.bottom_bar(cx))
    }
}
