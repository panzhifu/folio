//! 一张纸的身份、摆位、捕获角色与纸内内容。
//!
//! 三种摆法都先装配成 [`Paper`]：可重排分页 / 连续滚动装 [`PageScene`]，
//! 定版式装 [`FixedPaper`]；`Sheet` 的位移、纸面几何与缩放都记在纸这一层。

use std::rc::Rc;

use crate::ui::features::reader::model::fixed_zoom::FixedZoom;
use crate::ui::features::reader::model::page_geometry::{BandPolicy, PageBox, PageRect};
use crate::ui::features::reader::model::session::PageRef;
use crate::ui::features::reader::motion::turn_animation::{Cut, Sheet};
use crate::ui::features::reader::view::body::fixed::FixedTileAt;
use crate::ui::features::reader::view::paint::GlyphsElement;
use crate::ui::features::reader::view::scene::Scene;
use folio_core::content::format::PageText;
use gpui_kit::{AnyElement, Bounds, Pixels};

/// 这一栏这一帧归哪一层画。
///
/// 翻页按下时同一份内容可能同时出现在主场景、正面捕获、背面捕获与进场捕获里；
/// 这个枚举把「画到哪儿、加不加位移」收成一处，替代原先散在循环里的三个布尔量。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CaptureRole {
    /// 正常实时页；需要烘正面纹理时另有副本。
    Live,
    /// 平摊翻页的出场页：只由捕获网格画，不再实时画。
    Front,
    /// 双页折页的背面：只进背面捕获层，不上主场景。
    Back,
    /// 进场页：实时画（或由进场网格画），需要时再烘一份进场捕获。
    Incoming,
    /// 双页里停着的那一栏：实时画，但不跟 `Sheet` 的位移。
    StaticOther,
}

impl CaptureRole {
    /// 这一栏要不要进主场景。
    pub(super) fn draws_main(self) -> bool {
        matches!(self, Self::Live | Self::Incoming | Self::StaticOther)
    }

    /// 这一栏要不要进背面捕获层。
    pub(super) fn writes_back(self) -> bool {
        self == Self::Back
    }

    /// 这一栏要不要进进场页捕获层。
    pub(super) fn writes_incoming(self) -> bool {
        self == Self::Incoming
    }

    /// 这一栏的实时内容跟不跟这张纸的 `Sheet::offset` 走。
    pub(super) fn moves_with_sheet(self) -> bool {
        matches!(self, Self::Live | Self::Incoming)
    }
}

/// 一张纸里装的内容。
pub(super) enum PaperBody {
    Reflowable(Vec<PageScene>),
    Continuous(Vec<PageScene>),
    Fixed(FixedPaper),
}

/// 一页定版式：页号、每格的最终摆位、字层与命中尺。
pub(super) struct FixedPaper {
    pub(super) page: usize,
    pub(super) tiles: Vec<FixedTileAt>,
    pub(super) text: Option<Rc<PageText>>,
    pub(super) text_key: Option<u64>,
    pub(super) placed: Option<Bounds<Pixels>>,
}

/// 这一帧的一张纸：哪一页、摆在哪儿、纸里有什么。
pub(super) struct Paper {
    pub(super) identity: PageRef,
    pub(super) paper: PageRect,
    pub(super) content: PageRect,
    pub(super) bands: BandPolicy,
    pub(super) sheet: Sheet,
    pub(super) zoom: Option<FixedZoom>,
    pub(super) body: PaperBody,
}

impl Paper {
    pub(super) fn empty(identity: PageRef, sheet: Sheet) -> Self {
        Self {
            identity,
            paper: PageRect::default(),
            content: PageRect::default(),
            bands: BandPolicy::Reserved,
            sheet,
            zoom: None,
            body: PaperBody::Reflowable(Vec::new()),
        }
    }

    /// 可重排一页（分页 / 双页）。纸面几何整份来自这一帧的 [`PageBox`]。
    pub(super) fn reflowable(
        identity: PageRef,
        page_box: PageBox,
        sheet: Sheet,
        scenes: Vec<PageScene>,
    ) -> Self {
        Self::scenes(identity, page_box, sheet, PaperBody::Reflowable(scenes))
    }

    /// 连续滚动里的一章（一摞栏场景，纵向拼在一条链上）。
    pub(super) fn continuous(
        identity: PageRef,
        page_box: PageBox,
        sheet: Sheet,
        scenes: Vec<PageScene>,
    ) -> Self {
        Self::scenes(identity, page_box, sheet, PaperBody::Continuous(scenes))
    }

    fn scenes(identity: PageRef, page_box: PageBox, sheet: Sheet, body: PaperBody) -> Self {
        Self {
            identity,
            paper: page_box.paper,
            content: page_box.content,
            bands: page_box.bands,
            sheet,
            zoom: None,
            body,
        }
    }

    /// 定版式一页：纸就是整扇窗，两条带浮在页图之上，缩放跟着纸走。
    ///
    /// 身份用 `(0, 页号)` 而不是 `(页号, 0)`：捕获与翻页两处的键都是
    /// `(会话的 chapter_index, 页号)`，定版式的 `chapter_index` 恒为 0（它没有章）。
    /// 两套写法混着用，`plan_flat` 就找不到同一张纸。
    pub(super) fn fixed(
        body: FixedPaper,
        sheet: Sheet,
        zoom: Option<FixedZoom>,
        viewport: (f32, f32),
    ) -> Self {
        let page = body.page;
        Self {
            identity: PageRef { chapter: 0, page },
            paper: PageRect::new(0.0, 0.0, viewport.0, viewport.1),
            content: PageRect::new(0.0, 0.0, viewport.0, viewport.1),
            bands: BandPolicy::Overlay,
            sheet,
            zoom,
            body: PaperBody::Fixed(body),
        }
    }
}

/// 一栏要画的内容：排版场景 + 它这一帧的摆位与捕获取舍。
///
/// `x/top` 已经含这一栏自己的 `Sheet` 位移；纸面几何在 [`Paper`] 上，
/// 这里只管栏内的场景与角色。
pub(super) struct PageScene {
    pub(super) identity: PageRef,
    pub(super) column: usize,
    pub(super) origin_y: f32,
    pub(super) height: f32,
    pub(super) range: std::ops::Range<f32>,
    pub(super) role: CaptureRole,
    pub(super) back_copy: bool,
    pub(super) incoming_copy: bool,
    pub(super) x: f32,
    pub(super) top: f32,
    pub(super) cut: Option<Cut>,
    pub(super) picture: Option<Picture>,
    pub(super) art_drawn: bool,
    pub(super) scene: Scene,
}

/// 整页图的落点：源 href、屏上左缘（已含位移）、宽、顶、`contain` 还是 `cover`。
pub(super) type Picture = (String, f32, f32, f32, bool);

/// 带子 wrapper 的测试 id：章、页、栏压进一个 `u64`，避免四元组没有 `Into<ElementId>`。
pub(super) fn band_id(kind: &'static str, page: PageRef, column: usize) -> gpui_kit::ElementId {
    gpui_kit::ElementId::from((
        kind,
        ((page.chapter as u64) << 40) | ((page.page as u64) << 8) | column as u64,
    ))
}

/// 一张纸的捕获副本：字形元素 + 压在字下的装饰/图 + 压在字上的图 + 带子。
///
/// `element` 已经标成捕获副本；body 只按 capture layer 的包裹位移把它摆回去。
pub(super) struct PaperCapture {
    pub(super) element: GlyphsElement,
    pub(super) under: Vec<AnyElement>,
    pub(super) over: Vec<AnyElement>,
    pub(super) bands: Option<AnyElement>,
}

/// 一份只有图、没有字形的捕获副本（定版式的页图）。`GlyphsElement` 仍要占着那一格：
/// [`PaperCapture`] 的装配顺序是 `under → element → over`，页图排在 `over` ——
/// 定版式的痕迹压在页图**之上**，所以页图不能进 `under`。
pub(super) fn empty_capture_element() -> GlyphsElement {
    GlyphsElement::new().capture()
}
