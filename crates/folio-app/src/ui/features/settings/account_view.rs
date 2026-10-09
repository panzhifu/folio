//! 「我的」目的地（Account section）的根视图：主题、书库位置、关于。
//!
//! 它和同一目录下的设置页（`typography.rs` / `appearance.rs` / …）是同一件事的两半：
//! 这一屏是**入口**，桌面从它再开设置**窗口**（一整页字段，见根屏那条「阅读设置…」），
//! 移动从它进各个设置**子页**。两者收在 `features::settings` 下，P1 把设置容器统一之后会合到一处。
//! 「能不能再开一个窗口」是平台事实，问 [`crate::ui::host::supports_extra_windows`]，
//! 不在这里写 `cfg(mobile)`。
//!
//! **不用浮层**（下拉框、对话框、菜单）：这一屏全部由普通按钮组成。浮层在 Android 上
//! 现在点不住（长按菜单那条路，见 `shelf_view` 的注释与 `docs/known-limitations.md`），
//! 而设置项点不动等于设置没有。
//!
//! 改配置只走 [`ConfigHolder`]：外壳订阅了那个实体，改完自动落盘并重画，主题也跟着
//! 重涂 —— 这里不自己调 `theme::apply`，那是渲染那一帧的事（Android 上只有那一帧拿得到
//! 真的窗口浅深）。

use folio_core::config::Mode;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::ui::host;
use crate::ui::shared::about;
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::l10n::tr;
use crate::ui::shared::library::{candidate_dirs, suggested_dir};
use crate::ui::shared::theme;
use crate::ui::widgets::{hairline, muted_label};

/// 内容区两侧的留白，和书架的格间距同一档。
const GUTTER: f32 = 12.0;

/// 「我的」这一格里现在站着哪一屏。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    /// 入口列表。
    Root,
    Theme,
    Library,
    About,
    /// 嵌进二进制的两份文档全文。
    Plan,
    Limitations,
}

impl Screen {
    /// 这一屏的标题。根屏没有标题行（底栏那一格已经说明自己在哪儿）。
    fn title_key(self) -> &'static str {
        match self {
            Screen::Root => "",
            Screen::Theme => "account.theme",
            Screen::Library => "account.library",
            Screen::About => "account.about",
            Screen::Plan => "about.plan",
            Screen::Limitations => "about.limitations",
        }
    }
}

pub struct AccountView {
    screen: Screen,
}

impl AccountView {
    pub fn boot(_cx: &mut Context<Self>) -> Self {
        Self {
            screen: Screen::Root,
        }
    }

    fn go(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.screen = screen;
        cx.notify();
    }

    /// 系统返回键（escape）打到这里：往外弹一层，并报告有没有消费掉。
    ///
    /// 已经在入口列表时返回 `false`，让外壳接着决定（最终是退出应用）—— 这一屏不该
    /// 自己吞掉返回。
    pub fn go_back(&mut self, cx: &mut Context<Self>) -> bool {
        let up = match self.screen {
            Screen::Root => return false,
            Screen::Plan | Screen::Limitations => Screen::About,
            Screen::Theme | Screen::Library | Screen::About => Screen::Root,
        };
        self.go(up, cx);
        true
    }

    /// 一行入口：图标 + 标签，点击去某一屏。
    ///
    /// 用 `ListItem` 而不是 `Button`：按钮的内容那一层是**居中**排的，设置行要左对齐；
    /// 而 `ListItem` 本来就是「一行 = 一个可选目的地」这个形状，选中态、可访问名、
    /// 尾部插槽都是现成的，不必自己补一遍交互状态。
    ///
    /// 图标和标签要**自己包一层 `h_flex`**：`ListItem` 把传进来的 children 全塞进同一个
    /// `div`（那是给「标题 + 副标题」上下叠用的），直接给两个就成了图标一行、文字一行。
    fn row(
        &self,
        id: &'static str,
        icon: IconName,
        label: SharedString,
        screen: Screen,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity().clone();
        ListItem::new(id)
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(icon).small())
                    .child(div().min_w_0().child(label.clone())),
            )
            .accessibility_label(label)
            .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.go(screen, cx)))
    }

    /// 一行「当场执行」的入口。与 [`AccountView::row`] 分开，是因为 `row` 的目标是这一栈里的
    /// 另一屏（`go(screen)`），而这一条的目标在栈外 —— 桌面那个设置窗口就是一例。
    fn action_row(
        &self,
        id: &'static str,
        icon: IconName,
        label: SharedString,
        on_click: impl Fn(&mut Window, &mut gpui_kit::App) + 'static,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        ListItem::new(id)
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(icon).small())
                    .child(div().min_w_0().child(label.clone())),
            )
            .accessibility_label(label)
            .on_click(move |_, window, cx| on_click(window, cx))
    }

    /// 一个可选项（主题名、目录、浅深模式）。选中那一项由 `selected` 画，
    /// 不额外加勾 —— 设计指南的立场是选中态靠那一格自己的表面与文字重量表达。
    fn choice(
        &self,
        id: String,
        label: SharedString,
        selected: bool,
        on_pick: impl Fn(&mut AccountView, &mut Context<AccountView>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity().clone();
        ListItem::new(id)
            .w_full()
            .rounded_md()
            .selected(selected)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(div().min_w_0().child(label.clone())),
            )
            .accessibility_label(label)
            .on_click(move |_, _, cx| {
                entity.update(cx, |this, cx| on_pick(this, cx));
            })
    }

    fn section_title(text: SharedString, cx: &gpui_kit::App) -> impl IntoElement {
        div()
            .w_full()
            .pt_3()
            .pb_1()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }

    /// 子屏顶部那一条：返回 + 标题。返回回到入口列表，和正文那条返回同一个道理。
    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        let label = tr(cx, self.screen.title_key());
        let back_label = tr(cx, "shelf.back");
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_3()
            .h(px(44.0))
            .child(
                Button::new("account-back")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowLeft)
                    .label(back_label.clone().to_string())
                    .accessibility_label(back_label)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| this.go(Screen::Root, cx))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(label),
            )
    }

    /// 主题屏：浅深怎么选 + 两侧各用哪一套主题。候选来自框架的主题集，
    /// 本应用不定义任何颜色值（见 `ui/shared/theme.rs`）。
    fn theme_screen(&self, cx: &mut Context<Self>) -> AnyElement {
        let appearance = ConfigHolder::appearance(cx);
        let modes = [
            (Mode::System, tr(cx, "appearance.follow_system")),
            (Mode::Light, tr(cx, "appearance.light")),
            (Mode::Dark, tr(cx, "appearance.dark")),
        ];
        let mut body = v_flex().w_full().gap_1();
        body = body.child(Self::section_title(tr(cx, "field.appearance_mode"), cx));
        for (mode, label) in modes {
            let picked = appearance.mode == mode;
            body = body.child(self.choice(
                format!("mode-{}", mode.as_str()),
                label,
                picked,
                move |this, cx| {
                    let _ = this;
                    ConfigHolder::edit_appearance(cx, |a| a.mode = mode);
                },
                cx,
            ));
        }
        for (mode, key, chosen) in [
            (
                gpui_kit::component::ThemeMode::Light,
                "field.theme_light",
                appearance.light.clone(),
            ),
            (
                gpui_kit::component::ThemeMode::Dark,
                "field.theme_dark",
                appearance.dark.clone(),
            ),
        ] {
            body = body.child(Self::section_title(tr(cx, key), cx));
            for name in theme::theme_names(cx, mode) {
                let picked =
                    chosen.as_deref().unwrap_or(theme::default_name(mode)) == name.as_ref();
                let picked_name = name.clone();
                body = body.child(self.choice(
                    format!("theme-{mode:?}-{name}"),
                    name,
                    picked,
                    move |_, cx| {
                        let name = picked_name.to_string();
                        ConfigHolder::edit_appearance(cx, move |a| {
                            let slot = match mode {
                                gpui_kit::component::ThemeMode::Light => &mut a.light,
                                gpui_kit::component::ThemeMode::Dark => &mut a.dark,
                            };
                            *slot = Some(name);
                        });
                    },
                    cx,
                ));
            }
        }
        Self::scrollable(body)
    }

    /// 书库屏：从**可写的候选**里挑一个。
    ///
    /// 这里给的是候选而不是文件夹选择器：Android 的 SAF 选目录返回 tree URI
    /// （`content://…`）而不是文件系统路径，拷副本用的 `std::fs` 用不了它
    /// （见 `shared::library::candidate_dirs`）。
    fn library_screen(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = ConfigHolder::library(cx);
        let mut body = v_flex()
            .w_full()
            .gap_1()
            .child(muted_label(tr(cx, "account.library_hint"), cx));
        for dir in candidate_dirs() {
            let path = dir.display().to_string();
            let picked = current.dir == path;
            body = body.child(self.choice(
                format!("lib-{path}"),
                SharedString::from(path.clone()),
                picked,
                move |_, cx| {
                    ConfigHolder::edit_library(cx, |library| library.dir = path.clone());
                },
                cx,
            ));
        }
        if !current.is_confirmed() {
            body = body.child(muted_label(tr(cx, "account.library_unconfirmed"), cx));
        }
        body = body.child(muted_label(
            format!(
                "{} → {}",
                tr(cx, "account.library_default"),
                suggested_dir().display()
            ),
            cx,
        ));
        Self::scrollable(body)
    }

    /// 关于屏：这份二进制是用哪几版依赖编的，以及两份讲实际情况的文档的入口。
    fn about_screen(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = v_flex().w_full().gap_1();
        for (key, value) in [
            ("about.version", about::version()),
            ("about.gpui_pre", about::gpui_pre()),
            ("about.kit", about::kit()),
            ("about.mobile", about::mobile_layer()),
        ] {
            body = body
                .child(Self::section_title(tr(cx, key), cx))
                .child(div().w_full().child(value));
        }
        body = body
            .child(hairline(cx).into_any_element())
            .child(self.row(
                "about-plan",
                IconName::BookOpen,
                tr(cx, "about.plan"),
                Screen::Plan,
                cx,
            ))
            .child(self.row(
                "about-limits",
                IconName::TriangleAlert,
                tr(cx, "about.limitations"),
                Screen::Limitations,
                cx,
            ))
            .child(muted_label(tr(cx, "about.docs_note"), cx));
        Self::scrollable(body)
    }

    /// 一份嵌进来的文档全文。按行摊开而不是整篇一个文字元素：整篇会让这一格变成一个
    /// 比屏幕高几十倍的元素，滚动范围与裁剪都不好算。
    fn doc_screen(text: &'static str) -> AnyElement {
        let body = v_flex().w_full().gap_0().children(text.lines().map(|line| {
            div()
                .w_full()
                .text_sm()
                .child(if line.is_empty() { " " } else { line })
        }));
        Self::scrollable(body)
    }

    /// 入口列表：主题、书库位置、关于。
    ///
    /// 一行一个 `let`，而不是把三行都塞进同一条链子：`row` 要拿 `cx` 去取实体句柄，
    /// 一条链里连着调会把 `cx` 借出两次。
    fn root_screen(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = v_flex().w_full().gap_1();
        body = body.child(Self::section_title(tr(cx, "account.settings"), cx));
        body = body.child(self.row(
            "account-theme",
            IconName::Palette,
            tr(cx, "account.theme"),
            Screen::Theme,
            cx,
        ));
        body = body.child(self.row(
            "account-library",
            IconName::FolderOpen,
            tr(cx, "account.library"),
            Screen::Library,
            cx,
        ));
        body = body.child(self.row(
            "account-about",
            IconName::Info,
            tr(cx, "account.about"),
            Screen::About,
            cx,
        ));
        // 桌面那一整页字段（版面、字体、语言）住在**独立窗口**里：448px 的对话框装不下
        // 侧栏 + 正文两栏（见 `settings::window` 的注释）。手机上开不了第二个窗口，
        // 这一格就不长出来 —— 留一个点了没反应的入口比没有更糟。
        if host::supports_extra_windows(cx) {
            body = body.child(self.action_row(
                "account-reading-settings",
                IconName::Settings,
                tr(cx, "account.reading_settings"),
                |_, cx| super::open(cx),
                cx,
            ));
        }
        Self::scrollable(body)
    }

    /// 内容区的通用外壳：可滚动 + 有边距。不接 `cx` 是因为调用方要在同一处继续用它
    /// 造里面的行，多传一个可变引用过去只是把自己绊倒。
    fn scrollable(body: impl IntoElement) -> AnyElement {
        div()
            .id("account-scroll")
            .overflow_y_scroll()
            .flex_1()
            .min_h_0()
            .px(px(GUTTER))
            .pb(px(GUTTER))
            .child(body)
            .into_any_element()
    }
}

impl Render for AccountView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = v_flex().id("folio-account").size_full();
        if self.screen != Screen::Root {
            root = root.child(self.header(cx)).child(hairline(cx));
        }
        let body: AnyElement = match self.screen {
            Screen::Root => self.root_screen(cx),
            Screen::Theme => self.theme_screen(cx),
            Screen::Library => self.library_screen(cx),
            Screen::About => self.about_screen(cx),
            Screen::Plan => Self::doc_screen(about::PLAN),
            Screen::Limitations => Self::doc_screen(about::LIMITATIONS),
        };
        root.child(body)
    }
}
