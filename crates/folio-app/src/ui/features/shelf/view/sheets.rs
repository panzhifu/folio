//! 架面上那几张要读者答一句话的浮层：建组、扫描结果、切书库确认。
//!
//! 它们共用 `BottomSheet` 的档位与返回键行为（见 `ui::components::bottom_sheet`），
//! 这一份只管每张卡里装什么、按下去做什么。

use super::super::overlay;
use crate::ui::components::{access_question, muted_label, waiting_for};
use crate::ui::services::cloud_upload;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::{tf, tr};
use crate::ui::services::library::{Pick, suggested_dir};
use folio_core::Tag;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, TestSupportExt as _, div, px,
};

use super::{GUTTER, ShelfView};

/// 标记那张浮层的宽与整卡高：一行标题、一行提示、三颗短词按钮。宽 300 装得下
/// 手机那 384 逻辑像素的视口还有余（三颗小按钮一行放不下时 `flex_wrap` 折两行，
/// 卡片只偏几像素 —— 与 [`overlay::FORM_HEIGHT`] 那条「报的数与真实高对不上」
/// 同一条代价），高按内容报一个定数供居中上抬。
pub(in crate::ui::features::shelf) const TAG_WIDTH: f32 = 300.0;
pub(in crate::ui::features::shelf) const TAG_HEIGHT: f32 = 148.0;

/// 标签 → 它的本地化键。三个固定档各有一条，判据收在这一处。
fn tag_key(tag: Tag) -> &'static str {
    match tag {
        Tag::Unread => "shelf.tag_unread",
        Tag::Reading => "shelf.tag_reading",
        Tag::Finished => "shelf.tag_finished",
    }
}

impl ShelfView {
    /// 标记那张浮层：固定三档的**单选**（未读 / 在读 / 读完）—— 手动指定的书以手动
    /// 为准（[`Tag::effective`]），再点同一档退回自动判断。
    ///
    /// 两端都是居中的那一张：它答的是一句话（「这本读到哪儿了」），不是一块要滚的
    /// 清单；三颗小按钮也装得下手机那 384 逻辑像素的视口，不值得为它铺一套
    /// 底部弹层的双容器。打勾读的是打开那一刻的库（[`Self::tag_entry`] 里的快照），
    /// 点一格当场落库（[`Self::toggle_tag`]），关掉（点外面 / 返回）只收意图。
    pub(in crate::ui::features::shelf) fn tag_popover(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some((book_id, snapshot)) = &self.tag_entry else {
            return div().into_any_element();
        };
        let shelf = cx.entity().clone();
        let book_id = *book_id;
        let snapshot = snapshot.clone();
        // 三颗开关**就地搭好**：搭一颗要读一次文案（`cx` 的账），闭包里借不动它 ——
        // 与卡片角标、扫描筛选那两处同一个做法。
        let chips: Vec<AnyElement> = Tag::ALL
            .iter()
            .map(|tag| {
                let selected = snapshot.contains(tag);
                let chip_entity = shelf.clone();
                let tag = *tag;
                let key = format!("shelf-tag-{}", tag.as_str());
                let label = tr(cx, tag_key(tag)).to_string();
                Button::new(SharedString::from(key))
                    .when(selected, |button| button.secondary())
                    .when(!selected, |button| button.ghost())
                    .small()
                    .label(label.clone())
                    .accessibility_label(label)
                    .on_click(move |_, _, cx| {
                        chip_entity.update(cx, |this, cx| this.toggle_tag(book_id, tag, cx))
                    })
                    .into_any_element()
            })
            .collect();
        let body = v_flex()
            .id("shelf-tag-panel")
            .test_support()
            .w(px(TAG_WIDTH))
            .min_h(px(TAG_HEIGHT))
            .gap_3()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(tr(cx, "shelf.tag")),
            )
            .child(muted_label(tr(cx, "shelf.tag_hint"), cx))
            .child(
                // 三颗开关横排：三个短词一行放得下，一行也读得出「这是一套档位」。
                // 选中的实心（secondary），没选的描边（ghost）—— 与扫描筛选那一排
                // chips 同一套摆法，读者不用学第二种。
                h_flex().w_full().flex_wrap().gap_2().children(chips),
            )
            .into_any_element();
        overlay::centered(
            "shelf-tag-popover",
            TAG_HEIGHT,
            &shelf,
            |this, cx| this.close_tag_popover(cx),
            body,
        )
    }

    /// 多选上传的进度与结果。运行中不给关闭键 —— 那一批还在跑，收掉面板不会让它停下，
    /// 留一张「正在传」比让读者猜要诚实；完成之后一颗「关闭」。
    pub(in crate::ui::features::shelf) fn upload_popover(
        &self,
        progress: cloud_upload::Progress,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use cloud_upload::{Failure, Progress};

        let shelf = cx.entity().clone();
        let close_label = tr(cx, "shelf.upload_close");
        let (title, body) = match &progress {
            Progress::Running { total } => (
                tr(cx, "shelf.upload_title").to_string(),
                tf(cx, "shelf.upload_running", &[("n", total)]),
            ),
            Progress::Done {
                uploaded,
                failed,
                skipped,
            } => (
                tr(cx, "shelf.upload_done_title").to_string(),
                tf(
                    cx,
                    "shelf.upload_done",
                    &[("ok", uploaded), ("failed", failed), ("skipped", skipped)],
                ),
            ),
            Progress::Failed(Failure::Config) => (
                tr(cx, "shelf.upload_title").to_string(),
                tr(cx, "shelf.upload_failed_config").to_string(),
            ),
            Progress::Failed(Failure::Store) => (
                tr(cx, "shelf.upload_title").to_string(),
                tr(cx, "shelf.upload_failed_store").to_string(),
            ),
            Progress::Failed(Failure::Remote) => (
                tr(cx, "shelf.upload_title").to_string(),
                tr(cx, "shelf.upload_failed_remote").to_string(),
            ),
        };
        let mut card = v_flex()
            .id("shelf-upload")
            .test_support()
            .w(px(overlay::FORM_WIDTH))
            .min_h(px(overlay::FORM_HEIGHT))
            .gap_3()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
            .child(muted_label(body, cx));
        if !matches!(progress, Progress::Running { .. }) {
            card = card.child(
                Button::new("shelf-upload-close")
                    .ghost()
                    .label(close_label.clone().to_string())
                    .accessibility_label(close_label)
                    .on_click(|_, _, cx| cloud_upload::dismiss(cx)),
            );
        }
        overlay::centered(
            "shelf-upload-popover",
            overlay::FORM_HEIGHT,
            &shelf,
            |_, cx| cloud_upload::dismiss(cx),
            card.into_any_element(),
        )
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
            // 输入框在卡片中部，键盘会盖到它：默认的那一抬要留着。
            true,
            body,
        )
    }

    /// 扫描那一屏在手机上。清单要滚，所以展开那一档比另两屏高。
    ///
    /// 这一格必须是 **flex 列**而不是普通块：`scan_body` 交回来的是一棵
    /// `flex_1()` 的树，列表要靠父层给出确定高度才排得出行；块容器里
    /// `flex_1` 不生效，虚拟列表会塌成 0 高 —— 屏幕上就是只有标题与底部按钮、
    /// 中间一行候选都没有（勾选自然也无从谈起）。横向留白也不在这里给：
    /// `scan_body` 的头部、每行与底部各自带一条 `GUTTER`，这里再加一层就是双倍缩进。
    pub(in crate::ui::features::shelf) fn scan_sheet(&self, cx: &mut Context<Self>) -> AnyElement {
        let body = (!self.scan_sheet.read(cx).is_idle() && self.scan.is_some()).then(|| {
            v_flex()
                .w_full()
                .flex_1()
                .min_h_0()
                .pb_2()
                .child(self.scan_body(cx))
                .into_any_element()
        });
        overlay::sheet(
            "shelf-scan-sheet",
            &self.scan_sheet,
            overlay::SCAN_SHEET_EXPANDED,
            // **不抬**：搜索框钉在弹层顶部，展开档又占 85% 视口，键盘出来时它本来就在
            // 键盘上方；默认那一抬只会把整屏内容（含那颗输入框）往上顶一下 —— 读者报的
            // 「输入框明明很高，键盘一弹还是往上蹦」就是它。与阅读页目录那一屏同一形状、
            // 同一取舍（`reader::toc_panel::sheet_element` 的 `.ime_lift(false)`）。
            // 清单自己滚，底下一排按钮打字时用不上，都不缺键盘让位。
            false,
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

    /// 「新建分组」在桌面那一档：屏幕居中的一张浮层。
    ///
    /// 与「从链接导入」那一张同一个容器、同一个尺寸：两张卡都是「一句提示 + 一行输入 +
    /// 两颗按钮」的形状。也同一个理由摆在中间而不是钉在某颗按钮底下 —— 入口在卡片菜单里
    /// （长按或右键，那一格说的是「为这本书建个组」），而菜单点完就散了，没有留下能钉的
    /// 东西。
    pub(in crate::ui::features::shelf) fn group_popover(
        &self,
        entry: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shelf = cx.entity().clone();
        let body = self.group_form(entry, cx);
        overlay::centered(
            "shelf-group-popover",
            overlay::FORM_HEIGHT,
            &shelf,
            |this, cx| this.close_group_panel(cx),
            div()
                .w(px(overlay::FORM_WIDTH))
                .min_h(px(overlay::FORM_HEIGHT))
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
        let mut panel = v_flex()
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
            );
        // 按了「选择文件夹」而那道门没拿到授权：这一问摆在这排钮下面，而不是让那颗钮按下去
        // 什么都不发生（架面这一头是内联面板，不是浮层，摆得下）。
        if waiting_for(Pick::Library, cx) {
            panel = panel.child(access_question(cx));
        }
        panel.into_any_element()
    }
}
