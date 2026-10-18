//! 视图侧取文案的三个函数。
//!
//! 文案表在 `folio-core` 的 `locales/{en,zh-CN,ja}.toml`（一门语言一份），由 `rust-i18n` 在编译期嵌进来；
//! 这里只多做一件事：**把当前语言从配置实体里读出来**再交给库。
//!
//! 为什么不用 `rust_i18n` 的全局 locale 当唯一来源：那个值是进程级的，
//! 而一个进程里会开多个测试应用上下文，各自的语言不同 —— 并发跑测试时
//! 会互相把界面语言改掉。配置实体（`ConfigHolder`）是每个应用上下文自己的，
//! 读它就没有这个问题。全局值仍然会被同步（见 `ConfigHolder::edit`），
//! 这样 gpui-kit 组件自己的文案（下拉框、搜索框）也能跟着换语言。

use std::fmt::Display;

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

/// 「N 本书」这类要分单复数的：选哪条文案由语言的规则决定，
/// 而不是让调用处去记「英文有复数、中文没有」。
pub fn book_count(cx: &App, count: usize) -> String {
    let locale = locale(cx);
    let key = if locale.plural_one(count) {
        "reader.book_count_one"
    } else {
        "reader.book_count_other"
    };
    i18n::t_in(locale, key, &[("count", &count)])
}

#[cfg(test)]
mod tests {
    //! 界面里写下的每一个文案键，都必须真的能在三种语言里解出来。
    //!
    //! 这条测试存在的理由是一种**编译与测试全绿**的坏状态：`rust_i18n` 认不出的键不会报错，
    //! 而是把键本身原样画到界面上（屏幕上出现 `account.theme`）。而键是字符串，写错、漏加、
    //! 或者改了文案表却没重编，都只有跑到那一屏才看得见。
    //!
    //! 做法是把视图源码里以 `tr`/`tf` 加一个字面量键的那些调用全扫出来逐个问一遍。
    //! 这一行不能再举出带引号的实际形状：本文件此刻也在被扫的 `src/` 里，注释里出现
    //! needle 就会把它自己抠成一个「查不到的键」，这条测试会红在自己身上。
    //! 扫不到的是**通过变量传进来的键**（`Tab::label_key()` 那种），它们各自的字面量在
    //! `match` 分支里，只要同一批键里有一处是字面量调用就被覆盖到了。

    use std::collections::BTreeSet;
    use std::fmt::Write as _;
    use std::path::{Path, PathBuf};

    use folio_core::infra::i18n::{Locale, t_in};

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

    /// 每个键在每种语言里都解得出来 —— 解不出来时 `t_in` 回吐键本身。
    #[test]
    fn every_literal_key_resolves_in_every_language() {
        let keys = keys_in_source();
        assert!(!keys.is_empty(), "一个键都没扫到，测试本身坏了");
        let mut broken = String::new();
        for key in &keys {
            for locale in Locale::ALL {
                let got = t_in(*locale, key, &[]);
                if &got == key {
                    let _ = writeln!(broken, "{}  {key}", locale.code());
                }
            }
        }
        assert!(
            broken.is_empty(),
            "这些键在某种语言里解不出来（界面会原样显示键名）：\n{broken}"
        );
    }
}
