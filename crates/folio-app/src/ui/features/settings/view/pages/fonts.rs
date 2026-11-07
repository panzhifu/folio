//! 「字体」页：正文家族与字重，加上读者自己装的字体（下载 / 导入 / 删除）。
//!
//! 家族不再是自由文本：列的是**这台设备此刻真注册着的**家族（`TextSystem::all_font_names()`
//! 现算，含随包字体、打开这本书时注册的内嵌字体与自装字体）。手打一个不存在的名字不会
//! 报错，排版层只会静默退回读者的字体 —— 读者以为自己改成功了，这正是表单不该给的那条路。
//!
//! 字重是三档下拉：档位住在 `Reader::FONT_WEIGHT_PRESETS`，与阅读页那张卡片共用一份。
//!
//! 「获取字体」与「已装字体」两组住在 [`font_store`]：清单与动作接 `services::user_fonts`，
//! 已装列表的真相源是配置 —— 与词典页同一条。

use super::super::*;
use super::font_store;

/// 一档字重在界面上的名字。档位之间的数（手改配置）按下拉里实际会选中的那一档念，
/// 判据是 [`Reader::nearest_font_weight`]。
fn weight_label(cx: &App, weight: u16) -> SharedString {
    match Reader::nearest_font_weight(weight) {
        300 => tr(cx, "font_weight.light"),
        700 => tr(cx, "font_weight.bold"),
        _ => tr(cx, "font_weight.regular"),
    }
}

pub(in crate::ui::features::settings) fn fonts_page(cx: &App) -> Page {
    let family = Row::font(
        tr(cx, "field.font_family"),
        |cx: &App| SharedString::from(ConfigHolder::default_reader(cx).font_family),
        |val: SharedString, cx| {
            if !val.trim().is_empty() {
                ConfigHolder::edit_reader(cx, |p| p.font_family = val.to_string());
            }
        },
    )
    .description(tr(cx, "field.font_family.desc"))
    .keywords(&["font", "family", "字体"]);

    let weight = Row::dropdown(
        tr(cx, "field.font_weight"),
        Reader::FONT_WEIGHT_PRESETS
            .iter()
            .copied()
            .map(|weight| {
                (
                    SharedString::from(weight.to_string()),
                    weight_label(cx, weight),
                )
            })
            .collect(),
        false,
        |cx: &App| {
            SharedString::from(
                Reader::nearest_font_weight(ConfigHolder::default_reader(cx).font_weight)
                    .to_string(),
            )
        },
        |val: SharedString, cx| {
            if let Ok(weight) = val.parse::<u16>() {
                ConfigHolder::edit_reader(cx, |p| p.font_weight = weight);
            }
        },
    )
    .description(tr(cx, "field.font_weight.desc"))
    .keywords(&["weight", "bold", "字重", "粗细", "加粗"]);

    page(
        tr(cx, "page.fonts"),
        IconName::ALargeSmall,
        vec![
            group(tr(cx, "group.family"), vec![family, weight]),
            group(tr(cx, "group.fonts_store"), vec![font_store::store_row()]),
            group(
                tr(cx, "group.fonts_installed"),
                vec![font_store::installed_row()],
            ),
        ],
    )
}
