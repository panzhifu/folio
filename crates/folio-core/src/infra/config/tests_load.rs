use crate::infra::config::{Config, Env, Layer, ThemeBackground, TranslationEngine};
use crate::infra::i18n::Locale;
use crate::{AnnotationStyle, Appearance, Language, Mode, PageTurn, Reader};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

fn load(text: &str) -> Config {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, text).unwrap();
    Config::load_from(&path)
}

#[test]
fn credential_debug_is_redacted_without_changing_serialization() {
    use std::fmt::Debug;

    use crate::infra::config::{
        Account, AliyunDrive, BaiduDrive, Claim, DeepL, Env, OpdsCatalog, OpenAi, Sync, Vendor,
        WebDavDrive,
    };

    fn check<T: Debug + serde::Serialize + serde::de::DeserializeOwned>(json: &str) {
        let value: T = serde_json::from_str(json).unwrap();
        assert!(!format!("{value:?}").contains("SENTINEL"));
        assert!(serde_json::to_string(&value).unwrap().contains("SENTINEL"));
    }

    check::<Vendor>(r#"{"api_key":"SENTINEL","base_url":"https://SENTINEL@example.test"}"#);
    check::<OpenAi>(r#"{"api_key":"SENTINEL"}"#);
    check::<DeepL>(r#"{"api_key":"SENTINEL"}"#);
    check::<Sync>(r#"{"password":"SENTINEL"}"#);
    check::<Account>(r#"{"key":"SENTINEL"}"#);
    check::<Claim>(r#"{"order":"SENTINEL"}"#);
    check::<OpdsCatalog>(r#"{"password":"SENTINEL"}"#);
    check::<WebDavDrive>(r#"{"password":"SENTINEL"}"#);
    check::<BaiduDrive>(r#"{"access_token":"SENTINEL","refresh_token":"SENTINEL"}"#);
    check::<AliyunDrive>(r#"{"access_token":"SENTINEL","refresh_token":"SENTINEL"}"#);
    let env = Env {
        log: Some("SENTINEL".into()),
        ..Env::default()
    };
    assert!(!format!("{env:?}").contains("SENTINEL"));
}

/// 老文件没有版本戳（serde 缺省成 0）：按当前版本读，字段照旧，版本对齐到当前。
/// §2.6：读到旧版本是**迁移**，不是崩溃。
#[test]
fn a_versionless_file_is_read_and_realigned_to_the_current_version() {
    let got = load(r#"{"reader":{"font_size":21.0}}"#);
    assert_eq!(got.reader.font_size, 21.0, "老文件的字段照旧读出来");
    assert_eq!(got.version, crate::infra::config::CONFIG_VERSION);
}

/// 比本程序**新**的文件：认得的部分照读、未知字段忽略，版本对齐到本程序
/// （不是崩溃，也不是静默当新版用 —— 有一条 WARN）。
#[test]
fn a_newer_file_version_is_read_not_rejected() {
    let got = load(r#"{"version":999,"reader":{"font_size":21.0},"future_thing":true}"#);
    assert_eq!(got.reader.font_size, 21.0, "认得的字段要留下");
    assert_eq!(got.version, crate::infra::config::CONFIG_VERSION);
}

/// 写出去再读回来，版本戳要保住（往返相等已覆盖，这里把「版本」单点钉一下）。
#[test]
fn the_version_is_written_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    Config::default().save_to(&path).unwrap();
    assert_eq!(
        Config::load_from(&path).version,
        crate::infra::config::CONFIG_VERSION
    );
}

/// 旧版的内联端点要**折成供应商档案**，且各格首尾空白在归一里去掉 ——
/// 手抄地址时多带一个空格，拼出来的 URL 就是另一种东西。
#[test]
fn an_inline_endpoint_folds_into_a_vendor() {
    let got = load(
        r#"{"translation":{
            "target_lang":" ja ",
            "engine":"openai",
            "openai":{"base_url":" https://example.test/v1 ","model":" qwen2.5:7b ","api_key":" sk-1 "},
            "deepl":{"api_key":" fx-1 "}
        }}"#,
    );
    assert_eq!(got.translation.target_lang, "ja");
    assert_eq!(got.translation.engine, TranslationEngine::OpenAi);
    assert_eq!(got.translation.deepl.api_key, "fx-1");
    assert_eq!(got.translation.openai.model, "qwen2.5:7b");

    assert_eq!(got.vendors.len(), 1, "旧地址折成一条档案");
    let vendor = &got.vendors[0];
    assert_eq!(vendor.base_url, "https://example.test/v1");
    assert_eq!(vendor.api_key, "sk-1");
    assert_eq!(vendor.name, "example.test", "档案名默认取 host");
    assert_eq!(
        got.translation.openai.vendor, vendor.id,
        "翻译的引用指向折出来的那条"
    );
    assert!(
        got.translation.openai.base_url.is_empty() && got.translation.openai.api_key.is_empty(),
        "内联的两格折完就清空，重跑迁移才找不到东西"
    );
}

/// 迁移要**幂等**：折过的配置存出去再读回来，档案不翻倍。
#[test]
fn vendor_migration_is_idempotent() {
    let first = load(
        r#"{"translation":{"openai":{"base_url":"https://example.test/v1","model":"m","api_key":"k"}}}"#,
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    first.save_to(&path).unwrap();
    let second = Config::load_from(&path);
    assert_eq!(second.vendors.len(), 1, "重读不再造第二条");
    assert_eq!(
        second.translation.openai.vendor, first.translation.openai.vendor,
        "引用原样保住"
    );
}

/// 出厂地址 + 没填 key =「读者从没打开过这一格」⇒ 迁移**不造档案**，
/// 也不把出厂地址烤成一条真配置 —— 那等于替读者决定正文可以发去官方 API。
#[test]
fn the_shipped_endpoint_without_a_key_does_not_become_a_vendor() {
    let got = load(
        r#"{"translation":{"openai":{"base_url":"https://api.openai.com/v1","model":"gpt-4o-mini","api_key":""}}}"#,
    );
    assert!(got.vendors.is_empty());
    assert!(got.translation.openai.vendor.is_empty());
    assert!(got.translation.openai.base_url.is_empty(), "内联格照旧清空");
}

/// 同一份出厂地址，**贴了 key** 就是一条真配置 —— 折成档案，密钥跟着走。
#[test]
fn the_shipped_endpoint_with_a_key_becomes_a_vendor() {
    let got = load(
        r#"{"translation":{"openai":{"base_url":"https://api.openai.com/v1","model":"gpt-4o-mini","api_key":"sk-1"}}}"#,
    );
    assert_eq!(got.vendors.len(), 1);
    assert_eq!(got.vendors[0].api_key, "sk-1");
    assert_eq!(got.translation.openai.vendor, got.vendors[0].id);
}

/// 老配置里只有 `target_lang`：缺的两节各自取自己的默认值，引擎落到免 key 档。
#[test]
fn a_first_phase_config_gains_the_engine_defaults() {
    let got = load(r#"{"translation":{"target_lang":"en"}}"#);
    assert_eq!(got.translation.engine, TranslationEngine::Google);
    assert!(got.translation.openai.vendor.is_empty());
    assert_eq!(
        got.translation.openai.model,
        crate::infra::config::OpenAi::default().model
    );
    assert!(got.vendors.is_empty());
    assert!(got.translation.deepl.api_key.is_empty());
}

/// 认不出的引擎名回落 Google，**不连累同一节里的地址、模型与密钥**：
/// 手改坏一个字符串的代价不该是整份翻译配置归零。
#[test]
fn an_unknown_engine_name_falls_back_without_taking_the_section_with_it() {
    let got = load(
        r#"{"translation":{
            "target_lang":"ja",
            "engine":"polyglot",
            "deepl":{"api_key":" k "}
        }}"#,
    );
    assert_eq!(got.translation.engine, TranslationEngine::Google);
    assert_eq!(
        got.translation.target_lang, "ja",
        "写坏引擎名不该带走别的格"
    );
    assert_eq!(got.translation.deepl.api_key, "k");
}

/// 清空模型**不能**在归一里被默认值填回去：设置页边打边存，
/// 填回去等于读者删不空这一格。空值该不该有默认由引擎发请求前判。
#[test]
fn clearing_openai_fields_keeps_them_empty() {
    let got = load(r#"{"translation":{"openai":{"vendor":"","model":""}}}"#);
    assert!(got.translation.openai.vendor.is_empty());
    assert!(got.translation.openai.model.is_empty());
}

/// 供应商档案的 JSON 往返：新格式的三节（vendors / openai.vendor / ai.vendor）
/// 各自读得回来，空白在归一里去掉。
#[test]
fn vendors_and_their_references_round_trip() {
    let got = load(
        r#"{
            "vendors":[
                {"id":" v1 ","name":" DeepSeek ","base_url":" https://api.deepseek.com/v1 ","api_key":" sk-1 "},
                {"id":"v2","name":"自建网关","base_url":"https://gw.local/v1","api_key":""}
            ],
            "translation":{"engine":"openai","openai":{"vendor":" v1 ","model":" deepseek-chat "}},
            "ai":{"enabled":true,"vendor":" v2 ","model":" big-model "}
        }"#,
    );
    assert_eq!(got.vendors.len(), 2);
    assert_eq!(got.vendors[0].id, "v1");
    assert_eq!(got.vendors[0].name, "DeepSeek");
    assert_eq!(got.vendors[0].base_url, "https://api.deepseek.com/v1");
    assert_eq!(got.translation.openai.vendor, "v1");
    assert_eq!(got.ai.vendor, "v2", "AI 可以指向与翻译不同的一条");
    assert!(got.vendors[1].api_key.is_empty(), "空 key 合法（自建服务）");
}

/// 引用悬空**回退第一条**而不是报错：档案被删后引用它的功能自动落到列表头上。
/// 空表回 `None` —— 那是「还没添加过」，归「这一格不存在」的判据管。
#[test]
fn a_dangling_vendor_reference_falls_back_to_the_first() {
    use crate::infra::config::{Vendor, resolve_vendor};
    let vendors = vec![
        Vendor {
            id: "v1".into(),
            name: "第一".into(),
            base_url: "https://a.test/v1".into(),
            api_key: String::new(),
        },
        Vendor {
            id: "v2".into(),
            name: "第二".into(),
            base_url: "https://b.test/v1".into(),
            api_key: String::new(),
        },
    ];
    assert_eq!(resolve_vendor(&vendors, "v2").unwrap().id, "v2", "命中照常");
    assert_eq!(
        resolve_vendor(&vendors, "gone").unwrap().id,
        "v1",
        "悬空回退第一条"
    );
    assert_eq!(
        resolve_vendor(&vendors, "").unwrap().id,
        "v1",
        "空串也走回退"
    );
    assert!(
        resolve_vendor(&[], "v1").is_none(),
        "空表是「还没配置」，没有可回退的东西"
    );
}

/// 配置文件里住着 API Key：默认 umask 下新建文件是 0644，落盘必须收到 0600。
#[cfg(unix)]
#[test]
fn a_saved_config_is_readable_only_by_its_owner() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    Config::default().save_to(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "配置文件带着 API Key，权限该是 0600");
}

/// §2.1：文件里写了的项记成 `File`，没写的是 `Default`。
#[test]
fn provenance_tells_file_from_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"reader":{"font_size":21.0}}"#).unwrap();

    let resolved = Config::resolve(&path, &Env::default());
    assert_eq!(
        resolved.provenance.layer_of("reader.font_size"),
        Layer::File
    );
    assert_eq!(
        resolved.provenance.layer_of("reader.line_height"),
        Layer::Default,
        "文件里没写的项是内置默认"
    );
    assert_eq!(resolved.effective.reader.font_size, 21.0);
}

/// §2.1：环境变量只盖它负责的键、并记成 `Env`；**用户层不动**——落盘写的是用户层，
/// 所以环境变量的值不会被烤回文件。
#[test]
fn env_overrides_do_not_leak_into_the_user_layer() {
    let env = Env {
        ignore_file: false,
        log_off: true,
        log: None,
    };
    let resolved = Config::resolve(Path::new("/nonexistent/folio.json"), &env);
    assert!(!resolved.effective.logging.file, "环境变量把日志关掉了");
    assert!(
        resolved.user.logging.file,
        "用户层没被环境变量改到（它才是落盘内容）"
    );
    assert_eq!(resolved.provenance.layer_of("logging.file"), Layer::Env);
}

/// §2.1：`FOLIO_LOG` 压着 logging 那两格 —— provenance 必须说实话。它不在 `resolve`
/// 里盖值（RUST_LOG 写法落不进 `logging.level`），但「这两格归环境变量说了算」要标出来，
/// 否则读者会以为 `logging.level` 的「生效值」就是最终生效的。
#[test]
fn the_log_env_marks_the_logging_keys_as_env() {
    let env = Env {
        ignore_file: false,
        log_off: false,
        log: Some("debug,wgpu_core=error".into()),
    };
    let resolved = Config::resolve(Path::new("/nonexistent/folio.json"), &env);
    assert_eq!(resolved.provenance.layer_of("logging.level"), Layer::Env);
    assert_eq!(resolved.provenance.layer_of("logging.targets"), Layer::Env);
    assert_eq!(
        resolved.provenance.layer_of("reader.font_size"),
        Layer::Default,
        "只管日志那两格，不碰别的"
    );
}

/// §2.1：忽略配置文件的应急开关跳过文件层，全用默认 + 环境变量。
#[test]
fn the_emergency_switch_skips_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"reader":{"font_size":40.0}}"#).unwrap();

    let env = Env {
        ignore_file: true,
        log_off: false,
        log: None,
    };
    let resolved = Config::resolve(&path, &env);
    assert_eq!(
        resolved.effective.reader.font_size,
        Reader::default().font_size,
        "文件被跳过"
    );
    assert_eq!(
        resolved.provenance.layer_of("reader.font_size"),
        Layer::Default
    );
}

#[test]
fn config_roundtrips_through_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("folio/config.json");

    let config = Config {
        reader: Reader {
            font_size: 21.0,
            ..Default::default()
        },
        appearance: Appearance {
            mode: Mode::Light,
            // 这一格默认是 `None`，写个 `Some` 才算测到「Optional<Mode> 真写出去读回来」。
            reading_mode: Some(Mode::Dark),
            light: Some("Gruvbox".into()),
            dark: Some("Molokai".into()),
            // 故意不是默认值：这一趟要把新键真写出去再读回来，填默认值等于没测到它。
            reading_theme: Some("sepia".into()),
            page_turn: PageTurn::SlideUp,
            auto_advance_speed: 24.0,
            turn_duration: 420.0,
            highlight_style: AnnotationStyle::Wavy,
            brightness: Some(0.6),
        },
        language: Language { locale: Locale::Ja },
        ..Default::default()
    };
    config.save_to(&path).unwrap();

    let got = Config::load_from(&path);
    assert_eq!(got, config, "写出去再读回来必须逐字段相等");
}

#[test]
fn a_missing_file_yields_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let got = Config::load_from(&dir.path().join("does-not-exist.json"));
    assert_eq!(got, Config::default());
}

#[test]
fn a_corrupt_file_yields_defaults_instead_of_failing() {
    assert_eq!(load("{ not json at all"), Config::default());
    assert_eq!(load(""), Config::default());
}

/// 缺哪个键就用哪个键的默认值：加一节不该让老配置文件读不出来。
#[test]
fn missing_sections_and_keys_fall_back_per_key() {
    let got = load(r#"{"reader":{"font_size":21.0},"appearance":{"mode":"dark"}}"#);
    assert_eq!(got.reader.font_size, 21.0);
    assert_eq!(got.appearance.mode, Mode::Dark);
    assert_eq!(
        got.appearance.light, None,
        "没选过主题名就是用框架自带的那套，不是空串"
    );
    assert_eq!(got.reader.line_height, 1.75);
    assert!(
        got.library.dir.is_empty(),
        "没确认过书库位置就是空串，不是某个默认目录"
    );
}

/// 背景图那一节的归一：非有限与越界的不透明度退回能用的档，空文件名按「没配」算
/// —— 与 `Appearance` 里主题名那两条同一口径。没有这一节的老配置照常读。
#[test]
fn a_reading_background_entry_is_normalized() {
    let got =
        load(r#"{"reading_backgrounds":{"sepia":{"light":"abc.png","dark":"","opacity":7.0}}}"#);
    let background = &got.reading_backgrounds["sepia"];
    assert_eq!(background.light.as_deref(), Some("abc.png"));
    assert_eq!(background.dark, None, "空文件名不是一张图");
    assert_eq!(
        background.opacity,
        ThemeBackground::OPACITY_FULL,
        "越界归满"
    );

    // 非有限值 JSON 写不出来（"NaN" 是个字符串，整键在 serde 那层就回落默认），
    // 只能从程序路径进来 —— 直接构造一份过归一，钉住「坏数据不当成选择」。
    let mut backgrounds = BTreeMap::new();
    backgrounds.insert(
        "sepia".to_string(),
        ThemeBackground {
            opacity: f32::NAN,
            ..ThemeBackground::default()
        },
    );
    let nan = Config {
        reading_backgrounds: backgrounds,
        ..Config::default()
    }
    .normalized();
    assert_eq!(
        nan.reading_backgrounds["sepia"].opacity,
        ThemeBackground::OPACITY_FULL,
    );
}

/// 老版本的 `reader` 一节里有 `theme`；新版在 `appearance`。
/// 认不出来的字段 serde 会直接忽略，于是主题回到框架默认而不是报错。
#[test]
fn a_stale_theme_key_is_ignored_instead_of_failing() {
    let got = load(r#"{"reader":{"theme":"Folio Sepia"}}"#);
    assert_eq!(got.appearance.light, None);
    assert_eq!(got.appearance.mode, Mode::System);
}

/// 老版本的 `reader` 一节里有 `locale`；新版在 language 一节。
/// 认不出来的字段 serde 会直接忽略，于是语言回到默认（简体中文）而不是报错。
#[test]
fn an_unknown_locale_key_falls_back_to_the_default_language() {
    let got = load(r#"{"reader":{"locale":"fr"}}"#);
    assert_eq!(got.language.locale, Locale::ZhHans);
}

/// `language` 一节缺 locale 键时用的是缺省语言（简体中文），所以这里显式挑一个
/// 与缺省不同的值，让「读到了」和「回落了」不可能同值。
///
/// JSON 用 `json!` 现搭，不手写转义花括号：这一条测试曾经因为多打的一个引号
/// 变成了「整份 JSON 无效」，于是要验的 locale 从没被解过一遍。
#[test]
fn the_locale_survives_the_json_round_trip() {
    let different = Locale::En;

    // 界面上看到的就是 `code()`，读者照它手改，所以这一种写法是主路径。
    let got = load(&json!({ "language": { "locale": different.code() } }).to_string());
    assert_eq!(got.language.locale, different);

    // 老配置（derive serde 时代）里存的是变体名。这条同时钉住另一件事：
    // 认不出来时只该坏掉 language 一节，`font_size` 必须还在 —— 如果 locale
    // 让整份解析失败，退回默认的就是 17.0。
    let legacy = load(
        &json!({
            "language": { "locale": format!("{different:?}") },
            "reader": { "font_size": 21.0 },
        })
        .to_string(),
    );
    assert_eq!(
        legacy.language.locale, different,
        "derive 时代写出去的变体名必须继续认得"
    );
    assert_eq!(legacy.reader.font_size, 21.0);
}
