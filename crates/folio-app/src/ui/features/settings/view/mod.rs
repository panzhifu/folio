//! 阅读设置的页面集：每页一个文件，`mod.rs` 只管**顺序**与**共用前言**。
//!
//! 分层照的是框架里其它应用的做法（一个 `settings/` 目录、一页一个文件、`mod.rs`
//! 把组件与配置入口一次性 `pub(in crate::ui::features::settings) use` 出去，页面文件只写 `use super::*`）。
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

mod compact;
mod desktop;
mod notes_sheet;
mod pages;

// 「我的」目的地的两端视图。桌面画一整页字段（[`SettingsView`]），
// 手机画钻入式的紧凑列表（[`CompactSettings`]）—— 数据是同一份规格，
// 两份界面只是同一个配置的两个面子。
pub(crate) use compact::CompactSettings;
pub use desktop::SettingsView;

// 「更新日志」那张卡片：页面规格里的那颗按钮要拿它的句柄，两屏各自持有一份并挂在自己的
// `render` 里（一份宿主而不是全进程共用一份，理由写在 `notes_sheet` 顶部）。
pub(in crate::ui::features::settings) use notes_sheet::NotesSheet;

// 页面文件之间共用的前言：规格构造器、主题、配置入口与文案函数。
pub(in crate::ui::features::settings) use crate::ui::features::settings::model::spec;
pub(in crate::ui::features::settings) use crate::ui::features::settings::model::spec::{
    Group, NumberSpec, Page, Row, group, number, page,
};
pub(in crate::ui::features::settings) use folio_core::{
    Appearance, ImageScale, Mode, PageTurn, Reader, TextDirection,
};
pub(in crate::ui::features::settings) use gpui_kit::assets::IconName;
pub(in crate::ui::features::settings) use gpui_kit::component::ThemeMode;
pub(in crate::ui::features::settings) use gpui_kit::{App, Entity, SharedString};

pub(in crate::ui::features::settings) use crate::ui::foundation::theme;
pub(in crate::ui::features::settings) use crate::ui::services::config::ConfigHolder;
pub(in crate::ui::features::settings) use crate::ui::services::l10n::{tf, tr};

// 那颗「打开所在文件夹」的按钮要用的几样（页面文件通过 `super::` 拿到它本身）。
use gpui_kit::component::button::Button;
use gpui_kit::{ElementId, IntoElement};
use std::path::PathBuf;
use std::rc::Rc;

use crate::ui::components::FontPicker;
use crate::ui::platform;

// 每一页的构造函数只在 [`spec_pages`] 里用，所以是私有导入而不是再导出。
use pages::about::about_page;
use pages::ai::ai_page;
use pages::appearance::appearance_page;
use pages::fonts::fonts_page;
use pages::layout::layout_page;
use pages::library::library_page;
use pages::membership::membership_page;
use pages::speech::speech_page;
use pages::storage::storage_page;
use pages::sync::sync_page;
use pages::toc::toc_page;
use pages::translation::translation_page;
use pages::txt_rules::txt_rules_page;
use pages::typography::typography_page;

/// 「打开所在文件夹」那颗按钮：设置页两格共用（配置文件在哪儿、自定义主题往哪个目录放
/// JSON）。两格的动作一模一样，所以只写这一颗，别在两个文件里各拼一份。
///
/// 打不开时**不弹任何东西**：那一行本身还留着路径，读者能自己抄；摆一个报错弹窗只是把
/// 「这台设备的文件管理器不认领这个 URI」变成读者的问题。
pub(in crate::ui::features::settings) fn open_folder_button(
    id: impl Into<ElementId>,
    path: PathBuf,
    cx: &App,
) -> impl IntoElement {
    Button::new(id)
        .label(tr(cx, "field.open_folder").to_string())
        .on_click(move |_, _, _| {
            if !platform::open_folder(&path) {
                tracing::warn!("设置：没有应用接手打开 {path:?}");
            }
        })
}

/// 各设置页。变体顺序就是 `Settings` 侧栏里从上到下的顺序，
/// [`SettingsPage::index`] 靠它现算，所以新增变体插在导航里该在的位置，
/// 并与 [`ALL`](Self::ALL)、[`spec_pages`] 一起动。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsPage {
    Membership,
    Appearance,
    Typography,
    Layout,
    Fonts,
    Speech,
    Translation,
    Ai,
    Sync,
    Library,
    TxtRules,
    Toc,
    Storage,
    About,
}

impl SettingsPage {
    /// 全部页面，按导航顺序。
    ///
    /// `About` 排在末尾是刻意的：这一页不改任何东西，是读者**办完事顺手看一眼**的东西，
    /// 摆在最上面等于让每天要调的那几页往下挪一格。
    ///
    /// `Membership` 排在最上面（2026-11-03 这一版定的）。这一格从前紧挨 `About`，理由是
    /// 「一天不会调两次会员」；把它抬到第一格的是产品口径不是使用频次 —— 会员是**要不要开通**
    /// 的那个决定，读者找它时不该先扫过十来格才看见。手机端不受这条影响：那一屏的会员是
    /// 根顶上单独一张卡（当场念等级与到期日），入口列表按 [`SettingsPage::index`] 把它跳过
    /// （见 `compact.rs` 的 `root_screen`），所以置顶在手机上只是列表里少一格，画面不变。
    ///
    /// 变体顺序就是侧栏顺序，而深链靠 [`index`](Self::index) 现算 —— 挪位置不会指错页，
    /// 但会让「第 N 页」这句话在别处（测试、日志、旧截图）对不上，所以三处顺序
    /// （本枚举、`ALL`、[`spec_pages`]）必须一起动；用例里也别写死的序号，要按页名取。
    pub const ALL: [SettingsPage; 14] = [
        SettingsPage::Membership,
        SettingsPage::Appearance,
        SettingsPage::Typography,
        SettingsPage::Layout,
        SettingsPage::Fonts,
        SettingsPage::Speech,
        SettingsPage::Translation,
        SettingsPage::Ai,
        SettingsPage::Sync,
        SettingsPage::Library,
        SettingsPage::TxtRules,
        SettingsPage::Toc,
        SettingsPage::Storage,
        SettingsPage::About,
    ];

    /// 在 `Settings` 组件里的页序下标。必须与 [`spec_pages`] 的组装顺序一致 ——
    /// 「从阅读页进来落在文字那一页」全靠这个下标定位。
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|page| *page == self).unwrap_or(0)
    }
}

/// 两端共用的页面规格。手机（[`compact`]）直接渲染这一份，桌面经 [`pages`] 映射。
///
/// 顺序与 [`SettingsPage::ALL`] 一格对一格 —— 那一头的下标就是从这份 Vec 的位置来的。
///
/// `notes` 是**调用那一屏自己的**「更新日志」卡片句柄：只有「关于」那一页用它，而那一页的
/// 每一格是在渲染时才跑取值闭包的，规格里没法事后去找「我挂在哪一屏上」。
pub(in crate::ui::features::settings) fn spec_pages(
    cx: &App,
    notes: &Entity<NotesSheet>,
) -> Vec<spec::Page> {
    vec![
        membership_page(cx),
        appearance_page(cx),
        typography_page(cx),
        layout_page(cx),
        fonts_page(cx),
        speech_page(cx),
        translation_page(cx),
        ai_page(cx),
        sync_page(cx),
        library_page(cx),
        txt_rules_page(cx),
        toc_page(cx),
        storage_page(cx),
        about_page(cx, notes),
    ]
}

/// 桌面的整页字段：把规格映射回 `Settings` 组件的数据形状。
pub(in crate::ui::features::settings) fn pages(
    cx: &App,
    notes: &Entity<NotesSheet>,
) -> Vec<SettingPage> {
    spec_pages(cx, notes).iter().map(setting_page).collect()
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
        RowKind::Font { get, set } => {
            let (get, set) = (get.clone(), set.clone());
            SettingItem::new(
                row.title.clone(),
                SettingField::render(move |_, window, cx| {
                    let get = get.clone();
                    let set = set.clone();
                    let picker =
                        window.use_keyed_state("settings-font-picker", cx, move |window, cx| {
                            let get_value = get.clone();
                            let set_value = set.clone();
                            FontPicker::new(
                                Rc::new(move |cx: &App| get_value(cx).to_string()),
                                Rc::new(move |value: String, cx: &mut App| {
                                    set_value(SharedString::from(value), cx)
                                }),
                                window,
                                cx,
                            )
                        });
                    h_flex()
                        .w_full()
                        .justify_end()
                        .child(div().w_64().child(picker.clone()))
                }),
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
    use gpui_kit::{AppContext as _, TestAppContext};

    /// 「关于」那一页要一张「更新日志」卡片的句柄：测试里不挂窗，只组规格，所以就地建一份
    /// 拿着当参数（每屏自己那一份由各视图在构造时建）。
    fn card(cx: &mut App) -> Entity<NotesSheet> {
        cx.new(NotesSheet::new)
    }

    /// 回归门：加一页时只动了页面文件、忘了在 [`SettingsPage`] 里补变体，
    /// 侧栏就会多出一格没有下标可指的页 —— 深链会跳到别处去。两份数量必须相等。
    #[gpui_kit::test]
    fn the_page_set_and_the_navigation_list_stay_in_step(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let notes = card(cx);
            assert_eq!(
                pages(cx, &notes).len(),
                SettingsPage::ALL.len(),
                "`pages` 组出来的页数与 `SettingsPage::ALL` 对不上"
            );
        });
    }

    /// 「会员」那一页在两份顺序里必须是同一格。
    ///
    /// 手机端根屏那张卡片是按**序号**把会员从入口列表里跳掉的（`compact.rs` 的 `root_screen`），
    /// 序号来自 [`SettingsPage::index`]，列表来自 [`spec_pages`] —— 两份一旦错开，跳掉的就是
    /// 别的页，而会员同时在列表里出现两次。上面那条只数得出**数量**对不对。
    #[gpui_kit::test]
    fn the_membership_page_sits_where_its_index_points(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let notes = card(cx);
            let pages = spec_pages(cx, &notes);
            let ix = SettingsPage::Membership.index();
            assert_eq!(
                pages[ix].title,
                tr(cx, "page.membership"),
                "`SettingsPage::ALL` 里会员的位置与 `spec_pages` 组装的顺序错开了"
            );
            // 置顶本身也是钉住的东西：这是 2026-11-03 定下的产品位置（要开通的那一格摆在
            // 第一眼看得到处），谁顺手把它挪回末尾都不该没人说。
            assert_eq!(
                ix, 0,
                "会员现在是侧栏第一格；真要把每天调的那几页提到它前面，得先改掉这条"
            );
        });
    }

    /// 「这台设备」那一组只在**这台激活过**之后出现：没激活过时摆出来只会是一句「没有留痕」
    /// 加一颗按不动的钮。这一屏的解除是**按设备**的，所以界面上不该出现「先交订单号才能解除」
    /// 那种要求 —— 本机从来没存过那一串，而最需要解除的人正好是手里没有它的人（重装过的那位）。
    #[gpui_kit::test]
    fn the_membership_page_carries_the_device_group_once_activated(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let notes = card(cx);
            let devices = tr(cx, "group.membership_devices");
            let titles = |cx: &App| -> Vec<SharedString> {
                spec_pages(cx, &notes)[SettingsPage::Membership.index()]
                    .groups
                    .iter()
                    .map(|group| group.title.clone())
                    .collect()
            };

            // 从没开过：那一组不该在。
            assert!(
                !titles(cx).contains(&devices),
                "没激活过的这台没有可管的设备，组标题：{:?}",
                titles(cx)
            );

            // 记一笔到账之后它自己出现，而且第一行是设备标识、最后一格摆解除的钮。
            ConfigHolder::edit_membership(cx, |membership| {
                membership.record("2".repeat(29), 30, 20_000)
            });
            assert!(
                titles(cx).contains(&devices),
                "激活过就该有「这台设备」那一组，组标题：{:?}",
                titles(cx)
            );
            let pages = spec_pages(cx, &notes);
            let page = &pages[SettingsPage::Membership.index()];
            let group = page
                .groups
                .iter()
                .find(|group| group.title == devices)
                .expect("上面刚断言过它在");
            assert!(
                group
                    .rows
                    .iter()
                    .any(|row| row.title == tr(cx, "membership.device_id")),
                "那一组第一行该是设备标识"
            );
            // 解除那一格是自定义行（没有标题位）：摆的是「此刻那句状态 + 那颗钮」。
            assert!(
                group.rows.iter().any(|row| matches!(
                    &row.kind,
                    spec::RowKind::Custom { title, .. } if title.is_none()
                )),
                "那一组该有一格摆解除的钮"
            );

            // 解除干净之后它该跟着没：留着就是一颗按不动的钮。
            ConfigHolder::edit_membership(cx, |membership| membership.clear_activation());
            assert!(
                !titles(cx).contains(&devices),
                "清零后台账与权益都没了，那一组该跟着退场"
            );
        });
    }

    /// 翻译页只在选中那一档时摆引擎私有参数：Google 免 key 没有可填的，切到
    /// OpenAI / DeepL 各多出一组。组排在最后，前面两行的下标不随引擎切换而变。
    ///
    /// 「AI 开着就把 OpenAI 那三格也长出来」那条权宜已经撤了 —— AI 有自己的一屏
    /// （`pages/ai.rs`），所以这一屏的组数只该跟着**翻译用的引擎**走。最后一句钉的就是这件事。
    #[gpui_kit::test]
    fn the_translation_page_reveals_only_the_selected_engines_settings(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let index = SettingsPage::Translation.index();
            let notes = card(cx);
            let titles = |cx: &App| {
                spec_pages(cx, &notes)[index]
                    .groups
                    .iter()
                    .map(|group| group.title.clone())
                    .collect::<Vec<_>>()
            };

            let google = spec_pages(cx, &notes);
            let page = &google[index];
            assert_eq!(page.title, tr(cx, "page.translation"));
            assert_eq!(
                titles(cx),
                vec![tr(cx, "group.translation_output")],
                "Google 免 key 没有私有参数，这一屏就只有「译成 + 引擎」那一组"
            );
            assert_eq!(page.groups[0].rows.len(), 2, "译成 + 引擎");

            ConfigHolder::edit_translation(cx, |translation| {
                translation.engine = folio_core::TranslationEngine::OpenAi
            });
            let page = &spec_pages(cx, &notes)[index];
            assert_eq!(page.groups.len(), 2);
            assert_eq!(page.groups[1].title, tr(cx, "group.translation_openai"));
            assert_eq!(page.groups[1].rows.len(), 3, "地址 / 模型 / Key");

            ConfigHolder::edit_translation(cx, |translation| {
                translation.engine = folio_core::TranslationEngine::DeepL
            });
            let page = &spec_pages(cx, &notes)[index];
            assert_eq!(page.groups.len(), 2);
            assert_eq!(page.groups[1].title, tr(cx, "group.translation_deepl"));
            assert_eq!(page.groups[1].rows.len(), 1, "只有 Key");

            // 翻译用 DeepL 而 AI 开着：这一屏**不该**长出 OpenAI 那三格了。它们有自己的
            // 落点（AI 屏），而挂在别人屏末尾的那一组，读者找不到的症状就是「功能没做」。
            ConfigHolder::edit_ai(cx, |ai| ai.enabled = true);
            assert_eq!(
                titles(cx),
                vec![
                    tr(cx, "group.translation_output"),
                    tr(cx, "group.translation_deepl"),
                ],
                "AI 那一组已经从翻译屏搬走，这里不许因为它开着就多长一组"
            );
        });
    }

    /// 「AI」那一屏固定两组：AI 自己的两格（要不要 / 问哪只模型）＋ 与翻译共用的端点三格。
    ///
    /// 组数与标题都钉住：端点那一组少了，读者就没有地方填地址与密钥，而划词卡片上那一格
    /// 是按「端点填全了」才出现的 —— 缺落点的症状是「开关拨了却没入口」。
    #[gpui_kit::test]
    fn the_ai_page_carries_the_switch_and_the_shared_endpoint(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let index = SettingsPage::Ai.index();
            let notes = card(cx);
            let page = &spec_pages(cx, &notes)[index];
            assert_eq!(page.title, tr(cx, "page.ai"));
            assert_eq!(
                page.groups
                    .iter()
                    .map(|group| group.title.clone())
                    .collect::<Vec<_>>(),
                vec![tr(cx, "group.ai"), tr(cx, "group.ai_endpoint")],
                "两组：AI 自己的一组与那份共用端点"
            );
            assert_eq!(page.groups[0].rows.len(), 2, "开关 + AI 用的模型");
            assert_eq!(page.groups[1].rows.len(), 3, "地址 / 模型 / Key");

            // 端点那一组与翻译引擎无关：翻译用 Google（免 key）时 AI 也要能填自己的端点。
            let again = spec_pages(cx, &notes);
            assert_eq!(again[index].groups.len(), 2);

            // 页数与导航对得上：这一屏插在翻译之后，深链靠 index 现算。
            assert!(SettingsPage::Ai.index() > SettingsPage::Translation.index());
        });
    }

    /// 同步那一屏固定三组：WebDAV 的字段、WebDAV 的状态与两颗钮、然后是局域网那一整组。
    ///
    /// 钉的是**形状与顺序**：局域网那一组少了，读者就没有地方填那串要抄到对面去的密钥；而它
    /// 排在**最后**，因为读者熟悉的「测试连接 / 立即同步」一往下挪，在放不下整页的窗口里就
    /// 落到折叠线以下 —— 症状是「按钮不见了」。「有密钥」与「没有密钥」之间行数不该变：变的
    /// 是校验码那一行的内容（没有密钥时那里是那颗生成钮，有了之后那颗钮必须消失，再留着就是
    /// 递给读者一把能把自己从账号里摘出去的东西）。整趟往返的门在 `folio_core::net::lansync`
    /// 与 `net::transfer::lan` 那两条用例里，这里不重复。
    #[gpui_kit::test]
    fn the_sync_page_carries_both_routes_with_the_local_one_last(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let notes = card(cx);
            let index = SettingsPage::Sync.index();

            let page = &spec_pages(cx, &notes)[index];
            assert_eq!(page.title, tr(cx, "page.sync"));
            assert_eq!(page.groups.len(), 3, "WebDAV / 状态 / 局域网");
            assert_eq!(page.groups[0].title, tr(cx, "group.sync_server"));
            assert_eq!(
                page.groups[0].rows.len(),
                5,
                "WebDAV 那一组不该被新增的一组挤掉任何一行：开关 / 地址 / 账号 / 密码 / 目录"
            );
            // 状态那一组紧跟 WebDAV 的字段：读者熟悉的「测试连接 / 立即同步」不该往下挪。
            assert_eq!(page.groups[1].title, tr(cx, "group.sync_state"));
            assert_eq!(page.groups[2].title, tr(cx, "group.sync_lan"));
            assert_eq!(
                page.groups[2].rows.len(),
                5,
                "开关 / 校验码 / 密钥 / 对端地址 / 状态"
            );

            ConfigHolder::edit_account(cx, |account| {
                account.enabled = true;
                account.key = "0123456789abcdef0123456789abcdef".into();
                account.peer = "192.168.1.5:8410".into();
            });
            let page = &spec_pages(cx, &notes)[index];
            assert_eq!(
                page.groups[2].rows.len(),
                5,
                "填上密钥不该让这一组多一行或少一行"
            );
            assert_eq!(page.groups.len(), 3, "两边都在屏上，不是一家独大");
        });
    }

    /// TXT 分章规则那一屏的**行数规矩**：出厂那一组不随开关变少（列里没了就没法再打开），
    /// 阈值那一组永远五格，自定义那一组只往尾上加。
    ///
    /// 两端的输入框状态按（页、组、行）键控，所以这三条破任何一条，读者正打字的那一格就会
    /// 错位到别的规则上去 —— 症状是名字填进了另一条规则，那是最难从截图看出来的错。
    #[gpui_kit::test]
    fn the_rules_page_keeps_its_rows_while_the_reader_edits(cx: &mut TestAppContext) {
        use folio_core::content::format::novel::rules;

        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let notes = card(cx);
            let index = SettingsPage::TxtRules.index();
            let counts = |cx: &App| {
                spec_pages(cx, &notes)[index]
                    .groups
                    .iter()
                    .map(|group| group.rows.len())
                    .collect::<Vec<_>>()
            };
            let before = counts(cx);
            assert_eq!(
                before.len(),
                4,
                "四组：出厂规则 / 阈值 / 这张表 / 我加的规则"
            );
            assert_eq!(
                before[0],
                rules::builtin_table().markers.len(),
                "出厂那十几条该一条不落全摆出来"
            );
            assert_eq!(
                before[1], 5,
                "阈值五格：最少章节、章名最长、中位最短、目录连排、目录间隔"
            );
            assert_eq!(
                before[3], 1,
                "还没有自定义条目时，那一组只剩「加一条」那颗钮"
            );

            ConfigHolder::edit_novel(cx, |novel| novel.edits.off.push("番外".to_string()));
            assert_eq!(
                counts(cx),
                before,
                "关掉一条不许让那一组少一行，否则再也打不开它"
            );

            ConfigHolder::edit_novel(cx, |novel| {
                novel.edits.extra.push(rules::Marker {
                    name: "幕".to_string(),
                    kind: rules::MatchKind::Literal,
                    pattern: None,
                    text: Some("幕".to_string()),
                    level: 1,
                    numbering: rules::NumberingStyle::Either,
                    examples: Default::default(),
                });
            });
            let after = counts(cx);
            assert_eq!(
                &after[..3],
                &before[..3],
                "自定义条目只进最后一组：插在前面会把正打字那一格错位到别的规则上"
            );
            assert_eq!(
                after[3],
                before[3] + 6,
                "一条自定义规则该有六格：开关 / 名字 / 怎么匹配 / 模式 / 编号怎么读 / 删除"
            );
        });
    }

    /// 阈值填成**与出厂那个数一样**时，配置里不该留下这条改动。
    ///
    /// 差量里存一条与出厂相同的值没有任何作用，却会让以后出厂调那一格时被这份旧数盖回去 ——
    /// 那一格从此对这位读者永久失效，而屏上看着是他自己填的。
    #[gpui_kit::test]
    fn a_gate_value_equal_to_the_shipped_one_is_not_recorded(cx: &mut TestAppContext) {
        use folio_core::content::format::novel::rules;

        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            let notes = card(cx);
            let title = tr(cx, "field.rules_min_chapters");
            let shipped = rules::builtin_table().gates.min_chapters as f32;
            let write = |cx: &mut App, value: f32| {
                let pages = spec_pages(cx, &notes);
                let page = &pages[SettingsPage::TxtRules.index()];
                for row in page.groups.iter().flat_map(|group| &group.rows) {
                    if row.title == title {
                        let spec::RowKind::Number { set, .. } = &row.kind else {
                            panic!("这一格该是数值行");
                        };
                        set(value, cx);
                        return;
                    }
                }
                panic!("阈值那一组里找不着「{title}」这一行");
            };

            write(cx, shipped);
            assert_eq!(
                ConfigHolder::novel(cx).edits.gates.min_chapters,
                None,
                "填回出厂那个数不该在差量里留一条改动"
            );

            write(cx, shipped + 4.0);
            assert_eq!(
                ConfigHolder::novel(cx).edits.gates.min_chapters,
                Some(shipped as usize + 4),
                "真的改了数就该记下来，而且记的是**新值**不是差值"
            );
        });
    }
}
