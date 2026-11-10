//! 选区问 AI 与切章规则草案的**入口层**。机制（provider、提示词、响应解析、尺寸闸）
//! 住 [`folio_ai`] crate —— 「换 crate 不换形状」，这里沿用旧路径；本层只做一件事：
//! 把两份配置映射成纯字段递过去，并守住「这一格存不存在」的判据。
//!
//! ## 四种「回 None」与它们的画法
//!
//! [`configured`] 回 `None` 的四种情况：没开、地址空、模型两处都空、只有出厂那份地址
//! 且 key 没填（读者从没打开过这一格）。界面对 `None` 的画法一律是**这一格不出现**
//! （与听书在没有后端的那些平台上同一条判据），而不是摆一颗按下去报错的按钮。

pub use folio_ai::ai::{
    AiError, Answer, Ask, Asked, Assistant, CONTEXT_BUDGET, ChatCompletion, MAX_ANSWER_CHARS,
    MAX_INPUT_CHARS, clip, draft_prompt, draft_system, selection_prompt,
};

use crate::infra::config::Translation as TranslationConfig;
use crate::infra::config::{Ai, OpenAi};

/// 按两份配置现建 provider。**没开 / 地址空 / 两处模型都空 / 只有出厂那份地址且没填
/// key** 都回 `None`：那几种情况下界面对「这一格不存在」的画法已经写好了（与听书同一
/// 条判据），不在这里替读者编一个端点。
///
/// 第四条不是多余的谨慎：出厂把官方地址与模型填进了翻译那一档（见 `config::OpenAi` 的
/// `Default`，读者只贴 key 就能用），所以「地址 == 出厂那个 + key 空着」唯一的解释是
/// **读者从没打开过这一格**。这时候把选中的正文 POST 出去，是替读者做了一个他没做过的
/// 决定，而它只会换一个 401。反过来，本地 Ollama / LM Studio 那类不认 key 的服务地址
/// 必然是读者自己填的，那条路照常通 —— 所以这里问的是「这个值是读者给的还是我们给的」，
/// 不是「这个服务要不要 key」（那一格归 [`ChatCompletion`] 的请求闸，它不猜）。
pub fn configured(translation: &TranslationConfig, ai: &Ai) -> Option<Box<dyn Assistant>> {
    if !ai.enabled {
        return None;
    }
    let OpenAi {
        base_url,
        model,
        api_key,
    } = &translation.openai;
    let model = if ai.model.trim().is_empty() {
        model.trim().to_string()
    } else {
        ai.model.trim().to_string()
    };
    if base_url.trim().is_empty() || model.is_empty() {
        return None;
    }
    if api_key.trim().is_empty() && base_url == &OpenAi::default().base_url {
        return None;
    }
    Some(Box::new(ChatCompletion::new(base_url, &model, api_key)))
}

#[cfg(test)]
mod tests {
    //! 「这一格存不存在」的判据：机制测试（请求形状、解析、提示词）在 folio-ai，
    //! 这里只钉「配置到 None」的四条 —— 它们咬着 config 类型，跟着判据走。

    use super::configured;
    use crate::infra::config::{
        Ai as AiConfig, OpenAi, Translation as TranslationConfig, TranslationEngine,
    };

    /// 翻译那档的地址与模型（AI 共用它们），密钥照填。
    fn translation(base: &str, model: &str) -> TranslationConfig {
        arm(base, model, "k")
    }

    /// OpenAI 兼容那一档的三个值：地址、模型、密钥。
    fn arm(base: &str, model: &str, key: &str) -> TranslationConfig {
        TranslationConfig {
            engine: TranslationEngine::OpenAi,
            openai: OpenAi {
                base_url: base.to_string(),
                model: model.to_string(),
                api_key: key.to_string(),
            },
            ..Default::default()
        }
    }

    /// 开了、模型跟随翻译那一档。
    fn on() -> AiConfig {
        AiConfig {
            enabled: true,
            model: String::new(),
        }
    }

    /// 没开、地址空、两处模型都空 —— 三种「这一格不存在」都回 `None`。
    /// 这条钉的是「不许替读者编一个默认端点」：编了就等于悄悄把书本文发出去。
    #[test]
    fn unconfigured_never_invents_an_endpoint() {
        let off = AiConfig {
            enabled: false,
            model: String::new(),
        };
        assert!(configured(&translation("https://x", "m"), &off).is_none());
        assert!(configured(&translation("", "m"), &on()).is_none());
        assert!(
            configured(&translation("https://x", ""), &on()).is_none(),
            "翻译那档没填模型、AI 这档也没填 ⇒ 不知道该问哪只模型"
        );
        assert!(configured(&translation("https://x", "m"), &on()).is_some());
    }

    /// 出厂那份地址 + 没填 key =「读者从没打开过这一格」⇒ 一个字节都不发。
    ///
    /// 这条不是形式测试：官方地址与模型在 `config::OpenAi` 的 `Default` 里是现成填好的，
    /// 所以「把开关一拨就能用」看着理所当然，而按下那一格会把选中的正文 POST 到一个他
    /// 没配过的服务上，只换一个 401 —— 还是拿他的书本文去换。
    #[test]
    fn the_shipped_endpoint_without_a_key_is_not_a_configuration() {
        let shipped_base = OpenAi::default().base_url;
        let shipped_model = OpenAi::default().model;
        assert!(
            configured(&arm(&shipped_base, &shipped_model, ""), &on()).is_none(),
            "地址还是出厂那份、key 空着 ⇒ 这一格不该出现"
        );
        assert!(
            configured(&arm(&shipped_base, &shipped_model, "sk-1"), &on()).is_some(),
            "贴上 key 就是一条真配置"
        );
        // 本地 Ollama / LM Studio 不认 Authorization：地址是读者自己填的，空 key 照常。
        assert!(
            configured(&arm("http://localhost:11434/v1", "qwen2.5:7b", ""), &on()).is_some(),
            "本地服务不填 key 也要能用"
        );
    }

    #[test]
    fn the_ai_model_overrides_only_the_model() {
        let shared = translation("https://x", "translate-model");
        let same = configured(&shared, &on()).expect("该建得出");
        let other = configured(
            &shared,
            &AiConfig {
                enabled: true,
                model: "big-model".to_string(),
            },
        )
        .expect("该建得出");
        assert!(same.cache_key().contains("translate-model"));
        assert!(other.cache_key().contains("big-model"));
        assert_ne!(same.cache_key(), other.cache_key(), "换模型必须换缓存键");
    }
}
