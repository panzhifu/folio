//! 「字体」页：正文家族。
//!
//! 只有一个自由文本框，因为「装了哪些字体」是操作系统的事：这台设备上不存在的那个
//! 家族名，排版层会退回设备能解析的名字（见 `ui/shared/measure.rs` 的泛族名处理），
//! 而不是把字画成方框。列一个「本机字体清单」反而会把没装的也列出来。

use super::*;

pub(super) fn fonts_page(cx: &App) -> Page {
    let family = Row::text(
        tr(cx, "field.font_family"),
        |cx: &App| SharedString::from(ConfigHolder::reader(cx).font_family),
        |val: SharedString, cx| {
            // 空串不改：清空输入框不该把正文换成 gpui 的默认家族，
            // 那是个没有汉字的拉丁字体。
            if !val.trim().is_empty() {
                ConfigHolder::edit_reader(cx, |p| p.font_family = val.to_string());
            }
        },
    )
    .description(tr(cx, "field.font_family.desc"))
    .keywords(&["font", "family", "字体"]);

    page(
        tr(cx, "page.fonts"),
        IconName::ALargeSmall,
        vec![group(tr(cx, "group.family"), vec![family])],
    )
}
