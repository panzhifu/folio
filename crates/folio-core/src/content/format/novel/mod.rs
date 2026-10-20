//! 纯文本（`.txt`）小说的分章与书目提取，分成两份文件：
//!
//! - [`rules`]：规则表的形状、出厂默认、读取与校验 —— 数据与它的形状，不含扫描。
//! - [`detect`]：引擎 —— 吃一份编译好的规则表，扫一遍文本，交出章表与目录。
//!
//! 切在这一条线上而不是「分章 / 书目」各一份，是因为分章与书目**共用同一趟扫描**：
//! 头部那一块既是目录页（分章要跳它），又是书名与作者最高的来源。按功能切成两份，
//! 两边就各扫一遍全文、各判一次「这是不是头部」，而两处迟早分叉，症状是目录跳过去
//! 屏上没有那一章。
//!
//! 落点在 `format::text` 那一族的 `PlainFamily`：`split` / `extract` 两格换成这里的
//! 函数就是接通，共用件（`charset::read_plain` → `normalize` → 折成章表 → 包装）
//! 一行都不用动。**目前还没接**：接它要同时 bump `INDEX_VERSION` 并把
//! `CompiledRules::content_hash` 弄进失效判据，那是下一笔的事。

pub(crate) mod detect;
pub mod rules;

/// 中间那一段的字数：两枚标记之间夹了多少正文（目录页判据与间隔门都问它）。
///
/// 端点一律夹进范围，因为行号来自上一趟扫描而 `lines` 可能更短（尾部空行被规范化收成
/// 一个 `\n`），写开一档就是 panic 在导入的工作线程上。
pub(crate) fn body_chars(lines: &[&str], from: usize, to: usize) -> usize {
    let start = from.min(lines.len());
    let end = to.min(lines.len());
    if end <= start {
        return 0;
    }
    lines[start..end]
        .iter()
        .map(|line| line.trim().chars().count())
        .sum()
}
