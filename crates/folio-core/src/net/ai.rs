//! 选区问 AI 与切章规则草案的**入口层**。机制（provider、提示词、响应解析、尺寸闸）
//! 住 [`folio_ai`] crate —— 「换 crate 不换形状」，这里沿用旧路径；本层只做一件事：
//! 把配置里的供应商档案映射成纯字段递过去，并守住「这一格存不存在」的判据。
//!
//! ## 「回 None」与它们的画法
//!
//! [`configured`] 回 `None` 的几种情况：没开、注册表里一条档案都没有、地址空、
//! 模型两处都空。界面对 `None` 的画法一律是**这一格不出现**（与听书在没有后端的那些
//! 平台上同一条判据），而不是摆一颗按下去报错的按钮。
//!
//! 旧版还有一条「只有出厂地址且 key 没填」—— 那条判据现在住进迁移里
//! （[`crate::infra::config`] 的 `migrate_vendors`）：出厂地址不折成档案，注册表
//! 天然是空的，上面第二条就把它盖住了。供应商档案本身是读者亲手加的，
//! 「这是读者给的还是我们给的」不再需要在这里问。

pub use folio_ai::ai::{
    AiError, Answer, Ask, Asked, Assistant, CONTEXT_BUDGET, ChatCompletion, MAX_ANSWER_CHARS,
    MAX_INPUT_CHARS, clip, draft_prompt, draft_system, list_models, selection_prompt,
};

use crate::infra::config::{Ai, resolve_vendor};
use crate::infra::config::{Translation as TranslationConfig, Vendor};

/// 解析 AI 这一路**会用**的供应商与模型。与 [`configured`] 的差别是不看开关：
/// 「这一格存不存在」的判据要看 `enabled`，而设置页的探测（测试连接 / 拉模型清单）
/// 在读者还没拨开关时就得能跑 —— 两者共用这一份解析，不许各算一套。
///
/// 回 `None`：注册表空、解析出的地址空、模型两处都空。
pub fn resolved_ai_endpoint(
    translation: &TranslationConfig,
    ai: &Ai,
    vendors: &[Vendor],
) -> Option<(Vendor, String)> {
    let vendor_id = if ai.vendor.trim().is_empty() {
        &translation.openai.vendor
    } else {
        &ai.vendor
    };
    let vendor = resolve_vendor(vendors, vendor_id)?.clone();
    let model = if ai.model.trim().is_empty() {
        translation.openai.model.trim().to_string()
    } else {
        ai.model.trim().to_string()
    };
    if vendor.base_url.trim().is_empty() || model.is_empty() {
        return None;
    }
    Some((vendor, model))
}

/// 按三份配置现建 provider。**没开 / 注册表空 / 地址空 / 两处模型都空**都回 `None`：
/// 那几种情况下界面对「这一格不存在」的画法已经写好了（与听书同一判据），
/// 不在这里替读者编一个端点。
///
/// 供应商挑哪条：AI 自己引用的 [`Ai::vendor`] 优先，留空跟随翻译那档的引用；
/// 引用悬空回退注册表第一条（[`resolve_vendor`] 的规则，界面说得清）。
pub fn configured(
    translation: &TranslationConfig,
    ai: &Ai,
    vendors: &[Vendor],
) -> Option<Box<dyn Assistant>> {
    if !ai.enabled {
        return None;
    }
    let (vendor, model) = resolved_ai_endpoint(translation, ai, vendors)?;
    Some(Box::new(ChatCompletion::new(
        &vendor.base_url,
        &model,
        &vendor.api_key,
    )))
}

#[cfg(test)]
mod tests {
    //! 「这一格存不存在」的判据：机制测试（请求形状、解析、提示词）在 folio-ai，
    //! 这里只钉「配置到 None」的那几条 —— 它们咬着 config 类型，跟着判据走。

    use super::configured;
    use crate::infra::config::{
        Ai as AiConfig, OpenAi, Translation as TranslationConfig, TranslationEngine, Vendor,
    };

    /// 翻译那档：引用哪条档案、用哪只模型。
    fn translation(vendor_id: &str, model: &str) -> TranslationConfig {
        TranslationConfig {
            engine: TranslationEngine::OpenAi,
            openai: OpenAi {
                vendor: vendor_id.to_string(),
                model: model.to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// 一条供应商档案：id、地址、密钥。
    fn vendor(id: &str, base: &str, key: &str) -> Vendor {
        Vendor {
            id: id.to_string(),
            name: String::new(),
            base_url: base.to_string(),
            api_key: key.to_string(),
        }
    }

    /// 开了、供应商与模型都跟随翻译那一档。
    fn on() -> AiConfig {
        AiConfig {
            enabled: true,
            vendor: String::new(),
            model: String::new(),
        }
    }

    /// 缓存键里地址那截指纹（模型名在两台服务上可能不是同一个模型）。
    fn base_fp(base: &str) -> String {
        folio_base::identity::sha256_of_bytes(base.as_bytes()).to_hex()[..8].to_string()
    }

    /// 没开、注册表空、两处模型都空 —— 几种「这一格不存在」都回 `None`。
    /// 这条钉的是「不许替读者编一个默认端点」：编了就等于悄悄把书本文发出去。
    #[test]
    fn unconfigured_never_invents_an_endpoint() {
        let vendors = [vendor("v1", "https://x.test/v1", "k")];
        let off = AiConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(configured(&translation("v1", "m"), &off, &vendors).is_none());
        assert!(
            configured(&translation("v1", "m"), &on(), &[]).is_none(),
            "一条档案都没有 ⇒ 不知道该往哪儿问"
        );
        assert!(
            configured(&translation("v1", ""), &on(), &vendors).is_none(),
            "翻译那档没填模型、AI 这档也没填 ⇒ 不知道该问哪只模型"
        );
        assert!(configured(&translation("v1", "m"), &on(), &vendors).is_some());
    }

    /// AI 的供应商与模型**各自覆盖**翻译那一档：填了只换 AI 这一路。
    #[test]
    fn the_ai_vendor_and_model_override_only_the_ai_path() {
        let vendors = [
            vendor("cheap", "https://cheap.test/v1", "k1"),
            vendor("big", "https://big.test/v1", "k2"),
        ];
        // 都跟随：用的是翻译那条档案与那只模型。
        let followed =
            configured(&translation("cheap", "small-model"), &on(), &vendors).expect("该建得出");
        assert!(followed.cache_key().contains("small-model"));
        assert!(
            followed
                .cache_key()
                .ends_with(&base_fp("https://cheap.test/v1")),
            "地址进缓存键指纹：同一个模型名在两台服务上不是同一个模型"
        );

        // AI 自己挑了另一条档案 + 另一只模型。
        let own = AiConfig {
            enabled: true,
            vendor: "big".to_string(),
            model: "big-model".to_string(),
        };
        let overridden =
            configured(&translation("cheap", "small-model"), &own, &vendors).expect("该建得出");
        assert!(overridden.cache_key().contains("big-model"));
        assert_ne!(
            followed.cache_key(),
            overridden.cache_key(),
            "换档案换模型必须换缓存键"
        );

        // AI 只覆盖模型：档案仍跟随翻译那档。
        let model_only = AiConfig {
            enabled: true,
            vendor: String::new(),
            model: "big-model".to_string(),
        };
        let mixed = configured(&translation("cheap", "small-model"), &model_only, &vendors)
            .expect("该建得出");
        assert!(mixed.cache_key().contains("big-model"));
        assert!(
            mixed
                .cache_key()
                .ends_with(&base_fp("https://cheap.test/v1")),
            "没写供应商就跟翻译那条的地址"
        );
    }

    /// 引用悬空回退第一条：档案被删后 AI 自动落到列表头上，而不是整格消失。
    #[test]
    fn a_dangling_reference_falls_back_to_the_first_vendor() {
        let vendors = [vendor("v1", "https://first.test/v1", "k")];
        let got = configured(&translation("gone", "m"), &on(), &vendors).expect("回退该建得出");
        assert!(got.cache_key().ends_with(&base_fp("https://first.test/v1")));
    }

    /// 自建服务不填 key 也要能用：地址是读者亲手加的档案，空 key 照常是一条真配置。
    #[test]
    fn a_vendor_without_a_key_is_still_a_configuration() {
        let vendors = [vendor("local", "http://localhost:11434/v1", "")];
        assert!(configured(&translation("local", "qwen2.5:7b"), &on(), &vendors).is_some());
    }
}
