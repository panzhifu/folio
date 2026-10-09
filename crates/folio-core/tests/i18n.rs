//! 文案表的形状测试。价值在于**结构性**：不管措辞好不好，
//! 只管有没有漏翻、占位符对不对得上 —— 正是人眼最容易漏过去的两类错。
//!
//! 文案按语言分成 `locales/{en,zh-CN,ja}.toml` 三份之后，漏翻不再是「并排一张表里空着
//! 的那一格」，而是另一个文件里少了一行 —— 三份各自看都像是完整的。所以这两件事只能由
//! 这里来保证，别指望读文件的人发现。
//!
//! 换成 rust-i18n 之后 key 是字符串，写错只能运行时发现（界面上会原样显示
//! `page.foo`），所以这里把「三种语言的 key 集合一致」变成一条会失败的测试，
//! 顶替过去 `Msg` 枚举提供的编译期穷举。
//!
//! 除了 `apply_and_current_agree`，其余测试都走 `t_in`（显式语言）：全局 locale 只有一份，
//! 而 cargo 会把同一文件里的测试跑到不同线程上，改全局的断言会互相踩。

use std::collections::BTreeSet;
use std::fmt::Display;

use folio_core::_rust_i18n_backend;
use folio_core::i18n::{self, Locale};

/// 某个语言下全部 (key, 文案)。少一个语言就报错，而不是悄悄跳过。
fn messages(locale: &str) -> BTreeSet<(String, String)> {
    _rust_i18n_backend()
        .messages_for_locale(locale)
        .unwrap_or_else(|| panic!("locales/ 里没有 {locale}.toml 这一份"))
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// 抽出模板里用到的 `%{名字}` 占位符集合。
fn placeholders(template: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = template;
    while let Some(open) = rest.find("%{") {
        let tail = &rest[open + 2..];
        let Some(close) = tail.find('}') else { break };
        out.insert(tail[..close].to_string());
        rest = &tail[close + 1..];
    }
    out
}

fn at(locale: Locale, key: &str, args: &[(&str, &dyn Display)]) -> String {
    i18n::t_in(locale, key, args)
}

#[test]
fn every_language_carries_the_same_keys() {
    let base = messages(Locale::ZhHans.code());
    assert!(!base.is_empty(), "文案表不该是空的");
    for locale in &Locale::ALL[1..] {
        let other = messages(locale.code());
        let missing: Vec<&String> = base
            .iter()
            .filter(|(key, _)| !other.iter().any(|(k, _)| k == key))
            .map(|(key, _)| key)
            .collect();
        let extra: Vec<&String> = other
            .iter()
            .filter(|(key, _)| !base.iter().any(|(k, _)| k == key))
            .map(|(key, _)| key)
            .collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "{} 与 {} 的 key 集合不一致，缺 {missing:?}，多 {extra:?}",
            locale.code(),
            Locale::ZhHans.code()
        );
    }
}

#[test]
fn no_string_is_blank() {
    for locale in Locale::ALL {
        for (key, value) in messages(locale.code()) {
            assert!(
                !value.trim().is_empty(),
                "{key} 的 {} 文案是空的",
                locale.code()
            );
        }
    }
}

/// 漏翻一个 `%{total}` 是最隐蔽的 bug：界面照样出字，只是少了个数字。
#[test]
fn placeholders_match_across_languages() {
    let base = messages(Locale::ZhHans.code());
    for (key, zh) in &base {
        let want = placeholders(zh);
        for locale in &Locale::ALL[1..] {
            let text = messages(locale.code())
                .into_iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v)
                .expect("key 集合那条测试已经保证它存在");
            assert_eq!(
                want,
                placeholders(&text),
                "{key} 的 中文/{} 占位符不一致",
                locale.code()
            );
        }
    }
}

#[test]
fn interpolation_is_by_name_not_by_order() {
    // 中文「第 %{index} / %{total} 节」与英文 "Section %{index} of %{total}" 语序不同，
    // 靠名字替换才不会被语序带跑。
    let args: &[(&str, &dyn Display)] = &[("index", &3usize), ("total", &12usize)];
    assert_eq!(
        at(Locale::ZhHans, "reader.chapter_position", args),
        "第 3 / 12 节"
    );
    assert_eq!(
        at(Locale::En, "reader.chapter_position", args),
        "Section 3 of 12"
    );
    assert_eq!(
        at(Locale::Ja, "reader.chapter_position", args),
        "第3 / 12 節"
    );
}

#[test]
fn a_missing_argument_stays_visible() {
    let args: &[(&str, &dyn Display)] = &[("index", &7usize)];
    let out = at(Locale::ZhHans, "reader.chapter_position", args);
    assert!(
        out.contains("%{total}"),
        "漏传参数要把占位符留在原地，实际：{out}"
    );
}

#[test]
fn an_unknown_key_shows_itself_instead_of_nothing() {
    assert_eq!(
        at(Locale::ZhHans, "page.this_key_does_not_exist", &[]),
        "page.this_key_does_not_exist",
        "写错的 key 要在界面上认得出来，不能变成空白"
    );
}

#[test]
fn apply_and_current_agree() {
    // 唯一一条碰全局 locale 的测试：应用侧「改配置 → 界面换语言」靠的就是这一步。
    for locale in Locale::ALL {
        locale.apply();
        assert_eq!(i18n::current(), *locale, "{} 没被全局接受", locale.code());
    }
    Locale::ZhHans.apply();
}

#[test]
fn locale_parsing_is_lenient() {
    assert_eq!(Locale::parse("zh_CN.UTF-8"), Some(Locale::ZhHans));
    assert_eq!(Locale::parse("zh-Hans-CN"), Some(Locale::ZhHans));
    assert_eq!(Locale::parse("en"), Some(Locale::En));
    assert_eq!(Locale::parse("  JA_jp "), Some(Locale::Ja));
    assert_eq!(Locale::parse("fr"), None);
    assert_eq!(Locale::parse(""), None);
}

#[test]
fn codes_round_trip_through_parse() {
    for locale in Locale::ALL {
        assert_eq!(Locale::parse(locale.code()), Some(*locale));
    }
}

/// 配置文件里人能写出来的形式，以及 **derive serde 时代写进老配置里的变体名**。
/// 老文件必须继续认得：读失败的那一节会退回「跟随系统」，等于把读者选过的语言无声改掉。
#[test]
fn from_label_accepts_every_spelling_a_human_or_an_old_version_writes() {
    for (input, want) in [
        ("en", Locale::En),
        ("EN", Locale::En),
        ("English", Locale::En),
        ("En", Locale::En),
        ("en-US", Locale::En),
        ("en_US", Locale::En),
        ("zh-CN", Locale::ZhHans),
        ("zh-cn", Locale::ZhHans),
        ("zh_CN", Locale::ZhHans),
        ("简体中文", Locale::ZhHans),
        ("ZhHans", Locale::ZhHans),
        ("ja", Locale::Ja),
        ("JA_jp", Locale::Ja),
        ("日本語", Locale::Ja),
        ("Ja", Locale::Ja),
    ] {
        assert_eq!(
            Locale::from_label(input),
            Some(want),
            "{input:?} 应该认成 {want:?}"
        );
    }
    assert_eq!(Locale::from_label("fr"), None);
    assert_eq!(Locale::from_label(""), None);
    assert_eq!(Locale::from_label("   "), None);
}

/// 存出去的必须是 `code()`：与界面标签、与 `locales/` 那三份的文件名同一形态。
#[test]
fn locale_serializes_as_its_bcp47_code() {
    for locale in Locale::ALL.iter().copied() {
        let json = serde_json::to_string(&locale).unwrap();
        assert_eq!(
            json,
            format!("{:?}", locale.code()),
            "存出去的应该是 {:?}，不是变体名",
            locale.code()
        );
        assert_eq!(serde_json::from_str::<Locale>(&json).unwrap(), locale);
    }
    // 写只写 code()，读却宽松：老文件里的变体名照样能读回来。
    assert_eq!(
        serde_json::from_str::<Locale>(r#""En""#).unwrap(),
        Locale::En
    );
    assert_eq!(
        serde_json::from_str::<Locale>(r#""ZhHans""#).unwrap(),
        Locale::ZhHans
    );
    // 认不出的值要**报错**，好让 `Config::load_from` 的分节补救接管 ——
    // 静默回落某一个语言会把「跟随系统」和「简体中文」混成一件事。
    assert!(serde_json::from_str::<Locale>(r#""klingon""#).is_err());
    assert!(serde_json::from_str::<Locale>("3").is_err());
}

#[test]
fn unknown_system_locale_falls_back_to_the_default() {
    // detect 读环境变量，所以只能断言「它一定给出三种之一」而不是具体某个。
    assert!(Locale::ALL.contains(&Locale::detect()));
}

#[test]
fn only_english_distinguishes_singular_from_plural() {
    assert!(Locale::En.plural_one(1));
    assert!(!Locale::En.plural_one(2));
    assert!(!Locale::ZhHans.plural_one(1));
    assert!(!Locale::Ja.plural_one(1));
    // 库不做复数，所以「选单数那条」之后还得自己填对：这条测试盯着那一步。
    assert_eq!(
        at(Locale::En, "reader.book_count_one", &[("count", &1usize)]),
        "1 book",
        "单数那条要真的写成单数"
    );
    assert_eq!(
        at(Locale::En, "reader.book_count_other", &[("count", &2usize)]),
        "2 books"
    );
    assert_eq!(
        at(Locale::Ja, "reader.book_count_other", &[("count", &2usize)]),
        "2 冊"
    );
}

#[test]
fn language_names_are_written_in_their_own_language() {
    assert_eq!(Locale::ZhHans.endonym(), "简体中文");
    assert_eq!(Locale::En.endonym(), "English");
    assert_eq!(Locale::Ja.endonym(), "日本語");
}
