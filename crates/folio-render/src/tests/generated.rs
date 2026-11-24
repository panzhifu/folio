//! UA 默认生成内容的用例：没写 `q::before/after` 的书也要有引号。

use super::typeset;

/// UA 表给 `q` 生成成对引号；作者没写 `q::before/after` 时照样出字符。
#[test]
fn ua_gives_q_default_curly_quotes() {
    let (_dom, laid) = typeset("<p>前<q>中</q>后</p>", "p { text-indent: 0 }", 400.0);
    let text: String = (0..laid.lines.len())
        .map(|i| laid.line_text(i))
        .collect::<Vec<_>>()
        .join("");
    assert!(text.contains('“') && text.contains('”'), "默认引号：{text}");
}

/// 作者写 `q::before { color: … }` 只改颜色，不把 UA 给的 `content` 弄丢。
#[test]
fn author_color_on_q_before_keeps_the_ua_quote() {
    let (_dom, laid) = typeset(
        "<p>前<q>中</q>后</p>",
        "p { text-indent: 0 } q::before { color: red }",
        400.0,
    );
    let text: String = (0..laid.lines.len())
        .map(|i| laid.line_text(i))
        .collect::<Vec<_>>()
        .join("");
    assert!(text.contains('“'), "作者只改颜色时引号仍在：{text}");
}
