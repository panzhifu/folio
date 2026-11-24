//! 文本层的字符变换：`text-transform` 与 `font-variant: small-caps` 的 uppercase 近似。
//!
//! 两者都只在**字节长度与字符数都不变**时真的换字符 —— 锚点/选区/进度建立在源文本的
//! 字节偏移上，换长度会让它们漂。从 `wrap` 切出来只因为 wrap.rs 贴着行数上限。

use folio_format::css::computed::TextTransform;

/// `text-transform`：只返回**字节长度且字符数都不变**的结果，否则返回 `None`
/// （调用方保留原文）。`capitalize` 按空白切词，只大写首字符（标点打头的词算近似）。
pub(super) fn transform_text(src: &str, transform: TextTransform) -> Option<String> {
    let out = match transform {
        TextTransform::None => return None,
        TextTransform::Uppercase => src.to_uppercase(),
        TextTransform::Lowercase => src.to_lowercase(),
        TextTransform::Capitalize => {
            let mut out = String::with_capacity(src.len());
            let mut start_of_word = true;
            for ch in src.chars() {
                if ch.is_whitespace() {
                    start_of_word = true;
                    out.push(ch);
                } else if start_of_word {
                    out.extend(ch.to_uppercase());
                    start_of_word = false;
                } else {
                    out.push(ch);
                }
            }
            out
        }
    };
    (out.len() == src.len() && out.chars().count() == src.chars().count() && out != src)
        .then_some(out)
}

/// 把小写换成大写，**只做等字节长度的映射**。
///
/// 偏移（`norm` / `orig`）是按字节走的：`ß → SS`、`ﬁ → FI` 这种长度会变，换了之后
/// 选区与锚点全都会漂。那些字符原样留着，渲染上少一个字母的大写，比整段错位轻得多。
pub(super) fn small_caps_upper(src: &str) -> Option<String> {
    let mut out = String::with_capacity(src.len());
    let mut changed = false;
    for ch in src.chars() {
        if ch.is_lowercase() {
            let mut upper = ch.to_uppercase();
            if let (Some(first), None) = (upper.next(), upper.next())
                && first.len_utf8() == ch.len_utf8()
            {
                out.push(first);
                changed = true;
                continue;
            }
        }
        out.push(ch);
    }
    changed.then_some(out)
}
