//! 字体商店：内置清单里的开源字体，与读者已经装下的那几份。
//!
//! 清单来自 `folio_core::fonts`（随二进制走，每款都是钉了 tag 的 OFL 直链，页面上
//! 摆官网与许可证两颗外链）。卡片的动作接 `services::user_fonts` 的单槽状态 ——
//! 同一时刻只有一趟在装，「哪张卡在动」从状态里的 id 认，其余卡的下载钮照常可点
//! 但服务会拒。已装列表的真相源是配置（与词典同一条）；删除走退休表，选择器立刻
//! 看不见它。

use folio_core::fonts::FontItem;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{AnyElement, IntoElement as _, Styled as _, div};

use super::super::*;
use crate::ui::components::muted_label;
use crate::ui::platform;
use crate::ui::services::user_fonts::{self, Status};

pub(in crate::ui::features::settings) fn store_row() -> Row {
    Row::custom(store_control).keywords(&[
        "font", "download", "store", "字体", "下载", "霞鹜", "文楷", "思源", "OFL",
    ])
}

pub(in crate::ui::features::settings) fn installed_row() -> Row {
    Row::custom(installed_list).keywords(&["font", "installed", "字体", "已装", "我的"])
}

/// 商店那一行：导入入口 + 每款字体一张卡。
fn store_control(cx: &App) -> AnyElement {
    let mut column = v_flex().w_full().gap_2().child(import_control(cx));
    for item in folio_core::fonts::manifest().items.iter().cloned() {
        column = column.child(font_card(cx, item));
    }
    column.into_any_element()
}

/// 导入入口与上一趟的失败句。失败挂在整行（不是某张卡）：它可能来自任何一趟
/// （下载或导入的任何一份），摆回卡片上反而说不清是哪张卡的错。
fn import_control(cx: &App) -> AnyElement {
    let mut column = v_flex()
        .w_full()
        .gap_2()
        .child(
            Button::new("font-import")
                .label(tr(cx, "field.fonts_import").to_string())
                .on_click(|_, _, cx| user_fonts::import_from_picker(cx)),
        )
        .child(muted_label(tr(cx, "field.fonts_import.desc"), cx));
    match user_fonts::status(cx) {
        Status::Importing => {
            column = column.child(muted_label(tr(cx, "font_store.importing"), cx));
        }
        Status::Failed(reason) => {
            column = column.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tf(cx, "font_store.failed", &[("reason", &reason)])),
            );
        }
        _ => {}
    }
    column.into_any_element()
}

/// 一款字体一张卡：名字 + 大小 + 动作钮，描述与官网 / 许可证外链垫在下面。
fn font_card(cx: &App, item: FontItem) -> AnyElement {
    let desc = if chinese_ui(cx) || item.desc_en.is_empty() {
        item.desc.clone()
    } else {
        item.desc_en.clone()
    };
    let mut column = v_flex()
        .w_full()
        .gap_1p5()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(cx.theme().foreground)
                        .child(item.name.clone()),
                )
                .child(muted_label(human_bytes(item.size), cx))
                .child(action_button(cx, &item)),
        )
        .child(muted_label(desc, cx))
        .child(links(cx, &item));

    if let Status::Downloading {
        id,
        received,
        total,
    } = user_fonts::status(cx)
        && id == item.id
    {
        let fraction = if total > 0 {
            Some((received as f32 / total as f32).clamp(0.0, 1.0))
        } else {
            None
        };
        column = column.child(
            h_flex()
                .w_full()
                .items_center()
                .gap_3()
                .child(
                    Progress::new(format!("font-progress-{}", item.id))
                        .w_full()
                        .flex_1()
                        .loading(fraction.is_none())
                        .value(fraction.unwrap_or(0.0)),
                )
                .child(muted_label(
                    tf(
                        cx,
                        "font_store.received",
                        &[
                            ("received", &human_bytes(received)),
                            ("total", &human_bytes(total)),
                        ],
                    ),
                    cx,
                ))
                .child(
                    Button::new(format!("font-cancel-{}", item.id))
                        .ghost()
                        .small()
                        .label(tr(cx, "font_store.cancel").to_string())
                        .on_click(|_, _, cx| user_fonts::cancel(cx)),
                ),
        );
    }
    column.into_any_element()
}

/// 卡上那颗动作钮：装好了置灰，在装的不给钮（进度行自带「取消」），其余给「下载」。
fn action_button(cx: &App, item: &FontItem) -> AnyElement {
    if ConfigHolder::fonts(cx).has(&item.id) {
        return Button::new(format!("font-installed-{}", item.id))
            .label(tr(cx, "font_store.downloaded").to_string())
            .disabled(true)
            .into_any_element();
    }
    Button::new(format!("font-download-{}", item.id))
        .label(tr(cx, "font_store.download").to_string())
        .on_click({
            let id = item.id.clone();
            move |_, _, cx| user_fonts::download(cx, &id)
        })
        .into_any_element()
}

/// 官网与许可证两颗外链。`open_url` 返回有没有人接手 —— 没人接手时链接的内容
/// 就在钮上，读者不至于什么也拿不到（与更新页那两颗同一个兜底）。
fn links(cx: &App, item: &FontItem) -> AnyElement {
    let mut row = h_flex().w_full().gap_2();
    if !item.official.is_empty() {
        let url = item.official.clone();
        row = row.child(
            Button::new(format!("font-official-{}", item.id))
                .ghost()
                .small()
                .label(tr(cx, "font_store.official").to_string())
                .on_click(move |_, _, _| {
                    platform::open_url(&url);
                }),
        );
    }
    let url = item.license.url.clone();
    row = row.child(
        Button::new(format!("font-license-{}", item.id))
            .ghost()
            .small()
            .label(item.license.name.clone())
            .on_click(move |_, _, _| {
                platform::open_url(&url);
            }),
    );
    row.into_any_element()
}

/// 已装列表。每份一行：家族名 + 大小 + 删除。
fn installed_list(cx: &App) -> AnyElement {
    let entries = user_fonts::entries(cx);
    if entries.is_empty() {
        return muted_label(tr(cx, "field.fonts_installed_empty"), cx).into_any_element();
    }
    let mut column = v_flex().w_full().gap_2();
    for entry in entries {
        let name = if entry.family.is_empty() {
            entry.id.clone()
        } else {
            entry.family.clone()
        };
        let delete_label = tr(cx, "font_store.delete").to_string();
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
                        .child(div().truncate().child(name))
                        .child(muted_label(human_bytes(entry.size_bytes), cx)),
                )
                .child(
                    Button::new(format!("font-delete-{delete_id}"))
                        .ghost()
                        .small()
                        .icon(IconName::Trash)
                        .tooltip(delete_label.clone())
                        .accessibility_label(delete_label)
                        .on_click(move |_, _, cx| user_fonts::remove(cx, delete_id.clone())),
                ),
        );
    }
    column.into_any_element()
}

/// 界面语言是不是中文：清单描述按它挑 zh / en 那一句。
fn chinese_ui(cx: &App) -> bool {
    ConfigHolder::language(cx).locale.code().starts_with("zh")
}

fn human_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const KB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes >= MB {
        format!("{:.1} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    /// 大小的念法：MB 一位小数、KB 取整、零头给字节。清单页那句「约 25 MB」与
    /// 进度行都用它。
    #[test]
    fn bytes_read_as_human_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(4096), "4 KB");
        assert_eq!(human_bytes(25_928_940), "24.7 MB");
    }
}
