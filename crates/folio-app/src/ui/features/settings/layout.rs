//! 「版面」页：正文栏最大宽度与左右留白。
//!
//! 两项都影响**折行**，所以改完必须重排 —— 传播由外壳的配置观察者负责，
//! 这一页只声明取值与赋值。
//!
//! 一组而不是两组：这一页只有两项，且它们是同一件事的两头（一栏多宽、两侧留多少）。

use super::*;

pub(super) fn layout_page(cx: &App) -> Page {
    page(
        tr(cx, "page.layout"),
        IconName::Columns3,
        vec![group(
            tr(cx, "group.measure"),
            vec![
                number(
                    cx,
                    NumberSpec {
                        title: "field.max_measure",
                        description: "field.max_measure.desc",
                        keywords: &["width", "measure", "行长", "版心", "铺满"],
                        min: 0.0,
                        max: 4000.0,
                        step: 20.0,
                    },
                    |cx| ConfigHolder::reader(cx).max_measure,
                    |v, cx| ConfigHolder::edit_reader(cx, |p| p.max_measure = v),
                ),
                number(
                    cx,
                    NumberSpec {
                        title: "field.margin_x",
                        description: "field.margin_x.desc",
                        keywords: &["margin", "页边距", "留白"],
                        min: 0.0,
                        max: 400.0,
                        step: 8.0,
                    },
                    |cx| ConfigHolder::reader(cx).margin_x,
                    |v, cx| ConfigHolder::edit_reader(cx, |p| p.margin_x = v),
                ),
            ],
        )],
    )
}
