//! `overflow` 裁剪框的窄用例：排版折出的每帧只读表、祖先取交、命中/选区筛除。

use crate::tests::typeset;

#[test]
fn overflow_hidden_clips_lines_outside_the_box() {
    let html = "<div class='box'><p>甲</p><p>乙</p><p>丙</p></div>";
    let css = ".box { height: 28px; overflow: hidden } p { margin: 0; font-size: 16px; line-height: 28px }";
    let (_, laid) = typeset(html, css, 200.0);
    assert_eq!(laid.lines.len(), 3);
    let clips: Vec<_> = (0..laid.lines.len()).map(|i| laid.line_clip(i)).collect();
    assert!(clips.iter().all(Option::is_some), "三行都在 overflow 盒里");
    assert!(
        laid.line_clipped(2),
        "第三行整个落在 28px 高的盒外：{:?}",
        laid.lines[2]
    );
    assert!(!laid.line_clipped(0));
}

#[test]
fn overflow_clip_rect_intersects_nested_ancestors() {
    let html = "<div class='outer'><div class='inner'><p>甲乙</p></div></div>";
    let css = ".outer { width: 100px; overflow-x: hidden } .inner { width: 200px; overflow-y: hidden } p { margin: 0 }";
    let (_, laid) = typeset(html, css, 300.0);
    let clip = laid.line_clip(0).expect("两轴各有祖先裁剪");
    assert!(
        clip.x0.is_finite() && clip.x1.is_finite(),
        "x 被 outer 夹住"
    );
    assert!(
        clip.y0.is_finite() && clip.y1.is_finite(),
        "y 被 inner 夹住"
    );
}

#[test]
fn partially_clipped_lines_keep_their_full_clip_rect() {
    let html = "<div class='box'><p>甲乙丙丁戊己庚辛</p></div>";
    let css = ".box { width: 10px; overflow: hidden } p { margin: 0; font-size: 16px }";
    let (_, laid) = typeset(html, css, 200.0);
    let clip = laid.line_clip(0).unwrap();
    assert!(!laid.line_clipped(0), "部分相交的行仍要画可见的那一截");
    let row = &laid.lines[0];
    assert!(
        row.x + row.width > clip.x1,
        "这一行确实伸出了裁剪框：行宽 {} vs 右缘 {}",
        row.width,
        clip.x1
    );
}
