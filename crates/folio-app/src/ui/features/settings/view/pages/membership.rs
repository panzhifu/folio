//! 高级会员：本机只记一个**到期日**，等级是渲染时按今天现算出来的。
//!
//! 这一屏**不联网**。「试用」写进配置的就是 `membership.until_day`，权益那一组只是把
//! 「会员将来覆盖哪些功能」摆在同一处，**现在一个都不挡** —— 所以这一屏上没有一颗钮承诺了
//! 程序做不到的事：它说「试用 30 天」，给的确实是本机 30 天。以后接真支付，换掉的还是同一格
//! 的写入值（一笔带时长的成交记录），界面与规格都不用动。
//!
//! 「今天」由 [`today`] 现取，偏移来自 [`crate::ui::host::local_offset_secs`]：日界线跟着
//! 读者走，写死 UTC 会让跨过日界线那一天的读数差一格。统计那几屏按同一条取偏移。

use super::super::*;
use crate::ui::host::local_offset_secs;
use folio_core::{Membership, MembershipTier, infra::time};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, h_flex};

/// 今天，本地日序号。等级与到期日都从这一个数算，页面上没有第二处自己读时钟的地方。
pub(in crate::ui::features::settings) fn today() -> i64 {
    time::local_day(time::now(), local_offset_secs())
}

/// 根屏那张卡片要的一句话：从没开过 / 还有效（哪天到期）/ 已经过了（哪天过的）。
///
/// 页内那行「到期日」摆的是**光一个日期**：行左边已经写了「到期日」三个字，句子里再说一遍
/// 「到期」是重复。卡片没有那一列标签，只能由这一句自己说清是哪一种状态。两份都从同一个
/// `until_day` 与同一个 [`today`] 折出来，不会各说一套。
///
/// 三个分支各写一遍 `tr`/`tf` 的字面量键，而不是把键挑进一个变量再统一插值：
/// 门禁那条测试是**扫源码里的字面量**来要求九份表都有这一行的（见 `services/l10n.rs` 的
/// `every_literal_key_resolves_in_every_language`）。键一旦经过变量，那条就看不见它。
pub(in crate::ui::features::settings) fn status_line(cx: &App) -> SharedString {
    let membership = ConfigHolder::membership(cx);
    if membership.until_day <= 0 {
        return tr(cx, "membership.never");
    }
    let date = time::civil_date(membership.until_day);
    if membership.tier_on(today()) == MembershipTier::Premium {
        tf(cx, "membership.expires", &[("date", &date)]).into()
    } else {
        tf(cx, "membership.expired", &[("date", &date)]).into()
    }
}

pub(in crate::ui::features::settings) fn membership_page(cx: &App) -> Page {
    page(
        tr(cx, "page.membership"),
        IconName::Crown,
        vec![state_group(cx), benefit_group(cx)],
    )
}

/// 「当前状态」那一组：等级与到期日两行读数，末尾一颗按状态换身份的钮。
///
/// 过期了**照样摆出那个日子**：只写「普通」等于把「为什么不是会员」抹掉，而摆出旧日期
/// 读者一眼就看懂 —— 那一天已经过了。从没开过才是「未开通」那一格。
///
/// 规格每次渲染重建（见 `model/spec.rs` 的模块说明），所以下面按 `day` 折出来的等级与
/// 试用退场日期都是**这一帧**的：按完「试用」那一格当场换掉，不用另发一次通知。
fn state_group(cx: &App) -> Group {
    let day = today();
    let membership = ConfigHolder::membership(cx);
    let premium = membership.tier_on(day) == MembershipTier::Premium;
    let expiry = if membership.until_day > 0 {
        time::civil_date(membership.until_day).into()
    } else {
        tr(cx, "membership.never")
    };
    let tier_text = if premium {
        tr(cx, "membership.tier_premium")
    } else {
        tr(cx, "membership.tier_standard")
    };

    group(
        tr(cx, "group.membership_state"),
        vec![
            Row::info(tr(cx, "field.membership_tier"), tier_text).keywords(&[
                "membership",
                "premium",
                "会员",
                "等级",
            ]),
            Row::info(tr(cx, "field.membership_expiry"), expiry).keywords(&[
                "membership",
                "expiry",
                "会员",
                "到期",
            ]),
            Row::custom(move |cx: &App| {
                // 两种状态各摆各的钮：会员中才谈得上「取消」，不在有效期内才谈得上「试用」。
                // 摆两颗其中一颗永远点不动，不如只摆那颗此刻有意义的。
                //
                // 判的是上面那两行同一个 `premium`：那一格按钮与那两行读数必须说同一件事，
                // 而这里再读一遍配置就成了「同一帧里两个答案的来源」。
                if premium {
                    return action_slot(
                        Button::new("settings-membership-cancel")
                            .ghost()
                            .small()
                            .label(tr(cx, "action.membership_cancel").to_string()),
                    )
                    .into_any_element();
                }
                let days = Membership::TRIAL_DAYS.to_string();
                let label = tf(cx, "action.membership_trial", &[("days", &days)]).to_string();
                action_slot(
                    Button::new("settings-membership-trial")
                        .small()
                        .label(label)
                        .on_click(move |_, _, cx| {
                            ConfigHolder::edit_membership(cx, |membership| {
                                membership.until_day = Membership::trial_until_day(day)
                            })
                        }),
                )
                .into_any_element()
            })
            .keywords(&["membership", "trial", "会员", "试用", "开通"]),
        ],
    )
}

/// 那一颗动作钮的落点：整行只把它推到读序的那一端，行本身不可点（命中面归钮自己）。
fn action_slot(button: Button) -> impl IntoElement {
    h_flex().w_full().justify_end().child(button)
}

/// 「会员权益」那一组：五行勾选清单。
///
/// 这一组**不改变任何功能能不能用**，所以不摆「已可用 / 未解锁」那一列：五行一样的状态字
/// 既没有信息量，也不说实话（眼下这些功能对谁都开着）。每行就一枚勾 + 那句话。
fn benefit_group(cx: &App) -> Group {
    // 五句都写成字面量键的 `tr`，理由同 [`status_line`]：源码里摆着键，门禁才查得到九份表。
    let lines = [
        tr(cx, "membership.benefit_sync"),
        tr(cx, "membership.benefit_translate"),
        tr(cx, "membership.benefit_speech"),
        tr(cx, "membership.benefit_pdf"),
        tr(cx, "membership.benefit_opds"),
    ];
    group(
        tr(cx, "group.membership_benefits"),
        lines
            .into_iter()
            .map(|text| {
                Row::custom(move |cx: &App| benefit_row(text.clone(), cx).into_any_element())
            })
            .collect(),
    )
}

/// 权益清单的一行：一枚勾 + 那句话。左右内缩归卡片，这一行只排自己那两格。
///
/// 勾取 `muted_foreground` 而不是 `primary`：这一整组只是清单，强调的额度留给「当前状态」
/// 那一组的动作钮（规范：强调是有限预算，一屏一个焦点）。
fn benefit_row(text: SharedString, cx: &App) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .child(
            Icon::new(IconName::Check)
                .small()
                .text_color(cx.theme().muted_foreground),
        )
        .child(div().min_w_0().text_base().child(text))
}

#[cfg(test)]
mod tests {
    //! 这一屏钉的是**「存的那一天 ↔ 屏上那两行读数」**这条链：等级不是存进去的，所以日期
    //! 写错一格、或折等级时把「含当天」写成「不含」，都在这里露出来而不是在屏上。
    //!
    //! 「试用」那颗钮的**点击**没在这里测：`Row::custom` 装的是渲染闭包，而宿主测试到不了
    //! Compact 档的渲染（见本仓库那两条测试记录）。那颗钮的动作就是
    //! `edit_membership(until_day = trial_until_day(today))` 一句话 —— 那格算术由核心
    //! `the_trial_is_exactly_thirty_days_counting_today_as_the_first` 钉住，写回与读回由
    //! 下面两条钉住。**屏上真长成什么样仍未有人看过**，那一步要真机或桌面窗口。

    use super::{membership_page, status_line, today};
    use crate::ui::features::settings::model::spec::RowKind;
    use crate::ui::services::config::ConfigHolder;
    use crate::ui::services::l10n::tr;
    use folio_core::infra::time;
    use folio_core::{Config, Language, Locale, Membership};
    use gpui_kit::{App, SharedString, TestAppContext};

    /// 装一份**显式**写着简中的配置：下面那两条要按简中的措辞比对句子，靠默认值恰好
    /// 是简中不算数（默认哪天换了，这三条会一起红成别的原因）。
    fn install(cx: &mut App) {
        ConfigHolder::install(
            Config {
                language: Language {
                    locale: Locale::ZhHans,
                },
                ..Default::default()
            },
            cx,
        );
    }

    /// 「当前状态」那一组第 `row` 行的读数。不是读数行就直接红，别让它悄悄比 `None`。
    fn readout(cx: &App, row: usize) -> SharedString {
        match &membership_page(cx).groups[0].rows[row].kind {
            RowKind::Info { value } => value.clone(),
            _ => panic!("第 {row} 行不是读数行"),
        }
    }

    #[gpui_kit::test]
    fn the_two_readouts_follow_the_stored_expiry_day(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            install(cx);
            let day = today();
            // 从没开过：等级普通，日期那一格是「未开通」而不是 1970-01-01。
            assert_eq!(readout(cx, 0), tr(cx, "membership.tier_standard"));
            assert_eq!(readout(cx, 1), tr(cx, "membership.never"));

            // 到期日正好是今天：含当天，所以这一帧还算会员。
            ConfigHolder::edit_membership(cx, |membership| membership.until_day = day);
            assert_eq!(readout(cx, 0), tr(cx, "membership.tier_premium"));
            assert_eq!(
                readout(cx, 1),
                SharedString::from(time::civil_date(day)),
                "摆的必须是存的那个日子本身，不是「今天」这种过一天就换的说法"
            );

            // 过了一天：等级退回普通，而那一天仍摆在行上 —— 只写「普通」不说为什么。
            ConfigHolder::edit_membership(cx, |membership| membership.until_day = day - 1);
            assert_eq!(readout(cx, 0), tr(cx, "membership.tier_standard"));
            assert_eq!(
                readout(cx, 1),
                SharedString::from(time::civil_date(day - 1))
            );
        });
    }

    #[gpui_kit::test]
    fn the_card_line_names_which_of_the_three_states_it_is(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            install(cx);
            let day = today();
            assert_eq!(status_line(cx), tr(cx, "membership.never"));

            ConfigHolder::edit_membership(cx, |membership| {
                membership.until_day = Membership::trial_until_day(day)
            });
            let until = time::civil_date(Membership::trial_until_day(day));
            assert_eq!(
                status_line(cx),
                SharedString::from(format!("{until} 到期")),
                "简中那一份是「<日期> 到期」；这一句比的是形状，句子换了要一起改九份"
            );

            ConfigHolder::edit_membership(cx, |membership| membership.until_day = day - 1);
            let past = time::civil_date(day - 1);
            assert_eq!(
                status_line(cx),
                SharedString::from(format!("{past} 已到期"))
            );
        });
    }
}
