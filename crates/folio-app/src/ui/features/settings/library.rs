//! 书库：导入的书**拷到哪儿**，以及在哪儿改这个位置。
//!
//! 这一页存在的理由是导入门禁的另一头：导入第一次会拦下来问位置（见
//! `desktop::app` 里的 `import`，移动端是书架上那块内联面板），问过一次就存进配置、
//! 之后不再拦 —— 所以必须有个地方能改它，否则读者答错一次就再也改不回来了。
//!
//! 填成空串等于「没确认过」：下一次导入会重新问一遍。清掉这一格和第一次用这个应用
//! 是同一个状态，不需要额外的开关。

use gpui_kit::{ParentElement, SharedString, Styled, div};

use super::*;
use crate::ui::shared::library::suggested_dir;
use crate::ui::widgets::muted_label;

pub(super) fn library_page(cx: &App) -> SettingPage {
    page(
        tr(cx, "page.library"),
        IconName::FolderOpen,
        vec![
            SettingItem::new(
                tr(cx, "field.library_dir"),
                SettingField::input(
                    |cx: &App| SharedString::from(ConfigHolder::library(cx).dir),
                    |val: SharedString, cx| {
                        ConfigHolder::edit_library(cx, |library| library.dir = val.to_string());
                    },
                ),
            )
            .description(tr(cx, "field.library_dir.desc"))
            .keywords(["library", "书库", "目录", "folder"]),
            // 建议位置照实写出来：读者要的是知道不管填没填、默认会去哪儿，
            // 而不是自己去猜一个路径。
            SettingItem::render(move |_, _, cx| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(muted_label(tr(cx, "field.library_dir.suggested"), cx))
                    .child(div().child(suggested_dir().display().to_string()))
            }),
        ],
    )
}
