//! 阅读设置面板。字段一律走 `SettingField` 的 (取值, 赋值) 对，
//! 两端都通过 `PrefsHolder` 触碰同一个实体，所以改完立刻反映到已打开的书上。

use gpui_kit::component::setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem};
use gpui_kit::{App, SharedString};

use crate::prefs_model::PrefsHolder;

fn number(
    title: &'static str,
    hint: &'static str,
    min: f64,
    max: f64,
    step: f64,
    get: fn(&App) -> f32,
    set: fn(f32, &mut App),
) -> SettingItem {
    SettingItem::new(
        title,
        SettingField::number_input(
            NumberFieldOptions {
                min,
                max,
                step,
                ..Default::default()
            },
            move |cx: &App| get(cx) as f64,
            move |val: f64, cx| set(val as f32, cx),
        ),
    )
    .description(hint)
}

pub fn typography_group() -> SettingGroup {
    SettingGroup::new()
        .title("文字")
        .items(vec![
            number(
                "字号",
                "正文的像素字号。",
                9.0,
                72.0,
                1.0,
                |cx| PrefsHolder::get(cx).font_size,
                |v, cx| PrefsHolder::edit(cx, |p| p.font_size = v),
            ),
            number(
                "行距",
                "行高 = 字号 × 这个倍数。中文书一般 1.6–2.0。",
                1.0,
                4.0,
                0.05,
                |cx| PrefsHolder::get(cx).line_height,
                |v, cx| PrefsHolder::edit(cx, |p| p.line_height = v),
            ),
            number(
                "段距",
                "段落之间的空白，单位是字号倍。",
                0.0,
                4.0,
                0.05,
                |cx| PrefsHolder::get(cx).paragraph_gap_em,
                |v, cx| PrefsHolder::edit(cx, |p| p.paragraph_gap_em = v),
            ),
            number(
                "首行缩进",
                "中文排版习惯是 2 个字符；0 表示不缩进。",
                0.0,
                6.0,
                0.25,
                |cx| PrefsHolder::get(cx).indent_em,
                |v, cx| PrefsHolder::edit(cx, |p| p.indent_em = v),
            ),
            SettingItem::new(
                "两端对齐",
                SettingField::switch(
                    |cx: &App| PrefsHolder::get(cx).justify,
                    |val: bool, cx| PrefsHolder::edit(cx, |p| p.justify = val),
                ),
            )
            .description("中文正文建议开启；开启后行尾会更整齐。"),
        ])
}

pub fn layout_group() -> SettingGroup {
    SettingGroup::new()
        .title("版面")
        .items(vec![
            number(
                "最大行长",
                "正文栏的最大宽度（像素）。太长会伤阅读节奏。",
                280.0,
                2400.0,
                20.0,
                |cx| PrefsHolder::get(cx).max_measure,
                |v, cx| PrefsHolder::edit(cx, |p| p.max_measure = v),
            ),
            number(
                "左右页边距",
                "正文栏两侧留白（像素）。",
                0.0,
                400.0,
                8.0,
                |cx| PrefsHolder::get(cx).margin_x,
                |v, cx| PrefsHolder::edit(cx, |p| p.margin_x = v),
            ),
        ])
}

pub fn font_group() -> SettingGroup {
    SettingGroup::new()
        .title("字体")
        .items(vec![SettingItem::new(
            "正文字体",
            SettingField::input(
                |cx: &App| SharedString::from(PrefsHolder::get(cx).font_family),
                |val: SharedString, cx| {
                    if !val.trim().is_empty() {
                        PrefsHolder::edit(cx, |p| p.font_family = val.to_string());
                    }
                },
            ),
        )
        .description("填字体家族名，例如 LXGW WenKai、Noto Serif CJK SC。")])
}
