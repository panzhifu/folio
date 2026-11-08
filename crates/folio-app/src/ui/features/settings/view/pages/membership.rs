//! 高级会员：一张**买断制**的会员卡。本机记一个永久位加一份**这台的兑换台账**，
//! 等级是渲染时按今天现算出来的。
//!
//! 这一屏**不自己收钱**：开通那一颗钮把读者交给爱发电；成交之后回到这一屏，在「激活会员」
//! 那一组填爱发电的订单编号兑换 —— 付没付过钱由 `folio-redeem` 那支函数向平台查（爱发电的
//! api token 不能进客户端），本地这一层只管形状与结果句子。买断单兑成了写的是
//! `membership.lifetime` 这一位（永久），老的天数单照旧累计 `until_day` —— 两种权益并存，
//! 谁在谁就算数。权益网格那六格是**真的门禁**：云同步、网盘取书、听书、翻译与 AI 的入口
//! 都问 [`activation::premium`]。
//!
//! 「设备」那一组管换机：解除是**按设备**而不是按订单号的（本机从来没存过订单号，而重装
//! 之后连草稿都没了），而且是**软解除** —— 服务端留下那一行只标一个时间戳，删不得。买断单
//! 解除后到新设备上**认领的还是永久**；天数单搬过去的只带剩下的几天。
//!
//! 「今天」由 [`today`] 现取，偏移来自 [`crate::ui::host::local_offset_secs`]：日界线跟着
//! 读者走，写死 UTC 会让跨过日界线那一天的读数差一格。统计那几屏按同一条取偏移。

use super::super::*;
use crate::ui::components::muted_label;
use crate::ui::host::local_offset_secs;
use crate::ui::platform;
use crate::ui::services::activation::{self, AFDIAN_PAGE, ReleaseStatus, Status};
use folio_core::{Membership, MembershipTier, infra::time, net::redeem};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, div, px, rgb,
};

/// 会员卡的点睛金。全 app 唯一一处主题 token 之外的颜色：黑金卡是这一屏的身份，
/// 金色只出现在卡上，页面其余部分仍是 theme 的话。
fn gold() -> gpui_kit::Rgba {
    rgb(0xD9A119)
}

/// 今天，本地日序号。等级与到期日都从这一个数算，页面上没有第二处自己读时钟的地方。
pub(in crate::ui::features::settings) fn today() -> i64 {
    time::local_day(time::now(), local_offset_secs())
}

/// 根屏那张卡片要的一句话：永久 / 还有效（哪天到期）/ 已经过了（哪天过的）/ 从没开过。
///
/// hero 不念这一句（它有自己的一份，见 [`hero_status`]）；两个入口说的是同一件事，
/// 但措辞各按各的场合。分支各写一遍字面量键，而不是把键挑进变量再统一插值：门禁那条
/// 测试是**扫源码里的字面量**来要求九份表都有这一行的（见 `services/l10n.rs`）。
pub(in crate::ui::features::settings) fn status_line(cx: &App) -> SharedString {
    status_line_for(ConfigHolder::membership(cx), today(), cx)
}

/// 同一句话，但配置与「今天」由调用方读好再递进来。
fn status_line_for(membership: Membership, day: i64, cx: &App) -> SharedString {
    if membership.lifetime {
        return tr(cx, "membership.lifetime_status");
    }
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

/// 会员卡上的状态句。与根屏那句分开：卡上「没开过」要顺带把买断这件事说了 ——
/// 这是转化屏的开场白，不是一句纯状态。
fn hero_status(membership: &Membership, day: i64, cx: &App) -> SharedString {
    if membership.lifetime {
        return tr(cx, "membership.lifetime_status");
    }
    if membership.until_day <= 0 {
        return tr(cx, "membership.hero_pitch");
    }
    let date = time::civil_date(membership.until_day);
    if membership.tier_on(day) == MembershipTier::Premium {
        tf(cx, "membership.expires", &[("date", &date)]).into()
    } else {
        tf(cx, "membership.expired", &[("date", &date)]).into()
    }
}

/// 那颗动作钮此刻的措辞：不在有效期内是「一次买断」，天数档还活着是「升级买断」。
/// 两种措辞同一个视觉档（见 [`cta_row`]）—— 换的是话，不是强调。
fn cta_label(premium: bool, cx: &App) -> SharedString {
    if premium {
        tr(cx, "membership.buyout_upgrade")
    } else {
        tr(cx, "membership.buyout_cta")
    }
}

/// 那颗买断钮：**黑色实心**（primary）、整宽居中 —— 买断是这一屏唯一的主推，
/// 不给「已开通的人」换描边款。天数档在期是「升级买断」，措辞换、强调不换；
/// 买断到手的读者整颗退场（见 [`membership_page`]），永久位就是结果本身。
/// 点击交给爱发电 —— 这一屏不收钱，理由见模块头。
fn cta_row(cx: &App) -> impl IntoElement {
    let membership = ConfigHolder::membership(cx);
    let premium = membership.tier_on(today()) == MembershipTier::Premium;
    Button::new("settings-membership-buy")
        .large()
        .w_full()
        .primary()
        .label(cta_label(premium, cx).to_string())
        .on_click(|_, _, _| {
            if !platform::open_url(AFDIAN_PAGE) {
                tracing::warn!("打不开爱发电主页：{AFDIAN_PAGE}");
            }
        })
}

pub(in crate::ui::features::settings) fn membership_page(cx: &App) -> Page {
    // 会员那一节在这一屏**只读一遍**：`lifetime` 与台账同时决定「那一组摆不摆」和「组里念哪
    // 几笔」，读两次就可能出现两次答案不一样的那一帧（同 [`status_line_for`] 那条理由）。
    let membership = ConfigHolder::membership(cx);
    let mut groups = vec![status_group(&membership, cx), benefit_group(cx)];
    groups.push(redeem_group(cx));
    // 从没激活过的这台**没有那一组**：摆出来只会是一颗按不动的钮，而规范那条
    // 是「不适用就藏起来，或 disable 掉并给出为什么」。激活过一次（天数位或永久位）
    // 之后这一组自己出现；解除干净之后它自己退场。
    if membership.until_day > 0 || membership.lifetime {
        groups.push(device_group(&membership, cx));
    }
    page(tr(cx, "page.membership"), IconName::Crown, groups)
}

/// 各功能页的非会员那一屏：一句「这是什么功能、开了能得到什么」+ 一颗开通钮。
///
/// 门禁判据只有 [`activation::premium`] 一条；这里只负责把那一句话摆出来。判据在页
/// 函数自己那里 —— 非会员时整页换成这一组，会员时这一组根本不构造。组标题借
/// `page.membership`（「高级会员」）：这一组本来就是那张会员卡的引子。
pub(in crate::ui::features::settings) fn gate_group(text_key: &'static str, cx: &App) -> Group {
    group(
        tr(cx, "page.membership"),
        vec![Row::custom(move |cx: &App| {
            crate::ui::components::gate_banner(tr(cx, text_key), cx).into_any_element()
        })],
    )
}

/// 「当前状态」那一组：一张黑金会员卡 + 一颗买断钮，两者装进**同一张设置卡**，
/// 与「会员权益」「激活会员」那几组同形（买断到手后这一颗自己退场）。
fn status_group(membership: &Membership, cx: &App) -> Group {
    let lifetime = membership.lifetime;
    let mut rows = vec![
        Row::custom(|cx: &App| hero(cx).into_any_element()).keywords(&[
            "membership",
            "premium",
            "会员",
            "开通",
            "买断",
            "永久",
        ]),
    ];
    if !lifetime {
        rows.push(
            Row::custom(|cx: &App| cta_row(cx).into_any_element()).keywords(&[
                "membership",
                "buy",
                "会员",
                "买断",
                "购买",
            ]),
        );
    }
    group(tr(cx, "group.membership_state"), rows)
}

/// 会员卡：黑底（深色主题下翻成浅底）+ 金边 + 皇冠 + 状态 + 页脚那句「一次购买 · 永久有效」。
///
/// 卡底取 `foreground`、卡上的字取 `background`：浅色主题下是一张黑卡，深色主题下翻成
/// 一张浅卡 —— 两端都是「页面上唯一一张反色的卡」，高级感来自这份反差，而不是来自
/// 多少种颜色。金色只落在边、章与 PRO 签上，是点睛不是铺色。
fn hero(cx: &App) -> impl IntoElement {
    let membership = ConfigHolder::membership(cx);
    let ink = cx.theme().background;
    let ink_muted = cx.theme().background.opacity(0.62);
    v_flex()
        .w_full()
        .gap_3()
        .rounded(px(16.0))
        .border_1()
        .border_color(gold().opacity(0.38))
        .bg(cx.theme().foreground)
        .p_4()
        .text_color(ink)
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .size_9()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(ink.opacity(0.12))
                        .child(Icon::new(IconName::Crown).small().text_color(gold())),
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
                        .child(div().text_sm().text_color(ink_muted).child(hero_status(
                            &membership,
                            today(),
                            cx,
                        ))),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .px_2()
                        .py_0p5()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(gold().opacity(0.5))
                        .child(div().text_xs().text_color(gold()).child("PRO")),
                ),
        )
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_2()
                .border_t_1()
                .border_color(gold().opacity(0.18))
                .pt_2p5()
                .child(
                    div()
                        .text_xs()
                        .text_color(ink_muted)
                        .child(if membership.lifetime {
                            tr(cx, "membership.hero_footer_left").to_string()
                        } else {
                            String::new()
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(ink_muted)
                        .child(tr(cx, "membership.hero_footer_right")),
                ),
        )
}

/// 「会员权益」那一组：一格 2×3 权益网格。
///
/// 这六格是**真的门禁**（云同步 / 网盘取书 / 听书 / 翻译与 AI 的入口都问
/// [`activation::premium`]），不再是「规划中」的清单 —— 所以原来那枚「规划中」签
/// 撤了。哪些功能免费不在这页上摆字：免费的功能根本不会被门禁拦，读者用到哪里
/// 哪里就是答案。
fn benefit_group(cx: &App) -> Group {
    group(
        tr(cx, "group.membership_benefits"),
        vec![
            Row::custom(|cx: &App| benefits_row(cx).into_any_element()).keywords(&[
                "membership",
                "benefits",
                "会员",
                "权益",
            ]),
        ],
    )
}

/// 一格权益：图标章 + 名字 + 一句话。`flex_1` + `min_w` 让它手机上一行两格、
/// 桌面宽屏自动多列 —— 网格的列数交给宽度，不写死。
fn benefit_tile(icon: IconName, name: &str, desc: &str, cx: &App) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w(px(148.0))
        .gap_1p5()
        .rounded(px(12.0))
        .bg(cx.theme().muted)
        .p_3()
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(Icon::new(icon).small().text_color(cx.theme().foreground))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr(cx, name).to_string()),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(tr(cx, desc).to_string()),
        )
}

/// 权益网格，独占一个自定义行。
fn benefits_row(cx: &App) -> impl IntoElement {
    let tiles = [
        (
            IconName::Cloud,
            "membership.grid_sync",
            "membership.grid_sync_desc",
        ),
        (
            IconName::HardDrive,
            "membership.grid_cloud",
            "membership.grid_cloud_desc",
        ),
        (
            IconName::Headphones,
            "membership.grid_speech",
            "membership.grid_speech_desc",
        ),
        (
            IconName::Languages,
            "membership.grid_ai",
            "membership.grid_ai_desc",
        ),
        (
            IconName::Smartphone,
            "membership.grid_devices",
            "membership.grid_devices_desc",
        ),
        (
            IconName::Sparkles,
            "membership.grid_future",
            "membership.grid_future_desc",
        ),
    ];
    let mut grid = div().flex().flex_wrap().gap_2().w_full();
    for (icon, name, desc) in tiles {
        grid = grid.child(benefit_tile(icon, name, desc, cx));
    }
    grid.into_any_element()
}

/// 「设备」那一组：左边设备名，右边「解除」。按下先问一句，面板开着时整行换成面板。
///
/// 台账（`membership.claims`）不再逐笔摆上屏：读者要的答案是「我现在是不是会员」，
/// 那句在黑金卡上（永久 / 到期日）；「哪一单兑的」是排查时才要的东西，在日志与
/// 爱发电的订单页里。权益随设备走，换机靠的是服务端那一行绑定，不是这份本地清单。
///
/// 为什么按**设备**解除而不是按订单：本机从来没存过订单号（那条决定写在
/// `services/activation.rs` 顶上），而重装之后连草稿都没了 —— 要订单号才能解除，等于让最
/// 需要它的人正好交不出来。服务端那一侧的事实本来也就是「哪几行绑在这一台」。
fn device_group(_membership: &Membership, cx: &App) -> Group {
    group(
        tr(cx, "group.membership_devices"),
        vec![
            Row::custom(|cx: &App| {
                if activation::confirming(cx) {
                    release_panel(cx).into_any_element()
                } else {
                    device_row(cx).into_any_element()
                }
            })
            .keywords(&[
                "device", "unbind", "release", "afdian", "设备", "解除", "激活", "换机",
            ]),
        ],
    )
}

/// 设备名：Android 读 `ro.product.model`，桌面读主机名；都拿不到就用组名「设备」。
///
/// 读者要认的是「哪一台手机 / 哪一台电脑」，不是 `device_id` 那串哈希 —— 后者只在
/// 排查同步文件时有用，摆在这一行只会让人以为自己看到的是设备名。
fn device_label(cx: &App) -> SharedString {
    platform::device_name()
        .map(SharedString::from)
        .unwrap_or_else(|| tr(cx, "group.membership_devices"))
}

/// 设备那一行：左边设备名，右边一颗小「解除」。只有解除那一趟有话说时才在名字下面
/// 补一句状态：空闲时不摆「把这一单挪到另一台设备」这类解释，读者要看的只是设备名。
///
/// 钮取 `ghost` 而不是 `danger`：按它还只是**要问一句**，真正的承诺在面板里那一颗。
/// 一屏里两颗红钮会互相抢强调，而这一屏的焦点预算已经花在买断钮上。
fn device_row(cx: &App) -> gpui_kit::AnyElement {
    let status = activation::release_status(cx);
    let busy = status == ReleaseStatus::Running;
    let name = v_flex().flex_1().min_w_0().gap_0p5().child(
        div()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .child(device_label(cx)),
    );
    let name = if status == ReleaseStatus::Idle {
        name
    } else {
        name.child(muted_label(release_line(&status, cx), cx))
    };
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .child(name)
        .child(
            Button::new("settings-membership-release")
                .label(tr(cx, "action.release").to_string())
                .ghost()
                .small()
                .flex_shrink_0()
                .disabled(busy)
                .on_click(|_, _, cx| activation::ask_release(cx)),
        )
        .into_any_element()
}

/// 确认面板：一句问的是什么、一句后果、两颗钮（取消在左，结果在右、`danger`）。
///
/// 与「存储」那几颗清理钮同一形状（`pages/storage.rs` 的 `confirm_panel`）：标题与正文各是
/// **一条整句**，不拼「通用问句 + 档位名」—— 德语动词要落句尾、俄语要看性数，拼出来的是一句
/// 不像界面语言的机器话。
fn release_panel(cx: &App) -> gpui_kit::AnyElement {
    v_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(tr(cx, "confirm.release.title")),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr(cx, "confirm.release.body")),
        )
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .child(
                    Button::new("settings-release-cancel")
                        .ghost()
                        .label(tr(cx, "shelf.cancel").to_string())
                        .on_click(|_, _, cx| activation::cancel_release(cx)),
                )
                .child(
                    // 结果那颗写动词：对象已经问在标题里了，再摆一遍名字就是两个名词并排。
                    Button::new("settings-release-go")
                        .danger()
                        .label(tr(cx, "action.release").to_string())
                        .disabled(activation::release_status(cx) == ReleaseStatus::Running)
                        .on_click(|_, _, cx| activation::release_now(cx)),
                ),
        )
        .into_any_element()
}

/// 那一格此刻该念哪一句。句子从**档位**来而不是从 `error.to_string()` 来 —— 同
/// [`redeem_line`] 那条理由（core 那些 Display 是中文写死的）。
fn release_line(status: &ReleaseStatus, cx: &App) -> SharedString {
    let key = match status {
        ReleaseStatus::Idle => "membership.release_idle",
        ReleaseStatus::Running => "membership.release_running",
        ReleaseStatus::Done(redeem::Release::Released(_)) => "membership.release_done",
        ReleaseStatus::Done(redeem::Release::NothingBound) => "membership.release_none",
        ReleaseStatus::Failed(_) => "membership.release_failed",
    };
    tr(cx, key)
}

/// 「激活会员」那一组：订单编号 + 兑换钮，结果就落在同一组里。
///
/// 编号那一行**不带标题**（[`spec::Row::placeholder`]）：「爱发电订单编号」这六个字
/// 进框里当占位虚字 —— 框本身就是全部说明，标题挡在框上反而多一层。桌面端没有
/// 这条渲染路，标题照旧摆在框旁。
///
/// 手机端把「兑换」摆进编号那一行的右侧（[`spec::Row::text_action`]）：键盘弹起时
/// 输入框与动作在同一行，不必再往下找那颗钮。桌面端仍是「输入框一行 + 动作一行」。
/// 买断单也从这一格进：订单编号就是激活码，验过了置的是永久位。
fn redeem_group(cx: &App) -> Group {
    let mut rows = Vec::new();
    if cfg!(any(target_os = "android", target_os = "ios")) {
        rows.push(
            Row::text_action(
                tr(cx, "membership.order"),
                activation::draft,
                activation::set_draft,
                tr(cx, "membership.redeem"),
                activation::redeem_now,
                |cx| redeem_status(cx) == Status::Running,
            )
            .placeholder(tr(cx, "membership.order"))
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
        );
        // 空闲时不摆任何提示句：编号框与右边的兑换钮已经把这件事说完。
        // 在飞 / 结果 / 失败才补一句，落在同一张卡里。
        if redeem_status(cx) != Status::Idle {
            rows.push(Row::custom(|cx: &App| {
                v_flex()
                    .w_full()
                    .items_center()
                    .child(muted_label(redeem_line(&redeem_status(cx), cx), cx))
                    .into_any_element()
            }));
        }
    } else {
        rows.push(
            Row::text(
                tr(cx, "membership.order"),
                activation::draft,
                activation::set_draft,
            )
            .placeholder(tr(cx, "membership.order"))
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
        );
        rows.push(Row::custom(redeem_row));
    }
    group(tr(cx, "membership.redeem_group"), rows)
}

fn redeem_status(cx: &App) -> Status {
    activation::handle(cx)
        .map(|entity| entity.read(cx).status().clone())
        .unwrap_or_default()
}

/// 结果句 + 那颗钮：**两样都居中**，摆输入框的正下方 —— 激活是这一格里唯一的动作，
/// 居中说的是「就这一件事」。
///
/// 钮取 `ghost`：这一屏的强调预算已经花在 hero 下面那颗黑色买断钮上（一屏一个焦点），
/// 兑换是同一屏里的第二个动作。在飞的期间 `disabled` —— 双击会把同一单交两次，
/// 而第二次只会以「已经兑过了」收场，那是白给读者的一句看不懂的话。
fn redeem_row(cx: &App) -> gpui_kit::AnyElement {
    let handle = activation::handle(cx);
    let status = handle
        .as_ref()
        .map(|entity| entity.read(cx).status().clone())
        .unwrap_or_default();
    let busy = status == Status::Running;
    v_flex()
        .w_full()
        .items_center()
        .gap_2()
        .child(muted_label(redeem_line(&status, cx), cx))
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
/// 这里也不报天数：上面那张 hero 的状态句就是结果本身，重复一遍只是噪音。
fn redeem_line(status: &Status, cx: &App) -> SharedString {
    let key = match status {
        Status::Idle => "membership.redeem_idle",
        Status::Running => "membership.redeem_running",
        Status::Done(redeem::Outcome::Granted(_)) => "membership.redeem_granted",
        // 买断两档各一句：到账说的是「永久到手」，认领说的是「那一单现在在这台上」——
        // 念成同一句会让换机的读者以为自己又付了一次。
        Status::Done(redeem::Outcome::GrantedLifetime) => "membership.redeem_granted_lifetime",
        Status::Done(redeem::Outcome::MovedLifetime) => "membership.redeem_moved_lifetime",
        // 认领来的那一单单独一句：这一趟没有新买的时间，说的是「那一单现在在这台上」。
        // 念成「已到账」会让人以为自己又付了一次。
        Status::Done(redeem::Outcome::Moved(_)) => "membership.redeem_moved",
        Status::Done(redeem::Outcome::Replay) => "membership.redeem_replay",
        Status::Failed(error) => match error {
            redeem::Error::BadOrder => "membership.redeem_bad_order",
            redeem::Error::NotFound => "membership.redeem_not_found",
            redeem::Error::Unpaid => "membership.redeem_unpaid",
            redeem::Error::WrongPlan => "membership.redeem_wrong_plan",
            redeem::Error::Bound => "membership.redeem_bound",
            redeem::Error::Spent => "membership.redeem_spent",
            redeem::Error::Unconfigured => "membership.redeem_unconfigured",
            redeem::Error::Upstream => "membership.redeem_upstream",
            redeem::Error::Unreachable(_) => "membership.redeem_unreachable",
            redeem::Error::Malformed(_) => "membership.redeem_malformed",
        },
    };
    tr(cx, key)
}

#[cfg(test)]
mod tests {
    //! 这一屏钉四件事：**存的那一天/永久位 ↔ 状态句**（等级不是存进去的）、**状态组 hero 加
    //! 条件 CTA**、**动作钮在有效期内的身份**、**兑换那一格的每一档各说各的一句**。「开通」
    //! 那颗钮的点击没在这里测：它交给系统浏览器，宿主测试到不了外部应用；`open_url` 的返回值
    //! 只影响一行日志。兑换那颗钮也不在这里点：那会发一次真请求、花掉读者的一单。
    //!
    //! **屏上真长成什么样仍未有人看过**，那一步要真机或桌面窗口。

    use super::{Status, cta_label, membership_page, redeem_line, status_line, today};
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
    fn the_card_line_names_which_of_the_four_states_it_is(cx: &mut TestAppContext) {
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

            // 买断位一票通过：过期了的天数档 readers 只要买过断，状态句就是「永久会员」。
            ConfigHolder::edit_membership(cx, |membership| membership.lifetime = true);
            assert_eq!(status_line(cx), tr(cx, "membership.lifetime_status"));

            ConfigHolder::edit_membership(cx, |membership| {
                membership.lifetime = false;
                membership.until_day = day - 1;
            });
            let past = time::civil_date(day - 1);
            assert_eq!(
                status_line(cx),
                SharedString::from(format!("{past} 已到期"))
            );
        });
    }

    /// 四组：状态（hero + 条件 CTA）、权益（网格一格）、激活（编号那一格 + 动作那一格）、
    /// 这台设备（条件出现）。试用与取消两颗钮撤掉、兑换接上之后，这里钉的是当前形状：
    /// 谁把多行读数或一颗试用钮加回来，都得先改这条。
    #[gpui_kit::test]
    fn the_state_group_is_a_hero_with_a_conditional_cta_and_the_benefits_are_one_grid(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            install(cx);
            let page = membership_page(cx);
            assert_eq!(page.groups.len(), 3, "没激活过：状态 + 权益 + 激活");
            assert_eq!(page.groups[0].rows.len(), 2, "hero + 买断钮");
            assert!(matches!(
                &page.groups[0].rows[0].kind,
                RowKind::Custom { .. }
            ));
            assert_eq!(page.groups[1].rows.len(), 1, "权益整组一格：网格");
            assert!(matches!(
                &page.groups[1].rows[0].kind,
                RowKind::Custom { .. }
            ));
            assert_eq!(page.groups[2].rows.len(), 2, "编号那一格 + 动作那一格");
            assert!(matches!(&page.groups[2].rows[0].kind, RowKind::Text { .. }));

            // 天数档还活着：CTA 换身份但不消失（升级买断）。「这台设备」插在激活之后。
            ConfigHolder::edit_membership(cx, |membership| {
                membership.record("3".repeat(29), 30, today())
            });
            let page = membership_page(cx);
            assert_eq!(page.groups.len(), 4, "状态 + 权益 + 激活 + 这台设备");
            assert_eq!(page.groups[0].rows.len(), 2, "天数档仍有升级买断那一颗");

            // 买断到手：CTA 整颗退场 —— 永久位就是结果本身。
            ConfigHolder::edit_membership(cx, |membership| membership.lifetime = true);
            let page = membership_page(cx);
            assert_eq!(page.groups[0].rows.len(), 1, "永久会员没有续费钮");

            // 解除干净之后「这台设备」自己退场：留着就是一颗按不动的钮。
            ConfigHolder::edit_membership(cx, |membership| membership.clear_activation());
            assert_eq!(membership_page(cx).groups.len(), 3, "解除后回到三组");
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
                Status::Done(Outcome::GrantedLifetime),
                Status::Done(Outcome::Moved(200)),
                Status::Done(Outcome::MovedLifetime),
                Status::Done(Outcome::Replay),
                Status::Failed(Error::BadOrder),
                Status::Failed(Error::NotFound),
                Status::Failed(Error::Unpaid),
                Status::Failed(Error::WrongPlan),
                Status::Failed(Error::Bound),
                Status::Failed(Error::Spent),
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
            // 到账、买断到账、认领、买断认领四档各是一句：买断与天数混成一句，
            // 换机的读者就分不清自己拿到的是哪种权益。
            for left in 2..6 {
                for right in (left + 1)..7 {
                    assert_ne!(
                        lines[left].1, lines[right].1,
                        "{} 与 {} 念的是同一句",
                        lines[left].0, lines[right].0
                    );
                }
            }
            // 往后每一档（各句失败原因）两两不同。
            for left in 7..lines.len() {
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
    fn the_cta_says_buyout_or_upgrade(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            install(cx);
            assert_eq!(cta_label(false, cx), tr(cx, "membership.buyout_cta"));
            assert_eq!(cta_label(true, cx), tr(cx, "membership.buyout_upgrade"));
        });
    }
}
