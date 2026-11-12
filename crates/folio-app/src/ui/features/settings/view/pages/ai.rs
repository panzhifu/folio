//! AI：划词「问 AI」与 `.txt`「建议切章」共用的那一屏 —— 开不开、问哪条供应商、
//! 用哪只模型，以及供应商档案本身的管理。
//!
//! ## 供应商注册表住在这里
//!
//! 地址与密钥填一次存一条档案（`Config.vendors`），翻译与 AI 各自引用其中一条、
//! 各配各的模型 —— 换供应商只动档案那一处，不必把翻译屏整屏重填，翻译与 AI 也不再
//! 永远拴在同一个端点上。引用悬空回退第一条（`resolve_vendor` 的规则），界面在说明
//! 里说清，不用读者自己发现。
//!
//! ## 「AI 用的模型」与「推荐模型」是两格写同一格
//!
//! 模型格自由填（预设不是白名单：接入点 id、relay 名、比表新的模型都要能填）；
//! 「推荐模型」下拉把这一家的常用型号与上次「测试连接」拉回的清单摆出来，选中即写入
//! 模型格 —— 两格读写的都是 `Config.ai.model` 一份。
//!
//! ## 连接那一组是诚实的探针
//!
//! 「测试连接」拉一次 `/models`（清单到了 = 地址与密钥都对）；「问一句」发一次最小的
//! 真请求、把模型的原话摆上来 —— 只有这句话能证明对面是模型。两个动作各清各的忙线，
//! 结果互相保留。状态机与两颗钮照同步页那一组（`pages/sync.rs`）的形状。

use folio_core::net::ai::resolved_ai_endpoint;
use folio_core::{Vendor, host_of, new_vendor_id};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{Anchor, AnyElement, FontWeight, Styled as _, div};

use super::super::*;
use crate::ui::components::muted_label;
use crate::ui::services::ai::{self, Probe, VENDOR_PRESETS};

pub(in crate::ui::features::settings) fn ai_page(cx: &App) -> Page {
    // 门禁：选区问 AI 与划词翻译同一档会员。非会员整页换成引导。
    if !crate::ui::services::activation::premium(cx) {
        return page(
            tr(cx, "page.ai"),
            IconName::Sparkles,
            vec![super::membership::gate_group("membership.gate_ai", cx)],
        );
    }
    page(
        tr(cx, "page.ai"),
        IconName::Sparkles,
        vec![ask_group(cx), connect_group(cx), vendor_group(cx)],
    )
}

/// AI 这一路解析出的（供应商，模型）。探测与「推荐模型」的 host 都从它来；
/// 与 provider 的建法共用同一份解析（`resolved_ai_endpoint`），不许两套。
fn resolved(cx: &App) -> Option<(Vendor, String)> {
    resolved_ai_endpoint(
        &ConfigHolder::translation(cx),
        &ConfigHolder::ai(cx),
        &ConfigHolder::vendors(cx),
    )
}

/// AI 自己那一组：要不要用、问哪条供应商、用哪只模型。
fn ask_group(cx: &App) -> Group {
    let mut rows = vec![
        Row::toggle(
            tr(cx, "field.ai_enabled"),
            |cx| ConfigHolder::ai(cx).enabled,
            |on, cx| ConfigHolder::edit_ai(cx, |ai| ai.enabled = on),
        )
        .description(tr(cx, "field.ai_enabled.desc"))
        .keywords(&["ai", "ask", "gpt", "model", "问 ai", "人工智能", "助手"]),
        vendor_choice(cx),
        Row::text(
            tr(cx, "field.ai_model"),
            |cx| SharedString::from(ConfigHolder::ai(cx).model),
            |value, cx| ConfigHolder::edit_ai(cx, |ai| ai.model = value.to_string()),
        )
        .description(tr(cx, "field.ai_model.desc"))
        .keywords(&["ai", "model", "模型"]),
    ];
    // 「推荐模型」只在有东西可挑时摆：预设表按 host 给，探针清单按上次测试给；
    // 都没有（自建网关、没测过）就是纯自由填，摆一只空下拉只会让人疑惑。
    if let Some(choices) = model_choices(cx) {
        rows.insert(
            2,
            Row::dropdown(
                tr(cx, "field.ai_model_preset"),
                choices,
                false,
                |cx: &App| SharedString::from(ConfigHolder::ai(cx).model),
                |value: SharedString, cx| {
                    ConfigHolder::edit_ai(cx, |ai| ai.model = value.to_string())
                },
            )
            .description(tr(cx, "field.ai_model_preset.desc"))
            .keywords(&["ai", "model", "preset", "推荐", "模型"]),
        );
    }
    group(tr(cx, "group.ai"), rows)
}

/// AI 问哪条供应商：留空跟随翻译那档的选择，填了只换 AI 这一路。
fn vendor_choice(cx: &App) -> Row {
    let mut options: Vec<(SharedString, SharedString)> =
        vec![(SharedString::default(), tr(cx, "field.ai_vendor.follow"))];
    options.extend(ConfigHolder::vendors(cx).iter().map(vendor_option));
    Row::dropdown(
        tr(cx, "field.ai_vendor"),
        options,
        false,
        |cx: &App| SharedString::from(ConfigHolder::ai(cx).vendor),
        |value: SharedString, cx| ConfigHolder::edit_ai(cx, |ai| ai.vendor = value.to_string()),
    )
    .description(tr(cx, "field.ai_vendor.desc"))
    .keywords(&["ai", "vendor", "provider", "供应商", "服务"])
}

/// 一条档案在下拉里的一行：「名称 · host」。名称空了回落 host（`display_name`）。
fn vendor_option(vendor: &Vendor) -> (SharedString, SharedString) {
    let host = host_of(&vendor.base_url);
    let label = if host.is_empty() {
        vendor.display_name()
    } else {
        format!("{} · {}", vendor.display_name(), host)
    };
    (
        SharedString::from(vendor.id.clone()),
        SharedString::from(label),
    )
}

/// 「推荐模型」的候选：这一家的预设 + 上次「测试连接」拉回的清单 + 当前已填的那只
/// （在候选里就不再重复）。三处都空 ⇒ 不摆这一行。
fn model_choices(cx: &App) -> Option<Vec<(SharedString, SharedString)>> {
    let mut ids: Vec<String> = resolved(cx)
        .map(|(vendor, _)| {
            ai::model_presets(&vendor.base_url)
                .iter()
                .map(|model| model.to_string())
                .collect()
        })
        .unwrap_or_default();
    if let Some(ai::Probe::Ok {
        models: Some(got), ..
    }) = ai::handle(cx).map(|entity| entity.read(cx).probe().clone())
    {
        ids.extend(got);
    }
    let current = ConfigHolder::ai(cx).model;
    if !current.is_empty() && !ids.contains(&current) {
        ids.push(current);
    }
    ids.dedup();
    if ids.is_empty() {
        None
    } else {
        Some(
            ids.into_iter()
                .map(|id| (SharedString::from(id.clone()), SharedString::from(id)))
                .collect(),
        )
    }
}

/// 连接那一组：一行状态 + 两颗探针钮。取值闭包只在重绘时被调，探针那趟回来若不
/// `notify`，这一格会停在「正在测…」—— 状态机的通知由 `services::ai` 负责。
fn connect_group(cx: &App) -> Group {
    group(
        tr(cx, "group.ai_connect"),
        vec![
            Row::custom(move |_, cx| {
                let probe = ai::handle(cx)
                    .map(|entity| entity.read(cx).probe().clone())
                    .unwrap_or(Probe::Idle);
                let endpoint = resolved(cx);

                let mut column = v_flex().min_w_0().flex_1().gap_0p5();
                column = column.child(
                    div()
                        .text_base()
                        .text_ellipsis()
                        .child(probe_text(cx, &probe)),
                );
                if let Some(detail) = probe_detail(cx, &probe) {
                    column = column.child(muted_label(detail, cx));
                }

                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(column)
                    .child(
                        Button::new("settings-ai-test")
                            .ghost()
                            .small()
                            .label(tr(cx, "ai.action_test").to_string())
                            .disabled(probe.busy() || endpoint.is_none())
                            .on_click(move |_, _, cx| {
                                let Some((vendor, _)) = resolved(cx) else {
                                    return;
                                };
                                ai::test_connection(cx, vendor.base_url, vendor.api_key)
                            }),
                    )
                    .child(
                        Button::new("settings-ai-ask-once")
                            .ghost()
                            .small()
                            .label(tr(cx, "ai.action_ask_once").to_string())
                            .disabled(probe.busy() || endpoint.is_none())
                            .on_click(move |_, _, cx| {
                                let Some((vendor, model)) = resolved(cx) else {
                                    return;
                                };
                                ai::ask_once(cx, vendor.base_url, vendor.api_key, model)
                            }),
                    )
                    .into_any_element()
            })
            .keywords(&["ai", "test", "connect", "probe", "测试", "连接"]),
        ],
    )
}

/// 状态行那句主话。
fn probe_text(cx: &App, probe: &Probe) -> SharedString {
    match probe {
        Probe::Idle => tr(cx, "ai.probe_idle"),
        Probe::Probing => tr(cx, "ai.probe_probing"),
        Probe::Ok { models, .. } => match models {
            Some(ids) => tf(cx, "ai.probe_ok_models", &[("n", &ids.len().to_string())]).into(),
            None => tr(cx, "ai.probe_ok"),
        },
        Probe::Failed(_) => tr(cx, "ai.probe_fail_plain"),
    }
}

/// 状态行下面那行灰字：失败原因或模型的原话。
fn probe_detail(cx: &App, probe: &Probe) -> Option<SharedString> {
    match probe {
        Probe::Failed(reason) => Some(tf(cx, "ai.probe_fail", &[("reason", reason)]).into()),
        Probe::Ok {
            reply: Some(reply), ..
        } => Some(tf(cx, "ai.probe_ok_reply", &[("reply", reply)]).into()),
        _ => None,
    }
}

/// 供应商那一组：编辑哪一条 + 那三格 + 新建与删除。没有档案时三格不摆 ——
/// 写不进任何地方的三格只会让人对着空气打字，先「＋新建」一条。
fn vendor_group(cx: &App) -> Group {
    let editing = ai::editing(cx);
    let mut rows = vec![Row::dropdown(
        tr(cx, "field.ai_vendor_edit"),
        ConfigHolder::vendors(cx)
            .iter()
            .map(vendor_option)
            .collect(),
        false,
        move |cx: &App| SharedString::from(ai::editing(cx).unwrap_or_default()),
        move |value: SharedString, cx| {
            let id = value.to_string();
            ai::entity(cx).update(cx, |state, cx| state.set_editing(Some(id), cx));
        },
    )];
    if let Some(id) = editing {
        rows.push(vendor_text_row(
            cx,
            &id,
            tr(cx, "field.ai_vendor_name"),
            false,
            |vendor| vendor.name.as_str(),
            |vendor, value| vendor.name = value,
        ));
        rows.push(vendor_text_row(
            cx,
            &id,
            tr(cx, "field.ai_vendor_base"),
            false,
            |vendor| vendor.base_url.as_str(),
            |vendor, value| vendor.base_url = value,
        ));
        rows.push(vendor_text_row(
            cx,
            &id,
            tr(cx, "field.ai_vendor_key"),
            true,
            |vendor| vendor.api_key.as_str(),
            |vendor, value| vendor.api_key = value,
        ));
    }
    rows.push(actions_row(cx));
    group(tr(cx, "group.ai_vendors"), rows)
}

/// 编辑中那条档案的一格文本行。读写各拿一份字段闭包：读的按**这一刻**选中的那条算，
/// 写的落进当时选中的那条 —— 换编辑对象之后再打字，写的就是新选中的那条。
fn vendor_text_row(
    _cx: &App,
    id: &str,
    title: SharedString,
    secret: bool,
    read: impl Fn(&Vendor) -> &str + Copy + 'static,
    write: impl Fn(&mut Vendor, String) + Copy + 'static,
) -> Row {
    let getter_id = id.to_string();
    let getter = move |cx: &App| -> SharedString {
        ConfigHolder::vendors(cx)
            .iter()
            .find(|vendor| vendor.id == getter_id)
            .map(read)
            .unwrap_or_default()
            .to_string()
            .into()
    };
    let setter_id = id.to_string();
    let row = Row::text(title, getter, move |value: SharedString, cx| {
        let value = value.to_string();
        ConfigHolder::edit_vendors(cx, |vendors| {
            if let Some(vendor) = vendors.iter_mut().find(|vendor| vendor.id == setter_id) {
                write(vendor, value);
            }
        });
    });
    if secret { row.secret() } else { row }
}

/// 那两颗动作：＋新建（预设菜单）与删除（两段式确认）。常态摆两颗钮；
/// 确认挂起时**整行换成面板** —— 「点一下就删掉一条档案」不该靠读者记住钮在哪一侧
/// （与 storage 页的确认面板同一个形状）。
fn actions_row(_cx: &App) -> Row {
    Row::custom(move |_, cx| {
        if let Some(id) = ai::handle(cx).and_then(|entity| entity.read(cx).deleting().cloned()) {
            return delete_panel(cx, &id);
        }
        let editing = ai::editing(cx);
        let add = Button::new("ai-vendor-add")
            .ghost()
            .small()
            .label(tr(cx, "ai.action_add").to_string())
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu: PopupMenu, _, cx| {
                let mut menu = menu;
                for preset in VENDOR_PRESETS {
                    let (name, base) = (preset.name, preset.base_url);
                    menu = menu.item(PopupMenuItem::new(format!("{name} · {base}")).on_click(
                        move |_, _, cx| {
                            create_vendor(cx, name, base);
                        },
                    ));
                }
                menu.item(
                    PopupMenuItem::new(tr(cx, "ai.action_custom").to_string()).on_click(
                        move |_, _, cx| {
                            create_vendor(cx, "", "");
                        },
                    ),
                )
            });
        let mut row = h_flex().w_full().items_center().gap_2().child(add);
        if let Some(id) = editing {
            row = row.child(
                Button::new("ai-vendor-delete")
                    .ghost()
                    .small()
                    .label(tr(cx, "ai.action_delete").to_string())
                    .on_click(move |_, _, cx| {
                        ai::entity(cx).update(cx, |state, cx| state.ask_delete(id.clone(), cx));
                    }),
            );
        }
        row.into_any_element()
    })
}

/// 新建一条档案：预填名称与地址（预设）或全空（自定义），并把它设为编辑对象 ——
/// 读者接下来的三格打的就是这一条。
fn create_vendor(cx: &mut App, name: &str, base_url: &str) {
    let vendor = Vendor {
        id: new_vendor_id(),
        name: name.to_string(),
        base_url: base_url.to_string(),
        api_key: String::new(),
    };
    let id = vendor.id.clone();
    ConfigHolder::edit_vendors(cx, |vendors| vendors.push(vendor));
    ai::entity(cx).update(cx, |state, cx| state.set_editing(Some(id), cx));
}

/// 删除确认面板：一句问的是什么（带名字）、一句后果、两颗钮（取消在左，删除在右、danger）。
fn delete_panel(cx: &App, id: &str) -> AnyElement {
    let name = ConfigHolder::vendors(cx)
        .iter()
        .find(|vendor| vendor.id == id)
        .map(|vendor| vendor.display_name())
        .unwrap_or_default();
    let name: SharedString = name.into();
    v_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(tf(cx, "ai.delete_title", &[("name", &name)])),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr(cx, "ai.delete_body")),
        )
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .child(
                    Button::new("ai-vendor-delete-cancel")
                        .ghost()
                        .label(tr(cx, "shelf.cancel").to_string())
                        .on_click(move |_, _, cx| {
                            ai::entity(cx).update(cx, |state, cx| state.answer_delete(false, cx));
                        }),
                )
                .child(
                    Button::new("ai-vendor-delete-go")
                        .danger()
                        .label(tr(cx, "ai.action_delete").to_string())
                        .on_click(move |_, _, cx| {
                            ai::entity(cx).update(cx, |state, cx| state.answer_delete(true, cx));
                        }),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    //! 这一屏钉三件事：开关与模型写的是 `Config.ai` 那一节、供应商下拉引用的是
    //! `Config.vendors` 那份注册表（与翻译屏同一份）、档案三格写的是**被选中的那一条**。
    //! 探针与新建/删除是 custom 行的接线，规格层驱动不了窗口，由 services::ai 的
    //! 状态机测试与编译器一起看住。

    use super::ai_page;
    use crate::ui::features::settings::model::spec::{Page, Row, RowKind, Setter};
    use crate::ui::features::settings::view::ConfigHolder;
    use crate::ui::services::l10n::tr;
    use folio_core::{Config, Vendor};
    use gpui_kit::{App, SharedString, TestAppContext};
    use std::rc::Rc;

    /// 按**行标题**找一行。按序号找的话，任何一屏加一行就把断言指到别的行上去；
    /// 屏名进 panic 是为了说清少的是哪一屏那一格。
    fn row<'a>(page: &'a Page, which: &str, title: &str) -> &'a Row {
        page.groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .find(|item| item.title.as_str() == title)
            .unwrap_or_else(|| panic!("{which} 没有标题为「{title}」的行"))
    }

    /// 取一行的文本读写两格。不是文本行就直接红 —— 行型换了要重写这条断言，
    /// 而不是让它悄悄什么都不验。
    type Getter = dyn Fn(&App) -> SharedString;

    fn text(row: &Row) -> (&Rc<Getter>, &Setter<SharedString>) {
        match &row.kind {
            RowKind::Text { get, set, .. } => (get, set),
            _ => panic!("这一行该是文本行（行型换了要重写这条断言，别让它空跑）"),
        }
    }

    fn dropdown(row: &Row) -> (&Rc<Getter>, &Setter<SharedString>) {
        match &row.kind {
            RowKind::Dropdown { get, set, .. } => (get, set),
            _ => panic!("这一行该是下拉"),
        }
    }

    /// 测试用的会员位：这一屏整页在门禁后面。
    fn premium_config(cx: &mut App) {
        ConfigHolder::install(Config::default(), cx);
        ConfigHolder::edit_membership(cx, |membership| membership.lifetime = true);
    }

    /// 加一条档案（走公开写入口，与设置页同一条路）。
    fn add_vendor(cx: &mut App, name: &str, base: &str, key: &str) -> String {
        let vendor = Vendor {
            id: folio_core::new_vendor_id(),
            name: name.to_string(),
            base_url: base.to_string(),
            api_key: key.to_string(),
        };
        let id = vendor.id.clone();
        ConfigHolder::edit_vendors(cx, |vendors| vendors.push(vendor));
        id
    }

    /// 开关与模型写的是 `Config.ai` 那一节；改 AI 的模型不许动翻译那格的模型。
    #[gpui_kit::test]
    fn the_ai_switch_writes_the_ai_section(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            premium_config(cx);
            let id = add_vendor(cx, "DeepSeek", "https://api.deepseek.com/v1", "");
            let page = ai_page(cx);
            let switch = row(&page, "AI 屏", tr(cx, "field.ai_enabled").as_str());
            let (get, set) = match &switch.kind {
                RowKind::Toggle { get, set } => (get, set),
                _ => panic!("那一行该是个开关（行型换了要重写这条断言，别让它空跑）"),
            };
            assert!(!get(cx), "开局该是关的：默认关是隐私口径的一部分");
            set(true, cx);
            assert!(ConfigHolder::ai(cx).enabled, "开关没写进 `Config.ai`");

            let model_title = tr(cx, "field.ai_model").to_string();
            let (read_model, write_model) = text(row(&page, "AI 屏", &model_title));
            write_model(SharedString::from("big-model"), cx);
            assert_eq!(read_model(cx).as_str(), "big-model");
            assert_eq!(ConfigHolder::ai(cx).model, "big-model");

            // 「推荐模型」下拉出现（DeepSeek 的 host 有预设表），两格写的是同一格：
            // 当前值跟着模型格走，选中一项就把模型格换掉。
            let preset_title = tr(cx, "field.ai_model_preset").to_string();
            let (read_preset, pick) = dropdown(row(&page, "AI 屏", &preset_title));
            assert_eq!(
                read_preset(cx).as_str(),
                "big-model",
                "当前值与模型格同一份"
            );
            pick(SharedString::from("deepseek-chat"), cx);
            assert_eq!(ConfigHolder::ai(cx).model, "deepseek-chat");
            let _ = id;
        });
    }

    /// 供应商下拉写的是 `ai.vendor` 那一节；「跟随翻译」落空串。
    #[gpui_kit::test]
    fn the_vendor_choice_writes_the_ai_vendor_field(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            premium_config(cx);
            let id = add_vendor(cx, "DeepSeek", "https://api.deepseek.com/v1", "");
            let page = ai_page(cx);
            let title = tr(cx, "field.ai_vendor").to_string();
            let (read, set) = dropdown(row(&page, "AI 屏", &title));
            assert!(read(cx).is_empty(), "出厂跟随翻译那档（空串）");
            set(SharedString::from(id.clone()), cx);
            assert_eq!(ConfigHolder::ai(cx).vendor, id, "引用写进 `ai.vendor`");
            set(SharedString::default(), cx);
            assert!(ConfigHolder::ai(cx).vendor.is_empty(), "跟随翻译 = 空串");
        });
    }

    /// 档案的三格写的是**被选中的那一条**；翻译屏读的是同一份注册表。
    /// 没有选中态的实体时落第一条 —— 取值闭包只有 `&App`，这是它的口径。
    #[gpui_kit::test]
    fn the_vendor_rows_edit_the_selected_profile(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            premium_config(cx);
            add_vendor(cx, "DeepSeek", "https://api.deepseek.com/v1", "");
            let page = ai_page(cx);
            let base_title = tr(cx, "field.ai_vendor_base").to_string();
            let (read_base, set_base) = text(row(&page, "AI 屏", &base_title));
            assert_eq!(
                read_base(cx).as_str(),
                "https://api.deepseek.com/v1",
                "没有选中态时读第一条"
            );
            set_base(SharedString::from("https://api.changed.test/v1"), cx);
            assert_eq!(
                ConfigHolder::vendors(cx)[0].base_url,
                "https://api.changed.test/v1",
                "写进第一条档案"
            );

            let key_title = tr(cx, "field.ai_vendor_key").to_string();
            let (_, set_key) = text(row(&page, "AI 屏", &key_title));
            set_key(SharedString::from("sk-from-page"), cx);
            assert_eq!(
                ConfigHolder::vendors(cx)[0].api_key,
                "sk-from-page",
                "密钥落进同一份注册表 —— 翻译屏读到的是它"
            );
        });
    }
}
