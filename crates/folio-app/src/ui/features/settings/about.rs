//! 关于：配置文件在哪儿。
//!
//! 只有这一条，因为它是**这一页唯一读者改不了但需要知道**的事实 ——
//! 换主题要往哪个目录放 JSON、偏好存在哪个文件，都从这一条推出来。
//! 版本号、作者、许可证那类内容留给打包时再决定要不要上。

use folio_core::paths::config_path;
use gpui_kit::{ParentElement, Styled, div};

use super::*;

pub(super) fn about_page(cx: &App) -> SettingPage {
    let muted = cx.theme().muted_foreground;
    page(
        tr(cx, "page.about"),
        IconName::Info,
        vec![
            SettingItem::new(
                tr(cx, "field.config_path"),
                SettingField::render(move |_, _, _cx| {
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(config_path().display().to_string())
                }),
            )
            .description(tr(cx, "field.config_path.desc"))
            .keywords(["config", "path", "配置", "文件"]),
        ],
    )
}
