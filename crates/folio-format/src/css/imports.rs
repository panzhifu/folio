//! `@import` 的文本级展开：进规则解析之前，把被引样式表的正文内联进来。
//!
//! 口径见 [`expand_css_imports`]：路径按引用者目录折、被引文件的 `url()` 按它自己的
//! 目录折、环与深度封顶、远程与读不到的语句原样留下让解析器计 ignored。

use super::{is_remote, strip_comments};

/// 展开 `@import` 并把 `url()` 折成容器路径：输入一份样式表的原文，输出可直接
/// 进 `RuleSet::parse` 的文本 —— 调用点**不要再过一遍** `absolutize_css_urls`，
/// 否则已折好的路径会被再拼一层。
///
/// 出版书用 `@import` 挂第二份样式表（哈利·波特全集的
/// `@import url("OEBPS/Styles/fonts.css")` 就是这么进的），解析器对它计 ignored，
/// 等于这本书带的那份样式静默丢失 —— 所以展开发生在**文本级、解析之前**：每条语句
/// 按**引用者**的目录折成容器路径，字节由调用方给的读取闭包现取（容器先试、provider
/// 兜底的口径留在调用点）。被引文件递归走同一条路：它的 `url()` 与内层 `@import`
/// 都按**它自己的**目录折。
///
/// 媒体条件（`@import url(x) screen;` 的尾部）收下但不判 —— 与 `@supports` 同一口径，
/// 判不了按成立处理。远程目标原样留下：一期不取外部样式，也不假装取过。环与深度封顶：
/// `seen` 记展开过的容器路径，见过一次不再展开；深度超过 3 同样停。读不到的文件语句
/// 原样留下（路径仍折好），让解析器照旧计 ignored —— css-gap 那张表还能看见它，
/// 而不是静默消失。注释与 `absolutize_css_urls` 同一条：清掉，不进输出。
pub(crate) fn expand_css_imports(
    css: &str,
    base_dir: &str,
    depth: u8,
    seen: &mut std::collections::HashSet<String>,
    read: &mut dyn FnMut(&str) -> Option<String>,
) -> String {
    const MAX_IMPORT_DEPTH: u8 = 3;

    let text = strip_comments(css);
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0usize;
    while let Some(at) = find_ascii_token(&lower[pos..], "@import") {
        let at = pos + at;
        // `@import` 之前的段落属于**本文件**：url() 按本文件目录折。
        out.push_str(&crate::href::absolutize_css_urls(&text[pos..at], base_dir));
        // 语句体到第一个分号为止；没有分号就整段抄走，不猜。
        let Some(semi) = lower[at..].find(';') else {
            break;
        };
        let semi = at + semi;
        let statement = &text[at..semi];
        pos = semi + 1;
        // 目标拼不出来（远程、空串）：语句留下，路径照样折好。
        let Some(raw) = import_target(statement) else {
            out.push_str(&crate::href::absolutize_css_urls(statement, base_dir));
            out.push(';');
            continue;
        };
        let path = crate::href::resolve_href(base_dir, &raw);
        if depth >= MAX_IMPORT_DEPTH || !seen.insert(path.clone()) {
            // 环与深度封顶：语句换成空 —— 这份内容已从别的路径进来过。
            continue;
        }
        let Some(imported) = read(&path) else {
            // 读不到：语句留下，交给解析器计 ignored。
            seen.remove(&path);
            out.push_str(&crate::href::absolutize_css_urls(statement, base_dir));
            out.push(';');
            continue;
        };
        // 被引文件递归走同一条路：返回的文本 url() 已按它自己的目录折好。
        let dir = crate::href::dir_of(&path);
        out.push_str(&expand_css_imports(&imported, &dir, depth + 1, seen, read));
        out.push('\n');
    }
    out.push_str(&crate::href::absolutize_css_urls(&text[pos..], base_dir));
    out
}

/// 在 `text`（大小写已折好的副本）里找 ASCII 关键字的下一个位置。
///
/// `@importx` 不算 `@import`：关键字后面必须跟空白或分号。
fn find_ascii_token(text: &str, token: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let token = token.as_bytes();
    let mut i = 0;
    while i + token.len() <= bytes.len() {
        if &bytes[i..i + token.len()] == token
            && bytes
                .get(i + token.len())
                .is_none_or(|b| b.is_ascii_whitespace() || *b == b';')
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 一条 `@import` 语句体 → 目标 URL 原文（未折路径）。
///
/// 收字符串形式（`@import "x.css";`）与函数形式（`@import url(x.css);`），引号剥掉；
/// 远程与空串给 `None`（调用方原样留下语句）。路径折叠留在调用方：`@import` 按
/// **引用者**的目录折，与 `url()` 的口径一致。
fn import_target(statement: &str) -> Option<String> {
    let body = statement.trim_start().strip_prefix("@import")?.trim_start();
    let target = if let Some(rest) = body
        .strip_prefix("url(")
        .or_else(|| body.strip_prefix("URL("))
    {
        let close = rest.rfind(')')?;
        rest[..close]
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .trim()
    } else if matches!(body.as_bytes().first(), Some(b'"' | b'\'')) {
        let quote = body.as_bytes()[0] as char;
        let end = body[1..].find(quote)? + 1;
        &body[1..end]
    } else {
        return None;
    };
    if target.is_empty() || is_remote(target) {
        return None;
    }
    Some(target.to_string())
}

/// EPUB 清单样式的一份：读原文、按它自己的目录展开 `@import`。防环的 `seen` 一份
/// 清单用到底；大小写经 `locate` 归一，与样式表自己进 `css_hrefs` 的口径一致。
/// 展开返回的文本 `url()` 已折好，调用点不再过 `absolutize`。
pub(crate) fn expand_manifest_entry(
    zip: &mut crate::zip::Zip,
    found: &str,
    seen: &mut std::collections::HashSet<String>,
) -> Option<String> {
    let text = zip.read_text(found)?;
    let dir = crate::dir_of(found);
    Some(expand_css_imports(&text, &dir, 0, seen, &mut |p| {
        let name = zip.locate(p)?;
        zip.read_text(&name)
    }))
}
