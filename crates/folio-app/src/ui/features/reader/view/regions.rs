//! 阅读页上那几条**界面带**：顶栏、底栏、状态行、进度行、入口行、听书与自动那两颗浮钮。
//!
//! 它们都挂在 `ReadingPage` 这一屏上，读的是同一个会话的状态；拆出来是为了让
//! `reading_page.rs` 只剩「什么时候挂哪一条」。每条自己不带状态 —— 顶栏底栏开没开在
//! `chrome` 里判，两块弹层开没开在它们自己的实体里判，这里只画。

use crate::ui::components::TopBar;
use crate::ui::features::reader::model::chrome;

use crate::ui::features::reader::view::bands;
use crate::ui::services::l10n::{tf, tr};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::slider::Slider;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Selectable, Sizable, StyledExt as _, ThemeMode,
    ThemeStyled as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Corners, Hsla, InteractiveElement, IntoElement, ParentElement,
    SharedString, Styled, TestSupportExt as _, div, px,
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
        // 听书与「自动」都挂在卡片上方：听书是开关、自动也是开关，两颗并成一排贴在
        // 卡片上缘的右端（`listen_fabs` 自己绝对定位）。它们不占入口排那一行的位置
        // —— 一排里再挤两格，手机上每格就被挤到按不准。
        let card = card.child(self.listen_fabs(cx));
        card
            // 宿主门：阅读层整层 occlude 之后，正文自己的点击必须还能把它叫出来。
            .test_support()
            .into_any_element()
    }

    /// 自动阅读跑着时**顶替底栏**的那一条：速度的一格加一颗退出。
    ///
    /// 自动自己会翻页，这一层露着时读者要做的只有两件事 —— 快一点 / 慢一点，或者
    /// 不自动了。所以这一条不摆目录、夜间那些入口：退出自动，原来那条底栏就回来。
    /// 速度那一格与阅读设置里「自动前进」是同一副形状（标签在左、读数在右，下面
    /// `−` 滑条 `+`），同一份量程与步长（[`chrome::AUTO_ADVANCE_RANGE`] /
    /// [`chrome::AUTO_ADVANCE_STEPS`]），不另起一套。
    ///
    /// 卡片壳与底栏完全同款（贴底、铺满、上圆角、让开系统手势条）：换的只是内容，
    /// 不是又浮起来一张小卡。
    pub(in crate::ui::features::reader) fn auto_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let radius = cx.theme().radius_lg;
        let speed = crate::ui::services::config::ConfigHolder::appearance(cx).auto_advance_speed;
        let (min, max) = chrome::AUTO_ADVANCE_RANGE;
        let title = tr(cx, "reader.auto_advance");
        let value: SharedString = tf(
            cx,
            "reader.auto_advance.value",
            &[("n", &format!("{speed:.0}"))],
        )
        .into();
        let down = tf(cx, "reader.nudge_down", &[("field", &title)]);
        let up = tf(cx, "reader.nudge_up", &[("field", &title)]);
        let exit_label = tr(cx, "reader.auto_exit");
        let page_down = cx.entity().clone();
        let page_up = cx.entity().clone();
        let page_exit = cx.entity().clone();
        v_flex()
            .id("reader-auto-bar")
            // 与底栏同理：正文的 hitbox 是整屏的，不拦住空隙里的一击，它会顺着打到
            // 正文上把刚叫出来的这一条当场收掉。滚动留着。
            .block_mouse_except_scroll()
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .px_4()
            .pt_3()
            .pb(px(8.0 + self.bottom_inset))
            .gap_3()
            .popover_style(cx)
            .corner_radii(Corners {
                top_left: radius,
                top_right: radius,
                bottom_left: px(0.),
                bottom_right: px(0.),
            })
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .child(div().text_base().child(title.clone()))
                            .child(
                                div()
                                    .text_base()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(value),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("auto-bar-slower")
                                    .ghost()
                                    .large()
                                    // 到端就禁用：这一格到底了没有，读者要从控件本身读出来。
                                    .disabled(speed <= min + 0.001)
                                    .icon(IconName::Minus)
                                    .accessibility_label(down)
                                    .on_click(move |_, window, cx| {
                                        page_down.update(cx, |this, cx| {
                                            this.nudge_auto(
                                                -chrome::AUTO_ADVANCE_STEPS.nudge,
                                                window,
                                                cx,
                                            )
                                        })
                                    }),
                            )
                            .child(
                                Slider::new(&self.auto_speed)
                                    .flex_1()
                                    .min_w_0()
                                    .accessibility_label(title),
                            )
                            .child(
                                Button::new("auto-bar-faster")
                                    .ghost()
                                    .large()
                                    .disabled(speed >= max - 0.001)
                                    .icon(IconName::Plus)
                                    .accessibility_label(up)
                                    .on_click(move |_, window, cx| {
                                        page_up.update(cx, |this, cx| {
                                            this.nudge_auto(
                                                chrome::AUTO_ADVANCE_STEPS.nudge,
                                                window,
                                                cx,
                                            )
                                        })
                                    }),
                            ),
                    ),
            )
            .child(
                // 退出是这一条唯一的主操作，居中落在拇指自然够得到的位置，别贴右缘。
                h_flex().w_full().justify_center().child(
                    Button::new("auto-exit")
                        .outline()
                        .label(exit_label.to_string())
                        .accessibility_label(exit_label)
                        .on_click(move |_, _, cx| {
                            page_exit.update(cx, |this, cx| this.toggle_auto(cx))
                        }),
                ),
            )
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
    /// 夜间是当场生效的动作 —— 三件事不同，所以只有带档位的那两格吃 `selected`。
    ///
    /// 「排版」那一格早在 2026-10-16 并进了设置那块面板；那一块面板现在又是底部弹层
    /// （见 [`ReadingSheet`](crate::ui::features::reader::components::reading_sheet::ReadingSheet)），
    /// 行距、段距、版心、字体家族都搬了进去，所以这一排不再有「更多设置」那一格。
    /// 「双页」那一格这一轮也搬了进去，落在设置里「分页」那一行 —— 那一排每多一格，手机上
    /// 每一格就更窄一分，而换摆法不是翻几页就要碰一次的动作。
    /// 听书与「自动」都在卡片上方的浮钮里（[`Self::listen_fabs`]）。
    pub(in crate::ui::features::reader) fn entry_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let toc_open = self.toc.read(cx).is_open(cx);
        let settings_open = self.settings.read(cx).is_open(cx);
        let dark = cx.theme().mode == ThemeMode::Dark;
        // 「放大整页」那一格**只在定版式那一族摆**：那一族才有「这一页就是一张图」这件
        // 事，可重排那一族的插图另有入口（按宽度退档那一格在正文里，不在这一排）。
        let fixed = self.reader.read(cx).fixed().is_some();
        let page_toc = cx.entity().clone();
        let page_night = cx.entity().clone();
        let page_settings = cx.entity().clone();
        let night_label = tr(cx, if dark { "reader.day" } else { "reader.night" });
        let zoom_label = tr(cx, "reader.zoom_page");
        // 图标与文字同一档墨色：ghost 按钮自己的前景色，`Icon` 不继承文字样式，
        // 所以显式递进去（两处同色才不会一个深一个浅）。
        let ink = cx.theme().secondary_foreground;
        h_flex()
            .w_full()
            .items_center()
            .child(
                Button::new("entry-toc")
                    .ghost()
                    .small()
                    .flex_1()
                    .h(px(52.))
                    .justify_center()
                    .selected(toc_open)
                    .accessibility_label(tr(cx, "reader.toc"))
                    .child(entry_cell(IconName::ListTree, tr(cx, "reader.toc"), ink))
                    .on_click(move |_, _, cx| page_toc.update(cx, |this, cx| this.toggle_toc(cx))),
            )
            // 「放大整页」只在定版式那一族摆（判据在上面那一格 `fixed`）。
            //
            // 为什么这一格现在才需要：那一族的「看大图」原先挂在**长按**上，而那一族的
            // 字如今住在同一页里 —— 长按要留给选中（与可重排正文同一条规矩：有字的地方
            // 长按就是挑字），于是那一击不能再拿去看图。入口得有个不跟选中抢的地方，
            // 而底栏这一排正是读者已经在找功能的位置。
            //
            // 这一格也走「图标在上、文字在下」：同排四格等高，各占等分宽；「放大整页」
            // 四个字在 320 宽的屏上也放得下（11px 一行约 44px）。
            .when(fixed, |row| {
                let zoom_reader = self.reader.clone();
                let zoom_viewer = self.image_viewer.clone();
                row.child(
                    Button::new("entry-zoom-page")
                        .ghost()
                        .small()
                        .flex_1()
                        .h(px(52.))
                        .justify_center()
                        .tooltip(zoom_label.clone().to_string())
                        .accessibility_label(zoom_label.clone())
                        .child(entry_cell(IconName::Maximize, zoom_label, ink))
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
                            crate::ui::features::reader::view::body::images::open_picture(
                                &zoom_viewer,
                                &reader,
                                &key,
                                window,
                                cx,
                            );
                        }),
                )
            })
            // 听书与「自动」都不在这一排：两颗浮在卡片上方右端（见 `listen_fabs`）。
            .child(
                Button::new("entry-night")
                    .ghost()
                    .small()
                    .flex_1()
                    .h(px(52.))
                    .justify_center()
                    .accessibility_label(night_label.clone())
                    .child(entry_cell(
                        if dark {
                            IconName::SunMedium
                        } else {
                            IconName::Moon
                        },
                        night_label.clone(),
                        ink,
                    ))
                    .on_click(move |_, _, cx| {
                        page_night.update(cx, |this, cx| this.toggle_night(cx))
                    }),
            )
            // 「自动」那一格提到卡片上方、听书圆钮的右侧（`listen_fabs`）：它和听书
            // 一样是**开关**，不是入口，放在那一排里既挤又和「翻到哪」的读数隔得远。
            // 设置放最右：翻页、夜间这些当场生效的开关留在中间，设置单独占一端。
            .child(
                Button::new("entry-settings")
                    .ghost()
                    .small()
                    .flex_1()
                    .h(px(52.))
                    .justify_center()
                    .selected(settings_open)
                    .accessibility_label(tr(cx, "account.settings"))
                    .child(entry_cell(
                        IconName::Settings,
                        tr(cx, "account.settings"),
                        ink,
                    ))
                    .on_click(move |_, _, cx| {
                        page_settings.update(cx, |this, cx| this.toggle_settings(cx))
                    }),
            )
            .into_any_element()
    }

    /// 卡片上方那两颗浮钮：听书在左，「自动」在右，同一副安静的纸白面孔。
    ///
    /// 听书原来只是入口排里的一格图标。那一排摆到四格时手机每格就按不准了，而听书是
    /// **开关**不是入口，于是提到卡片上缘。现在「自动」也走同一条判据 —— 它同样是开关
    /// （开着就一直往前翻），放进那一排既挤、又和「翻到哪」的读数隔着一整块屏。
    /// 两颗贴着右缘上方，读者从右下角一抬手就够得到。
    ///
    /// 两颗是同一类表面，共用一副底色：不透明的纸白外壳、墨色图标、一圈墨色细描边与
    /// xs 投影。底色本身不制造对比，靠描边 + 投影把圆片从纸上托起来；实心墨底只留给
    /// 「正在念」这一个状态，不是听书常驻的重量。参考界面的三颗圆钮就是这么处理的，
    /// 设计指南也要求同类表面同款处理、强调是有限预算。
    ///
    /// 底色必须画在 Button **外面**的一层不透明外壳上（[`Self::listen_fabs`] 里的
    /// `fab_shell`）：`ButtonCustomVariant` 会把底色混 20% 透明，直接给 Button 上色时
    /// 浅纸底压在正文上，黑字会透进圆片里。
    ///
    /// 听书的摆不摆仍在会话侧（`speech_entry_visible`：定版式书 / 还没开书 → 不摆），
    /// 会员门禁再叠一条；**自动不受这两条管** —— 它对每一本可读的书都成立，所以听书
    /// 缺席时它自己还在原位，只是往右补齐。
    ///
    /// 自动这颗只有「开自动」这一张面孔：自动跑着时整张卡片被 [`Self::auto_bar`] 顶替，
    /// 浮钮连同卡片一起不在场，退出那条路上有明确的出口，不必在这里再留一个态。
    pub(in crate::ui::features::reader) fn listen_fabs(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let radius = cx.theme().radius_full();
        let playing = self.reader.read(cx).speech_playing();
        let page_auto = cx.entity().clone();
        // 这两颗浮钮压在**纸**上（卡片上缘之外），底色跟正文的纸墨走，不跟壳主题：
        // 壳的 `primary` / `button_secondary` 在浅色与宣纸两套底上只比纸深一点，
        // 实机上就是两颗看不清的钮。纸墨是一对保证对比的颜色，换阅读主题时一起换，
        // 换壳主题时也不会再飘一层与纸无关的颜色上来。
        let paper = crate::ui::foundation::reading_theme::resolve(
            &crate::ui::services::config::ConfigHolder::appearance(cx),
            cx,
        );
        let (ink, sheet) = (paper.fg, paper.bg);
        // 静止时的同一副面孔：外壳画不透明纸白底，Button 只留墨色图标、一圈墨色细
        // 描边、xs 投影与 hover / active 两档。描边要留到真机上仍读得出是按钮（从前
        // `button_secondary` 就栽在淡到没有），又不能重到把圆片框成一枚徽章。
        let idle_style = ButtonCustomVariant::new(cx)
            .foreground(ink)
            .hover(ink.opacity(0.06))
            .active(ink.opacity(0.12))
            .shadow(true);
        // 「正在念」那一档：外壳填实墨、图标翻成纸色。强调是状态，不是常驻重量。
        // 听书屏一开这颗圆钮就被整窗层盖住，能看见实心底的只有快捷键起念、圆钮还
        // 露在底栏上这一种。
        let playing_style = ButtonCustomVariant::new(cx)
            .foreground(sheet)
            .hover(ink.opacity(0.88))
            .active(ink.opacity(0.78))
            .shadow(true);
        let fab_border = ink.opacity(0.30);
        // **底色由这层不透明外壳画，不能交给 Button**：gpui-kit 的
        // `ButtonCustomVariant::bg_color()`（`button.rs:1028`）把底色
        // `mix_oklab(transparent, 0.2)` —— custom 按钮的底永远只有八成不透明，纸白底
        // 压在正文上黑字会整个透上来（真机 2026-11-08）。外壳与 Button 同尺寸同圆角，
        // Button 的描边 / 投影 / 命中 / 图标 / hover-active 照旧叠在它上面。
        let fab_shell = |base: Hsla, child: AnyElement| {
            h_flex()
                .w(px(52.))
                .h(px(52.))
                .rounded(radius)
                .bg(base)
                .items_center()
                .justify_center()
                .child(child)
                .into_any_element()
        };
        let auto_label = tr(cx, "reader.auto");
        let mut group = h_flex()
            // **这一排要自己声明挡住身后的 hitbox**：它挂在卡片上缘之外，不在 card 的
            // bounds 里，card 那道 `block_mouse_except_scroll` 护不到。少了这一句，一击会
            // 同时落到正文的整屏 hitbox 上，右下角按 `chrome::zone` 判成「下一页」——
            // 点听书 / 自动会把书翻过去（真机 2026-11-08）。拦截只作用于比它更早绘制的
            // hitbox，两颗钮自己的点击、hover 与从这一片起手的滚动都不受影响。
            .block_mouse_except_scroll()
            // 钮高 52，再留 12 的缝：整颗都在卡片上缘之外，不压住状态行。
            .absolute()
            .right(px(16.))
            .top(px(-64.))
            .items_center()
            .gap_2();
        if self.reader.read(cx).speech_entry_visible()
            && crate::ui::services::activation::premium(cx)
        {
            let label = tr(cx, "reader.tts");
            let page = cx.entity().clone();
            let style = if playing { playing_style } else { idle_style };
            let button = Button::new("entry-speech")
                .custom(style)
                .rounded(radius)
                .w(px(52.))
                .h(px(52.))
                .p_0()
                .justify_center()
                // 实心那一档不需要边框；静止档与「自动」同款细描边。
                .when(!playing, |button| {
                    button.border_1().border_color(fab_border)
                })
                .icon(if playing {
                    IconName::Pause
                } else {
                    IconName::BookHeadphones
                })
                .tooltip(label.clone().to_string())
                .accessibility_label(label)
                .on_click(move |_, _, cx| page.update(cx, |this, cx| this.open_listen(cx)));
            // 播放档整颗填实墨，静止档纸白 —— 两档都不透明，正文透不上来。
            let base = if playing { ink } else { sheet };
            group = group.child(fab_shell(base, button.into_any_element()));
        }
        let auto_button = Button::new("entry-auto")
            .custom(idle_style)
            .rounded(radius)
            .w(px(52.))
            .h(px(52.))
            .p_0()
            .justify_center()
            .border_1()
            .border_color(fab_border)
            .icon(IconName::FastForward)
            // 钮上只留图标（与听书那颗同宽）：胶囊里写「自动」在手机上会把这一排
            // 挤宽，而这一格的动作由图标 + tooltip + 无障碍名说清。
            .tooltip(auto_label.clone().to_string())
            .accessibility_label(auto_label)
            .on_click(move |_, _, cx| page_auto.update(cx, |this, cx| this.toggle_auto(cx)));
        group
            .child(fab_shell(sheet, auto_button.into_any_element()))
            .into_any_element()
    }
}

/// 入口排的一格：图标在上、文字在下。
///
/// 为什么不直接给 `Button` 加 `.icon().label()`：那两样在组件的内容层里是**横排**的
/// `h_flex`，而这一排要的是导航栏那种「图标压着两个字」的格子。`Button` 实现了
/// `ParentElement`，把这一格作为**一个** child 塞进去 —— 命中区、hover / selected
/// 与无障碍仍归组件，不必另画一颗按钮、也不必手工复刻那几档状态。
///
/// `ink` 显式给：`Icon` 的着色不走文字继承（它取 `window.text_style()` 的兜底色），
/// 与旁边那行字各写各的就会一深一浅。
fn entry_cell(icon: IconName, label: SharedString, ink: Hsla) -> AnyElement {
    v_flex()
        .items_center()
        .justify_center()
        .gap(px(3.))
        .child(Icon::new(icon).with_size(px(20.)).text_color(ink))
        .child(
            div()
                .text_size(px(11.))
                .line_height(px(13.))
                .whitespace_nowrap()
                .text_color(ink)
                .child(label),
        )
        .into_any_element()
}
