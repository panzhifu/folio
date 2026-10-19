//! 架面上那几张要读者答一句话的浮层：建组、从链接导入、扫描结果、切书库确认。
//!
//! 它们共用 `BottomSheet` 的档位与返回键行为（见 `ui::components::bottom_sheet`），
//! 这一份只管每张卡里装什么、按下去做什么。

use super::super::overlay;
use crate::ui::components::muted_label;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::tr;
use crate::ui::services::library::suggested_dir;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::{
    Anchor, AnyElement, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, TestSupportExt as _, div, px,
};

use super::{GUTTER, ShelfView};

impl ShelfView {
    /// 「从链接导入」那一屏的内容。与建组那一屏同一形状，也只有容器不同。
    pub(in crate::ui::features::shelf) fn url_form(
        &self,
        entry: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity().clone();
        let close_entity = entity.clone();
        let import_entity = entity.clone();
        let cancel_label = tr(cx, "shelf.cancel");
        let import_label = tr(cx, "shelf.url_import");
        v_flex()
            .id("shelf-url-panel")
            .test_support()
            .w_full()
            .gap_3()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(cx, "shelf.url_title")),
            )
            .child(muted_label(tr(cx, "shelf.url_hint"), cx))
            .child(div().w_full().child(Input::new(&entry).cleanable(true)))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("shelf-url-cancel")
                            .ghost()
                            .label(cancel_label.to_string())
                            .accessibility_label(cancel_label)
                            .on_click(move |_, _, cx| {
                                close_entity.update(cx, |this, cx| this.close_url_panel(cx))
                            }),
                    )
                    .child(
                        Button::new("shelf-url-import")
                            .primary()
                            .label(import_label.to_string())
                            .accessibility_label(import_label)
                            .on_click(move |_, _, cx| {
                                import_entity.update(cx, |this, cx| this.import_from_url(cx))
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 「从链接导入」在桌面那一档：屏幕居中的一张浮层。
    ///
    /// 居中而不是钉在 ⋮ 那颗按钮底下：这一屏的入口在那条菜单里，而菜单点开这张卡的
    /// 同一帧就散了 —— 钉在一个刚消失的元素上，读者看到的是卡从半空里长出来。
    pub(in crate::ui::features::shelf) fn url_popover(
        &self,
        entry: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shelf = cx.entity().clone();
        let body = self.url_form(entry, cx);
        overlay::centered(
            "shelf-url-popover",
            overlay::URL_HEIGHT,
            &shelf,
            |this, cx| this.close_url_panel(cx),
            div()
                .w(px(overlay::URL_WIDTH))
                .min_h(px(overlay::URL_HEIGHT))
                .child(body)
                .into_any_element(),
        )
    }

    /// 顶部那颗分组按钮，外加它底下那张浮层（开着的时候）。
    ///
    /// 浮层挂在按钮**之后**而不是包着那颗按钮：`Popover` 要拿一个元素的 bounds 当锚点，
    /// 而那颗按钮自己已经挂着一条筛选菜单 —— 两层浮层抢同一个触发器会互相顶掉。
    /// 外面包一层 `div` 而不是把它塞成 `h_flex` 的下一个孩子：那样那两颗按钮之间会多算
    /// 一条 `gap_2`，而浮层是个零尺寸的东西。
    pub(in crate::ui::features::shelf) fn group_cell(&self, cx: &mut Context<Self>) -> AnyElement {
        let button = self.group_button(cx).into_any_element();
        match (&self.group_entry, self.group_open) {
            (Some((_, entry)), true) => div()
                .child(button)
                .child(self.group_popover(entry.clone(), cx))
                .into_any_element(),
            _ => button,
        }
    }

    /// 建组那一屏在手机上：同一份表单压进底部弹层。
    pub(in crate::ui::features::shelf) fn group_sheet(&self, cx: &mut Context<Self>) -> AnyElement {
        let body = self.form_body(
            &self.group_sheet,
            self.group_entry.as_ref().map(|(_, e)| e),
            cx,
            |this, entry, cx| this.group_form(entry, cx),
        );
        overlay::sheet(
            "shelf-group-sheet",
            &self.group_sheet,
            overlay::SHEET_EXPANDED,
            body,
        )
    }

    /// 链接导入那一屏在手机上。
    pub(in crate::ui::features::shelf) fn url_sheet(&self, cx: &mut Context<Self>) -> AnyElement {
        let body = self.form_body(
            &self.url_sheet,
            self.url_entry.as_ref(),
            cx,
            |this, entry, cx| this.url_form(entry, cx),
        );
        overlay::sheet(
            "shelf-url-sheet",
            &self.url_sheet,
            overlay::SHEET_EXPANDED,
            body,
        )
    }

    /// 扫描那一屏在手机上。清单要滚，所以展开那一档比另两屏高。
    pub(in crate::ui::features::shelf) fn scan_sheet(&self, cx: &mut Context<Self>) -> AnyElement {
        let body = (!self.scan_sheet.read(cx).is_idle() && self.scan.is_some()).then(|| {
            div()
                .flex_1()
                .min_h_0()
                .px(px(GUTTER))
                .pb_2()
                .child(self.scan_body(cx))
                .into_any_element()
        });
        overlay::sheet(
            "shelf-scan-sheet",
            &self.scan_sheet,
            overlay::SCAN_SHEET_EXPANDED,
            body,
        )
    }

    /// 扫描那一屏在桌面那一档：屏幕居中的一张卡，清单在里面自己滚。
    ///
    /// 高是**定死**的而不是「至少」：`scan_body` 里那条 `v_virtual_list` 要一个能滚的框，
    /// 「至少多高」给不出这个框。
    pub(in crate::ui::features::shelf) fn scan_popover(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shelf = cx.entity().clone();
        let body = div()
            .w(px(overlay::SCAN_WIDTH))
            .h(px(overlay::SCAN_HEIGHT))
            .flex()
            .flex_col()
            .child(self.scan_body(cx))
            .into_any_element();
        overlay::centered(
            "shelf-scan-popover",
            overlay::SCAN_HEIGHT,
            &shelf,
            |this, cx| this.close_scan(cx),
            body,
        )
    }

    /// 「新建分组」那一屏的内容：一行输入 + 两颗按钮。桌面与移动共用这一份，
    /// 只有容器不同（见 [`overlay`]）。
    ///
    /// 不带横向留白也不带 `flex_1`：宽度与高归容器给 —— 桌面那张浮层是定宽的，手机上
    /// 弹层天生满幅，写在这里两头都会多算一层。
    pub(in crate::ui::features::shelf) fn group_form(
        &self,
        entry: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity().clone();
        let close_entity = entity.clone();
        let create_entity = entity.clone();
        let cancel_label = tr(cx, "shelf.cancel");
        let create_label = tr(cx, "shelf.group_create");
        v_flex()
            .id("shelf-group-panel")
            .test_support()
            .w_full()
            .gap_3()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(cx, "shelf.group_new")),
            )
            .child(muted_label(tr(cx, "shelf.group_hint"), cx))
            .child(div().w_full().child(Input::new(&entry).cleanable(true)))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("shelf-group-cancel")
                            .ghost()
                            .label(cancel_label.to_string())
                            .accessibility_label(cancel_label)
                            .on_click(move |_, _, cx| {
                                close_entity.update(cx, |this, cx| this.close_group_panel(cx))
                            }),
                    )
                    .child(
                        Button::new("shelf-group-create")
                            .primary()
                            .label(create_label.to_string())
                            .accessibility_label(create_label)
                            .on_click(move |_, _, cx| {
                                create_entity.update(cx, |this, cx| this.create_group(cx))
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 「新建分组」在桌面那一档：钉在顶部那颗分组按钮底下的一张浮层。
    ///
    /// 入口仍在书的长按菜单里（那一格说的是「为这本书建个组」），但浮层跟着**管组的那颗
    /// 按钮**开：菜单点完就没了，钉在它消失的位置上等于钉在一个已经不存在的锚点上。
    /// 挂在按钮之后而不是按钮里面 —— 它是那颗按钮那一条命令的延续。
    pub(in crate::ui::features::shelf) fn group_popover(
        &self,
        entry: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shelf = cx.entity().clone();
        let body = self.group_form(entry, cx);
        overlay::anchored(
            "shelf-group-popover",
            Anchor::TopLeft,
            6.0,
            &shelf,
            |this, cx| this.close_group_panel(cx),
            div()
                .w(px(overlay::GROUP_WIDTH))
                .child(body)
                .into_any_element(),
        )
    }

    /// 「书要拷到哪儿」那块面板。
    ///
    /// 它是架面的一部分而不是浮层：浮层在 Android 上现在点不住（长按菜单那条路的缺陷），
    /// 而这一步是导入的前提，点不动就等于没有导入。
    pub(in crate::ui::features::shelf) fn confirm_panel(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pick_entity = cx.entity().clone();
        let confirm_entity = pick_entity.clone();
        let cancel_entity = pick_entity.clone();
        let pick_label = tr(cx, "library.pick_dir");
        let confirm_label = tr(cx, "library.confirm");
        let cancel_label = tr(cx, "library.cancel");
        // 已经选过就显示选中的那个位置，没选过显示建议位置 —— 选完当场能看出结果。
        let chosen = ConfigHolder::library(cx).dir;
        let shown = if chosen.trim().is_empty() {
            suggested_dir().display().to_string()
        } else {
            chosen
        };
        v_flex()
            .id("library-confirm-panel")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_3()
            .px(px(GUTTER))
            .py_4()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(cx, "library.title")),
            )
            .child(muted_label(tr(cx, "library.hint"), cx))
            .child(
                div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(cx.theme().muted)
                    .text_color(cx.theme().muted_foreground)
                    .child(shown),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("library-pick")
                            .ghost()
                            .label(pick_label.clone().to_string())
                            .accessibility_label(pick_label)
                            .on_click(move |_, _, cx| {
                                pick_entity.update(cx, |this, cx| this.pick_library(cx))
                            }),
                    )
                    .child(
                        Button::new("library-confirm")
                            .primary()
                            .label(confirm_label.clone().to_string())
                            .accessibility_label(confirm_label)
                            .on_click(move |_, _, cx| {
                                confirm_entity.update(cx, |this, cx| this.confirm_library(cx))
                            }),
                    )
                    .child(
                        Button::new("library-cancel")
                            .ghost()
                            .label(cancel_label.clone().to_string())
                            .accessibility_label(cancel_label)
                            .on_click(move |_, _, cx| {
                                cancel_entity.update(cx, |this, cx| this.cancel_confirm(cx))
                            }),
                    ),
            )
            .into_any_element()
    }
}
