//! 阅读设置的页面集：每页一个文件，`mod.rs` 只管**顺序**与**共用前言**。
//!
//! 分层照的是框架里其它应用的做法（一个 `settings/` 目录、一页一个文件、`mod.rs`
//! 把组件与配置入口一次性 `pub(super) use` 出去，页面文件只写 `use super::*`）。
//! 这样加一页只需要动两处：新文件 + [`SettingsPage`] 里一个变体，
//! 而不是在一个越来越长的文件里找位置。
//!
//! 这里只负责**描述**每一页：标题、分组、控件、取值与赋值。挂到哪儿是 [`view`] 的事 ——
//! 这一组页面是主窗口里的一屏（桌面把它当作「我的」那一个目的地），理由写在那个子模块里。
//!
//! 字段一律走 `SettingField` 的 (取值, 赋值) 对，两端都通过 [`ConfigHolder`]
//! 触碰同一个实体，所以改完立刻反映到已打开的书上并落盘。

mod about;
mod account;
pub mod account_view;
mod appearance;
mod fonts;
mod layout;
mod library;
mod typography;
mod view;

// 「我的」目的地（Account section）的根视图。它与设置页是同一件事的两半：
// 桌面直接画这一组页面，手机那一屏仍是行列表（放不进那块小屏），
// 所以收在同一个 feature 下。
pub use account_view::AccountView;
pub use view::SettingsView;

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

// 每一页的构造函数只在 [`pages`] 里用，所以是私有导入而不是再导出。
use about::about_page;
use account::account_page;
use appearance::appearance_page;
use fonts::fonts_page;
use layout::layout_page;
use library::library_page;
use typography::typography_page;

/// 各设置页。变体顺序就是 `Settings` 侧栏里从上到下的顺序，
/// [`SettingsPage::index`] 靠它定位，所以**新增变体只能加在末尾**。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsPage {
    Typography,
    Layout,
    Fonts,
    Appearance,
    Account,
    About,
    Library,
}

impl SettingsPage {
    /// 全部页面，按导航顺序。
    pub const ALL: [SettingsPage; 7] = [
        SettingsPage::Typography,
        SettingsPage::Layout,
        SettingsPage::Fonts,
        SettingsPage::Appearance,
        SettingsPage::Account,
        SettingsPage::About,
        SettingsPage::Library,
    ];

    /// 在 `Settings` 组件里的页序下标。必须与 [`pages`] 的组装顺序一致 ——
    /// 「从阅读页进来落在文字那一页」全靠这个下标定位。
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|page| *page == self).unwrap_or(0)
    }
}

/// 按 [`SettingsPage`] 的顺序组装全部页面。
pub(super) fn pages(cx: &App) -> Vec<SettingPage> {
    vec![
        typography_page(cx),
        layout_page(cx),
        fonts_page(cx),
        appearance_page(cx),
        account_page(cx),
        about_page(cx),
        library_page(cx),
    ]
}

/// 一页 = 标题 + 图标 + 若干**带标题的分组**。
///
/// 分组必须带标题：一页只有一组时侧栏不列子项，一页有多组时子项就是这些标题，
/// 没有标题的那一组等于在导航里消失。
///
/// `resettable(false)`：这一页的每一项都是写入即生效的，控件里也没有「默认值」那份信息，
/// 所以页头不该出现一颗「恢复默认」的按钮。
pub(super) fn page(
    title: impl Into<SharedString>,
    icon: IconName,
    groups: Vec<SettingGroup>,
) -> SettingPage {
    SettingPage::new(title)
        .icon(icon)
        .resettable(false)
        .default_open(true)
        .groups(groups)
}

/// 一组 = 标题 + 若干项。
pub(super) fn group(title: impl Into<SharedString>, items: Vec<SettingItem>) -> SettingGroup {
    SettingGroup::new().title(title).items(items)
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

#[cfg(test)]
mod tests {
    //! 这一层能被钉住的是**两份顺序是不是同一份**：页面由 [`pages`] 组装，
    //! 而「从阅读页进来落在文字那一页」用的是 [`SettingsPage::index`] 数出来的下标。

    use super::*;
    use folio_core::Config;
    use gpui_kit::TestAppContext;

    /// 回归门：加一页时只动了页面文件、忘了在 [`SettingsPage`] 里补变体，
    /// 侧栏就会多出一格没有下标可指的页 —— 深链会跳到别处去。两份数量必须相等。
    #[gpui_kit::test]
    fn the_page_set_and_the_navigation_list_stay_in_step(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            assert_eq!(
                pages(cx).len(),
                SettingsPage::ALL.len(),
                "`pages` 组出来的页数与 `SettingsPage::ALL` 对不上"
            );
        });
    }
}
