//! 翻译：选区翻译译成哪种语言、由哪只引擎来译，以及引擎自己的参数。
//!
//! 「译成」与「引擎」同住一组 —— 它们共同决定弹层里拿到什么；引擎私有的参数
//! （地址、模型、密钥）只在选中那一档时单独成组。引擎组排在最后，所以切换引擎时
//! 前面那两行的下标不动：两端的输入框状态按（页、组、行）键控，插在前面会把
//! 正打字的那一格错位到另一项上。
//!
//! 密钥按普通文本行摆：本机 `config.json` 里的值本来就可读，输入框遮不遮只影响
//! 肩窥；而遮罩输入要另起一种行型（桌面那套 `SettingField` 没有密码档），两端会分叉。
//! 真需要遮罩时再统一加，不在这里单独造一个只在一端生效的样子。

use folio_core::TranslationEngine;
use folio_core::net::translate::TARGET_LANGUAGES;

use super::super::*;

pub(in crate::ui::features::settings) fn translation_page(cx: &App) -> Page {
    let mut groups = vec![group(
        tr(cx, "group.translation_output"),
        vec![target_language(cx), engine(cx)],
    )];
    let openai_used = matches!(
        ConfigHolder::translation(cx).engine,
        TranslationEngine::OpenAi
    );
    if openai_used {
        groups.push(openai_group(cx));
    }
    if matches!(
        ConfigHolder::translation(cx).engine,
        TranslationEngine::DeepL
    ) {
        groups.push(deepl_group(cx));
    }
    // AI 与翻译**共用同一份 OpenAI 兼容配置**（地址与密钥只存一处）。读者如果翻译用
    // Google / DeepL 而 AI 要接自己的服务，那三格在别处没有落点 —— 所以「AI 开着」
    // 就把它们摆出来，标题照旧：这是同一个端点的两个用途，不是两份设置。
    let ai = ConfigHolder::ai(cx);
    if ai.enabled && !openai_used {
        groups.push(openai_group(cx));
    }
    groups.push(ai_group(cx));
    page(tr(cx, "page.translation"), IconName::Languages, groups)
}

/// AI 那一组：只有「要不要」与「用哪只模型」两格，加一行说清地址与密钥在哪填。
///
/// **排在最后**：两端的输入框状态按（页、组、行）键控，往前插会把读者正打字那一格错位到
/// 另一项上（与引擎那一组同一条理由）。
fn ai_group(cx: &App) -> Group {
    group(
        tr(cx, "group.ai"),
        vec![
            Row::toggle(
                tr(cx, "field.ai_enabled"),
                |cx| ConfigHolder::ai(cx).enabled,
                |on, cx| ConfigHolder::edit_ai(cx, |ai| ai.enabled = on),
            )
            .description(tr(cx, "field.ai_enabled.desc"))
            .keywords(&["ai", "ask", "gpt", "问 ai", "人工智能"]),
            Row::text(
                tr(cx, "field.ai_model"),
                |cx| SharedString::from(ConfigHolder::ai(cx).model),
                |value, cx| ConfigHolder::edit_ai(cx, |ai| ai.model = value.to_string()),
            )
            .description(tr(cx, "field.ai_model.desc"))
            .keywords(&["ai", "model", "模型"]),
            Row::info(
                tr(cx, "field.ai_endpoint"),
                tr(cx, "field.ai_endpoint.value"),
            )
            .description(tr(cx, "field.ai_endpoint.desc"))
            .keywords(&["ai", "endpoint", "base url", "地址"]),
        ],
    )
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

/// OpenAI 兼容那一档的三格。地址与模型清空时引擎会在发请求前报「还没填」，
/// 这里不补默认值 —— 边打边存时补回去，读者就删不空这一格。
fn openai_group(cx: &App) -> Group {
    group(
        tr(cx, "group.translation_openai"),
        vec![
            Row::text(
                tr(cx, "field.translation_openai_base"),
                |cx: &App| SharedString::from(ConfigHolder::translation(cx).openai.base_url),
                |val: SharedString, cx| {
                    ConfigHolder::edit_translation(cx, |translation| {
                        translation.openai.base_url = val.to_string()
                    })
                },
            )
            .description(tr(cx, "field.translation_openai_base.desc"))
            .keywords(&["openai", "url", "endpoint", "地址", "接口"]),
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
            Row::text(
                tr(cx, "field.translation_openai_key"),
                |cx: &App| SharedString::from(ConfigHolder::translation(cx).openai.api_key),
                |val: SharedString, cx| {
                    ConfigHolder::edit_translation(cx, |translation| {
                        translation.openai.api_key = val.to_string()
                    })
                },
            )
            .description(tr(cx, "field.translation_openai_key.desc"))
            .keywords(&["openai", "key", "token", "密钥"]),
        ],
    )
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
