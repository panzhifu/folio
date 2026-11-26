//! 新增 grid 轨道声明的解析用例；从 `tests.rs` 切出来守行数基线。

use super::*;

/// `grid-template-rows` 与列一样逐 token 解析；不认识的值整条作废。
#[test]
fn grid_template_rows_are_parsed_and_counted() {
    let ok = author(
        "<div class='g'></div>",
        ".g { display: grid; grid-template-rows: 40px 1fr minmax(20px, 80px) }",
    );
    let g = style(&ok, tag(&ok, "div"));
    assert_eq!(g.grid_template_rows.len(), 3);
    assert!(matches!(g.grid_template_rows[0], GridTrack::Length(_)));
    assert!(matches!(g.grid_template_rows[1], GridTrack::Fr(_)));
    assert!(matches!(g.grid_template_rows[2], GridTrack::MinMax(_, _)));
    assert_eq!(ok.styled.ignored, 0, "合法的轨道不该算落差");

    let bad = author(
        "<div class='g'></div>",
        ".g { display: grid; grid-template-rows: 40px nope }",
    );
    assert_eq!(bad.styled.ignored, 1, "整条作废并留一条痕迹");
    assert!(style(&bad, tag(&bad, "div")).grid_template_rows.is_empty());
}


