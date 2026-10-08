//! 应用配置：分节、JSON 往返、缺文件/坏文件回落、读入夹取、语言键。只依赖公开 API。
//!
//! 这些测试盯的是「用户手改这个 JSON 之后程序会怎样」，所以全部从 JSON 文本出发，
//! 不去直接改结构体 —— 结构体改不出坏输入。

use folio_core::config::Config;
use folio_core::i18n::Locale;
use folio_core::{Account, Appearance, Language, Library, Mode, Reader};

fn load(text: &str) -> Config {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, text).unwrap();
    Config::load_from(&path)
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
            light: Some("Gruvbox".into()),
            dark: Some("Molokai".into()),
        },
        language: Language { locale: Locale::Ja },
        account: Account {
            name: "阿遥".into(),
        },
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
    assert!(got.library.dirs.is_empty());
    assert!(got.account.name.is_empty());
}

/// 老版本的 `reader` 一节里有 `theme`；新版在 `appearance`。
/// 认不出来的字段 serde 会直接忽略，于是主题回到框架默认而不是报错。
#[test]
fn a_stale_theme_key_is_ignored_instead_of_failing() {
    let got = load(r#"{"reader":{"theme":"Folio Sepia"}}"#);
    assert_eq!(got.appearance.light, None);
    assert_eq!(got.appearance.mode, Mode::System);
}

/// 老版本把界面语言存在 reader 里；新版在 language 一节。
/// 认不出来的字段 serde 会直接忽略，于是语言回到「跟随系统」而不是报错。
#[test]
fn an_unknown_locale_key_falls_back_to_the_system_language() {
    let got = load(r#"{"reader":{"locale":"fr"}}"#);
    assert_eq!(got.language.locale, Locale::detect());
}

#[test]
fn the_locale_survives_the_json_round_trip() {
    let got = load(r#"{"language":{"locale":"en"}}"#);
    assert_eq!(got.language.locale, Locale::En);
}

/// 手改过的越界值在读入时要被夹住（否则排版会算出零高或无限宽）。
#[test]
fn out_of_range_values_are_clamped_on_load() {
    let got = load(
        r#"{"reader":{"font_size":9000,"line_height":0,"indent_em":-5,"margin_x":1e9},"account":{"name":"x"}}"#,
    );
    assert_eq!(got.reader.font_size, 72.0);
    assert_eq!(got.reader.line_height, 1.0);
    assert_eq!(got.reader.indent_em, 0.0);
    assert_eq!(got.reader.margin_x, 400.0);
    assert_eq!(got.account.name, "x", "夹取只管排版数值，不碰别的节");
}

/// 空白主题名被当成「没设」；0 的 max_measure 是合法的铺满，不能被夹成别的值。
#[test]
fn blank_theme_names_are_dropped_and_fill_width_survives_clamping() {
    let got = load(r#"{"reader":{"max_measure":0},"appearance":{"light":"   ","dark":"Molokai"}}"#);
    assert_eq!(
        got.appearance.light, None,
        "全空白的主题名不该存成一个显示不出来的选项"
    );
    assert_eq!(got.appearance.dark.as_deref(), Some("Molokai"));
    assert_eq!(got.reader.max_measure, 0.0, "0 = 随窗口铺满");
}

/// 认不出的 mode 字符串不该让配置读失败或直接变成深色。
#[test]
fn an_unknown_mode_falls_back_to_following_the_system() {
    assert_eq!(
        load(r#"{"appearance":{"mode":"nonsense"}}"#)
            .appearance
            .mode,
        Mode::System
    );
    assert_eq!(Mode::parse("dark"), Mode::Dark);
    assert_eq!(Mode::default(), Mode::System);
}

#[test]
fn an_empty_font_family_falls_back_instead_of_nuking_the_text() {
    let got = load(r#"{"reader":{"font_family":"   "}}"#);
    assert_eq!(got.reader.font_family, Reader::default().font_family);
}

#[test]
fn the_library_section_holds_its_dirs() {
    let config = Config {
        library: Library {
            dirs: vec!["~/Books".into(), "/srv/reading".into()],
        },
        ..Default::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    config.save_to(&path).unwrap();
    assert_eq!(Config::load_from(&path).library.dirs, config.library.dirs);
}

/// 写文件是「临时文件 + rename」：中途失败不该留下半份坏 JSON。
/// 这里能验证的部分是目标目录不存在时会自动建出来。
#[test]
fn saving_creates_the_parent_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deep/nested/config.json");
    Config::default().save_to(&path).unwrap();
    assert!(path.exists());
}
