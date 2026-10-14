//! 目录那一屏：一块底部弹层里装着整棵目录树 —— 书头、「已读 N / M」那一行、搜索、列表。
//!
//! 从 `page.rs` 搬出来独立成一个实体，是因为这一屏要跨帧持有的状态一大把（整棵树与它的
//! 折叠集、搜索词与那颗输入框、弹层开合与停在哪一档、虚拟列表的尺寸表与滚动句柄、书头查回来的
//! 那一行书目），而它们与阅读页自己的那几样（浮层开合、三条滑条、图片查看器）改的从来不是
//! 同一件事。搬完之后 `page.rs` 只剩一个句柄：`toc`。
//!
//! 分工照 `reader` 这一层一贯的样子：**判断**在 [`super::toc_tree`]（摊平、折叠、当前行、
//! 搜索，纯函数，宿主的 `cargo test` 直接跑），**会话那一侧**的跳转命令在
//! `modules/toc.rs`，这一文件只管「这一屏摆成什么、点了哪一行要做什么」。
//!
//! 它向外只报两件事（[`TocEvent`]）：跳到了某一章（顶栏底栏与排版面板都在阅读页手里，
//! 收干净得由它来），以及「目录样式」那一下（设置那一屏要压进导航栈，栈也在阅读页手里）。

use std::rc::Rc;
use std::sync::Arc;

use folio_core::store::BookRow;
use gpui_kit::assets::IconName;
use gpui_kit::base::{VirtualListScrollHandle, v_virtual_list};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, Selectable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FontWeight,
    Image, InteractiveElement, IntoElement, ObjectFit, ParentElement, Pixels, Render,
    ScrollStrategy, SharedString, Size, StatefulInteractiveElement, Styled, StyledImage,
    Subscription, Window, div, hsla, img, px, size,
};

use super::chrome::percent_label;
use super::session::ReaderSession;
use super::toc_tree::TocView;
use crate::ui::features::shelf::{AUTHOR_SEP, fmt_size};
use crate::ui::shared::config_model::ConfigHolder;
use crate::ui::shared::images::cover_image;
use crate::ui::shared::l10n::{tf, tr};
use crate::ui::shell::navigation::SizeClass;
use crate::ui::widgets::{BottomSheet, BottomSheetState};

/// 目录这一屏的横向刻度，全部是在 Meizu 20（scale 2.8）上逐行数参照 App 的墨迹
/// 边界量出来的（dp = px / 2.8）。
///
/// 面板的统一左右内缩：书头那张封面与列表文字都从这条起（实测封面左沿 x=41px）。
const TOC_GUTTER: f32 = 15.0;
/// 当前行左边那条标记条的宽（实测 x=6..13px）。
const TOC_MARKER: f32 = 2.9;
/// 那条标记条距屏幕左边多少。
const TOC_MARKER_INSET: f32 = 2.1;
/// 当前行那条底色带的左边界 = 标记条的右沿（实测灰底从 x=14px 起，一直铺到屏幕右边）。
const TOC_BAND_LEFT: f32 = TOC_MARKER_INSET + TOC_MARKER;
/// 顶层标题文字的左边界（实测 L1 墨迹左沿 x=87..91px，两条独立行互相对上）。
const TOC_TEXT_LEFT: f32 = 31.0;
/// chevron 那一列的宽度。叶子也占同样一格，标题才不会因为有没有下级而左右跳。
const TOC_CHEVRON: f32 = 15.3;
/// 每一级往右让多少（实测 L1 文字 87px、L2 文字 120px → 差 32px）。
///
/// 这一档比「看得出挂在下级下面」的直觉窄得多，但参照 App 三级书也只让这么多：
/// 小屏上缩进每多一级就吃掉一行能摆几个字，层级靠 chevron 那一列已经读得出来。
const TOC_INDENT: f32 = 11.4;

/// 目录弹层的两档：全屏档留 8% 让读者认「这是从哪本书抬上来的」，半屏档是扫「当前章上下
/// 各几章」的那一眼 —— 目录真正的用途是扫，所以打开就落在全屏档，缩到半屏是主动选择。
const TOC_EXPANDED: f32 = 0.92;
const TOC_PEEK: f32 = 0.5;
/// 宽档（平板 / PC）上目录那条左侧栏的宽。
///
/// 与外壳导航侧栏的 `SIDEBAR_WIDTH`（260）差 20：那条栏放的是目的地列表，这一条放的是
/// 章节树，而正文在这一档还要留够一栏的宽度 —— 840 的窗口上 240 之后剩下的 600 才刚够
/// 双页对开退到分页那道线（`chrome::MIN_COLUMN_WIDTH` + 两条边距）以上。
const TOC_RAIL: f32 = 240.0;
/// 目录列表底部留出的空隙：滚到最后一条时，那一行不贴着弹层下沿（否则最后一行
/// 与手势条/圆角挤在一起，读起来像被切掉了）。
const TOC_BOTTOM_SPACE: f32 = 24.0;
/// 这一屏那四块，装配一次、两种容器各摆各的（见 [`TocPanel::pieces`]）。
struct Pieces {
    /// 钉住的那一行：「已读 N / M」+ 搜索 + ⋯（左侧栏那一档再多一颗关闭）。
    header: AnyElement,
    head: AnyElement,
    /// 搜索那一行：只在点开搜索之后出现。
    search_row: Option<AnyElement>,
    list: AnyElement,
}

/// 目录面板顶上那块书头要的那几样：一次打开查一回库。
///
/// 存整条 [`BookRow`] 而不是拆成书名 / 作者 / 格式三个字段：拆出来的那几份与库里那一行
/// 是同一件事的两处记录，读者在「信息」那一屏改过书名之后就会有一处是旧的。
/// 第三行那个「读到 X%」不在这里 —— 它是当场算出来的进度，存进来就成了第二真相源。
struct BookHeader {
    row: BookRow,
    /// 解码好的封面。`None` 画一块灰底，不留一格空白。
    cover: Option<Arc<Image>>,
}

/// 目录那一屏向外报的两件事。跳转与开设置的动作都长在**阅读页**那一层（浮层的开合状态、
/// 导航栈都在它手里），这一屏只说「读者要的是哪个」。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::ui::features::reader) enum TocEvent {
    /// 从目录里跳到了某一章：字已经在了，顶栏底栏与排版面板该整个收干净。
    Navigated,
    /// 「⋯」里那颗「目录样式」：设置那一屏要压进导航栈。
    OpenSettings,
}

/// 目录那一屏自己的状态。跨帧持有，一页一个（阅读页建它，随阅读页一起活）。
pub(in crate::ui::features::reader) struct TocPanel {
    /// 那一本书。树的形状、当前章、到过的章、进度都从它问，跳转命令也派给它。
    reader: Entity<ReaderSession>,
    /// 底部弹层：开合与停在哪一档（全屏 / 半屏）都在它手里。句柄必须跨帧持有 ——
    /// 弹层每帧都要构造（弹簧的采样在里面），与三条滑条同一条理由。
    sheet: Entity<BottomSheetState>,
    /// 目录树开着时的那份状态（整棵树 + 折叠集 + 搜索词）。`None` = 还没按当前位置
    /// 建过 —— 每次重新打开都重建一次，于是「树跟着阅读位置走」这条规矩不会被上一次
    /// 手动折的格带过来。
    tree: Option<TocView>,
    /// 搜索框开没开。框自己的状态由 `window.use_keyed_state` 按这一屏的键持有，
    /// 所以这里只存一个开关。
    search_open: bool,
    /// 虚拟列表的逐项尺寸表。只在「行数或行高变了」时重建，不每帧分配一张千项的表。
    sizes: Rc<Vec<Size<Pixels>>>,
    list_len: usize,
    row_height: f32,
    /// 列表的滚动句柄。**必须由这一屏跨帧持有**：`v_virtual_list` 每次调用都会新建一份
    /// 内部句柄，若不在建列表时用 `.track_scroll` 顶掉它，滚动偏移就每帧归零 —— 手指滑了、
    /// 下一帧又弹回原处，看起来像「滑不动」。
    scroll: VirtualListScrollHandle,
    /// 右上角那颗「⋯」的菜单开没开。菜单本体是 gpui-kit 的 `PopupMenu`（按下即弹，
    /// 点别处/选中即收），这一格只为了让那颗按钮吃到选中态，并让阅读页知道「身上压着东西」。
    more_open: bool,
    /// 书头那几行。与目录树同一次打开一起建（见 [`TocPanel::toggle`]）。
    header: Option<BookHeader>,
    _watches: Vec<Subscription>,
}

impl TocPanel {
    /// 建这一屏。弹层状态订阅一次：它不是这一屏的子视图，`notify` 只有观察方才收得到，
    /// 而开合、换档、读者一把下拉关掉全靠随后那一次重画。
    pub(in crate::ui::features::reader) fn new(
        reader: Entity<ReaderSession>,
        cx: &mut Context<Self>,
    ) -> Self {
        let sheet = cx.new(|cx| BottomSheetState::closed(cx));
        let watches = vec![cx.observe(&sheet, |_, _, cx| cx.notify())];
        Self {
            reader,
            sheet,
            tree: None,
            search_open: false,
            sizes: Rc::new(Vec::new()),
            list_len: 0,
            row_height: 0.0,
            scroll: VirtualListScrollHandle::new(),
            more_open: false,
            header: None,
            _watches: watches,
        }
    }

    /// 弹层开着吗。阅读页拿它决定「身上压着东西」要不要报给会话（那一击的语义是收浮层，
    /// 不是选字），也拿它决定底栏那一格的选中态。
    pub(in crate::ui::features::reader) fn is_open(&self, cx: &App) -> bool {
        self.sheet.read(cx).is_open()
    }

    /// 那颗「⋯」的菜单开着吗。它也是压在正文之上的一层。
    pub(in crate::ui::features::reader) fn menu_open(&self) -> bool {
        self.more_open
    }

    /// 这一屏此刻占掉正文左边多少像素。
    ///
    /// 只有宽档的那条左侧栏占位：手机那一档它是压在正文**之上**的弹层，正文一寸不让
    /// （阅读页的规矩 —— 点一下屏幕字不该跳走）。调用方把这 returned 加进
    /// `Reserve.width`，正文的换行、分栏与命中换算就全跟着走同一份数。
    pub(in crate::ui::features::reader) fn rail_width(&self, cx: &App, size: SizeClass) -> f32 {
        if size.is_compact() || !self.is_open(cx) {
            0.0
        } else {
            TOC_RAIL
        }
    }

    /// 目录弹层：开 or 关。
    ///
    /// 开的时候按当前位置重算一遍：读者上次手动折的那些格不该跨次生效，他这一次要回答的
    /// 问题还是「我现在在哪」。搜索框与「⋯」那一层一并收掉；书头跟着重查（读者可能在
    /// 「信息」那一屏改过书名或作者）。
    pub(in crate::ui::features::reader) fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.sheet.read(cx).is_open() {
            self.hide(cx);
            return;
        }
        self.tree = None;
        self.search_open = false;
        self.header = self.read_header(cx);
        self.sheet.update(cx, |sheet, cx| sheet.show(cx));
        cx.notify();
    }

    /// 收掉这一层（读者点了遮罩、下拉到底、或按了系统返回）。
    pub(in crate::ui::features::reader) fn hide(&mut self, cx: &mut Context<Self>) {
        self.sheet.update(cx, |sheet, cx| sheet.hide(cx));
        cx.notify();
    }

    /// 查一次书头要的那几样（书名 / 作者 / 格式 / 大小 / 封面）。
    ///
    /// 查不到就 `None`：兜底那一行没有书，那时候这一屏只剩「已读」那一行与一句实话，
    /// 而不是摆半截空书头让读者以为这本书坏了。
    ///
    /// 在打开这一层时查而不是在渲染里查，与架面点开「信息」那一屏同一取舍：一次打开
    /// 一回，两趟查询（`book` 与 `book_cover`），不每帧把界面线程钉在 SQLite 上。
    fn read_header(&self, cx: &App) -> Option<BookHeader> {
        let reader = self.reader.read(cx);
        let store = reader.store.clone()?;
        let id = reader.book_id?;
        let row = store.book(id).ok().flatten()?;
        let cover = match store.book_cover(id).ok().flatten() {
            Some(bytes) => cover_image(&bytes),
            None => None,
        };
        Some(BookHeader { row, cover })
    }

    /// 按当前位置建一次目录树。已经建过就不动 —— 重建会把读者手动折的格冲掉；
    /// 「重新打开要跟着位置走」那条重置在 [`Self::toggle`] 里显式做。
    ///
    /// 建好顺手把列表滚到当前章那里：默认全折，所以那常常是**顶层那条装着它的分支**
    /// （判据在 `toc_tree::landing_row`）。`scroll_to_item` 记的是「下一趟布局要滚到哪」，
    /// 此刻列表还没搭 —— 正合适，虚拟列表建起来那一帧自己会应用它。
    fn ensure_tree(&mut self, cx: &mut Context<Self>) {
        if self.tree.is_some() {
            return;
        }
        let rows = self.reader.read(cx).toc_rows();
        let current = self.reader.read(cx).chapter_index();
        self.tree = Some(TocView::open(rows));
        let toc = self.tree.as_ref().expect("上面刚建好");
        if let Some(ix) = toc
            .landing_ix(current)
            .and_then(|ix| toc.visible_position(ix))
        {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
    }

    /// 这一屏那四块：钉住的表头、书头、（开着的）搜索行、列表。
    ///
    /// 底部弹层与左侧栏两种容器读的是同一份装配 —— 哪些行看得见、折了哪几枝、搜的什么词、
    /// 行高哪一档，换容器不该改任何一条。`width` 递进来只为一件事：虚拟列表那张逐项尺寸表
    /// 要用**它自己那块容器**的宽，而不是视口的宽。
    ///
    /// 这几块子元素先各自算完：`cx.theme()` 还回去的 `&Theme` 借的是 `*cx`，把它留在
    /// 一条链里会活过整条 `let`，链上那几个要 `&mut cx` 的调用就过不去。
    fn pieces(&mut self, window: &mut Window, rail: bool, cx: &mut Context<Self>) -> Pieces {
        // 尺寸表用**这一屏自己那块容器**的宽：弹层铺满视口，左侧栏只有 `TOC_RAIL`。
        let width = if rail {
            TOC_RAIL
        } else {
            f32::from(window.viewport_size().width)
        };
        self.ensure_tree(cx);
        // 搜索框的状态按这一屏的键持有，订阅句柄跟着它一起活：输入一改变就重画这一屏。
        let search = window.use_keyed_state("reader-toc-search", cx, |window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx));
            let watch = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
            (input, watch)
        });
        // 只克隆那个句柄：对 `search.read(cx)` 整体调 `.clone()` 克隆的是**引用**
        // （`&T: Clone` 是平凡实现），拿回来的还是借用的元组，下面要 `&mut cx` 的
        // 每一次调用都会撞在它上面。
        let input = search.read(cx).0.clone();
        if let Some(toc) = &mut self.tree {
            toc.set_query(input.read(cx).value().to_string());
        }

        let row_height = ConfigHolder::reader(cx).toc_density.row_height();
        let count = self.tree.as_ref().map_or(0, |toc| toc.visible().len());
        // 行数（折展、搜索会改）或行高（设置里换了密度）变了才重造那张尺寸表。
        if count != self.list_len || (row_height - self.row_height).abs() > 0.01 {
            self.sizes = Rc::new(vec![size(px(width), px(row_height)); count]);
            self.list_len = count;
            self.row_height = row_height;
        }

        Pieces {
            head: self.book_head(cx),
            header: self.header_row(rail, cx),
            search_row: self.search_open.then(|| self.search_row(&input, cx)),
            list: self.tree_list(cx),
        }
    }

    /// 目录那一屏（手机档）：一块底部弹层，停在「全屏 / 半屏」两档上，往下拖过头就关掉。
    ///
    /// 换成弹层换来的是**一套统一的退场**：拇指在屏幕上往下一划、点遮罩、按返回，三条路都
    /// 收这一层。以前只有「点正文那一下」收得掉它，而那一击在目录开着的时候语义是含糊的 ——
    /// 读者以为点的是空白，实际点的是遮罩，而遮罩底下还有整屏的正文 hitbox 在等同一下。
    ///
    /// 顶栏与底栏一起压在遮罩下面，所以这一屏不再为它们让出那 172 像素，只还回把手带那 36：
    /// 在 Meizu 20（逻辑高约 857）上净多约 67 像素（Compact 档两行不到），桌面窗口越矮省得
    /// 越多。行高密度还是从设置读，不因为换了容器就改读者量出来的那一档。
    ///
    /// 列表自带滚动，所以这里**不开** `surface_draggable`：内容区那一把长拖该归目录自己滚，
    /// 判「滚到顶了再接着往下拽」这一步 gpui 给不出偏移。
    fn sheet_element(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let sheet = BottomSheet::new(&self.sheet, "reader-toc")
            .expanded(TOC_EXPANDED)
            .peek(TOC_PEEK);
        // 收稳了就不白算这一屏：整棵树要逐层展开成可见行、尺寸表要比对、搜索框要回读。
        // 元素照旧构造（弹簧的采样在里面），只是这一帧不给内容。
        if self.sheet.read(cx).is_idle() {
            return sheet.into_any_element();
        }
        let pieces = self.pieces(window, false, cx);
        sheet
            // 「已读 N / M」+ 搜索 + ⋯ 那一行钉在把手下面、不跟着列表滚：读者翻到第 800 章
            // 那一带时还得够得着搜索和「⋯」。
            //
            // 书头**也钉住**（曾经设想它跟着列表滚，但列表是 `v_virtual_list`、自己就是滚动
            // 容器：它是书头的兄弟，两者不在同一条滚动里，滑的只会是列表）。实测下来钉住更好认
            // ——「这是哪一本、读到哪儿了」始终在眼前。搜索那一行只在点开搜索后出现，也跟着钉。
            .header(pieces.header)
            .child(pieces.head)
            .children(pieces.search_row)
            .child(pieces.list)
            .into_any_element()
    }

    /// 目录那一屏（平板 / PC 档）：正文左边一条通高的定宽栏。
    ///
    /// 与弹层那一屏的**唯一**区别是容器：四块内容、行高、折叠、搜索全同一份（见 [`Self::pieces`]）。
    /// 这条栏占掉的宽由阅读页并进 `Reserve.width`，所以正文的换行、分栏与命中换算跟着同一数走，
    /// 不是「栏压字、字不知道」。
    ///
    /// `occlude` 不能少：正文的 hitbox 是整屏的，gpui 的一击发给**每一个** hitbox 含按下点的
    /// handler，不拦住的话，点栏里的一片空白会同时被正文吃掉 —— 在正文那一侧就是「顺手翻了页」。
    fn rail_element(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.sheet.read(cx).is_open() {
            // 没开着就一格不占：阅读页那头的 `rail_width` 也会跟着归零，两边必须同一个判据。
            return div().into_any_element();
        }
        let pieces = self.pieces(window, true, cx);
        let (surface, border) = (cx.theme().background, cx.theme().border);
        div()
            .id("reader-toc-rail")
            .occlude()
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .w(px(TOC_RAIL))
            .flex()
            .flex_col()
            .bg(surface)
            .child(pieces.header)
            .child(pieces.head)
            .children(pieces.search_row)
            .child(pieces.list)
            // 右边那条竖发的分隔线自己画：栏与正文同为 `background` 色，没有这一条就分不出
            // 「这边是目录、那边是书」。框架只有四边同宽的 `border_1`，单边只能自己摆一格。
            .child(
                div()
                    .absolute()
                    .right_0()
                    .top_0()
                    .bottom_0()
                    .w(px(0.5))
                    .bg(border),
            )
            .into_any_element()
    }

    /// 目录右上角那颗「⋯」：四件事收进一条 gpui-kit 的 `PopupMenu`。
    ///
    /// 换成组件换来的是**一整套现成的开合**：按下即弹、点别处即收、上下键走位、回车确认，
    /// 全在 `PopupMenu` 里，这一屏不再自己垫底、不再判「点在不在卡片里」。菜单内容在
    /// **每次打开时重建**（`DropdownMenu` 的 builder 是 `Fn`），所以它读到的是当下状态
    /// （展开 / 折叠、逆向排序开没开），与旧的那块弹层同一套语义。四件事各自把命令派回
    /// 这一屏的既有方法。
    ///
    /// 锚点选 `BottomRight`：菜单挂在按钮的右下角，往下展开时不会盖住它自己的那张脸。
    fn more_button(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let more_label = tr(cx, "reader.toc.more");
        let build = {
            let panel = cx.entity().clone();
            move |menu: PopupMenu, _window: &mut Window, cx: &mut Context<PopupMenu>| {
                let (all_open, reversed) = {
                    let this = panel.read(cx);
                    let all_open = this.tree.as_ref().is_some_and(|toc| toc.all_open());
                    let reversed = this.tree.as_ref().is_some_and(|toc| toc.reversed());
                    (all_open, reversed)
                };
                let expand_label = tr(
                    cx,
                    if all_open {
                        "reader.toc.collapse_all"
                    } else {
                        "reader.toc.expand_all"
                    },
                );
                let reverse_label = tr(cx, "reader.toc.reverse");
                let style_label = tr(cx, "reader.toc.style");
                let clear_label = tr(cx, "reader.toc.clear_read");
                // 四个闭包各自要一份句柄（`Entity` 不 `Copy`），所以一件事一个名字。
                let panel_expand = panel.clone();
                let panel_reverse = panel.clone();
                let panel_style = panel.clone();
                let panel_clear = panel.clone();
                menu.min_w(px(180.))
                    .item(
                        PopupMenuItem::new(expand_label)
                            .icon(if all_open {
                                IconName::FoldVertical
                            } else {
                                IconName::UnfoldVertical
                            })
                            .on_click(move |_, _, cx| {
                                panel_expand.update(cx, |this, cx| this.toggle_expand_all(cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new(reverse_label)
                            .icon(IconName::ArrowDownUp)
                            .checked(reversed)
                            .on_click(move |_, _, cx| {
                                panel_reverse.update(cx, |this, cx| this.toggle_reversed(cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new(style_label)
                            .icon(IconName::SlidersHorizontal)
                            .on_click(move |_, _, cx| {
                                panel_style.update(cx, |this, cx| this.settings_wanted(cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new(clear_label)
                            .icon(IconName::Eraser)
                            .on_click(move |_, _, cx| {
                                panel_clear.update(cx, |this, cx| this.clear_read_marks(cx))
                            }),
                    )
            }
        };
        let open_panel = cx.entity().clone();
        Button::new("toc-more")
            .ghost()
            .small()
            .selected(self.more_open)
            .icon(IconName::CircleEllipsis)
            .tooltip(more_label.clone().to_string())
            .accessibility_label(more_label)
            .dropdown_menu_with_anchor(Anchor::BottomRight, build)
            .on_open_change(move |open, _window, cx| {
                let open = *open;
                open_panel.update(cx, |this, cx| {
                    if this.more_open != open {
                        this.more_open = open;
                        cx.notify();
                    }
                });
            })
            .into_any_element()
    }

    /// 头部那一行：「已读 N / M 章」+ 搜索 + ⋯。
    ///
    /// ⋯ 里放的是这一屏用得上的那四件事（展开目录 / 逆向排序 / 目录样式 / 清除已读痕迹），
    /// 见 [`Self::more_button`]。原先这里直接是一颗「目录样式」，那是四件事里的一件，
    /// 摆成一颗按钮就把其余三件挤没了。
    fn header_row(&mut self, rail: bool, cx: &mut Context<Self>) -> AnyElement {
        let current = self.reader.read(cx).chapter_index();
        let (read, total) = self
            .tree
            .as_ref()
            .map(|toc| toc.read_count(toc.current_ix(current)))
            .unwrap_or((0, 0));
        let label = tf(cx, "reader.toc.read", &[("read", &read), ("total", &total)]);
        let search_panel = cx.entity().clone();
        let search_label = tr(cx, "reader.toc.search");
        h_flex()
            .w_full()
            .items_center()
            .gap_1()
            .px(px(TOC_GUTTER))
            .h(px(44.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                Button::new("toc-search")
                    .ghost()
                    .small()
                    .selected(self.search_open)
                    .icon(IconName::Search)
                    .tooltip(search_label.clone().to_string())
                    .accessibility_label(search_label)
                    .on_click(move |_, _, cx| {
                        search_panel.update(cx, |this, cx| {
                            this.search_open = !this.search_open;
                            cx.notify();
                        })
                    }),
            )
            .child(self.more_button(cx))
            // 左侧栏那一档没有遮罩可点、也没有「往下划」这回事，收它要靠这一颗：
            // 正文那一条入口在浮层收起时就看不见，唯一常驻的入口只能长在这一屏自己身上。
            .when(rail, |this| {
                let close_panel = cx.entity().clone();
                let close_label = tr(cx, "reader.toc.close");
                this.child(
                    Button::new("toc-close")
                        .ghost()
                        .small()
                        .icon(IconName::X)
                        .tooltip(close_label.clone().to_string())
                        .accessibility_label(close_label)
                        .on_click(move |_, _, cx| close_panel.update(cx, |this, cx| this.hide(cx))),
                )
            })
            .into_any_element()
    }

    /// 书头那一块：封面 + 书名 + 作者 + 「读到 X% · 格式 · 大小」。
    ///
    /// 摆在目录树上面，因为读者从目录里认的是「这是哪一本、读到哪儿了」，而那一屏
    /// 原本只有「已读 N / M 章」一句 —— 那个数不带书名，跳完一章回来就不确定还在不在
    /// 同一本书里。
    ///
    /// 查不到书（兜底那一行）时整块不画，而不是画三个空框：那一行本来就没有书可认。
    fn book_head(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(header) = self.header.as_ref() else {
            return div().into_any_element();
        };
        let progress = self.reader.read(cx).progress();
        let row = &header.row;
        let authors: SharedString = row.authors.join(AUTHOR_SEP).into();
        // 那个百分比与底栏状态行、页脚那一条共用 `percent_label`：同一本书在两处念出来
        // 的进度必须是同一个数，两处各写一套小数位迟早对不上。
        let meta: SharedString = tf(
            cx,
            "reader.toc.head_meta",
            &[
                ("percent", &percent_label(progress)),
                ("format", &row.format),
                ("size", &fmt_size(row.size)),
            ],
        )
        .into();
        let muted = cx.theme().muted_foreground;
        let placeholder = cx.theme().muted;
        h_flex()
            .id("reader-toc-head")
            .w_full()
            .items_start()
            .gap_3()
            .px(px(TOC_GUTTER))
            .pt_3()
            .pb_2()
            .child(
                // 封面 48×66dp（参照 App 实测 135×185px @2.8），与架上那一格同一条
                // 2:3 比例；没有图就画一块灰底 —— 旁边那两行写着书名，不会读成坏了。
                div()
                    .flex_none()
                    .w(px(48.0))
                    .h(px(72.0))
                    .overflow_hidden()
                    .rounded(px(2.0))
                    .bg(placeholder)
                    .when_some(header.cover.clone(), |this, image| {
                        this.child(img(image).size_full().object_fit(ObjectFit::Cover))
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .truncate()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(row.title.clone()),
                    )
                    .when(!authors.is_empty(), |this| {
                        this.child(div().truncate().text_sm().text_color(muted).child(authors))
                    })
                    .child(div().truncate().text_sm().text_color(muted).child(meta)),
            )
            .into_any_element()
    }

    /// 「展开目录 / 折叠目录」：两端来回摆。
    ///
    /// 行数变了不在这儿重造那张尺寸表：渲染那一趟自己比对「行数或行高变了才重建」
    /// （见 [`Self::sheet_element`]），在这儿再写一遍就是同一件事两处做。
    /// 菜单的收场由 `PopupMenu` 自己管（选中一项即 dismiss），这里不再手动收。
    fn toggle_expand_all(&mut self, cx: &mut Context<Self>) {
        if let Some(toc) = self.tree.as_mut() {
            if toc.all_open() {
                toc.collapse_all();
            } else {
                toc.expand_all();
            }
        }
        cx.notify();
    }

    /// 正着摆 / 倒着摆。
    fn toggle_reversed(&mut self, cx: &mut Context<Self>) {
        if let Some(toc) = self.tree.as_mut() {
            toc.set_reversed(!toc.reversed());
        }
        cx.notify();
    }

    /// 抹掉「来过」的那份痕迹：灰字是读者自己翻出来的，他要一次清干净。
    fn clear_read_marks(&mut self, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.clear_read_marks(cx));
        cx.notify();
    }

    fn search_row(&self, input: &Entity<InputState>, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .px(px(TOC_GUTTER))
            .pb_2()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Search)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    // 没有占位文案可给：`InputState` 与 `Input` 都不收 placeholder
                    // （gpui-kit 0.7.1 只有数字输入框有那一项）。这一行只在点了搜索
                    // 之后出现，旁边那颗放大镜就是它自己的说明。
                    .child(Input::new(input).flex_1().min_w_0()),
            )
            .into_any_element()
    }

    /// 列表本体。没有目录的书（TXT / MD / FB2 与定版式）在这里给一句实话，
    /// 而不是摆一屏空白让读者以为是排版没做完。
    ///
    /// 外面那层 `flex_1` 是必需的：`v_virtual_list` 建成 `size_full`，若让它直接做弹层
    /// 内容那一格的兄弟，它会按**整格高**撑开、比剩下可用高多出书头那一段 —— 多出来的
    /// 部分被裁在格底之下，滚到最后一条时那一行就看不见了（用户报的「滑到底被遮挡」）。
    /// 包一层 `flex_1` 把「剩下多少高」交给它，列表底边才与内容底边对齐；底部再留出
    /// [`TOC_BOTTOM_SPACE`]，最后一行不贴着弹层下沿。
    fn tree_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.list_len == 0 {
            return div()
                .w_full()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr(cx, "reader.toc.empty"))
                .into_any_element();
        }
        let view = cx.entity().clone();
        div()
            .flex_1()
            .min_h_0()
            .pb(px(TOC_BOTTOM_SPACE))
            .child(
                v_virtual_list(
                    view,
                    "reader-toc",
                    self.sizes.clone(),
                    |panel, range, _, cx| panel.rows(range, cx),
                )
                // 句柄跨帧持有（见字段 `toc_scroll`）：不顶掉 `v_virtual_list` 每帧新建的
                // 那一份，滚动偏移每帧归零，目录就滑不动。
                .track_scroll(&self.scroll),
            )
            .into_any_element()
    }

    /// `[range.start, range.end)` 这几行目录。
    ///
    /// 三个信号各管各的，不互相顶：当前行 = 底色 + 左边那条标记；来过 = 文字变灰
    /// （设置里可以关掉）；对不上正文的条目 = 灰到底且**没有**点击回调。
    fn rows(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(toc) = &self.tree else {
            return Vec::new();
        };
        let rows = toc.visible();
        let end = range.end.min(rows.len());
        if range.start >= end {
            return Vec::new();
        }
        let prefs = ConfigHolder::reader(cx);
        let row_height = prefs.toc_density.row_height();
        let lines = prefs.toc_max_title_lines as usize;
        let current = self.reader.read(cx).chapter_index();
        let read = self.reader.read(cx).read_chapters().clone();
        let (foreground, muted, surface) = (
            cx.theme().foreground,
            cx.theme().muted_foreground,
            cx.theme().muted,
        );
        let transparent = hsla(0.0, 0.0, 0.0, 0.0);
        let panel = cx.entity().clone();
        rows[range.start..end]
            .iter()
            .map(|row| {
                let ix = row.ix;
                let active = row.chapter == Some(current);
                let visited = prefs.toc_dim_read
                    && row.chapter.is_some_and(|chapter| read.contains(&chapter));
                // 当前行不跟着「来过」变灰：它已经吃了底色与标记条，再灰一层就变成
                // 「看不出现在在哪」，而那正是这一屏唯一必须答对的问题。
                let color = if active || !visited {
                    foreground
                } else {
                    muted
                };
                let chevron = if row.branch {
                    let panel_toggle = panel.clone();
                    Button::new(ElementId::named_usize("reader-toc-toggle", ix))
                        .ghost()
                        .small()
                        .w(px(TOC_CHEVRON))
                        .p_0()
                        .justify_center()
                        .icon(if toc.is_collapsed(ix) {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .on_click(move |_, _, cx| {
                            // 不拦的话这一击会接着冒到整行上：读者点的是「展开」，
                            // 结果书也跳了、面板也收了，看起来像这颗箭头是坏的。
                            cx.stop_propagation();
                            panel_toggle.update(cx, |this, cx| {
                                if let Some(toc) = &mut this.tree {
                                    toc.toggle(ix);
                                }
                                cx.notify();
                            })
                        })
                        .into_any_element()
                } else {
                    // 叶子占同样一格，标题才不会因为有没有下级而左右跳。
                    div().w(px(TOC_CHEVRON)).flex_none().into_any_element()
                };
                // 三段：一点空隙 → 标记条 → 底色带。带子从标记条右边起、铺到屏幕右沿，
                // 标题落在 `TOC_TEXT_LEFT` 那一列上（chevron 那一格在它左边）。
                let mut row_el = div()
                    .id(ElementId::named_usize("reader-toc-row", ix))
                    .w_full()
                    .h(px(row_height))
                    .flex()
                    .items_stretch()
                    .child(
                        div()
                            .flex_none()
                            .w(px(TOC_MARKER))
                            .ml(px(TOC_MARKER_INSET))
                            .bg(if active { foreground } else { transparent }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .overflow_hidden()
                            .pl(px(TOC_TEXT_LEFT - TOC_BAND_LEFT - TOC_CHEVRON
                                + (row.depth as f32) * TOC_INDENT))
                            .pr(px(TOC_GUTTER))
                            .when(active, |el| el.bg(surface))
                            .child(chevron)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .line_clamp(lines)
                                    .when(active, |el| el.font_weight(FontWeight::SEMIBOLD))
                                    .text_color(color)
                                    .child(row.title.clone()),
                            ),
                    );
                // 对不上正文的那几条不挂回调：灰着且点不动，比「点了什么也没发生」诚实。
                if let Some(chapter) = row.chapter {
                    let fragment = row.fragment.clone();
                    let panel_jump = panel.clone();
                    row_el = row_el.on_click(move |_, _, cx| {
                        panel_jump.update(cx, |this, cx| {
                            this.jump_to_entry(chapter, fragment.clone(), cx)
                        })
                    });
                }
                row_el.into_any_element()
            })
            .collect()
    }

    /// 跳到目录里的某一条，并把这一层整个收掉：读者要看的正是跳到的那几行字。
    fn jump_to_entry(&mut self, chapter: usize, fragment: Option<String>, cx: &mut Context<Self>) {
        self.reader
            .update(cx, |reader, cx| reader.goto_entry(chapter, fragment, cx));
        // 跳到哪一条就把目录整个收掉：读者要看的正是跳到的那几行字。
        self.sheet.update(cx, |sheet, cx| sheet.hide(cx));
        self.tree = None;
        self.search_open = false;
        // 顶栏底栏与排版面板都在阅读页手里，收干净由它来（见 [`TocEvent::Navigated`]）。
        cx.emit(TocEvent::Navigated);
        cx.notify();
    }

    /// 「目录样式」那一下：设置那一屏要压进导航栈，而栈长在阅读页手里 —— 这里只把意愿报出去。
    fn settings_wanted(&mut self, cx: &mut Context<Self>) {
        cx.emit(TocEvent::OpenSettings);
    }
}

impl Render for TocPanel {
    /// 同一份状态、两种表面：手机那一档是压在正文之上的底部弹层，平板与桌面那一档是
    /// 左边一条定宽栏（正文让出那条宽）。哪些行看得见、折了哪几枝、搜的是什么词、
    /// 跳到哪一条 —— 全部只有一份，两种摆法读的是同一个 `TocView`。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if SizeClass::of(window).is_compact() {
            // 弹层**每帧都构造**，收着时也构造 —— 弹簧的采样在里面，只在开着时构造会把
            // 退场动画连状态一起丢（见 `widgets::bottom_sheet` 那段模块注释）。
            self.sheet_element(window, cx)
        } else {
            self.rail_element(window, cx)
        }
    }
}

/// 这一屏与阅读页之间那两道通知。订阅方是 [`super::page::ReadingPage`]（见它的 `_watches`）。
impl EventEmitter<TocEvent> for TocPanel {}

#[cfg(test)]
mod tests {
    //! 目录那一屏搬进自己的实体之后，这几条钉的是**接线**：`toggle` 开得起这一层、画一帧
    //! 整屏不炸（树、书头、尺寸表、虚拟列表都在这条路上）、点一条能把跳转派给会话并把
    //! 「浮层该收了」报回阅读页。摆出来的样子 —— 刻度、缩进、颜色 —— 仍然只有真机量得出来。
    //!
    //! 点名导入而不 `use super::*`：`gpui_kit` 根把 `gpui` 整个转了出来，里面有一个
    //! 叫 `test` 的属性宏，glob 会让下面的 `#[test]` 递归展开成它。

    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use folio_core::content::document::Document;
    use folio_core::content::format::TocEntry;
    use folio_core::{Chapter, Config, Content, DocMeta, FlowDoc, Format, Locator, Reader};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, Subscription, TestAppContext, px, size,
    };

    use super::{TOC_RAIL, TocEvent, TocPanel};
    use crate::ui::features::reader::session::ReaderSession;
    use crate::ui::shared::config_model::ConfigHolder;
    use crate::ui::shell::navigation::SizeClass;

    /// 一套开好了的「目录这一屏」：会话、这一屏、窗口句柄、收到的事件，以及那条订阅自己。
    ///
    /// 起别名而不写在这里展开：五层嵌套的元组签名，clippy 与读的人都要费劲。
    type Opened = (
        Entity<ReaderSession>,
        Entity<TocPanel>,
        AnyWindowHandle,
        Rc<RefCell<Vec<TocEvent>>>,
        Subscription,
    );

    /// 一本两章、每章各挂着一条目录的书，加一扇把**这一屏**摆在根上的窗。
    ///
    /// 根就是这一屏：`v_virtual_list` 与弹层都要真窗口，而这一屏的渲染路径正是这里要测的那条。
    /// 最后那格是订阅句柄 —— `Subscription` 一 drop 就退订，测试必须把它攥着。
    fn open_panel(cx: &mut TestAppContext, width: f32) -> Opened {
        let flow = FlowDoc {
            chapters: vec![
                Chapter::inline("第一章", "<p>第一章的第一段字。</p>")
                    .with_spine_href("c0.xhtml".to_string()),
                Chapter::inline("第二章", "<p>第二章的第一段字。</p>")
                    .with_spine_href("c1.xhtml".to_string()),
            ],
            toc: vec![
                TocEntry {
                    title: "第一章".into(),
                    href: "c0.xhtml".into(),
                    ..Default::default()
                },
                TocEntry {
                    title: "第二章".into(),
                    href: "c1.xhtml".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let meta = DocMeta {
            title: "测试书".into(),
            authors: Vec::new(),
            format: Format::Markdown,
            size: 0,
            ..DocMeta::default()
            };
        let doc: Arc<dyn Document> = Arc::from(folio_core::content::document::from_content(
            meta,
            Content::Reflow(flow),
        ));
        let events: Rc<RefCell<Vec<TocEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let mut built = None;
        let mut watch = None;
        let handle = cx.open_window(size(px(width), px(520.)), |window, cx| {
            ConfigHolder::install(Config::default(), cx);
            let session = cx.new(|cx| {
                ReaderSession::with_document(
                    doc,
                    Reader::default(),
                    None,
                    None,
                    Locator::start(),
                    cx,
                )
            });
            let panel = cx.new(|cx| TocPanel::new(session.clone(), cx));
            let sink = events.clone();
            // 句柄必须留着：`Subscription` 一 drop 就退订，事件那条路就此不通。
            watch = Some(cx.subscribe(&panel, move |_, _, event: &TocEvent, _| {
                sink.borrow_mut().push(*event)
            }));
            let root_view = panel.clone();
            built = Some((session, panel));
            Root::new(root_view, window, cx)
        });
        let (session, panel) = built.expect("会话与目录那一屏都该建起来");
        (
            session,
            panel,
            handle.into(),
            events,
            watch.expect("订阅句柄该在开窗时挂上"),
        )
    }

    /// 打开这一屏：弹层开着，两条目录都摆得出来。
    #[gpui_kit::test]
    fn the_panel_opens_and_draws_its_rows(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_session, panel, handle, _events, _watch) = open_panel(cx, 460.);
        panel.update(cx, |panel, cx| panel.toggle(cx));
        assert!(
            panel.read_with(cx, |panel, cx| panel.is_open(cx)),
            "toggle 该把这块底部弹层开起来"
        );
        // 画一帧：树、书头、尺寸表与虚拟列表都在这条路上。挪文件时漏掉任何一根线，
        // 红要红在这里，而不是等读者在真机上看到一屏空白。
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("这一屏该画得出来");
        let rows = panel.update(cx, |panel, cx| panel.rows(0..4, cx));
        assert_eq!(rows.len(), 2, "两条目录都该摆出来");
    }

    /// 宽档上同一份状态换的是**容器**，不是内容：那条栏占的宽要报给阅读页，而列表里
    /// 摆的还是那两行。判据走反了（宽档也算「压着正文」）症状是桌面上点字选不中。
    #[gpui_kit::test]
    fn the_wide_screen_swaps_the_container_not_the_content(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_session, panel, handle, _events, _watch) = open_panel(cx, 900.);
        // 900 宽是 Expanded 档：这条栏占位。
        panel.update(cx, |panel, cx| {
            assert_eq!(
                panel.rail_width(cx, SizeClass::Expanded),
                0.0,
                "没开着时一寸不让"
            );
            panel.toggle(cx);
            assert_eq!(
                panel.rail_width(cx, SizeClass::Expanded),
                TOC_RAIL,
                "开着时正文让出这条栏"
            );
            // 手机那一档同一状态走的是弹层：不占位，压在正文之上。
            assert_eq!(panel.rail_width(cx, SizeClass::Compact), 0.0);
        });
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("左侧栏那一档也该画得出来");
        let rows = panel.update(cx, |panel, cx| panel.rows(0..4, cx));
        assert_eq!(rows.len(), 2, "两种表面读的是同一棵树");
    }

    /// 点一条目录：跳章派给会话，「浮层该收了」报回阅读页，这一层自己收掉。
    ///
    /// 那两条报事（`Navigated` / `OpenSettings`）是这一屏与阅读页之间**唯一**的口子，
    /// 事件没接上的症状是「跳完字了、顶栏底栏还压在上面」。
    #[gpui_kit::test]
    fn a_jump_goes_to_the_session_and_reports_back(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (session, panel, handle, events, _watch) = open_panel(cx, 460.);
        panel.update(cx, |panel, cx| panel.toggle(cx));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("这一屏该画得出来");

        panel.update(cx, |panel, cx| panel.jump_to_entry(1, None, cx));

        assert_eq!(
            session.read_with(cx, |v, _| v.chapter_index()),
            1,
            "会话该落到第二章"
        );
        assert_eq!(
            events.borrow().as_slice(),
            &[TocEvent::Navigated],
            "跳转要报给阅读页，它才好把顶栏底栏与排版面板收干净"
        );
        assert!(
            !panel.read_with(cx, |panel, cx| panel.is_open(cx)),
            "跳完该把这一层收掉：读者要看的正是跳到的那几行字"
        );
    }
}
