//! 朗读：念哪些行、空行不占格。

use super::harness::{PARA, flow, open, prefs, settle};
use folio_core::{Chapter, FlowDoc, Locator};
use gpui_kit::TestAppContext;

/// 读屏那一份「这一屏的字」：**念的正好是带内那些非空行，顺序与画出来的一致，带外一条不念**。
///
/// 树那一半（`folio-reader` 上的 `.role()` + `.a11y_synthetic_children()`）**在宿主上验不了**，
/// 而且不是「测不到就算了」那种验不了：测试平台的 `a11y_init` 走的是 `PlatformWindow` 那个
/// 默认空实现（`gpui-pre-0.3.9/src/platform.rs:1387`；`src/platform/test.rs` 里 `a11y` 零命中），
/// 没有谁会调 `activation` ⇒ `is_a11y_active()` 恒为 false ⇒ 整棵 accessibility 树在测试里
/// 根本不生成，`Window::debug_a11y_tree_json()` 恒为 `None`。拿它写断言就是断一个空集，
/// 红的绿的都是假。所以这里钉的是**递给树的那份数据**；播报与焦点顺序要到真机上把 TalkBack
/// 打开按一下才算。
#[gpui_kit::test]
fn the_spoken_lines_are_the_visible_band_and_nothing_else(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, handle) = open(
        cx,
        &flow(),
        "markdown",
        prefs(16.0),
        Locator::start(),
        460.0,
    );
    settle(cx, handle);
    let (spoken, in_band, whole_chapter) = view.read_with(cx, |v, _| {
        let doc = v
            .book()
            .and_then(|book| book.span_doc(0))
            .expect("第一章该已经排版");
        // 带取整章高的三分之一：80 段的章远高过一屏，「带外不念」才不是空话。
        let band = 0.0..doc.height() / 3.0;
        let lines = doc.visible_lines(band.clone());
        let texts = |range: std::ops::Range<usize>| -> Vec<String> {
            range.map(|ix| doc.layout().line_text(ix)).collect()
        };
        (
            super::super::spoken_lines(doc, lines.clone()),
            texts(lines),
            texts(0..doc.line_count()),
        )
    });

    // 念出来的 = 带内那些行的可见文本，逐行、按顺序、空行不占格。这一条同时钉住
    // 「文本向引擎要」（哨兵字符已摘）与「空行不落进树」两件事。
    let expected: Vec<&str> = in_band
        .iter()
        .map(String::as_str)
        .filter(|t| !t.is_empty())
        .collect();
    let got: Vec<&str> = spoken.iter().map(|line| line.as_str()).collect();
    assert_eq!(got, expected, "念的该正好是带内那些非空行");

    // 带外那些行一条都不许念 —— 读屏走到末尾就是这一屏的末尾，不该把滚出屏的字也念一遍。
    let all: String = got.concat();
    let outside: Vec<&str> = whole_chapter
        .iter()
        .map(String::as_str)
        .filter(|t| !t.is_empty() && !in_band.iter().any(|line| line == t))
        .collect();
    assert!(!outside.is_empty(), "带外该有字可排除，否则这条断言是空的");
    for line in &outside {
        assert!(!all.contains(line), "带外那一行不该被念出来：{line}");
    }
    assert!(
        all.contains("（第 0 段）"),
        "带内的第一段该念得到：{all:.80}"
    );
}

/// 空行不占一格：读屏的序列里不该出现一条什么都没有的空节点（点过去会念成「空白」）。
///
/// 夹具自己也要断言一次 —— `<p><br></p>` 在引擎里是**一条没有片段的行**
/// （`folio_core::layout::geometry.rs` 的 `LineBox::fragments` 那句注释），它给的
/// `line_text` 是空串。没有这种行，这条用例就是在断言空集。
#[gpui_kit::test]
fn a_blank_line_takes_no_spoken_node(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let source = format!("<p>{PARA}（第一段）</p><p><br></p><p>{PARA}（第三段）</p>");
    let flow = FlowDoc {
        chapters: vec![Chapter::inline("第一章", source)],
        ..Default::default()
    };
    let (view, handle) = open(cx, &flow, "markdown", prefs(16.0), Locator::start(), 460.0);
    settle(cx, handle);
    let (spoken, every_line) = view.read_with(cx, |v, _| {
        let doc = v
            .book()
            .and_then(|book| book.span_doc(0))
            .expect("第一章该已经排版");
        // 带取整章高：这一条要的不是「裁到屏内」，是空行会不会漏进序列。
        let lines = 0..doc.line_count();
        let every: Vec<String> = lines.clone().map(|ix| doc.layout().line_text(ix)).collect();
        (super::super::spoken_lines(doc, lines), every)
    });
    assert!(
        every_line.iter().any(String::is_empty),
        "夹具里那条 `<br>` 该产出一行空文本，否则这条用例什么都没钉：{every_line:?}"
    );
    assert_eq!(
        spoken.len(),
        2,
        "两段字各一格，中间那条空行不占格：{spoken:?}"
    );
    assert!(
        spoken.iter().all(|line| !line.is_empty()),
        "念出来的每一格都该有字"
    );
}
