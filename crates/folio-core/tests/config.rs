//! 应用配置：分节、JSON 往返、缺文件/坏文件回落、读入夹取、语言键。只依赖公开 API。
//!
//! 这些测试盯的是「用户手改这个 JSON 之后程序会怎样」，所以全部从 JSON 文本出发，
//! 不去直接改结构体 —— 结构体改不出坏输入。

use folio_core::config::Config;
use folio_core::i18n::Locale;
use folio_core::{Account, Appearance, Language, Library, Mode, Reader};
use serde_json::json;

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

/// 这一条必须**与运行环境无关**。`language` 一节缺 locale 键时用的是
/// `Locale::detect()`，而 detect 读 `LANGUAGE`/`LC_ALL`/`LANG`，所以原来那句
/// 「`"en"` 读回来是 `En`」在 `LANG=en_US.UTF-8` 的机器上，即便 JSON 里的语言
/// 完全没被读到（整节退回 detect）也一样通过 —— 本机全绿、CI 上红的就是它。
/// 这里显式挑一个**与本机系统语言不同**的值，让「读到了」和「回落了」不可能同值。
///
/// JSON 用 `json!` 现搭，不手写转义花括号：这一条测试曾经因为多打的一个引号
/// 变成了「整份 JSON 无效」，于是要验的 locale 从没被解过一遍。
#[test]
fn the_locale_survives_the_json_round_trip() {
    let different = Locale::ALL
        .iter()
        .copied()
        .find(|locale| *locale != Locale::detect())
        .expect("三种语言里总能找到一个和系统语言不同的");

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

/// 一处类型写错的字段只该毁掉**它自己那一节**：读者手改 `"font_size":"big"` 之后，
/// 不该看到主题名、书库目录、读者名一起消失，而且没有任何提示。
#[test]
fn one_broken_section_does_not_take_the_others_down() {
    let got = load(
        r#"{"reader":{"font_size":"big","line_height":2.5},
            "appearance":{"mode":"dark","light":"Gruvbox"},
            "library":{"dirs":["~/Books"]},
            "account":{"name":"阿遥"}}"#,
    );
    assert_eq!(
        got.reader,
        Reader::default(),
        "坏的是 reader 一节，就只有这一节退回默认"
    );
    assert_eq!(got.appearance.mode, Mode::Dark);
    assert_eq!(got.appearance.light.as_deref(), Some("Gruvbox"));
    assert_eq!(got.library.dirs, vec!["~/Books".to_string()]);
    assert_eq!(got.account.name, "阿遥");
}

/// 补救的粒度是「一节」而不是「一个键」：`line_height` 没写错，但它的同节邻居错了，
/// 于是整节一起退回。serde 的字段级默认只在整节解成功时才有机会生效，
/// 这里把它明确成预期行为，而不是让人以为是 bug。
#[test]
fn the_good_keys_in_a_broken_section_fall_back_too() {
    let got = load(r#"{"reader":{"font_size":"big","line_height":2.5}}"#);
    assert_eq!(got.reader.line_height, Reader::default().line_height);
}

/// 连 JSON 本身都解析不了（截断、括号不配对）时没有「节」可以逐节尝试，
/// 仍然整份退回默认值。
#[test]
fn unparseable_json_still_falls_back_as_a_whole() {
    assert_eq!(load(r#"{"reader":{"font_size":21.0}"#), Config::default());
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
