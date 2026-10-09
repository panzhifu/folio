//! 移动端外壳：顶部那条章导航、中间的目的地、底部的导航栏。
//!
//! 这里的控件走 `gpui_kit::component`，与桌面端同一套 —— 两端用的是同一个 `gpui-pre`
//! 快照（`gpui_kit` 顶层把它整个转了出来），所以组件在手机上也能用。用组件而不是 `div`
//! 拼的原由不是审美：`Button` 自带 hover / pressed / selected / disabled / focus 五档
//! 状态与可访问名，手拼一套就得把这些重新实现一遍，而且实现对不对只有逐档看才知道。
//!
//! **Android 上图标可用，iOS / HarmonyOS 上不可用**：`gpui_kit::component` 的 `Icon` /
//! `IconName` 走 `svg().path("icons/xxx.svg")`，要由应用的 `AssetSource` 解析出 SVG 字节，
//! 而 `AssetSource` 只能在 `Application` 构造期注册。Android 入口链了
//! `.with_assets(gpui_kit::assets::AllAssets)`（见 `super::android_main`），所以这一层可以
//! 用图标；iOS / HarmonyOS 的 `Application` 构造在 gpui-mobile 内部且没有注册口，图标会是
//! 一片空白。
//!
//! 图标是**补充**标签的，不是替换它：这一层每个控件都保留文字，图标加在文字前面。
//! 两个理由。一是设计指南的立场——不该把一个陌生的动作换成一个猜的符号；二是
//! iOS / HarmonyOS 上没有 `AssetSource`，只用图标的按钮在那两端会整颗隐形，
//! 而「点不到的导航」比「少一个装饰」严重得多。
//!
//! ## 字体
//!
//! 组件的文字取 `Theme::font_family`，默认值是 `.SystemUIFont`，在 Android 上那一串
//! 回退名可能一个都不存在，而 `TextSystem::resolve_font` 解析不到时是 **panic**。
//! 所以 [`ShellView::boot`] 先把这个字段换成这台设备真能解析出的家族
//! （`reader_view::device_family`），再建任何用文字的视图。
//!
//! ## 触摸点击
//!
//! 靠 gpui 自己的手势识别器：平台层只上报 `PlatformInput::Touch`
//! （`gpui-mobile/src/android/window.rs:1490`），由
//! `Window::dispatch_recognized_touch_gesture` 把一次轻点合成
//! `RecognizedTouchGesture::Tap { down, up }` 再派发 MouseDown/MouseUp
//! （`gpui-pre-0.3.8/src/window.rs:5719`）。因此这里用 `on_click`（抬起才算），
//! 而不是 `on_mouse_down`（按下就算，误触率更高）。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, div, px,
};

use super::reader_view::{ReaderView, device_family};
use crate::ui::shared::chrome::{empty_state, hairline};
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::l10n::tr;
use crate::ui::shared::theme;

/// 底部栏的高度（逻辑像素）。同一个数既用来定这条栏的高度，也在 [`ShellView::boot`] 里
/// 作为「底部被外壳占掉的高度」告诉正文，让正文的可滚范围据此缩短。
///
/// 这是应用布局里少见的显式像素值，原因是它必须和正文那侧用的是**同一个数**：
/// 正文按「窗口高 − 这条栏」筛可见行，量不到 flex 之后的真实高度。
/// 56 是 Material 底栏的标准高度，也是拇指能稳定命中的最小格高。
const BAR_HEIGHT: f32 = 56.0;

/// 四个目的地。顺序就是导航栏从左到右的顺序。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// 正文。移动端的书库（导入、多书列表）还没接，
    /// 所以这一格显示的是打开的那本书，而不是书架网格。
    Shelf,
    Stats,
    Notes,
    Account,
}

impl Tab {
    /// 全部目的地，导航栏从左到右的顺序。
    const ALL: [Tab; 4] = [Tab::Shelf, Tab::Stats, Tab::Notes, Tab::Account];

    /// 导航栏上的文字对应的文案键。
    fn label_key(self) -> &'static str {
        match self {
            Tab::Shelf => "mobile.tab.shelf",
            Tab::Stats => "mobile.tab.stats",
            Tab::Notes => "mobile.tab.notes",
            Tab::Account => "mobile.tab.account",
        }
    }

    /// 这一格的图标。名字与桌面端同一概念同一个符号：侧栏的品牌位是
    /// `Library`、账号头像那格是 `CircleUser`，这里换了名字就等于让同一个目的地
    /// 在两端长得不一样。
    ///
    /// `Stats` 用 `ChartColumn` 而不是 `BarChart3`：那套 Lucide 目录里没有
    /// `bar-chart-3.svg`（改版后叫 `chart-column`），写错的名字不会报错，
    /// 只会画出一片空白。
    fn icon(self) -> IconName {
        match self {
            Tab::Shelf => IconName::Library,
            Tab::Stats => IconName::ChartColumn,
            Tab::Notes => IconName::NotebookPen,
            Tab::Account => IconName::CircleUser,
        }
    }

    /// 这一格还没接的东西，空状态里说出来。
    ///
    /// 「暂无数据」和「这台设备上还没有这个功能」是两件事，混成一句会让人
    /// 以为自己把书读空了。
    fn pending_key(self) -> &'static str {
        match self {
            Tab::Shelf => "",
            Tab::Stats => "mobile.pending.stats",
            Tab::Notes => "mobile.pending.notes",
            Tab::Account => "mobile.pending.account",
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
    /// 配置实体的订阅。留着它才有「改完偏好即落盘、即重画」；一 drop 就静默失效，
    /// 所以字段名以下划线开头表示「只为活着而存在」。
    _config_watch: gpui_kit::Subscription,
    /// 上一次涂到界面上的外观配置。渲染时比较它，见 [`ShellView::render`]。
    applied: Option<folio_core::config::Appearance>,
}

impl ShellView {
    pub fn boot(cx: &mut Context<Self>) -> Self {
        // 先把主题的应用字体换成设备上存在的那个家族，再建任何用文字的视图：
        // 顺序反了，第一帧就会拿 `.SystemUIFont` 去 resolve 而 panic。
        let wanted = ConfigHolder::reader(cx).font_family;
        let family: SharedString = device_family(&wanted, cx).into();
        theme::set_application_font(&family, cx);

        let reader = cx.new(|cx| ReaderView::boot(cx));
        // 正文的有效视口比窗口矮一个栏高，否则最后 `BAR_HEIGHT` 的内容
        // 永远滚不到（可见行筛选和滚动上限都按窗口高算过）。
        reader.update(cx, |reader, _| reader.set_bottom_reserve(BAR_HEIGHT));

        // 偏好一改就落盘并重画这一屏。重涂主题不在这里做：改偏好时手上没有窗口，
        // 而 Android 上的平台级浅深查询恒为深色 —— 只有渲染那一帧能拿到真的那个。
        let handle = ConfigHolder::handle(cx);
        let _config_watch = cx.observe(&handle, |_, _, cx| {
            ConfigHolder::save(cx);
            cx.notify();
        });

        Self {
            tab: Tab::Shelf,
            reader,
            _config_watch,
            applied: None,
        }
    }

    fn change_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    fn reader_chapter(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.advance_chapter(delta, cx));
    }

    /// 顶部那条：书名在左，章标题与两个换章按钮在右。
    ///
    /// 换章是移动设备上最主要的导航动作，所以它常驻屏顶，不藏进菜单 ——
    /// 手势翻页要等分页模式（M4）一起做。
    fn top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.reader.read(cx).title();
        let chapter = self.reader.read(cx).chapter_title();
        let position = self.reader.read(cx).chapter_position(cx);
        let crumb = match (chapter.is_empty(), position.is_empty()) {
            (false, false) => format!("{chapter} · {position}"),
            (false, true) => chapter,
            (true, false) => position,
            (true, true) => String::new(),
        };
        let can_prev = self.reader.read(cx).can_advance(-1);
        let can_next = self.reader.read(cx).can_advance(1);
        let prev = cx.entity().clone();
        let next = cx.entity().clone();

        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title.clone())),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(crumb),
            )
            .child(
                Button::new("prev-chapter")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronLeft)
                    .label(tr(cx, "reader.prev_chapter").to_string())
                    .disabled(!can_prev)
                    .on_click(move |_, _, cx| {
                        prev.update(cx, |this, cx| this.reader_chapter(-1, cx))
                    }),
            )
            .child(
                Button::new("next-chapter")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronRight)
                    .label(tr(cx, "reader.next_chapter").to_string())
                    .disabled(!can_next)
                    .on_click(move |_, _, cx| {
                        next.update(cx, |this, cx| this.reader_chapter(1, cx))
                    }),
            )
    }

    /// 底部导航栏：四个目的地，选中的那格由 `Button` 自己的 selected 态画。
    ///
    /// 不手画一条选中标记：设计指南明确说选中状态要靠那一格自己的表面与文字重量表达，
    /// 额外加一条标记会打破它的圆角轮廓，并给本已对齐的一列多出第二条边。
    fn bottom_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut bar = h_flex().w_full().h(px(BAR_HEIGHT)).items_center();
        for tab in Tab::ALL {
            let active = self.tab == tab;
            let label = tr(cx, tab.label_key());
            let entity = cx.entity().clone();
            bar = bar.child(
                Button::new(tab.slug())
                    .ghost()
                    .small()
                    .flex_1()
                    .w_full()
                    .justify_center()
                    .selected(active)
                    .icon(tab.icon())
                    .label(label.clone().to_string())
                    .accessibility_label(label)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| this.change_tab(tab, cx))
                    }),
            );
        }
        bar
    }

    /// 中间那块：正文，或者一句「这一格还缺什么」。
    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.tab == Tab::Shelf {
            return v_flex()
                .flex_1()
                .min_h_0()
                .child(self.top_bar(cx))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .child(self.reader.clone()),
                )
                .into_any_element();
        }
        div()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(empty_state(
                tr(cx, self.tab.label_key()),
                tr(cx, self.tab.pending_key()),
                cx,
            ))
            .into_any_element()
    }
}

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 外观一改（或者第一帧还没涂过）就重涂一次。
        //
        // 用 [`theme::repaint`] 而不是 [`theme::apply`]：这一帧本来就要重画，
        // 再刷所有窗口是自己触发自己。浅深问的是**这一窗口**的外观 —— Android 上
        // `cx.window_appearance()` 恒为深色，只有 `window.appearance()` 真的查过系统的夜间模式。
        let appearance = ConfigHolder::appearance(cx);
        if self.applied.as_ref() != Some(&appearance) {
            theme::repaint(&appearance, Some(window), cx);
            self.applied = Some(appearance);
        }

        // 栏和正文同为 `background` 色，靠一条发丝线分出边界。
        v_flex()
            .id("folio-shell")
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.body(cx))
            .child(hairline(cx))
            .child(self.bottom_bar(cx))
    }
}
