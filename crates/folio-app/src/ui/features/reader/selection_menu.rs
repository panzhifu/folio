//! 选中的就地菜单：一条**只有图标**的动作条 + 一条颜色胶囊。
//!
//! 样式照读者给的那张参考图量：动作收进一条圆角浮条，图标在上面，**不带文字**；
//! 颜色那五档单独缩成一条更矮的胶囊，压在动作条下面。两块共用同一份表面色与投影，
//! 中间留一道 6 像素的缝 —— 读起来是「一条工具条 + 一排颜色」，而不是一张两行的卡片。
//!
//! 独立成一份文件是因为它是阅读页里**改动最频繁**的一块：加一项、换图标、调格子
//! 的尺寸与摆位，动到的都只有这一处，而它原先住在 `view.rs` 里 —— 那个文件是
//! 「排版结果 → 元素」的渲染主干，两者改的从来不是同一件事。
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
//! 「笔记」那一格是 M7 的入口：普通选区打开一张空白便签，挑中库里某条划线则打开它
//! 已经写下的那条（图标换成带勾的便签，提示「这一条有笔记」）。
//!
//! 摆位分两半：[`slot`] 是**纯函数**（首末高亮的上下边 + 视口高 + 卡片高 → 摆在哪一边、
//! 顶边在哪一像素），所以「上面放不下就翻到下面」这条规则能在宿主上直接钉死；渲染那一侧
//! 只负责把 [`Slot`] 落成元素。

use folio_core::store::ANNOTATION_COLORS;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme, Icon, h_flex, v_flex};
use gpui_kit::{
    AnyElement, BoxShadow, Context, Entity, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, PathBuilder, SharedString, StatefulInteractiveElement as _, Styled, Window,
    canvas, div, hsla, point, px,
};

use super::modules::annotations::Palette;
use super::session::ReaderSession;
use crate::ui::shared::l10n::tr;

/// 动作条里一格的可点边长（逻辑像素）。拇指能稳定命中的下限是 44，而图标本身只有
/// 20：这里取 40 是参考图里那排图标的呼吸感，一颗一颗之间不挨着。
const ACTION_CELL: f32 = 40.0;
/// 图标本身多大。参考图里那一排图标约 22，落在 40 的格子里正好。
const ACTION_ICON: f32 = 21.0;
/// 动作条的内缩：上下左右各 4，整条于是 48 高。
const BAR_PAD: f32 = 4.0;
/// 颜色胶囊的内缩与高：五枚 20 的圆点，上下各留 6。
const SWATCH_PAD: f32 = 6.0;
const SWATCH_DOT: f32 = 20.0;
/// 一枚颜色格子的可点宽。比圆点宽 6，五枚连起来仍然比动作条窄。
const SWATCH_CELL: f32 = 28.0;
/// 颜色胶囊的高（与摆位共用）。
const SWATCH_BAR: f32 = SWATCH_DOT + SWATCH_PAD * 2.0;
/// 动作条与颜色胶囊之间的缝。
const BAR_GAP: f32 = 6.0;
/// 指向选区那条小尾巴的高与宽。
const MENU_TAIL: f32 = 8.0;
const MENU_TAIL_WIDTH: f32 = 16.0;
/// 菜单与选区之间的缝，也是它与视口上下边的最小间距。
const MENU_GAP: f32 = 6.0;
/// 叠在圆点上的那枚勾的字号（`Icon` 取当前文字号）。
const SWATCH_CHECK: f32 = 13.0;

/// 动作条那一行的宽：四格 + 左右内缩。整块菜单的横向居中按它算。
const ACTION_BAR_WIDTH: f32 = ACTION_CELL * 4.0 + BAR_PAD * 2.0;

/// 整块菜单占的高（含尾巴）：摆位与「放不下就翻面」都按这个数算。
/// 动作条 + 缝 + 颜色胶囊 + 尾巴；颜色那一行**恒在**，所以没有「一行 / 两行」两种高。
pub(in crate::ui::features::reader) const fn card_height() -> f32 {
    ACTION_CELL + BAR_PAD * 2.0 + BAR_GAP + SWATCH_BAR + MENU_TAIL
}

/// 整块菜单的宽。颜色胶囊比动作条窄，真正决定横向占位的是动作条。
pub(in crate::ui::features::reader) const fn card_width() -> f32 {
    ACTION_BAR_WIDTH
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
        anchor_x: Option<f32>,
    ) -> Option<AnyElement> {
        let (top, tail_up) = match slot {
            Slot::Above(top) => (*top, false),
            Slot::Below(top) => (*top, true),
            // 没有锚位：调用方连元素都不该要，`menu_bounds` 由它一并收掉。
            Slot::Hidden => return None,
        };
        let reader = cx.entity().clone();
        let bounds_cell = self.menu_bounds.clone();
        let surface = cx.theme().muted;
        let foreground = cx.theme().foreground;
        let hover = cx.theme().border;
        let palette = Palette::of(cx.theme());
        // 勾选标记压在圆点上，取的是页面底色：浅色主题圆点是深色（`*-600`）、底色近白，
        // 深色主题圆点是浅色（`*-400`）、底色近黑 —— 一个数在两套主题里都够对比。
        let check = cx.theme().background;
        let copy = tr(cx, "reader.sel_copy");
        let highlight = tr(cx, "reader.sel_highlight");
        let unhighlight = tr(cx, "reader.sel_unhighlight");
        let cancel = tr(cx, "reader.sel_cancel");
        // 挑中的正是留在书上的那一条时，第二格改做「取消划线」；下面那一行颜色恒在，
        // 只是给它那一档多打一枚勾。
        let marked = self.selection_annotation();
        let has_note = marked
            .as_ref()
            .is_some_and(|(id, _)| self.annotation_has_note(*id));
        let mark_cell = match marked {
            Some(_) => action_cell(
                "selection-unhighlight",
                IconName::Trash,
                unhighlight,
                foreground,
                hover,
                reader.clone(),
                |r, _, cx| r.delete_highlight(cx),
            ),
            None => action_cell(
                "selection-highlight",
                IconName::Highlighter,
                highlight,
                foreground,
                hover,
                reader.clone(),
                |r, _, cx| r.add_highlight(cx),
            ),
        };
        let note_icon = if has_note {
            IconName::StickyNoteCheck
        } else {
            IconName::StickyNote
        };
        // 颜色那一行恒在。挑中的正是库里某条划线时，给它那一档打勾、点色即换色；
        // 普通选区没有「当下那一档」，点色即「就用这个色划一条」。
        let current = marked.as_ref().map(|(_, color)| color.as_str());
        let actions = h_flex()
            .p(px(BAR_PAD))
            .gap_0()
            .rounded(px(14.0))
            .bg(surface)
            .shadow(floating_shadow())
            .child(action_cell(
                "selection-copy",
                IconName::Copy,
                copy,
                foreground,
                hover,
                reader.clone(),
                |r, _, cx| r.copy_selection(cx),
            ))
            .child(mark_cell)
            .child(action_cell(
                "selection-note",
                note_icon,
                tr(cx, "reader.sel_note"),
                foreground,
                hover,
                reader.clone(),
                |r, window, cx| r.open_note_editor(window, cx),
            ))
            .child(action_cell(
                "selection-cancel",
                IconName::X,
                cancel,
                foreground,
                hover,
                reader.clone(),
                |r, _, cx| {
                    r.clear_selection();
                    cx.notify();
                },
            ));
        let swatches = swatch_row(reader.clone(), &palette, surface, check, current);
        // 横向对着选区中线；中线不在手上（首帧还没画过）就居中在正文那一条上。
        // 两种摆法都夹在页内，不把卡片顶出屏幕。
        let page_width = f32::from(self.content_bounds.get().size.width);
        let left = menu_left(page_width, self.prefs.margin_x, card_width(), anchor_x);
        let mut bars = v_flex().items_center().child(actions).child(
            // 一条空 div 当缝：`card_height` 算的就是这 6 像素，不能靠 `gap` 的两套账。
            div().h(px(BAR_GAP)).flex_none(),
        );
        bars = bars.child(swatches);
        Some(
            v_flex()
                .id("selection-menu")
                // 菜单的点击必须先被它自己吃掉：正文的 hitbox 是整屏的，gpui 会把同一下
                // 依次发给每个含按下点的 handler。少了这一条，点「划线」会同时命中正文，
                // 轻则多画一帧，重则把顶栏底栏也翻出来 —— 读者看到的就是闪一下。
                .occlude()
                .absolute()
                .left(px(left))
                .top(px(top))
                .items_center()
                .children(tail_up.then(|| menu_tail(true, surface)))
                .child(bars)
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

/// 动作条里的一格。只有图标；`label` 不画出来，只留给无障碍与排障时读。
///
/// 图标画多大由 `text_size` 定（`Icon` 取的是当前文字号）。参考图里那一排没有文字，
/// 所以 `label` 只进 `accessibility`；不要把它当成「文字太小看不见」。
fn action_cell(
    id: &'static str,
    icon: IconName,
    label: SharedString,
    foreground: Hsla,
    hover: Hsla,
    reader: Entity<ReaderSession>,
    action: impl Fn(&mut ReaderSession, &mut Window, &mut Context<ReaderSession>) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .w(px(ACTION_CELL))
        .h(px(ACTION_CELL))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(9.0))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(
            div()
                .text_size(px(ACTION_ICON))
                .text_color(foreground)
                .child(Icon::new(icon)),
        )
        // `label` 在这条链里没有可见的位置：图标按钮的说明只走无障碍。
        .aria_label(label)
        .on_click(move |_, window, cx| {
            reader.update(cx, |reader, cx| action(reader, window, cx));
        })
        .into_any_element()
}

/// 颜色那一行：`ANNOTATION_COLORS` 那五档按库里的顺序摆。
///
/// 挑中的正是库里某条划线时，它那一档的圆点上叠一枚勾（`check` 是页面底色，压在色块上
/// 对比得出）；普通选区没有勾 —— 挑选中的是哪一个颜色，光靠色块本身看不出来。
/// 点哪一格：已有划线改色，普通选区就用这个色划一条。
fn swatch_row(
    reader: Entity<ReaderSession>,
    palette: &Palette,
    surface: Hsla,
    check: Hsla,
    current: Option<&str>,
) -> AnyElement {
    h_flex()
        .px(px(SWATCH_PAD))
        .h(px(SWATCH_BAR))
        .gap_0()
        .rounded(px(SWATCH_BAR / 2.0))
        .bg(surface)
        .shadow(floating_shadow())
        .children(ANNOTATION_COLORS.iter().enumerate().map(|(ix, name)| {
            let name = *name;
            let active = current == Some(name);
            let reader = reader.clone();
            div()
                .id(("selection-swatch", ix))
                .w(px(SWATCH_CELL))
                // 整格 28×32 都可点：圆点只有 20，命中区按视觉尺寸给就会很难按中。
                .h(px(SWATCH_BAR))
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

/// 菜单横向落在哪儿：优先对着选区中线，没有锚点就居中；两边都夹在页内。
///
/// `page_width` 是正文那一栏的宽（不是窗口宽：分页/双页时正文只占其中一栏）。
/// `anchor_x` 是选区首末高亮中线的平均，单位与正文元素同一套局部坐标。
pub(in crate::ui::features::reader) fn menu_left(
    page_width: f32,
    margin: f32,
    width: f32,
    anchor_x: Option<f32>,
) -> f32 {
    // 页宽还没量到的那一帧，夹出来的上界会小于下界：统一退到左边距。
    let max_left = (page_width - width - margin).max(margin);
    let wanted = match anchor_x {
        Some(x) if page_width > width + margin * 2.0 => x - width * 0.5,
        _ if page_width > width + margin * 2.0 => (page_width - width) * 0.5,
        _ => return margin,
    };
    wanted.clamp(margin, max_left)
}

/// 浮条那层投影：与顶栏同一档的柔影，不借 `popover_style` 的 ring ——
/// 参考图里那两条是压在纸上的轻浮层，描一圈边会读成「另一个窗口」。
fn floating_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.), px(2.), hsla(0., 0., 0., 0.18)).blur_radius(px(8.))]
}

/// 指向选区的那条小尾巴：一个实心三角形，填色与浮条同一份表面色。
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
    .w(px(MENU_TAIL_WIDTH))
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

    use super::{
        ACTION_CELL, BAR_GAP, BAR_PAD, MENU_TAIL, SWATCH_BAR, Slot, card_height, menu_left, slot,
    };

    /// 卡片总高（动作条 + 颜色胶囊）。摆位只认递进去的这个数。
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

    /// 菜单恒是两块：摆位用的高必须把颜色胶囊与中间的缝都算进去。漏掉任何一项，
    /// 卡片就会比算出来的高多出一截、贴到屏幕顶边上去 —— 这条就是防那个的。
    #[test]
    fn the_card_height_includes_the_swatches_and_the_gap() {
        let actions_only = ACTION_CELL + BAR_PAD * 2.0 + MENU_TAIL;
        assert!(
            CARD >= actions_only + SWATCH_BAR + BAR_GAP,
            "卡片高没把颜色胶囊或中间那道缝算进去：{CARD}"
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

    /// 横向摆位：对着选区中线，但永远夹在页内；首帧没有中线时居中。
    #[test]
    fn the_menu_stays_inside_the_page() {
        let width = 168.0;
        let margin = 24.0;
        assert_eq!(menu_left(1000.0, margin, width, Some(500.0)), 416.0);
        assert_eq!(menu_left(1000.0, margin, width, Some(0.0)), margin);
        assert_eq!(menu_left(1000.0, margin, width, Some(1000.0)), 808.0);
        assert_eq!(menu_left(1000.0, margin, width, None), 416.0);
        // 页宽还没量到、或窄到放不下整条：退到左边距，不给负数也不顶出右边。
        assert_eq!(menu_left(0.0, margin, width, Some(500.0)), margin);
        assert_eq!(menu_left(150.0, margin, width, None), margin);
    }
}
