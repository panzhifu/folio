//! 「版面」页：正文栏最大宽度、左右与上下留白，以及正文按哪根轴排。
//!
//! 留白与版心都影响**折行**（上下留白只影响一页装多少行），所以改完必须重排 ——
//! 传播由外壳的配置观察者负责，这一页只声明取值与赋值。
//!
//! 「版心」那一组是同一件事的两头（一栏多宽、四周留多少）；「方向」单独一组：
//! 它换的是整页那根轴，与宽窄不是一回事。

use super::super::*;

pub(in crate::ui::features::settings) fn layout_page(cx: &App) -> Page {
    page(
        tr(cx, "page.layout"),
        IconName::Columns3,
        vec![
            group(
                tr(cx, "group.measure"),
                vec![
                    number(
                        cx,
                        NumberSpec {
                            title: "field.max_measure",
                            description: "field.max_measure.desc",
                            keywords: &["width", "measure", "行长", "版心", "铺满"],
                            min: Reader::MAX_MEASURE_MIN,
                            max: Reader::MAX_MEASURE_MAX,
                            step: 20.0,
                        },
                        |cx| ConfigHolder::default_reader(cx).max_measure,
                        |v, cx| ConfigHolder::edit_reader(cx, |p| p.max_measure = v),
                    ),
                    number(
                        cx,
                        NumberSpec {
                            title: "field.margin_x",
                            description: "field.margin_x.desc",
                            keywords: &["margin", "页边距", "留白"],
                            min: Reader::MARGIN_X_MIN,
                            max: Reader::MARGIN_X_MAX,
                            step: 8.0,
                        },
                        |cx| ConfigHolder::default_reader(cx).margin_x,
                        |v, cx| ConfigHolder::edit_reader(cx, |p| p.margin_x = v),
                    ),
                    number(
                        cx,
                        NumberSpec {
                            title: "field.margin_y",
                            description: "field.margin_y.desc",
                            keywords: &["margin", "页边距", "上下", "留白"],
                            min: Reader::MARGIN_Y_MIN,
                            max: Reader::MARGIN_Y_MAX,
                            step: 8.0,
                        },
                        |cx| ConfigHolder::default_reader(cx).margin_y,
                        |v, cx| ConfigHolder::edit_reader(cx, |p| p.margin_y = v),
                    ),
                ],
            ),
            group(
                tr(cx, "group.direction"),
                vec![
                    Row::dropdown(
                        tr(cx, "field.text_direction"),
                        TextDirection::ALL
                            .iter()
                            .copied()
                            .map(|direction| {
                                (
                                    SharedString::from(direction.as_str()),
                                    match direction {
                                        TextDirection::Auto => tr(cx, "text_direction.auto"),
                                        TextDirection::Vertical => {
                                            tr(cx, "text_direction.vertical")
                                        }
                                        TextDirection::Horizontal => {
                                            tr(cx, "text_direction.horizontal")
                                        }
                                    },
                                )
                            })
                            .collect(),
                        false,
                        |cx: &App| {
                            SharedString::from(ConfigHolder::default_reader(cx).text_direction.as_str())
                        },
                        |val: SharedString, cx| {
                            ConfigHolder::edit_reader(cx, |p| {
                                p.text_direction = TextDirection::parse(&val)
                            })
                        },
                    )
                    .description(tr(cx, "field.text_direction.desc"))
                    .keywords(&[
                        "direction",
                        "vertical",
                        "方向",
                        "竖排",
                        "横排",
                    ]),
                ],
            ),
        ],
    )
}
