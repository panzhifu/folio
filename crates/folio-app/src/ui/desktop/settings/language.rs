//! 界面语言。选项用**该语言自己的写法**标注：用户切换语言往往正是在看不懂
//! 当前界面的时候，把 "English" 翻成「英语」反而认不出来。

use folio_core::i18n::Locale;

use super::*;

pub(super) fn language_page(cx: &App) -> SettingPage {
    let options: Vec<(SharedString, SharedString)> = Locale::ALL
        .iter()
        .map(|locale| {
            (
                SharedString::from(locale.endonym()),
                SharedString::from(locale.code()),
            )
        })
        .collect();
    page(
        tr(cx, "page.language"),
        IconName::Languages,
        vec![
            SettingItem::new(
                tr(cx, "field.language"),
                SettingField::dropdown(
                    options,
                    |cx: &App| SharedString::from(ConfigHolder::language(cx).locale.code()),
                    |val: SharedString, cx| {
                        // 认不出的值不改：下拉框给不出认不出的值，
                        // 真出现时保留当前语言比切成未知语言好。
                        if let Some(locale) = Locale::parse(&val) {
                            ConfigHolder::edit_language(cx, |l| l.locale = locale);
                        }
                    },
                ),
            )
            .description(tr(cx, "field.language.desc"))
            .keywords(["language", "locale", "语言"]),
        ],
    )
}
