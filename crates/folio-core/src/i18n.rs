//! 界面语言：`Locale` 是「用户选了哪种语言」这件事的表示，
//! 文案本身在 `locales/{en,zh-CN,ja}.toml`（一门语言一份，**文件名就是语言标签**），
//! 由 `rust_i18n` 在编译期嵌进来（见 `lib.rs` 的 `i18n!`）。
//!
//! 为什么交给库而不是自己写一张表：`rust-i18n` 就是 gpui-kit 组件层自己在用的那套
//! （`gpui-component/src/lib.rs` 里一句 `i18n!("locales", fallback = "en")`），
//! 用它意味着 `Settings` 搜索框、下拉框这些框架文案能和 Folio 的文案同步切换，
//! 而且加一条文案只是往三份 TOML 各加一行，不用同时改枚举、`ALL` 数组和三列大表。
//!
//! 两个必须知道的取舍：
//! - **key 写错不再是编译期错误**。库里查不到的 key 会原样显示成 `page.foo`，
//!   看得见但太晚，所以本文件的 `tests` 模块会拿三种语言的 key 集合互相比对。
//! - **全局只有一份 locale**（`rust_i18n::set_locale`）。真正的来源仍是
//!   `Config.language.locale`，改配置的人负责同步一次（见 `ConfigHolder`）。

use std::fmt::Display;

use serde::{Deserialize, Serialize};

/// 界面语言。`Default` 是简体中文：这个项目的正文排版以中文书为第一目标。
///
/// **JSON 形式由下面的 `Serialize` / `Deserialize` 手写，不用 derive。** derive 会把
/// 变体名（`En` / `ZhHans`）当唯一合法值，而界面上、`locales/` 那三份的文件名里、
/// 以及 `code()` 给的标签都是 `en` / `zh-CN` / `ja` —— 读者照自己看到的写法手改
/// `config.json` 就会读失败。失败在这里不是小事：`Config::load_from` 读不出来时会把
/// **整节** `language` 退回默认值（也就是「跟随系统」），于是读者刚选的语言被无声改掉，
/// 界面下一次打开还是他看不懂的那门语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Locale {
    #[default]
    ZhHans,
    En,
    Ja,
}

impl Serialize for Locale {
    /// 存出去的必须是 `code()`：与界面上的标签、与 `locales/` 那三份的文件名同一形态。
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code())
    }
}

impl<'de> Deserialize<'de> for Locale {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Locale::from_label(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!("不认识的语言 {raw:?}；可用的是 zh-CN / en / ja"))
        })
    }
}

/// 比较语言写法用的归一化：去首尾空白、转小写、把 `_` 和 `-` 这类分隔符抹平，
/// 于是 `zh_CN`、`zh-CN`、`ZH-cn` 在比较时是同一个键。
fn normalize(input: &str) -> String {
    input
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .collect()
}

impl Locale {
    pub const ALL: &'static [Locale] = &[Locale::ZhHans, Locale::En, Locale::Ja];

    /// 宽松到「凡是人能写出来的形式都认」：`en`、`en-US`、`EN_us`、以及 BCP-47 的
    /// `zh-Hans-CN`。老配置文件里还可能是 derive 时代的 serde 变体名 `En` / `ZhHans`，
    /// 语言自己的名字 `English` / `简体中文` 同样认 —— 那些文件必须继续读得出来，
    /// 读失败的那一节会退回「跟随系统」，等于把读者选过的语言无声改掉
    /// （见上面的 `Deserialize` 与 `config.rs::salvage`）。
    ///
    /// 与 [`Locale::parse`] 的区别只在覆盖面：`parse` 认的是环境变量的形如
    /// `zh_CN.UTF-8` 的前缀写法，这里还要认标签、变体名和本族名。
    pub fn from_label(input: &str) -> Option<Locale> {
        if let Some(found) = Locale::parse(input) {
            return Some(found);
        }
        let key = normalize(input);
        Locale::ALL.iter().copied().find(|locale| {
            key == normalize(locale.code())
                || key == normalize(&format!("{locale:?}"))
                || key == normalize(locale.endonym())
        })
    }

    /// BCP-47 标签：既是 `locales/<code>.toml` 的文件名，也是设置项的「值」和 `lang` 属性。
    pub fn code(self) -> &'static str {
        match self {
            Locale::ZhHans => "zh-CN",
            Locale::En => "en",
            Locale::Ja => "ja",
        }
    }

    /// 语言名一律用**它自己的语言**写：这是软件界面的通行约定，
    /// 因为用户切换语言往往正是在还看不懂当前界面时发生的。
    pub fn endonym(self) -> &'static str {
        match self {
            Locale::ZhHans => "简体中文",
            Locale::En => "English",
            Locale::Ja => "日本語",
        }
    }

    /// 宽松解析：`zh`、`zh_CN`、`zh-Hans-CN`、`en_US`、`ja_JP` 都能认。
    pub fn parse(input: &str) -> Option<Locale> {
        let s = input.trim().to_ascii_lowercase().replace('_', "-");
        let head = s.split('-').next().unwrap_or("");
        match head {
            "zh" => Some(Locale::ZhHans),
            "en" => Some(Locale::En),
            "ja" => Some(Locale::Ja),
            _ => None,
        }
    }

    /// 跟随系统环境。`LANGUAGE`（glibc 的优先级列表）先看，再看 `LC_ALL`/`LANG`。
    /// 认不出来的语言用默认值，不猜成英文 —— 猜错会让首次启动的界面整体变味。
    pub fn detect() -> Locale {
        for key in ["LANGUAGE", "LC_ALL", "LANG"] {
            let Ok(value) = std::env::var(key) else {
                continue;
            };
            let value = value.split(':').next().unwrap_or("").to_string();
            if value.is_empty() || value.eq_ignore_ascii_case("C") || value == "POSIX" {
                continue;
            }
            if let Some(locale) = Self::parse(&value) {
                return locale;
            }
        }
        Locale::default()
    }

    /// 该语言里 `n` 是否用「单数」那条文案。中日文没有单复数形态变化。
    ///
    /// rust-i18n 4 不做复数，所以「选哪条文案」这件事留在这里由我们判断。
    pub fn plural_one(self, n: usize) -> bool {
        match self {
            Locale::En => n == 1,
            Locale::ZhHans | Locale::Ja => false,
        }
    }

    /// 把当前选择推给 `rust_i18n` 的全局 locale（框架组件读同一个值）。
    pub fn apply(self) {
        rust_i18n::set_locale(self.code());
    }
}

/// 当前生效的界面语言。没同步过配置时（测试、移动端宿主）就是系统语言。
pub fn current() -> Locale {
    Locale::parse(&rust_i18n::locale()).unwrap_or_default()
}

/// 取一条文案（按全局 locale）。`key` 可以是运行期的字符串 —— 宏对非字面量 key 一样成立。
pub fn t(key: &str) -> String {
    rust_i18n::t!(key).into_owned()
}

/// 带参文案（按全局 locale）：`args` 里的名字对应模板里的 `%{名字}`。
pub fn t_with(key: &str, args: &[(&str, &dyn Display)]) -> String {
    t_in(current(), key, args)
}

/// 指定语言的带参文案。
///
/// 单独给一个显式版本有两个用处：测试可以不必改动全局（全局只有一份，
/// 并发跑测试时会互相踩），以及以后「正文语言与界面语言分开」那类需求能直接复用。
pub fn t_in(locale: Locale, key: &str, args: &[(&str, &dyn Display)]) -> String {
    let template =
        crate::_rust_i18n_try_translate(locale.code(), key).unwrap_or_else(|| key.into());
    if args.is_empty() {
        return template.into_owned();
    }
    // 走库自己的替换，`%{x}` 的转义、连续替换这些细节不用再造一遍。
    // 查不到的占位符原样留在结果里，好让漏传参数在界面上看得见。
    let names: Vec<&str> = args.iter().map(|(name, _)| *name).collect();
    let values: Vec<String> = args.iter().map(|(_, value)| value.to_string()).collect();
    rust_i18n::replace_patterns(&template, &names, &values)
}

#[cfg(test)]
mod tests {
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

    use crate::_rust_i18n_backend;
    use crate::i18n::{self, Locale};

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
}
