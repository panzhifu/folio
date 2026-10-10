//! 关于：这份二进制是什么、界面说什么语言、以及两份讲实际情况的文档。
//!
//! 分组照的是参照应用同一页的形状 —— **版本与语言在同一页**，语言不自成一页：读者到这一页
//! 要问的是「关于这个应用」这两件事，而字、版面那些是要改的东西，各归各页。
//!
//! 这里没有那颗「检查更新」的开关，也没有许可证那一组：本应用还没有更新检查与授权子系统，
//! 为了对齐样式先摆一颗点了没用的控件是错的。依赖版本那几行是构建时从 `Cargo.lock` 抄来的
//! （见 `folio-app/build.rs`），两份文档是 `docs/` 里那两个文件的原文 —— 这一页的全部价值
//! 就是它不会另说一套。

use folio_core::i18n::Locale;
use folio_core::paths::config_path;
use gpui_kit::component::button::Button;
use gpui_kit::component::{Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::{
    DefiniteLength, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div,
};

use super::*;
use crate::ui::shared::about;

pub(super) fn about_page(cx: &App) -> SettingPage {
    page(
        tr(cx, "page.about"),
        IconName::Info,
        vec![
            version_group(cx),
            language_group(cx),
            docs_group(cx),
            config_group(cx),
        ],
    )
}

/// 「关于」这一组：跑着的这一份二进制，以及它是用哪几版依赖编出来的。
fn version_group(cx: &App) -> SettingGroup {
    SettingGroup::new().title(tr(cx, "page.about")).items(vec![
        value_item(
            tr(cx, "about.version"),
            about::version(),
            Some(tr(cx, "about.version_desc")),
            &["version", "build", "版本"],
        ),
        value_item(
            tr(cx, "about.gpui_pre"),
            about::gpui_pre(),
            None,
            &["gpui", "engine"],
        ),
        value_item(
            tr(cx, "about.kit"),
            about::kit(),
            None,
            &["gpui-kit", "component"],
        ),
        value_item(
            tr(cx, "about.mobile"),
            about::mobile_layer(),
            None,
            &["android", "mobile", "移动"],
        ),
    ])
}

/// 一行「读得出但改不了」的事实：标签在左，值在右 —— 与每项字段那一行的动作位同一侧。
fn value_item(
    label: SharedString,
    value: &str,
    description: Option<SharedString>,
    keywords: &[&'static str],
) -> SettingItem {
    let value: SharedString = value.into();
    let mut item = SettingItem::new(
        label,
        SettingField::render(move |_, _, _| {
            h_flex()
                .w_full()
                .justify_end()
                .child(div().text_sm().child(value.clone()))
        }),
    );
    if let Some(description) = description {
        item = item.description(description);
    }
    item.keywords(keywords.iter().map(|word| (*word).to_string()))
}

/// 界面语言。选项用**该语言自己的写法**标注：用户切换语言往往正是在看不懂
/// 当前界面的时候，把 "English" 翻成「英语」反而认不出来。
fn language_group(cx: &App) -> SettingGroup {
    let options: Vec<(SharedString, SharedString)> = Locale::ALL
        .iter()
        .map(|locale| {
            (
                SharedString::from(locale.endonym()),
                SharedString::from(locale.code()),
            )
        })
        .collect();
    group(
        tr(cx, "field.language"),
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

/// 两份嵌进来的文档，各占一行。点开是一侧展开的面板：整篇文档不是
/// 「一个简短的决定」，那一格装不下它。
fn docs_group(cx: &App) -> SettingGroup {
    SettingGroup::new()
        .title(tr(cx, "group.docs"))
        .description(tr(cx, "about.docs_note"))
        .items(vec![
            doc_item(
                "plan",
                tr(cx, "about.plan"),
                tr(cx, "about.open_doc"),
                about::PLAN,
                &["doc", "plan", "文档", "方案"],
            ),
            doc_item(
                "limitations",
                tr(cx, "about.limitations"),
                tr(cx, "about.open_doc"),
                about::LIMITATIONS,
                &["doc", "gap", "文档", "缺口"],
            ),
        ])
}

/// `slug` 是那颗按钮的元素 id 的一部分：用 ASCII 而不是标签文字，措辞改了
/// 元素树里的状态键不用跟着改。
fn doc_item(
    slug: &'static str,
    label: SharedString,
    button_label: SharedString,
    doc: &'static str,
    keywords: &[&'static str],
) -> SettingItem {
    SettingItem::new(
        label.clone(),
        SettingField::render(move |_, _, _| {
            // 渲染闭包是 `Fn`，所以标题只能一次一次拷给那颗按钮的点击闭包 ——
            // 后者要把这份标题带进面板的构造里，而面板的构造又是另一层 `Fn`。
            let sheet_title = label.clone();
            h_flex().w_full().justify_end().child(
                Button::new(format!("open-doc-{slug}"))
                    .outline()
                    .small()
                    .label(button_label.to_string())
                    .accessibility_label(button_label.clone())
                    .on_click(move |_, window, cx| {
                        let sheet_title = sheet_title.clone();
                        window.open_sheet(cx, move |sheet, _, _| {
                            // 默认那 350px 是给一行表单用的；一篇文档要读就得宽得多。
                            sheet
                                .title(sheet_title.clone())
                                .size(DefiniteLength::Fraction(0.5))
                                .child(doc_body(doc))
                        });
                    }),
            )
        }),
    )
    .keywords(keywords.iter().map(|word| (*word).to_string()))
}

/// 文档全文：按行摊开的一块滚动区。
///
/// 整篇一个文字元素会让这一格变成一个比屏幕高几十倍的元素，滚动范围与裁剪都不好算。
fn doc_body(doc: &'static str) -> impl IntoElement {
    div()
        .id("about-doc-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(v_flex().gap_0().children(doc.lines().map(|line| {
            div()
                .w_full()
                .text_sm()
                .child(if line.is_empty() { " " } else { line })
        })))
}

/// 配置文件在哪儿。
///
/// 它是这一页少数几条「读者改不了但需要知道」的事实之一：换主题要往哪个目录放 JSON、
/// 偏好存在哪个文件，都从这一条推出来。
fn config_group(cx: &App) -> SettingGroup {
    let path = config_path().display().to_string();
    group(
        tr(cx, "field.config_path"),
        vec![
            SettingItem::new(
                tr(cx, "field.config_path"),
                // 自己拼这一行而不是用 `widgets::muted_label`：那个小件返回的类型借用着
                // `&App`，而字段的渲染闭包要一个 `'static` 的元素。
                SettingField::render(move |_, _, cx| {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(path.clone())
                }),
            )
            .description(tr(cx, "field.config_path.desc"))
            .keywords(["config", "path", "配置", "文件"]),
        ],
    )
}
