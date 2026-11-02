//! 整页画（固定底图章 / 单图页 / Calibre 封面）的识别与图片挑选。
//!
//! 这一层只回答「这一章/这一页有没有整页画、用哪几张、铺 cover 还是 contain」。
//! 元素树的摆放仍在 `body` 里：背板、读数带、翻页位移都由那边决定。

use crate::ui::features::reader::model::book::Book;
use crate::ui::features::reader::model::chapter_layout::Document;
use folio_core::content::format::css::computed::BackgroundSize;
use folio_core::layout::DecorSeed;

/// 这一帧当前章要画成整页画时的那张画（见 [`current_page_art`]）。
pub(super) struct PageArt {
    /// 属于哪一章：绘制时只对**这一章**跳过版心里的那张图。
    pub(super) chapter: usize,
    /// 按窗口尺寸交给 `scene::backdrop_decor` 折格子的层。
    pub(super) pictures: Vec<folio_core::layout::DecorPicture>,
    /// 单图页：那唯一一张图已经在背板上，正文那一章整章不画。
    pub(super) picture_only: bool,
}

/// 整页图该不该用 `contain`：比例和屏幕差得远时，宁可留底色也不裁掉内容。
///
/// 封面有 `0~slim` 这种同比例兄弟，走 `cover` 仍然不裁；正文里的横图/方图没有兄弟，
/// `cover` 会只剩中间一条 —— 读者报的「图片显示不全」就是它。
pub(super) fn page_art_contain(doc: &Document, href: &str, want_aspect: f32) -> bool {
    let (w, h) = doc.images().natural_size(href);
    if w <= 0.0 || h <= 0.0 || !want_aspect.is_finite() || want_aspect <= 0.0 {
        return false;
    }
    ((w / h) - want_aspect).abs() / want_aspect > 0.15
}

/// 这一帧的整页画：当前章（视口顶所在的那章）是一整页底图、或整章只有一张满宽图。
///
/// 阅读器把它贴到**整窗**上、读数带浮在上面（Reeden 那种画法），而不是排进版心：
/// 版心从页眉下面开始、左右还让着页边距，整页的画在那里永远只是一截。
/// 这一章的**整页底图**：根上那张铺满整章的背景图。
///
/// 两档，判据与 [`page_art_for_doc`] 第 1 条同一份：
///
/// - `full_bleed`（作者写了 `background-attachment: fixed`）无条件算；
/// - 其余根底图只在**这一章没有可读正文**时算 —— 扉页那种「底图 + 标题图、一个字没有」
///   的章，底图也该铺满整窗。
///
/// 返回装饰种子里那一条；绘制侧按它的 `pictures` 铺，排版侧按它决定「块里那份别画」。
pub(super) fn page_backdrop(doc: &Document) -> Option<&DecorSeed> {
    page_backdrop_index(doc).and_then(|index| doc.decor_seeds().get(index))
}

/// [`page_backdrop`] 挑中的那一条种子在 `decor_seeds()` 里的**下标**。
///
/// 判据与那一问完全同一份，只是多回一个位置：绘制侧要跳过的正是**这一条**，而
/// `full_bleed` 只是它的一个特例。《哈利·波特全集》`v1.xhtml` 的底图没写
/// `background-attachment: fixed`，从前按 `full_bleed` 筛的那一道闸（`scene::place_decorations`）
/// 拦不住它，于是同一张图既进了整窗背板、又留在版心里画了一遍 —— 而版心那一份压在
/// 不透明的纸底色下面，读者只看得见被裁成章高一条的那一截（2026-11-02 真机）。
pub(super) fn page_backdrop_index(doc: &Document) -> Option<usize> {
    let seeds = doc.decor_seeds();
    if let Some(index) = seeds
        .iter()
        .position(|seed| seed.full_bleed && !seed.pictures.is_empty())
    {
        return Some(index);
    }
    if doc.has_text() {
        return None;
    }
    seeds
        .iter()
        .position(|seed| seed.x.abs() < 0.5 && seed.y.abs() < 0.5 && !seed.pictures.is_empty())
}

pub(super) fn current_page_art(book: &Book, want_aspect: f32) -> Option<PageArt> {
    let chapter = book.chapter_index();
    let doc = book.span_doc(chapter)?;
    // 排版还没落地的那一帧没有可信的种子与行盒：等下一帧。
    if !doc.is_current() {
        return None;
    }
    page_art_for_doc(chapter, doc, want_aspect)
}

/// 某一章的整页画：根上的底图章，或整章只有一张满宽图。
pub(super) fn page_art_for_doc(
    chapter: usize,
    doc: &Document,
    want_aspect: f32,
) -> Option<PageArt> {
    // 1) 根上的底图。两档：
    //
    //    - `full_bleed`（作者写了 `background-attachment: fixed`）：卷首 / 画廊那类
    //      整页底图章，**不管有没有正文**都铺满；
    //    - 其余根底图只在**这一章没有可读正文**时铺满。《哈利·波特全集》`v1.xhtml`
    //      是「body 背景 + 两张标题图、一个字没有」的扉页，底图没写 `fixed`，从前掉进
    //      正文页，于是顶上顶着一条不透明页眉带、图从它下面起（读者报的
    //      「封面没有全屏，上面空出来一大片白」）。
    //
    //    判据用的是「有没有可读正文」（`Document::has_text`），不是「这一屏有没有字」：
    //    按屏判会让页眉带在滚动中忽隐忽现，比现在这版更吵。
    if let Some(seed) = page_backdrop(doc) {
        return Some(PageArt {
            chapter,
            pictures: seed.pictures.clone(),
            picture_only: false,
        });
    }
    // 2) 单图页：Calibre 封面、整页插图章。判据**不问界面**——排版层切完片之后那一片
    //    自己带着旗（`Fragment::picture_page`：这一片没有一行有宽度的正文，而且恰好一张
    //    铺到版心八成以上的图）。连续滚动那一档一片装完整章，所以这一问等价于从前
    //    「扫整章」那一条，但只有一份实现。
    //
    //    从前这里还要求「同目录里有一张与窗口宽高比差 ≤0.2 的兄弟图」（`0.jpg` 旁边
    //    摆 `0~slim.jpg` 那种封面变体）才肯拉满屏——两道闸只有封面页过得去，
    //    正文里那张独占一页的照片因此从来没被当整页画过。铺满那一档已定为 cover
    //    （盖住整窗、超出裁掉），比例不合不再是理由。
    let hrefs = doc.slice_of(0, 0)?.picture_page.clone();
    let [href] = hrefs.as_slice() else {
        return None;
    };
    // 4:3 的 `0.jpg` 与手机版的 `0~slim.jpg` 并排时，按屏幕比例挑更合身的那张：
    // 否则 cover 会把 4:3 左右各裁一截（读者报的「标题两端被切」）。
    let href = doc
        .images()
        .closest_portrait_variant(href, want_aspect)
        .unwrap_or_else(|| href.clone());
    let fit = if page_art_contain(doc, &href, want_aspect) {
        BackgroundSize::Contain
    } else {
        BackgroundSize::Cover
    };
    Some(PageArt {
        chapter,
        pictures: vec![crate::ui::features::reader::view::scene::full_page_picture(
            &href, fit,
        )],
        picture_only: true,
    })
}

/// 滚动档的整页画：宽度铺满这一格的图改成**贴顶**画（`background-position-y: 0`），
/// 其余原样。
///
/// 读者要的是「封面那种图在滚动档里贴着系统栏」——`contain` 居中会在顶上留一条底色，
/// 看起来像页眉下面又空了一块（Reeden 那张封面就是宽度铺满、贴顶、余下的地方留给底色）。
/// 分页 / 双页不走这里：那两档要的是「单独居中放一页」。
pub(super) fn scroll_art_pictures(
    art: &PageArt,
    width: f32,
    height: f32,
    images: &crate::ui::foundation::images::ChapterImages,
) -> Vec<folio_core::layout::DecorPicture> {
    let mut pictures = art.pictures.clone();
    for picture in &mut pictures {
        if crate::ui::features::reader::view::scene::picture_fills_width(
            picture, width, height, images,
        ) {
            picture.position.y =
                folio_core::content::format::css::computed::BackgroundAxis::Fraction(0.0);
        }
    }
    pictures
}

/// 分页档里「这一页的整页底图」那一层的元素 id。
///
/// 章与页各占一段：翻页途中两张纸可能同章也可能跨章，id 不能撞；宿主测试按它量
/// 「这张画这一帧在不在、走没走」（见 `body/tests/swipe.rs` 里那条平移用例）。
pub(super) fn page_art_element_id(chapter: usize, page: usize) -> gpui_kit::ElementId {
    (
        "reader-page-art",
        chapter.saturating_mul(1_000_000).saturating_add(page),
    )
        .into()
}

/// 这一章在滚动档里是不是按**窗口坐标**画的（单图页 / Calibre 封面）。
///
/// 命中换算要它：这一档的正文从窗口顶起画（`Scene::build` 拿到的 `top = 0`），
/// 反算时不把「页眉 + 安全区」那一段加回去，点到的是上面一截的字。
/// **固定底图章不算**：它的底图铺满整窗，正文仍从 `content_top` 起画。
///
/// 判据本体在 [`Document::is_picture_only_page`] —— 与 `page_art_for_doc` 的
/// `picture_only` 是同一件事，别在这里另写一份。
pub(in crate::ui::features::reader) fn chapter_is_picture_only_page(doc: &Document) -> bool {
    doc.is_picture_only_page()
}
