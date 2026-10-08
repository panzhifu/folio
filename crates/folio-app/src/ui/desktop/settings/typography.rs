//! 「文字」页：字号、行距、段距、首行缩进、两端对齐。
//!
//! 五项都是**即时生效**的：写入即规整、即落盘、即让所有已打开的书重排，
//! 所以这里没有「应用」按钮，也没有改了不生效的中间态。

use super::*;

pub(super) fn typography_page(cx: &App) -> SettingPage {
    page(
        tr(cx, "page.typography"),
        IconName::Type,
        vec![
            number(
                cx,
                NumberSpec {
                    title: "field.font_size",
                    description: "field.font_size.desc",
                    keywords: &["font", "size", "字号"],
                    min: 9.0,
                    max: 72.0,
                    step: 1.0,
                },
                |cx| ConfigHolder::reader(cx).font_size,
                |v, cx| ConfigHolder::edit_reader(cx, |p| p.font_size = v),
            ),
            number(
                cx,
                NumberSpec {
                    title: "field.line_height",
                    description: "field.line_height.desc",
                    keywords: &["line-height", "行距", "行高"],
                    min: 1.0,
                    max: 4.0,
                    step: 0.05,
                },
                |cx| ConfigHolder::reader(cx).line_height,
                |v, cx| ConfigHolder::edit_reader(cx, |p| p.line_height = v),
            ),
            number(
                cx,
                NumberSpec {
                    title: "field.paragraph_gap",
                    description: "field.paragraph_gap.desc",
                    keywords: &["paragraph", "段距", "段间距"],
                    min: 0.0,
                    max: 4.0,
                    step: 0.05,
                },
                |cx| ConfigHolder::reader(cx).paragraph_gap_em,
                |v, cx| ConfigHolder::edit_reader(cx, |p| p.paragraph_gap_em = v),
            ),
            number(
                cx,
                NumberSpec {
                    title: "field.indent",
                    description: "field.indent.desc",
                    keywords: &["indent", "缩进"],
                    min: 0.0,
                    max: 6.0,
                    step: 0.25,
                },
                |cx| ConfigHolder::reader(cx).indent_em,
                |v, cx| ConfigHolder::edit_reader(cx, |p| p.indent_em = v),
            ),
            SettingItem::new(
                tr(cx, "field.justify"),
                SettingField::switch(
                    |cx: &App| ConfigHolder::reader(cx).justify,
                    |val: bool, cx| ConfigHolder::edit_reader(cx, |p| p.justify = val),
                ),
            )
            .description(tr(cx, "field.justify.desc"))
            .keywords(["justify", "align", "对齐", "两端"]),
        ],
    )
}
