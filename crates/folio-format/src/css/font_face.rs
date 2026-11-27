//! `@font-face` 的声明类型：只存声明，不存字节。
//!
//! 从 `css::mod` 切出来只因为 mod.rs 贴着行数上限；对外路径经 `pub use` 保持不变。

use super::{parse_declaration, split_top_level, value};

/// `@font-face` 的一条声明。**只存声明，不存字节** —— 字节由 `format` 那层按
/// `src` 里的容器路径去读，CSS 这层不该知道 ZIP 的存在。
#[derive(Debug, Clone, PartialEq)]
pub struct FontFaceRule {
    /// `font-family` 描述符，已去引号。一条 `@font-face` 写了逗号列表时**每个名字一条**。
    pub family: String,
    /// `src`，按书写顺序，第一个是首选。
    pub src: Vec<FontSrc>,
    /// `font-weight`。写了区间（`400 700`）取第一个值。缺省 400。
    pub weight: u16,
    /// `font-style`：`italic` / `oblique` 为 `Some(true)`，`normal` 为 `Some(false)`，
    /// **没写是 `None`**，不等于 `normal` —— 没写时书的意图是「这套字体的常规面」，
    /// 而注册进来的内部名自己带面信息。
    pub italic: Option<bool>,
}

/// `src` 描述符里的一项。`format()` / `tech()` 这些修饰下一个来源的函数认得但不影响
/// 来源本身，解析时跳过。
#[derive(Debug, Clone, PartialEq)]
pub enum FontSrc {
    /// 容器内的规范路径。EPUB 的 CSS 在拼接前把 `url(../Fonts/a.otf)` 改写成了绝对路径
    /// （见 `format::epub`），所以这里的串可以直接当查表键。
    Url(String),
    /// 设备上已装的字体名：只查注册名单，不注册字节。
    Local(String),
    /// 设备上的字体文件（`res://…` / `file://…` / `/system/fonts/…`）。
    ///
    /// 字节不随书来，也读不到：只拿文件名去设备已注册的家族里找同一张脸
    /// （见 `BookFonts::for_document`）。http(s) 之类的来源仍然整条丢掉。
    System(String),
}

use super::is_remote;

/// `@font-face` 的块体 → face 清单。
///
/// `font-family` 写逗号列表时**每个名字出一条**（CSS 就是这个语义：同一份字体数据
/// 挂了两个别名），因为下游是按声明名查表的。缺 `font-family` 的整块作废：那样一条
/// 谁也引用不到，留着只会让日志里的「这本书有字体」变成假象。
/// 认不出的声明计 `ignored`，与 `font` 简写同一套纪律；`unicode-range` /
/// `font-stretch` / 三个 `*-override` 认得但一期用不上，不计数。
pub(super) fn parse_font_face(body: &str) -> (Vec<FontFaceRule>, usize) {
    let mut family = String::new();
    let mut src: Vec<FontSrc> = Vec::new();
    let mut weight = 400u16;
    let mut italic = None;
    let mut ignored = 0usize;

    for decl in split_top_level(body, ';') {
        let Some(d) = parse_declaration(&decl) else {
            ignored += 1;
            continue;
        };
        match d.property.as_str() {
            // 原样留着：逗号列表要**先切再去引号**，先去引号会把 `"A", "B"` 削成 `A", "B`。
            "font-family" => family = d.value.clone(),
            "src" => src = parse_font_src(&d.value),
            "font-weight" => {
                // 区间写法 `400 700` 取第一个值：一期一个 face 只匹配一个字重，
                // 认区间要的是「同一声明名下的多份数据怎么挑」那套算法。
                let head = d.value.split_whitespace().next().unwrap_or("");
                match value::parse_font_weight(head) {
                    Some(w) => weight = w,
                    None => ignored += 1,
                }
            }
            "font-style" => {
                italic = match d.value.to_ascii_lowercase().as_str() {
                    "normal" => Some(false),
                    "italic" | "oblique" => Some(true),
                    _ => None,
                };
                if italic.is_none() {
                    ignored += 1;
                }
            }
            "font-stretch"
            | "font-variant"
            | "font-feature-settings"
            | "unicode-range"
            | "ascent-override"
            | "descent-override"
            | "line-gap-override" => {}
            _ => ignored += 1,
        }
    }
    // 认不出来的写法只是少一条线索，不该让整块作废 —— 除了缺 font-family，
    // 那条让这块声明无处可查。
    if family.is_empty() {
        return (Vec::new(), ignored);
    }
    let faces = split_top_level(&family, ',')
        .into_iter()
        .map(|name| unquote(&name))
        .filter(|name| !name.is_empty())
        .map(|name| FontFaceRule {
            family: name,
            src: src.clone(),
            weight,
            italic,
        })
        .collect::<Vec<_>>();
    (faces, ignored)
}

/// `src: url(a.otf) format("opentype"), local("Book Serif")` → 来源列表。
///
/// `format()` / `tech()` 是修饰**前一个**来源的，认得就跳过。`url()` 分三档：
/// - 容器相对路径 → [`FontSrc::Url`]，下游按容器查字节；
/// - `res://` / `file://` / `/system/…` → [`FontSrc::System`]，按文件名映射到设备字体；
/// - http(s)、`//host/…`、`data:` 之类的远程来源整条丢掉（不取，也不假装取了）。
///
/// 引号与空格都清掉，因为下游拿这个串直接当路径键。
fn parse_font_src(value: &str) -> Vec<FontSrc> {
    let mut out = Vec::new();
    for part in split_top_level(value, ',') {
        let Some((head, rest)) = split_function(&part) else {
            continue;
        };
        let inner = rest.trim().trim_matches(|c| c == '"' || c == '\'');
        match head.to_ascii_lowercase().as_str() {
            "url" => {
                let lowered = inner.to_ascii_lowercase();
                let remote = is_remote(inner);
                if lowered.starts_with("res:") || lowered.starts_with("file:") {
                    out.push(FontSrc::System(inner.to_string()));
                } else if !remote && inner.starts_with('/') {
                    // 绝对设备路径（`/system/fonts/…`）：容器里不可能有这个名字。
                    // `//host/…` 也是 `is_remote`，排在上一档之外，仍然丢掉。
                    out.push(FontSrc::System(inner.to_string()));
                } else if !remote {
                    out.push(FontSrc::Url(inner.to_string()));
                }
            }
            "local" => out.push(FontSrc::Local(unquote(inner))),
            // format / tech 与其余函数：修饰或暂不支持，不动已收到的来源。
            _ => {}
        }
    }
    out
}

/// `url(...)` / `local(...)` 这类函数 token → `(函数名, 括号内)`。
fn split_function(text: &str) -> Option<(String, &str)> {
    let open = text.find('(')?;
    let name = text[..open].trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphabetic() || c == '-') {
        return None;
    }
    let body = &text[open + 1..];
    let close = matching_paren(body)?;
    Some((name.to_string(), &body[..close]))
}

/// 返回第一个配对 `)` 的结束偏移（不含）。括号里只可能有引号包裹的路径，
/// 所以按引号跳过就行，不必再数一层嵌套。
fn matching_paren(text: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            ')' => return Some(i),
            _ => {}
        }
    }
    None
}

fn unquote(text: &str) -> String {
    text.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}
