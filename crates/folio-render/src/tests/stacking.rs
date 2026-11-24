//! 层叠序（z-index / stacking context）的绘制顺序用例。
//! 从 `tests.rs` 分出，只因为那个文件贴着行数上限；共享 helper 经 `super` 取用。

use super::{find, typeset};
use folio_format::dom::Dom;

/// 每行按 owner 的祖先链收集层叠上下文 z：相对定位标题 [1]，正文没有上下文 → `None`。
#[test]
fn line_z_orders_positioned_title_above_absolute_poster() {
    let html = "<p>正文</p><div class='poster'><img/></div>\
                <h1 class='title'>章</h1><img src='poster.png'/>";
    let css = ".poster { position: absolute; z-index: 0; bottom: 0; left: 0 } \
               .title { position: relative; z-index: 1 }";
    let (_, laid) = typeset(html, css, 200.0);
    assert_eq!(laid.line_z.len(), laid.lines.len(), "每行都有 z 链");
    let dom = Dom::parse(html.as_bytes());
    let title = find(&dom, "h1");
    let para = find(&dom, "p");
    let title_lines: Vec<usize> = laid
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.owner == title)
        .map(|(i, _)| i)
        .collect();
    let para_lines: Vec<usize> = laid
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.owner == para)
        .map(|(i, _)| i)
        .collect();
    assert!(!title_lines.is_empty() && !para_lines.is_empty());
    assert_eq!(
        laid.line_z[title_lines[0]].as_deref(),
        Some(&[1][..]),
        "相对定位 z=1 的标题行链是 [1]"
    );
    assert_eq!(laid.line_z[para_lines[0]], None, "正文没有上下文");
}

/// 没有定位/z-index/opacity/transform 时所有行都没有链 —— 绘制走原序的零回归前提。
#[test]
fn line_z_is_all_none_without_stacking_contexts() {
    let (_dom, laid) = typeset("<p>甲</p><p>乙</p>", "p { text-indent: 0 }", 200.0);
    assert!(laid.line_z.iter().all(Option::is_none));
}

/// 嵌套上下文保留「根→owner」的完整链：外层 1 + 内层 2 → [1, 2]。
#[test]
fn nested_contexts_keep_the_outer_to_inner_chain() {
    let html = "<div class='outer'><p class='inner'>内</p></div>";
    let css = ".outer { position: relative; z-index: 1 } .inner { position: relative; z-index: 2; text-indent: 0 }";
    let (dom, laid) = typeset(html, css, 200.0);
    let inner = find(&dom, "p");
    let line = laid
        .lines
        .iter()
        .position(|l| l.owner == inner)
        .expect("内层应有一行");
    assert_eq!(
        laid.line_z[line].as_deref(),
        Some(&[1, 2][..]),
        "外层先、内层后"
    );
}
