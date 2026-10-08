//! 界面语言：`Locale` 是「用户选了哪种语言」这件事的表示，
//! 文案本身在 `locales/folio.yml`，由 `rust_i18n` 在编译期嵌进来（见 `lib.rs` 的 `i18n!`）。
//!
//! 为什么交给库而不是自己写一张表：`rust-i18n` 就是 gpui-kit 组件层自己在用的那套
//! （`gpui-component/src/lib.rs` 里一句 `i18n!("locales", fallback = "en")`），
//! 用它意味着 `Settings` 搜索框、下拉框这些框架文案能和 Folio 的文案同步切换，
//! 而且加一条文案只是往 YAML 里加一个节点，不用同时改枚举、`ALL` 数组和三列大表。
//!
//! 两个必须知道的取舍：
//! - **key 写错不再是编译期错误**。库里查不到的 key 会原样显示成 `page.foo`，
//!   看得见但太晚，所以 `tests/i18n.rs` 会拿三种语言的 key 集合互相比对。
//! - **全局只有一份 locale**（`rust_i18n::set_locale`）。真正的来源仍是
//!   `Config.language.locale`，改配置的人负责同步一次（见 `ConfigHolder`）。

use std::fmt::Display;

use serde::{Deserialize, Serialize};

/// 界面语言。`Default` 是简体中文：这个项目的正文排版以中文书为第一目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Locale {
    #[default]
    ZhHans,
    En,
    Ja,
}

impl Locale {
    pub const ALL: &'static [Locale] = &[Locale::ZhHans, Locale::En, Locale::Ja];

    /// BCP-47 标签：既是 `locales/folio.yml` 里的语言键，也是设置项的「值」和 `lang` 属性。
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
