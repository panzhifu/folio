//! 书名 → 文件名里那一段干。
//!
//! 住在地基这一层是因为它只依赖标准库而用的人不止一处：划词分享卡把书名拼成
//! `<干>-quote.png`，书架的「分享这本书」把书名拼成 `<干>.<格式后缀>`。两处递出去的
//! 都是 Android 那侧 `cache/share/` 里一个**真实文件**的名字，判据是同一条，所以过滤
//! 只有这一份。
//!
//! 为什么必须在交出去之前就过滤：收这一串的那一侧是 `new File(shareDir, name)`，它不
//! 折叠 `..` 也不拒绝分隔符 —— 而书名读自书自己的元数据（`dc:title`），里面出现什么
//! 都有可能。指望「反正落在 shareDir 下面」是不成立的。

/// 书名 → 文件名干：只留字母与数字，其余（空格、标点、斜杠、引号）一律丢掉，截到 40 个
/// 字符；一个字符都不剩时给 `folio`，不然拼出来的名字就只剩后缀那一截。
///
/// 汉字属于 `is_alphanumeric`，所以「未来简史」整名留下；截的是**字符数**而不是字节数，
/// 不会把一个多字节字符切一半。后缀不归这里，由调用方按各自要交出去的东西拼。
pub fn title_stem(title: &str) -> String {
    let stem: String = title
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(40)
        .collect();
    if stem.is_empty() {
        "folio".into()
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use super::title_stem;

    /// 路径成分与标点必须在名字离开应用之前就没有：这一串会被当成一个真实路径来收，
    /// 而收它的那一侧不折叠 `..`。
    #[test]
    fn separators_punctuation_and_blanks_are_dropped() {
        assert_eq!(title_stem("a/b:c*d"), "abcd");
        assert_eq!(title_stem("../../etc/passwd"), "etcpasswd");
        assert_eq!(title_stem("三体: 地球往事"), "三体地球往事");
        assert_eq!(title_stem("   .\t "), "folio", "一个字符都不剩时兜底");
    }

    /// 上限按字符数算：一本俄文书的书名轻松过百字符，而文件系统那一头的名字长度是有限的。
    #[test]
    fn a_long_title_is_cut_at_40_characters_not_40_bytes() {
        assert_eq!(title_stem(&"书".repeat(80)).chars().count(), 40);
        assert_eq!(title_stem(&"书".repeat(80)), "书".repeat(40));
    }
}
