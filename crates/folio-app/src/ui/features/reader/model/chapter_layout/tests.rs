//! 本章的排版状态（本文件）与 `measure.rs` 在宿主上跑的用例。
//!
//! 为什么这些算移动端的测试：移动 UI 本身只在 `cfg(mobile)` 下编译，宿主的
//! `cargo test` 碰不到那一侧的视图。但「同一本书在两端排出一样的行」这件事
//! 完全落在这一层 —— 源文本包装、层叠、版心宽度、断行与缓存都在这里，
//! 移动端只是把它们摆上屏的那层薄壳。所以这一层被测住，移动端的排版就不是没测过。
//!
//! 反过来讲也很清楚：这里**测不到**的是触摸滚动、章节按钮的点击、主题在 Android 上
//! 解析到浅还是深 —— 那几件只有真开窗口、真上机器才算验证。

use std::ops::Range;
use std::sync::Arc;

use crate::ui::features::reader::model::book_fonts::BookFonts;
use crate::ui::features::reader::model::chapter_layout::{
    ChapterSource, Document, body_of, narrowed_lines,
};
use crate::ui::foundation::images::ChapterImages;
use folio_core::content::document::Document as OpenedDocument;
use folio_core::layout::LineFragment;
use folio_core::{Chapter, Content, DocMeta, FlowDoc, Format, Reader};
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Context, Render, Styled, TestAppContext, div, px, size,
};

/// 一个只占满窗口的空视图。这些用例要的是**窗口**（度量与视口都挂在窗口上），
/// 界面内容无关，所以给它最小的那一副。
struct Blank;

impl Render for Blank {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        _cx: &mut Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        div().size_full()
    }
}

/// 开一个指定尺寸的窗口，返回它的句柄。版心宽度的用例靠它给视口。
fn window(cx: &mut TestAppContext, width: f32, height: f32) -> AnyWindowHandle {
    cx.open_window(size(px(width), px(height)), |_, _| Blank)
        .into()
}

/// 两端对齐摊开的位移也要推图片：文本 run 靠 `justify_shift` 在绘制时推，
/// 图片没有那一层，漏掉就会被前面的字挤到左边。
#[test]
fn a_justified_line_pushes_an_inline_image_with_its_text() {
    use super::inline_image_x;
    use folio_core::layout::{Justify, JustifyMode};
    let justify = Justify {
        extra: 4.0,
        mode: JustifyMode::Chars,
        gaps: 10,
    };
    assert_eq!(inline_image_x(100.0, 2.0, Some(justify), 3), 114.0);
    assert_eq!(inline_image_x(100.0, 2.0, None, 3), 102.0);
}

/// 一段足够长的中文，保证它会折行 —— 不折行的话「缩进改变了布局」这种断言无从成立。
const LONG: &str = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。";

fn prefs() -> Reader {
    Reader::default().clamped()
}

/// 把版心相关的两项设成「不参与这段」的值：`margin_x` 归零、上限关掉。
/// 留白与上限本身另有用例，这里只关心缩进与折行。
fn bare_prefs() -> Reader {
    let mut prefs = prefs();
    prefs.margin_x = 0.0;
    prefs.max_measure = 0.0;
    prefs
}

/// 在窗口里把一段 XHTML 排完，返回排好的 [`Document`]。
///
/// 返回整个文档而不是行坐标，是因为要断言的东西有一半在文本里（哪一行是什么、
/// 有没有留下一个画不出来的图片位）。
fn layout_of(cx: &mut TestAppContext, xhtml: &str, prefs: &Reader, width: f32) -> Document {
    let handle = window(cx, width, 900.0);
    let mut document = Document::new(
        xhtml.to_string(),
        "",
        prefs.clone(),
        Arc::new(ChapterImages::empty()),
        // 这些用例钉的是排版与坐标，不是字体：空别名表 = 没有内嵌字体，
        // 与没有这个功能时逐字节同一条路径。
        Arc::new(BookFonts::empty()),
    );
    cx.update_window(handle, |_, win, cx| {
        document.ensure_layout(win, cx, 0.0);
    })
    .expect("窗口应当可排版");
    document
}

/// `max_measure == 0` 是「不设上限、随窗口铺满」的**哨兵值**，不是 0 像素的上限。
/// 当成上限去夹就会塌成 1px 版心、一行只剩一个字 —— 全屏阅读时整个正文会变成一列竖字。
#[gpui_kit::test]
fn a_zero_max_measure_means_no_cap(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = window(cx, 1000.0, 600.0);
    let mut uncapped = prefs();
    uncapped.margin_x = 48.0;
    uncapped.max_measure = 0.0;
    let mut capped = uncapped.clone();
    capped.max_measure = 400.0;

    let (free, limited) = cx
        .update_window(handle, |_, win, _| {
            (
                Document::content_width(win, &uncapped, 0.0),
                Document::content_width(win, &capped, 0.0),
            )
        })
        .expect("窗口应当可读");
    assert_eq!(free, 1000.0 - 48.0 * 2.0, "不设上限时版心随视口铺满");
    assert_eq!(limited, 400.0, "设了上限就按上限");
}

/// 侧栏那种「不属于阅读栏的宽度」必须从版心里扣掉，否则引擎按整窗宽折行、
/// 屏幕按窄栏裁剪，居中的标题会被推到栏外。移动端没有侧栏，所以传 0 ——
/// 同一个函数、同一个语义，这里也替那条路径把式子钉住。
#[gpui_kit::test]
fn the_pane_reserve_is_subtracted_from_the_measure(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = window(cx, 1000.0, 600.0);
    let prefs = bare_prefs();
    let width = cx
        .update_window(handle, |_, win, _| {
            Document::content_width(win, &prefs, 260.0)
        })
        .expect("窗口应当可读");
    assert_eq!(width, 740.0, "侧栏占位要从视口宽里扣掉");
}

/// **缓存的键必须覆盖所有排版输入**。
///
/// 这一条是回归门：缓存键曾经只记 `(宽度, 字号, 行距)`，于是读者在设置里改首行缩进或段距
/// 时，`ensure_layout` 认定几何参数没变而直接复用旧布局 —— 界面上看不到任何报错，
/// 只是那一项设置怎么改都不生效。现在键里带整份偏好，这条断言才会成立。
#[gpui_kit::test]
fn an_indent_change_reflows_the_document(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = window(cx, 600.0, 900.0);
    let mut prefs = bare_prefs();
    prefs.indent_em = 0.0;
    let mut document = Document::new(
        format!("<p>{}</p>", LONG.repeat(6)),
        "",
        prefs.clone(),
        Arc::new(ChapterImages::empty()),
        // 这些用例钉的是排版与坐标，不是字体：空别名表 = 没有内嵌字体，
        // 与没有这个功能时逐字节同一条路径。
        Arc::new(BookFonts::empty()),
    );

    // 第一次排版：无缩进。
    cx.update_window(handle, |_, win, cx| document.ensure_layout(win, cx, 0.0))
        .expect("第一次排版应当成功");
    let flat = document.lines().first().map(|line| line.x).unwrap_or(0.0);

    // 同一个文档、同一个窗口，**只**改首行缩进：宽度、字号、行距都没变。
    // 缓存键只记那三项时这里会直接复用上面的布局，第一行的左缘一动不动。
    prefs.indent_em = 2.0;
    document.set_prefs(prefs);
    cx.update_window(handle, |_, win, cx| document.ensure_layout(win, cx, 0.0))
        .expect("第二次排版应当成功");
    let indented = document.lines().first().map(|line| line.x).unwrap_or(0.0);

    assert!(
        indented > flat,
        "首行缩进改了必须重排：不缩进时第一行左缘 {flat}，缩进两字后 {indented}"
    );
}

/// **解不出资源的 `<img>` 整张跳过**，不塌成空盒、也不留一块画不出来的空白。
///
/// 这条对移动端尤其要紧：那一侧的图片现在按引擎给的位真画
/// （`view.rs` 用 gpui 的 `img` 元素），所以一个解不出来的 `src`
/// 若还留着位子，读者看到的就是一块空洞，还会以为正文排错了。
/// 跳过之后行里的文本必须依然完整。
#[gpui_kit::test]
fn an_unresolvable_image_is_skipped_not_left_as_a_gap(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let document = layout_of(
        cx,
        "<p>前<img src=\"missing.png\" width=\"120\" height=\"80\">后</p>",
        &bare_prefs(),
        600.0,
    );
    let line = document.lines().first().expect("至少要有一行");
    assert!(
        !line
            .fragments
            .iter()
            .any(|fragment| matches!(fragment, LineFragment::Image(_))),
        "解不出资源的 `<img>` 该整张跳过，不该留一个画不出来的位"
    );
    assert_eq!(
        document.line_texts().concat(),
        "前后",
        "跳过之后行里的文字仍然是完整的"
    );
}

/// 排版层要找的那个根：html5ever 会替片段补出 `<body>`，所以裸片段也有一个 body 可用；
/// 取到的必须是那个元素而不是文档根，否则 `<body>` 自己的 padding 与样式不生效。
#[test]
fn body_of_picks_the_body_element() {
    for source in [
        "<html><body><p>有 body</p></body></html>",
        "<p>只有正文</p>",
        "",
    ] {
        let dom = folio_core::content::dom::Dom::parse(source.as_bytes());
        let body = body_of(&dom);
        assert_eq!(
            dom.tag_name(body),
            Some("body"),
            "{source:?} 该排到 `<body>` 下面，实际取到了别的节点"
        );
    }
}

/// 进度坐标的往返：行顶像素 → 「第几个字」→ 行号，在**同一份排版**里必须是同一行；
/// 而换了版心宽与字号之后，它必须还落在同一段里。
///
/// 这就是开发方案 M5 那句验收（改字号 + 缩窗口 → 锚点命中）在共用层的落地检查。
/// 三段文本各带一个段号标记，所以「还在同一段」是可判定的，而不是靠感觉像。
#[gpui_kit::test]
fn the_progress_coordinate_survives_a_relayout(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let xhtml = (0..3)
        .map(|i| format!("<p>第{i}段{LONG}</p>"))
        .collect::<Vec<_>>()
        .join("");
    let base = layout_of(cx, &xhtml, &bare_prefs(), 600.0);
    let texts = base.line_texts();
    // 找第二段的第一行：它的文本一定含「第1段」，且行顶像素可反解成坐标。
    let target = (0..texts.len())
        .find(|&k| texts[k].starts_with("第1段"))
        .expect("三段都该排出来");
    let y = base.lines()[target].y;

    let offset = base.offset_at_y(y);
    assert!(offset > 0, "第二段不可能从第 0 个字开始");
    assert_eq!(
        base.line_at_offset(offset),
        target,
        "同一份排版里，像素 → 坐标 → 行号必须回到原行"
    );
    // 反向也成立：坐标 → 行顶像素，得到的就是那一行的行顶。
    assert_eq!(base.y_at_offset(offset), y);

    for (width, size) in [(300.0, 16.0), (900.0, 24.0), (420.0, 30.0)] {
        let mut prefs = bare_prefs();
        prefs.font_size = size;
        let moved = layout_of(cx, &xhtml, &prefs, width);
        let landed = moved.line_at_offset(offset);
        let landed_text = &moved.line_texts()[landed];
        assert!(
            landed_text.contains("第1段"),
            "宽 {width}、字号 {size}：坐标 {offset} 落到了「{landed_text}」，不在第二段里"
        );
    }
}

/// 排版还没做过时这套换算不能给出假坐标：一个全新的 `Document` 没有行盒，
/// 任何偏移都只能映射到「章首」，否则恢复进度会把人送到看不见的地方。
#[test]
fn an_unlaid_out_document_maps_everything_to_the_chapter_start() {
    let document = Document::new(
        format!("<p>{LONG}</p>"),
        "",
        prefs(),
        Arc::new(ChapterImages::empty()),
        // 这些用例钉的是排版与坐标，不是字体：空别名表 = 没有内嵌字体，
        // 与没有这个功能时逐字节同一条路径。
        Arc::new(BookFonts::empty()),
    );
    assert_eq!(document.line_count(), 0, "没排过版就没有行盒");
    assert_eq!(document.offset_at_y(4000.0), 0);
    assert_eq!(document.y_at_offset(1234), 0.0);
    assert_eq!(document.line_at_offset(1234), 0);
    // 坐标表不依赖排版，构造就有 —— 批注可以在排版之前建锚点。
    assert_eq!(document.text_map().text(), LONG);
}

/// **接线的证据**：块级缓存在应用侧的调用路径上真的活着，不是只在 core 的单测里活着。
///
/// 增量本身由 core 的 `block_cache_only_relayouts_the_changed_block` 钉住；这条钉的是
/// 生产里最容易重现的那一幕 —— 读者的字号滑条**拉上去又拉回来**：第二趟回来时每个块的
/// 子树样式哈希与第一趟逐个相同，键的「复用范围」（章文本 / 图片 / 字体表 / 度量口径）
/// 也没变，于是一块都不必重排，只把缓存里的行盒搬出来。
///
/// 为什么不是「把同一个偏好再设一遍」：那算出的是**同一把键**，压根不会派活
/// （见 `setting_the_same_preferences_relayouts_nothing`），拿它测复用等于测了个空转。
#[gpui_kit::test]
fn a_font_size_round_trip_reuses_every_block(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = window(cx, 600.0, 900.0);
    let plain = bare_prefs();
    let mut big = plain.clone();
    big.font_size += 4.0;
    let mut document = Document::new(
        format!(
            "<p>{}</p><p>{}</p><p>{}</p>",
            LONG.repeat(3),
            LONG.repeat(3),
            LONG.repeat(3)
        ),
        "",
        plain.clone(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );

    let mut typeset = |document: &mut Document, prefs: &Reader| {
        document.set_prefs(prefs.clone());
        cx.update_window(handle, |_, win, cx| document.ensure_layout(win, cx, 0.0))
            .expect("排版应当成功");
    };

    typeset(&mut document, &plain);
    let first_lines = document.lines().len();
    let first_height = document.height();
    assert_eq!(
        document.blocks.hits, 0,
        "首排没有可复用的东西可言：命中只可能出现在之后几趟"
    );
    let misses_after_first = document.blocks.misses;
    assert!(
        misses_after_first >= 3,
        "三个段落都该实排一次，实测 {misses_after_first}"
    );

    // 拉上去：字号是继承属性，每个块的样式哈希都变，于是全部重排。
    typeset(&mut document, &big);
    assert!(
        document.blocks.misses > misses_after_first,
        "换了字号必须重排（{misses_after_first} → {}）",
        document.blocks.misses
    );
    let misses_after_big = document.blocks.misses;
    // 「真重排了」的证据取**章高**而不是行数：这三段在两个字号下折出来的行数恰好相同
    // （每段 78 个字，行宽 600px 下 18px 字号一行 33 字、22px 一行 27 字，向上取整都是
    // 三行三段），拿行数当证据会在换个文本长度时莫名翻红。
    assert_ne!(
        document.height(),
        first_height,
        "字号变了整章高必须变，否则这一趟是空转"
    );

    // 拉回来：样式哈希与第一趟逐个相同 → 一块都不重排，命中往上走。
    let hits0 = document.blocks.hits;
    typeset(&mut document, &plain);
    assert_eq!(
        document.blocks.misses, misses_after_big,
        "回到原字号还重排，说明块缓存被整清了（复用范围那一格算错）或键漏了东西"
    );
    assert!(
        document.blocks.hits > hits0,
        "第二趟回来必须复用块（{hits0} → {}）",
        document.blocks.hits
    );
    assert_eq!(
        document.lines().len(),
        first_lines,
        "复用出来的行盒必须与第一趟全量重排一致"
    );
}

/// 键的唯一构造点顺带买到的一件事：**输入没变就不派活**。
///
/// 旧写法靠 `set_prefs` 手动 `generation += 1`，于是「把同一个偏好再设一遍」也会重排
/// 整章 —— 而设置面板里每一个滑条松手都算一次。现在键是从输入现算的，再设一遍得到
/// 同一把键，`request_typeset` 一进门就返回：块缓存的计数一格都不该动。
#[gpui_kit::test]
fn setting_the_same_preferences_relayouts_nothing(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = window(cx, 600.0, 900.0);
    let prefs = bare_prefs();
    let mut document = Document::new(
        format!("<p>{}</p><p>{}</p>", LONG.repeat(3), LONG.repeat(3)),
        "",
        prefs.clone(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );

    cx.update_window(handle, |_, win, cx| document.ensure_layout(win, cx, 0.0))
        .expect("第一次排版应当成功");
    let (hits0, misses0) = (document.blocks.hits, document.blocks.misses);
    assert!(misses0 > 0, "首排要真排过，否则下面那条「不动」是空转");

    // 再设一遍同一个偏好：算出的是同一把键。
    document.set_prefs(prefs);
    cx.update_window(handle, |_, win, cx| document.ensure_layout(win, cx, 0.0))
        .expect("第二次调用应当成功");
    assert_eq!(
        (document.blocks.hits, document.blocks.misses),
        (hits0, misses0),
        "同一把键不该再派活：块计数一格都不许动"
    );
    assert!(document.is_current(), "没派活是因为已经是想要的那一份了");
}

/// 接线验收：**改一次读者偏好，重新解析的章数必须为 0**
/// （`plans/阅读管线.md` §十二那条计数断言，也是 §十三第 2 步「接 LayoutKey + 章产物缓存」的验收）。
///
/// 用计数而不是计时：计时在这台机器上被调度噪声牵着走（`plans/layout-optimization.md` §5.3
/// 实测 ±20%），而「解析了几次」是一个确定的整数。这条**能红**：把 `typeset_job` 里
/// 「产物的内容哈希与这趟取到的正文对得上就复用」那一支退回成无条件 `ChapterArtifacts::build`，
/// 最后那条断言立刻翻红。
#[gpui_kit::test]
fn a_preference_change_reparses_nothing(cx: &mut TestAppContext) {
    use folio_core::infra::metrics::{Metric, take_local};
    cx.update(gpui_kit::init);
    let handle = window(cx, 600.0, 900.0);
    let plain = bare_prefs();
    let mut big = plain.clone();
    big.font_size += 4.0;
    // 数之前先归零：这一格在取章 / 校验 XHTML 那一路也会被加，而 `take_local` 是线程局部，
    // 别的测试在本线程上做过什么都数不进来。
    take_local(Metric::DomParses);

    let mut document = Document::new(
        format!("<p>{}</p><p>{}</p>", LONG.repeat(2), LONG.repeat(2)),
        "",
        plain.clone(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    cx.update_window(handle, |_, win, cx| document.ensure_layout(win, cx, 0.0))
        .expect("第一次排版应当成功");
    let parsed = take_local(Metric::DomParses);
    assert!(
        !document.lines().is_empty(),
        "第一段都没排出来，后面那条 0 就是空的"
    );
    assert!(
        parsed >= 1,
        "首排至少解析一次，否则「第二趟为 0」是零比零的空断言"
    );

    document.set_prefs(big);
    assert!(
        !document.is_current(),
        "刚改完偏好、还没重算键，就不许声称排版跟上了 —— 恢复进度与批注回显等的就是这一句"
    );
    cx.update_window(handle, |_, win, cx| document.ensure_layout(win, cx, 0.0))
        .expect("第二次排版应当成功");
    assert!(
        document.is_current(),
        "第二趟排版要真上过屏，否则这条测的是没干活"
    );
    assert!(
        !document.lines().is_empty(),
        "换了字号还得有行盒：这里确实重排了"
    );
    assert_eq!(
        take_local(Metric::DomParses),
        0,
        "改一次字号重解析了一章：产物没按内容哈希复用"
    );

    // 换章就必须重解析 —— 反面不成立说明复用条件写反了（拿上一章的 DOM 排这一章）。
    document.set_chapter(ChapterSource::text(format!("<p>{}</p>", LONG.repeat(4))));
    cx.update_window(handle, |_, win, cx| document.ensure_layout(win, cx, 0.0))
        .expect("换章排版应当成功");
    assert_eq!(
        take_local(Metric::DomParses),
        1,
        "换章必须恰好重解析一次：新章的 DOM 只能新建"
    );
}

/// 接线验收（`plans/阅读管线.md` §十三第 2 步）：排版键**一处构造**，
/// 同一代内问两次得到同一把键，而每类输入只动它该动的那几格。
///
/// 这条钉的是两种相反的毛病各一半：
/// - 「该换的没换」→ 屏幕上留着上一代的行盒（改字号不动、换图不动）；
/// - 「不该换的换了」→ 缓存白清，增量排版退化成整章重排（改字号把复用范围也推了）。
/// 复用范围那一格（`context_rev`）是 [`BlockCache`](folio_core::layout::BlockCache)
/// 唯一依据，所以它单独断言：章文本、图片索引、字体表、度量口径变了才动。
#[gpui_kit::test]
fn the_layout_key_is_composed_in_one_place(cx: &mut TestAppContext) {
    // 这一条纯算键，不开窗口：排版键是输入的函数，与度量、宿主都无关。
    let _ = cx;
    let plain = bare_prefs();
    let mut bigger = plain.clone();
    bigger.font_size += 4.0;
    let source = "<p>键的唯一构造点</p>".to_string();
    let document = Document::new(
        source.clone(),
        "p { margin: 0 }",
        plain.clone(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );

    assert_eq!(
        document.layout_key(600.0, 1, None),
        document.layout_key(600.0, 1, None),
        "同一代内问两次必须给出同一把键"
    );
    assert_eq!(
        document.layout_key(600.4, 1, None).width,
        600,
        "栏宽量化到 1px"
    );

    // 改字号：换键，但不换复用范围。
    let (a, b) = (
        document.layout_key(600.0, 1, None),
        document.layout_key(600.0, 2, None),
    );
    assert_ne!(a, b, "一页两栏与单栏不是同一份产物（切片要区分）");
    assert_eq!(
        a.context_rev(),
        b.context_rev(),
        "栏数变了 DOM 与字体表都没变，缓存不该整清"
    );

    let mut document = Document::new(
        source,
        "",
        plain,
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    let before = document.layout_key(600.0, 1, None);
    document.set_prefs(bigger);
    let after = document.layout_key(600.0, 1, None);
    assert_ne!(before, after, "改字号必须换键");
    assert_eq!(
        before.context_rev(),
        after.context_rev(),
        "改字号不许清块缓存 —— 只许让样式变了的块自己未命中"
    );

    // 换图：固有尺寸是排版输入，复用范围必须换。
    document.set_images(Arc::new(ChapterImages::empty()));
    assert_ne!(
        before.context_rev(),
        document.layout_key(600.0, 1, None).context_rev(),
        "换了图片索引还复用旧块，行高就是按上一批图片算的"
    );

    // 换章：章文本进了复用范围（`NodeId` 只在同一份 DOM 里作数）。
    document.set_chapter(ChapterSource::text("<p>下一章</p>"));
    assert_ne!(
        before.chapter,
        document.layout_key(600.0, 1, None).chapter,
        "换章必须换章身份（块缓存的复用范围跟着清）"
    );

    // 作者 CSS：换键（层叠要重算），但不换复用范围（DOM 还是那份 DOM）。
    let with_css = Document::new(
        "<p>键的唯一构造点</p>".to_string(),
        "p { margin: 0 }",
        bare_prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    assert_ne!(
        document.layout_key(600.0, 1, None).author_css_hash,
        with_css.layout_key(600.0, 1, None).author_css_hash,
        "作者样式进键"
    );
}

/// 字体表版本进键：注册过内嵌字体的那本书，与一本没字体的书不能共用同一格版本。
///
/// 这一格是块级缓存接线前那个「键里不含设备字体表」的缺口。它数的是**注册事件**，
/// 所以两张先后建好、中间没人注册过任何东西的空表，版本相同是对的（字体表确实没变）。
#[gpui_kit::test]
fn the_font_revision_of_a_book_that_registered_faces_moves_the_key() {
    let plain = bare_prefs();
    let make = |faces: Arc<BookFonts>| {
        Document::new(
            "<p>字体版本</p>".to_string(),
            "",
            plain.clone(),
            Arc::new(ChapterImages::empty()),
            faces,
        )
        .layout_key(600.0, 1, None)
    };
    let empty_a = make(Arc::new(BookFonts::empty()));
    let empty_b = make(Arc::new(BookFonts::empty()));
    assert_eq!(
        empty_a.fonts_rev, empty_b.fonts_rev,
        "中间没注册过任何字体，版本就不该动"
    );

    // 手工推进版本：`for_document` 里那一次真实注册要开窗口拿文本系统，
    // 这里只钉「版本不同 → 复用范围不同」这半条，那半条在 `book_fonts` 的注册处。
    let mut stamped = empty_a;
    stamped.fonts_rev += 1;
    assert_ne!(
        empty_a.context_rev(),
        stamped.context_rev(),
        "字体表变了就必须让块缓存整清：旧行宽是新字体算不出来的"
    );
}

// ---- 每项偏好有没有消费者 ----

/// 一项读者偏好的**归属**。这张分类表是 `plans/基础设施.md` §2.4 那条规矩的落地形式：
/// 「只有字段上的标注能一眼看出哪个是设计、哪个是漏」。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PrefClass {
    /// 正文排版输入：改它，整章几何必须真的变（下面就是这么断言的）。
    Layout,
    /// 走到根样式为止，再由度量层拿去用。落点是 `Document::root_base` 里那一格。
    RootStyle,
    /// 消费它的代码在别处，且那里已有一条测试钉住（写清楚是哪一条）。
    PinnedElsewhere,
    /// 目录那一屏的字段：不许动正文版式。
    Toc,
}

/// 「动一下某项偏好」的写法 + 它的归属。名字必须与 `Reader` 的字段名一模一样。
type PrefTweak = (&'static str, fn(&mut Reader), PrefClass);

const PREFS: [PrefTweak; 12] = [
    ("font_size", |p| p.font_size += 2.0, PrefClass::Layout),
    ("line_height", |p| p.line_height += 0.3, PrefClass::Layout),
    (
        "paragraph_gap_em",
        |p| p.paragraph_gap_em += 0.25,
        PrefClass::Layout,
    ),
    ("indent_em", |p| p.indent_em += 1.0, PrefClass::Layout),
    ("margin_x", |p| p.margin_x += 60.0, PrefClass::Layout),
    ("max_measure", |p| p.max_measure = 300.0, PrefClass::Layout),
    (
        "font_family",
        |p| p.font_family = "serif".into(),
        PrefClass::RootStyle,
    ),
    (
        "mono_font_family",
        |p| p.mono_font_family = "mono2".into(),
        PrefClass::PinnedElsewhere,
    ),
    // 2026-10-16 起「两端对齐」真接上了：`Justify` 不再落 `_ => 0.0`，而是按行的剩余
    // 宽度把余量摊到行内的隙上（`layout::collect::justify_plan`）。分类跟着从
    // `NotWired` 提到 `Layout`。
    ("justify", |p| p.justify = !p.justify, PrefClass::Layout),
    (
        "toc_density",
        |p| p.toc_density = folio_core::infra::config::TocDensity::Compact,
        PrefClass::Toc,
    ),
    (
        "toc_max_title_lines",
        |p| p.toc_max_title_lines += 1,
        PrefClass::Toc,
    ),
    (
        "toc_dim_read",
        |p| p.toc_dim_read = !p.toc_dim_read,
        PrefClass::Toc,
    ),
];

/// 每一项偏好都归了类，一类都不许漏。
///
/// 两道闸：解构 `Reader` 的**全部**字段（加字段而没在这里补名字，那一行直接编译不过），
/// 以及逐个名字对一遍 [`PREFS`]（补了解构却没分类，运行期就红）。
#[test]
fn every_preference_is_classified() {
    let Reader {
        font_size,
        line_height,
        paragraph_gap_em,
        indent_em,
        max_measure,
        margin_x,
        justify,
        font_family,
        mono_font_family,
        toc_density,
        toc_max_title_lines,
        toc_dim_read,
    } = Reader::default();
    // 解构出来的值本身没要用：这一份的存在只是让「新增偏好字段」变成编译错误。
    let _ = (
        font_size,
        line_height,
        paragraph_gap_em,
        indent_em,
        max_measure,
        margin_x,
        justify,
        font_family,
        mono_font_family,
        toc_density,
        toc_max_title_lines,
        toc_dim_read,
    );

    const NAMES: [&str; 12] = [
        "font_size",
        "line_height",
        "paragraph_gap_em",
        "indent_em",
        "max_measure",
        "margin_x",
        "justify",
        "font_family",
        "mono_font_family",
        "toc_density",
        "toc_max_title_lines",
        "toc_dim_read",
    ];
    for name in NAMES {
        assert!(
            PREFS.iter().any(|(listed, ..)| *listed == name),
            "偏好 {name} 没在分类表里：它改完该动哪一层？"
        );
    }
    assert_eq!(PREFS.len(), NAMES.len(), "分类表与解构清单对不上长度");
}

/// **改了这项偏好，版式必须真的变** —— core 那条
/// `every_reader_preference_changes_the_key` 的另一半。
///
/// 那一条只钉「会不会重排」，钉不住「重排完还是不是原来那份」。差别是实测出来的：
/// `paragraph_gap_em` 曾经键换了、整章重排、行盒纹丝不动（`plans/基础设施.md` §2.4）。
/// 断言取**整章高 + 每一行的 (x, y, width)**，不是行数：只看行数会在「改了字号却恰好
/// 没多出一行」时假绿。
///
/// 字族这一类不在这里断言几何：那要求这台机器真装着两个能解析出不同宽度的家族，
/// 换一台机器就变红，而红的原因与代码无关。
#[gpui_kit::test]
fn a_layout_preference_change_moves_the_geometry(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    // 三段、每段约 93 字：段间有段距，段内有多行，缩进与折行都落在几何上。
    let xhtml = (0..3)
        .map(|i| format!("<p>第{i}段{}。</p>", LONG.repeat(3)))
        .collect::<Vec<_>>()
        .join("");
    let base = bare_prefs();
    let mut signature = |prefs: &Reader| {
        let document = layout_of(cx, &xhtml, prefs, 600.0);
        (
            document.height(),
            document
                .lines()
                .iter()
                // `ink_width` 必须在这份签名里：两端对齐**不改** `x` / `width`，只把余量
                // 摊到行内（于是墨迹铺到 `ink_width`）。只看前三格，「两端对齐」这类改动
                // 会假绿 —— 这正是接通前它明明落 `_ => 0.0` 却测不出来的原因。
                .map(|line| (line.x, line.y, line.width, line.ink_width()))
                .collect::<Vec<_>>(),
        )
    };
    let at_base = signature(&base);
    assert!(
        at_base.1.len() >= 6,
        "三段每段至少两行都没排出来，后面那些「变了 / 没变」全是空断言：实测 {} 行",
        at_base.1.len()
    );

    for (name, tweak, class) in PREFS {
        let mut prefs = base.clone();
        tweak(&mut prefs);
        let changed = signature(&prefs) != at_base;
        match class {
            PrefClass::Layout => assert!(
                changed,
                "偏好 {name} 改了版式没动：面板说有、后端没有，重排是白排的"
            ),
            PrefClass::Toc => assert!(!changed, "偏好 {name} 是目录那一屏的字段，不该动到正文版式"),
            PrefClass::RootStyle => {
                let (before, after) = (Document::root_base(&base), Document::root_base(&prefs));
                assert_ne!(
                    before.font_family, after.font_family,
                    "偏好 {name} 该走进根样式，实际没走"
                );
            }
            // 等宽族名由 `measure.rs` 的 `monospace_generic_uses_the_mono_preference` 钉住：
            // 它的落点是「泛族名换成哪个家族」，在度量映射里，不在这条断言的视野里。
            PrefClass::PinnedElsewhere => {}
        }
    }
}

// ---- 脚注：条引用 → 注文（`Document::footnote_note_for_node` 一族） ----

/// `蛇结` 那种形状：标记是一张「注」图（外面套 `<a epub:type="noteref">`），
/// 注文在同一个文件的 `<aside epub:type="footnote">` 里。目标元素的文本优先于 `alt`。
#[test]
fn a_noteref_marker_returns_the_bodys_text_before_the_marker_alt() {
    let source = "<p>正文开头\
        <a epub:type=\"noteref\" href=\"#fn1\">\
        <img src=\"p.png\" alt=\"alt 里的兜底注文\" class=\"epub-footnote\"/></a>。</p>\
        <aside epub:type=\"footnote\" id=\"fn1\"><ol class=\"duokan-footnote-content\">\
        <li class=\"duokan-footnote-item\">真正的注文</li></ol></aside>";
    let doc = Document::new(
        source.to_string(),
        "",
        prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    let dom = doc.dom();
    let img = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("img"))
        .map(|(id, _)| id)
        .expect("该有那张「注」图");
    assert_eq!(
        doc.footnote_note_for_node(img).as_deref(),
        Some("真正的注文"),
        "目标 <aside> 的子树文本该胜出（alt 只是兜底）"
    );
}

/// 正文是图片、标记图的 `alt` 只写了 `"alt"`：不能把占位符当注文显示。
#[test]
fn a_placeholder_alt_is_not_mistaken_for_footnote_text() {
    let source = "<p>正文开头\
        <a epub:type=\"noteref\" href=\"notes.xhtml#fn1\">\
        <img src=\"p.png\" alt=\"alt\"/></a>。</p>";
    let doc = Document::new(
        source.to_string(),
        "",
        prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    let dom = doc.dom();
    let img = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("img"))
        .map(|(id, _)| id)
        .expect("该有那张「注」图");
    assert_eq!(
        doc.footnote_note_for_node(img),
        None,
        "`alt=\"alt\"` 是占位符，不是注文；宁可回落到普通链接跳转"
    );
}

/// 目标在**别的文件**里（本章解不出正文）时，退回标记图上的 `zy-footnote` / `alt`。
#[test]
fn a_cross_file_noteref_falls_back_to_the_marker_attributes() {
    let source = "<p>正文开头\
        <a epub:type=\"noteref\" href=\"notes.xhtml#fn1\">\
        <img src=\"p.png\" zy-footnote=\"跨文件的注文\"/></a>。</p>";
    let doc = Document::new(
        source.to_string(),
        "",
        prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    let dom = doc.dom();
    let img = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("img"))
        .map(|(id, _)| id)
        .expect("该有那张「注」图");
    assert_eq!(
        doc.footnote_note_for_node(img).as_deref(),
        Some("跨文件的注文"),
        "跨文件目标在本地取不到，该退回标记自己的属性"
    );
}

/// 一张**不在** `<a>` 里的普通插图不该被当成脚注。
#[test]
fn a_plain_illustration_is_not_a_footnote() {
    let source = "<p>正文</p><p><img src=\"p.png\" alt=\"插图说明\"/></p>";
    let doc = Document::new(
        source.to_string(),
        "",
        prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    let dom = doc.dom();
    let img = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("img"))
        .map(|(id, _)| id)
        .expect("该有那张图");
    assert_eq!(
        doc.footnote_note_for_node(img),
        None,
        "没挂在 `<a>` 上就不是脚注标记"
    );
}

/// 《罗生门》那种形状：标记是 `<a href="part0004.xhtml#m1"><sup>[1]</sup></a>`（带文件名
/// 的**同章**链接），注文是章末尾的 `<p class="note">`，`id` 挂在里面一个空 `<a>` 上。
/// 只认纯 `#id` 会把它的脚注全漏掉。
#[test]
fn a_relative_noteref_into_a_note_paragraph_yields_the_note() {
    let source = "<p>正文<a href=\"part0004.xhtml#m1\"><sup>[1]</sup></a>继续。</p>\
        <p class=\"note\"><a id=\"m1\"></a> 这是注文的内容。</p>";
    let doc = Document::new(
        source.to_string(),
        "",
        prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    let dom = doc.dom();
    let marker = dom
        .elements()
        .find(|(id, _)| dom.attr(*id, "href") == Some("part0004.xhtml#m1"))
        .map(|(id, _)| id)
        .expect("该有那条标记链接");
    let note = doc
        .footnote_note_for_node(marker)
        .expect("同章 note 段落该被认成脚注");
    assert!(
        note.contains("这是注文的内容。"),
        "注文该取到最近那个有字的祖先块：{note:?}"
    );
}

/// 同一种 `文件.xhtml#id` 的写法，指向的却是标题（目录链接），**不能**当成脚注 ——
/// 不然点正文目录里的每一条都会弹注文、跳不过去。
#[test]
fn a_relative_link_into_a_heading_is_not_a_footnote() {
    let source = "<p>见<a href=\"c.xhtml#a005\">罗生门</a>。</p>\
        <h1 id=\"a005\" class=\"zw-text1\">罗生门</h1>";
    let doc = Document::new(
        source.to_string(),
        "",
        prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    let dom = doc.dom();
    let link = dom
        .elements()
        .find(|(id, _)| dom.attr(*id, "href") == Some("c.xhtml#a005"))
        .map(|(id, _)| id)
        .expect("该有那条链接");
    assert_eq!(
        doc.footnote_note_for_node(link),
        None,
        "指向标题的链接是普通目录链接，不是脚注"
    );
}

/// 正文**由作业按位置取**的那一路：键声称的章与作业取回的章必须是同一章。
///
/// 这一格是 2026-10-15 把取文本塞进排版作业时**新引入的唯一坏法**。
/// `LayoutKey::chapter` 不再含正文，于是「换章」这件事在键这一侧只剩
/// `Document::set_chapter` 里那一句 `self.chapter = source.identity()` 在管。
/// 漏掉它：键不换 → `request_typeset` 在「已应用的这把键就是想要的」那一支直接返回 →
/// 屏幕上继续是上一章的行盒，而界面以为自己已经在新章上 —— 页眉、进度、落库全跟着错，
/// 并且没有任何一处会报错。把那一句删掉，这一条当场红。
///
/// 另一半钉的是「作业真的去取了」：`Document::fetching` 构造时界面没有正文，
/// 第一趟排版必须带回**按章号取到的那一段**，而不是构造时那份空章。
#[gpui_kit::test]
fn the_key_and_the_fetched_chapter_are_the_same_chapter(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    // 两章正文各带一个标记，排回来认得出是哪一章。`Chapter::inline` 让这份文档
    // 不需要容器也能回答 `chapter_source(index)` —— 被测的是「按位置取」这条来路，
    // 不是解压。
    let flow = FlowDoc {
        chapters: vec![
            Chapter::inline("第一章", format!("<p>{LONG}（甲）</p>")),
            Chapter::inline("第二章", format!("<p>{LONG}（乙）</p>")),
        ],
        ..Default::default()
    };
    let doc: Arc<dyn OpenedDocument> = Arc::from(folio_core::content::document::from_content(
        DocMeta {
            title: "两章".into(),
            authors: Vec::new(),
            format: Format::Epub,
            size: 0,
            ..DocMeta::default()
        },
        Content::Reflow(flow),
    ));

    let handle = window(cx, 1000.0, 900.0);
    let mut document = Document::fetching(
        doc.clone(),
        0,
        "",
        bare_prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    cx.update_window(handle, |_, win, cx| {
        document.ensure_layout(win, cx, 0.0);
    })
    .expect("窗口应当可排版");
    let first = document.line_texts().concat();
    assert!(
        first.contains('甲'),
        "第 0 章的来路必须排回第 0 章的正文，实际是：{first}"
    );
    assert_eq!(
        document.layout_key(600.0, 1, None).chapter,
        0,
        "按位置取的那一路，键里的身份就是章号（内容哈希活在产物上，不在键里）"
    );

    // 换到第 1 章：身份与来路必须一起换。
    document.set_chapter(ChapterSource::fetch(doc.clone(), 1));
    cx.update_window(handle, |_, win, cx| {
        document.ensure_layout(win, cx, 0.0);
    })
    .expect("窗口应当可排版");
    let second = document.line_texts().concat();
    assert_eq!(
        document.layout_key(600.0, 1, None).chapter,
        1,
        "换章之后键里的身份要跟着换成第 1 章"
    );
    assert!(
        second.contains('乙') && !second.contains('甲'),
        "换章之后排回来的必须是第 1 章的正文，实际是：{second}"
    );
}

/// 界面问键**不许重算输入的哈希**。
///
/// 这是 `layout::tests::cache::the_layout_inputs_hash_once` 的**对面那半**：那条钉的是
/// `LayoutKey::compose` 自己不算，这条钉的是调用方不许每次都现造一份 `LayoutInputs`。
/// 真实的退化形状是后者 —— 有人把 `layout_key()` 写回成
/// `compose(LayoutInputs::compute(...), ...)`，每帧、链上每个章各一趟
/// 「整本书的 CSS 走 SipHash」+「整份 `Reader` 用 serde_json 序列化一遍」。
/// 把 `Document::layout_key` 改成那样，这一条当场红（增量从 0 变成问键的次数）。
#[gpui_kit::test]
fn asking_for_the_key_never_rehashes_the_inputs(cx: &mut TestAppContext) {
    use folio_core::infra::metrics::{Metric, take_local};
    let _ = cx;

    let mut document = Document::new(
        "<p>问键</p>".to_string(),
        "p { margin: 0 }",
        bare_prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    // 构造那一趟算过一次的账先结掉：这条门问的是**之后**每帧还付不付。
    take_local(Metric::LayoutInputHashes);

    for _ in 0..300 {
        let _ = document.layout_key(600.0, 1, None);
    }
    assert_eq!(
        take_local(Metric::LayoutInputHashes),
        0,
        "问键是每帧每个进链的章各一次的动作，一次哈希都不许在这里发生"
    );

    // 而偏好真的换了，那一格必须刷新（省掉刷新与每帧重算一样糟，只是坏得看不出来）。
    let before = document.layout_key(600.0, 1, None);
    let mut bigger = bare_prefs();
    bigger.font_size += 4.0;
    document.set_prefs(bigger);
    assert_eq!(
        take_local(Metric::LayoutInputHashes),
        1,
        "`set_prefs` 要重算一次，且只此一次"
    );
    assert_ne!(
        before.prefs_hash,
        document.layout_key(600.0, 1, None).prefs_hash,
        "换了字号键里的偏好那一格必须跟着变"
    );
}

/// 装饰短表**只随排版换代重折**，每帧问键都不重折。
///
/// 这是 §十一 第 12 条的门。改之前 `collect_decorations` 每帧、链上每个进链的章各把
/// **整章的块表**走一遍才筛出那几条装饰（一份语料一章 160 块，长章 600+）；现在整章
/// 只在 `apply_typeset` 里走一次（`Document::decorated`），每帧扫的是那张短表。
///
/// 前后两半各钉一头：反复问键必须一次都不重折（**这一头是这次的收益**，把
/// `Document::apply_typeset` 里那行改成每帧现折就当场红）；而换了字号必须重折
/// （**这一头防的是缓存化本身**，删掉重折就会停在旧表上，画出的底色与新行盒不配套）。
#[gpui_kit::test]
fn scrolling_never_rebuilds_the_decor_table(cx: &mut TestAppContext) {
    use folio_core::infra::metrics::{Metric, take_local};
    cx.update(gpui_kit::init);
    let handle = window(cx, 600.0, 900.0);
    let mut document = Document::new(
        format!("<div><p>{LONG}</p><p>{LONG}</p></div>"),
        "div { background-color: #C1CCC0 }",
        prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    let typeset = |document: &mut Document, cx: &mut TestAppContext| {
        cx.update_window(handle, |_, win, cx| {
            document.ensure_layout(win, cx, 0.0);
        })
        .expect("窗口应当可排版");
    };
    typeset(&mut document, cx);
    assert!(
        !document.decor_seeds().is_empty(),
        "这一章的书自己写了底色，短表里该有条目"
    );
    // 第一代排完的那笔账结掉：重折发生在排版换代时，那是合法的。
    take_local(Metric::DecorRebuilds);

    for _ in 0..50 {
        typeset(&mut document, cx);
    }
    assert_eq!(
        take_local(Metric::DecorRebuilds),
        0,
        "排版没换代就不许重折整章块表 —— 那正是从前每帧付的那一趟"
    );

    let mut bigger = prefs();
    bigger.font_size += 4.0;
    document.set_prefs(bigger);
    typeset(&mut document, cx);
    assert!(
        take_local(Metric::DecorRebuilds) > 0,
        "改字号重排了新一代，装饰表必须跟着重折，否则底色还是按旧行盒算的"
    );
}

/// 行索引**只在有人问时折、只在排版换代时重折**。
///
/// 折它要走一遍整章的行、每行逐片段问一次文本坐标，所以两头都要钉：没划过字、没开
/// 选区、没恢复落点的那本书一分钱不许付（前半）；问过之后两条查询共用同一张表，
/// 不是每帧重折（后半）。接线之前这里是另一个形状 —— **每条批注 × 每帧 × 每个进链的
/// 章**各把整章的行走一遍（`plans/阅读管线.md` §十一 第 9 条，实测一章 25 条批注
/// 每帧 +1.7–2.2 ms）。
#[gpui_kit::test]
fn the_line_index_waits_for_its_first_query(cx: &mut TestAppContext) {
    use std::rc::Rc;
    cx.update(gpui_kit::init);
    let handle = window(cx, 600.0, 900.0);
    let mut document = Document::new(
        format!("<p>{}</p>", LONG.repeat(6)),
        "",
        prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    cx.update_window(handle, |_, win, cx| {
        document.ensure_layout(win, cx, 0.0);
    })
    .expect("窗口应当可排版");
    assert!(
        document.char_lines.borrow().is_none(),
        "排完版但没人问过：整章的行一趟都不该折"
    );

    // 第一条查询：批注底色每帧走的那一条（带内矩形）。
    cx.update_window(handle, |_, win, app| {
        document.selection_rects_in_band(win, app, 4..16, 0.0..f32::MAX)
    })
    .expect("窗口应当可求矩形");
    assert!(
        document.char_lines.borrow().is_some(),
        "求过矩形却没有折索引：`selection_rects` 没走窄化那条路，每帧还在扫整章"
    );
    let built = document.char_lines();
    assert!(
        Rc::ptr_eq(&built, &document.char_lines()),
        "第二次问又折了一张新表：这就退回「每问一次走一遍整章」了"
    );

    // 换代：行号与字符区间这两个身份都不再作数，旧表必须掉。
    let mut bigger = prefs();
    bigger.font_size += 4.0;
    document.set_prefs(bigger);
    assert!(
        document.char_lines.borrow().is_none(),
        "排版换代后索引还留着：那是拿旧一代的坐标去答新问句"
    );
    cx.update_window(handle, |_, win, cx| {
        document.ensure_layout(win, cx, 0.0);
    })
    .expect("第二次排版应当成功");
    // 第二条查询：字符偏移 → 行（恢复落点那一跳）。
    let _ = document.line_at_offset(8);
    let rebuilt = document.char_lines();
    assert!(
        !Rc::ptr_eq(&built, &rebuilt),
        "换代之后还在用旧那张表：行号在新排版里指向别处"
    );
}

/// 接了索引的三条查询，答案必须与「整章扫」**逐条相同**。
///
/// 核心那一层已经按每个偏移、每条短区间比过（`content::anchor::tests` 与
/// `content::hit::tests`），这里钉的是**接线**：`Document` 递出去的行号段确实是从
/// 它自己这一代排版折出来的那一份。把 `selection_rects` 里的 `range.clone()` 写成
/// `0..` 那种张冠李戴，只有这条能抓住。
#[gpui_kit::test]
fn the_wired_queries_answer_like_the_whole_chapter_scan(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = window(cx, 600.0, 900.0);
    let mut document = Document::new(
        format!("<p>{}</p>", LONG.repeat(6)),
        "",
        prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    cx.update_window(handle, |_, win, cx| {
        document.ensure_layout(win, cx, 0.0);
    })
    .expect("窗口应当可排版");
    let total = document.text_map().chars();
    assert!(
        document.lines().len() > 3 && total > 40,
        "fixture 要能跨行才有得比：行数 {}、字数 {total}",
        document.lines().len()
    );

    // 偏移 → 行、偏移 → 行顶：与整章扫逐偏移对齐。
    for offset in [0, 1, 7, 19, 40, total - 1, total] {
        let scanned = folio_core::content::anchor::line_at_offset(
            document.layout(),
            document.dom(),
            document.text_map(),
            offset,
        );
        assert_eq!(
            document.line_at_offset(offset),
            scanned,
            "偏移 {offset} 上接了索引反而与整章扣分家"
        );
        assert_eq!(
            document.y_at_offset(offset),
            document.layout().lines[scanned].y,
            "偏移 {offset} 的行顶与该行的 y 不符"
        );
    }

    // 区间 → 矩形：逐条比，包括跨行、越界与空区间。
    for range in [0..1, 5..40, total..total + 50, Range { start: 30, end: 4 }] {
        let (tight, wide) = cx
            .update_window(handle, |_, win, app| {
                let measure = document.measure(win, app);
                let wide = folio_core::content::hit::rects_for_range(
                    document.layout(),
                    document.dom(),
                    document.text_map(),
                    &measure,
                    range.clone(),
                );
                let tight = document.selection_rects(win, app, range.clone());
                (tight, wide)
            })
            .expect("窗口应当可求矩形");
        assert_eq!(
            tight,
            wide,
            "区间 {range:?}：走索引那条路换了答案（窄化后的条数 {}，整章扫 {}）",
            tight.len(),
            wide.len()
        );
    }
}

/// 交集的两条不等式要一条条钉住：哪一头夹错了都是「少一块底色」，而那种错在
/// 屏上看不到任何线索（矩形只是不见了）。
#[test]
fn narrowed_lines_intersect_without_dropping_a_line() {
    // 字符段整段在带内：不动。
    assert_eq!(narrowed_lines(Some(2..7), 0..10), 2..7);
    // 两头各伸出去一点：夹到带上，一行都不许丢。
    assert_eq!(narrowed_lines(Some(2..7), 4..10), 4..7);
    assert_eq!(narrowed_lines(Some(6..12), 0..9), 6..9);
    // 索引不可用（那一章的行不按时序单调）：只剩可见带那一头。
    assert_eq!(narrowed_lines(None, 4..9), 4..9);
    // 两不相交（这一条批注根本不在这屏上）：给空区间，一行都不走。
    // 空区间落在哪一头无所谓（它是拿来迭代的，不迭代就是不迭代），但取「起 = 两个起点的
    // 较大者」这一头的写法要与 `lines_in_band` 那句 `start..start` 同一条口径。
    assert_eq!(narrowed_lines(Some(0..3), 5..9), 5..5);
    assert_eq!(narrowed_lines(Some(7..9), 0..5), 7..7);
    // 空带与反向带：`lines_in_band` 那种情况给整区间，这里只验空的那种。
    assert_eq!(narrowed_lines(Some(2..7), 3..3), 3..3);
}

/// 带内那条查询，与「整章算完再按 y 滤」**逐条相同**。
///
/// 这条是 `selection_rects_in_band` 唯一的正确性凭据 —— `TestWindow` 不暴露绘制场景，
/// 「底色不见了」这种错在 headless 里没人看得见（同一件事在 §十四 那条 `#[ignore]`
/// 记录里已经栽过一次），所以判据只能是「窄化后的结果 == 没窄化再滤一遍的结果」。
#[gpui_kit::test]
fn the_banded_query_agrees_with_filtering_afterwards(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let handle = window(cx, 600.0, 900.0);
    let mut document = Document::new(
        format!(
            "<body>{}</body>",
            (0..20)
                .map(|_| format!("<p>{}</p>", LONG.repeat(3)))
                .collect::<String>()
        ),
        "",
        prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    cx.update_window(handle, |_, win, cx| {
        document.ensure_layout(win, cx, 0.0);
    })
    .expect("窗口应当可排版");
    let total = document.text_map().chars();
    let all_lines = document.layout().lines.len();
    assert!(
        all_lines > 10,
        "fixture 要长到一屏装不下才有得比：{all_lines} 行"
    );

    // 带：整章、屏顶几行、跨两条批注的中间一段、正好卡在行边界上、空带。
    let first_line = &document.layout().lines[0];
    let bands = [
        0.0f32..f32::MAX,
        first_line.y..first_line.y + first_line.height * 3.0,
        document.layout().lines[4].y..document.layout().lines[7].y + 0.5,
        // 正好压在第一行的底沿：那道 y 滤用的是 `<=`，这里要同一条口径。
        first_line.y + first_line.height..f32::MAX,
        100.0..100.0,
    ];
    let ranges = [
        0..total,
        10..total - 10,
        Range {
            start: total / 2,
            end: total / 2 + 8,
        },
        Range {
            start: total - 4,
            end: total + 40,
        },
    ];
    for band in bands {
        for range in ranges.clone() {
            let (banded, wide) = cx
                .update_window(handle, |_, win, app| {
                    (
                        document.selection_rects_in_band(win, app, range.clone(), band.clone()),
                        document.selection_rects(win, app, range.clone()),
                    )
                })
                .expect("窗口应当可求矩形");
            // 调用方那道 y 滤（`view::highlight_rects` 里原样留着的那一条）。
            let keeps = |rect: &folio_core::content::hit::SelectionRect| {
                !(rect.y + rect.height <= band.start || rect.y >= band.end)
            };
            let mut expect = wide.clone();
            expect.retain(keeps);
            // ① 该画的都在：漏一条就是屏上少一块底色，而那件事在 headless 里没人看得见。
            for rect in &expect {
                assert!(
                    banded.contains(rect),
                    "带 {band:?}、区间 {range:?}：带内窄化把这条矩形弄丢了 {rect:?}"
                );
            }
            // ② 没多画：窄化不许凭空造出整章扫里没有的矩形。
            for rect in &banded {
                assert!(
                    wide.contains(rect),
                    "带 {band:?}、区间 {range:?}：窄化凭空多出一条 {rect:?}"
                );
            }
            // ③ 过了那道 y 滤之后，两边**逐条**相同 —— 窄化只是把丢弃提前到行走之前。
            let mut got = banded.clone();
            got.retain(keeps);
            assert_eq!(
                got, expect,
                "带 {band:?}、区间 {range:?}：滤完之后与整章扫不同"
            );
        }
    }
}

/// 一帧走几行是这条优化的**量**：`Metric::RectLines`。
///
/// 四条臂各钉一头：整章区间不带窄化必须**走满**（证明这一格数的是真被走过的行，
/// 不是一句永远成立的「≤」）；同一条区间夹上可见带只许走带内那几行；带内一条八个字的
/// 短批注还得被**字符**那一头再夹一次（只靠可见带不算窄化完成）；而一条根本不在这一屏上
/// 的批注一行都不许走 —— 那正是「一章 25 条、屏上 3 条」这一帧的形状
/// （`plans/阅读管线.md` §十一 第 9 条）。
#[gpui_kit::test]
fn a_banded_query_walks_only_the_visible_lines(cx: &mut TestAppContext) {
    use folio_core::infra::metrics::{Metric, take_local};
    cx.update(gpui_kit::init);
    let handle = window(cx, 600.0, 900.0);
    let mut document = Document::new(
        format!("<p>{}</p>", LONG.repeat(120)),
        "",
        prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    cx.update_window(handle, |_, win, cx| {
        document.ensure_layout(win, cx, 0.0);
    })
    .expect("窗口应当可排版");
    let total = document.text_map().chars();
    let all_lines = document.layout().lines.len();

    // 整条带（等效于不窄化 y）：满章都走。
    take_local(Metric::RectLines);
    cx.update_window(handle, |_, win, app| {
        document.selection_rects_in_band(win, app, 0..total, 0.0..f32::MAX)
    })
    .expect("窗口应当可求矩形");
    assert_eq!(
        take_local(Metric::RectLines),
        all_lines as u64,
        "整章区间没走满：这一格数的可能不是被走过的行"
    );

    // 只留屏顶三行：走的就是那三行。
    let band = 0.0..document.layout().lines[2].y + document.layout().lines[2].height;
    let expected = document.visible_lines(band.clone()).len();
    assert!(expected < all_lines, "带要真比整章短才有可比性");
    take_local(Metric::RectLines);
    cx.update_window(handle, |_, win, app| {
        document.selection_rects_in_band(win, app, 0..total, band.clone())
    })
    .expect("窗口应当可求矩形");
    assert_eq!(
        take_local(Metric::RectLines) as usize,
        expected,
        "带内查询走的行数与可见带不符：窄化没接上，或夹错了区间"
    );

    // 带内的一条**短**批注：字符那一头也得真的夹上，只靠可见带不算窄化完成。
    // 取第 1 行行首那八个字 —— 带是三行高，字符窄化该把它压到一两行。
    let at = document.coordinate_of_line(1);
    take_local(Metric::RectLines);
    cx.update_window(handle, |_, win, app| {
        document.selection_rects_in_band(win, app, at..at + 8, band.clone())
    })
    .expect("窗口应当可求矩形");
    let walked = take_local(Metric::RectLines) as usize;
    assert!(
        walked < expected,
        "带内八个字走了 {walked} 行，与整条带的 {expected} 行一样宽：字符那一头没接上"
    );

    // 一条完全不在这一屏上的批注：一行都不走。
    let last = document.layout().lines.last().expect("有行");
    let away = (last.y + last.height + 1.0)..(last.y + last.height + 2.0);
    take_local(Metric::RectLines);
    let got = cx
        .update_window(handle, |_, win, app| {
            document.selection_rects_in_band(win, app, 0..8, away)
        })
        .expect("窗口应当可求矩形");
    assert!(got.is_empty(), "带外的一条批注还画出了矩形：{got:?}");
    assert_eq!(
        take_local(Metric::RectLines),
        0,
        "带外的一行都不该走，这一格是这条优化的本体"
    );
}

/// 一本书的作者样式表只解析**一次**，之后每进一章分的都是同一份 `Arc`。
///
/// 数的是 `Metric::RuleSetParses`。改之前 `Document::with_source` 每建一章把整本 CSS
/// 重解析一遍 —— 真机拆开量到 **1.5–1.9 ms / 章**（《未来简史》23.6 KB / 323 条规则，
/// `RuleSet::parse` 自己占 1.16 ms，release），跳一次章一次铺四章就是 7 ms 落在
/// UI 线程的同一帧里。
///
/// 变异验活：把 `Book::make_span` 与 `set_anchor` 那两处换回自己解析的
/// `Document::fetching` / `Document::new`，下面那条「进了几章」的增量断言立刻红。
#[gpui_kit::test]
fn the_author_stylesheet_is_parsed_once_per_book(cx: &mut TestAppContext) {
    use folio_core::Locator;
    use folio_core::infra::metrics::{Metric, take_local};

    use crate::ui::features::reader::model::book::Book;
    cx.update(gpui_kit::init);
    let flow = FlowDoc {
        chapters: (0..6)
            .map(|i| Chapter::inline(format!("第{i}章"), format!("<p>{LONG}（{i}）</p>")))
            .collect(),
        ..Default::default()
    };
    let doc: Arc<dyn OpenedDocument> = Arc::from(folio_core::content::document::from_content(
        DocMeta {
            title: "六章".into(),
            authors: Vec::new(),
            format: Format::Epub,
            size: 0,
            ..DocMeta::default()
        },
        Content::Reflow(flow),
    ));
    let prefs = bare_prefs();

    take_local(Metric::RuleSetParses);
    let mut book = cx.update(|app| Book::new(doc, prefs.clone(), Locator::start(), app));
    assert_eq!(
        take_local(Metric::RuleSetParses),
        1,
        "开一本书该解析一次作者样式表"
    );

    // 换锚章 + 窗口往前铺：新进来的每一章都只分那份 `Arc`，一次都不许多解析。
    book.set_anchor(4, &prefs);
    book.ensure_window_spans(&prefs, 900.0, 0.0);
    let spans = book.span_indices();
    assert!(
        spans.len() > 1,
        "fixture 得真的让几章进窗口，否则这条门是空跑的：{spans:?}"
    );
    assert_eq!(
        take_local(Metric::RuleSetParses),
        0,
        "进了 {} 章却解析了样式表：那一整本 CSS 与每章配的都是同一份规则",
        spans.len()
    );

    // 换字号也不该重解析（它只动偏好那一半哈希）。
    let mut bigger = bare_prefs();
    bigger.font_size += 4.0;
    book.set_anchor(1, &bigger);
    assert_eq!(
        take_local(Metric::RuleSetParses),
        0,
        "改偏好把作者层重解析了一遍"
    );
}

/// 一本声明了竖排的书，界面怎么走到竖排上 —— 以及走过去之后**停得下来**。
///
/// 轴写在书的 CSS 里，而 CSS 要到后台作业里才层叠，所以第一遍只能按横排排。
/// `apply_typeset` 读出那一条声明、把 `wants_vertical` 订正，下一帧折行宽换成列长重排。
/// 这一条钉三件事：第二遍真的换了轴；第三遍不再换（不收敛就是每帧重排一整章）；
/// 换完之后命中与选区矩形都跟着轴走 —— 屏幕上一个物理点要能落回它自己那一列。
#[gpui_kit::test]
fn a_vertical_book_settles_on_the_vertical_axis(cx: &mut TestAppContext) {
    use crate::ui::features::reader::model::chapter_layout::Viewport;

    let xhtml = "<body><p>中文足够长的一段，长到必须折成好几行才排得下，\
                 这样才有好几列可以对着看：折行处、留白处、块与块的接缝处都该有东西可探。\
                 再多写几句，好让这一章的列数配得上「翻页」这两个字。一列能排四十来个字，\
                 所以这里要把段落写得再长一些：中文竖排的本子通常一页十几二十列，\
                 每列从列顶排到列底，列与列之间靠行高隔开，读的顺序是从右往左一列一列推。\
                 这一段的长度要够切出好几页，也要够让命中与选区逐列走一遍，\
                 否则那些断言只在第一列里打转，第二列之后的换算错了也没人替我说话。</p></body>";
    // 窗口只有 100 宽：一页装得下三四列，这一段就非跨页不可 —— 分页那一档的
    // 「沿推进轴切片」要被真切到才算验过。
    let handle = window(cx, 100.0, 900.0);
    let mut doc = Document::new(
        xhtml.to_string(),
        "body { writing-mode: vertical-rl }",
        bare_prefs(),
        Arc::new(ChapterImages::empty()),
        Arc::new(BookFonts::empty()),
    );
    // 分页：一片 700 高、单栏、没有侧栏。竖排只在分页那一档落地。
    let page = || Viewport {
        reserve: 0.0,
        columns: 1,
        height: 700.0,
        gap: 0.0,
    };

    cx.update_window(handle, |_, win, cx| {
        doc.ensure_viewport(win, cx, page());
    })
    .expect("窗口应当可排版");
    assert!(
        !doc.layout().vertical(),
        "轴要层叠完才看得见，所以第一遍只能按横排排"
    );

    cx.update_window(handle, |_, win, cx| {
        doc.ensure_viewport(win, cx, page());
    })
    .expect("窗口应当可排版");
    assert!(doc.layout().vertical(), "第二遍必须换成竖排那根轴");
    let columns = doc.lines().len();
    assert!(columns >= 4, "这段得排出几列才有东西可验：{columns}");

    // 第三遍不许再变：不收敛的话每一帧都在重排整章。
    cx.update_window(handle, |_, win, cx| {
        doc.ensure_viewport(win, cx, page());
    })
    .expect("窗口应当可排版");
    assert!(doc.layout().vertical(), "轴定了就不该摆回横排");
    assert_eq!(
        doc.lines().len(),
        columns,
        "收敛之后每帧重排了一遍，行数还会跟着抖"
    );

    // 列从右往左堆，而每一列都从同一个列顶起。
    for pair in doc.lines().windows(2) {
        assert!(
            pair[1].x <= pair[0].x,
            "后一列应当更靠左：{} 之后是 {}",
            pair[0].x,
            pair[1].x
        );
    }
    // 首行缩进跟着轴走：横排里它推的是行首的 x，竖排里推的是**列顶的 y**。
    // 读者的 2em 缩进落在哪一根轴，是「转置做对了没有」最省事的一个判据 ——
    // 接错轴的症状是段首那一格跑到左边去，而列顶整整齐齐。
    let first = &doc.lines()[0];
    assert!(
        first.y > 0.0,
        "第一列的列顶该被首行缩进推开，实际 y = {}",
        first.y
    );
    assert!(
        doc.lines().iter().skip(1).any(|line| line.y < first.y),
        "同一段后面的列该回到列顶，不该一路带着缩进"
    );

    // 命中：拿列内靠上的一个物理点问回去，必须落在它自己那一列。
    cx.update_window(handle, |_, win, cx| {
        for (i, line) in doc.lines().iter().enumerate() {
            let at_x = line.x + line.width * 0.5;
            let at_y = line.y + 4.0;
            let hit = doc
                .hit_test(win, cx, at_x, at_y)
                .unwrap_or_else(|| panic!("第 {i} 列里点进去应当命中"));
            assert_eq!(hit.line, i, "竖排的命中要落回同一列");
        }
    })
    .expect("窗口应当可命中");

    // 选区矩形是「高的一条」而不是「宽的一条」：轴换过来的直接证据。
    let total = doc.text_map().chars();
    let rects = cx
        .update_window(handle, |_, win, cx| doc.selection_rects(win, cx, 0..total))
        .expect("窗口应当可求矩形");
    assert!(
        rects.len() >= 4,
        "整章的选区应当逐列给矩形：{}",
        rects.len()
    );
    for rect in &rects {
        assert!(
            rect.height > rect.width,
            "竖排的选区矩形该沿 y 铺：{rect:?}"
        );
    }

    // 切片切的是列：一页装得下几列由列厚决定，而且片与片按列号首尾相接。
    assert!(doc.page_count() > 1, "这么长一章在 600 宽的页里该不止一页");
}
