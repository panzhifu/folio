use crate::ImageScale;
use crate::infra::config::{Config, Membership, MembershipTier};

fn load(text: &str) -> Config {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, text).unwrap();
    Config::load_from(&path)
}

/// 买断位与到期日**并存不互斥**：老的天数订单过期后，只要买过一次断就还是 Premium；
/// 解除时两位一起清。台账那一行由 `lifetime` 位说话，`days` 为 0。
#[test]
fn a_lifetime_bit_outlives_an_expired_days_order_and_clears_with_it() {
    let mut member = Membership {
        until_day: 100,
        ..Membership::default()
    };
    member.record_lifetime("3".repeat(29), 50);
    assert!(member.lifetime);
    assert_eq!(member.claims.len(), 1);
    assert!(member.claims[0].lifetime);
    assert_eq!(member.claims[0].days, 0);

    // 天数早过去了，买断还在。
    assert_eq!(member.tier_on(20_501), MembershipTier::Premium);

    // 解除是整段清零：买断位跟着走，服务端那一单标回可认领。
    member.clear_activation();
    assert!(!member.lifetime);
    assert_eq!(member.tier_on(50), MembershipTier::Standard);

    // 空订单号不记，也不置位。
    let mut member = Membership::default();
    member.record_lifetime("  ", 50);
    assert!(!member.lifetime);
    assert!(member.claims.is_empty());
}

/// 兑来的天数接在哪一天之后。三条都会把钱算少或算多，所以各自钉一次：
/// 从没开过时今天算第一天、有效期内续期不亏剩下的、过期了不从旧到期日起算。
#[test]
fn redeemed_days_are_counted_from_whichever_day_the_reader_is_actually_on() {
    let today = 20_500;
    // 从没开过：30 天就是今天起 30 个日子，最后一天是今天 + 29。
    assert_eq!(Membership::default().extended_on(30, today), today + 29);
    // 第一天就到期的极端：只给一天，到期日就是今天。
    assert_eq!(Membership::default().extended_on(1, today), today);
    // 还剩 10 天时续 30：旧的 10 天一天不少，新到期日在旧的后面。
    let active = Membership {
        until_day: today + 9,
        ..Membership::default()
    };
    assert_eq!(active.extended_on(30, today), today + 39);
    // 过期一年后再兑：不从旧到期日起算，否则白送了中间那三百多天。
    let stale = Membership {
        until_day: today - 365,
        ..Membership::default()
    };
    assert_eq!(stale.extended_on(30, today), today + 29);
    // 同一单重交：天数不动，且**不许**把过期的到期日拉回今天。
    assert_eq!(stale.extended_on(0, today), today - 365);
    assert_eq!(active.extended_on(0, today), today + 9);
    // 兑完仍在有效期内这一条，由新到期日自己说话。
    assert_eq!(
        Membership::default().extended_on(30, today),
        Membership {
            until_day: today + 29,
            ..Membership::default()
        }
        .until_day
    );
}

/// 记一笔到账：天数按 [`Membership::extended_on`] 接上去，台账留一行。
/// **同一单再记一次不叠成两行** —— 解除之后在同一台重新兑，屏上要看的还是那一单，
/// 两行就成了「这台名下有两笔」，而服务端只有一行。
#[test]
fn recording_a_claim_extends_the_balance_and_keeps_one_line_per_order() {
    let order = "1".repeat(29);
    let mut got = Membership::default();
    got.record(&order, 30, 20_000);
    assert_eq!(
        got.until_day, 20_029,
        "从没开过那一档：今天就是买到的第一天"
    );
    assert_eq!(got.claims.len(), 1);
    assert_eq!(got.claims[0].days, 30);

    got.record(&order, 30, 20_010);
    assert_eq!(
        got.claims.len(),
        1,
        "同一单记成两行，「这台名下有什么」就没人读得懂了"
    );
    assert_eq!(got.claims[0].on_day, 20_010, "留的是最后一次那一趟");
    // 同一单第二次「发货」在这一版之后不会再发生（服务端把重复那一趟回成 `moved` 或
    // `replay`），所以这里续上的 30 天钉的是「万一两边形状对不上，数字要看得见」，
    // 而不是让它悄悄翻倍。
    assert_eq!(got.until_day, 20_059);

    // 重交（0 天）与没号的单都不进台账：前者没给新东西，后者没什么可认。
    got.record(&order, 0, 20_040);
    got.record("", 30, 20_040);
    assert_eq!(got.claims.len(), 1);
    assert_eq!(got.until_day, 20_059, "0 天不该把到期日往后挪");
}

/// 解除是**整段清零**：剩下的时间与台账一起没。
///
/// 「扣掉那一笔的天数」那条路走不通：一笔 365 天买了两个月，该退 365 还是 300？谁也算不
/// 清，而服务端那一侧只有「这一单还绑不绑在这台」一个事实。界面在按下去之前就把这一句
/// 摆出来了（`confirm.release.body`）。
#[test]
fn clearing_activation_forfeits_the_balance_and_the_ledger() {
    let mut got = Membership::default();
    got.record("9".repeat(29), 365, 20_000);
    assert!(got.until_day > 20_300);
    got.clear_activation();
    assert_eq!(got.until_day, 0);
    assert!(got.claims.is_empty());
    assert_eq!(got.tier_on(20_001), MembershipTier::Standard);
}

/// 台账要能从 JSON 读回来；**老配置里没有 `claims` 这一格，按空表读**，不许把整个
/// `membership` 判成读坏了 —— 那会顺手抹掉读者的到期日。
#[test]
fn the_ledger_round_trips_and_an_older_config_without_it_still_loads() {
    let with = load(
        r#"{"membership":{"until_day":20500,"claims":[{"order":"111","days":30,"on_day":20470}]}}"#,
    );
    assert_eq!(with.membership.claims.len(), 1);
    assert_eq!(with.membership.claims[0].days, 30);
    assert_eq!(with.membership.until_day, 20_500);
    assert_eq!(
        load(r#"{"membership":{"until_day":20500}}"#)
            .membership
            .claims
            .len(),
        0,
        "这一版之前写下的配置，读回来不该丢掉到期日"
    );
    assert_eq!(
        load(r#"{"membership":{"until_day":20500}}"#)
            .membership
            .until_day,
        20_500
    );
}

/// 「图片缩放」那一格：三档各认自己的键，认不出的回落**初始那一档**，而**同一节里别的键
/// 一个都不许跟着没**。
///
/// 这一格是自己实现 `Deserialize` 的（同 [`PageTurn`]、[`TextDirection`]）：derive 碰到
/// 拼错的字符串会让整节 `Reader` 退回默认 —— 字号、行距、字体家族就成了一个错键的代价。
#[test]
fn the_image_scale_tiers_round_trip_while_an_unknown_one_only_falls_back_for_itself() {
    for (text, want) in [
        ("follow", ImageScale::Follow),
        ("screen", ImageScale::Screen),
        ("original", ImageScale::Original),
        // 认不出的一律落初始那一档（现在是「铺满」），不许把读者的别的偏好卷走。
        ("fullscreen", ImageScale::default()),
        ("", ImageScale::default()),
    ] {
        let got = load(&format!(r#"{{"reader":{{"image_scale":"{text}"}}}}"#));
        assert_eq!(got.reader.image_scale, want, "写法 {text:?}");
    }
    let both = load(r#"{"reader":{"image_scale":"spin","font_size":22.0}}"#);
    assert_eq!(both.reader.image_scale, ImageScale::default());
    assert_eq!(
        both.reader.font_size, 22.0,
        "认不出的那一格不该把字号一起抹掉"
    );
    // 缺节 / 缺字段都按初始那一档读回来。
    assert_eq!(load("{}").reader.image_scale, ImageScale::default());
    // 写出去的形态与读回来的一致（配置文件里就是这三个词），初始那一档写的是 `screen`。
    let text = serde_json::to_string(&Config::default().reader).unwrap();
    assert!(
        text.contains(r#""image_scale":"screen""#),
        "写出的是 {text}"
    );
}
