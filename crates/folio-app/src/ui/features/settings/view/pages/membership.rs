//! 高级会员：本机只记一个**到期日**，等级是渲染时按今天现算出来的。
//!
//! 这一屏**不自己收钱**：开通那一颗钮把读者交给爱发电；成交之后回到这一屏，在「激活会员」
//! 那一组填爱发电的订单编号兑换 —— 付没付过钱由 `folio-redeem` 那支函数向平台查（爱发电的
//! api token 不能进客户端），本地这一层只管形状与结果句子。兑成了写进配置的仍是
//! `membership.until_day` 这一格。权益那一组只是把「会员将来覆盖哪些功能」摆在同一处，
//! **现在一个都不挡** —— 所以整组标「规划中」，并写明当前版本都能直接用，不让读者把清单
//! 当成已经生效的承诺。
//!
//! 「今天」由 [`today`] 现取，偏移来自 [`crate::ui::host::local_offset_secs`]：日界线跟着
//! 读者走，写死 UTC 会让跨过日界线那一天的读数差一格。统计那几屏按同一条取偏移。

use super::super::*;
use crate::ui::components::muted_label;
use crate::ui::host::local_offset_secs;
use crate::ui::platform;
use crate::ui::services::activation::{self, Status};
use folio_core::{Membership, MembershipTier, infra::time, net::redeem};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, div};

/// 创作者的爱发电主页。开通钮把读者交给这一页；成交之后的激活码流程另算。
const AFDIAN_PAGE: &str = "https://ifdian.net/a/nokenice";

/// 今天，本地日序号。等级与到期日都从这一个数算，页面上没有第二处自己读时钟的地方。
pub(in crate::ui::features::settings) fn today() -> i64 {
    time::local_day(time::now(), local_offset_secs())
}

/// 根屏那张卡片要的一句话：从没开过 / 还有效（哪天到期）/ 已经过了（哪天过的）。
///
/// hero 也念这一句；两个入口说的是同一件事。三个分支各写一遍 `tr`/`tf` 的字面量键，
/// 而不是把键挑进一个变量再统一插值：门禁那条测试是**扫源码里的字面量**来要求九份表
/// 都有这一行的（见 `services/l10n.rs` 的 `every_literal_key_resolves_in_every_language`）。
pub(in crate::ui::features::settings) fn status_line(cx: &App) -> SharedString {
    status_line_for(ConfigHolder::membership(cx), today(), cx)
}

/// 同一句话，但配置与「今天」由调用方读好再递进来。
///
/// hero 要同时用到 `premium` 与这句话；两处各读一遍配置就会留下「同一帧里两个答案」的口子。
fn status_line_for(membership: Membership, day: i64, cx: &App) -> SharedString {
    if membership.until_day <= 0 {
        return tr(cx, "membership.never");
    }
    let date = time::civil_date(membership.until_day);
    if membership.tier_on(day) == MembershipTier::Premium {
        tf(cx, "membership.expires", &[("date", &date)]).into()
    } else {
        tf(cx, "membership.expired", &[("date", &date)]).into()
    }
}

/// 那一颗动作钮此刻的身份：不在有效期内是「开通」，有效期内是「续费」。
///
/// 判据用 hero 已经算好的 `premium`，不再读第二遍配置 —— 钮与状态句必须说同一件事。
fn action_label(premium: bool, cx: &App) -> SharedString {
    if premium {
        tr(cx, "action.membership_renew")
    } else {
        tr(cx, "action.membership_open")
    }
}

pub(in crate::ui::features::settings) fn membership_page(cx: &App) -> Page {
    page(
        tr(cx, "page.membership"),
        IconName::Crown,
        vec![status_group(cx), redeem_group(cx), benefit_group(cx)],
    )
}

/// 「激活会员」那一组：一格订单编号 + 一颗兑换钮，结果就落在同一格里。
///
/// 分成两行而不是一格：编号要在按下之前**看得见、改得动**（粘错了要能当场发现），
/// 而结果属于按下之后的那一趟 —— 挤在同一格会让读者分不清现在改的是哪一样。
fn redeem_group(cx: &App) -> Group {
    group(
        tr(cx, "membership.redeem_group"),
        vec![
            Row::text(
                tr(cx, "membership.order"),
                activation::draft,
                activation::set_draft,
            )
            .description(tr(cx, "membership.order_desc"))
            .keywords(&[
                "membership",
                "redeem",
                "order",
                "afdian",
                "会员",
                "激活",
                "兑换",
                "订单",
                "爱发电",
            ]),
            Row::custom(redeem_row),
        ],
    )
}

/// 结果句 + 那颗钮。
///
/// 钮取 `ghost`：这一屏的强调预算已经花在 hero 那一颗上（一屏一个焦点），兑换是同一屏里的
/// 第二个动作。在飞的期间 `disabled` —— 双击会把同一单交两次，而第二次只会以「已经兑过了」
/// 收场，那是白给读者的一句看不懂的话。
fn redeem_row(cx: &App) -> gpui_kit::AnyElement {
    let handle = activation::handle(cx);
    let status = handle
        .as_ref()
        .map(|entity| entity.read(cx).status().clone())
        .unwrap_or_default();
    let busy = status == Status::Running;
    h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(muted_label(redeem_line(&status, cx), cx)),
        )
        .child(
            Button::new("settings-membership-redeem")
                .label(tr(cx, "membership.redeem").to_string())
                .ghost()
                .small()
                .disabled(busy)
                .on_click(|_, _, cx| activation::redeem_now(cx)),
        )
        .into_any_element()
}

/// 那一格此刻该念哪一句：还没兑过是提示，在飞是进度，兑成了是结果，其余每一档是
/// 「怎么了 + 下一步怎么办」。
///
/// 句子从**档位**来而不是从 `error.to_string()` 来：core 那些 Display 是中文写死的，
/// 直接念到屏上就成了半英半韩（九门语言的表由 `services/l10n.rs` 那条门禁钉着）。
/// 这里也不报天数：上面那张 hero 的到期日就是结果本身，重复一遍只是噪音。
fn redeem_line(status: &Status, cx: &App) -> SharedString {
    let key = match status {
        Status::Idle => "membership.redeem_idle",
        Status::Running => "membership.redeem_running",
        Status::Done(redeem::Outcome::Granted(_)) => "membership.redeem_granted",
        Status::Done(redeem::Outcome::Replay) => "membership.redeem_replay",
        Status::Failed(error) => match error {
            redeem::Error::BadOrder => "membership.redeem_bad_order",
            redeem::Error::NotFound => "membership.redeem_not_found",
            redeem::Error::Unpaid => "membership.redeem_unpaid",
            redeem::Error::WrongPlan => "membership.redeem_wrong_plan",
            redeem::Error::Bound => "membership.redeem_bound",
            redeem::Error::Unconfigured => "membership.redeem_unconfigured",
            redeem::Error::Upstream => "membership.redeem_upstream",
            redeem::Error::Unreachable(_) => "membership.redeem_unreachable",
            redeem::Error::Malformed(_) => "membership.redeem_malformed",
        },
    };
    tr(cx, key)
}

/// 「当前状态」那一组：只剩一张 hero。等级与到期日都收进同一句，页面上没有第二处读数。
fn status_group(cx: &App) -> Group {
    group(
        tr(cx, "group.membership_state"),
        vec![
            Row::custom(|cx: &App| hero(cx).into_any_element()).keywords(&[
                "membership",
                "premium",
                "会员",
                "开通",
                "续费",
            ]),
        ],
    )
}

/// Hero：皇冠 + 产品名 + 状态句 + 一颗动作钮。
///
/// 强调只花在这一张上：皇冠与钮取 `primary`，其余一律 muted。整行不可点，命中面归钮自己；
/// 有效期内那颗钮换成 `outline`，续费不是这一屏的默认承诺。
fn hero(cx: &App) -> impl IntoElement {
    let day = today();
    let membership = ConfigHolder::membership(cx);
    let premium = membership.tier_on(day) == MembershipTier::Premium;
    let button = Button::new("settings-membership-open")
        .small()
        .label(action_label(premium, cx).to_string())
        .on_click(|_, _, _| {
            if !platform::open_url(AFDIAN_PAGE) {
                tracing::warn!("打不开爱发电主页：{AFDIAN_PAGE}");
            }
        });
    let button = if premium {
        button.outline()
    } else {
        button.primary()
    };

    h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .child(
            div()
                .size_8()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(cx.theme().primary.opacity(0.12))
                .child(
                    Icon::new(IconName::Crown)
                        .small()
                        .text_color(cx.theme().primary),
                ),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(tr(cx, "page.membership")),
                )
                .child(muted_label(status_line_for(membership, day, cx), cx)),
        )
        .child(button)
}

/// 「会员权益」那一组：一行「规划中」的实话 + 五行勾选清单。
///
/// 这一组**不改变任何功能能不能用**，所以不摆「已可用 / 未解锁」那一列：五行一样的状态字
/// 既没有信息量，也不说实话（眼下这些功能对谁都开着）。状态只在组首说一次。
fn benefit_group(cx: &App) -> Group {
    // 五句都写成字面量键的 `tr`，理由同 [`status_line`]：源码里摆着键，门禁才查得到九份表。
    let lines = [
        tr(cx, "membership.benefit_sync"),
        tr(cx, "membership.benefit_translate"),
        tr(cx, "membership.benefit_speech"),
        tr(cx, "membership.benefit_pdf"),
        tr(cx, "membership.benefit_opds"),
    ];
    let mut rows = vec![
        Row::custom(|cx: &App| benefits_note(cx).into_any_element()).keywords(&[
            "membership",
            "planned",
            "会员",
            "规划",
        ]),
    ];
    rows.extend(
        lines.into_iter().map(|text| {
            Row::custom(move |cx: &App| benefit_row(text.clone(), cx).into_any_element())
        }),
    );
    group(tr(cx, "group.membership_benefits"), rows)
}

/// 组首那一行：一枚「规划中」状态签 + 一句实话。
///
/// 签是**行的内容**（这一组权益现在的状态），不是对某个控件的解释，所以它留在卡里而不是
/// 躲进桌面才有的灰色说明。
fn benefits_note(cx: &App) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_shrink_0()
                .px_2()
                .py_0p5()
                .rounded_full()
                .border_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr(cx, "membership.planned")),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(muted_label(tr(cx, "membership.benefits_note"), cx)),
        )
}

/// 权益清单的一行：一枚勾 + 那句话。左右内缩归卡片，这一行只排自己那两格。
///
/// 勾取 `muted_foreground` 而不是 `primary`：这一整组只是清单，强调的额度留给 hero 那一颗钮
/// （规范：强调是有限预算，一屏一个焦点）。
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
    //! 这一屏钉四件事：**存的那一天 ↔ 状态句**（等级不是存进去的）、**状态组只剩一张
    //! hero**、**动作钮在有效期内换身份**、**兑换那一格的每一档各说各的一句**。「开通」那颗
    //! 钮的点击没在这里测：它交给系统浏览器，宿主测试到不了外部应用；`open_url` 的返回值
    //! 只影响一行日志。兑换那颗钮也不在这里点：那会发一次真请求、花掉读者的一单。
    //!
    //! **屏上真长成什么样仍未有人看过**，那一步要真机或桌面窗口。

    use super::{Status, action_label, membership_page, redeem_line, status_line, today};
    use crate::ui::features::settings::model::spec::RowKind;
    use crate::ui::services::config::ConfigHolder;
    use crate::ui::services::l10n::tr;
    use folio_core::infra::time;
    use folio_core::{Config, Language, Locale};
    use gpui_kit::{App, SharedString, TestAppContext};

    /// 装一份**显式**写着简中的配置：下面那几条要按简中的措辞比对句子，靠默认值恰好
    /// 是简中不算数（默认哪天换了，这几条会一起红成别的原因）。
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

    #[gpui_kit::test]
    fn the_card_line_names_which_of_the_three_states_it_is(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            install(cx);
            let day = today();
            assert_eq!(status_line(cx), tr(cx, "membership.never"));

            ConfigHolder::edit_membership(cx, |membership| membership.until_day = day + 29);
            let until = time::civil_date(day + 29);
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

    /// 三组：状态（一张 hero）、激活（编号那一格 + 动作那一格）、权益（规划中一句 + 五条）。
    /// 试用与取消两颗钮撤掉、兑换接上之后，这里钉的是当前形状：谁把两行读数或一颗试用钮
    /// 加回来，都得先改这条。
    #[gpui_kit::test]
    fn the_state_group_is_one_hero_and_the_benefits_carry_the_planned_note(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            install(cx);
            let page = membership_page(cx);
            assert_eq!(page.groups.len(), 3, "状态 + 激活 + 权益");
            assert_eq!(page.groups[0].rows.len(), 1, "状态那一组只剩一张 hero");
            assert!(matches!(
                &page.groups[0].rows[0].kind,
                RowKind::Custom { .. }
            ));
            assert_eq!(page.groups[1].rows.len(), 2, "编号那一格 + 动作那一格");
            assert!(matches!(&page.groups[1].rows[0].kind, RowKind::Text { .. }));
            assert!(matches!(
                &page.groups[1].rows[1].kind,
                RowKind::Custom { .. }
            ));
            assert_eq!(page.groups[2].rows.len(), 6, "规划中一句 + 五条权益");
            assert!(matches!(
                &page.groups[2].rows[0].kind,
                RowKind::Custom { .. }
            ));
        });
    }

    /// 每一档各说各的一句，而且**一句都不与另一档撞**。
    ///
    /// 撞了意味着两种不同的处境让读者看到同一句话 —— 比如把「这一单不是会员方案」与
    /// 「已经在另一台设备上兑过了」混成一句：前者该换个方案再买，后者该找回原来那台设备，
    /// 下一步完全两样。这条不测网络、也不测真兑换（那会花掉读者在爱发电的一单），
    /// 它比的只是档位落到哪几个键上、那些键解不解得开。
    #[gpui_kit::test]
    fn each_redeem_state_says_its_own_thing(cx: &mut TestAppContext) {
        use folio_core::net::redeem::{Error, Outcome};
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            install(cx);
            let states = [
                Status::Idle,
                Status::Running,
                Status::Done(Outcome::Granted(365)),
                Status::Done(Outcome::Replay),
                Status::Failed(Error::BadOrder),
                Status::Failed(Error::NotFound),
                Status::Failed(Error::Unpaid),
                Status::Failed(Error::WrongPlan),
                Status::Failed(Error::Bound),
                Status::Failed(Error::Unconfigured),
                Status::Failed(Error::Upstream),
                Status::Failed(Error::Unreachable("dns 挂了".into())),
                Status::Failed(Error::Malformed("不是 json".into())),
            ];
            let lines: Vec<(String, String)> = states
                .iter()
                .map(|status| {
                    let text = redeem_line(status, cx).to_string();
                    assert!(!text.is_empty(), "{status:?} 那一句是空的");
                    // 键名不该念到屏上：那是「表里根本没有这一行」的坏状态。
                    assert!(
                        !text.contains("membership."),
                        "{status:?} 念出了键名：{text}"
                    );
                    (format!("{status:?}"), text)
                })
                .collect();
            // 没兑过与在飞必须不同句，否则读者不明白那颗钮为什么点不动。
            assert_ne!(lines[0].1, lines[1].1);
            // 到账与重交也必须不同句：重交不是又一次成功。
            assert_ne!(lines[2].1, lines[3].1);
            // 往后每一档（各句失败原因）两两不同。
            for left in 4..lines.len() {
                for right in (left + 1)..lines.len() {
                    assert_ne!(
                        lines[left].1, lines[right].1,
                        "{} 与 {} 念的是同一句",
                        lines[left].0, lines[right].0
                    );
                }
            }
        });
    }

    #[gpui_kit::test]
    fn the_action_says_open_or_renew(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            install(cx);
            assert_eq!(action_label(false, cx), tr(cx, "action.membership_open"));
            assert_eq!(action_label(true, cx), tr(cx, "action.membership_renew"));
        });
    }
}
