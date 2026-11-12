//! 翻译：选区翻译译成哪种语言、由哪只引擎来译，以及引擎自己的参数。
//!
//! 「译成」与「引擎」同住一组 —— 它们共同决定弹层里拿到什么；引擎私有的参数
//! 只在选中那一档时单独成组。引擎组排在最后，所以切换引擎时前面那两行的下标不动：
//! 两端的输入框状态按（页、组、行）键控，插在前面会把正打字的那一格错位到另一项上。
//!
//! OpenAI 兼容那一档的地址与密钥住在**供应商注册表**（`Config.vendors`）里 ——
//! 这一屏只摆「用哪条档案」的引用与模型，档案的管理在「AI」屏（编辑器只做一处）。

use folio_core::TranslationEngine;
use folio_core::net::translate::TARGET_LANGUAGES;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{SharedString, Styled as _, div};

use super::super::*;
use crate::ui::components::muted_label;

pub(in crate::ui::features::settings) fn translation_page(cx: &App) -> Page {
    // 门禁：划词翻译是会员功能（AI 屏共用同一份端点，门禁也同一条）。非会员整页换成引导。
    if !crate::ui::services::activation::premium(cx) {
        return page(
            tr(cx, "page.translation"),
            IconName::Languages,
            vec![super::membership::gate_group("membership.gate_ai", cx)],
        );
    }
    let mut groups = vec![group(
        tr(cx, "group.translation_output"),
        vec![target_language(cx), engine(cx)],
    )];
    let openai_used = matches!(
        ConfigHolder::translation(cx).engine,
        TranslationEngine::OpenAi
    );
    if openai_used {
        groups.push(openai_group(cx, tr(cx, "group.translation_openai")));
    }
    if matches!(
        ConfigHolder::translation(cx).engine,
        TranslationEngine::DeepL
    ) {
        groups.push(deepl_group(cx));
    }
    // AI 那一节早先挂在这一屏末尾，现在它有自己的一屏（`pages/ai.rs`）：那一格换的是
    // 划词卡片上有没有「问 AI」，跟「这段话由哪只引擎译」不是同一件事，藏在别的屏末尾的
    // 症状就是找不到。
    page(tr(cx, "page.translation"), IconName::Languages, groups)
}

/// 目标语言。空串是「跟随界面语言」—— 与配置里的语义同一份，不另造一个布尔。
fn target_language(cx: &App) -> Row {
    let mut options: Vec<(SharedString, SharedString)> =
        vec![(SharedString::default(), tr(cx, "translation.follow_ui"))];
    options.extend(
        TARGET_LANGUAGES
            .iter()
            .map(|(code, label)| (SharedString::from(*code), SharedString::from(*label))),
    );
    // 十项：手机端推一屏勾选，桌面用可滚动的下拉 —— 与「多选一」那两条规矩一致。
    Row::choice(
        tr(cx, "field.translation_target_lang"),
        options,
        true,
        |cx: &App| SharedString::from(ConfigHolder::translation(cx).target_lang),
        |val: SharedString, cx| {
            ConfigHolder::edit_translation(cx, |translation| {
                translation.target_lang = val.to_string()
            })
        },
    )
    .description(tr(cx, "field.translation_target_lang.desc"))
    .keywords(&["translate", "target", "language", "翻译", "语言", "目标"])
}

/// 引擎。选项标签按当前界面语言现译；写回配置的值是稳定的键名（`google` 等）。
fn engine(cx: &App) -> Row {
    let options = TranslationEngine::ALL
        .iter()
        .map(|engine| {
            let label = match engine {
                TranslationEngine::Google => tr(cx, "translation.engine_google"),
                TranslationEngine::OpenAi => tr(cx, "translation.engine_openai"),
                TranslationEngine::DeepL => tr(cx, "translation.engine_deepl"),
            };
            (SharedString::from(engine.as_str()), label)
        })
        .collect();
    Row::dropdown(
        tr(cx, "field.translation_engine"),
        options,
        false,
        |cx: &App| SharedString::from(ConfigHolder::translation(cx).engine.as_str()),
        |val: SharedString, cx| {
            ConfigHolder::edit_translation(cx, |translation| {
                translation.engine = TranslationEngine::parse(&val)
            })
        },
    )
    .description(tr(cx, "field.translation_engine.desc"))
    .keywords(&["engine", "provider", "引擎", "服务"])
}

/// OpenAI 兼容那一档：用哪条供应商档案、用哪只模型，加一行连接探针。
///
/// 地址与密钥住在**供应商注册表**（`Config.vendors`）里，这一组只摆引用与模型；
/// 档案的管理（新建、改名、换地址、删）在「AI」屏那一组 —— 两个功能共用同一份
/// 注册表，编辑器只做一处，免得两屏各说各话。注册表一条档案都没有时，下拉里那句
/// 「还没有供应商」就是唯一的选项，引擎会在发请求前报「还没填」。
///
/// 组标题由调用方给：「翻译」屏叫「OpenAI 兼容」。
pub(in crate::ui::features::settings::view::pages) fn openai_group(
    cx: &App,
    title: impl Into<SharedString>,
) -> Group {
    group(
        title.into(),
        vec![
            vendor_choice(cx),
            Row::text(
                tr(cx, "field.translation_openai_model"),
                |cx: &App| SharedString::from(ConfigHolder::translation(cx).openai.model),
                |val: SharedString, cx| {
                    ConfigHolder::edit_translation(cx, |translation| {
                        translation.openai.model = val.to_string()
                    })
                },
            )
            .description(tr(cx, "field.translation_openai_model.desc"))
            .keywords(&["openai", "model", "模型"]),
            probe_row(cx),
        ],
    )
}

/// 翻译用哪条供应商档案。空串 = 回退注册表第一条（`resolve_vendor` 的规则）——
/// 折出来或新建的第一条自动就是它，读者不用再点一下。
fn vendor_choice(cx: &App) -> Row {
    let vendors = ConfigHolder::vendors(cx);
    let mut options: Vec<(SharedString, SharedString)> = vendors
        .iter()
        .map(|vendor| {
            let host = folio_core::host_of(&vendor.base_url);
            let label = if host.is_empty() {
                vendor.display_name()
            } else {
                format!("{} · {}", vendor.display_name(), host)
            };
            (
                SharedString::from(vendor.id.clone()),
                SharedString::from(label),
            )
        })
        .collect();
    if options.is_empty() {
        options.push((
            SharedString::default(),
            tr(cx, "field.translation_vendor.empty"),
        ));
    }
    Row::dropdown(
        tr(cx, "field.translation_vendor"),
        options,
        false,
        |cx: &App| SharedString::from(ConfigHolder::translation(cx).openai.vendor),
        |val: SharedString, cx| {
            ConfigHolder::edit_translation(cx, |translation| {
                translation.openai.vendor = val.to_string()
            })
        },
    )
    .description(tr(cx, "field.translation_vendor.desc"))
    .keywords(&["openai", "vendor", "provider", "供应商", "服务"])
}

/// 连接探针：测的是翻译解析出的那条档案。与 AI 屏共用同一个探针状态机
/// （`services::ai`）—— 两个屏测的是同一份注册表，状态也没必要各存一份。
fn probe_row(_cx: &App) -> Row {
    Row::custom(move |_, cx| {
        let probe = crate::ui::services::ai::handle(cx)
            .map(|entity| entity.read(cx).probe().clone())
            .unwrap_or(crate::ui::services::ai::Probe::Idle);
        let endpoint = translation_endpoint(cx);

        let mut column = v_flex().min_w_0().flex_1().gap_0p5();
        column = column.child(
            div()
                .text_base()
                .text_ellipsis()
                .child(probe_line(cx, &probe)),
        );
        if let crate::ui::services::ai::Probe::Failed(reason) = &probe {
            let reason = reason.clone();
            column = column.child(muted_label(
                tf(cx, "ai.probe_fail", &[("reason", &reason)]),
                cx,
            ));
        }

        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(column)
            .child(
                Button::new("settings-translation-test")
                    .ghost()
                    .small()
                    .label(tr(cx, "ai.action_test").to_string())
                    .disabled(probe.busy() || endpoint.is_none())
                    .on_click(
                        move |_: &gpui_kit::ClickEvent,
                              _: &mut gpui_kit::Window,
                              cx: &mut gpui_kit::App| {
                            let Some((vendor, _)) = translation_endpoint(cx) else {
                                return;
                            };
                            crate::ui::services::ai::test_connection(
                                cx,
                                vendor.base_url,
                                vendor.api_key,
                            )
                        },
                    ),
            )
            .into_any_element()
    })
    .keywords(&["openai", "test", "connect", "测试", "连接"])
}

/// 翻译解析出的（供应商，模型）。探针与引擎的建法共用这一份解析。
fn translation_endpoint(cx: &App) -> Option<(folio_core::Vendor, String)> {
    let translation = ConfigHolder::translation(cx);
    let vendors = ConfigHolder::vendors(cx);
    let vendor = folio_core::resolve_vendor(&vendors, &translation.openai.vendor)?;
    let model = translation.openai.model.trim().to_string();
    if vendor.base_url.trim().is_empty() || model.is_empty() {
        return None;
    }
    Some((vendor.clone(), model))
}

/// 状态行那句主话（与 AI 屏同一张词表，少「问一句」那一档）。
fn probe_line(cx: &App, probe: &crate::ui::services::ai::Probe) -> SharedString {
    match probe {
        crate::ui::services::ai::Probe::Idle => tr(cx, "ai.probe_idle"),
        crate::ui::services::ai::Probe::Probing => tr(cx, "ai.probe_probing"),
        crate::ui::services::ai::Probe::Ok { models, .. } => match models {
            Some(ids) => tf(cx, "ai.probe_ok_models", &[("n", &ids.len().to_string())]).into(),
            None => tr(cx, "ai.probe_ok"),
        },
        crate::ui::services::ai::Probe::Failed(_) => tr(cx, "ai.probe_fail_plain"),
    }
}

fn deepl_group(cx: &App) -> Group {
    group(
        tr(cx, "group.translation_deepl"),
        vec![
            Row::text(
                tr(cx, "field.translation_deepl_key"),
                |cx: &App| SharedString::from(ConfigHolder::translation(cx).deepl.api_key),
                |val: SharedString, cx| {
                    ConfigHolder::edit_translation(cx, |translation| {
                        translation.deepl.api_key = val.to_string()
                    })
                },
            )
            .description(tr(cx, "field.translation_deepl_key.desc"))
            .keywords(&["deepl", "key", "token", "密钥"]),
        ],
    )
}
