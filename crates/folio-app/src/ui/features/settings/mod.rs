//! 阅读设置的页面集：每页一个文件，`mod.rs` 只管**顺序**与**共用前言**。
//!
//! 分层照的是框架里其它应用的做法（一个 `settings/` 目录、一页一个文件、`mod.rs`
//! 把组件与配置入口一次性 `pub(super) use` 出去，页面文件只写 `use super::*`）。
//! 这样加一页只需要动两处：新文件 + [`SettingsPage`] 里一个变体，
//! 而不是在一个越来越长的文件里找位置。
//!
//! 页面文件产出的是 [`spec`] 里的中立规格 —— 两端共用的唯一数据源：桌面在
//! [`pages`] 里映射回 `Settings` 组件的数据形状，手机（[`compact`]）直接渲染。
//! 挂到哪儿是 [`view`] 的事 —— 这一组页面是主窗口里的一屏（桌面把它当作
//! 「我的」那一个目的地），理由写在那个子模块里。
//!
//! 字段的取值赋值都在规格里钉成函数指针，两端都通过 [`ConfigHolder`]
//! 触碰同一个实体，所以改完立刻反映到已打开的书上并落盘。

mod about;
mod appearance;
mod compact;
mod fonts;
mod layout;
mod library;
mod spec;
mod toc;
mod typography;
mod view;

// 「我的」目的地的两端视图。桌面画一整页字段（[`SettingsView`]），
// 手机画钻入式的紧凑列表（[`CompactSettings`]）—— 数据是同一份规格，
// 两份界面只是同一个配置的两个面子。
pub(crate) use compact::CompactSettings;
pub use view::SettingsView;

// 页面文件之间共用的前言：规格构造器、主题、配置入口与文案函数。
pub(super) use folio_core::{Mode, PageTurn};
pub(super) use gpui_kit::assets::IconName;
pub(super) use gpui_kit::component::ThemeMode;
pub(super) use gpui_kit::{App, SharedString};
pub(super) use spec::{Group, NumberSpec, Page, Row, group, number, page};

pub(super) use crate::ui::services::config::ConfigHolder;
pub(super) use crate::ui::services::l10n::{tf, tr};
pub(super) use crate::ui::foundation::theme;

// 那颗「打开所在文件夹」的按钮要用的几样（页面文件通过 `super::` 拿到它本身）。
use gpui_kit::IntoElement;
use gpui_kit::component::button::Button;
use std::path::PathBuf;

use crate::ui::platform;

// 每一页的构造函数只在 [`spec_pages`] 里用，所以是私有导入而不是再导出。
use about::about_page;
use appearance::appearance_page;
use fonts::fonts_page;
use layout::layout_page;
use library::library_page;
use toc::toc_page;
use typography::typography_page;

/// 「打开所在文件夹」那颗按钮：设置页两格共用（配置文件在哪儿、自定义主题往哪个目录放
/// JSON）。两格的动作一模一样，所以只写这一颗，别在两个文件里各拼一份。
///
/// 打不开时**不弹任何东西**：那一行本身还留着路径，读者能自己抄；摆一个报错弹窗只是把
/// 「这台设备的文件管理器不认领这个 URI」变成读者的问题。
pub(super) fn open_folder_button(id: &'static str, path: PathBuf, cx: &App) -> impl IntoElement {
    Button::new(id)
        .label(tr(cx, "field.open_folder").to_string())
        .on_click(move |_, _, _| {
            if !platform::open_folder(&path) {
                tracing::warn!("设置：没有应用接手打开 {path:?}");
            }
        })
}

/// 各设置页。变体顺序就是 `Settings` 侧栏里从上到下的顺序，
/// [`SettingsPage::index`] 靠它定位，所以**新增变体只能加在末尾**。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsPage {
    About,
    Appearance,
    Typography,
    Layout,
    Fonts,
    Library,
    Toc,
}

impl SettingsPage {
    /// 全部页面，按导航顺序。
    pub const ALL: [SettingsPage; 7] = [
        SettingsPage::About,
        SettingsPage::Appearance,
        SettingsPage::Typography,
        SettingsPage::Layout,
        SettingsPage::Fonts,
        SettingsPage::Library,
        SettingsPage::Toc,
    ];

    /// 在 `Settings` 组件里的页序下标。必须与 [`spec_pages`] 的组装顺序一致 ——
    /// 「从阅读页进来落在文字那一页」全靠这个下标定位。
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|page| *page == self).unwrap_or(0)
    }
}

/// 两端共用的页面规格。手机（[`compact`]）直接渲染这一份，桌面经 [`pages`] 映射。
pub(super) fn spec_pages(cx: &App) -> Vec<spec::Page> {
    vec![
        about_page(cx),
        appearance_page(cx),
        typography_page(cx),
        layout_page(cx),
        fonts_page(cx),
        library_page(cx),
        toc_page(cx),
    ]
}

/// 桌面的整页字段：把规格映射回 `Settings` 组件的数据形状。
pub(super) fn pages(cx: &App) -> Vec<SettingPage> {
    spec_pages(cx).iter().map(setting_page).collect()
}

use gpui_kit::component::h_flex;
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
};
use gpui_kit::{ParentElement, Styled, div};

/// 一页规格 → 组件的一页。**分组必须带标题**：一页只有一组时侧栏不列子项，
/// 一页有多组时子项就是这些标题，没有标题的那一组等于在导航里消失。
///
/// `default_open(false)`：侧栏默认只列页名，各页的分组子项折起来 —— 全摊开时六页几十行
/// 子项，找一页得先穿过一屏标题树。点页名照样展开并跳过去（`Settings` 组件里的
/// `click_to_open` 恒开），折着不影响到达。
///
/// `resettable(false)`：这一页的每一项都是写入即生效的，控件里也没有「默认值」那份信息，
/// 所以页头不该出现一颗「恢复默认」的按钮。
fn setting_page(page: &spec::Page) -> SettingPage {
    SettingPage::new(page.title.clone())
        .icon(page.icon)
        .resettable(false)
        .default_open(false)
        .groups(page.groups.iter().map(setting_group))
}

fn setting_group(group: &spec::Group) -> SettingGroup {
    SettingGroup::new()
        .title(group.title.clone())
        .items(group.rows.iter().map(setting_item))
}

/// 一行规格 → 组件的一项。每种控件映射到组件的同形字段；`Custom` 行按有没有标题
/// 走两条构造路 —— 有标题的行桌面要画「标题在左、内容在右」。
///
/// 规格里装箱的闭包在这里**再包一层**送进组件：组件要的是 `'static` 的 `Fn`，
/// 而我们手上是 `&Rc<dyn Fn>`，克隆出拥有权才能交出去。
fn setting_item(row: &spec::Row) -> SettingItem {
    use spec::RowKind;
    let item = match &row.kind {
        RowKind::Toggle { get, set } => {
            let (get, set) = (get.clone(), set.clone());
            SettingItem::new(
                row.title.clone(),
                SettingField::switch(move |cx| get(cx), move |val, cx| set(val, cx)),
            )
        }
        // `push` 是手机端怎么交这一格，桌面的 `Select` 不受影响，所以这里不读它。
        RowKind::Dropdown {
            options,
            scrollable,
            get,
            set,
            ..
        } => {
            let options = options.as_ref().clone();
            let (get, set) = (get.clone(), set.clone());
            let field = if *scrollable {
                SettingField::scrollable_dropdown(
                    options,
                    move |cx| get(cx),
                    move |val, cx| set(val, cx),
                )
            } else {
                SettingField::dropdown(options, move |cx| get(cx), move |val, cx| set(val, cx))
            };
            SettingItem::new(row.title.clone(), field)
        }
        RowKind::Number { spec, get, set } => {
            let (get, set) = (get.clone(), set.clone());
            SettingItem::new(
                row.title.clone(),
                SettingField::number_input(
                    NumberFieldOptions {
                        min: f64::from(spec.min),
                        max: f64::from(spec.max),
                        step: f64::from(spec.step),
                    },
                    // 组件的数值字段是 f64 的，配置里的排版量是 f32：换算只发生在这一处边界。
                    move |cx| f64::from(get(cx)),
                    move |val, cx| set(val as f32, cx),
                ),
            )
        }
        RowKind::Text { get, set } => {
            let (get, set) = (get.clone(), set.clone());
            SettingItem::new(
                row.title.clone(),
                SettingField::input(move |cx| get(cx), move |val, cx| set(val, cx)),
            )
        }
        RowKind::Info { value } => {
            let value = value.clone();
            SettingItem::new(
                row.title.clone(),
                SettingField::render(move |_, _, _| {
                    h_flex()
                        .w_full()
                        .justify_end()
                        .child(div().text_sm().child(value.clone()))
                }),
            )
        }
        RowKind::Custom { title, render } => match title {
            Some(title) => {
                let render = render.clone();
                SettingItem::new(
                    title.clone(),
                    SettingField::render(move |_, _, cx| render(cx)),
                )
            }
            None => {
                let render = render.clone();
                SettingItem::render(move |_, _, cx| render(cx))
            }
        },
    };
    let item = match &row.desc {
        Some(desc) => item.description(desc.clone()),
        None => item,
    };
    item.keywords(row.keywords.iter().map(|word| (*word).to_string()))
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
