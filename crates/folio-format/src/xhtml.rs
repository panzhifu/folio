//! 纯文本 → 引擎就绪的 XHTML —— **格式差异的最后一站**。
//!
//! 排版引擎（`crate::layout`）只认一件事：输入是 XHTML 片段（DOM）。所以每个
//! provider 有一份契约：**`Chapter::source` 永远是引擎能直接吃的 XHTML**。
//! EPUB / FB2 / HTML 的章源本来就是 XHTML，原样透传；Markdown / TXT 的章源是
//! 规范化后的**纯文本**，由这里补一次最小包装。
//!
//! 这一步在 provider 里做，不在界面里做 —— 判据只有一条：*「新增一个容器格式」
//! 时该改哪里*。放在界面里，界面就得知道「哪个格式的章源是纯文本」，
//! 于是 `Format` 渗进排版链，引擎的输入契约也就不再是单一的。放在这里之后，
//! 界面拿到的 `Chapter::source` 一律可以直接 `Dom::parse`。
//!
//! 包装只做四件事，**不改动一个可见字符**（段落顺序、字符内容、锚点坐标都不变）：
//!
//! - 空行分段 → 每段一个 `<p>`；
//! - 段内换行 → `<br/>`（`white-space: normal` 下它仍是硬换行）；
//! - 1–6 个 `#` 加空格开头的行 → `<hN>`（该行独占一个块）；
//! - 文字全部 XML 转义 —— 否则正文里的 `<script>` 会被 html5ever 当成真标签，
//!   读者就少一段，而屏幕上完全看不出为什么。

/// 规范化纯文本 → 引擎就绪的 XHTML 片段。
///
/// 输入是 [`super::markdown`] 的 `normalize()` 产物：行尾统一、空行压到最多一个。
/// 输出没有 `<body>`：排版层自己找根（见 `ui::reader::document::body_of`）。
pub fn from_plain_text(plain: &str) -> String {
    let mut out = String::with_capacity(plain.len() + 64);
    for block in plain.split("\n\n") {
        let lines: Vec<&str> = block.lines().collect();
        if lines.iter().all(|line| line.trim().is_empty()) {
            continue;
        }
        // 1–6 个 `#` 加空格才是 ATX 标题，其余一律当正文。
        if let Some((level, text)) = lines
            .first()
            .and_then(|first| atx_heading(first.trim_start()))
        {
            // 标题独占一行：整块只有它是标题，正文留给后面的段。
            out.push_str(&format!("<h{level}>"));
            escape(&mut out, text);
            out.push_str(&format!("</h{level}>"));
            let rest = &block[block.find('\n').map(|i| i + 1).unwrap_or(block.len())..];
            if !rest.trim().is_empty() {
                out.push('\n');
                paragraph(&mut out, rest);
            }
            out.push('\n');
            continue;
        }
        paragraph(&mut out, block);
        out.push('\n');
    }
    out
}

/// 一段正文：内部换行保留成 `<br/>`（`white-space: normal` 下 `\n` 仍是硬换行，
/// 见 `crate::layout` 的 `hard_lines`）。
fn paragraph(out: &mut String, text: &str) {
    out.push_str("<p>");
    for (ix, line) in text.lines().enumerate() {
        if ix > 0 {
            out.push_str("<br/>");
        }
        escape(out, line.trim());
    }
    out.push_str("</p>");
}

fn atx_heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim();
    (!text.is_empty()).then_some((hashes as u8, text))
}

fn escape(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

/// **测试用**：源文本里 ATX 标题的标记去掉之后的样子。
///
/// `from_plain_text` 唯一允许「少」字符的地方就是标题的 `#`（以及每行首尾的空白），
/// 所以契约测试拿它当期望值：可见文本只允许在这些位置上变短，其余一个字符都不能丢。
#[cfg(test)]
pub(crate) fn atx_markers_removed(source: &str) -> String {
    source
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            let hashes = trimmed.chars().take_while(|c| *c == '#').count();
            if hashes > 0 && hashes <= 6 {
                let rest = &trimmed[hashes..];
                if rest.starts_with([' ', '\t']) {
                    return rest.trim().trim_end_matches('#').trim().to_string();
                }
            }
            trimmed.to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    //! 包装的四条规矩，以及「一个可见字符都不改」这条不变量。

    use super::from_plain_text;

    /// 空行分段、段内换行成硬换行、ATX 标题成 `<hN>`、特殊字符转义 —— 少任何一条，
    /// 读 `.md` / `.txt` 就会把整篇排成一个段落，或把尖括号当标签吃掉。
    #[test]
    fn blocks_headings_and_escapes() {
        let xhtml = from_plain_text("# 标题\n\n第一段\n还是第一段\n\n第二段\n\n<script>x</script>");
        assert!(
            xhtml.contains("<h1>标题</h1>"),
            "ATX 标题要成 `<h1>`：{xhtml}"
        );
        assert!(
            xhtml.contains("<p>第一段<br/>还是第一段</p>"),
            "段内换行要成硬换行：{xhtml}"
        );
        assert!(xhtml.contains("<p>第二段</p>"), "空行分段：{xhtml}");
        // 转义后不能剩下裸的 `<script>`，否则 html5ever 会真当标签解析，正文就少一段。
        assert!(
            !xhtml.contains("<script>"),
            "正文里的尖括号必须转义：{xhtml}"
        );
        assert!(xhtml.contains("&lt;script&gt;"));
    }

    /// 标题行独占块：标题后面的正文要留在同一个块里，且标题行本身不能被当成正文再包一次。
    #[test]
    fn a_heading_takes_the_first_line_and_the_rest_stays_a_paragraph() {
        let xhtml = from_plain_text("## 章名\n正文一\n正文二\n");
        assert_eq!(xhtml, "<h2>章名</h2>\n<p>正文一<br/>正文二</p>\n");
    }

    /// 七个 `#` 不是标题；`#` 后面没有空格也不是。两条都会落到 `<p>` 里。
    #[test]
    fn not_every_line_starting_with_a_hash_is_a_heading() {
        let xhtml = from_plain_text("####### 太深\n#没有空格\n");
        assert!(xhtml.starts_with("<p>"), "{xhtml}");
        assert!(!xhtml.contains("<h"), "{xhtml}");
    }

    /// 空白块整体丢掉，不产生空 `<p>`。
    #[test]
    fn blank_blocks_produce_nothing() {
        assert_eq!(from_plain_text("\n\n \n\n"), "");
    }
}
