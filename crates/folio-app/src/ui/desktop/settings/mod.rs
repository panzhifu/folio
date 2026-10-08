//! 阅读设置的页面集：每页一个文件，`mod.rs` 只管**顺序**与**共用前言**。
//!
//! 分层照的是框架里其它应用的做法（一个 `settings/` 目录、一页一个文件、`mod.rs`
//! 把组件与配置入口一次性 `pub(super) use` 出去，页面文件只写 `use super::*`）。
//! 这样加一页只需要动两处：新文件 + [`SettingsPage`] 里一个变体，
//! 而不是在一个越来越长的文件里找位置。
//!
//! 这里只负责**描述**每一页：标题、说明、控件、取值与赋值。挂到哪儿是 [`window`] 的事 ——
//! 设置是一个独立的、开-or-聚焦的窗口，不是对话框，理由写在那个子模块里。
//!
//! 字段一律走 `SettingField` 的 (取值, 赋值) 对，两端都通过 [`ConfigHolder`]
//! 触碰同一个实体，所以改完立刻反映到已打开的书上并落盘。

mod about;
mod account;
mod appearance;
mod fonts;
mod language;
mod layout;
mod typography;
pub mod window;

// 宿主只和这三个动作打交道：开（默认页）、开到某一页、关。
pub use window::{close, open, open_at};

// 页面文件之间共用的前言：设置组件、主题、配置入口与文案函数。
pub(super) use folio_core::Mode;
pub(super) use gpui_kit::assets::IconName;
pub(super) use gpui_kit::component::ActiveTheme;
pub(super) use gpui_kit::component::ThemeMode;
pub(super) use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};
pub(super) use gpui_kit::{App, SharedString};

pub(super) use crate::ui::shared::config_model::ConfigHolder;
pub(super) use crate::ui::shared::l10n::tr;
pub(super) use crate::ui::shared::theme;

// 每一页的构造函数只在 `pages` 里用，所以是私有导入而不是再导出。
use about::about_page;
use account::account_page;
use appearance::appearance_page;
use fonts::fonts_page;
use language::language_page;
use layout::layout_page;
use typography::typography_page;

/// 各设置页。变体顺序就是 `Settings` 侧栏里从上到下的顺序，
/// [`SettingsPage::index`] 靠它定位，所以**新增变体只能加在末尾**。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsPage {
    Typography,
    Layout,
    Fonts,
    Appearance,
    Language,
    Account,
    About,
}

impl SettingsPage {
    /// 全部页面，按导航顺序。
    pub const ALL: [SettingsPage; 7] = [
        SettingsPage::Typography,
        SettingsPage::Layout,
        SettingsPage::Fonts,
        SettingsPage::Appearance,
        SettingsPage::Language,
        SettingsPage::Account,
        SettingsPage::About,
    ];

    /// 在 `Settings` 组件里的页序下标。必须与 [`pages`] 的组装顺序一致 ——
    /// 「从账号进账号页、从工具条进文字页」全靠这个下标定位。
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|page| *page == self).unwrap_or(0)
    }

    /// 从下标反解，越界时回落到第一页（工具条那种「打开设置」不指定页时也是第一页）。
    pub fn from_index(ix: usize) -> Self {
        Self::ALL.get(ix).copied().unwrap_or(Self::Typography)
    }
}

/// 按 [`SettingsPage`] 的顺序组装全部页面。
pub fn pages(cx: &App) -> Vec<SettingPage> {
    vec![
        typography_page(cx),
        layout_page(cx),
        fonts_page(cx),
        appearance_page(cx),
        language_page(cx),
        account_page(cx),
        about_page(cx),
    ]
}

/// 一页 = 标题 + 图标 + 一组（不带组标题，页面标题已经说明了一切）。
pub(super) fn page(
    title: impl Into<SharedString>,
    icon: IconName,
    items: Vec<SettingItem>,
) -> SettingPage {
    SettingPage::new(title)
        .icon(icon)
        .default_open(true)
        .groups(vec![SettingGroup::new().items(items)])
}

/// 一个数值字段的规格：标签键、说明键、搜索关键词，以及可填范围与步长。
///
/// 范围必须在场，因为 `NumberFieldOptions` 的夹取是这个控件唯一的越界防线 ——
/// 面板里填进 9000 号字会把排版算成零高或无限宽。
pub(super) struct NumberSpec {
    pub title: &'static str,
    pub description: &'static str,
    pub keywords: &'static [&'static str],
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

/// 数值字段。读写两端都过 [`ConfigHolder`]，所以赋值即规整、即落盘、即重排。
pub(super) fn number(
    cx: &App,
    spec: NumberSpec,
    get: fn(&App) -> f32,
    set: fn(f32, &mut App),
) -> SettingItem {
    SettingItem::new(
        tr(cx, spec.title),
        SettingField::number_input(
            NumberFieldOptions {
                min: spec.min,
                max: spec.max,
                step: spec.step,
            },
            move |cx: &App| f64::from(get(cx)),
            move |val: f64, cx| set(val as f32, cx),
        ),
    )
    .description(tr(cx, spec.description))
    .keywords(spec.keywords.iter().map(|word| (*word).to_string()))
}
