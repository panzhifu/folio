//! 视图侧取文案的三个函数。
//!
//! 文案表在 `folio-core` 的 `locales/folio.yml`，由 `rust-i18n` 在编译期嵌进来；
//! 这里只多做一件事：**把当前语言从配置实体里读出来**再交给库。
//!
//! 为什么不用 `rust_i18n` 的全局 locale 当唯一来源：那个值是进程级的，
//! 而一个进程里会开多个测试应用上下文，各自的语言不同 —— 并发跑测试时
//! 会互相把界面语言改掉。配置实体（`ConfigHolder`）是每个应用上下文自己的，
//! 读它就没有这个问题。全局值仍然会被同步（见 `ConfigHolder::edit`），
//! 这样 gpui-kit 组件自己的文案（下拉框、搜索框）也能跟着换语言。

use std::fmt::Display;

use folio_core::i18n::{self, Locale};
use gpui_kit::{App, SharedString};

use crate::config_model::ConfigHolder;

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
