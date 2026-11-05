//! 词典：本地 StarDict 的导入、启用与删除。
//!
//! 列表的真相源是配置（`Config::dictionary.installed`），不是扫盘：导入完成时写一条、
//! 删除时摘一条，所以这一页画的就是阅读器动作条判「有没有词典」时读的那一份数。
//!
//! 导入是文件活（拷贝几十 MB），走 `services::dictionary` 的作业池；这一屏只 observe
//! 它那份状态实体，「正在导入 / 失败原因」才跟着回来。

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable as _, h_flex, v_flex};
use gpui_kit::{AnyElement, IntoElement as _, ParentElement as _, Styled as _, div};

use super::super::*;
use crate::ui::components::{access_question, muted_label, waiting_for};
use crate::ui::services::dictionary;
use crate::ui::services::library::Pick;

pub(in crate::ui::features::settings) fn dictionary_page(cx: &App) -> Page {
    let installed = Row::custom(installed_list).keywords(&[
        "dictionary",
        "stardict",
        "词典",
        "詞典",
        "查词",
        "释义",
        "离线",
        "offline",
    ]);
    let import = Row::custom(import_control).keywords(&[
        "dictionary",
        "import",
        "stardict",
        "导入",
        "匯入",
        "词典",
        "文件夹",
        "folder",
    ]);
    page(
        tr(cx, "page.dictionary"),
        IconName::BookOpen,
        vec![
            group(tr(cx, "group.dictionary_installed"), vec![installed]),
            group(tr(cx, "group.dictionary_import"), vec![import]),
        ],
    )
}

/// 已导入列表。每本一行：书名 + 词数 + 启用开关 + 删除。
fn installed_list(cx: &App) -> AnyElement {
    let entries = dictionary::entries(cx);
    let config = ConfigHolder::dictionary(cx);
    let mut column = v_flex().w_full().gap_2();
    if entries.is_empty() {
        return muted_label(tr(cx, "field.dictionary_empty"), cx).into_any_element();
    }
    for entry in entries {
        let enabled = config.is_enabled(&entry.id);
        let words = tf(
            cx,
            "dictionary.words",
            &[("count", &entry.words.to_string())],
        );
        let enable_label = tf(cx, "dictionary.enable", &[("name", &entry.name)]);
        let delete_label = tf(cx, "dictionary.delete", &[("name", &entry.name)]);
        let switch_id = entry.id.clone();
        let delete_id = entry.id.clone();
        column = column.child(
            h_flex()
                .w_full()
                .items_center()
                .gap_3()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().truncate().child(entry.name.clone()))
                        .child(muted_label(words, cx)),
                )
                .child(
                    Switch::new(format!("dict-enabled-{switch_id}"))
                        .large()
                        .checked(enabled)
                        .accessibility_label(enable_label)
                        .on_click(move |checked, _, cx| {
                            dictionary::set_enabled(cx, &switch_id, *checked)
                        }),
                )
                .child(
                    Button::new(format!("dict-delete-{delete_id}"))
                        .ghost()
                        .small()
                        .icon(IconName::Trash)
                        .tooltip(delete_label.clone())
                        .accessibility_label(delete_label)
                        .on_click(move |_, _, cx| dictionary::remove(cx, delete_id.clone())),
                ),
        );
    }
    column.into_any_element()
}

/// 导入入口：没拿到「所有文件访问」时先摆那一问，与书库页同一个面板。
fn import_control(cx: &App) -> AnyElement {
    if waiting_for(Pick::Dictionary, cx) {
        return access_question(cx);
    }
    let mut column = v_flex()
        .w_full()
        .gap_2()
        .child(
            Button::new("dictionary-import")
                .label(tr(cx, "field.dictionary_import").to_string())
                .on_click(|_, _, cx| dictionary::import_from_picker(cx)),
        )
        .child(muted_label(tr(cx, "field.dictionary_import.desc"), cx));
    match dictionary::status(cx) {
        dictionary::Status::Idle => {}
        dictionary::Status::Importing => {
            column = column.child(muted_label(tr(cx, "dictionary.importing"), cx));
        }
        dictionary::Status::ImportFailed(reason) => {
            column = column.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tf(cx, "dictionary.import_failed", &[("reason", &reason)])),
            );
        }
        dictionary::Status::DeleteFailed(reason) => {
            column = column.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tf(cx, "dictionary.delete_failed", &[("reason", &reason)])),
            );
        }
    }
    column.into_any_element()
}
