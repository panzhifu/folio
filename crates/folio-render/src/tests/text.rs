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
