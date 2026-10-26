//! 阅读页上那几条**界面带**：顶栏、底栏、状态行、进度行、入口行、听书那颗圆钮。
//!
//! 它们都挂在 `ReadingPage` 这一屏上，读的是同一个会话的状态；拆出来是为了让
//! `reading_page.rs` 只剩「什么时候挂哪一条」。每条自己不带状态 —— 顶栏底栏开没开在
//! `chrome` 里判，两块弹层开没开在它们自己的实体里判，这里只画。

use crate::ui::components::TopBar;
use crate::ui::features::reader::model::chrome::{self, PageMode};

use crate::ui::features::reader::view::bands;
use crate::ui::services::l10n::tr;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::slider::Slider;
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, StyledExt as _, ThemeMode, ThemeStyled as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Corners, InteractiveElement, IntoElement, ParentElement, Styled,
    TestSupportExt as _, div, px,
};

use super::reading_page::ReadingPage;

impl ReadingPage {
    /// 顶栏：返回在左，右侧是书签与宽档的目录开关。
    ///
    /// 书名与章名不摆在这一条上 —— 阅读时它们不改变下一步动作，只占掉返回与右侧开关
    /// 之间的宽度。章名在干净态页眉那条小字里仍然念得到（`bands::header`）。
    ///
    /// `wide` 决定目录开关长不长在这一条上：手机档的目录是底部弹层，入口在底栏；平板 / PC
    /// 档的目录是盖在正文上的抽屉，没有常驻的栏可以挂关闭键，开关就长在这里。
    pub(in crate::ui::features::reader) fn top_bar(
        &self,
        wide: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let page = cx.entity().clone();
        // 书签开关两端都在：钉的是「正在读的这一处」，与目录、设置那类入口不同类 ——
        // 它是有状态的开关（钉着 / 没钉），所以吃 `selected`。判据在会话里
        // （`position_bookmarked`：当前位置落在某枚书签的区间里就算钉着）。
        let bookmarked = self.reader.read(cx).position_bookmarked();
        let bookmark_page = cx.entity().clone();
        let bookmark_label = tr(cx, "reader.bookmark");
        let bookmark = Button::new("bookmark-toggle")
            .ghost()
            .small()
            .selected(bookmarked)
            .icon(if bookmarked {
                IconName::BookmarkCheck
            } else {
                IconName::Bookmark
            })
            .tooltip(bookmark_label.clone().to_string())
            .accessibility_label(bookmark_label)
            .on_click(move |_, _, cx| bookmark_page.update(cx, |this, cx| this.toggle_bookmark(cx)))
            .into_any_element();
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
            .back_tooltip(tr(cx, "shelf.back"))
            .on_back(move |_, cx| {
                page.update(cx, |this, cx| this.pop(cx));
            })
            .children(toggle)
            .child(bookmark)
            .into_any_element()
    }

    /// 底栏：一块卡片，从上到下是「状态行 + 进度条 + 一排入口」。
    ///
    /// 这块卡片是常驻的三行，不再跟着展开的内容长高：从前「设置」那一格就地展开一块面板，
    /// 底栏因此有两张脸，而进度条与入口排在最底下，面板一展开就把它们顶到读者刚调完的那
    /// 几格下面。两块带档位的内容（目录、阅读设置）现在都是盖住整屏的底部弹层。
    pub(in crate::ui::features::reader) fn bottom_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let radius = cx.theme().radius_lg;
        let card = v_flex()
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
            })
            .child(self.status_row(cx))
            .child(self.progress_row(cx))
            .child(self.entry_row(cx));
        // 听书那颗圆钮挂在卡片上、压着卡片上缘的右端（`speech_fab` 自己绝对定位）。
        // 它不占入口排那一行的位置：听书是开关，不是入口 —— 一排里再挤一格图标，
        // 手机上每格就被挤到按不准。
        let card = match self.speech_fab(cx) {
            Some(fab) => card.child(fab),
            None => card,
        };
        card
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

    /// 一排入口。两块带档位的（目录、设置）各是一块盖住整屏的弹层，选中态读弹层自己的状态；
    /// 夜间是当场生效的动作，双页是当场换摆法 —— 四件事不同，所以只有带档位的那三格吃 `selected`。
    ///
    /// 「排版」那一格早在 2026-10-16 并进了设置那块面板；那一块面板现在又是底部弹层
    /// （见 [`ReadingSheet`](crate::ui::features::reader::components::reading_sheet::ReadingSheet)），
    /// 行距、段距、版心、字体家族都搬了进去，所以这一排不再有「更多设置」那一格。
    /// 听书在卡片上方那颗圆钮（[`Self::speech_fab`]）。
    pub(in crate::ui::features::reader) fn entry_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let toc_open = self.toc.read(cx).is_open(cx);
        let settings_open = self.settings.read(cx).is_open(cx);
        let dark = cx.theme().mode == ThemeMode::Dark;
        // 「双页」这一格只管单页 ⇄ 双页，连续滚动不归它 —— 那一档现在只有「翻页方式」
        // 那条列表里「上下滚动」这一格进得去（`chrome::reconcile`）。所以读者选了上下
        // 滚动时这一格整个不摆：摆一颗按下去只会把书从滚动里踢回分页的键，读起来是两
        // 个控件在抢同一件事。
        let spread_mode = self.reader.read(cx).wanted_mode();
        // 「放大整页」那一格**只在定版式那一族摆**：那一族才有「这一页就是一张图」这件
        // 事，可重排那一族的插图另有入口（按宽度退档那一格在正文里，不在这一排）。
        let fixed = self.reader.read(cx).fixed().is_some();
        // 定版式没有「一屏两页」：它的页是既定的一张位图，双页档对它是无意义的。
        // 连续竖滚那一档由「翻页方式 → 上下滚动」进入，这一格同样不摆。
        let spread_shown = spread_mode != PageMode::Continuous && !fixed;
        // 选中态读**读者选的那一档**而不是生效档：手机上按下去会先亮双页再被宽度退成
        // 分页，读按钮的人要看到自己按着了。
        let spread = spread_mode == PageMode::Spread;
        let page_toc = cx.entity().clone();
        let page_night = cx.entity().clone();
        let page_spread = cx.entity().clone();
        let page_settings = cx.entity().clone();
        let night_label = tr(cx, if dark { "reader.day" } else { "reader.night" });
        let spread_label = tr(cx, "reader.mode_spread");
        let zoom_label = tr(cx, "reader.zoom_page");
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
            // 「放大整页」只在定版式那一族摆（判据在上面那一格 `fixed`）。
            //
            // 为什么这一格现在才需要：那一族的「看大图」原先挂在**长按**上，而那一族的
            // 字如今住在同一页里 —— 长按要留给选中（与可重排正文同一条规矩：有字的地方
            // 长按就是挑字），于是那一击不能再拿去看图。入口得有个不跟选中抢的地方，
            // 而底栏这一排正是读者已经在找功能的位置。
            //
            // 只给图标，与「双页」那一格同一条理由：一排几格各带两个字，手机上每格会被
            // 挤到点不准。
            .when(fixed, |row| {
                let zoom_reader = self.reader.clone();
                let zoom_viewer = self.image_viewer.clone();
                row.child(
                    Button::new("entry-zoom-page")
                        .ghost()
                        .small()
                        .flex_1()
                        .justify_center()
                        .icon(IconName::Maximize)
                        .tooltip(zoom_label.clone().to_string())
                        .accessibility_label(zoom_label)
                        .on_click(move |_, window, cx| {
                            // 键按**按下这一刻**问：从渲染到这一击之间读者可能已经翻过页，
                            // 把键烘进闭包会开成上一页的那一张。
                            let key = zoom_reader.read_with(cx, |session, _| {
                                session.fixed().and_then(|book| book.current_page_key())
                            });
                            let reader = zoom_reader.downgrade();
                            let Some(key) = key else {
                                tracing::debug!(
                                    "放大整页: 这一页取不到那张图的键，这一击什么都不做"
                                );
                                return;
                            };
                            crate::ui::features::reader::view::body::open_picture(
                                &zoom_viewer,
                                &reader,
                                &key,
                                window,
                                cx,
                            );
                        }),
                )
            })
            .when(spread_shown, |row| {
                row.child(
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
                        .accessibility_label(spread_label.clone())
                        .on_click(move |_, _, cx| {
                            page_spread.update(cx, |this, cx| this.toggle_spread(cx))
                        }),
                )
            })
            // 听书不在这一排：它是一颗浮在卡片上方右端的圆钮（见 `speech_fab`）。
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
            // 设置放最右：翻页、夜间这些当场生效的开关留在中间，设置单独占一端。
            .child(
                Button::new("entry-settings")
                    .ghost()
                    .small()
                    .flex_1()
                    .justify_center()
                    .selected(settings_open)
                    .icon(IconName::Settings)
                    .label(tr(cx, "account.settings").to_string())
                    .on_click(move |_, _, cx| {
                        page_settings.update(cx, |this, cx| this.toggle_settings(cx))
                    }),
            )
            .into_any_element()
    }

    /// 听书那颗圆钮：浮在底栏卡片**上方的右端**。
    ///
    /// 原来它只是入口排里的一格图标。那一排现在有四格（目录 / 双页 / 夜间 / 设置），
    /// 再挤一格在手机上每格就被挤到按不准；而听书与它们不同类 —— 它是有状态的开关
    /// （念着 / 停了），圆钮比一排里的图标更像开关。位置贴着底栏右缘、压在卡片上方的
    /// 正文上：拇指从右下角自然够得到，也不占卡片里已经排满的那一行。
    ///
    /// 摆不摆的判据仍在会话侧（`speech_entry_visible`：引擎不可用 / 定版式书 /
    /// 还没探到 → 不摆），与原来那一格同一个。
    pub(in crate::ui::features::reader) fn speech_fab(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.reader.read(cx).speech_entry_visible() {
            return None;
        }
        let playing = self.reader.read(cx).speech_playing();
        let label = tr(cx, "reader.tts");
        let page = cx.entity().clone();
        // 圆卡的面用 `button_secondary` 那一档，不用 `popover`。
        //
        // 原因是量出来的：夜读的正文底与底栏卡片都是 `neutral-950`（(10,10,10)），
        // 而 `popover` 在这套主题里**也是那个值** —— 第一版拿它做钮面，真机上圆钮
        // 整颗看不见，只剩正文右缘那片空白（截图里钮所在的那 52×52 一格与页边距
        // 同一个颜色）。`button_secondary` 是 `neutral-800` 量级，浅色主题下同样
        // 比正文底深一档，两套主题都看得见，且它自带前景 / hover / active 三档。
        let (surface, foreground, hover, active, radius) = {
            let theme = cx.theme();
            (
                theme.button_secondary,
                theme.button_secondary_foreground,
                theme.button_secondary_hover,
                theme.button_secondary_active,
                theme.radius_full(),
            )
        };
        let style = ButtonCustomVariant::new(cx)
            .color(surface)
            .foreground(foreground)
            .hover(hover)
            .active(active)
            .shadow(true);
        Some(
            div()
                .absolute()
                .right(px(16.))
                // 钮高 52，再留 12 的缝：整颗都在卡片上缘之外，不压住状态行。
                .top(px(-64.))
                .child(
                    Button::new("entry-speech")
                        .custom(style)
                        .rounded(radius)
                        .w(px(52.))
                        .h(px(52.))
                        .p_0()
                        .justify_center()
                        .icon(if playing {
                            IconName::Pause
                        } else {
                            IconName::BookHeadphones
                        })
                        .tooltip(label.clone().to_string())
                        .accessibility_label(label)
                        .on_click(move |_, _, cx| {
                            page.update(cx, |this, cx| this.toggle_speech(cx))
                        }),
                )
                .into_any_element(),
        )
    }
}
