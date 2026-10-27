//! 视图侧取文案的那几个函数。
//!
//! 文案表在 `folio-core` 的 `locales/<语言标签>.toml`（一门语言一份），由 `rust-i18n` 在编译期嵌进来；
//! 这里只多做一件事：**把当前语言从配置实体里读出来**再交给库。
//!
//! 为什么不用 `rust_i18n` 的全局 locale 当唯一来源：那个值是进程级的，
//! 而一个进程里会开多个测试应用上下文，各自的语言不同 —— 并发跑测试时
//! 会互相把界面语言改掉。配置实体（`ConfigHolder`）是每个应用上下文自己的，
//! 读它就没有这个问题。全局值仍然会被同步（见 `ConfigHolder::edit`），
//! 这样 gpui-kit 组件自己的文案（下拉框、搜索框）也能跟着换语言。

use std::fmt::Display;

use ely_gpui_component::typography::format::{self, Separators};
use folio_core::duration_parts;
use folio_core::infra::i18n::{self, Locale};
use gpui_kit::{App, SharedString};

use super::config::ConfigHolder;

pub fn locale(cx: &App) -> Locale {
    ConfigHolder::language(cx).locale
}

/// 取一条文案，直接给要进组件链的 `SharedString`。
pub fn tr(cx: &App, key: &str) -> SharedString {
    SharedString::from(i18n::t_in(locale(cx), key, &[]))
}

/// 带参文案：`args` 里的名字对应模板里的 `%{名字}`。
pub fn tf(cx: &App, key: &str, args: &[(&str, &dyn Display)]) -> String {
    i18n::t_in(locale(cx), key, args)
}

/// 「N 本书」这类要分单复数的：选哪一条文案由这门语言的复数规则决定，
/// 而不是让调用处去记「英文有单复数、俄语有三种、中文没有」。
///
/// 后缀就是 `Plural` 的四臂，每条键在每一份 `locales/*.toml` 里都摆着
/// （这一门用不到的臂照抄 `_other`），所以拼出来的键永远解得开。
pub fn book_count(cx: &App, count: usize) -> String {
    let locale = locale(cx);
    let key = format!("reader.book_count_{}", locale.plural(count).key_suffix());
    i18n::t_in(locale, &key, &[("count", &count)])
}

/// 一段时长的读数，按语言的形状挑句式。
///
/// 数由 `folio_core::duration_parts` 拆，这里只决定说哪一句：整点不提分钟、
/// 不足一小时不提小时、不足一分钟才说秒。三条规则各语言自己写在 TOML 里
/// （`stats.duration_*` 那四条），所以这一段一次都没拼「小时」两个字
/// （`plans/基础设施.md` §五.4）。住在这一层而不是统计屏内部，是因为「笔记」那一屏
/// 的卡片也要说同一句话 —— 两处各挑一次句式，改日就会说出两种口径。
pub fn duration(cx: &App, secs: i64) -> SharedString {
    let parts = duration_parts(secs);
    let text = match parts.hours {
        0 if parts.sub_minute => tf(cx, "stats.duration_seconds", &[("seconds", &parts.seconds)]),
        0 => tf(cx, "stats.duration_minutes", &[("minutes", &parts.minutes)]),
        hours if parts.minutes == 0 => tf(cx, "stats.duration_hours", &[("hours", &hours)]),
        hours => tf(
            cx,
            "stats.duration_hours_minutes",
            &[("hours", &hours), ("minutes", &parts.minutes)],
        ),
    };
    text.into()
}

/// 每一门语言的**数字分隔符**：千位分隔与小数点。
///
/// 「句子怎么翻」在 `locales/*.toml`，「数字怎么写」不在那里：表里的 `%{n}` 只管插值，
/// 插进来的是不是 `1 234` 还是 `1,234` 得由界面这一层决定。这份表只有九个字符对，
/// 其中四门与 Ely 组件库那张逐字对得上（`de-DE`/`fr-FR`/`ja-JP`/`zh-CN`），
/// 另外五门按 CLDR 的常用值给。「九门一条不缺」由 `match` 的**穷尽性**钉住：加一门语言时
/// 这里编不过，而不是静默沿用某一门的写法；[`tests::separators_are_never_the_same_character`]
/// 钉的是另一件会静默的事 —— 某一门的分组符与小数点撞成同一个字符（德语手滑写成两个点），
/// 屏幕上就是「1.000.5」这种没人写得出来的数。
fn separators(locale: Locale) -> Separators {
    match locale {
        // 中英日韩：千位逗号、小数点。
        Locale::ZhHans | Locale::ZhHant | Locale::En | Locale::Ja | Locale::Ko => Separators {
            group: ',',
            decimal: '.',
        },
        // 法语：CLDR 现行的是窄不换行空格（U+202F），不是普通空格。
        Locale::Fr => Separators {
            group: '\u{202f}',
            decimal: ',',
        },
        // 德语与西语：点分组、逗号小数。
        Locale::De | Locale::Es => Separators {
            group: '.',
            decimal: ',',
        },
        // 俄语：不换行空格分组、逗号小数。
        Locale::Ru => Separators {
            group: '\u{a0}',
            decimal: ',',
        },
    }
}

/// 按当前语言写一个数：`decimals` 位小数，千位按这一门分组。
///
/// 走的是 Ely 组件库里那两个**纯函数**（`typography::format`）：不碰它的主题、不需要它的
/// `init`，所以这一层引过来的只有分组算法本身。
pub fn number(cx: &App, value: f64, decimals: usize) -> String {
    format::number(value, decimals, separators(locale(cx)))
}

/// 按当前语言写一个整数（计数那一类：俄语的「2 831 个文件」中间是硬空格，不是逗号）。
pub fn integer(cx: &App, value: u64) -> String {
    format::group_digits(&value.to_string(), separators(locale(cx)).group)
}

#[cfg(test)]
mod tests {
    //! 界面里写下的每一个文案键，都必须真的存在于每一份语言表里。
    //!
    //! 这条测试存在的理由是一种**编译与测试全绿**的坏状态：`rust_i18n` 认不出一个键时不报错。
    //! 这里有两种坏法，症状不一样：
    //! - 所有语言都没有这个键（写错、改了文案表却没重编）→ 界面上原样画出键名；
    //! - 只有某一门没有 → 按 `fallback = "en"` 回落成英文那一句，屏幕上半英半韩。
    //!
    //! 语言从三份加到九份之后，第二种成了常态，而它**用返回值是查不出来的**（回落也算解开了），
    //! 所以判据是「这门语言自己那份表里有没有这一行」，见 [`keys_in_language`]。
    //!
    //! 做法是把视图源码里以 `tr`/`tf` 加一个字面量键的那些调用全扫出来逐个问一遍。
    //! 这一行不能再举出带引号的实际形状：本文件此刻也在被扫的 `src/` 里，注释里出现
    //! needle 就会把它自己抠成一个「查不到的键」，这条测试会红在自己身上。
    //! 扫不到的是**通过变量传进来的键**（`Tab::label_key()` 那种），它们各自的字面量在
    //! `match` 分支里，只要同一批键里有一处是字面量调用就被覆盖到了。

    use std::collections::BTreeSet;
    use std::fmt::Write as _;
    use std::path::{Path, PathBuf};

    use super::{duration, separators, tf};
    use folio_core::infra::i18n::Locale;
    use folio_core::{Config, Language};
    use gpui_kit::App;

    use crate::ui::services::config::ConfigHolder;

    /// 收集 `src/` 下所有 `.rs` 里以字面量出现的文案键。
    fn keys_in_source() -> BTreeSet<String> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs(&root, &mut files);
        assert!(files.len() > 10, "扫不到几个源文件说明路径不对：{root:?}");

        let mut keys = BTreeSet::new();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            for needle in ["tr(cx, \"", "tr(_cx, \"", "tf(cx, \""] {
                let mut rest = text.as_str();
                while let Some(at) = rest.find(needle) {
                    let tail = &rest[at + needle.len()..];
                    let Some(end) = tail.find('"') else { break };
                    keys.insert(tail[..end].to_string());
                    rest = &tail[end..];
                }
            }
        }
        keys
    }

    fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    /// 每一门的分组符与小数点不许是同一个字符。
    ///
    /// `match` 的穷尽性保证「九门都有值」，保证不了「值没写撞」：把德语那一对写成
    /// `('.', '.')` 编译与测试都全绿，而屏幕上会出现「1.000.5 MB」这种谁也没写过的数。
    #[test]
    fn separators_are_never_the_same_character() {
        for locale in Locale::ALL {
            let marks = separators(*locale);
            assert_ne!(
                marks.group,
                marks.decimal,
                "{} 的千位分隔与小数点撞成同一个字符 {:?}",
                locale.code(),
                marks.group
            );
            // 小数点必须是能看的标点（那两门空格分组容易顺手把小数点也敲成空格）。
            assert!(
                marks.decimal.is_ascii_punctuation(),
                "{} 的小数点不是一位标点：{:?}",
                locale.code(),
                marks.decimal
            );
        }
    }

    /// 某一门语言**自己那份表**里的全部键。
    ///
    /// 判存在不能用 `t_in` 的返回值：`i18n!` 带 `fallback = "en"`，一门语言里缺一条时它
    /// 回吐的是英文那一句 —— 看着像解开了，界面上其实是半英半韩。这里拿的正是核心侧
    /// `every_language_carries_the_same_keys` 用的同一份表。
    fn keys_in_language(tag: &str) -> BTreeSet<String> {
        folio_core::_rust_i18n_backend()
            .messages_for_locale(tag)
            .unwrap_or_else(|| panic!("locales/ 里没有 {tag}.toml 这一份"))
            .into_iter()
            .map(|(key, _)| key.to_string())
            .collect()
    }

    /// 界面用到的每个键，每一份语言表里都得真有这一行。
    #[test]
    fn every_literal_key_resolves_in_every_language() {
        let keys = keys_in_source();
        assert!(!keys.is_empty(), "一个键都没扫到，测试本身坏了");
        let tables: Vec<(&str, BTreeSet<String>)> = Locale::ALL
            .iter()
            .map(|l| (l.code(), keys_in_language(l.code())))
            .collect();
        let mut broken = String::new();
        for key in &keys {
            for (tag, table) in &tables {
                if !table.contains(key) {
                    let _ = writeln!(broken, "{tag}  {key}");
                }
            }
        }
        assert!(
            broken.is_empty(),
            "这些键在某门语言的表里根本没有（回落成英文，界面会半英半韩）：\n{broken}"
        );
    }

    /// 把界面语言装进这个上下文（[`duration`] 要说哪门语言的句子由配置实体决定，
    /// 不是由进程全局决定 —— 并发跑测试时那一份会被别人改掉）。
    fn set_locale(cx: &mut gpui_kit::TestAppContext, locale: Locale) {
        cx.update(|cx: &mut App| {
            ConfigHolder::install(
                Config {
                    language: Language { locale },
                    ..Default::default()
                },
                cx,
            );
        });
    }

    /// 时长那句按语言换形状（函数从统计屏的 `heatmap` 搬了过来，规矩没变）。
    #[gpui_kit::test]
    fn the_duration_sentence_changes_shape_with_the_language(cx: &mut gpui_kit::TestAppContext) {
        set_locale(cx, Locale::ZhHans);
        assert_eq!(cx.read(|cx| duration(cx, 5_040)).as_str(), "1 小时 24 分钟");
        assert_eq!(cx.read(|cx| duration(cx, 45)).as_str(), "45 秒");
        assert_eq!(cx.read(|cx| duration(cx, 7_200)).as_str(), "2 小时");

        set_locale(cx, Locale::En);
        assert_eq!(cx.read(|cx| duration(cx, 5_040)).as_str(), "1h 24m");
        // 月标签那一行：中文带「月」，英文就是那个数字（一年十二个数排成一排，
        // 顺序已经把「这是月份」说清了）。
        assert_eq!(cx.read(|cx| tf(cx, "stats.month", &[("month", &3)])), "3");
    }
}
