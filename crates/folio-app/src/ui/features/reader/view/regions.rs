//! 阅读页上那几条**界面带**：顶栏、底栏、状态行、进度行、入口行、设置面板、翻页那一格。
//!
//! 它们都挂在 `ReadingPage` 这一屏上，读的是同一个会话的状态；拆出来是为了让
//! `reading_page.rs` 只剩「什么时候挂哪一条」。每条自己不带状态 —— 开没开在
//! `chrome` 里判，这里只画。

use crate::ui::components::TopBar;
use crate::ui::features::reader::model::chrome::{self, PageMode, Panel};
use crate::ui::features::reader::motion::turn_animation;
use crate::ui::features::reader::view::bands;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::tr;
use folio_core::infra::config::PageTurn;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::slider::Slider;
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, StyledExt as _, ThemeMode, ThemeStyled as _,
    h_flex, v_flex,
};
use gpui_kit::{
    Anchor, AnyElement, Context, Corners, InteractiveElement, IntoElement, ParentElement,
    SharedString, Styled, TestSupportExt as _, div, px,
};

use super::reading_page::ReadingPage;

impl ReadingPage {
    /// 顶栏：返回 + 书名，右边是这一章的章名。那条本身的样子（高、内缩、popover 表面、
    /// 只圆下角、拦住落在它空隙里的一击）都在 [`TopBar`] 里，与书信息页、紧凑设置那一屏
    /// 共用一份；高从 [`chrome::TOP_BAR_HEIGHT`] 递进去，那个数同时管布局与画面。
    ///
    /// 右边不摆页码 —— 底栏那条状态行已经念过一遍，而「读到哪一章了」是两栏都想一眼看到的。
    /// `wide` 决定目录开关长不长在这一条上：手机档的目录是底部弹层，入口在底栏；平板 / PC
    /// 档的目录是盖在正文上的抽屉，没有常驻的栏可以挂关闭键，开关就长在这里。
    pub(in crate::ui::features::reader) fn top_bar(
        &self,
        wide: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = self.reader.read(cx).title();
        let chapter: SharedString = self.reader.read(cx).chapter_title().into();
        let page = cx.entity().clone();
        // 宽档上目录开关长在顶栏：底栏浮层收起时读者常驻看见的是这一条，而抽屉本身
        // 也带一颗关闭 —— 开的关的两个入口都在这块屏上。
        let toggle = wide.then(|| {
            let open = self.toc.read(cx).is_open(cx);
            let toc_page = cx.entity().clone();
            let label = tr(cx, "reader.toc");
            Button::new("toc-toggle")
                .ghost()
                .small()
                .selected(open)
                .icon(IconName::ListTree)
                .tooltip(label.clone().to_string())
                .accessibility_label(label)
                .on_click(move |_, _, cx| {
                    toc_page.update(cx, |this, cx| this.toggle_toc(cx));
                })
                .into_any_element()
        });
        TopBar::new("reader-top-bar")
            .overlay(self.top_inset)
            .height(chrome::TOP_BAR_HEIGHT)
            .title(title)
            .subtitle(chapter)
            .back_tooltip(tr(cx, "shelf.back"))
            .on_back(move |_, cx| {
                page.update(cx, |this, cx| this.pop(cx));
            })
            .children(toggle)
            .into_any_element()
    }

    /// 底栏：一块卡片，从上到下是「面板（可有可无）+ 状态行 + 进度条 + 一排入口」。
    ///
    /// 面板和入口条在**同一块**表面里，不是两张卡片：两张会各有一套圆角与投影，
    /// 接缝处读起来是两个东西叠着，而它们说的是同一件事。
    pub(in crate::ui::features::reader) fn bottom_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let radius = cx.theme().radius_lg;
        let mut card = v_flex()
            .id("reader-bottom-bar")
            // 底栏与顶栏同理：正文的 hitbox 是整屏的，不拦住空隙里的一击，它会顺着
            // 帧内的监听列表接着打到正文上，把刚切过去的面板当场收掉。滚动留着，
            // 读者从底栏那一片起手也要能滚字。
            .block_mouse_except_scroll()
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .px_4()
            .pt_3()
            // 底下那条系统手势条那一段也归卡片：卡片一直铺到屏幕底，内容抬高
            // `bottom_inset`（多出来的那一段是卡片自己的底色，不是一条缝）。
            .pb(px(8.0 + self.bottom_inset))
            .gap_3()
            .popover_style(cx)
            .corner_radii(Corners {
                top_left: radius,
                top_right: radius,
                bottom_left: px(0.),
                bottom_right: px(0.),
            });
        card = match self.chrome.panel() {
            Panel::None => card,
            // 目录不在这块卡片里：它盖的是整块窗口，那一屏自己的实体在 [`TocPanel`]。
            // 阅读设置留在这里：两条滑条加一行下拉，抬着正文看一眼再放回去正合适。
            Panel::Reading => card.child(self.reading_panel(cx)),
        };
        card.child(self.status_row(cx))
            .child(self.progress_row(cx))
            .child(self.entry_row(cx))
            // 宿主门：阅读层整层 occlude 之后，正文自己的点击必须还能把它叫出来。
            .test_support()
            .into_any_element()
    }

    /// 状态行：章内位置与进度各占一端的边。分页 / 双页时左边那半念「第几页 / 共几页」，
    /// 连续滚动念「第几节 / 共几节」—— 读者认的是自己那一档的刻度。
    pub(in crate::ui::features::reader) fn status_row(&self, cx: &mut Context<Self>) -> AnyElement {
        // 与干净态那条页脚共用 `bands::readouts`：两处念的必须是同一份数。
        let bands::Readouts {
            position, percent, ..
        } = bands::readouts(&self.reader, cx);
        let progress_label = format!("{} {}", tr(cx, "reader.progress"), percent);
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(position),
            )
            .child(div().text_sm().child(progress_label))
            .into_any_element()
    }

    /// 进度条两端是上一章 / 下一章：手指在条上既能「跳到这一章的哪儿」，
    /// 也能顺手跨到隔壁一章。
    pub(in crate::ui::features::reader) fn progress_row(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reader = self.reader.read(cx);
        // 箭头仍然只指「往哪边走」，翻书方向跟着整本走：从右往左读的书里，左边那颗
        // 按下去是**下一章**（图标不用换 —— 它指左，而左就是阅读的前方），要换的是
        // 动作、标签和可不可用三样。
        let rtl = reader.progression_rtl();
        let (left_delta, right_delta) = if rtl {
            (1isize, -1isize)
        } else {
            (-1isize, 1isize)
        };
        let can_left = reader.can_advance(left_delta);
        let can_right = reader.can_advance(right_delta);
        // 两个闭包各自要一份句柄（`Entity` 不 `Copy`），所以左边右边各克隆一次。
        let page_left = cx.entity().clone();
        let page_right = cx.entity().clone();
        let prev_label = tr(cx, "reader.prev_chapter");
        let next_label = tr(cx, "reader.next_chapter");
        let (left_label, right_label) = if left_delta > 0 {
            (next_label.clone(), prev_label.clone())
        } else {
            (prev_label.clone(), next_label.clone())
        };
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .child(
                Button::new("prev-chapter")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronLeft)
                    .tooltip(left_label.to_string())
                    .accessibility_label(left_label)
                    .disabled(!can_left)
                    .on_click(move |_, _, cx| {
                        page_left.update(cx, |this, cx| this.advance(left_delta, cx))
                    }),
            )
            .child(Slider::new(&self.progress).flex_1().min_w_0())
            .child(
                Button::new("next-chapter")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronRight)
                    .tooltip(right_label.to_string())
                    .accessibility_label(right_label)
                    .disabled(!can_right)
                    .on_click(move |_, _, cx| {
                        page_right.update(cx, |this, cx| this.advance(right_delta, cx))
                    }),
            )
            .into_any_element()
    }

    /// 一排入口。目录是一块弹层（选中态读弹层自己的状态），设置是底栏卡片里的面板，夜间
    /// 是当场生效的动作，双页是当场换摆法 —— 四件事不同，所以只有带档位的那三格吃 `selected`。
    ///
    /// 「排版」那一格并进了设置面板（亮度 / 字号 / 翻页方式都在那块里），入口因此从五格减到
    /// 四格；完整那一屏（行距、段距、版心、字体）从面板右下角那颗进去，也与目录那屏的 ⋯
    /// 同一条路。
    pub(in crate::ui::features::reader) fn entry_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.chrome.panel();
        let toc_open = self.toc.read(cx).is_open(cx);
        let dark = cx.theme().mode == ThemeMode::Dark;
        let spread = self.reader.read(cx).wanted_mode() != PageMode::Continuous;
        let page_toc = cx.entity().clone();
        let page_night = cx.entity().clone();
        let page_spread = cx.entity().clone();
        let page_settings = cx.entity().clone();
        let night_label = tr(cx, if dark { "reader.day" } else { "reader.night" });
        let spread_label = tr(cx, "reader.mode_spread");
        h_flex()
            .w_full()
            .items_center()
            .child(
                Button::new("entry-toc")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .selected(toc_open)
                    .icon(IconName::ListTree)
                    .label(tr(cx, "reader.toc").to_string())
                    .on_click(move |_, _, cx| page_toc.update(cx, |this, cx| this.toggle_toc(cx))),
            )
            .child(
                Button::new("entry-settings")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .selected(panel == Panel::Reading)
                    .icon(IconName::Settings)
                    .label(tr(cx, "account.settings").to_string())
                    .on_click(move |_, _, cx| {
                        page_settings.update(cx, |this, cx| this.pick(Panel::Reading, cx))
                    }),
            )
            .child(
                // 这一格只给图标：一排几格入口再各带两个字，手机上每格会被挤到点不准。
                // 双页按宽度退档，所以这格在窄屏上也仍然按得下去（按下去就是分页）。
                Button::new("entry-spread")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .selected(spread)
                    .icon(IconName::Columns3)
                    .tooltip(spread_label.clone().to_string())
                    .accessibility_label(spread_label)
                    .on_click(move |_, _, cx| {
                        page_spread.update(cx, |this, cx| this.toggle_spread(cx))
                    }),
            )
            .child(
                Button::new("entry-night")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .icon(if dark {
                        IconName::SunMedium
                    } else {
                        IconName::Moon
                    })
                    .label(night_label.to_string())
                    .on_click(move |_, _, cx| {
                        page_night.update(cx, |this, cx| this.toggle_night(cx))
                    }),
            )
            .into_any_element()
    }

    /// 阅读设置面板：亮度、字号两条滑条，最后一行左边是翻页方式、右边是「更多设置」。
    /// 标签在左、当前值在右、滑条在下面 —— 手机上横着排三样会让滑条只剩一半宽，
    /// 调一档要划好几下。
    ///
    /// 顺序照读者动它们的频度排：亮度一天里跟着光线走，字号换一本书动一次，翻页方式是
    /// 钉下来就不再碰的那一格。
    pub(in crate::ui::features::reader) fn reading_panel(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let prefs = ConfigHolder::reader(cx);
        // 「跟随系统」念成一句话而不是「100%」：那一档应用确实什么都没接管。
        let brightness_label = match ConfigHolder::appearance(cx).brightness {
            None => tr(cx, "appearance.follow_system").to_string(),
            Some(value) => format!("{}%", (value * 100.0).round()),
        };
        v_flex()
            .w_full()
            .gap_3()
            .child(self.slider_row(
                tr(cx, "reader.brightness").to_string(),
                brightness_label,
                &self.brightness,
                cx,
            ))
            .child(self.slider_row(
                tr(cx, "field.font_size").to_string(),
                format!("{:.1}", prefs.font_size),
                &self.font_size,
                cx,
            ))
            .child(self.turn_row(cx))
            .into_any_element()
    }

    /// 面板最后一行：左边那颗下拉选翻页怎么走，右边那颗把完整的设置屏压进来。
    ///
    /// 下拉**往上开**（`Anchor::BottomLeft`）：这一行长在底栏卡片里、贴在屏幕底，往下开的话
    /// 那两块菜单有一半掉到屏幕外，读者只点得到上面那一块。
    ///
    /// 选项按**当前摆法**筛过（[`turn_animation::applies`]）：连续里只有「上下滚动」与「不走
    /// 动画」讲得通，分页里「上下滚动」没有东西可滚。筛掉而不是灰着摆 —— 一排点不动的选项
    /// 在手机上读起来像坏了。
    ///
    /// 按钮上念的是 `label_of`（与列表同源、对每一档都有答案），不是「在列表里找一找」：
    /// 读者在连续里选了「上下滚动」再换成分页，那一档被筛掉了，但按钮不该因此退成一句
    /// 「翻页动画」—— 他选的东西还在配置里，只是眼下这一屏用不上。
    /// 右边那一格存在是因为底栏的「设置」不再压屏了：完整那一屏（行距、段距、版心、
    /// 字体家族）得在这一屏上有个入口。
    pub(in crate::ui::features::reader) fn turn_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.reader.read(cx).mode();
        let turn = ConfigHolder::appearance(cx).page_turn;
        let label_of = |turn| match turn {
            PageTurn::Slide => tr(cx, "page_turn.slide"),
            PageTurn::SlideUp => tr(cx, "page_turn.slide_up"),
            PageTurn::Cover => tr(cx, "page_turn.cover"),
            PageTurn::Curl => tr(cx, "page_turn.curl"),
            PageTurn::Scroll => tr(cx, "page_turn.scroll"),
            PageTurn::None => tr(cx, "page_turn.none"),
        };
        let items: Vec<(PageTurn, SharedString)> = PageTurn::ALL
            .iter()
            .copied()
            .filter(|value| turn_animation::applies(*value, mode))
            .map(|value| (value, label_of(value)))
            .collect();
        let current = label_of(turn);
        let page_menu = cx.entity().clone();
        let page_more = cx.entity().clone();
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                Button::new("reader-page-turn")
                    .outline()
                    .small()
                    .label(current.to_string())
                    .dropdown_caret(true)
                    .accessibility_label(tr(cx, "field.page_turn").to_string())
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                        items.iter().fold(menu, |menu, (value, label)| {
                            let checked = *value == turn;
                            let value = *value;
                            // 菜单闭包是 `Fn`，每一项的回调要各自一份句柄（`Entity` 不 `Copy`）。
                            let page = page_menu.clone();
                            menu.item(PopupMenuItem::new(label.clone()).checked(checked).on_click(
                                move |_, _, cx| {
                                    page.update(cx, |this, cx| this.set_page_turn(value, cx))
                                },
                            ))
                        })
                    }),
            )
            .child(div().flex_1())
            .child(
                Button::new("reader-more-settings")
                    .ghost()
                    .small()
                    .icon(IconName::Settings)
                    .label(tr(cx, "reader.more_settings").to_string())
                    .on_click(move |_, _, cx| {
                        page_more.update(cx, |this, cx| this.open_settings(cx))
                    }),
            )
            .into_any_element()
    }
}
