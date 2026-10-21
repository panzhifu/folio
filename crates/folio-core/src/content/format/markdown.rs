//! Markdown 那一族（`.md` / `.markdown`）：按 ATX 标题切章，书名取第一个一级标题，
//! 正文过一次**真的 CommonMark 解析**再交给引擎。
//!
//! 这一族比纯文本多出来的只有三处 —— 怎么切、书名从哪儿来、正文里那些标记怎么变成结构 ——
//! 其余那条链（读成文本 → 规范化 → 折成章表 → 包装）住在 [`super::text`] 且**只有一份** ——
//! 那里面 `normalize` 的产物就是锚点坐标系，各写一份迟早会漂，漂的症状是已存批注
//! 与进度整体错位。分成两个 provider 而不是一个 provider 按扩展名返回不同格式，
//! 是因为 `DocumentProvider::format()` 是**身份**：一个 provider 只能有一个 `Format`，
//! 而 `books.format` 那一列要把 Markdown 与纯文本分开。

use std::path::Path;

use markdown::{CompileOptions, Constructs, Options, ParseOptions};

use super::{Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, Format};
use crate::Result;
use crate::content::format::text::{PlainFamily, read_with, section_source};

pub struct MarkdownAdapter;

impl DocumentProvider for MarkdownAdapter {
    fn format(&self) -> Format {
        Format::Markdown
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(
            super::extension_of(path).as_deref(),
            Some("md" | "markdown")
        )
    }

    /// 只分章、**不包装**：正文等翻开那一章再取。
    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, &MARKDOWN, false)
    }

    /// 单文件容器里的第 N 章：重读一遍、重分一次章，取第 N 段。
    /// 分章规则是确定的，所以同一个序号每次都指到同一章。
    fn chapter_source(&self, path: &Path, at: &ChapterLocator) -> Result<String> {
        section_source(path, &MARKDOWN, at)
    }

    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, &MARKDOWN, true)
    }
}

/// Markdown 相对那条链的四个答案：按 1–2 级 ATX 标题切，书名取第一个一级标题，
/// 目录交空表，正文过一遍 CommonMark 再把图片换成 alt。
///
/// 目录那一格交回空表：这一族的章表**就是**结构（章名即标题），再抄一份进 `FlowDoc.toc`
/// 会多出两份「这本书有哪些章」的说法，而它们迟早要漂。
const MARKDOWN: PlainFamily = PlainFamily {
    format: Format::Markdown,
    split: split_on_headings,
    info: book_info_from_heading,
    toc: |_| Vec::new(),
    wrap: to_xhtml,
};

/// 一章的 Markdown 源文本 → 引擎就绪的 XHTML。
///
/// 解析那一大块交给 `markdown` 这个包（为什么不自写、以及「锁里已有、一条新包都不带」
/// 那组实测都写在根 `Cargo.toml` 那条上）。这里只定夺**我们这一族特有的三件事**：
///
/// **一、开哪几档构造。** 取 CommonMark 默认档再多开一条 `gfm_strikethrough`：删除线
/// 的产物是 `<del>`，而 `css::ua::seed` 给 `<del>` 写的 `line_through` 早就在了，
/// 开这一条是白拿。**GFM 表格那一档故意不开**：`css::ua::is_inline_tag` 那条规则把
/// 认不出的标签一律按 block 降级，真表格会被拆成一竖列互相不知彼此的格子字块；
/// 不开的那一档里 `| 甲 | 乙 |` 保持原样一段字，读者看得见那是没渲好的东西。
///
/// **二、作者手写的 HTML 按字面收。** `CompileOptions::default()` 就是这一档
/// （`allow_dangerous_html` 为假）：源里写 `<i> venus </i>`，屏上就是这几个字符，
/// 不是斜体。这与纯文本那一族 [`super::xhtml::from_plain_text`] 的 XML 转义同一条
/// 立场 —— 一个可见字符都不丢，屏上永远说真话。代价照认下来：混写 HTML 的技术书
/// 会露出一排字面标签，而那是可以一句话决定的另一刀（把 `allow_dangerous_html` 打开，
/// 或按标签走白名单 —— 名单能与 `css::ua::SKIPPED_TAGS` 那份共用是后一条的好处）。
///
/// **三、图片换成它的替代文字。** 单文件格式没有容器：副本就是书库根那一个
/// `<哈希前缀>.md`，`assets/` 那些兄弟文件在导入那一刻就没跟进来，provider 的
/// `image_bytes` 对这种格式恒回 `None`。留着 `<img>` 有两处会说谎：排版按 (0,0)
/// 画一块空，而 `crate::content::scan::chapter_summary` 会把这条 src 收进
/// 「整本书那一列图」，图片查看器于是凭空多一张永远点不开的图。换成 alt 文字是
/// 这一层给得起的真话；**alt 为空时整条删掉**，因为源文本里除了标记本来就没有别的字，
/// 而「渲染出来的每个字符都在源里出现过」这条不变量由 `contract.rs` 的
/// `the_markdown_wrapper_only_removes_markers` 钉着。
pub(crate) fn to_xhtml(source: &str) -> String {
    let html = markdown::to_html_with_options(source, &options());
    let html = match html {
        Ok(html) => html,
        Err(error) => {
            // 解析失败**不该让这本书打不开**（同 `format::rtf` 那条「坏一处不整本」的
            // 立场）。退回去的产物是「这一版还没做 Markdown 时的样子」：整段按纯文本包，
            // 标记原样显示，但读者看得见的字一个不少。留一句 warn 说清发生了什么。
            tracing::warn!("markdown: 解析失败（{error}），这一章按纯文本包");
            return super::xhtml::from_plain_text(source);
        }
    };
    images_to_alt(&html)
}

/// 这一族要的解析档位。每次调用现造：`Constructs` 就是三十四个布尔位，造一个不值一提，
/// 换来的是没有全局可变状态（与 [`super::provider::providers`] 同一条口径）。
fn options() -> Options {
    Options {
        parse: ParseOptions {
            constructs: Constructs {
                gfm_strikethrough: true,
                ..Constructs::default()
            },
            ..ParseOptions::default()
        },
        // 原始 HTML 转义成字面文本、`javascript:` 那类危险协议滤掉 —— 默认值就是要的那两样。
        compile: CompileOptions::default(),
    }
}

/// 把渲染产物里每一个 `<img …>` 换成它的 `alt` 值；alt 为空、或根本没有这个属性时
/// 整条删掉。
///
/// 这个扫描只动**字面**的 `<img`，而字面的 `<img` 只可能出自渲染器自己：作者手写的
/// `<img>` 在默认编译档位里被转义成 `&lt;img`，代码块里的同样以实体形式待在 `<pre>`
/// 里，两处都不是 `<img` 这四个字节。所以「扫描碰不到的东西」正是「该原样留下的东西」。
///
/// 属性是一条一条读出来的，不是 `find("alt=")`：`src="alt.png"` 那种文件名里就带着
/// `alt=` 三个字母，按子串找会把 src 的值当成 alt，结果这张图被换成一个叫 `alt.png`
/// 的字。带引号的属性值里出现的 `>` 也要跳过，否则 `alt="a>b"` 会把标签在中间截断。
fn images_to_alt(html: &str) -> String {
    if !html.contains("<img") {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(found) = rest.find("<img") {
        out.push_str(&rest[..found]);
        let tag = &rest[found..];
        // 标签结束 = 第一个不在引号里的 `>` 的**后一位**。读不出来就原样抄完剩下的，
        // 不替它猜一个结束位（宁可留一个渲不出东西的标签，也不吃掉后面的正文）。
        let Some(end) = tag_end(tag) else {
            out.push_str(tag);
            return out;
        };
        if let Some(alt) = attribute(&tag[..end], "alt") {
            // alt 的值是渲染器已经转义过的一段文本，原样递出去 —— 再转一遍会编出
            // `&amp;quot;` 那种双层的假字。
            out.push_str(&alt);
        }
        rest = &rest[found + end..];
    }
    out.push_str(rest);
    out
}

/// 这个标签到哪里结束（含收尾 `>` 之后一位）；`<img` 之后四字节就是标签名本身，跳过。
fn tag_end(tag: &str) -> Option<usize> {
    let bytes = tag.as_bytes();
    let mut quote: Option<u8> = None;
    for (index, &byte) in bytes.iter().enumerate().skip(4) {
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'>' => return Some(index + 1),
            None => {}
        }
    }
    None
}

/// 一个属性名到此为止的那些字节：`=` 带值、空白与 `/` 与 `>` 收尾、引号开值。
const ATTRIBUTE_STOP: &[u8] = b"= \t\n>/\"'";

/// 从一个切好的标签里取一个属性的值。没有这个属性、或值全是空白都回 `None`。
fn attribute(tag: &str, name: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut index = 4;
    loop {
        index = skip_spaces(bytes, index);
        if index >= bytes.len() {
            return None;
        }
        let key_start = index;
        while index < bytes.len() && !ATTRIBUTE_STOP.contains(&bytes[index]) {
            index += 1;
        }
        let key = &tag[key_start..index];
        index = skip_spaces(bytes, index);
        // 没有 `=` 跟着的是一个空属性（或收尾的 `/`），跳到下一位再看。
        if bytes.get(index) != Some(&b'=') {
            index += 1;
            continue;
        }
        index = skip_spaces(bytes, index + 1);
        let quote = bytes
            .get(index)
            .copied()
            .filter(|&q| q == b'"' || q == b'\'')?;
        let body = index + 1;
        let close = bytes[body..].iter().position(|&b| b == quote)?;
        let value = &tag[body..body + close];
        index = body + close + 1;
        if key.eq_ignore_ascii_case(name) && !value.trim().is_empty() {
            return Some(value.to_string());
        }
    }
}

fn skip_spaces(bytes: &[u8], from: usize) -> usize {
    let mut index = from;
    while matches!(bytes.get(index), Some(b' ') | Some(b'\t') | Some(b'\n')) {
        index += 1;
    }
    index
}

/// Markdown 的书目只有书名那一格（第一个一级标题），作者与简介**这族没有地方可说**：
/// 单个 `.md` 文件里既没有 OPF 也没有 `<meta>`，硬猜只会造出一条不存在的事实。
fn book_info_from_heading(
    source: &str,
    stem: &str,
) -> crate::content::format::novel::detect::BookInfo {
    crate::content::format::novel::detect::BookInfo {
        title: first_heading(source).unwrap_or_else(|| stem.to_owned()),
        authors: Vec::new(),
        description: String::new(),
    }
}

/// 在每个 1–2 级 ATX 标题前分章，标题归属于其后的内容。
fn split_on_headings(source: &str, fallback_title: &str) -> Vec<Chapter> {
    let mut chapters = Vec::new();
    let mut buf = String::new();
    let mut title: Option<String> = None;
    let mut fence = false;
    let mut saw_heading = false;

    for line in source.lines() {
        let trimmed = line.trim_start();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        let heading = if !is_fence && !fence {
            atx_heading(line, 2)
        } else {
            None
        };
        if is_fence {
            fence = !fence;
        }

        if let Some(heading_title) = heading {
            // 由标题触发的收尾：这一段没有标题，说明它位于首个标题之前，是「前言」。
            flush(&mut buf, &mut title, "前言", &mut chapters);
            title = Some(heading_title);
            saw_heading = true;
        }
        buf.push_str(line);
        buf.push('\n');
    }
    // 收尾：全文一个标题都没有时，章名回落到文件名。
    let tail_default = if saw_heading {
        "前言"
    } else {
        fallback_title
    };
    flush(&mut buf, &mut title, tail_default, &mut chapters);

    if chapters.is_empty() {
        chapters.push(Chapter::inline(fallback_title, source));
    }
    chapters
}

fn flush(
    buf: &mut String,
    title: &mut Option<String>,
    default_title: &str,
    chapters: &mut Vec<Chapter>,
) {
    let text = std::mem::take(buf);
    if text.trim().is_empty() {
        return;
    }
    chapters.push(Chapter::inline(
        title.take().unwrap_or_else(|| default_title.to_string()),
        text,
    ));
}

/// `max_level` 以内的 ATX 标题返回其文字。
fn atx_heading(line: &str, max_level: usize) -> Option<String> {
    let hashes = line.trim_start().chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > max_level {
        return None;
    }
    let rest = &line.trim_start()[hashes..];
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn first_heading(source: &str) -> Option<String> {
    let mut fence = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if !fence && let Some(h) = atx_heading(line, 1) {
            return Some(h);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    //! Markdown 适配器：分章、标题回落、代码围栏里的 `#` 不算标题。黑盒 ——
    //! 把内容写进临时 `.md`，经公开的 `open()` 读回来断言，只依赖公开 API。

    use crate::content::format::text::test_support::{opened, opened_bytes, utf16le, visible};

    /// UTF-16 这一档通到 Markdown 的正文：解出来的章表与同一份内容的 UTF-8 版
    /// **逐字相同**，包括分章、章名与 `#` 有没有被剥掉。
    #[test]
    fn a_utf16_markdown_lands_on_the_same_chapters_as_its_utf8_twin() {
        let src = "# 书名\n前言\n## 第一章\n正文一\n## 第二章\n正文二\n";
        let (utf8_meta, utf8) = opened("d.md", src);
        let (u16_meta, u16) = opened_bytes("d.md", &utf16le(src));
        assert_eq!(u16_meta.title, utf8_meta.title, "书名要一样");
        assert_eq!(
            u16.chapters
                .iter()
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>(),
            utf8.chapters
                .iter()
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>(),
            "章数与每一章的章名都该与 UTF-8 那份相同"
        );
        for (a, b) in u16.chapters.iter().zip(&utf8.chapters) {
            assert_eq!(a.source(), b.source(), "正文要逐字相同：{:?}", a.title);
        }
        // 独立验一次内容而不是只验「两趟一致」：两趟都坏也会一致。
        assert!(
            !u16.chapters[1].source().contains('\u{fffd}')
                && u16.chapters[1].source().contains("正文一"),
            "UTF-16 不该退成一排问号：{}",
            u16.chapters[1].source()
        );
    }

    /// **纯文本没有声明位，所以那一档对它故意不生效**（`format::charset` 里那条）。
    /// 一本合法 UTF-8 的技术书在前 1 KB 里出现 `charset=gbk` 那样一串字（真书里就长在
    /// 代码清单里），按 HTML 那套「文件自己说的优先」会把整本书解成 GBK —— 症状是通屏
    /// 像汉字的乱码，而判定与导入全程报「成功」。这一条走 `.md`，与 `.txt` 那条同判据。
    #[test]
    fn a_plain_file_does_not_obey_a_charset_line_inside_it() {
        let src = "```html\n<meta http-equiv=\"Content-Type\" content=\"text/html; charset=gbk\">\n```\n\n# 第一章\n这一行是合法 UTF-8 的正文：大雪封山的那一夜\n";
        let (meta, flow) = opened("note.md", src);
        assert_eq!(meta.title, "第一章", "标题要按 UTF-8 解出来，不是乱码");
        let body = flow
            .chapters
            .iter()
            .map(|c| visible(c.source()))
            .collect::<String>();
        assert!(
            body.contains("大雪封山的那一夜") && !body.contains('\u{fffd}'),
            "被声明劫持过的 UTF-8 文本会在这里露出来：{body}"
        );
    }

    #[test]
    fn splits_on_h1_and_h2() {
        let src = "# 书名\n前言\n## 第一章\n正文一\n## 第二章\n正文二\n";
        let (_, flow) = opened("d.md", src);
        assert_eq!(flow.chapters.len(), 3);
        assert_eq!(flow.chapters[0].title, "书名");
        assert_eq!(flow.chapters[1].title, "第一章");
        assert!(
            flow.chapters[1].source().starts_with("<h2>第一章</h2>"),
            "标题要包成 `<h2>`：{}",
            flow.chapters[1].source()
        );
        assert!(flow.chapters[1].source().contains("正文一"));
        assert_eq!(flow.chapters[2].title, "第二章");
    }

    #[test]
    fn h3_does_not_split() {
        let (_, flow) = opened("d.md", "# A\n### deep\nbody\n");
        assert_eq!(flow.chapters.len(), 1);
    }

    #[test]
    fn fence_content_is_not_a_heading() {
        let (_, flow) = opened("d.md", "# A\n```\n# not a heading\n```\nstill A\n");
        assert_eq!(flow.chapters.len(), 1);
        assert!(flow.chapters[0].source().contains("# not a heading"));
    }

    /// 分章不能丢字符，也不能凭空造字符，包装的口径由 `contract.rs` 的
    /// `the_markdown_wrapper_only_removes_markers` 钉；这一族自己还要钉一件包装管不着的
    /// 事：**索引那一趟数出的字数，必须等于翻开那一章之后数出的字数**。
    ///
    /// 两趟共用同一个 `wrap` 才算同一把尺。按还没包过的源文本数，就会比读者看到的多出一截
    /// （`**`、`- `、围栏的反引号都在源里而不在屏上），而多出来的那一截**逐章不均匀**：
    /// 标记密的章分母偏得更多。症状是进度条前后不单调、批注 relocate 整体偏后，
    /// 屏幕上完全看不出原因。
    #[test]
    fn the_index_count_and_the_opened_chapter_agree() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.md");
        std::fs::write(
            &path,
            "# 书名\n\n这是**重要**的一句。\n\n## 第二章\n\n- 甲项\n- 乙项\n\n```\n代码行\n```\n",
        )
        .unwrap();
        let doc = crate::content::document::open(&path).expect("该读得动");
        let flow = doc.flow().expect("该读成可重排");
        assert_eq!(flow.chapters.len(), 2, "两个一级与二级标题，两章");
        for ix in 0..flow.chapters.len() {
            let stored = flow.chapters[ix]
                .char_len
                .unwrap_or_else(|| panic!("第 {ix} 章的字数该在索引那一趟就定下来"));
            let opened = doc.chapter_source(ix).expect("正文该取得到");
            let counted = crate::content::format::visible_chars(&opened).expect("包装产物该解得开");
            assert_eq!(stored, counted, "第 {ix} 章：两趟数的必须是同一个数");
        }
    }

    /// 正文里那些标记真的变成了**结构**，不是一行字。可见文本对不对由契约那条管，
    /// 这一条管的是「排版引擎拿得到什么」：列表要有 `<li>`（`css::ua` 给它 `Display::ListItem`，
    /// 那个圆点才有地方画），引用要有 `<blockquote>`（左右那两道缩进从它来），
    /// 围栏要有 `<pre>`（`white_space_pre` 靠它，否则代码的对齐全塌）。
    #[test]
    fn markers_land_on_tags_the_engine_already_knows() {
        let (_, flow) = opened(
            "d.md",
            "# 书名\n\n这是**重要**的一句。\n\n> 引用一行\n\n- 甲项\n\n1. 第一\n2. 第二\n\n~~删~~\n\n```\ncode\n```\n",
        );
        let body = flow.chapters[0].source();
        for (want, why) in [
            (
                "<strong>重要</strong>",
                "粗体要成标签，UA 表才给得出 700 字重",
            ),
            ("<blockquote>", "引用要成块，缩进才有宿主"),
            ("<ul>\n<li>甲项</li>", "无序列表要出 `<li>`，圆点是排版画的"),
            (
                "<ol>\n<li>第一</li>",
                "有序列表同理，序号按 list-style-type 数",
            ),
            ("<del>删</del>", "删除线是这一族唯一多开的那一档 GFM"),
            (
                "<pre><code>code",
                "围栏要成 `<pre>`，否则代码里的换行与缩进会被折叠",
            ),
        ] {
            assert!(body.contains(want), "{why}：{body}");
        }
    }

    /// **代码围栏里的 `#`、`**`、`<img` 一个都不许当标记处理**。这条防的不是 CommonMark
    /// 出错，而是把这一层换成「字符串替掉标记」那种省事写法：它会把代码清单一起洗掉，
    /// 而屏上正好看不出少了几个星号。
    #[test]
    fn fenced_content_survives_verbatim() {
        let (_, flow) = opened(
            "d.md",
            "# 书名\n\n```\n# 这不是标题\nlet p = \"a > b\";\n<img src=\"x\" alt=\"y\" />\n**还不是标记**\n```\n\n结尾段\n",
        );
        let body = flow.chapters[0].source();
        assert!(
            body.contains("# 这不是标题"),
            "围栏里的井号该原样留着：{body}"
        );
        assert!(
            body.contains("**还不是标记**"),
            "围栏里的星号该原样留着：{body}"
        );
        // 那一行 `<img …>` 待在代码里时是**转义过的字节**，`images_to_alt` 找的是字面
        // `<img`，所以它碰不到这里 —— 图片那一步不该把代码清单里的示例改成一句话。
        assert!(
            body.contains("&lt;img src=&quot;x&quot; alt=&quot;y&quot; /&gt;"),
            "代码里的 img 是示例文字，不该被换成 alt：{body}"
        );
        assert!(
            body.contains("结尾段"),
            "围栏收尾之后的正文不许被吞：{body}"
        );
    }

    /// 属性是一条一条读的，不是 `find("alt=")`。这一条钉住那个区别：**文件名里就带着
    /// `alt=`** 的那张图，被换出去的必须是 alt 的文字而不是 src 的尾巴。
    #[test]
    fn an_image_named_alt_dot_png_does_not_lose_its_caption() {
        let html =
            super::images_to_alt(r#"<p><img src="./assets/alt=x.png" alt="真图" title="t" /></p>"#);
        assert_eq!(html, r#"<p>真图</p>"#);
        // alt 为空 → 整条删掉，连 src 都不许漏出来当字。
        assert_eq!(
            super::images_to_alt(r#"<p><img src="a.png" alt="" /></p>"#),
            "<p></p>"
        );
        // 属性值里带 `>`：收尾的 `>` 必须是**引号外**那一个，否则标签在中间被截断，
        // 剩下的半截 `b" src="x.png" />` 会当成正文出在屏上。渲染器自己发的是 `&gt;`
        // （实测 `![a>b](x.png)` 出 `alt="a&gt;b"`），所以下面这条是**防御**：万一输入
        // 换了来源，也不许把标签劈成两半。
        assert_eq!(
            super::images_to_alt(r#"<p><img alt="a>b" src="x.png" /></p>"#),
            "<p>a>b</p>"
        );
        // 渲染器真正会发的那一形：实体形式待在引号里。
        assert_eq!(
            super::images_to_alt(r#"<p><img src="x.png" alt="a&gt;b" /></p>"#),
            "<p>a&gt;b</p>"
        );
        // 带 title 的（`![](x.png "t")`）：title 在 HTML 里本来就不是可见文字，
        // 一起删掉才是「不造字」。
        assert_eq!(
            super::images_to_alt(r#"<p><img src="x.png" alt="" title="t" /></p>"#),
            "<p></p>"
        );
        // 作者手写的（转义下来的）那一条一个字都不动。
        let untouched = "<p>&lt;img src=\"x\" alt=\"y\"&gt;</p>";
        assert_eq!(super::images_to_alt(untouched), untouched);
    }

    /// 分章要保证的那件事本来就是「这一章的正文以它自己的标题开头」——
    /// 直接从正文看，不看位置（`spine_href` 里那个字节偏移早已不外带）。
    #[test]
    fn each_chapter_starts_at_its_own_heading() {
        let src = "# A\ntext\n## B\nmore\n";
        let (_, flow) = opened("d.md", src);
        assert_eq!(flow.chapters.len(), 2);
        for c in &flow.chapters {
            let first = c.source().lines().next().unwrap_or_default();
            assert!(
                first.contains(&c.title),
                "每一章该以它自己的标题开头：{first:?} vs {:?}",
                c.title
            );
        }
    }

    #[test]
    fn doc_without_heading_is_one_chapter_named_after_the_file() {
        let (meta, flow) = opened("mydoc.md", "just a paragraph\n");
        assert_eq!(flow.chapters.len(), 1);
        assert_eq!(flow.chapters[0].title, "mydoc");
        assert_eq!(meta.title, "mydoc");
    }

    #[test]
    fn content_before_the_first_heading_is_a_preface() {
        let (_, flow) = opened("d.md", "题记\n# 第一章\n正文\n");
        assert_eq!(flow.chapters.len(), 2);
        assert_eq!(flow.chapters[0].title, "前言");
        assert_eq!(flow.chapters[1].title, "第一章");
    }

    #[test]
    fn the_document_title_ignores_fenced_content() {
        let (meta, _) = opened("d.md", "```\n# fake\n```\n# real\n");
        assert_eq!(meta.title, "real");
    }

    /// 这条只是让上面那句「与 `.txt` 同判据」不是一句空话：两族共用同一份规范化，
    /// 所以 CRLF 与多余空行在 `.md` 上的结果与在 `.txt` 上一字不差。
    #[test]
    fn normalization_is_the_same_on_both_families() {
        let body = "a\r\n\r\n\r\n\r\nb\n\n";
        let (_, md) = opened("d.md", body);
        let (_, txt) = opened("d.txt", body);
        assert_eq!(
            visible(md.chapters[0].source()),
            visible(txt.chapters[0].source()),
            "规范化只有一份，两族的结果必须相同"
        );
    }
}
