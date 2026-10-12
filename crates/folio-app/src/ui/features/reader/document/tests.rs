//! 本章的排版状态（本文件）与 `measure.rs` 在宿主上跑的用例。
//!
//! 为什么这些算移动端的测试：移动 UI 本身只在 `cfg(mobile)` 下编译，宿主的
//! `cargo test` 碰不到那一侧的视图。但「同一本书在两端排出一样的行」这件事
//! 完全落在这一层 —— 源文本包装、层叠、版心宽度、断行与缓存都在这里，
//! 移动端只是把它们摆上屏的那层薄壳。所以这一层被测住，移动端的排版就不是没测过。
//!
//! 反过来讲也很清楚：这里**测不到**的是触摸滚动、章节按钮的点击、主题在 Android 上
//! 解析到浅还是深 —— 那几件只有真开窗口、真上机器才算验证。

use std::sync::Arc;

use crate::ui::features::reader::book_fonts::BookFonts;
use crate::ui::features::reader::document::{Document, body_of};
use crate::ui::shared::images::ChapterImages;
use folio_core::Reader;
use folio_core::layout::LineFragment;
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

/// 章里写的 `<style>` 也要进层叠：样式表**文件**按清单回容器取，而写在章里的
/// 那一份随正文到达 —— 作者把样式写在哪一份里，层叠结果都不该有差别。
#[test]
fn a_chapters_own_style_block_reaches_the_cascade() {
    use folio_core::content::format::css::computed::{Color, cascade};
    let source = "<html><head><style>p { color: #C0392B }</style></head>\
                      <body><p>字</p></body></html>";
    let dom = folio_core::content::dom::Dom::parse(source.as_bytes());
    let rules = Document::chapter_style_rules(&dom);
    assert_eq!(rules.rules.len(), 1, "章里就一条规则：{:?}", rules.rules);

    let styles = cascade(
        &dom,
        &rules,
        &folio_core::content::format::css::computed::ComputedStyle::default(),
    );
    let p = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("p"))
        .map(|(id, _)| id)
        .expect("该有 p");
    assert_eq!(
        styles.get(p).unwrap().color,
        Some(Color(0xC0, 0x39, 0x2B)),
        "章内 <style> 的颜色要落到 p 上"
    );
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
/// 「`chapter_hash` 对得上就复用产物」那一支退回成无条件 `ChapterArtifacts::build`，
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
        "改一次字号重解析了一章：产物没按 `chapter_hash` 复用"
    );

    // 换章就必须重解析 —— 反面不成立说明复用条件写反了（拿上一章的 DOM 排这一章）。
    document.set_chapter(format!("<p>{}</p>", LONG.repeat(4)));
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
        document.layout_key(600.0, 1),
        document.layout_key(600.0, 1),
        "同一代内问两次必须给出同一把键"
    );
    assert_eq!(document.layout_key(600.4, 1).width, 600, "栏宽量化到 1px");

    // 改字号：换键，但不换复用范围。
    let (a, b) = (document.layout_key(600.0, 1), document.layout_key(600.0, 2));
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
    let before = document.layout_key(600.0, 1);
    document.set_prefs(bigger);
    let after = document.layout_key(600.0, 1);
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
        document.layout_key(600.0, 1).context_rev(),
        "换了图片索引还复用旧块，行高就是按上一批图片算的"
    );

    // 换章：章文本进了复用范围（`NodeId` 只在同一份 DOM 里作数）。
    document.set_chapter("<p>下一章</p>".to_string());
    assert_ne!(
        before.chapter_hash,
        document.layout_key(600.0, 1).chapter_hash,
        "换章必须换章哈希"
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
        document.layout_key(600.0, 1).author_css_hash,
        with_css.layout_key(600.0, 1).author_css_hash,
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
        .layout_key(600.0, 1)
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
    /// **已知没接上**的那一格：断言它现在动不了版式。哪天接上了，这条测试会红，
    /// 就把格子改成 `Layout` —— 挂着的口子至少是登记过的，而不是没人知道的。
    NotWired,
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
    ("justify", |p| p.justify = !p.justify, PrefClass::NotWired),
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
                .map(|line| (line.x, line.y, line.width))
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
            // 「两端对齐」现在动不了版式：`layout/collect.rs` 的对齐只有
            // `Right` / `Center` 两个分支，`Justify` 落进 `_ => 0.0` 与左对齐同解。
            // 接上那一天把这一格改成 `Layout`，这条会先红一次提醒。
            PrefClass::NotWired => assert!(
                !changed,
                "偏好 {name} 登记的是「没接上」，它要是突然能动版式了，说明有人接上了却没改分类"
            ),
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
