//! 关于：这份二进制是什么、界面说什么语言、配置文件在哪儿。
//!
//! 分组照的是参照应用同一页的形状 —— **版本与语言在同一页**，语言不自成一页：读者到这一页
//! 要问的是「关于这个应用」这两件事，而字、版面那些是要改的东西，各归各页。
//!
//! 这里没有那颗「检查更新」的开关，也没有许可证那一组：本应用还没有更新检查与授权子系统，
//! 为了对齐样式先摆一颗点了没用的控件是错的。依赖版本那几行是构建时从 `Cargo.lock` 抄来的
//! （见 `folio-app/build.rs`）—— 这一页的全部价值就是它不会另说一套。

use folio_core::infra::i18n::Locale;
use folio_core::infra::paths::config_path;
use gpui_kit::{IntoElement as _, ParentElement, SharedString, Styled, div};

use super::*;

pub(super) fn about_page(cx: &App) -> Page {
    page(
        tr(cx, "page.about"),
        IconName::Info,
        vec![version_group(cx), language_group(cx), config_group(cx)],
    )
}

/// 「关于」这一组：跑着的这一份二进制，以及它是用哪几版依赖编出来的。
fn version_group(cx: &App) -> Group {
    group(
        tr(cx, "page.about"),
        vec![
            value_item(
                tr(cx, "about.version"),
                version(),
                Some(tr(cx, "about.version_desc")),
                &["version", "build", "版本"],
            ),
            value_item(tr(cx, "about.gpui_pre"), gpui_pre(), None, &["gpui", "engine"]),
            value_item(tr(cx, "about.kit"), kit(), None, &["gpui-kit", "component"]),
            value_item(
                tr(cx, "about.mobile"),
                mobile_layer(),
                None,
                &["android", "mobile", "移动"],
            ),
        ],
    )
}

// 下面四份事实刻意不手写：值由 `build.rs` 从 `Cargo.lock` 抄成编译期常量，
// 手写的一份和锁文件迟早分叉，而分叉的恰好是最需要准的那几行。

/// 本应用版本（`Cargo.toml` 里那一份 workspace 版本）。
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// GPUI 本体（crates.io 上的 `gpui-pre` 快照）。
fn gpui_pre() -> &'static str {
    option_env!("FOLIO_GPUI_PRE").unwrap_or("未知")
}

/// 组件层 gpui-kit：版本号 + 用的那个 commit。
fn kit() -> &'static str {
    option_env!("FOLIO_KIT").unwrap_or("未知")
}

/// 移动平台层 gpui-pre-mobile（fork）：版本号 + 钉住的那个 commit。
fn mobile_layer() -> &'static str {
    option_env!("FOLIO_MOBILE").unwrap_or("未知")
}

/// 一行「读得出但改不了」的事实：标签在左，值在右 —— 与每项字段那一行的动作位同一侧。
fn value_item(
    label: SharedString,
    value: &str,
    description: Option<SharedString>,
    keywords: &'static [&'static str],
) -> Row {
    let mut row = Row::info(label, value);
    if let Some(description) = description {
        row = row.description(description);
    }
    row.keywords(keywords)
}

/// 界面语言。选项用**该语言自己的写法**标注：用户切换语言往往正是在看不懂
/// 当前界面的时候，把 "English" 翻成「英语」反而认不出来。
fn language_group(cx: &App) -> Group {
    group(
        tr(cx, "field.language"),
        vec![Row::dropdown(
            tr(cx, "field.language"),
            language_options(),
            false,
            |cx: &App| SharedString::from(ConfigHolder::language(cx).locale.code()),
            |val: SharedString, cx| {
                // 认不出的值不改：下拉框给不出认不出的值，
                // 真出现时保留当前语言比切成未知语言好。
                if let Some(locale) = Locale::from_label(&val) {
                    ConfigHolder::edit_language(cx, |l| l.locale = locale);
                }
            },
        )
        .description(tr(cx, "field.language.desc"))
        .keywords(&[
            "language",
            "locale",
            "语言",
            "中文",
            "简体",
            "English",
            "日本語",
        ])],
    )
}

/// 语言下拉框的选项：`(值, 标签)`，值是 BCP-47 码（配置里存的就是它），
/// 标签是语言自己的名字。
///
/// **值必须在第一位**：下拉框的约定是元组首项作为选中值（与取值闭包给出的
/// `locale.code()` 对得上）与写回 setter 的实参，次项才是按钮和菜单里显示的文字。
/// 写反了会出现两重故障：当前语言对不上任何一个选项值，按钮只能显示裸代码；
/// 点菜单项传给 setter 的是语言名，`from_label` 之外的路径认不出来，改动被静默
/// 丢弃 —— 读者点什么语言都不生效。
fn language_options() -> Vec<(SharedString, SharedString)> {
    Locale::ALL
        .iter()
        .map(|locale| {
            (
                SharedString::from(locale.code()),
                SharedString::from(locale.endonym()),
            )
        })
        .collect()
}

/// 配置文件在哪儿。
///
/// 它是这一页少数几条「读者改不了但需要知道」的事实之一：换主题要往哪个目录放 JSON、
/// 偏好存在哪个文件，都从这一条推出来。
fn config_group(cx: &App) -> Group {
    let path = config_path().display().to_string();
    group(
        tr(cx, "field.config_path"),
        vec![Row::custom_titled(
            tr(cx, "field.config_path"),
            move |cx| {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(path.clone())
                    .into_any_element()
            },
        )
        .description(tr(cx, "field.config_path.desc"))
        .keywords(&["config", "path", "配置", "文件"])],
    )
}

#[cfg(test)]
mod tests {
    //! 下拉框的选项是一份 (值, 标签) 的表格，写错顺序不会有任何编译期或运行期报错
    //! —— setter 对认不出的值是**静默丢弃**（见 [`language_group`] 的取值对）。
    //! 这里把「值必须认得回来、标签必须是语言自己的名字」钉成会失败的测试。

    use super::*;

    /// 每个选项的值都要能被写回路径（`Locale::from_label`）认出来，
    /// 且认出来的就是标签写的那个语言。这里曾经把 (值, 标签) 写反：
    /// 值成了语言名，setter 认不出，点什么语言都不生效。
    #[test]
    fn every_language_option_round_trips_through_its_own_value() {
        for (value, label) in language_options() {
            let locale = Locale::from_label(&value)
                .unwrap_or_else(|| panic!("选项值 {value:?} 必须被写回路径认出来"));
            assert_eq!(
                label,
                locale.endonym(),
                "{value:?} 的标签应是这个语言自己的名字"
            );
        }
    }

    /// 取值闭包给出的是 `locale.code()`，它必须对得上某个选项值，
    /// 否则按钮显示的不是当前语言的标签，而是裸代码的兜底。
    #[test]
    fn the_current_locale_is_always_representable_as_an_option_value() {
        let values: Vec<_> = language_options().into_iter().map(|(v, _)| v).collect();
        for locale in Locale::ALL {
            assert!(
                values.iter().any(|value| *value == locale.code()),
                "{} 应该作为选项值出现",
                locale.code()
            );
        }
    }
}
