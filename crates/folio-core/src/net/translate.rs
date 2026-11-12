//! 选区翻译的**入口层**。引擎机制（三只引擎、请求/响应解析、编码、语言名表）
//! 住 [`folio_translate`] crate —— 「换 crate 不换形状」，这里沿用旧路径；本层只做
//! 一件事：把配置里的引擎档与密钥映射成一只 `Translator`。
//!
//! ## 错误分档
//!
//! [`TranslateError`] 把传输层的 WebDAV 词表翻成翻译场景能说的话：缺 key、密钥被拒、
//! 限流 / 配额、引擎返回的状态、响应读不懂。界面直接把这一句摆进失败卡片。

pub use folio_translate::{
    GoogleTranslator, MAX_SOURCE_CHARS, TARGET_LANGUAGES, TranslateError, Translation, Translator,
    source_hash,
};
pub mod deepl {
    //! DeepL 引擎（机制在 folio-translate，旧路径沿用）。
    pub use folio_translate::deepl::*;
}
pub mod openai {
    //! OpenAI 兼容引擎（机制在 folio-translate，旧路径沿用）。
    pub use folio_translate::openai::*;
}

use crate::infra::config::resolve_vendor;
use crate::infra::config::{Translation as TranslationConfig, TranslationEngine, Vendor};

/// 按配置现建一只引擎。返回装箱 trait object：会话把它整只搬进后台作业，
/// 请求发出去之后即使配置又改了，这一趟用的仍是出发时那份配置。
///
/// OpenAI 兼容档的地址与密钥从被引用的供应商档案里取（[`resolve_vendor`]：
/// 引用悬空回退第一条）。**注册表一条档案都没有时不在这里报错** —— 建一只
/// 地址为空的引擎，它会在发请求前报「还没填」，与旧版地址被清空时的口径同一条。
pub fn configured(config: &TranslationConfig, vendors: &[Vendor]) -> Box<dyn Translator> {
    match config.engine {
        TranslationEngine::Google => Box::new(GoogleTranslator),
        TranslationEngine::OpenAi => {
            let vendor = resolve_vendor(vendors, &config.openai.vendor);
            let (base, key) = vendor
                .map(|vendor| (vendor.base_url.as_str(), vendor.api_key.as_str()))
                .unwrap_or(("", ""));
            Box::new(openai::OpenAiTranslator::new(
                base,
                &config.openai.model,
                key,
            ))
        }
        TranslationEngine::DeepL => Box::new(deepl::DeepLTranslator::new(&config.deepl.api_key)),
    }
}

#[cfg(test)]
mod tests {
    //! 分发只认配置里的引擎档；引擎机制测试（请求形状、解析、语言映射）在 folio-translate。
    use super::configured;
    use crate::infra::config::{
        OpenAi, Translation as TranslationConfig, TranslationEngine, Vendor,
    };

    /// 一条供应商档案：id、地址、密钥。
    fn vendor(id: &str, base: &str, key: &str) -> Vendor {
        Vendor {
            id: id.to_string(),
            name: String::new(),
            base_url: base.to_string(),
            api_key: key.to_string(),
        }
    }

    /// 分发只按配置里的引擎走：默认那档就是 Google，OpenAI 档带读者引用的档案与模型。
    #[test]
    fn the_configured_engine_follows_the_engine_field() {
        let google = TranslationConfig::default();
        assert_eq!(configured(&google, &[]).cache_key(), "google");

        let vendors = [vendor("v1", "https://example.test/v1", "sk-secret")];
        let openai = TranslationConfig {
            engine: TranslationEngine::OpenAi,
            openai: OpenAi {
                vendor: "v1".into(),
                model: "qwen2.5:7b".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let key = configured(&openai, &vendors).cache_key();
        assert!(key.starts_with("openai:qwen2.5:7b@"), "{key}");
        assert!(!key.contains("sk-secret"), "密钥不许进缓存键");

        let deepl = TranslationConfig {
            engine: TranslationEngine::DeepL,
            ..Default::default()
        };
        assert_eq!(configured(&deepl, &vendors).cache_key(), "deepl");
    }

    /// 注册表空 / 引用悬空时不在这里报错：引擎的请求闸会在出网前报「还没填」。
    #[test]
    fn an_empty_registry_yields_an_unconfigured_engine_not_a_panic() {
        let openai = TranslationConfig {
            engine: TranslationEngine::OpenAi,
            ..Default::default()
        };
        let engine = configured(&openai, &[]);
        assert!(
            engine.cache_key().starts_with("openai:"),
            "建得出引擎就行 —— 报「还没填」是它出网前自己的事"
        );
    }
}
