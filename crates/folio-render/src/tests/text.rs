//! `text-transform` 与 `content: attr()` 的收集层用例。

use super::typeset;

/// `uppercase` / `lowercase` 在字节与字符数都不变时生效。
#[test]
fn text_transform_changes_case_when_offsets_stay_aligned() {
    let (_dom, laid) = typeset(
        "<p class='u'>abc</p>",
        "p { text-indent: 0 } .u { text-transform: uppercase }",
        300.0,
    );
    assert_eq!(laid.line_text(0), "ABC");
    let (_dom, laid) = typeset(
        "<p class='l'>ABC</p>",
        "p { text-indent: 0 } .l { text-transform: lowercase }",
        300.0,
    );
    assert_eq!(laid.line_text(0), "abc");
}

/// 改长度或改字符数的（`ß → SS`）保留原文 —— 锚点按源文本字节走。
#[test]
fn text_transform_keeps_length_changing_forms_verbatim() {
    let (_dom, laid) = typeset(
        "<p class='u'>straße</p>",
        "p { text-indent: 0 } .u { text-transform: uppercase }",
        300.0,
    );
    assert_eq!(laid.line_text(0), "straße");
}

/// `capitalize` 只大写每个空白分隔词的首字符。
#[test]
fn text_transform_capitalize_touches_word_starts() {
    let (_dom, laid) = typeset(
        "<p class='c'>hello world</p>",
        "p { text-indent: 0 } .c { text-transform: capitalize }",
        300.0,
    );
    assert_eq!(laid.line_text(0), "Hello World");
}

/// `content: attr(名字)` 取所属元素的属性值。
#[test]
fn content_attr_reads_the_owner_attribute() {
    let (_dom, laid) = typeset(
        "<p><span class='n' data-end='完'>中</span></p>",
        "p { text-indent: 0 } .n::after { content: attr(data-end) }",
        300.0,
    );
    let text = laid.line_text(0);
    assert!(text.contains('完'), "attr 生成字符：{text}");
}

/// `no-open-quote` 抵掉 UA 给 `q` 的开引号。
#[test]
fn no_open_quote_suppresses_the_default_quote() {
    let (_dom, laid) = typeset(
        "<p>前<q>中</q>后</p>",
        "p { text-indent: 0 } q::before { content: no-open-quote }",
        300.0,
    );
    let text = laid.line_text(0);
    assert!(!text.contains('“'), "开引号被抵掉：{text}");
}

/// `text-combine` 把连续两位数字并成一格：竖排四个数字只占两格；
/// 每位带交叉轴偏移，与绘制同源。
#[test]
fn text_combine_pairs_digits_into_one_cell() {
    use crate::LineFragment;

    let runs = |laid: &crate::Layout| {
        laid.lines[0]
            .fragments
            .iter()
            .filter_map(|frag| match frag {
                LineFragment::Text(run) => Some(run.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let css = "body { writing-mode: vertical-rl } \
               p { text-indent: 0; font-size: 20px; font-family: monospace; \
                   white-space: nowrap; -webkit-text-combine: horizontal }";
    let (_dom, combined) = typeset("<p class='p'>1234</p>", css, 400.0);
    let combined = runs(&combined);
    assert_eq!(
        combined.iter().map(|run| run.width).collect::<Vec<_>>(),
        vec![20.0, 0.0, 20.0, 0.0],
        "两对数字各占一格"
    );
    assert_eq!(
        combined
            .iter()
            .map(|run| run.cross_offset)
            .collect::<Vec<_>>(),
        vec![0.0, -10.0, 0.0, -10.0],
        "每对第二位在 vertical-rl 里沿交叉轴挪到右边（正深度往左）"
    );
    assert_eq!(
        combined.iter().map(|run| run.x_offset).collect::<Vec<_>>(),
        vec![0.0, -20.0, 0.0, -20.0],
        "第二位回退一格，与第一位同处一格"
    );

    let plain_css = "body { writing-mode: vertical-rl } \
                     p { text-indent: 0; font-size: 20px; font-family: monospace; white-space: nowrap }";
    let (_dom, plain) = typeset("<p class='p'>1234</p>", plain_css, 400.0);
    let plain = runs(&plain);
    assert_eq!(plain.len(), 1, "不带 text-combine 时四位数字仍是一段");
    assert_eq!(plain[0].width, 40.0);
}

/// 并格只认 ASCII 数字，多字节汉字不能被按字节劈开。
#[test]
fn text_combine_keeps_multibyte_neighbours_intact() {
    use crate::LineFragment;

    let css = "body { writing-mode: vertical-rl } \
               p { text-indent: 0; font-size: 20px; font-family: monospace; \
                   white-space: nowrap; -webkit-text-combine: horizontal }";
    let (_dom, laid) = typeset("<p class='p'>甲12乙</p>", css, 400.0);
    assert_eq!(laid.line_text(0), "甲12乙");
    let widths = laid.lines[0]
        .fragments
        .iter()
        .filter_map(|frag| match frag {
            LineFragment::Text(run) => Some(run.width),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        widths,
        vec![25.0, 25.0, 0.0, 20.0],
        "汉字按 1.25em 回退度量，数字对仍只占一格"
    );
}

/// 断点不落进数字对中间：宽度只够一格半时，`012` 只能断在成对之后。
#[test]
fn text_combine_does_not_break_inside_a_pair() {
    let css = "body { writing-mode: vertical-rl } \
               p { text-indent: 0; font-size: 20px; font-family: monospace; \
                   -webkit-text-combine: horizontal }";
    let (_dom, laid) = typeset("<p class='p'>012</p>", css, 20.0);
    assert!(laid.lines.len() >= 2, "三格内容放不进一格半的列高");
    assert!(
        laid.line_text(0).contains("01"),
        "第一行不能把 01 拆开：{}",
        laid.line_text(0)
    );
}
