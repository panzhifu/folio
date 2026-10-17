//! 「目录」页：目录那一屏长什么样 —— 行的松紧、标题占几行、来过的章画不画成浅色。
//!
//! 三项都只影响**目录**，一项都不碰正文：正文的松紧是「文字」那页的行距，
//! 这里改的是列表。所以它们不并进「文字」或「版面」那一页 —— 混进去之后
//! 「我刚才调的是哪个」就没人答得出来。
//!
//! 没有「恢复默认」那颗按钮：与其余各页同一条规矩，控件里没有默认值那份信息
//! （见 `mod.rs` 的 `setting_page`）。

use folio_core::TocDensity;

use super::*;

pub(super) fn toc_page(cx: &App) -> Page {
    page(
        tr(cx, "page.toc"),
        IconName::ListTree,
        vec![group(
            tr(cx, "group.rows"),
            vec![
                Row::choice(
                    tr(cx, "field.toc_density"),
                    density_options(cx),
                    false,
                    |cx: &App| SharedString::from(ConfigHolder::reader(cx).toc_density.as_str()),
                    |val: SharedString, cx| {
                        ConfigHolder::edit_reader(cx, |prefs| {
                            prefs.toc_density = TocDensity::parse(&val)
                        });
                    },
                )
                .description(tr(cx, "field.toc_density.desc"))
                .keywords(&["density", "spacing", "密度", "行高", "目录"]),
                number(
                    cx,
                    NumberSpec {
                        title: "field.toc_max_lines",
                        description: "field.toc_max_lines.desc",
                        keywords: &["title", "lines", "行数", "标题", "省略"],
                        min: 1.0,
                        max: 3.0,
                        step: 1.0,
                    },
                    |cx| f32::from(ConfigHolder::reader(cx).toc_max_title_lines),
                    |v, cx| {
                        ConfigHolder::edit_reader(cx, |prefs| {
                            prefs.toc_max_title_lines = v.round().clamp(1.0, 3.0) as u8
                        });
                    },
                ),
                Row::toggle(
                    tr(cx, "field.toc_dim_read"),
                    |cx: &App| ConfigHolder::reader(cx).toc_dim_read,
                    |val: bool, cx| ConfigHolder::edit_reader(cx, |prefs| prefs.toc_dim_read = val),
                )
                .description(tr(cx, "field.toc_dim_read.desc"))
                .keywords(&["read", "visited", "已读", "浅色", "进度"]),
            ],
        )],
    )
}

/// 行高档位的选项。`(值, 标签)` —— 值是配置里存的那串（与 `TocDensity::as_str` 同形），
/// 标签是当前界面语言下的说法。顺序就是下拉框里的顺序。
fn density_options(cx: &App) -> Vec<(SharedString, SharedString)> {
    [
        (TocDensity::Compact, "density.compact"),
        (TocDensity::Comfortable, "density.comfortable"),
    ]
    .into_iter()
    .map(|(density, label_key)| (SharedString::from(density.as_str()), tr(cx, label_key)))
    .collect()
}

#[cfg(test)]
mod tests {
    //! 「值」那一半是能被写坏的：下拉框的取值闭包给的是 `toc_density.as_str()`，
    //! 选项值必须是同一个字符串，否则按钮永远显示裸代码而不是「紧凑 / 宽松」，
    //! 而 setter 那边 `parse` 认不出来就静默回落 —— 读者点了没反应。

    use super::*;
    use crate::ui::shared::config_model::ConfigHolder;
    use folio_core::Config;
    use gpui_kit::TestAppContext;

    #[gpui_kit::test]
    fn every_density_option_round_trips_through_the_config(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let options = density_options(cx);
            let stored = SharedString::from(ConfigHolder::reader(cx).toc_density.as_str());
            assert!(
                options.iter().any(|(value, _)| *value == stored),
                "当前档位 {stored:?} 必须是某个选项值，否则下拉框只能显示裸代码"
            );
            for (value, label) in &options {
                assert!(!label.is_empty(), "{value:?} 的标签是空的");
                // 认不出的值会回落，所以这一句真正在测的是「值拼错了」：
                // parse(错值) 回落 Comfortable，与 Compact 的键不同 → 红。
                ConfigHolder::edit_reader(cx, |prefs| prefs.toc_density = TocDensity::parse(value));
                let back = ConfigHolder::reader(cx).toc_density;
                assert_eq!(
                    back.as_str(),
                    value.as_ref(),
                    "{value:?} 写进去再读出来不是它自己"
                );
            }
        });
    }
}
