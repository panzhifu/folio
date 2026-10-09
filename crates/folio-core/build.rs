//! `folio-core` 的构建脚本：只为一件事 —— 让文案表改了真的能重编。
//!
//! `rust_i18n::i18n!` 是在**宏展开时**去读 `locales/*.toml` 的，cargo 完全不知道有这回事：
//! 没有这一行，往文案表里加一条键只会重编下游的 `folio-app`（它的 `build.rs` 挂了同样的
//! 一条），而宏在 `folio-core` 里展开 —— 这一层不脏就不重跑，新键于是以
//! 「界面上原样显示 `account.theme`」的形式暴露出来，且**编译与测试全绿**。
//!
//! 这条依赖只能写在这里：宏所在的 crate 才是需要被弄脏的那一个。

fn main() {
    println!("cargo:rerun-if-changed=locales");
}
