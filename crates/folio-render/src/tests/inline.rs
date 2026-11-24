//! 行内级 display（inline-block / inline-table）的退化口径：内容就地摊平、不断行。

use super::typeset;

/// `inline-block`（脚注图标那种裹一层图的写法）留在段落同一行，不把段落劈成块。
#[test]
fn inline_block_content_stays_in_the_paragraph_line() {
    let (_dom, laid) = typeset(
        "<p>前<span class='b'>中</span>后</p>",
        "p { text-indent: 0 } .b { display: inline-block }",
        400.0,
    );
    assert_eq!(laid.lines.len(), 1, "inline-block 不该断行");
    assert_eq!(laid.line_text(0), "前中后");
}

/// `inline-table`（语调标记的两层 fallback）同样按行内摊平，内容顺序不变。
#[test]
fn inline_table_content_stays_in_the_paragraph_line() {
    let (_dom, laid) = typeset(
        "<p>前<span class='t'><span class='r'><span class='c'>啊</span></span>\
         <span class='r'><span class='c'>△</span></span></span>后</p>",
        "p { text-indent: 0 } .t { display: inline-table }\
         .r { display: table-row } .c { display: table-cell }",
        400.0,
    );
    assert_eq!(laid.lines.len(), 1, "inline-table 不该断行");
    let text = laid.line_text(0);
    assert!(text.starts_with('前') && text.ends_with('后'), "{text}");
    assert!(
        text.contains('啊') && text.contains('△'),
        "内容顺序保留：{text}"
    );
}
