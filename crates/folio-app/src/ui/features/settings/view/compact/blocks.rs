//! 手机根屏那条入口列表的分块表：块名 + 落进这一块的页。
//!
//! 从 [`super`] 里单独切出来的理由只有一条：那个文件在 FOLIO-GOV-004 的债务基线上，只许变短
//! 不许变长（见 `config/folio-debt-baseline.json`），而块表是那一屏里唯一能整块搬走、又不牵着
//! 渲染状态的一段。页表本身仍由 `super::spec_pages` 组装，块只是给那些页分类。
//!
//! 为什么要块：[`SettingsPage::ALL`] 已经 16 格（调试页在场时 17 格），从前除会员外全拍成一条
//! 平铺列表 —— 十四行同一个字号、同一种行、只靠发丝线分开，读者要找「行距」得把整列读完才
//! 知道它在第几格。分块把这条改成「先落到哪一类，再在那一类里找」，与规范那条「一个个看
//! 起来合理的控件凑在一起仍可能失败，因为它们没有形成一个阅读顺序」对得上。
//!
//! **三格不进块**，都落在末尾那张无节头的卡里：**会员**在根顶是单独一张卡（要当场念等级与
//! 到期日，见 `super::CompactSettings` 的根屏）；**关于**与**调试**按 [`SettingsPage::ALL`]
//! 的顺序排在那张尾卡上 —— 一块只剩一行时，节头念的就是那一行自己的名字，那句话是重复
//! （规范「上下文已经建立的信息不要再讲一遍」）。调试页因此压在整张列表的最下面：它属于
//! 「排查完顺手看一眼」的东西，不该挡在读者每天要用的那几页前面。
//!
//! **没登记的页不会从屏上消失**：它落进末尾那张无节头的卡，同时
//! `every_page_lands_in_a_root_block` 那条用例红 —— 加一页忘了分块，症状是「它少一句块名」，
//! 不是「它不见了」。

use crate::ui::features::settings::view::SettingsPage;

/// 切片而不是定长数组：块表本身不再按构建变化，但它将来可能因为「某一页只在特定构建里
/// 存在」再挂一次 `#[cfg]`，切片不必为那一刻改类型。
pub(super) const ROOT_BLOCKS: &[(&str, &[SettingsPage])] = &[
    (
        "section.reading",
        &[
            SettingsPage::Appearance,
            SettingsPage::Typography,
            SettingsPage::Layout,
            SettingsPage::Fonts,
            SettingsPage::Speech,
            SettingsPage::Dictionary,
        ],
    ),
    (
        "section.translation_ai",
        &[SettingsPage::Translation, SettingsPage::Ai],
    ),
    (
        "section.books_data",
        &[
            SettingsPage::Library,
            SettingsPage::TxtRules,
            SettingsPage::Toc,
            SettingsPage::Sync,
            SettingsPage::Storage,
            SettingsPage::Backup,
        ],
    ),
];

/// 这一页落进根屏的哪一块；`None` = 末尾那张无节头的卡。
///
/// 按**页**查而不是按下标：块里写的是目的地，`ALL` 挪顺序时块跟着走，不会指错格子。
pub(super) fn root_block(page: SettingsPage) -> Option<usize> {
    ROOT_BLOCKS
        .iter()
        .position(|(_, members)| members.contains(&page))
}

#[cfg(test)]
mod tests {
    use super::{ROOT_BLOCKS, root_block};
    use crate::ui::features::settings::view::SettingsPage;

    /// 分块不许漏页：会员、关于与调试三格不进块（会员在根顶那张卡，另两格在尾卡），
    /// 其余每一页都要落进某一块；同一页也不许落进两块。
    ///
    /// 这条用例管的是「加一页忘了登记」。那种页不会从屏上消失（它掉进尾卡），但它没有
    /// 块名，而根屏那条列表又会重新变回「一串看不出类的行」—— 正是分块要治的那个病。
    #[test]
    fn every_page_lands_in_a_root_block() {
        let unblocked: Vec<SettingsPage> = SettingsPage::ALL
            .iter()
            .filter(|page| root_block(**page).is_none())
            .copied()
            .collect();
        let mut expected = vec![SettingsPage::Membership, SettingsPage::About];
        if cfg!(any(debug_assertions, feature = "debug-ui")) {
            expected.push(SettingsPage::Debug);
        }
        assert_eq!(
            unblocked, expected,
            "除会员、关于与调试之外，每一页都要登记进某一块"
        );

        let registered = SettingsPage::ALL
            .iter()
            .filter(|page| root_block(**page).is_some())
            .count();
        let slots: usize = ROOT_BLOCKS.iter().map(|(_, members)| members.len()).sum();
        assert_eq!(registered, slots, "ROOT_BLOCKS 里有一页被登记了两次");
    }

    /// 调试页压在整张入口列表的最下面（尾卡里跟在「关于」后面）。这一条钉的是**顺序**：
    /// 它属于「排查完顺手看一眼」的东西，挡在每天要调的页前面就等于把那些页往下挤一格。
    #[test]
    fn the_debug_page_sits_at_the_very_end() {
        if !cfg!(any(debug_assertions, feature = "debug-ui")) {
            return;
        }
        assert_eq!(SettingsPage::ALL.last(), Some(&SettingsPage::Debug));
    }
}
