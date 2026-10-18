//! 界面语言：`Locale` 是「用户选了哪种语言」这件事的表示，
//! 文案本身在 `locales/<语言标签>.toml`（一门语言一份，**文件名就是语言标签**），
//! 由 `rust_i18n` 在编译期嵌进来（见 `lib.rs` 的 `i18n!`）。
//!
//! 文案表在 **crate 根**的 `locales/`，不在这个目录里：宏的路径是相对
//! `CARGO_MANIFEST_DIR` 解，不是相对本文件，所以模块搬进 `infra/` 而文案表不动。
//!
//! 为什么交给库而不是自己写一张表：`rust-i18n` 就是 gpui-kit 组件层自己在用的那套
//! （`gpui-component/src/lib.rs` 里一句 `i18n!("locales", fallback = "en")`），
//! 用它意味着 `Settings` 搜索框、下拉框这些框架文案能和 Folio 的文案同步切换，
//! 而且加一条文案只是往每份 TOML 各加一行，不用同时改枚举、`ALL` 数组和一张按语言分列的大表。
//!
//! 两个必须知道的取舍：
//! - **key 写错不再是编译期错误**。库里查不到的 key 会原样显示成 `page.foo`，
//!   看得见但太晚，所以本文件的 `tests` 模块会拿各门语言的 key 集合互相比对。
//! - **全局只有一份 locale**（`rust_i18n::set_locale`）。真正的来源仍是
//!   `Config.language.locale`，改配置的人负责同步一次（见 `ConfigHolder`）。

use std::fmt::Display;

use serde::{Deserialize, Serialize};

/// 界面语言。`Default` 是简体中文：这个项目的正文排版以中文书为第一目标。
///
/// `ALL` 里的顺序就是语言下拉框里的顺序：本项目的主要读者群在前（简中、繁中、英文、日文、
/// 韩文），欧洲语言按字母跟在后面。加一门语言要做两件事 —— 往 `locales/` 放一份以语言标签
/// 命名的 TOML，在这里加一个变体并把 `code()` / `endonym()` / `parse()` / `plural()` 的分支
/// 补齐。少放那份 TOML 不会静默少一种语言：`every_language_carries_the_same_keys` 会直接报错。
///
/// **JSON 形式由下面的 `Serialize` / `Deserialize` 手写，不用 derive。** derive 会把
/// 变体名（`En` / `ZhHans`）当唯一合法值，而界面上、`locales/` 那些文件的文件名里、
/// 以及 `code()` 给的标签都是 `en` / `zh-CN` / `ja` —— 读者照自己看到的写法手改
/// `config.json` 就会读失败。失败在这里不是小事：`Config::load_from` 读不出来时会把
/// **整节** `language` 退回默认值（简体中文），于是读者刚选的语言被无声改掉，
/// 界面下一次打开还是他看不懂的那门语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Locale {
    #[default]
    ZhHans,
    ZhHant,
    En,
    Ja,
    Ko,
    Fr,
    De,
    Es,
    Ru,
}

impl Serialize for Locale {
    /// 存出去的必须是 `code()`：与界面上的标签、与 `locales/` 那个文件名同一形态。
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code())
    }
}

impl<'de> Deserialize<'de> for Locale {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Locale::from_label(&raw).ok_or_else(|| {
            // 可用清单从 `ALL` 现取。这里曾经硬编码成 `zh-CN / en / ja`：那句话是读者手改
            // 配置出错时唯一能看到的提示，语言加过去了而提示没跟上，等于把可用值说错。
            let available = Locale::available_labels();
            serde::de::Error::custom(format!("不认识的语言 {raw:?}；可用的是 {available}"))
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

/// `zh` 打头的标签里挑简体还是繁体：后面的子签出现 `hant*` / `tw` / `hk` / `mo` 就算繁体，
/// 其余（`hans` / `cn` / `sg` / `my`，以及光秃秃的 `zh`）算简体。
///
/// 这一步不能省成「`zh` 前缀一律简体」：`zh-TW`、`zh-HK` 是现实里会有人手写进配置或系统
/// 语言里给出的标签，折成简体等于把看得懂繁体的人无声降级 —— 而这一节要是读失败，退回的
/// 还是简体中文，一句反馈都没有（见 `Deserialize`）。裸 `zh` 没有别的线索可依据，仍走简体。
fn chinese_variant(tag: &str) -> Locale {
    let traditional = tag
        .split('-')
        .skip(1)
        .any(|part| part.starts_with("hant") || matches!(part, "tw" | "hk" | "mo"));
    if traditional {
        Locale::ZhHant
    } else {
        Locale::ZhHans
    }
}

impl Locale {
    /// 下拉框、错误提示与测试遍历用的全部语言。前面是本项目的主要读者群，
    /// 后面按语言自己的名字排 —— 这个顺序就是界面上看得到的顺序。
    pub const ALL: &'static [Locale] = &[
        Locale::ZhHans,
        Locale::ZhHant,
        Locale::En,
        Locale::Ja,
        Locale::Ko,
        Locale::Fr,
        Locale::De,
        Locale::Es,
        Locale::Ru,
    ];

    /// 可用语言标签的清单，供报错与日志用：由 [`Locale::ALL`] 现拼，
    /// 这样加一门语言不用再改第二处文案。
    pub fn available_labels() -> String {
        Locale::ALL
            .iter()
            .map(|locale| locale.code())
            .collect::<Vec<_>>()
            .join(" / ")
    }

    /// 宽松到「凡是人能写出来的形式都认」：`en`、`en-US`、`EN_us`、BCP-47 的
    /// `zh-Hans-CN`、`ko_KR`、`ru-RU`，以及繁体中文的 `zh-TW` / `zh-Hant-HK`。
    /// 老配置文件里还可能是 derive 时代的 serde 变体名 `En` / `ZhHans`，
    /// 语言自己的名字 `English` / `简体中文` 同样认 —— 那些文件必须继续读得出来，
    /// 读失败的那一节会退回默认值（简体中文），等于把读者选过的语言无声改掉
    /// （见上面的 `Deserialize` 与 `config.rs::salvage`）。
    ///
    /// 与 [`Locale::parse`] 的区别只在覆盖面：`parse` 认的是语言标签那种写法
    /// （BCP-47 的子签就够它判），这里还要认变体名和本族名。
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
            Locale::ZhHant => "zh-TW",
            Locale::En => "en",
            Locale::Ja => "ja",
            Locale::Ko => "ko",
            Locale::Fr => "fr",
            Locale::De => "de",
            Locale::Es => "es",
            Locale::Ru => "ru",
        }
    }

    /// 语言名一律用**它自己的语言**写：这是软件界面的通行约定，
    /// 因为用户切换语言往往正是在还看不懂当前界面时发生的。
    pub fn endonym(self) -> &'static str {
        match self {
            Locale::ZhHans => "简体中文",
            Locale::ZhHant => "繁體中文",
            Locale::En => "English",
            Locale::Ja => "日本語",
            Locale::Ko => "한국어",
            Locale::Fr => "Français",
            Locale::De => "Deutsch",
            Locale::Es => "Español",
            Locale::Ru => "Русский",
        }
    }

    /// 宽松解析：`zh`、`zh_CN`、`zh-Hans-CN`、`zh_TW`、`en_US`、`ja_JP`、`ko_KR`、`ru-RU` 都能认。
    pub fn parse(input: &str) -> Option<Locale> {
        let s = input.trim().to_ascii_lowercase().replace('_', "-");
        let head = s.split('-').next().unwrap_or("");
        match head {
            "zh" => Some(chinese_variant(&s)),
            "en" => Some(Locale::En),
            "ja" => Some(Locale::Ja),
            "ko" => Some(Locale::Ko),
            "fr" => Some(Locale::Fr),
            "de" => Some(Locale::De),
            "es" => Some(Locale::Es),
            "ru" => Some(Locale::Ru),
            _ => None,
        }
    }

    /// `n` 在这个语言里归哪一类复数 —— 决定取文案表里 `_one` / `_few` / `_many` / `_other`
    /// 后缀的那一条。
    ///
    /// 判据取 CLDR 的整数规则，不是「只有英文有单复数」：`fr` 把 0 也算单数（`0 livre`，
    /// 但 `2 livres`），`ru` 更是要三类 —— 一本书 `книга`、两本 `книги`、五本 `книг`，
    /// 只备两条文案必然有一句是错话。中文（繁简）、日文、韩文没有数的形态变化，恒为 `Other`。
    ///
    /// rust-i18n 4 不做复数，所以「选哪一条」这件事留在这里由我们自己判。
    pub fn plural(self, n: usize) -> Plural {
        let m10 = n % 10;
        let m100 = n % 100;
        match self {
            Locale::ZhHans | Locale::ZhHant | Locale::Ja | Locale::Ko => Plural::Other,
            Locale::En | Locale::De | Locale::Es => {
                if n == 1 {
                    Plural::One
                } else {
                    Plural::Other
                }
            }
            Locale::Fr => {
                if n == 0 || n == 1 {
                    Plural::One
                } else {
                    Plural::Other
                }
            }
            // 三条按 CLDR 的顺序判：先 one，再 few，再 many，剩下的 other。
            // 落不到前三条的俄语 `n` 只有「十分位是 1 且 11~14 之外」这一族的补集，
            // 而那条已经在 `One` 用掉 —— 留 `Other` 是为了这个 `match` 保持穷举，
            // 实际取值范围里它是不可达的一臂。
            Locale::Ru if m10 == 1 && m100 != 11 => Plural::One,
            Locale::Ru if (2..=4).contains(&m10) && !(12..=14).contains(&m100) => Plural::Few,
            Locale::Ru if m10 == 0 || (5..=9).contains(&m10) || (11..=14).contains(&m100) => {
                Plural::Many
            }
            Locale::Ru => Plural::Other,
        }
    }

    /// 把当前选择推给 `rust_i18n` 的全局 locale（框架组件读同一个值）。
    pub fn apply(self) {
        rust_i18n::set_locale(self.code());
    }
}

/// CLDR 的整数复数类别，只列文案表里真备了文案的那四种。
///
/// 没有 `Two`（斯洛文尼亚语、马耳他语那一类要用）：本项目还没这些语言，多加一臂只会让
/// 「哪个后缀背后真有文案」这件事变成猜。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plural {
    One,
    Few,
    Many,
    Other,
}

impl Plural {
    /// 文案键的后缀：`Plural::Few` 就是 `reader.book_count_few` 里那一段。
    pub fn key_suffix(self) -> &'static str {
        match self {
            Plural::One => "one",
            Plural::Few => "few",
            Plural::Many => "many",
            Plural::Other => "other",
        }
    }
}

/// 当前生效的界面语言。没同步过配置时（测试）就是 rust_i18n 自己的缺省 locale
/// （`en`）；真机上 [`crate::Config`] 一装好就会 apply 一次，之后跟着配置走。
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
    //! 文案按语言分成 `locales/<语言标签>.toml`，一门语言一份。漏翻在这里不再是「并排一张表里
    //! 空着的那一格」，而是另一个文件里少了一行 —— 每一份各自看都像是完整的。所以这两件事只能
    //! 由这里来保证，别指望读文件的人发现。
    //!
    //! 换成 rust-i18n 之后 key 是字符串，写错只能运行时发现（界面上会原样显示
    //! `page.foo`），所以这里把「各门语言的 key 集合一致」变成一条会失败的测试，
    //! 顶替过去 `Msg` 枚举提供的编译期穷举。
    //!
    //! 除了 `apply_and_current_agree`，其余测试都走 `t_in`（显式语言）：全局 locale 只有一份，
    //! 而 cargo 会把同一文件里的测试跑到不同线程上，改全局的断言会互相踩。

    use std::collections::{BTreeSet, HashSet};
    use std::fmt::Display;

    use crate::_rust_i18n_backend;
    use crate::infra::i18n::{self, Locale, Plural};

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
        assert_eq!(Locale::parse("ko-KR"), Some(Locale::Ko));
        assert_eq!(Locale::parse("ru_RU"), Some(Locale::Ru));
        assert_eq!(Locale::parse("de-AT"), Some(Locale::De));
        assert_eq!(Locale::parse(""), None);
        // 还认不出来的写法必须是真不存在的语言，不能是还没接上的那一门。
        assert_eq!(Locale::parse("th"), None);
        assert_eq!(Locale::parse("pt-BR"), None);
    }

    /// `zh-` 打头的标签里，繁体那几个子签不能被折进简体 —— 折进去等于把看得懂繁体的人
    /// 无声降级成一份他要不要都行的文案，而且一句反馈都没有。
    #[test]
    fn traditional_chinese_is_not_folded_into_simplified() {
        for input in [
            "zh-TW",
            "zh-tw",
            "zh_TW",
            "zh-Hant",
            "zh-Hant-HK",
            "zh-Hant-MO",
            "zh-HK",
        ] {
            assert_eq!(
                Locale::parse(input),
                Some(Locale::ZhHant),
                "{input:?} 应当认成繁体中文"
            );
            assert_eq!(
                Locale::from_label(input),
                Some(Locale::ZhHant),
                "{input:?} 走宽松路径也要认成繁体中文"
            );
        }
        // 简体那一侧的写法照旧，裸 `zh` 也照旧落简体：这条是既有的默认。
        assert_eq!(Locale::parse("zh"), Some(Locale::ZhHans));
        assert_eq!(Locale::parse("zh-CN"), Some(Locale::ZhHans));
        assert_eq!(Locale::parse("zh-Hans"), Some(Locale::ZhHans));
        assert_eq!(Locale::parse("zh-SG"), Some(Locale::ZhHans));
    }

    /// 两个 `Locale` 共用一个标签时，`messages_for_locale` 会给同一份文案，
    /// 上面那条 key 集合一致的测试就悄悄绿了 —— 下拉框里则出现两个看不出差别的选项。
    #[test]
    fn every_language_has_its_own_label_and_name() {
        let codes: HashSet<&str> = Locale::ALL.iter().map(|locale| locale.code()).collect();
        assert_eq!(
            codes.len(),
            Locale::ALL.len(),
            "语言标签不能撞车：{codes:?}"
        );
        let names: HashSet<&str> = Locale::ALL.iter().map(|locale| locale.endonym()).collect();
        assert_eq!(
            names.len(),
            Locale::ALL.len(),
            "语言的本族名不能撞车：{names:?}"
        );
    }

    #[test]
    fn codes_round_trip_through_parse() {
        for locale in Locale::ALL {
            assert_eq!(Locale::parse(locale.code()), Some(*locale));
        }
    }

    /// 配置文件里人能写出来的形式，以及 **derive serde 时代写进老配置里的变体名**。
    /// 老文件必须继续认得：读失败的那一节会退回默认值（简体中文），
    /// 等于把读者选过的语言无声改掉。
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
            ("zh-TW", Locale::ZhHant),
            ("繁體中文", Locale::ZhHant),
            ("ZhHant", Locale::ZhHant),
            ("ja", Locale::Ja),
            ("JA_jp", Locale::Ja),
            ("日本語", Locale::Ja),
            ("Ja", Locale::Ja),
            ("ko", Locale::Ko),
            ("ko_KR", Locale::Ko),
            ("한국어", Locale::Ko),
            ("fr-FR", Locale::Fr),
            ("Français", Locale::Fr),
            ("de", Locale::De),
            ("Deutsch", Locale::De),
            ("es-MX", Locale::Es),
            ("Español", Locale::Es),
            ("ru", Locale::Ru),
            ("RU", Locale::Ru),
            ("Русский", Locale::Ru),
        ] {
            assert_eq!(
                Locale::from_label(input),
                Some(want),
                "{input:?} 应该认成 {want:?}"
            );
        }
        assert_eq!(Locale::from_label("th"), None);
        assert_eq!(Locale::from_label(""), None);
        assert_eq!(Locale::from_label("   "), None);
    }

    /// 存出去的必须是 `code()`：与界面标签、与 `locales/` 那个文件名同一形态。
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
        // 静默吞成一个看似合理的语言，读者手改配置出错时就得不到任何反馈。
        assert!(serde_json::from_str::<Locale>(r#""klingon""#).is_err());
        assert!(serde_json::from_str::<Locale>("3").is_err());
    }

    /// 报错里那份可用清单必须由 `ALL` 现拼：语言加过去了而提示没跟上，等于把可用值说错。
    #[test]
    fn the_error_names_every_language_on_offer() {
        let listing = Locale::available_labels();
        for locale in Locale::ALL {
            assert!(
                listing.split(" / ").any(|part| part == locale.code()),
                "{} 没有出现在可用清单里：{listing}",
                locale.code()
            );
        }
        let err = serde_json::from_str::<Locale>(r#""klingon""#)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(Locale::Ru.code()) && err.contains(Locale::ZhHant.code()),
            "报错要把最新加上的语言也说出去，实际：{err}"
        );
    }

    #[test]
    fn plural_arms_follow_the_language_not_a_one_or_other_guess() {
        // 中日韩没有数的形态变化。
        for locale in [Locale::ZhHans, Locale::ZhHant, Locale::Ja, Locale::Ko] {
            for n in [0usize, 1, 2, 5, 21] {
                assert_eq!(locale.plural(n), Plural::Other, "{locale:?} 的 {n}");
            }
        }
        // 英德西只看 1。
        for locale in [Locale::En, Locale::De, Locale::Es] {
            assert_eq!(locale.plural(1), Plural::One);
            assert_eq!(locale.plural(0), Plural::Other);
            assert_eq!(locale.plural(2), Plural::Other);
        }
        // 法语按 CLDR 把 0 也算单数：0 livre，但 2 livres。
        assert_eq!(Locale::Fr.plural(0), Plural::One);
        assert_eq!(Locale::Fr.plural(1), Plural::One);
        assert_eq!(Locale::Fr.plural(2), Plural::Other);
        // 俄语的三类：个位 1（除 11）→ one，个位 2~4（除 12~14）→ few，其余 → many。
        for n in [1usize, 21, 101] {
            assert_eq!(Locale::Ru.plural(n), Plural::One, "{n} 应当配 книга");
        }
        for n in [2usize, 3, 4, 22, 102, 122] {
            assert_eq!(Locale::Ru.plural(n), Plural::Few, "{n} 应当配 книги");
        }
        // 12~14 这十几个是个位看着像 few、整条却归 many 的例子。
        for n in [0usize, 5, 11, 12, 14, 20, 25, 105, 112] {
            assert_eq!(Locale::Ru.plural(n), Plural::Many, "{n} 应当配 книг");
        }
    }

    /// 库不做复数，所以「四臂选对之后文案还得真的写成那样」这一步仍然要人负责。
    #[test]
    fn the_plural_arms_read_right_in_the_languages_that_have_them() {
        let count = |locale: Locale, n: usize| {
            let key = format!("reader.book_count_{}", locale.plural(n).key_suffix());
            at(locale, &key, &[("count", &n)])
        };
        assert_eq!(count(Locale::En, 1), "1 book");
        assert_eq!(count(Locale::En, 2), "2 books");
        assert_eq!(count(Locale::Fr, 0), "0 livre", "法语里 0 走单数");
        assert_eq!(count(Locale::Fr, 2), "2 livres");
        assert_eq!(count(Locale::De, 1), "1 Buch");
        assert_eq!(count(Locale::Es, 2), "2 libros");
        assert_eq!(count(Locale::Ru, 1), "1 книга");
        assert_eq!(count(Locale::Ru, 3), "3 книги");
        assert_eq!(count(Locale::Ru, 5), "5 книг");
        assert_eq!(count(Locale::Ru, 21), "21 книга");
        assert_eq!(count(Locale::Ru, 11), "11 книг");
        assert_eq!(count(Locale::Ja, 2), "2 冊");
        assert_eq!(count(Locale::Ko, 2), "2권");
        assert_eq!(count(Locale::ZhHant, 2), "2 本書");
    }

    /// 每一条带复数后缀的键，每一份语言表都必须四条臂齐全：缺一条，选到那一臂时
    /// 界面就显示键名本身。
    #[test]
    fn every_plural_key_has_all_four_arms_in_every_language() {
        const ARMS: [Plural; 4] = [Plural::One, Plural::Few, Plural::Many, Plural::Other];
        let base = messages(Locale::ZhHans.code());
        let prefixed: BTreeSet<String> = base
            .iter()
            .filter(|(key, _)| key.ends_with("_one"))
            .map(|(key, _)| key[..key.len() - "_one".len()].to_string())
            .collect();
        assert!(
            !prefixed.is_empty(),
            "扫不到任何复数键，说明这条测试的形状已经和文案表脱钩了"
        );
        for stem in &prefixed {
            for locale in Locale::ALL {
                for arm in ARMS {
                    let key = format!("{stem}_{}", arm.key_suffix());
                    assert!(
                        messages(locale.code()).iter().any(|(k, _)| *k == key),
                        "{} 缺 {key}",
                        locale.code()
                    );
                }
            }
        }
    }

    #[test]
    fn language_names_are_written_in_their_own_language() {
        assert_eq!(Locale::ZhHans.endonym(), "简体中文");
        assert_eq!(Locale::ZhHant.endonym(), "繁體中文");
        assert_eq!(Locale::En.endonym(), "English");
        assert_eq!(Locale::Ja.endonym(), "日本語");
        assert_eq!(Locale::Ko.endonym(), "한국어");
        assert_eq!(Locale::Fr.endonym(), "Français");
        assert_eq!(Locale::De.endonym(), "Deutsch");
        assert_eq!(Locale::Es.endonym(), "Español");
        assert_eq!(Locale::Ru.endonym(), "Русский");
    }
}
