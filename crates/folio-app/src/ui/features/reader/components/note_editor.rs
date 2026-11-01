//! 笔记那一层：选中一段字之后，就地为它写一条笔记。
//!
//! 它是阅读页里**唯一**带文本输入的就地浮层，形状与脚注、翻译那两张同一族 —— 都是
//! gpui-kit 的 `Popover`：卡顶挂一枚小三角，尖对着开这张卡时拍下的那一处（选区中线），
//! 与划词那条浮条的尾巴接的是同一个点。卡里仍是「选中的引文 + 多行输入框 + 取消 / 保存」，
//! 底下仍压着半透明遮罩。状态（输入框、改的是哪一条批注、锚在哪一像素）存在会话里，
//! 见 [`NoteEditor`]；开 / 存 / 取消三个命令在 `model/annotations.rs`，这一文件只把状态
//! 摆成元素。
//!
//! 两件容易写错的判断：
//!
//! - **`annotation: Some` 与 `None` 是两种保存语义**：挑中库里已有一条划线时，保存是
//!   `update_annotation`（笔记连同那一行原来的颜色与画法一起写回）；普通选区保存才新建一条
//!   批注，用的是默认那档颜色与配置里那一格画法。把两者混成一条「先划再改」的路，
//!   会在读者只改笔记时多落一条。
//! - **新笔记空着不能保存**：按「笔记」却什么都不写，等于什么都没有发生；已有笔记
//!   清空则是有意义的一步（把笔记删掉，划线留着），所以只有 `None` 那一臂禁用它。
//!
//! 竖直摆位仍钉在顶上（手机 56、宽屏 96），跟着选区的只有横向：Android 的软键盘是
//! `adjustNothing`（`AndroidManifest.xml` 里那一条），卡一旦落到选区下方就被键盘盖掉，
//! 而这张卡是拿来打字的。三角因此是「朝上、对准选区那一栏」的方向记号，不是把卡挂到字下面。
//!
//! 焦点要点名交给输入框：`Popover` 升起那一刻把焦点移进它自己的 focus handle，不
//! `track_focus` 输入框那一格，软键盘就不会为这张卡弹起来。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, Context, Entity, Focusable, InteractiveElement as _, IntoElement,
    ParentElement, Pixels, SharedString, StatefulInteractiveElement as _, Styled, Subscription,
    Window, div, px,
};

use crate::ui::features::reader::model::annotations::MarkedRow;
use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::services::l10n::tr;
use crate::ui::shell::navigation::SizeClass;

/// 便签卡的最宽那一条（含弹层自己那层内边距）。
const NOTE_CARD_W: f32 = 420.0;
/// 卡片离屏边最少留这么多：窄窗口按这一档退宽，三角也被夹在同一条边之内。
const NOTE_EDGE: f32 = 16.0;
/// `Popover` 的 `appearance` 给的那层内边距（`p_3`）。内容的宽是卡宽减掉它 ——
/// 两笔账分开写的话，卡会比能写字那一块宽出 24 像素。
const NOTE_PAD: f32 = 12.0;
/// 锚点与卡片顶边之间的缝（三角长在缝里，尖对着锚点）。
const NOTE_GAP: f32 = 10.0;

/// 会话里那份笔记编辑状态。
pub(in crate::ui::features::reader) struct NoteEditor {
    /// 多行输入框。改动会经订阅转成一次重画：保存键的可用状态跟着它走。
    pub(in crate::ui::features::reader) input: Entity<TextareaState>,
    /// `Some(那一行)` = 改库里已有的一条（保存时把它的颜色与画法原样带回去）；
    /// `None` = 保存时才落一条新的。类型是 [`MarkedRow`] 而不是 `(id, 颜色)` 那一对：
    /// `update_annotation` 整条覆盖，少带一格就悄悄把读者另一格改的东西退回默认档。
    pub(in crate::ui::features::reader) annotation: Option<MarkedRow>,
    /// 选中文字的开头一段（最多 160 字）。打开时取一次，不跟着每帧重切。
    pub(in crate::ui::features::reader) excerpt: SharedString,
    /// 三角对准的那一像素（窗口坐标里的横向，开这张卡时从菜单拍下来的快照）。
    /// `None` = 首帧还没量到菜单，渲染那一头兜底居中。
    pub(in crate::ui::features::reader) anchor_x: Option<f32>,
    /// 输入框的变更订阅。跟着编辑器一起活，drop 掉就收不到重画了。
    pub(in crate::ui::features::reader) _watch: Subscription,
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
        let input = editor.input.clone();
        let excerpt = editor.excerpt.clone();
        let reader = cx.entity().clone();
        let scrim_reader = reader.clone();
        let can_save =
            editor.annotation.is_some() || !editor.input.read(cx).value().trim().is_empty();
        // 手机上贴近上沿：软键盘是 `adjustNothing`，贴着底边的卡片会被它盖掉。
        // 宽屏上往下压到视口上部（不是正中），留出键盘和手势条。
        let top = if SizeClass::of(window).is_compact() {
            56.0
        } else {
            96.0
        };
        let viewport = f32::from(window.viewport_size().width).max(1.0);
        // 零尺寸触发器落在哪一像素，三角就对准哪一像素：选区中线；没量到就居中。
        let anchor_x = editor
            .anchor_x
            .unwrap_or(viewport * 0.5)
            .clamp(NOTE_EDGE, (viewport - NOTE_EDGE).max(NOTE_EDGE));
        let width = px((viewport - NOTE_EDGE * 2.0).min(NOTE_CARD_W) - NOTE_PAD * 2.0);
        let focus = editor.input.read(cx).focus_handle(cx).clone();
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
                            scrim_reader.update(cx, |reader, cx| reader.cancel_note_editor(cx))
                        }),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(anchor_x))
                        .top(px(top))
                        .child(
                            Popover::new("reader-note")
                                .anchor(Anchor::TopCenter)
                                .arrow(true)
                                .offset(px(NOTE_GAP))
                                .open(true)
                                .track_focus(&focus)
                                // 弹层之外那一击由弹层自己收（`on_mouse_down_out`），与
                                // 脚注、翻译那两张同一条路，这里只把它接回会话。
                                .on_open_change({
                                    let reader = reader.clone();
                                    move |open, _, cx| {
                                        if !*open {
                                            reader.update(cx, |reader, cx| {
                                                reader.cancel_note_editor(cx)
                                            });
                                        }
                                    }
                                })
                                .trigger(
                                    Button::new("reader-note-trigger")
                                        .ghost()
                                        .p_0()
                                        .w(px(0.))
                                        .h(px(0.))
                                        .min_w(px(0.))
                                        .min_h(px(0.)),
                                )
                                .content(move |_, _, cx| {
                                    note_card(&reader, &input, &excerpt, can_save, width, cx)
                                }),
                        )
                        .into_any_element(),
                )
                .into_any_element(),
        )
    }
}

/// 便签那张卡的本体。弹层开着时**每帧现建**（与 `reading_page::translation_card` 同一条
/// 理由：元素不是 `Clone`，闭包里重建是唯一不撒谎的写法），但输入框那个实体不是 ——
/// 它握着光标、选区与撤销历史，每帧新建会把读者刚打的字连同光标一起丢回第一帧。
///
/// 卡的表面（底色、描边、投影、内边距）由 `Popover` 的 `appearance` 给，这里不叠第二份。
///
/// `track_focus` 要点名输入框：弹层的内容层自己带一枚 focus handle 并覆盖整张卡，
/// 不点名的话，点在标题、引文、卡片的空白处都会把焦点从输入框收走 —— 手机上那一下
/// 表现为软键盘收起，读者只是想挪个手指。点在输入框与按钮上仍归它们自己（更深处赢）。
fn note_card(
    reader: &Entity<ReaderSession>,
    input: &Entity<TextareaState>,
    excerpt: &SharedString,
    can_save: bool,
    width: Pixels,
    cx: &mut Context<PopoverState>,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let close = reader.clone();
    let cancel = reader.clone();
    let save = reader.clone();
    let input_focus = input.read(cx).focus_handle(cx).clone();
    v_flex()
        .id("reader-note-card")
        .track_focus(&input_focus)
        .w(width)
        .gap_3()
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
                    .border_color(border)
                    .pl_2()
                    .text_sm()
                    .line_clamp(2)
                    .text_color(muted)
                    .child(excerpt.clone()),
            )
        })
        .child(
            Textarea::new(input)
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
        )
        .into_any_element()
}
