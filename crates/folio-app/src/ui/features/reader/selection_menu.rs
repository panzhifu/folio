//! 选中的就地菜单：复制 / 划线 / 全选本段 / 全选本章 / 取消。
//!
//! 独立成一份文件是因为它是阅读页里**改动最频繁**的一块：加一项、换图标、调格子
//! 的尺寸与摆位，动到的都只有这一处，而它原先住在 `view.rs` 里 —— 那个文件是
//! 「排版结果 → 元素」的渲染主干，两者改的从来不是同一件事。
//!
//! 一格「图标在上、文字在下」，整格可点 —— 图标必须配着文字：只有 Android 注册了
//! asset source，iOS / HarmonyOS 上图标是静默空白的（`plans/facts.md`），只剩文字的格子
//! 在那两端就是一颗按了不知道干什么的空白。唯一的例外是颜色那一行的实心圆：那不是
//! 图标资源，是当场画出来的色块，两端都画得出来。
//!
//! 压在正文之上的真按钮而不是浮层：见 `page.rs` 模块注释与 `plans/facts.md`。
//! 画出的位置经 `canvas` 回写进会话的 `menu_bounds`：压在它上面的一击属于菜单，
//! 不算「点别处清除」（`modules/selection.rs` 的 `tap_with_selection`）。
//!
//! 第二格跟着选区自己是谁换：选区**正好等于**库里某一条划线时它是「取消划线」
//! （[`ReaderSession::selection_annotation`]）；改过范围的选区就是普通选区，那一格回到
//! 「划线」。下面那一行五格颜色**恒在**：挑中某条划线时点色＝换色，普通选区点色＝就用
//! 这个色划一条 —— 于是「划线」那一格给的是默认那档黄，想换个色直接点圆点。
//!
//! 摆位分两半：[`slot`] 是**纯函数**（首末高亮的上下边 + 视口高 + 卡片高 → 摆在哪一边、
//! 顶边在哪一像素），所以「上面放不下就翻到下面」这条规则能在宿主上直接钉死；渲染那一侧
//! 只负责把 [`Slot`] 落成元素。

use folio_core::store::ANNOTATION_COLORS;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Icon, ThemeStyled as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement,
    PathBuilder, SharedString, StatefulInteractiveElement as _, Styled, Window, canvas, div, point,
    px,
};

use super::modules::annotations::Palette;
use super::session::ReaderSession;
use crate::ui::shared::l10n::tr;

/// 菜单里一格的边长（逻辑像素）。拇指能稳定命中的下限是 44，图标 + 文字竖排
/// 要 56 才不挤（参照真机上同类阅读器的那张划词卡，量得 55dp 见方）。
const MENU_CELL: f32 = 56.0;
/// 卡片内缩。
const MENU_PAD: f32 = 6.0;
/// 指向选区那条小尾巴的高。
const MENU_TAIL: f32 = 8.0;
/// 菜单与选区之间的缝，也是它与视口上下边的最小间距。
const MENU_GAP: f32 = 6.0;
/// 颜色那一行的高。五枚圆点 20 见方，上下各留 7 —— 这一行不需要文字，
/// 颜色本身就是它的说明。
const MENU_SWATCH: f32 = 34.0;
/// 一枚颜色圆点的直径。
const SWATCH_DOT: f32 = 20.0;
/// 叠在圆点上的那枚勾的字号（`Icon` 取当前文字号）。
const SWATCH_CHECK: f32 = 13.0;

/// 整块菜单占的高（含尾巴）：摆位与「放不下就翻面」都按这个数算。
/// 颜色那一行**恒在**，所以没有「一行 / 两行」两种高。
pub(in crate::ui::features::reader) const fn card_height() -> f32 {
    MENU_CELL + MENU_PAD * 2.0 + MENU_SWATCH + MENU_TAIL
}

/// 这一帧菜单该摆在哪。
#[derive(Debug, PartialEq)]
pub(in crate::ui::features::reader) enum Slot {
    /// 摆在首行高亮之上，`y` 是卡片顶边；尾巴朝下指着选区。
    Above(f32),
    /// 上面放不下，掉到末行高亮之下；尾巴翻过来朝上。
    Below(f32),
    /// 没有高亮可锚（排版还没跟上、选区滚出屏了）—— 这一帧不画。
    Hidden,
}

/// 首末高亮的上下边 + 视口高 + 卡片高 → 菜单摆位。
///
/// 优先摆在选区之上：读者刚选完，视线还在这一段上，菜单出现在它头顶不打断阅读。
/// 顶部放不下才掉到末行之下 —— 而不是硬塞半个屏幕外。
pub(in crate::ui::features::reader) fn slot(
    first_top: Option<f32>,
    last_bottom: Option<f32>,
    viewport_height: f32,
    height: f32,
) -> Slot {
    let (Some(first_top), Some(last_bottom)) = (first_top, last_bottom) else {
        return Slot::Hidden;
    };
    // 留 8 像素的余量：差一点点时宁可翻面，也不要贴住屏幕顶边长出半个卡片。
    if first_top >= height + 8.0 {
        return Slot::Above(first_top - height - MENU_GAP);
    }
    let below = (last_bottom + MENU_GAP).min(viewport_height - height);
    // `.max(2.0)` 是给极矮的视口兜底：夹完可能是负数，负数意味着菜单自己跑出屏幕。
    Slot::Below(below.max(2.0))
}

impl ReaderSession {
    /// 把这一帧的菜单摆出来。`slot` 是 [`Self::selection_menu_open`] 之外唯一的输入：
    /// 顶边在哪一像素、尾巴朝哪边，都由它决定。
    pub(in crate::ui::features::reader) fn selection_menu(
        &self,
        cx: &mut Context<Self>,
        slot: &Slot,
    ) -> Option<AnyElement> {
        let (top, tail_up) = match slot {
            Slot::Above(top) => (*top, false),
            Slot::Below(top) => (*top, true),
            // 没有锚位：调用方连元素都不该要，`menu_bounds` 由它一并收掉。
            Slot::Hidden => return None,
        };
        let reader = cx.entity().clone();
        let bounds_cell = self.menu_bounds.clone();
        let surface = cx.theme().popover;
        let palette = Palette::of(cx.theme());
        // 勾选标记压在圆点上，取的是页面底色：浅色主题圆点是深色（`*-600`）、底色近白，
        // 深色主题圆点是浅色（`*-400`）、底色近黑 —— 一个数在两套主题里都够对比。
        let check = cx.theme().background;
        let copy = tr(cx, "reader.sel_copy");
        let highlight = tr(cx, "reader.sel_highlight");
        let unhighlight = tr(cx, "reader.sel_unhighlight");
        let paragraph = tr(cx, "reader.sel_paragraph");
        let chapter = tr(cx, "reader.sel_chapter");
        let cancel = tr(cx, "reader.sel_cancel");
        // 挑中的正是留在书上的那一条时，第二格改做「取消划线」；下面那一行颜色恒在，
        // 只是给它那一档多打一枚勾。
        let marked = self.selection_annotation();
        let mark_cell = match marked {
            Some(_) => menu_cell(
                "selection-unhighlight",
                IconName::Trash,
                unhighlight,
                reader.clone(),
                |r, _, cx| r.delete_highlight(cx),
            ),
            None => menu_cell(
                "selection-highlight",
                IconName::Highlighter,
                highlight,
                reader.clone(),
                |r, _, cx| r.add_highlight(cx),
            ),
        };
        // 颜色那一行恒在。挑中的正是库里某条划线时，给它那一档打勾、点色即换色；
        // 普通选区没有「当下那一档」，点色即「就用这个色划一条」。
        let current = marked.as_ref().map(|(_, color)| color.as_str());
        let colors = swatch_row(reader.clone(), &palette, check, current);
        Some(
            v_flex()
                .absolute()
                .left(px(self.prefs.margin_x))
                .top(px(top))
                .children(tail_up.then(|| menu_tail(true, surface)))
                .child(
                    v_flex()
                        .p(px(MENU_PAD))
                        // 两行之间不留缝：`card_height` 算的就是「格子高 + 颜色行 + 内缩 + 尾巴」，
                        // 中间再插一段 gap 就成了第二个真相源，摆位按少算的那几像素翻面。
                        .gap_0()
                        .rounded(px(10.0))
                        .popover_style(cx)
                        .child(
                            h_flex()
                                .gap_0()
                                .child(menu_cell(
                                    "selection-copy",
                                    IconName::Copy,
                                    copy,
                                    reader.clone(),
                                    |r, _, cx| r.copy_selection(cx),
                                ))
                                .child(mark_cell)
                                .child(menu_cell(
                                    "selection-paragraph",
                                    IconName::Pilcrow,
                                    paragraph,
                                    reader.clone(),
                                    |r, window, cx| r.select_paragraph(window, cx),
                                ))
                                .child(menu_cell(
                                    "selection-chapter",
                                    IconName::BookText,
                                    chapter,
                                    reader.clone(),
                                    |r, _, cx| r.select_chapter(cx),
                                ))
                                .child(menu_cell(
                                    "selection-cancel",
                                    IconName::X,
                                    cancel,
                                    reader.clone(),
                                    |r, _, cx| {
                                        r.clear_selection();
                                        cx.notify();
                                    },
                                )),
                        )
                        .child(colors),
                )
                .children((!tail_up).then(|| menu_tail(false, surface)))
                // 记下这一帧菜单真实落地的位置（窗口坐标），点按判定要用。
                .child(
                    canvas(
                        move |painted, _, _| bounds_cell.set(Some(painted)),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .into_any_element(),
        )
    }
}

/// 菜单里的一格。动作按参数传进来，五格只差图标、文字与那一下做什么。
///
/// 图标画多大由 `text_size` 定（`Icon` 取的是当前文字号），文字单独压小一档：
/// 一格 56 像素里图标占 24，文字再占 11，剩下的留给上下留白。
fn menu_cell(
    id: &'static str,
    icon: IconName,
    label: SharedString,
    reader: Entity<ReaderSession>,
    action: impl Fn(&mut ReaderSession, &mut Window, &mut Context<ReaderSession>) + 'static,
) -> AnyElement {
    v_flex()
        .id(id)
        .w(px(MENU_CELL))
        .h(px(MENU_CELL))
        .items_center()
        .justify_center()
        .gap_1()
        .rounded_md()
        .cursor_pointer()
        .child(div().text_size(px(24.0)).child(Icon::new(icon)))
        .child(div().text_size(px(11.0)).child(label))
        .on_click(move |_, window, cx| {
            reader.update(cx, |reader, cx| action(reader, window, cx));
        })
        .into_any_element()
}

/// 颜色那一行：`ANNOTATION_COLORS` 那五档按库里的顺序摆。
///
/// 格子与上面那行同宽（`MENU_CELL`）。挑中的正是库里某条划线时，它那一档的圆点上叠一枚
/// 勾（`check` 是页面底色，压在色块上对比得出）；普通选区没有勾 —— 挑选中的是哪一个
/// 颜色，光靠色块本身看不出来。点哪一格：已有划线改色，普通选区就用这个色划一条。
fn swatch_row(
    reader: Entity<ReaderSession>,
    palette: &Palette,
    check: Hsla,
    current: Option<&str>,
) -> AnyElement {
    h_flex()
        .gap_0()
        .children(ANNOTATION_COLORS.iter().enumerate().map(|(ix, name)| {
            let name = *name;
            let active = current == Some(name);
            let reader = reader.clone();
            div()
                .id(("selection-swatch", ix))
                .w(px(MENU_CELL))
                .h(px(MENU_SWATCH))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(
                    div()
                        .w(px(SWATCH_DOT))
                        .h(px(SWATCH_DOT))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(SWATCH_DOT / 2.0))
                        .bg(palette.color(name))
                        .children(active.then(|| {
                            div()
                                .text_size(px(SWATCH_CHECK))
                                .text_color(check)
                                .child(Icon::new(IconName::Check))
                        })),
                )
                .on_click(move |_, _, cx| {
                    reader.update(cx, |reader, cx| match reader.selection_annotation() {
                        Some(_) => reader.set_highlight_color(name, cx),
                        None => reader.add_highlight_with(name, cx),
                    });
                })
                .into_any_element()
        }))
        .into_any_element()
}

/// 指向选区的那条小尾巴：一个实心三角形，填色与卡片同一份 `popover` 底色。
fn menu_tail(up: bool, surface: Hsla) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let (tip, left, right) = if up {
                (
                    point(bounds.center().x, bounds.top()),
                    point(bounds.left(), bounds.bottom()),
                    point(bounds.right(), bounds.bottom()),
                )
            } else {
                (
                    point(bounds.center().x, bounds.bottom()),
                    point(bounds.left(), bounds.top()),
                    point(bounds.right(), bounds.top()),
                )
            };
            let mut fill = PathBuilder::fill();
            fill.move_to(tip);
            fill.line_to(left);
            fill.line_to(right);
            fill.close();
            if let Ok(path) = fill.build() {
                window.paint_path(path, surface);
            }
        },
    )
    .w(px(MENU_TAIL * 2.0))
    .h(px(MENU_TAIL))
    .self_center()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    //! 只钉这一层唯一值得钉的判断：**摆位会不会翻面、卡片多高**。格子尺寸、图标、尾巴形状
    //! 都是画出来的东西，测了只是把常数抄进断言里两遍。
    //!
    //! 点名导入而不 `use super::*`：`gpui_kit` 根把 `gpui` 整个转了出来，里面有一个
    //! 叫 `test` 的属性宏，glob 会让下面的 `#[test]` 递归展开成它。

    use super::{MENU_CELL, MENU_PAD, MENU_SWATCH, MENU_TAIL, Slot, card_height, slot};

    /// 卡片总高（动作行 + 恒在的颜色行）。摆位只认递进去的这个数。
    const CARD: f32 = card_height();

    /// 顶部够用时摆在选区之上，尾巴朝下。
    #[test]
    fn the_menu_sits_above_the_selection_when_there_is_room() {
        let first_top = CARD + 8.0;
        assert_eq!(
            slot(Some(first_top), Some(first_top + 30.0), 900.0, CARD),
            Slot::Above(first_top - CARD - 6.0)
        );
    }

    /// 上面放不下就翻到末行之下 —— 这条是「首行被页眉吃掉时菜单去哪」的全部答案。
    #[test]
    fn it_flips_below_when_the_top_is_taken() {
        let last_bottom = 400.0;
        assert_eq!(
            slot(Some(20.0), Some(last_bottom), 900.0, CARD),
            Slot::Below(last_bottom + 6.0)
        );
    }

    /// 临界点：刚好够就朝上，差一像素就翻面。留那 8 像素是为了不贴住屏幕顶边。
    #[test]
    fn the_flip_happens_at_the_height_plus_the_safety_margin() {
        assert!(matches!(
            slot(Some(CARD + 8.0), Some(600.0), 900.0, CARD),
            Slot::Above(_)
        ));
        assert!(matches!(
            slot(Some(CARD + 7.0), Some(600.0), 900.0, CARD),
            Slot::Below(_)
        ));
    }

    /// 菜单恒是两行：摆位用的高必须把颜色那一行算进去。漏掉 `MENU_SWATCH` 那一项，
    /// 卡片就会比算出来的高多出一行、贴到屏幕顶边上去 —— 这条就是防那个的。
    #[test]
    fn the_card_height_includes_the_color_row() {
        let actions_only = MENU_CELL + MENU_PAD * 2.0 + MENU_TAIL;
        assert!(
            CARD >= actions_only + MENU_SWATCH,
            "卡片高没把颜色那一行算进去：{CARD}"
        );
    }

    /// 下面也放不下（选区贴在视口底边）时夹在视口内，而不是掉出屏幕外。
    #[test]
    fn a_below_slot_never_leaves_the_viewport() {
        let viewport = 600.0;
        let Slot::Below(top) = slot(Some(20.0), Some(viewport), viewport, CARD) else {
            panic!("顶部放不下该翻到下面");
        };
        assert!(top + CARD <= viewport, "菜单底边越过了视口：{top}");
        // 视口矮到装不下整块菜单时也不给负数：负顶边意味着菜单自己跑出屏幕。
        let Slot::Below(tiny) = slot(Some(20.0), Some(40.0), 100.0, CARD) else {
            panic!("同上");
        };
        assert!(tiny >= 2.0, "顶边不该是负数：{tiny}");
    }

    /// 没有高亮可锚（排版还没跟上、选区滚出屏了）就不画，而不是拿 0 当锚位。
    #[test]
    fn no_anchor_means_no_menu() {
        assert_eq!(slot(None, Some(300.0), 900.0, CARD), Slot::Hidden);
        assert_eq!(slot(Some(300.0), None, 900.0, CARD), Slot::Hidden);
        assert_eq!(slot(None, None, 900.0, CARD), Slot::Hidden);
    }
}
