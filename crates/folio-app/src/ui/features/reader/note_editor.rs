//! 笔记那一层：选中一段字之后，就地为它写一条笔记。
//!
//! 它是阅读页里**唯一**带文本输入的就地浮层：下面压着半透明遮罩，正中一张便签卡，
//! 卡里是「选中的引文 + 多行输入框 + 取消 / 保存」。状态（输入框、改的是哪一条批注）
//! 存在会话里，见 [`NoteEditor`]；开 / 存 / 取消三个命令在 `modules/annotations.rs`，
//! 这一文件只把状态摆成元素。
//!
//! 两件容易写错的判断：
//!
//! - **`annotation: Some` 与 `None` 是两种保存语义**：挑中库里已有一条划线时，保存是
//!   `update_annotation`（笔记连同原来的颜色一起写回）；普通选区保存才新建一条批注，
//!   用的是默认那档颜色。把两者混成一条「先划再改」的路，会在读者只改笔记时多落一条。
//! - **新笔记空着不能保存**：按「笔记」却什么都不写，等于什么都没有发生；已有笔记
//!   清空则是有意义的一步（把笔记删掉，划线留着），所以只有 `None` 那一臂禁用它。
//!
//! 摆位在手机上贴着上沿、在宽屏上压在视口中上部：Android 的软键盘是 `adjustNothing`
//! （`AndroidManifest.xml` 里那一条），贴着底边的卡片会被键盘盖掉。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Sizable, ThemeStyled as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Entity, InteractiveElement as _, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div, px,
};

use super::session::ReaderSession;
use crate::ui::services::l10n::tr;
use crate::ui::shell::navigation::SizeClass;

/// 会话里那份笔记编辑状态。
pub(super) struct NoteEditor {
    /// 多行输入框。改动会经订阅转成一次重画：保存键的可用状态跟着它走。
    pub(super) input: Entity<TextareaState>,
    /// `Some((批注 id, 颜色名))` = 改库里已有的一条；`None` = 保存时才落一条新的。
    pub(super) annotation: Option<(i64, String)>,
    /// 选中文字的开头一段（最多 160 字）。打开时取一次，不跟着每帧重切。
    pub(super) excerpt: SharedString,
    /// 输入框的变更订阅。跟着编辑器一起活，drop 掉就收不到重画了。
    pub(super) _watch: Subscription,
}

impl ReaderSession {
    /// 笔记那一层该不该画。阅读页拿它决定「身上压着东西」要不要报给会话。
    pub fn note_editor_open(&self) -> bool {
        self.note_editor.is_some()
    }

    /// 把这一层摆出来。收着时一个节点都不多。
    pub(in crate::ui::features::reader) fn note_editor_element(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let editor = self.note_editor.as_ref()?;
        let excerpt = editor.excerpt.clone();
        let muted = cx.theme().muted_foreground;
        let reader = cx.entity().clone();
        let close = reader.clone();
        let cancel = reader.clone();
        let save = reader.clone();
        let can_save =
            editor.annotation.is_some() || !editor.input.read(cx).value().trim().is_empty();
        // 手机上贴近上沿：软键盘是 `adjustNothing`，贴着底边的卡片会被它盖掉。
        // 宽屏上往下压到视口上部（不是正中），留出键盘和手势条。
        let top = if SizeClass::of(window).is_compact() {
            56.0
        } else {
            96.0
        };
        let card = v_flex()
            .id("reader-note-card")
            .occlude()
            .w_full()
            .max_w(px(420.0))
            .gap_3()
            .p_4()
            .popover_style(cx)
            .rounded(px(16.0))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(18.0))
                            .child(Icon::new(IconName::StickyNote)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_base()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(tr(cx, "reader.note_title")),
                    )
                    .child(
                        Button::new("note-close")
                            .ghost()
                            .small()
                            .icon(IconName::X)
                            .tooltip(tr(cx, "reader.sel_cancel").to_string())
                            .on_click(move |_, _, cx| {
                                close.update(cx, |reader, cx| reader.cancel_note_editor(cx))
                            }),
                    ),
            )
            .when(!excerpt.is_empty(), |this| {
                this.child(
                    div()
                        .w_full()
                        .max_h(px(58.0))
                        .overflow_hidden()
                        .border_l_2()
                        .border_color(cx.theme().border)
                        .pl_2()
                        .text_sm()
                        .line_clamp(2)
                        .text_color(muted)
                        .child(excerpt),
                )
            })
            .child(
                Textarea::new(&editor.input)
                    .w_full()
                    .h(px(128.0))
                    .text_sm()
                    .aria_label(tr(cx, "reader.note_title")),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("note-cancel")
                            .ghost()
                            .label(tr(cx, "reader.sel_cancel").to_string())
                            .on_click(move |_, _, cx| {
                                cancel.update(cx, |reader, cx| reader.cancel_note_editor(cx))
                            }),
                    )
                    .child(
                        Button::new("note-save")
                            .primary()
                            .label(tr(cx, "reader.note_save").to_string())
                            .disabled(!can_save)
                            .on_click(move |_, _, cx| {
                                save.update(cx, |reader, cx| reader.save_note(cx))
                            }),
                    ),
            );
        let overlay = cx.theme().overlay;
        Some(
            div()
                .id("reader-note-layer")
                .absolute()
                .inset_0()
                .child(
                    div()
                        .id("reader-note-scrim")
                        .occlude()
                        .absolute()
                        .inset_0()
                        .bg(overlay)
                        .on_click(move |_, _, cx| {
                            reader.update(cx, |reader, cx| reader.cancel_note_editor(cx))
                        }),
                )
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_start()
                        .justify_center()
                        .pt(px(top))
                        .px_4()
                        .child(card),
                )
                .into_any_element(),
        )
    }
}
