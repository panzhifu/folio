//! 选中（M6）一份文件装全：交互状态机、绘制件、就地菜单条。
//!
//! 这三半讲的是同一件事 ——「屏幕上这一段字被挑中了」：状态机把事件翻译成选区，绘制件把
//! 选区画成高亮与两端把手，菜单条把选区能做的几件事摆出来。它们原先住在
//! `modules/selection.rs` + `paint.rs` + `selection_menu.rs` 三份文件里，而改一处常常
//! 三处都要动（加一个动作、换一种把手尺寸、调菜单翻面的余量），所以合在这里。
//!
//! **状态字段不住在这儿**，仍在 [`ReaderSession`](crate::ui::features::reader::model::session::ReaderSession) 上（与
//! `LaidOut` / `Source` 同一批）：要读写的正是会话自己那点状态，拆成一个被会话持有的子
//! struct 只会多一层转发。选区存的是**章内可见文本坐标**
//! （`folio_core::content::anchor::TextMap` 那套「第几个字」），与进度、批注锚点同源：
//! 滚动不动它，重排不丢它，换章必须丢（坐标系换了）。
//!
//! 几何知识全部在引擎（`folio_core::content::hit`：命中、吸词、选区矩形），这里只做三件事：
//! 把屏幕点换算成排版点、把交互事件翻译成状态迁移、把「复制」落进剪贴板。
//! 交互规矩照搬 gpui-kit 的 `TextSelectionLayer` / `touch_selection`：
//!
//! - **长按**（触摸）：`LongPress` Started 吸一个词，手指继续拖就绕词锚点扩展
//!   （`capture_long_press` 后续 Moved/Ended 都归我们），松手成选、把手与菜单条出现。
//! - **指针**（桌面与触控板）：按下只记锚点，**拖过 4px 阈值才成立选中**——普通点击
//!   不留高亮；拖选松手的那一下 `ClickEvent` 由 `suppress_tap` 吃掉，否则
//!   `page::on_body_tap` 会把刚选的清掉。
//! - **把手**：松手后两端各留一个（杆 + 圆钮，那份尺寸与命中区都取自 gpui-kit 的
//!   `TouchHandle`，不在这儿另定一套），是长按之后**唯一**能改
//!   选中的路。拖动期间记「手指→插入符」偏移（圆钮长在行外，手指不压在文字上）；
//!   拖到另一端正上停住不塌缩；拖过另一端两端互换、手指改握新的那头；拖动期间
//!   菜单条让位、松手即回。
//! - **点按**：选中态下点选区外清空、点选区内把菜单条请回来，两下都消费这一击
//!   （不翻页）；没有选区时点在一条**已有的划线**上就是把那一条重新选中（判断与命中
//!   在 `modules/annotations.rs`）；浮层（顶栏/底栏）开着时指针让位——那一击的语义是收浮层。
//!
//! 与 [`paint`](crate::ui::features::reader::view::paint) 的边界只有一条：`GlyphsElement` 那份元素留在 paint.rs（它
//! 同时还要画字形与批注下划线），选中的那一格 `selection: SelectionVisual` 与三个调用点
//! （画高亮、注册监听、画把手）递到这里。事件监听必须在绘制期注册 —— gpui 的
//! `window.on_mouse_event` 只活到本帧事件分发结束，所以 [`register`] 是被 paint 调的，
//! 不是反过来。
//!
//! 菜单条是压在正文之上的**真按钮**而不是浮层：见 `page.rs` 模块注释与 `plans/facts.md`。
//! 它画出的位置经 `canvas` 回写进会话的 `menu_bounds`：压在它上面的一击属于菜单，
//! 不算「点别处清除」（[`ReaderSession::tap_with_selection`]）。
//!
//! 样式照读者给的那张参考图量：动作收进一条圆角浮条，图标在上面，**不带文字**；
//! 上面那一行是**两条胶囊分列两端** —— 左边三格「划线方式」（底色带 / 字下实线 / 字下波浪），
//! 右边五枚颜色圆点，各自带一份表面色与投影，顶到整块菜单的左右两边；两块与下面那条
//! 动作条同一档投影，中间留一道 6 像素的缝。
//!
//! 整块菜单的宽由「那一行」与「动作条」里较宽的一边定（[`card_width`] 取两者的较大者）：
//! 今天那一行是 260，动作条按可选那三格摆不摆是 208 / 248 / 288 / 328 —— 桌面上分享
//! 那一格不摆，所以常常窄于那一行；手机上几格都摆时反过来由动作条定。动作条在它下面
//! 自己居中 ——「两端对齐」说的是那一行里那两组各顶各的边，不是三块同宽。
//!
//! 第二格跟着选区自己是谁换：选区**正好等于**库里某一条划线时它是「取消划线」
//! （[`ReaderSession::selection_annotation`]）；改过范围的选区就是普通选区，那一格回到
//! 「划线」。上面那一行的画法与颜色**恒在**：挑中某条划线时点它们＝改那一条，
//! 普通选区点它们＝就用这一档划一条 —— 于是「划线」那一格给的是默认那档黄加配置里那一格
//! 画法，想换个色直接点圆点，想换种画法直接点左边那三格。
//!
//! 「笔记」那一格是 M7 的入口：普通选区打开一张空白便签，挑中库里某条划线则打开它
//! 已经写下的那条（图标换成带勾的便签，提示「这一条有笔记」）。
//!
//! 「分享卡片」那一格只在**能把视图离屏渲成图的那一端**摆（判据
//! [`crate::ui::platform::RENDERS_SHARE_CARDS`]），整块菜单的宽跟着它一起算，见
//! [`card_width`]；卡片本体的画法在 `share_card.rs`。
//!
//! 摆位分两半：[`slot`] 是**纯函数**（首末高亮的上下边 + 视口高 + 卡片高 → 摆在哪一边、
//! 顶边在哪一像素），所以「上面放不下就翻到下面」这条规则能在宿主上直接钉死；渲染那一侧
//! 只负责把 [`Slot`] 落成元素。

use std::ops::Range;

use folio_core::store::{ANNOTATION_COLORS, AnnotationStyle};
use gpui_kit::assets::IconName;
use gpui_kit::base::{ColorSwatch, SelectionEdge, TouchHandle};
use gpui_kit::component::{ActiveTheme, Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, BoxShadow, ClipboardItem, Context, Entity, Hsla,
    InteractiveElement as _, IntoElement, LongPressEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, PathBuilder, Pixels, Point, SharedString,
    StatefulInteractiveElement as _, Styled, TouchDragEvent, TouchPhase, WeakEntity, Window,
    canvas, div, fill, hsla, point, px, size,
};

use folio_core::content::format::PageText;

use crate::ui::features::reader::components::share_card;
use crate::ui::features::reader::model::annotations::Palette;
use crate::ui::features::reader::model::book::Book;
use crate::ui::features::reader::model::chapter_layout::Document;
use crate::ui::features::reader::model::chrome;
use crate::ui::features::reader::model::fixed_book::FixedBook;
use crate::ui::features::reader::model::session::{
    DRAG_THRESHOLD_PX, EdgeDrag, MouseDrag, ReaderSession, Selection,
};
use crate::ui::platform;
use crate::ui::services::l10n::tr;

// ====================== 绘制件（由 paint.rs 的字形元素调用）======================

/// 一屏里选中的全部绘制信息，元素局部坐标（逻辑像素）。
///
/// 由 `view.rs` 按「排版矩形 → 屏幕矩形」换算好整批传入；绘制与命中都不再碰排版层。
#[derive(Default, Clone)]
pub(in crate::ui::features::reader) struct SelectionVisual {
    /// 每行一条的高亮矩形，压在字形之下。
    pub highlights: Vec<Bounds<Pixels>>,
    /// 两端把手的插入符盒（0 宽、一行高，长在首个/末个选中字符的字缘上）。
    /// 那一端被裁出可视范围时不给 —— 把手跟着它标记的字消失，与 gpui-kit 一致。
    pub start_caret: Option<Bounds<Pixels>>,
    pub end_caret: Option<Bounds<Pixels>>,
    /// 把手的颜色（主题的 selection 色）。
    pub color: Hsla,
    /// 高亮那一块另给一色的口子。**只有定版式那一族用它**。
    ///
    /// 理由是两族的层级相反：可重排的高亮压在字形**之下**，实色不会吃掉笔画；定版式的
    /// 字在那张栅格出来的图里，高亮只能压在图**之上**，实色就把读者要选的那几个字糊掉了。
    /// 所以那一族递一个压过透明度的色，而把手仍用上面那个实色 —— 一条半透明的把手
    /// 在一页密密麻麻的字上根本找不着。
    ///
    /// `None` = 与把手同色（可重排那一族的从前）。
    pub highlight_color: Option<Hsla>,
}

/// 高亮矩形：压在字形**之下**画，所以必须在字形之前调用。
///
/// `origin` 是字形元素在窗口里的原点 —— 传进来的矩形是元素局部坐标，跟字形与把手一样要
/// 加上它：阅读视图不在窗口原点（Android 的系统栏 inset、桌面的侧栏）时，漏加这一个偏移，
/// 高亮就会整片飘到文字上方。
pub(in crate::ui::features::reader) fn paint_highlights(
    visual: &SelectionVisual,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    let color = visual.highlight_color.unwrap_or(visual.color);
    for rect in &visual.highlights {
        window.paint_quad(fill(*rect + origin, color));
    }
}

/// 两端把手：压在字形**之上**画，所以必须在字形之后调用。缺哪一端的插入符就不画哪一端
/// （那一端被裁出可视范围）。
///
/// 形状（杆沿插入符立起来、圆钮长在行外 —— 起点在上、终点在下）与命中区尺寸都取自
/// [`TouchHandle`]，即 gpui-kit 自家输入框画把手用的那一份，不是这里另定一套。
pub(in crate::ui::features::reader) fn paint_handles(
    visual: &SelectionVisual,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    if let Some(caret) = visual.start_caret {
        TouchHandle::paint(SelectionEdge::Start, caret + origin, visual.color, window);
    }
    if let Some(caret) = visual.end_caret {
        TouchHandle::paint(SelectionEdge::End, caret + origin, visual.color, window);
    }
}

/// 把这一帧的选中几何交给会话（内容边界、把手命中区），并注册手势监听。
///
/// `window.on_mouse_event` 注册的监听只活到本帧事件分发结束，所以每个绘制帧
/// 都要重新挂一遍 —— 这与 gpui-kit 的 `TextSelectionLayer` 同一条规矩。监听收到
/// 的是窗口里**所有**同类事件，落到会话的方法里再按范围与状态过滤。
pub(in crate::ui::features::reader) fn register(
    visual: &SelectionVisual,
    reader: &WeakEntity<ReaderSession>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(entity) = reader.upgrade() else {
        return;
    };
    let handle_hits = {
        let session = entity.read(cx);
        session.content_bounds.set(bounds);
        session.handle_hits.clone()
    };
    // 插入符盒换算成窗口坐标：状态机这一侧只认这一份。
    *handle_hits.borrow_mut() = [
        (SelectionEdge::Start, visual.start_caret),
        (SelectionEdge::End, visual.end_caret),
    ]
    .into_iter()
    .flat_map(|(edge, caret)| {
        caret.map(|caret| (edge, TouchHandle::hit_bounds(edge, caret) + bounds.origin))
    })
    .collect();

    // 指针按下（capture）：新的一手开始。上一手攒下的 click 豁免在这里作废——
    // 正常它已被拖选松手的那一下 click 消费掉，没消费说明那一下没落在正文上。
    let down_reader = reader.clone();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if !phase.capture() || event.button != MouseButton::Left {
            return;
        }
        let _ = down_reader.update(cx, |reader, cx| {
            reader.suppress_tap = false;
            if reader.content_bounds.get().contains(&event.position) {
                reader.begin_mouse_selection(window, cx, event.position);
            }
        });
    });

    // 指针拖动（bubble）：把手拖动优先，否则过了阈值进入拖选。
    let move_reader = reader.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if !phase.bubble() || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let _ = move_reader.update(cx, |reader, cx| {
            reader.drag_mouse_selection(window, cx, event.position)
        });
    });

    // 指针抬起（bubble）：拖选成过就拦掉紧跟着的 click。不按落点过滤——
    // 拖出正文松手也要把拖动状态收掉。
    let up_reader = reader.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if !phase.bubble() || event.button != MouseButton::Left {
            return;
        }
        let _ = up_reader.update(cx, |reader, cx| reader.end_mouse_selection(cx));
    });

    // 长按（触摸）：500ms 后由手势识别器送来。Started 落在字上才抢占这一手势，
    // 抢占后手指的移动/抬起都从这里走（后续选区绕词锚点扩展）。
    let long_press_reader = reader.clone();
    window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
        if !phase.bubble() {
            return;
        }
        let Some(entity) = long_press_reader.upgrade() else {
            return;
        };
        match event.phase {
            TouchPhase::Started => {
                let began = entity.update(cx, |reader, cx| {
                    reader.begin_long_press(window, cx, event.start_position)
                });
                if !began {
                    return;
                }
                window.capture_long_press(&entity);
                window.prevent_default();
                cx.stop_propagation();
            }
            _ if window.has_long_press_capture(&entity) => {
                entity.update(cx, |reader, cx| match event.phase {
                    TouchPhase::Moved => reader.drag_long_press(window, cx, event.position),
                    TouchPhase::Ended | TouchPhase::Cancelled => reader.end_long_press(cx),
                    TouchPhase::Started => {}
                });
                window.prevent_default();
                cx.stop_propagation();
            }
            _ => {}
        }
    });

    // 触摸拖动：落在把手上才认领（prevent_default 把它从「滚动」里抢过来），
    // 认领后的移动/抬起归把手拖动；落在别处什么都不做，滚动照常。
    let drag_reader = reader.clone();
    window.on_mouse_event(move |event: &TouchDragEvent, phase, window, cx| {
        if !phase.bubble() {
            return;
        }
        let Some(entity) = drag_reader.upgrade() else {
            return;
        };
        match event.phase {
            TouchPhase::Started => {
                let edge = entity.read_with(cx, |reader, _| {
                    reader
                        .content_bounds
                        .get()
                        .contains(&event.position)
                        .then(|| reader.handle_at(event.position))
                        .flatten()
                });
                let Some(edge) = edge else {
                    return;
                };
                entity.update(cx, |reader, _| reader.begin_edge_drag(edge, event.position));
                window.prevent_default();
                cx.stop_propagation();
            }
            _ => {
                let dragging = entity.read_with(cx, |reader, _| reader.edge_drag.is_some());
                if !dragging {
                    return;
                }
                cx.stop_propagation();
                entity.update(cx, |reader, cx| match event.phase {
                    TouchPhase::Moved => reader.update_edge_drag(window, cx, event.position),
                    TouchPhase::Ended | TouchPhase::Cancelled => reader.end_edge_drag(cx),
                    TouchPhase::Started => {}
                });
            }
        }
    });
}

// ====================== 交互状态机（写在会话上）======================

impl ReaderSession {
    // ---- 查询：视图与 page.rs 用 ----

    /// 当前选中（挂在某一章上的章内字符坐标，含头不含尾）。
    pub(in crate::ui::features::reader) fn selection(&self) -> Option<Selection> {
        self.selection.clone()
    }

    /// 菜单条现在该不该摆出来。拖动期间收起（让位），松手即回；笔记那张便签压着时
    /// 也不摆 —— 两层同时上屏就是两套输入焦点在抢同一只手。
    pub fn selection_menu_open(&self) -> bool {
        self.selection_menu_open && self.edge_drag.is_none() && self.note_editor.is_none()
    }

    /// 浮层（顶栏/底栏）开合，由 `page.rs` 每帧报告。开着时指针让位：
    /// 那一击的语义是收浮层，不是选字。
    pub fn set_overlay_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.overlay_open != open {
            self.overlay_open = open;
            cx.notify();
        }
    }

    // ---- 长按（触摸）：gpui 的手势识别器 500ms 后送 LongPress ----

    /// 长按落词。返回 `false` = 这一下没落在字上，调用方别抢占手势。
    pub(in crate::ui::features::reader) fn begin_long_press(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) -> bool {
        if self.overlay_open || self.note_editor.is_some() || self.turn_in_flight() {
            return false;
        }
        let Some((chapter, word)) = self.word_at(window, cx, at) else {
            return false;
        };
        self.long_press_anchor = Some((chapter, word.start));
        self.selection = Some(Selection {
            chapter,
            range: word,
        });
        self.selection_menu_open = false;
        self.mouse_drag = None;
        cx.notify();
        true
    }

    /// 长按之后手指继续拖：活动端绕词锚点扩展，反向拖两端互换。
    /// 拖出锚点所在的那一章就停住：章内坐标不能跨章比较。
    pub(in crate::ui::features::reader) fn drag_long_press(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) {
        let Some((anchor_chapter, anchor)) = self.long_press_anchor else {
            return;
        };
        let Some((chapter, offset)) = self.offset_at(window, cx, at) else {
            return;
        };
        if chapter != anchor_chapter || offset == anchor {
            return; // 停在锚点上不塌缩
        }
        self.selection = Some(Selection {
            chapter: anchor_chapter,
            range: anchor.min(offset)..anchor.max(offset),
        });
        cx.notify();
    }

    /// 长按手势结束：成选就把菜单条请出来，拖塌了就当没选中过。
    pub(in crate::ui::features::reader) fn end_long_press(&mut self, cx: &mut Context<Self>) {
        self.long_press_anchor = None;
        let non_empty = self
            .selection
            .as_ref()
            .is_some_and(|selected| selected.range.start < selected.range.end);
        if non_empty {
            self.selection_menu_open = true;
        } else {
            self.clear_selection();
        }
        cx.notify();
    }

    // ---- 指针（桌面）：按下记锚点，拖过阈值才成选 ----

    /// 指针按下。把手命中就去拖把手，否则只记锚点——普通点击到此为止，
    /// 交给 click 的分区逻辑；拖过阈值才算「在选中」。
    pub(in crate::ui::features::reader) fn begin_mouse_selection(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) {
        // 新的一手开始，上一手拖选攒下的豁免到此为止（正常情况下它已被
        // 拖选松手的那一下 click 消费掉了；没消费说明那一下没落在正文上，丢弃）。
        self.suppress_tap = false;
        if let Some(edge) = self.handle_at(at) {
            self.begin_edge_drag(edge, at);
            return;
        }
        if self.overlay_open || self.note_editor.is_some() || self.turn_in_flight() {
            return;
        }
        let Some((chapter, offset)) = self.offset_at(window, cx, at) else {
            return;
        };
        self.mouse_drag = Some(MouseDrag {
            chapter,
            anchor: offset,
            origin: at,
            dragged: false,
        });
    }

    /// 指针拖动：把手拖动优先；否则过了阈值就进入拖选，选区从锚点跟着手指走。
    /// 拖出起手的那一章就停：章内坐标不能跨章比较。
    pub(in crate::ui::features::reader) fn drag_mouse_selection(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) {
        if self.edge_drag.is_some() {
            self.update_edge_drag(window, cx, at);
            return;
        }
        let Some(drag) = self.mouse_drag.as_ref() else {
            return;
        };
        let moved = (at - drag.origin).magnitude() > f64::from(DRAG_THRESHOLD_PX);
        if !moved {
            return;
        }
        let (chapter, anchor) = (drag.chapter, drag.anchor);
        let Some((hit_chapter, offset)) = self.offset_at(window, cx, at) else {
            return;
        };
        if hit_chapter != chapter || offset == anchor {
            return;
        }
        self.mouse_drag
            .as_mut()
            .expect("drag 在上面刚检查过")
            .dragged = true;
        self.selection = Some(Selection {
            chapter,
            range: anchor.min(offset)..anchor.max(offset),
        });
        self.selection_menu_open = false;
        cx.notify();
    }

    /// 指针松手。拖选成过才拦 click；普通点击什么都不做（留给分区逻辑）。
    pub(in crate::ui::features::reader) fn end_mouse_selection(&mut self, cx: &mut Context<Self>) {
        if self.edge_drag.is_some() {
            self.end_edge_drag(cx);
            return;
        }
        if let Some(drag) = self.mouse_drag.take()
            && drag.dragged
        {
            self.suppress_tap = true;
            self.selection_menu_open = true;
            cx.notify();
        }
    }

    // ---- 把手：长按之后唯一能改选中的路 ----

    /// 从把手处起拖。`finger` 是按下/起拖的位置，记下它与插入符的偏移——
    /// 圆钮长在行的上/下侧，手指不压在文字上，后续每帧用这个偏移还原「手指指的是哪个字」。
    pub(in crate::ui::features::reader) fn begin_edge_drag(
        &mut self,
        edge: SelectionEdge,
        finger: Point<Pixels>,
    ) {
        let Some(caret) = self.handle_center(edge) else {
            return;
        };
        self.selection_menu_open = false;
        self.mouse_drag = None;
        self.edge_drag = Some(EdgeDrag {
            edge,
            offset: caret - finger,
        });
    }

    /// 把手拖动：手指映射回文字位置后改被拖的那一端。
    /// 拖到另一端正上停住不塌缩；拖过另一端两端互换、手指改握新的那头；
    /// 拖出选区所在的那一章也停。
    pub(in crate::ui::features::reader) fn update_edge_drag(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        finger: Point<Pixels>,
    ) {
        let Some(drag) = self.edge_drag.as_ref() else {
            return;
        };
        let (edge, offset) = (drag.edge, drag.offset);
        let Some(selected) = self.selection.clone() else {
            return;
        };
        let at = point(offset.x + finger.x, offset.y + finger.y);
        let Some((hit_chapter, moving)) = self.offset_at(window, cx, at) else {
            return;
        };
        if hit_chapter != selected.chapter {
            return;
        }
        let fixed = match edge {
            SelectionEdge::Start => selected.range.end,
            SelectionEdge::End => selected.range.start,
        };
        if moving == fixed {
            return; // 正停在另一端上：不塌缩
        }
        let crossed = match edge {
            SelectionEdge::Start => moving > fixed,
            SelectionEdge::End => moving < fixed,
        };
        if crossed {
            // 拖过了另一端：互换，手指从此握的是新的那一头。
            self.edge_drag.as_mut().expect("drag 在上面刚检查过").edge = edge.opposite();
        }
        self.selection = Some(Selection {
            chapter: selected.chapter,
            range: moving.min(fixed)..moving.max(fixed),
        });
        cx.notify();
    }

    /// 把手松手：菜单条回到选区上方。这一下不产生任何 click 语义，
    /// 但保险起见照拖选一样拦掉紧跟着的那一下。
    pub(in crate::ui::features::reader) fn end_edge_drag(&mut self, cx: &mut Context<Self>) {
        if self.edge_drag.take().is_some() {
            self.suppress_tap = true;
            self.selection_menu_open = true;
            cx.notify();
        }
    }

    // ---- 点按（page.rs 的 on_body_tap 进来）----

    /// 拖选/把手拖动松手后，吃掉紧跟着的那一下 click。
    pub fn consume_tap_suppression(&mut self) -> bool {
        std::mem::take(&mut self.suppress_tap)
    }

    /// 选中态下正文上的一击。返回 `true` = 这一击被选中消费（点选区外清空、
    /// 点选区内把菜单条请回来），`page.rs` 不该再拿它去翻页。
    pub fn tap_with_selection(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) -> bool {
        if self.taps_selection_ui(at) {
            return true; // 把手与菜单条自己处理，别当成「点别处」
        }
        let on_selection = self
            .offset_at(window, cx, at)
            .is_some_and(|(chapter, offset)| {
                self.selection.as_ref().is_some_and(|selected| {
                    selected.chapter == chapter && selected.range.contains(&offset)
                })
            });
        if on_selection {
            self.selection_menu_open = true;
        } else {
            self.clear_selection();
        }
        cx.notify();
        true
    }

    /// 正文上的一击要不要被「选中」这一套消费掉（消费了就不算点正文：不开浮层、不翻页）。
    ///
    /// 四条判据的**顺序**就是修的那个 bug 本身，所以它待在会话里而不是 `page.rs`：
    /// 只有在这里，「菜单刚被点掉、选区已经清空」那个窗口期才测得到。
    ///
    /// 1. 拖选 / 拖把手松手紧跟着的那一下 —— 纯粹是合成的 click，谁都不该理它。
    /// 2. 落在菜单或把手上 —— 菜单里那颗「划线」「取消」会在自己的 click 里就把选区
    ///    清掉，所以这条必须在「有没有选区」之前问，否则等这一下冒泡到正文时
    ///    `selection()` 已是 `None`，顶栏与底栏会跟着一起冒出来。
    /// 3. 有选区时交给 [`Self::tap_with_selection`]：点选区内请回菜单、点选区外清空。
    /// 4. 没选区时点在一条**已有的划线**上，就是把那一条重新选中
    ///    （[`Self::reselect_annotation`]）—— 留在书上的痕迹不能只读不能改。
    ///    这一条不分左中右三区：落在底色上的那一击归选中，两侧照常翻页。
    pub fn tap_on_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) -> bool {
        // 点一下正文就是「人还在」的证据 —— 哪怕这一击只把浮层收起来、位置一点没动。
        self.note_reading_activity();
        // 笔记那张便签压着时，正文这一击归它自己的遮罩（正常情况下也到不了这里，
        // 遮罩是 `occlude` 的；这一句是防手势层与元素层差一帧的那一下）。
        if self.note_editor.is_some() || self.turn_in_flight() {
            return true;
        }
        if self.consume_tap_suppression() {
            return true;
        }
        if self.taps_selection_ui(at) {
            return true;
        }
        if self.selection().is_some() {
            return self.tap_with_selection(window, cx, at);
        }
        self.reselect_annotation(window, cx, at)
    }

    /// 这一击是否落在选中的交互件上（把手命中区或菜单条）。这些面上的一击
    /// 属于它们自己，不算「点别处清除」。
    pub(in crate::ui::features::reader) fn taps_selection_ui(&self, at: Point<Pixels>) -> bool {
        self.menu_bounds.get().is_some_and(|b| b.contains(&at))
            || self
                .handle_hits
                .borrow()
                .iter()
                .any(|(_, hit)| hit.contains(&at))
    }

    // ---- 命令（菜单条）----

    /// 复制所选文本进剪贴板。菜单条收起，高亮留着——读者复制完往往还要看清选了什么。
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.selected_text() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.selection_menu_open = false;
        cx.notify();
    }

    /// 「分享这一段」：把选区连同书名与当前位置渲成一张卡片，递给系统的分享面板。
    ///
    /// 卡片不在这里画：三份文字都只有会话读得到（选区的字、书名、位置念法），所以在
    /// 这里收齐，渲染与递交交给 [`crate::ui::features::reader::components::share_card`]。菜单里那一格只在有离屏
    /// 渲染那条路的平台上摆（判据 [`crate::ui::platform::RENDERS_SHARE_CARDS`]），
    /// 命令本身两端都有：桌面上渲不出字节，于是安静地什么都不递。
    pub fn share_selection(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(quote) = self.selected_text() else {
            return;
        };
        let title = self.title();
        let copy = share_card::card_copy(&quote, title.as_ref(), &self.position_label(cx));
        self.selection_menu_open = false;
        cx.notify();
        share_card::present(copy, window.appearance(), cx);
    }

    /// 当前选区里的字。复制与笔记的引文用同一个来源：两处念出来的必须是同一段。
    ///
    /// 定版式那一族走 [`PageText::slice`]：跨行处补一个换行，复制出去还是那一行一行的
    /// 样子。锚点存的那一串**不带**那个换行（它数的是字，见
    /// `content::format::PageText::text_map`），两份串各有各的主人，别在这里合并成一串。
    pub(in crate::ui::features::reader) fn selected_text(&self) -> Option<String> {
        let selected = self.selection.as_ref()?;
        if let Some(book) = self.fixed() {
            let text = book
                .page_text(selected.chapter)
                .map(|text| text.slice(selected.range.clone()))
                .unwrap_or_default();
            return (!text.is_empty()).then_some(text);
        }
        let text = self
            .book()
            .and_then(|book| book.span_doc(selected.chapter))
            .map(|doc| doc.text_map().slice(selected.range.clone()))
            .unwrap_or_default();
        (!text.is_empty()).then_some(text)
    }

    /// 清空选中。菜单条、笔记编辑器与拖动临时态一并收掉。
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_menu_open = false;
        self.note_editor = None;
        self.long_press_anchor = None;
        self.mouse_drag = None;
        self.edge_drag = None;
    }

    // ---- 几何：屏幕点 ↔ 某一章的章内字符偏移 ----

    /// 屏幕点 → `(哪一章, 章内字符偏移)`。没有书 / 版心还没量过 / 那一点落进的章
    /// 还没排出版来，都是 `None`。
    ///
    /// 定版式那一族在这一处岔开：它的「第几章」就是**第几页**（`LocatorKind::Page`
    /// 那条口径），而换算不读排版产物 —— 那一族没有排版。见 [`Self::fixed_offset_at`]。
    pub(in crate::ui::features::reader) fn offset_at(
        &self,
        window: &Window,
        cx: &App,
        at: Point<Pixels>,
    ) -> Option<(usize, usize)> {
        if let Some(book) = self.fixed() {
            return self.fixed_offset_at(book, at);
        }
        let (chapter, x, y) = self.to_layout(window, at)?;
        let doc = self.book()?.span_doc(chapter)?;
        doc.hit_test(window, cx, x, y)
            .map(|hit| (chapter, hit.offset))
    }

    /// 屏幕点 → `(哪一页, 那一页字序里的插入点)`。
    ///
    /// 尺只有两把：那一格的页这一帧摆在版心里哪一块（[`ReaderSession::fixed_text_boxes`]，
    /// 由定版式那一臂每帧写入），以及那一页自己的可见框多大（`PageText` 带着）。
    /// 两把尺之间只差一个等比缩放，所以这里不做「猜比例」那一类兜底：量不出比例
    /// 就是这一页选不出字，而不是把色块画到别处。
    fn fixed_offset_at(&self, book: &FixedBook, at: Point<Pixels>) -> Option<(usize, usize)> {
        let boxes = self.fixed_text_boxes.borrow();
        let local = at - self.content_bounds.get().origin;
        let (page, box_) = *boxes.iter().find(|(_, box_)| box_.contains(&local))?;
        let text = book.page_text(page)?;
        let (x, y) = page_units(&text, box_, local)?;
        text.hit(x, y).map(|index| (page, index))
    }

    /// 屏幕点吸一个词（长按用）。命中不到字（纯图片行、空白）给 `None`。
    ///
    /// 定版式走 [`PageText::snap`]：那一份收边判据不重写，与排版那一族共用
    /// `content::hit::CharClass`（拉丁连成词、中文与空白单字自为界），
    /// 而它同样不跨行 —— 两条路的观感因此是一个手势一套规矩。
    fn word_at(
        &self,
        window: &Window,
        cx: &App,
        at: Point<Pixels>,
    ) -> Option<(usize, Range<usize>)> {
        if let Some(book) = self.fixed() {
            let (page, offset) = self.fixed_offset_at(book, at)?;
            let word = book.page_text(page)?.snap(offset);
            return (word.start < word.end).then_some((page, word));
        }
        let (chapter, offset) = self.offset_at(window, cx, at)?;
        let word = self.book()?.span_doc(chapter)?.snap_word(offset);
        (word.start < word.end).then_some((chapter, word))
    }

    /// 窗口坐标 → `(哪一章, 排版坐标)`。绘制（`view.rs` 摆行）与命中
    /// （`folio_core::content::hit`）的换算互为逆运算，两头的偏移必须同源：连续滚动是
    /// 「全局 y − 章基线」，分页/双页是片的 `origin_y` 与栏位 `x`，两条带子的高度
    /// 都扣在同一个地方。
    pub(in crate::ui::features::reader) fn to_layout(
        &self,
        window: &Window,
        at: Point<Pixels>,
    ) -> Option<(usize, f32, f32)> {
        let origin = self.content_bounds.get().origin;
        let local = at - origin;
        let prefs = &self.prefs;
        let content_top = self.page_box.content.y;
        if self.laid_out.mode.is_continuous() {
            let book = self.book()?;
            let gap = Book::chapter_gap(prefs);
            let local_y = f32::from(local.y);
            // 单图页（整章只有一张满宽图）在滚动档里按**窗口坐标**画：底图与正文都从
            // 窗口顶起，不走 `content_top` 那一段。先问它自己这一章在屏上的带，
            // 否则视口顶那一截会按「上一章的章尾」去命中 —— 手指落在图上，选中的
            // 却是上一节看不到的几行。
            let anchor = book.chapter_index();
            if let Some(doc) = book.span_doc(anchor)
                && doc.is_current()
                && crate::ui::features::reader::view::body::art::chapter_is_picture_only_page(doc)
                && let Some(base) = book.base_of(anchor, gap)
            {
                let in_chapter = local_y + self.scroll_painted - base;
                if (0.0..doc.height().max(1.0)).contains(&in_chapter) {
                    return Some((anchor, f32::from(local.x) - prefs.margin_x, in_chapter));
                }
            }
            // 其余章按版心坐标画：读**画出来的**那一个 y（`scroll_painted`）而不是
            // `scroll_y` —— 平移补间在走的那两百多毫秒里，屏上的字按画出来的位置摆，
            // 命中也按它算才对得上一个字段。
            let global_y = local_y - content_top + self.scroll_painted;
            let chapter = book.chapter_at_y(global_y, gap)?;
            let base = book.base_of(chapter, gap).unwrap_or(0.0);
            return Some((
                chapter,
                f32::from(local.x) - prefs.margin_x,
                global_y - base,
            ));
        }
        // 分页 / 双页：先按横坐标定栏位，再按那一栏的片定纵向原点。
        let gap = chrome::spread_gap(prefs.margin_x);
        let usable = Document::content_width(window, prefs, self.reserve.width);
        let width =
            folio_core::layout::fragment::column_width(usable, self.laid_out.columns, gap).max(1.0);
        // 平移翻页在走的时候整页偏了一段（`page_shift`，由渲染帧按当前页写入）。减掉它，
        // 两条轴才与 `view.rs` 烘进 `Scene::build` 的那两个加数互为逆运算。
        let (shift_x, shift_y) = self.page_shift;
        let column = if (f32::from(local.x) - prefs.margin_x - shift_x) >= width + gap / 2.0 {
            1
        } else {
            0
        };
        let book = self.book()?;
        let doc = book.document();
        let slice = doc.slice_of(self.page, column)?;
        let x = prefs.margin_x
            + folio_core::layout::fragment::column_offset(width, gap, slice.column)
            + shift_x;
        // 竖排那一档这一句是 `Scene::build` 里那两个平移数的**逆**：
        // 屏上的横偏移对应章内物理 x，而那一根轴从长条右缘反着数（`slide`），
        // 片的推进轴原点因此加在横坐标上而不是纵坐标上；纵坐标直接就是列内位置。
        if doc.layout().vertical() {
            let slide = slice.origin_y + slice.height - doc.height();
            return Some((
                book.chapter_index(),
                f32::from(local.x) - x - slide,
                f32::from(local.y) - content_top - shift_y,
            ));
        }
        Some((
            book.chapter_index(),
            f32::from(local.x) - x,
            f32::from(local.y) - content_top - shift_y + slice.origin_y,
        ))
    }

    // ---- 把手命中 ----

    /// 这一帧里 `edge` 那端把手插入符的中心（窗口坐标）。[`register`] 每帧写入
    /// [`ReaderSession::handle_hits`]，这里只读。
    fn handle_center(&self, edge: SelectionEdge) -> Option<Point<Pixels>> {
        self.handle_hits
            .borrow()
            .iter()
            .find(|(candidate, _)| *candidate == edge)
            .map(|(_, hit)| hit.center())
    }

    /// 落点在哪个把手的命中区里（宁大勿小 —— 那是拇指的命中下限，尺寸见
    /// [`TouchHandle::hit_bounds`]）。手势注册在 [`register`]，触摸拖动的 Started 也从这里问。
    pub(in crate::ui::features::reader) fn handle_at(
        &self,
        at: Point<Pixels>,
    ) -> Option<SelectionEdge> {
        self.handle_hits
            .borrow()
            .iter()
            .find(|(_, hit)| hit.contains(&at))
            .map(|(edge, _)| *edge)
    }
}

// ====================== 定版式那一族的几何（页内字序 ↔ 屏上那一格）======================

/// 那一页字序自己的尺 → 版心里那一格上的位置（元素局部坐标）。
///
/// `PageText` 的坐标单位是**它自己那一页的可见框**（`width` / `height` 那一格说清了
/// 单位在哪里，见 `folio_core::content::format::PageText`），而屏上那一格是等比缩放后
/// 的像素。两把尺之间只差这一个比例，所以横竖**各按各的轴**除：拿一个比例同时乘两条轴
/// 等于假设页的宽高比与那一格的逐位相等，而取整与裁边都会让它差出一两个像素 ——
/// 差出来的那一格在屏上是「色块偏一截」，不是「尺寸不对」。
///
/// 页的尺寸说不出来（0）、或那一格还没量过时给 `None`。宁可这一页这一帧选不出字，
/// 也不按一个猜的比例把色块画到别的字上：前者读者只当这页没字（与扫描页同档），
/// 后者是一条会说谎的痕迹。
pub(in crate::ui::features::reader) fn page_units(
    text: &PageText,
    box_: Bounds<Pixels>,
    local: Point<Pixels>,
) -> Option<(f32, f32)> {
    let scale = page_scale(text, box_)?;
    Some((
        (f32::from(local.x) - f32::from(box_.origin.x)) * scale.0,
        (f32::from(local.y) - f32::from(box_.origin.y)) * scale.1,
    ))
}

/// 那一页字序 → 屏上那一格的选区矩形（每行一条，与排版那一族同一口径：
/// 一行一段连续的字并成一块，跨行断开）。夹在那一格之内 —— 色块不该长出页图去，
/// 落在留白上的一条黄带读起来像选中的是页外的东西。
pub(in crate::ui::features::reader) fn page_boxes(
    text: &PageText,
    range: Range<usize>,
    box_: Bounds<Pixels>,
) -> Vec<Bounds<Pixels>> {
    let Some((per_px_x, per_px_y)) = page_scale(text, box_) else {
        return Vec::new();
    };
    // [`page_scale`] 给的是「一像素等于多少页内单位」—— 命中要的正是那一把（屏幕点折回
    // 字序）。色块要的是它的倒数（一个单位占几像素），两处都从同一条式子来：这样
    // 「按在哪个字上」与「那块色画在哪」不可能各拿一把尺。
    let (sx, sy) = (1.0 / per_px_x, 1.0 / per_px_y);
    // 夹回那一格的四条边。手写而不叫 `Bounds` 自己求交：这一族要的只是「别长出页图」
    // （落在留白上的一条黄带读起来像选中的是页外的东西），而 min/max 这两下不依赖
    // 引擎版本给不给得出某个助手。
    let (left, top) = (f32::from(box_.origin.x), f32::from(box_.origin.y));
    let (right, bottom) = (
        left + f32::from(box_.size.width),
        top + f32::from(box_.size.height),
    );
    text.rects(range)
        .into_iter()
        .filter_map(|rect| {
            let x0 = (left + rect.x * sx).clamp(left, right);
            let x1 = (left + (rect.x + rect.width) * sx).clamp(left, right);
            let y0 = (top + rect.y * sy).clamp(top, bottom);
            let y1 = (top + (rect.y + rect.height) * sy).clamp(top, bottom);
            if x1 <= x0 || y1 <= y0 {
                return None;
            }
            Some(Bounds::new(
                point(px(x0), px(y0)),
                size(px(x1 - x0), px(y1 - y0)),
            ))
        })
        .collect()
}

/// 定版式这一帧的选中几何一次算齐：高亮那几块与两端把手。
///
/// 把手的落点交给首末那一块矩形，与可重排那一族同一条式子（`view/body.rs` 里那份）——
/// 两端把手长在色块的字缘上这一观感不该因为这一族没有排版就换一套。
pub(in crate::ui::features::reader) fn page_visual(
    text: &PageText,
    range: Range<usize>,
    box_: Bounds<Pixels>,
    color: Hsla,
    highlight: Hsla,
) -> SelectionVisual {
    let highlights = page_boxes(text, range, box_);
    let start_caret = highlights
        .first()
        .map(|first| Bounds::new(first.origin, size(px(0.0), first.size.height)));
    let end_caret = highlights.last().map(|last| {
        Bounds::new(
            point(last.right(), last.top()),
            size(px(0.0), last.size.height),
        )
    });
    SelectionVisual {
        highlights,
        start_caret,
        end_caret,
        color,
        highlight_color: Some(highlight),
    }
}

/// 那一层「盖在页图之上」的选中件：色块、把手，以及那几条手势监听。
///
/// 为什么定版式要另起一层，而不是复用字形元素里那一份：这一族的字就在那张栅格出来的
/// 图里，而 gpui 的 z-order 是后入树者居上 —— 页图是 `overlays`，比字形元素晚入树，
/// 所以压在字形之下的高亮与把手会被整页图盖得看不见。可重排那一族的高亮必须压在
/// 字形**之下**才不吃掉笔画，两族要的层级正好相反。这里不是把同一件事画两遍，
/// 是同一份 [`SelectionVisual`] 换一层画。
///
/// 监听也挂在这一层（`window.on_mouse_event` 只活到本帧事件分发结束，与 [`register`]
/// 同一条规矩）。递进去的 `bounds` 是整块版心（这一层 `inset_0`），所以会话里
/// `content_bounds` 写进来的与字形那一层是同一个数 —— 点按分区不必问自己盖的是哪一族。
pub(in crate::ui::features::reader) fn overlay(
    visual: SelectionVisual,
    reader: &WeakEntity<ReaderSession>,
) -> AnyElement {
    let reader = reader.clone();
    canvas(
        |_, _, _| {},
        move |bounds, _, window, cx| {
            paint_highlights(&visual, bounds.origin, window);
            register(&visual, &reader, bounds, window, cx);
            paint_handles(&visual, bounds.origin, window);
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// 那一格上**一像素等于多少页内单位**（横、竖各一条）。
///
/// 只有一条式子，两个方向用它的正反面：命中把屏幕点乘过去（[`page_units`]），色块把
/// 页内盒除回来（[`page_boxes`]）。量不出来时 `None`，理由见 [`page_units`] —— 那一格
/// 是这一族唯一「宁可什么都不画」的口子。
fn page_scale(text: &PageText, box_: Bounds<Pixels>) -> Option<(f32, f32)> {
    let width = f32::from(box_.size.width);
    let height = f32::from(box_.size.height);
    if text.width <= 0.0 || text.height <= 0.0 || width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some((text.width / width, text.height / height))
}

// ====================== 就地菜单条（图标浮条 + 两条胶囊分列两端）======================
/// 动作条里一格的可点边长（逻辑像素）。拇指能稳定命中的下限是 44，而图标本身只有
/// 20：这里取 40 是参考图里那排图标的呼吸感，一颗一颗之间不挨着。
const ACTION_CELL: f32 = 40.0;
/// 图标本身多大。参考图里那一排图标约 22，落在 40 的格子里正好。
const ACTION_ICON: f32 = 21.0;
/// 动作条的内缩：上下左右各 4，整条于是 48 高。
const BAR_PAD: f32 = 4.0;
/// 顶端那一行两条胶囊的内缩与高：颜色那五枚 20 的圆点，上下各留 6。
const SWATCH_PAD: f32 = 6.0;
const SWATCH_DOT: f32 = 20.0;
/// 一枚颜色格子的可点宽。比圆点宽 6。
const SWATCH_CELL: f32 = 28.0;
/// 一枚画法格子的可点宽。**与颜色格子同宽**：那两条胶囊分列两端、同一高度，
/// 一格宽不一样就会读出「这是两排不同的东西」，而它们答的是同一件事（这一条怎么留）。
const STYLE_CELL: f32 = SWATCH_CELL;
/// 画法那三格的图标多大。胶囊只有 32 高，21 那个数（动作条 40 格子里的尺寸）会顶到边。
const STYLE_ICON: f32 = 17.0;
/// 颜色胶囊的高（与摆位共用，那一行的两条胶囊同一个高）。
const SWATCH_BAR: f32 = SWATCH_DOT + SWATCH_PAD * 2.0;
/// 动作条与顶端那一行之间的缝。
const BAR_GAP: f32 = 6.0;
/// 顶端那一行里两条胶囊之间的缝。比 [`BAR_GAP`] 宽一点：同一行里两块是并列的两件事，
/// 而上下两块属于同一张卡片。
const MARK_GAP: f32 = 12.0;
/// 指向选区那条小尾巴的高与宽。
const MENU_TAIL: f32 = 8.0;
const MENU_TAIL_WIDTH: f32 = 16.0;
/// 菜单与选区之间的缝，也是它与视口上下边的最小间距。
const MENU_GAP: f32 = 6.0;
/// 叠在圆点上的那枚勾的字号（`Icon` 取当前文字号）。
const SWATCH_CHECK: f32 = 13.0;

/// 动作条的格子数：复制、划线（或取消）、笔记、翻译、清除，加上三格**按端与按配置缺席**
/// 的：「分享卡片」只在能离屏渲染的那一端摆（判据在
/// [`crate::ui::platform::RENDERS_SHARE_CARDS`]，桌面上那一按渲不出字节，摆出来就是
/// 一个坏按钮）；「问 AI」只在端点填全的那一台机器上摆（判据在 `ReaderSession::ai_available`，
/// 与真发请求共用同一份）；「词典」只在导入过本地词典的那一台摆（判据在
/// [`crate::ui::services::dictionary::available`]）。三处不一致的后果不是难看，是
/// **摆一颗按下去只会报错的按钮**或者反过来 —— 明明查得出去却没有入口。
fn action_cells(share: bool, ai: bool, dict: bool) -> f32 {
    5.0 + share as u8 as f32 + ai as u8 as f32 + dict as u8 as f32
}

/// 整块菜单占的高（含尾巴）：摆位与「放不下就翻面」都按这个数算。
/// 顶端那一行 + 缝 + 动作条 + 尾巴；那一行**恒在**，所以没有「一行 / 两行」两种高。
/// 画法那三格与颜色那五枚同住那一行，所以这一格高**没有因为加画法而变**。
pub(in crate::ui::features::reader) const fn card_height() -> f32 {
    ACTION_CELL + BAR_PAD * 2.0 + BAR_GAP + SWATCH_BAR + MENU_TAIL
}

/// 左边那条画法胶囊的宽。
fn style_pill_width() -> f32 {
    SWATCH_PAD * 2.0 + STYLE_CELL * AnnotationStyle::ALL.len() as f32
}

/// 右边那条颜色胶囊的宽。
fn swatch_pill_width() -> f32 {
    SWATCH_PAD * 2.0 + SWATCH_CELL * ANNOTATION_COLORS.len() as f32
}

/// 顶端那一行的宽：两条胶囊 + 中间那道缝。不摆「问 AI」那一格时它是整块菜单最宽的一行，
/// 所以它常常决定横向占位 —— 但下面那句 `max` 不许把这层关系当成已知，见 [`card_width`]。
fn mark_row_width() -> f32 {
    style_pill_width() + MARK_GAP + swatch_pill_width()
}

/// 整块菜单的宽 = 顶端那一行与动作条里的**较宽者**。
///
/// 为什么不写死其中一个：动作条的宽跟着那三格可选的摆不摆变（208 / 248 / 288 / 328），
/// 而那一行的宽跟着「几档画法、几档颜色」变（今天 260）。不摆可选格时那一行更宽，
/// 摆上两三格之后动作条反过来更宽 —— 写死任何一边都会让另一边探出去或摆歪。见
/// [`the_card_is_as_wide_as_whichever_row_is_wider`](tests)。
///
/// `share` / `ai` / `dict` 是这一端、这一台机器上摆不摆那三格（与渲染、与真发请求、
/// 与有没有词典读同一份判据），横向居中与夹边都按它们算 —— 两处不一致同样会摆歪。
pub(in crate::ui::features::reader) fn card_width(share: bool, ai: bool, dict: bool) -> f32 {
    (ACTION_CELL * action_cells(share, ai, dict) + BAR_PAD * 2.0).max(mark_row_width())
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
        let share = tr(cx, "reader.sel_share");
        // 挑中的正是留在书上的那一条时，第二格改做「取消划线」；上面那一行画法与颜色恒在，
        // 只是给它那一档多打个标记。
        let marked = self.selection_annotation();
        let has_note = marked
            .as_ref()
            .is_some_and(|row| self.annotation_has_note(row.id));
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
        // 顶端那一行恒在。挑中的正是库里某条划线时，它那一档打勾 / 打亮，点下去即改那一条；
        // 普通选区没有「当下那一条」，画法的当下那一档取自配置（点下去即用它划一条，
        // 并把它记成配置里那一格 —— 见 [`ReaderSession::pick_style`]），颜色则一律「点色即划线」。
        let current = marked.as_ref().map(|row| row.color.as_str());
        let style = marked
            .as_ref()
            .map(|row| row.style)
            .unwrap_or_else(|| self.pending_style(cx));
        // 「问 AI」那一格摆不摆，与卡片按几格算宽，读的是同一个值：取两次就可能一次说
        // 有、一次按没有算，卡片于是正好窄掉那一格。
        // 会员门禁与端点配置同一条资格线：不是会员时翻译与 AI 两格整段缺席 ——
        // 「能力不存在时这一格不出现」，与词典那一格同一判据，不摆按下去才报错的按钮。
        let premium = crate::ui::services::activation::premium(cx);
        let ai = premium && ReaderSession::ai_available(cx);
        // 词典那一格同理：本地一本都没导入时整个缺席，判据与查词那一趟读同一份配置。
        let dict = crate::ui::services::dictionary::available(cx);
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
            // 翻译：把选区交给外部引擎，弹层锚在菜单落地的那一处（菜单先收）。
            // 与笔记那格同一类「就地做一件事」的入口，所以排在它旁边。
            // 会员门禁：非会员整格缺席（与 AI 那格同一条资格线）。
            .children(premium.then(|| {
                action_cell(
                    "selection-translate",
                    IconName::Languages,
                    tr(cx, "reader.translate"),
                    foreground,
                    hover,
                    reader.clone(),
                    |r, _, cx| r.open_translation(cx),
                )
            }))
            // 词典：查的是本地 StarDict，和翻译一样是「就地做一件事」的入口。一本都没
            // 导入时整格缺席 —— 摆一颗按下去只会说「没有词典」的按钮没有意义。
            .children(dict.then(|| {
                action_cell(
                    "selection-dictionary",
                    IconName::BookOpen,
                    tr(cx, "reader.dictionary"),
                    foreground,
                    hover,
                    reader.clone(),
                    |r, _, cx| r.open_dictionary(cx),
                )
            }))
            // 问 AI：与翻译同一类「就地做一件事」的入口，排在它旁边。端点没填全时
            // 这一格整个缺席（判据与真发请求用的是同一份，见 `model/ai.rs`），
            // 所以它也是 `children(Option)` 而不是一个按下去报错的按钮。
            .children(ai.then(|| {
                action_cell(
                    "selection-ai",
                    IconName::Sparkles,
                    tr(cx, "reader.ask_ai"),
                    foreground,
                    hover,
                    reader.clone(),
                    |r, _, cx| r.open_ai_question(cx),
                )
            }))
            // 渲不出图的那一端这一格直接缺席（见 [`crate::ui::platform::RENDERS_SHARE_CARDS`]），
            // 所以它是 `children(Option)` 而不是一个禁用的按钮。
            .children(platform::RENDERS_SHARE_CARDS.then(|| {
                action_cell(
                    "selection-share",
                    IconName::Share2,
                    share,
                    foreground,
                    hover,
                    reader.clone(),
                    |r, window, cx| r.share_selection(window, cx),
                )
            }))
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
        let styles = style_row(
            reader.clone(),
            style,
            surface,
            hover,
            foreground,
            &style_cells(cx),
        );
        // 横向对着选区中线；中线不在手上（首帧还没画过）就居中在正文那一条上。
        // 两种摆法都夹在页内，不把卡片顶出屏幕。
        let page_width = f32::from(self.content_bounds.get().size.width);
        let width = card_width(platform::RENDERS_SHARE_CARDS, ai, dict);
        let left = menu_left(page_width, self.prefs.margin_x, width, anchor_x);
        // 顶端那一行摆在上头，动作条贴着尾巴。理由是那一行的点击**直接写库或写配置**：
        // 挑中某条划线时点画法即改那一条、点色即换色，普通选区点哪格就用那一档新划一条
        // （见 [`style_row`] 与 [`swatch_row`] 里的 `on_click`），中间没有预览也没有撤销
        // 台阶 —— 于是把它推到离选区、离拇指远的那一头。
        //
        // 两条胶囊顶到整块菜单的左右两端（`justify_between` + 显式给宽）：宽给足之后两边
        // 各顶各的边，「两端对齐」这件事就写在布局里而不是靠常数凑。哪一边定下这个宽见
        // [`card_width`] —— 摆上「问 AI」那一格之后是动作条更宽，这一行被拉宽、两条胶囊
        // 之间的缝跟着变大，仍然各顶各的边。
        let marks = h_flex()
            .w(px(width))
            .items_center()
            .justify_between()
            .child(styles)
            .child(swatches);
        let bars = v_flex()
            .items_center()
            .child(marks)
            .child(
                // 一条空 div 当缝：`card_height` 算的就是这 6 像素，不能靠 `gap` 的两套账。
                div().h(px(BAR_GAP)).flex_none(),
            )
            .child(actions);
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
            // `ColorSwatch` 只带语义（radio 那一套：role、选中态、焦点轨、点击与 hover 的
            // 派发），色由这一层自己涂 —— 28×32 那一整格是它的命中面，圆点是它的孩子。
            ColorSwatch::new(("selection-swatch", ix), palette.color(name))
                .selected(active)
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
                // 回调给的是 `(色, ClickEvent, Window, App)`：色与 window 这一层都不必用
                // —— 库里认的是那个颜色**名字**，写回的也是它。
                .on_click(move |_, _, _, cx| {
                    reader.update(cx, |reader, cx| match reader.selection_annotation() {
                        Some(_) => reader.set_highlight_color(name, cx),
                        None => reader.add_highlight_with(name, cx),
                    });
                })
                .into_any_element()
        }))
        .into_any_element()
}

/// 三档画法在菜单里的样子：`(哪一档, 图标, 说明)`，顺序就是 `AnnotationStyle::ALL` 的顺序
/// —— 左边那条胶囊从左到右摆它，库里那一列存的也是同一套名字，所以三处不会各排各的。
///
/// 图标各挑各的形状而不是一枚图标配三个数：这三格与右边那五枚同住一行，读的是「哪种痕迹」，
/// 形状本身要能一眼分开。`label` 不画出来，只进无障碍（与动作条那几格同一条规矩）。
fn style_cells(cx: &App) -> Vec<(AnnotationStyle, IconName, SharedString)> {
    AnnotationStyle::ALL
        .iter()
        .copied()
        .map(|style| {
            let (icon, key) = match style {
                AnnotationStyle::Highlight => (IconName::Highlighter, "reader.style_highlight"),
                AnnotationStyle::Line => (IconName::Underline, "reader.style_line"),
                AnnotationStyle::Wavy => (IconName::WavesHorizontal, "reader.style_wavy"),
            };
            (style, icon, tr(cx, key))
        })
        .collect()
}

/// 左边那条胶囊：三格「划线方式」。
///
/// 当下是哪一格：挑中了库里某一条时是**那一条自己**那一格，普通选区时是配置里那一格
/// （调用方把两件事折成一个数递进来，见 [`ReaderSession::pending_style`]）。打亮的方式与
/// 圆点不同 —— 圆点是色块上叠一枚勾，这里图标压不住第二层，所以给那一格一块背景，
/// 与动作条 hover 同一份色，读起来是「这一格正按着」。
///
/// 点下去做什么由 [`ReaderSession::pick_style`] 分岔：已有一条就改那一条（不碰配置），
/// 普通选区就用这个画法划一条 **并**把它记成配置里那一格（跨书持久，与设置里那一格同家）。
fn style_row(
    reader: Entity<ReaderSession>,
    current: AnnotationStyle,
    surface: Hsla,
    hover: Hsla,
    foreground: Hsla,
    cells: &[(AnnotationStyle, IconName, SharedString)],
) -> AnyElement {
    h_flex()
        .px(px(SWATCH_PAD))
        .h(px(SWATCH_BAR))
        .gap_0()
        .rounded(px(SWATCH_BAR / 2.0))
        .bg(surface)
        .shadow(floating_shadow())
        .children(cells.iter().enumerate().map(|(ix, (style, icon, label))| {
            let active = *style == current;
            let reader = reader.clone();
            let style = *style;
            div()
                .id(("selection-style", ix))
                .w(px(STYLE_CELL))
                // 整格可点：图标只有 17，按图标尺寸给命中面在手机上就按不中。
                .h(px(SWATCH_BAR))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(SWATCH_BAR / 2.0))
                .cursor_pointer()
                .when(active, |cell| cell.bg(hover))
                .hover(move |cell| cell.bg(hover))
                .child(
                    div()
                        .text_size(px(STYLE_ICON))
                        .text_color(foreground)
                        .child(Icon::new(*icon)),
                )
                .aria_label(label.clone())
                .on_click(move |_, _, cx| {
                    reader.update(cx, |reader, cx| reader.pick_style(style, cx));
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
    //! 只钉三类值得钉的判断：**摆位会不会翻面、卡片多高多宽**，**哪一边决定宽**，
    //! 以及**把手的可点宽还够不够拇指**。
    //! 格子尺寸、图标、尾巴形状都是画出来的东西，测了只是把常数抄进断言里两遍；唯独把手那份
    //! 常数不住在这份文件里（在 gpui-kit 的 `TouchHandle` 上），它变小了这边一行代码都不会坏，
    //! 只有读者的拇指会察觉。
    //!
    //! 点名导入而不 `use super::*`：`gpui_kit` 根把 `gpui` 整个转了出来，里面有一个
    //! 叫 `test` 的属性宏，glob 会让下面的 `#[test]` 递归展开成它。

    use folio_core::content::format::{PageChar, PageRect, PageText};
    use gpui_kit::{Bounds, Hsla, point, px, size};

    use super::{
        ACTION_CELL, BAR_GAP, BAR_PAD, MARK_GAP, MENU_TAIL, STYLE_CELL, SWATCH_BAR, SWATCH_CELL,
        SelectionEdge, Slot, TouchHandle, action_cells, card_height, card_width, mark_row_width,
        menu_left, page_boxes, page_units, page_visual, slot, style_pill_width, swatch_pill_width,
    };

    /// 卡片总高（动作条 + 顶端那一行）。摆位只认递进去的这个数。
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

    /// 整块菜单的宽是「顶端那一行」与「动作条」里的**较宽者**，两者都不许探出去。
    ///
    /// 三格可选的（分享、问 AI、词典）各加**正好一格**：渲染那一侧与摆位那一侧读同一个
    /// 判据，各算一次就会摆歪 —— 卡片按五格算宽而动作条摆了八格，多出来的那一格是被
    /// 裁掉的一段，读者看到的是「翻译」右边什么都没有。
    ///
    /// 今天哪几档里决定宽的是**哪一边**也钉住：五到六格时顶端那一行（260）更宽，七到八格
    /// 之后动作条反过来成了较宽的一边。这条会红的日子就是胶囊那两条不再顶到卡片两边的日子
    /// （`justify_between` 只会把它们之间的缝拉开），到时候看一眼那两条胶囊该不该跟着加宽。
    #[test]
    fn the_card_is_as_wide_as_whichever_row_is_wider() {
        for share in [false, true] {
            for ai in [false, true] {
                for dict in [false, true] {
                    let actions = ACTION_CELL * action_cells(share, ai, dict) + BAR_PAD * 2.0;
                    assert!(
                        card_width(share, ai, dict) >= actions
                            && card_width(share, ai, dict) >= mark_row_width(),
                        "窄的一边被当成了整块的宽：{} 格动作条 = {actions}，那一行 = {}",
                        action_cells(share, ai, dict),
                        mark_row_width()
                    );
                    // 取的是较宽者而不是两者相加 —— 多算一项就会白白把菜单推到边外。
                    assert_eq!(card_width(share, ai, dict), actions.max(mark_row_width()));
                }
            }
        }
        for share in [false, true] {
            for dict in [false, true] {
                assert_eq!(
                    ACTION_CELL * action_cells(share, true, dict)
                        - ACTION_CELL * action_cells(share, false, dict),
                    ACTION_CELL,
                    "「问 AI」那一格该正好多占一格宽"
                );
            }
        }
        for ai in [false, true] {
            for dict in [false, true] {
                assert_eq!(
                    ACTION_CELL * action_cells(true, ai, dict)
                        - ACTION_CELL * action_cells(false, ai, dict),
                    ACTION_CELL,
                    "「分享卡片」那一格该正好多占一格宽"
                );
            }
        }
        for share in [false, true] {
            for ai in [false, true] {
                assert_eq!(
                    ACTION_CELL * action_cells(share, ai, true)
                        - ACTION_CELL * action_cells(share, ai, false),
                    ACTION_CELL,
                    "「词典」那一格该正好多占一格宽"
                );
            }
        }
        assert_eq!(card_width(true, false, false), mark_row_width());
        assert!(
            card_width(true, false, true) > mark_row_width(),
            "七格时动作条更宽"
        );
        assert!(
            card_width(false, false, false)
                > ACTION_CELL * action_cells(false, false, false) + BAR_PAD * 2.0
        );
    }

    /// 加画法那一档**不该把卡片撑高**：三格画法与五枚颜色同住顶端那一行（同一份 32 高），
    /// 所以整块卡片的高与加这一档之前一模一样，`slot` 那条「放不下就翻面」的临界点没动。
    /// 哪天把那一行改成两行（或把画法格子做成 40 高），这条会红 —— 那是摆位的账，
    /// 不只是好看与否：卡片高了，头顶放不下的一批选区就都翻到下面去了。
    #[test]
    fn the_style_pill_does_not_make_the_card_taller() {
        assert_eq!(
            STYLE_CELL, SWATCH_CELL,
            "两格不同宽，两条胶囊读起来是两排东西"
        );
        assert!(
            style_pill_width() + MARK_GAP + swatch_pill_width() == mark_row_width(),
            "那一行的宽不是两条胶囊加一道缝"
        );
        assert_eq!(
            card_height(),
            ACTION_CELL + BAR_PAD * 2.0 + BAR_GAP + SWATCH_BAR + MENU_TAIL
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

    /// 把手的可点宽要一直在拇指下限（44）之上。**这份数不住在这份文件里** —— 它是
    /// gpui-kit 的 `TouchHandle::HIT_SIZE`，上游把它调小时这里一行代码都不会坏，
    /// 只有真机上「把手按不中」，所以拿一条断言钉住。
    #[test]
    fn the_handle_touch_target_is_wide_enough_for_a_thumb() {
        // 插入符盒：0 宽、一行高，落在 x = 30。两端都必须凑出同样的可点宽。
        let caret = Bounds::new(point(px(30.), px(10.)), size(px(0.), px(28.)));
        for edge in [SelectionEdge::Start, SelectionEdge::End] {
            let width = f32::from(TouchHandle::hit_bounds(edge, caret).size.width);
            assert!(width >= 44.0, "把手的可点宽低于拇指下限：{width}");
        }
    }

    // ---- 定版式那一族：页内字序 ↔ 屏上那一格 ----

    /// 一页两行：第一行 `ab`，第二行 `中文`。页自己的框是 **100 × 60**（`PageText`
    /// 的坐标单位就是它），每格 10 宽、行高 12。
    fn page() -> PageText {
        let rows = [('a', 0u32), ('b', 0), ('中', 1), ('文', 1)];
        let mut chars = Vec::new();
        for (index, (ch, line)) in rows.iter().enumerate() {
            // 每行两格，一格 10 单位宽；行与行之间空 20。
            chars.push(PageChar {
                ch: *ch,
                rect: PageRect {
                    x: (index % 2) as f32 * 10.0,
                    y: 10.0 + *line as f32 * 20.0,
                    width: 10.0,
                    height: 12.0,
                },
                line: *line,
            });
        }
        PageText {
            width: 100.0,
            height: 60.0,
            chars,
        }
    }

    /// **两条轴各按各的比例**。这一格故意把那一格摆成 50 × 120（页框 100 × 60 的
    /// 反比例），于是「拿一个比例同时乘两条轴」的写法会把纵坐标算成横的六倍：
    /// 横 25 px ↔ 50 单位，纵 60 px ↔ 120 单位。
    ///
    /// 症状是色块与手指各指一处，而页面上看不出是哪条轴错了 —— 所以这一条按两条轴
    /// 各自断言，而不是只断「落在那一个字上」。
    #[test]
    fn the_page_maps_each_axis_by_its_own_box() {
        let box_ = Bounds::new(point(px(8.), px(20.)), size(px(50.), px(120.)));
        // 页内 (10, 20) 那一格的正中 → 屏上 (8 + 5, 20 + 40)。
        let back = page();
        let at = point(px(13.0), px(60.0));
        let (x, y) = page_units(&back, box_, at).expect("量得出比例");
        assert!((x - 10.0).abs() < 0.01, "横该按 100/50 折：{x}");
        assert!((y - 20.0).abs() < 0.01, "纵该按 60/120 折：{y}");
        // 这一格左上角就是页内 (0, 0)。
        let (x0, y0) = page_units(&back, box_, box_.origin).expect("同上");
        assert_eq!((x0, y0), (0.0, 0.0));
    }

    /// 页框说不出来（0）或那一格还没量过时交 `None` —— 而不是猜一个比例。
    ///
    /// 猜的症状是色块画在别的字上（读者看得见、说不清），而 `None` 的表现与扫描页
    /// 逐字节相同：这一页选不出字。宁可后者。
    #[test]
    fn an_unmeasurable_page_or_tile_selects_nothing() {
        let box_ = Bounds::new(point(px(0.), px(0.)), size(px(100.), px(100.)));
        let no_size = PageText {
            width: 0.0,
            height: 0.0,
            chars: page().chars,
        };
        assert!(page_units(&no_size, box_, point(px(1.), px(1.))).is_none());
        let no_tile = Bounds::new(point(px(0.), px(0.)), size(px(0.), px(0.)));
        assert!(page_units(&page(), no_tile, point(px(1.), px(1.))).is_none());
        assert!(page_boxes(&page(), 0..4, no_tile).is_empty());
    }

    /// 色块**夹在那一格之内**。一页真 PDF 里偶尔有一个字的盒越过页面自己声明的可见框
    /// （内容流把字排出页边，而 `PageText` 的宽与高说的就是那一带），选中那样的字之后
    /// 黄带不许画到留白上 —— 落在页外的一条带读起来像「选中了页外的东西」。
    ///
    /// 同时钉住「按行并成一条」：末行那两个字的盒中间空了一大截，并成一条才能画得下
    /// 一次跨它们的选中（`PageText::rects` 那一头是它自己数行号，这里只把它折上屏）。
    #[test]
    fn the_boxes_stay_inside_the_tile() {
        // 页框 100 × 60 单位摆进 100 × 60 像素：一单位 = 一像素，好核对。
        let box_ = Bounds::new(point(px(0.), px(0.)), size(px(100.), px(60.)));
        let mut page = page();
        let last = page.chars.last_mut().expect("这一页有字");
        last.rect.x = 90.0;
        last.rect.width = 50.0; // 越过页框右沿 40 个单位
        let boxes = page_boxes(&page, 0..4, box_);
        assert_eq!(boxes.len(), 2, "两行各一条");
        assert!(
            boxes
                .iter()
                .all(|rect| f32::from(rect.right()) <= 100.0 + 0.001
                    && f32::from(rect.origin.x) >= -0.001),
            "色块探出那一格之外：{boxes:?}"
        );
        let second = &boxes[1];
        assert!(
            (f32::from(second.right()) - 100.0).abs() < 0.01,
            "越界的那一条该收到页框右沿为止：{second:?}"
        );
        assert!(
            f32::from(second.size.width) > 0.0,
            "越界的一截也不能把整条丢掉 —— 读者选中的那几个字在屏上看得见"
        );
    }

    /// 两端把手落在首末那两条的字缘上，而高亮另压一档透明度（那一族的字在页图里，
    /// 实色会把要选的那几个字糊掉）。
    #[test]
    fn the_carets_sit_on_the_two_ends_and_the_band_is_translucent() {
        let handle = Hsla {
            h: 0.1,
            s: 0.5,
            l: 0.5,
            a: 1.0,
        };
        let band = Hsla {
            h: 0.1,
            s: 0.5,
            l: 0.5,
            a: 0.35,
        };
        let box_ = Bounds::new(point(px(0.), px(0.)), size(px(100.), px(60.)));
        let visual = page_visual(&page(), 1..4, box_, handle, band);
        assert_eq!(visual.highlights.len(), 2, "两行两条");
        let start = visual.start_caret.expect("起手端该有把手");
        assert_eq!(
            (f32::from(start.origin.x), f32::from(start.size.width)),
            (10.0, 0.0),
            "起手那只把手落在 `b` 的字缘上"
        );
        let end = visual.end_caret.expect("收尾端该有把手");
        assert_eq!(
            f32::from(end.origin.x),
            20.0,
            "末了那只该落在最后那条的右沿"
        );
        assert_eq!(visual.color, handle, "把手用实色");
        assert_eq!(
            visual.highlight_color,
            Some(band),
            "高亮用压过透明度的那一色"
        );
    }
}
