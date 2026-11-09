//! 容器内路径与 CSS `url(…)` 的解析。
//!
//! 同一句话有三种写法位置 —— 外链样式表、章内 `<style>`、元素上的 `style="…"` ——
//! 必须折成同一个形状；这里只留一份实现。

/// 把 `href` 相对 `base_dir`（通常是 OPF 或某章所在的目录）解析成容器内的规范路径。
/// 去掉 `#fragment`，做百分号解码，折叠 `./` 与 `../`。
///
/// 这是 epub 适配器与 app 侧图片 resolver 共用的同一份实现 —— 两边各写一次
/// 迟早会在「文件名里有空格 / 百分号编码」这类书上分叉。
pub fn resolve_href(base_dir: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href).trim();
    let decoded = percent_decode(href);
    let merged = if decoded.starts_with('/') {
        decoded.trim_start_matches('/').to_string()
    } else if base_dir.is_empty() {
        decoded
    } else {
        format!("{base_dir}{decoded}")
    };
    let mut stack: Vec<&str> = Vec::new();
    for seg in merged.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    stack.join("/")
}

/// 取 `href` 里 `#` 后面那一截并百分号解码；没有 `#`、截出来是空的、或根本不是元素 id
/// 的都是 `None`（= 跳章首）。
///
/// 必须在 [`resolve_href`] 之前取：那条的第一件事就是把 `#` 整条剥掉。解码也走同一份
/// 实现 —— 两处口径不一致的话，锚点里带百分号编码的书会在「路径解开了、锚点解不开」
/// 这种最难查的地方分叉。
///
/// 只认 XHTML 的 `id`。`epubcfi(...)` 是另一套定位语法、OEB 17 的 `$` 是历史遗留，
/// 这里都不解析：认不出时读者的表现是落到章首，而不是跳到一个错的位置上。
pub fn fragment_of(href: &str) -> Option<String> {
    // fragment 是**第一个** `#` 之后的全部内容（它自己也可以含 `#`）。
    let raw = href.split_once('#')?.1.trim();
    if raw.is_empty() || raw.starts_with("epubcfi(") {
        return None;
    }
    // `#a,xpointer(...)` 这种复合片段里只有第一段是元素 id。
    let id = raw.split(',').next().unwrap_or(raw);
    (!id.is_empty()).then(|| percent_decode(id))
}

/// 路径所在目录，带结尾 `/`；没有目录时是空串。
pub fn dir_of(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => String::new(),
    }
}

/// 一条作者写下的资源引用（`<img src>`、`<link href>`）折成容器内的相对路径候选。
/// 空串 = 这一条**不该去取**（空值、`data:`、带协议的外链）。
///
/// 三个容器（单文件 `html`、`htmlz`、`chm`）共用这一份是必须的：前两个从前各抄一遍
/// （逐字相同），CHM 认 `<link rel=stylesheet>` 时要再抄第三遍，「哪种写法算外链」就会
/// 在三个容器里长得不一样 —— 而它的症状是某一本书「样式表整包看不见」，屏上没有线索。
/// 真正把路径解析掉（按本页目录合并、折 `..`）留给 [`resolve_href`]。
pub fn asset_href(src: &str) -> String {
    let src = src.trim();
    if src.is_empty() || src.starts_with("data:") || src.contains("://") {
        return String::new();
    }
    let cut = src.split(['?', '#']).next().unwrap_or(src);
    percent_decode(cut)
}

pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(n) = u8::from_str_radix(&input[i + 1..i + 3], 16)
        {
            out.push(n);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    // href 按 UTF-8 百分号编码；解出来的字节流不一定落在字符边界上，
    // 那种情况保留替换字符而不是整本读失败。
    String::from_utf8_lossy(&out).into_owned()
}

/// 把一份 CSS 里的 `url(…)` 按**那份 CSS 自己的目录**解成容器绝对路径。
///
/// 正文出库之后样式表按清单回容器现取（见 `Document::stylesheet`），
/// 每一份都要在拼接前进这一步 —— 拼完就再也不知道某条 `url(Fonts/a.otf)` 原本写在
/// `OEBPS/Styles/` 还是 `OEBPS/` 下面。而 `@font-face` 的 `src` 要拿去查容器里的
/// 条目 —— 差一层目录的表现是「书里明明有字体，就是读不到」，且屏幕上看不出原因。
///
/// 输出**一律带引号**：解出来的路径常含空格（真书里的 `body italic.otf`），
/// 不加引号那是非法 CSS，解析器会在空格处断开。
///
/// **公开是刻意的**：同一句话有三种写法位置 —— 外链样式表、章内那块 `<style>`、
/// 元素上的 `style="…"`。三者必须折成**同一个形状**，否则图片索引与字体表按容器路径
/// 精确查时，只有一路查得到（哈利波特的七张册扉页正是断在第三种上：
/// `<body style="background: url(../Images/v2.jpg)">`）。既然只允许一个答案，
/// 就只允许一个函数，宿主与引擎各折一遍迟早会漂。
pub fn absolutize_css_urls(css: &str, base_dir: &str) -> String {
    // 先清注释：注释里的 `url(x)` 不是声明，改写它等于往 CSS 里塞假路径。
    let text = crate::css::strip_comments(css);
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let Some(start) = find_url_function(rest) else {
            out.push_str(rest);
            return out;
        };
        // `rest[start..start + 4]` 就是 `url(`，`open` 是它之后第一位。
        let open = start + 4;
        let Some(close) = find_close_paren(&rest[open..]) else {
            // 括号不闭合：剩下的原样抄走，不猜。
            out.push_str(rest);
            return out;
        };
        let token_end = open + close + 1; // 整个 `url(…)` 函数 token 的结束（不含）
        let inner = rest[open..open + close]
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .trim();
        // `url(` 之前的部分先抄走；`url(` 本身**不能**跟着抄，否则改写会变成
        // `url(url("解出来的路径")` —— 括号一下就不配对了。
        out.push_str(&rest[..start]);
        if inner.is_empty() || crate::css::is_remote(inner) {
            // 远程与空的都原样留着：一期不取外部字体，也不假装取过。
            out.push_str(&rest[start..token_end]);
        } else {
            out.push_str("url(\"");
            out.push_str(&resolve_href(base_dir, inner));
            out.push_str("\")");
        }
        rest = &rest[token_end..];
    }
    out
}

/// 下一个 `url(` 函数 token 的起始字节偏移。
///
/// 要判前一个字符：`dataurl(` / `my-url(` 里都含 `url(`，按子串找会把它们当函数改写。
fn find_url_function(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i + 4 <= bytes.len() {
        if bytes[i..i + 4].eq_ignore_ascii_case(b"url(")
            && (i == 0 || !is_function_char(bytes[i - 1]))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 能构成一个标识符的字符：命中的话它前面那串就不是独立的 `url` 函数名。
fn is_function_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b >= 0x80
}

/// 从 `text` 开头找到配对 `)` 的位置（不含）。引号里的括号跳过。
fn find_close_paren(text: &str) -> Option<usize> {
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
