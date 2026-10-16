//! 列表标记的内容：把 `<li>` 的序号折成屏上那一小串字。
//!
//! 为什么这一层归排版而不归层叠：CSS 层只管「作者要哪一种记数」，而序号是**数出来**的
//! —— 同层 `display: list-item` 的兄弟按文档序递增，`<ol start>` 挪起点、`<li value>`
//! 挪这一项与其后的续值。那是排这一章时才有的信息，层叠期看不到兄弟节点。
//!
//! 后缀（`.` / `、`）照的是浏览器的观感，规范把 `::marker` 的内容留给实现：数字与
//! 字母类跟一个点，汉字与假名类跟一个顿号。

use crate::content::format::css::computed::ListStyle;

/// 第 `index` 项（**1 起**）的标记文字。`None` = 一个字符都不画。
///
/// 两种情形给 `None`：作者写了 `list-style-type: none`，以及这一项落在表外又实在
/// 编不出来（假名表只有 46 / 47 格，见 [`GOJÜON`] 下面那段）。后者会留一条
/// `debug!`，因为「第 60 项没有标记」是读者看得见的洞，不该静默。
pub(super) fn marker_text(kind: ListStyle, index: usize) -> Option<String> {
    let index = index.max(1);
    Some(match kind {
        ListStyle::None => return None,
        ListStyle::Disc => "•".into(),
        ListStyle::Circle => "◦".into(),
        ListStyle::Square => "▪".into(),
        ListStyle::Decimal => format!("{index}."),
        ListStyle::LowerAlpha => format!("{}.", alphabetic(index, false)),
        ListStyle::UpperAlpha => format!("{}.", alphabetic(index, true)),
        ListStyle::LowerRoman => format!("{}.", roman(index)),
        ListStyle::UpperRoman => format!("{}.", roman(index).to_uppercase()),
        ListStyle::CjkIdeographic => format!("{}、", cjk_ideographic(index)),
        ListStyle::Hiragana => kana(index, &GOJÜON, false)?,
        ListStyle::HiraganaIroha => kana(index, &IROHA, false)?,
        ListStyle::Katakana => kana(index, &GOJÜON, true)?,
        ListStyle::KatakanaIroha => kana(index, &IROHA, true)?,
    })
}

/// 拉丁字母记数：CSS 的 `lower-alpha` 是 a…z, aa…（**没有 0 位**的双射记数），
/// 不是把 26 当进制直接换算 —— 后者会把第 27 项写成 `aa` 与 `ab` 之外的另一种东西。
fn alphabetic(mut index: usize, upper: bool) -> String {
    let base = if upper { b'A' } else { b'a' };
    let mut chars = Vec::new();
    loop {
        let rem = (index - 1) % 26;
        chars.push((base + rem as u8) as char);
        index = (index - 1) / 26;
        if index == 0 {
            break;
        }
    }
    chars.reverse();
    chars.into_iter().collect()
}

/// 罗马数字：标准减写表，从大到小贪心。超出 3999（`MMMCMXCIX`）的写法实现各异，
/// 我们照常见的做法把千位以上直接堆 `M` —— 书目里没有更长的列表，堆出来也不会错读。
fn roman(mut index: usize) -> String {
    const STEPS: [(usize, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (value, glyph) in STEPS {
        while index >= value {
            out.push_str(glyph);
            index -= value;
        }
    }
    out
}

/// 汉字记数（`cjk-ideographic`）：一…十…百…千，带中文的零规则。
///
/// 写法照通行习惯：`10 → 十`、`11 → 十一`、`20 → 二十`、`105 → 一百零五`、
/// `1010 → 一千零一十`。超过 9999 退回阿拉伯数字 —— 那是任何一本列表都不会到的深度，
/// 而「退回 12345、」比凭空造一个「万一、」好核对。
fn cjk_ideographic(index: usize) -> String {
    const DIGITS: [char; 10] = ['零', '一', '二', '三', '四', '五', '六', '七', '八', '九'];
    if index > 9999 {
        return index.to_string();
    }
    let units: [(usize, char); 4] = [(1000, '千'), (100, '百'), (10, '十'), (1, '一')];
    let mut out = String::new();
    let mut rest = index;
    let mut zero_pending = false;
    for (value, unit) in units {
        let digit = rest / value;
        rest %= value;
        if digit == 0 {
            // 中间的零要等**后面真出现非零位**才补写，末尾的零不写。
            zero_pending = !out.is_empty() && rest > 0;
            continue;
        }
        if zero_pending {
            out.push(DIGITS[0]);
            zero_pending = false;
        }
        // 「十」在**整头就是十几**时不读「一十」（十、十一……）；一旦前面已经有位，
        // 那一就要写出来 —— 「一百一十」省成一「百十」就读不成数了。
        if !(unit == '十' && digit == 1 && out.is_empty()) {
            out.push(DIGITS[digit]);
        }
        if unit != '一' {
            out.push(unit);
        }
    }
    out
}

/// 假名记数（`hiragana` / `katakana` 及其 `*-iroha` 变体）：按表逐格取，超过表长
/// 就不画（并留一条 `debug!`）。
///
/// 规范让实现用叠字符（ゝ、ゞ）续下去，那是另一种观感，也是另一种宽度；列表真长到
/// 48 项的书我们没见过，宁可少一个标记，也不要凭猜的记数把目录排歪。
fn kana(index: usize, table: &[char], katakana: bool) -> Option<String> {
    let Some(&one) = table.get(index - 1) else {
        tracing::debug!(
            "marker: 第 {index} 项超出假名表（{} 格），这一格不画",
            table.len()
        );
        return None;
    };
    let text = if katakana {
        to_katakana(one).to_string()
    } else {
        one.to_string()
    };
    Some(format!("{text}、"))
}

/// 五十音（46 格，CSS 的 `hiragana` 序列：不含古假名 ゐ / ゑ）。
const GOJÜON: [char; 46] = [
    'あ', 'い', 'う', 'え', 'お', 'か', 'き', 'く', 'け', 'こ', 'さ', 'し', 'す', 'せ', 'そ', 'た',
    'ち', 'つ', 'て', 'と', 'な', 'に', 'ぬ', 'ね', 'の', 'は', 'ひ', 'ふ', 'へ', 'ほ', 'ま', 'み',
    'む', 'め', 'も', 'や', 'ゆ', 'よ', 'ら', 'り', 'る', 'れ', 'ろ', 'わ', 'を', 'ん',
];

/// いろは序（47 格，含 ゐ / ゑ —— 那两句古假名本身就是这首诗的一部分）。
const IROHA: [char; 47] = [
    'い', 'ろ', 'は', 'に', 'ほ', 'へ', 'と', 'ち', 'り', 'ぬ', 'る', 'を', 'わ', 'か', 'よ', 'た',
    'れ', 'そ', 'つ', 'ね', 'な', 'ら', 'む', 'う', 'ゐ', 'の', 'お', 'く', 'や', 'ま', 'け', 'ふ',
    'こ', 'え', 'て', 'あ', 'さ', 'き', 'ゆ', 'め', 'み', 'し', 'ゑ', 'ひ', 'も', 'せ', 'す',
];

/// 平假名 → 片假名：两张表只差 0x60（U+3041–U+3096 与 U+30A1–U+30F6 同序），
/// 而且表里全是假名音节、没有 `ゝ`/`ゞ` 这类标点，所以直接加偏移就够，不用查表。
fn to_katakana(c: char) -> char {
    char::from_u32(c as u32 + 0x60).unwrap_or(c)
}

#[cfg(test)]
mod tests {
    //! 记数本身：四种拉丁 / 罗马 / 汉字 / 假名的写法，以及表外与 `none` 那两条边界。
    use super::*;
    use ListStyle::*;

    #[test]
    fn bullets_are_single_characters() {
        assert_eq!(marker_text(Disc, 1).as_deref(), Some("•"));
        assert_eq!(marker_text(Circle, 7).as_deref(), Some("◦"));
        assert_eq!(marker_text(Square, 99).as_deref(), Some("▪"));
        assert!(
            marker_text(None, 1).is_none(),
            "`list-style-type: none` 一个字符都不画"
        );
    }

    #[test]
    fn decimal_and_alphabetic_carry_a_period() {
        assert_eq!(marker_text(Decimal, 3).as_deref(), Some("3."));
        assert_eq!(marker_text(LowerAlpha, 1).as_deref(), Some("a."));
        assert_eq!(marker_text(LowerAlpha, 26).as_deref(), Some("z."));
        // 双射记数：27 是 `aa` 而不是 `ba`，52 是 `az`。
        assert_eq!(marker_text(LowerAlpha, 27).as_deref(), Some("aa."));
        assert_eq!(marker_text(LowerAlpha, 52).as_deref(), Some("az."));
        assert_eq!(marker_text(UpperAlpha, 2).as_deref(), Some("B."));
    }

    #[test]
    fn roman_uses_the_standard_subtractive_forms() {
        for (index, want) in [
            (1, "i."),
            (4, "iv."),
            (9, "ix."),
            (14, "xiv."),
            (40, "xl."),
            (90, "xc."),
            (400, "cd."),
            (1990, "mcmxc."),
        ] {
            assert_eq!(
                marker_text(LowerRoman, index).as_deref(),
                Some(want),
                "第 {index} 项"
            );
        }
        assert_eq!(marker_text(UpperRoman, 4).as_deref(), Some("IV."));
    }

    #[test]
    fn chinese_numerals_follow_the_reading_rules() {
        for (index, want) in [
            (1, "一、"),
            (10, "十、"),
            (11, "十一、"),
            (20, "二十、"),
            (100, "一百、"),
            (105, "一百零五、"),
            (110, "一百一十、"),
            (1000, "一千、"),
            (1001, "一千零一、"),
            (1010, "一千零一十、"),
            (9999, "九千九百九十九、"),
        ] {
            assert_eq!(
                marker_text(CjkIdeographic, index).as_deref(),
                Some(want),
                "第 {index} 项"
            );
        }
        // 表外退回阿拉伯数字：看得见的降级，比凭空造一个读法好核对。
        assert_eq!(
            marker_text(CjkIdeographic, 10000).as_deref(),
            Some("10000、")
        );
    }

    #[test]
    fn kana_indexes_run_through_the_two_orders() {
        assert_eq!(marker_text(Hiragana, 1).as_deref(), Some("あ、"));
        assert_eq!(marker_text(Hiragana, 2).as_deref(), Some("い、"));
        assert_eq!(marker_text(Katakana, 1).as_deref(), Some("ア、"));
        assert_eq!(marker_text(HiraganaIroha, 2).as_deref(), Some("ろ、"));
        assert_eq!(marker_text(KatakanaIroha, 2).as_deref(), Some("ロ、"));
        // 五十音最后一格是 ん（46），第 47 项在表外 → 不画。
        assert_eq!(marker_text(Hiragana, 46).as_deref(), Some("ん、"));
        assert!(marker_text(Hiragana, 47).is_none());
        assert_eq!(marker_text(HiraganaIroha, 47).as_deref(), Some("す、"));
    }

    #[test]
    fn the_kana_tables_are_the_css_sequences() {
        // 表长与序列末尾都是 CSS 记数那两份表写死的东西，漂一格就是整本目录错位。
        assert_eq!(GOJÜON.len(), 46);
        assert_eq!(IROHA.len(), 47);
        assert_eq!(GOJÜON.last(), Some(&'ん'));
        assert_eq!(IROHA.first(), Some(&'い'));
        assert_eq!(IROHA.last(), Some(&'す'));
        // 五十音里没有古假名，いろは序有 —— 这是两张表唯一实质的差别。
        assert!(!GOJÜON.contains(&'ゐ') && IROHA.contains(&'ゐ'));
    }

    #[test]
    fn ordinal_zero_is_read_as_the_first_item() {
        // `<ol start="0">` 是合法的 HTML，读起来仍该是「第一项」而不是空标记。
        assert_eq!(marker_text(Decimal, 0).as_deref(), Some("1."));
    }
}
