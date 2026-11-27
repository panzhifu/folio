//! Flex 简写的新用例；从 `tests.rs` 切出来守行数基线。

use super::*;

/// `flex` 简写：三值拆成 grow/shrink/basis；单值长度当 basis、单值数字当 grow。
#[test]
fn flex_shorthand_splits_into_grow_shrink_basis() {
    let page = author(
        "<i>a</i><i>b</i><i>c</i>",
        "i:nth-child(1) { flex: 0 1 34% } \
         i:nth-child(2) { flex: 2 } \
         i:nth-child(3) { flex: 50px }",
    );
    let ids = tags(&page, "i");
    let a = style(&page, ids[0]);
    assert_eq!(a.flex_grow, 0.0);
    assert_eq!(a.flex_shrink, 1.0);
    assert_eq!(a.flex_basis, Some(Length::Percent(0.34)));
    let b = style(&page, ids[1]);
    assert_eq!(b.flex_grow, 2.0);
    assert_eq!(b.flex_shrink, 1.0);
    assert_eq!(b.flex_basis, None);
    let c = style(&page, ids[2]);
    assert_eq!(c.flex_grow, 1.0);
    assert_eq!(c.flex_basis, Some(Length::Px(50.0)));
}
