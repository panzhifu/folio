//! 「文字」页：字号、行距、字距、段距、首行缩进、两端对齐，加上翻页沿哪根轴走、
//! 新划的一条留哪种痕迹。
//!
//! 各项都是**即时生效**的：写入即规整、即落盘，正文各项即让所有已打开的书重排，
//! 所以这里没有「应用」按钮，也没有改了不生效的中间态。
//!
//! 「翻页」与「划线」那两组是例外，而且是刻意的：它们即时生效但**不重排**，因为它们存在
//! `appearance` 而不是 `Reader` 那一节 —— `Reader` 整份是排版失效键，把动画档位或一条痕迹
//! 怎么画塞进去，等于「调一次翻页速度 / 换一种划线方式触发整章重排」
//! （见 `folio_core::infra::config::PageTurn` 与 `AnnotationStyle`）。
//!
//! 前面两组是照读者改它们的时机分的：「正文」那一组是行的高度（一路跟着眼睛走），
//! 「段落」那一组是每一段的形状（开头缩进与行尾齐不齐）。

use folio_core::AnnotationStyle;

use super::super::*;

pub(in crate::ui::features::settings) fn typography_page(cx: &App) -> Page {
    page(
        tr(cx, "page.typography"),
        IconName::Type,
        vec![
            group(
                tr(cx, "group.body"),
                vec![
                    number(
                        cx,
                        NumberSpec {
                            title: "field.font_size",
                            description: "field.font_size.desc",
                            keywords: &["font", "size", "字号"],
                            min: Reader::FONT_SIZE_MIN,
                            max: Reader::FONT_SIZE_MAX,
                            step: 1.0,
                        },
                        |cx| ConfigHolder::default_reader(cx).font_size,
                        |v, cx| ConfigHolder::edit_reader(cx, |p| p.font_size = v),
                    ),
                    number(
                        cx,
                        NumberSpec {
                            title: "field.line_height",
                            description: "field.line_height.desc",
                            keywords: &["line-height", "行距", "行高"],
                            min: Reader::LINE_HEIGHT_MIN,
                            max: Reader::LINE_HEIGHT_MAX,
                            step: 0.05,
                        },
                        |cx| ConfigHolder::default_reader(cx).line_height,
                        |v, cx| ConfigHolder::edit_reader(cx, |p| p.line_height = v),
                    ),
                    number(
                        cx,
                        NumberSpec {
                            title: "field.letter_spacing",
                            description: "field.letter_spacing.desc",
                            keywords: &["letter-spacing", "字距", "字间"],
                            min: Reader::LETTER_SPACING_MIN,
                            max: Reader::LETTER_SPACING_MAX,
                            step: 0.01,
                        },
                        |cx| ConfigHolder::default_reader(cx).letter_spacing_em,
                        |v, cx| ConfigHolder::edit_reader(cx, |p| p.letter_spacing_em = v),
                    ),
                    number(
                        cx,
                        NumberSpec {
                            title: "field.paragraph_gap",
                            description: "field.paragraph_gap.desc",
                            keywords: &["paragraph", "段距", "段间距"],
                            min: Reader::PARAGRAPH_GAP_MIN,
                            max: Reader::PARAGRAPH_GAP_MAX,
                            step: 0.05,
                        },
                        |cx| ConfigHolder::default_reader(cx).paragraph_gap_em,
                        |v, cx| ConfigHolder::edit_reader(cx, |p| p.paragraph_gap_em = v),
                    ),
                ],
            ),
            group(
                tr(cx, "group.paragraph"),
                vec![
                    number(
                        cx,
                        NumberSpec {
                            title: "field.indent",
                            description: "field.indent.desc",
                            keywords: &["indent", "缩进"],
                            min: Reader::INDENT_MIN,
                            max: Reader::INDENT_MAX,
                            step: 0.25,
                        },
                        |cx| ConfigHolder::default_reader(cx).indent_em,
                        |v, cx| ConfigHolder::edit_reader(cx, |p| p.indent_em = v),
                    ),
                    Row::toggle(
                        tr(cx, "field.justify"),
                        |cx: &App| ConfigHolder::default_reader(cx).justify,
                        |val: bool, cx| ConfigHolder::edit_reader(cx, |p| p.justify = val),
                    )
                    .description(tr(cx, "field.justify.desc"))
                    .keywords(&["justify", "align", "对齐", "两端"]),
                ],
            ),
            group(
                tr(cx, "group.page_turn"),
                vec![
                    Row::choice(
                        tr(cx, "field.page_turn"),
                        PageTurn::ALL
                            .iter()
                            .copied()
                            .map(|turn| {
                                // 六档全摆，与底栏那一格同一个可选集。这一屏不知道当前
                                // 摆法也不需要知道：摆法由这一档自己带过去
                                // （`chrome::reconcile` 在渲染帧里解），不存在「选到一个
                                // 当场不出场的档」那种组合了。
                                (
                                    SharedString::from(turn.as_str()),
                                    match turn {
                                        PageTurn::Slide => tr(cx, "page_turn.slide"),
                                        PageTurn::SlideUp => tr(cx, "page_turn.slide_up"),
                                        PageTurn::Cover => tr(cx, "page_turn.cover"),
                                        PageTurn::Curl => tr(cx, "page_turn.curl"),
                                        PageTurn::Scroll => tr(cx, "page_turn.scroll"),
                                        PageTurn::None => tr(cx, "page_turn.none"),
                                    },
                                )
                            })
                            .collect(),
                        false,
                        |cx: &App| {
                            SharedString::from(ConfigHolder::default_appearance(cx).page_turn.as_str())
                        },
                        |val: SharedString, cx| {
                            ConfigHolder::edit_appearance(cx, |a| {
                                a.page_turn = PageTurn::parse(&val)
                            })
                        },
                    )
                    .description(tr(cx, "field.page_turn.desc"))
                    .keywords(&[
                        "page",
                        "turn",
                        "animation",
                        "翻页",
                        "动画",
                        "平移",
                    ]),
                ],
            ),
            group(
                tr(cx, "group.marking"),
                vec![
                    // 三档画法的键名就是库里那一列存的东西（`AnnotationStyle::as_str`），
                    // 与 `[page_turn]` 那张表同一条口径：配置里那份与库里的读法共用一套名字，
                    // 换一档不需要在两个地方各改一次。
                    Row::choice(
                        tr(cx, "field.highlight_style"),
                        AnnotationStyle::ALL
                            .iter()
                            .copied()
                            .map(|style| {
                                (
                                    SharedString::from(style.as_str()),
                                    match style {
                                        AnnotationStyle::Highlight => {
                                            tr(cx, "highlight_style.highlight")
                                        }
                                        AnnotationStyle::Line => tr(cx, "highlight_style.line"),
                                        AnnotationStyle::Wavy => tr(cx, "highlight_style.wavy"),
                                    },
                                )
                            })
                            .collect(),
                        false,
                        |cx: &App| {
                            SharedString::from(
                                ConfigHolder::default_appearance(cx).highlight_style.as_str(),
                            )
                        },
                        |val: SharedString, cx| {
                            ConfigHolder::edit_appearance(cx, |a| {
                                a.highlight_style = AnnotationStyle::parse(&val)
                            })
                        },
                    )
                    .description(tr(cx, "field.highlight_style.desc"))
                    .keywords(&[
                        "highlight",
                        "underline",
                        "wavy",
                        "划线",
                        "高亮",
                        "波浪",
                    ]),
                ],
            ),
        ],
    )
}
