//! 纯文本这一族（`.txt`），以及它与 Markdown 共用的那半条链。
//!
//! ## 为什么共用件住在这一份
//!
//! 两族走的是同一条链：读成文本 → [`normalize`] → 切成章 → 包装成引擎就绪的 XHTML。
//! 链只有一份，两族各给的是链上的**四个差异点** —— [`PlainFamily`] 那四个函数指针：
//! 怎么切、书目从哪儿来、目录怎么折、正文里那些标记怎么变成结构。
//!
//! 值得单独说清的是 [`normalize`]：锚点坐标系就是它的产物（见那条自己的说明），
//! 两族各写一份迟早会漂，而漂的症状是已存批注与进度**整体错位**、屏幕上查不出原因。
//! 同理 [`read_with`] 里「索引那一档怎么折成章表」也只有一份 —— 那格定的是
//! `chapters.char_len` 与 `ChapterLocator::Section` 的口径，两边本该逐字相同。
//!
//! Markdown 那一族多的只有那四个格子里的答案，所以它住在 [`super::markdown`] 并从这一份
//! 拿共用件；反过来不成立，纯文本不该知道 `#` 是什么。

use std::path::Path;

use super::{
    Chapter, ChapterLocator, Content, DocMeta, DocumentProvider, FlowDoc, Format, TocEntry, stem_of,
};
use crate::Result;
use crate::content::format::novel;

/// 按规则表分章的 `.txt`（纯文本电子书的主要形态）。
pub struct PlainTextAdapter;

impl DocumentProvider for PlainTextAdapter {
    fn format(&self) -> Format {
        Format::Text
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(super::extension_of(path).as_deref(), Some("txt"))
    }

    /// 只切章、**不包装**：正文等翻开那一章再取。
    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, &TEXT, false)
    }

    /// 单文件容器里的第 N 章：重读一遍、重切一次，取第 N 段。
    fn chapter_source(&self, path: &Path, at: &ChapterLocator) -> Result<String> {
        section_source(path, &TEXT, at)
    }

    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        read_with(path, &TEXT, true)
    }
}

/// 一族纯文本相对这条链的四个差异点。函数指针而不是泛型参数：调用点就两处，
/// 而它们要能被当成常量传下去。
pub(crate) struct PlainFamily {
    /// 书目里那一格报的**身份**：一个 provider 只能有一个 `Format`，
    /// 而 `books.format` 那一列要把 Markdown 与纯文本分开。
    pub format: Format,
    /// (规范化全文, 文件名主名) → 章表。
    pub split: fn(&str, &str) -> Vec<Chapter>,
    /// (规范化全文, 文件名主名) → 书目（书名 / 作者 / 简介）。
    pub info: fn(&str, &str) -> novel::detect::BookInfo,
    /// 章表 → 目录。空表就是「这本没有目录」，界面上那一屏什么都不摆。
    pub toc: fn(&[Chapter]) -> Vec<TocEntry>,
    /// 一章的源文本 → 引擎就绪的 XHTML。
    ///
    /// 这一格**同时定着那把尺**：`chapters.char_len` 与 `Locator.offset` 数的都是它产物
    /// 的可见文本，所以它是「一族有几个字段」里最不能各写一份的那一个 —— 索引那一趟与
    /// 开书那一趟问的是同一个问题，两份答案就会让进度分母与章内偏移不同源（见
    /// [`read_with`] 里数字数那一格）。纯文本认的是没有标记的正文，
    /// [`super::xhtml::from_plain_text`]；Markdown 的源文本满屏是 `**` 与 `- `，
    /// 要过一次真的解析。
    pub wrap: fn(&str) -> String,
}

/// `.txt`：按规则表分章（`format::novel`），认出目录页，目录跟着章表一起交出去。
///
/// 三格都吃同一份生效那套表：`split` 用 `markers` / `gates`，`info` 用 `meta` 那几串别名，
/// `toc` 从切好的章表建。**没接的只剩一格**：`volumes` 那族认得出来但不切，它要的是目录
/// 的第二层（`plans/纯文本小说.md` 的后续）。
const TEXT: PlainFamily = PlainFamily {
    format: Format::Text,
    split: split_by_rules,
    info: book_info_by_rules,
    toc: novel::detect::toc_of,
    wrap: super::xhtml::from_plain_text,
};

pub(crate) fn book_info_by_rules(source: &str, stem: &str) -> novel::detect::BookInfo {
    novel::detect::book_info(source, stem, &novel::rules::CompiledRules::builtin())
}

pub(crate) fn split_by_rules(source: &str, stem: &str) -> Vec<Chapter> {
    novel::detect::split_with(source, stem, &novel::rules::CompiledRules::builtin())
}

/// 读一遍并按 [`normalize`] 折好：两族共用的起点。
///
/// 走 `charset::read_plain` 而不是 `std::fs::read_to_string`：后者只吃 UTF-8，而中文小说的
/// `.txt` 与日文轻小说的 `.txt` 满地是非 UTF-8。纯文本没有声明位，所以「文件里写着
/// `charset=` 就听它的」那一档在这一族故意不生效（会被书里代码清单的一行劫持），
/// 顺序是 BOM > 合法 UTF-8 > 白名单内的码页判定 > UTF-8，判据写在 `format::charset`。
pub(crate) fn read_normalized(path: &Path) -> Result<String> {
    let raw = super::charset::read_plain(path)?;
    Ok(normalize(&raw))
}

/// 索引那一趟 / 全文那一趟，两族共用这一份形状：怎么切由 `family` 给。
///
/// **字数在这一趟算**（`chapters.char_len`，见 `plans/导入管线.md` §八 第 3 条）：
/// 单文件格式的 I/O 本来就躲不掉（切章要把整份文本过一遍），所以这一趟的正文
/// 已经在手上，数一遍是白拿的。数的是**包装之后**那份 XHTML 的可见文本 ——
/// 与 `Chapter::inline` 同一条口径，也与引擎真正吃到的东西同一份。
pub(crate) fn read_with(
    path: &Path,
    family: &PlainFamily,
    with_text: bool,
) -> Result<(DocMeta, Content)> {
    let source = read_normalized(path)?;
    let stem = stem_of(path);
    content_from_source(&source, &stem, family, with_text)
}

/// 已经拿到**规范化文本**之后的那半条链：书目、切章、目录、包装。
///
/// 单文件格式里只有 `.txt` / `.md` / PDB 三族走这里，而 PDB 的文本不是从文件直接
/// 读出来的（要先解 PalmDOC 记录），所以把「读文件」与「文本之后」拆开 —— 两族用的
/// 仍是同一套分章、书目与字尺，不另写一份。
pub(crate) fn content_from_source(
    source: &str,
    stem: &str,
    family: &PlainFamily,
    with_text: bool,
) -> Result<(DocMeta, Content)> {
    let info = (family.info)(source, stem);
    let meta = DocMeta {
        title: info.title,
        authors: info.authors,
        description: info.description,
        format: family.format,
        size: source.len() as u64,
        ..DocMeta::default()
    };
    let chapters = (family.split)(source, stem);
    // 目录从**切好的**章表建：索引那一趟随后会把它们换成「只有标题与序号」的位置，
    // 那时标题还在，但间隔与字数已经数过了 —— 早一步取省一次遍历。
    let toc = (family.toc)(&chapters);
    let chapters = chapters
        .into_iter()
        .enumerate()
        .map(|(ix, chapter)| {
            if with_text {
                wrap(family, chapter)
            } else {
                // 字数要按**包装之后**那一份数，不是按源文本数。两趟数的是同一件东西
                // （`Locator.offset` 那把尺），而 Markdown 的包装会吃掉 `**` 与 `- `
                // 这些源里看得见、屏上看不见的字符：按源文本数就比读者看到的多几个字，
                // 进度分母偏大、章内偏移整体偏小，而屏幕上只会表现为「进度条差一点点」。
                // 宁可在这里多包一次（一次纯内存的解析），也不留两份口径。
                let chars = super::visible_chars(&(family.wrap)(chapter.source()));
                // 图片清单**不**在这一档带走：这里的 `chapter` 装的是还没包成 XHTML 的
                // 原始文本，而排版看到的是 `(family.wrap)` 之后的那一份（纯文本那一族
                // 会把源里写着的 `<img>` 转义成一行字）。拿原始文本扫出来的清单会比
                // 正文多几张画不出来的图。
                Chapter::section(chapter.title.clone(), ix).with_char_len(chars)
            }
        })
        .collect();
    Ok((
        meta,
        Content::Reflow(FlowDoc {
            chapters,
            toc,
            ..Default::default()
        }),
    ))
}

/// [`ChapterLocator::Section`] 那一路：重读整本、重切一次，取第 N 段并包装。
///
/// 每次翻章都要过一遍全文件 —— 阅读会话按章缓存（`Book::section_texts` 那格 memo 与
/// 它那条「N 章来回翻，整本只该解一遍」的门），所以一本书里一章只付一次。
pub(crate) fn section_source(
    path: &Path,
    family: &PlainFamily,
    at: &ChapterLocator,
) -> Result<String> {
    let source = read_normalized(path)?;
    let stem = stem_of(path);
    section_from_source(&source, &stem, path, family, at)
}

/// 已经拿到规范化文本之后的 [`ChapterLocator::Section`] 那一路（PDB 用这一支）。
pub(crate) fn section_from_source(
    source: &str,
    stem: &str,
    path: &Path,
    family: &PlainFamily,
    at: &ChapterLocator,
) -> Result<String> {
    let unavailable = |index: usize| crate::Error::ChapterUnavailable {
        path: path.to_path_buf(),
        index,
        at: format!("{at:?}"),
    };
    let index = match at {
        ChapterLocator::Section(index) => *index,
        _ => return Err(unavailable(0)),
    };
    (family.split)(source, stem)
        .into_iter()
        .nth(index)
        .map(|chapter| (family.wrap)(chapter.source()))
        .ok_or_else(|| unavailable(index))
}

/// 把一章的源文本包成引擎就绪的 XHTML。
///
/// 包装放在这一层而不是界面里：正文的契约是「引擎就绪」，界面就不必知道哪个格式
/// 需要包一层（见 [`super::xhtml`] 的模块说明）。怎么包由 `family` 给 —— 纯文本的
/// 源文本已经没有标记，Markdown 的还有 `**`、`- `、围栏要折成结构。
pub(crate) fn wrap(family: &PlainFamily, chapter: Chapter) -> Chapter {
    let wrapped = (family.wrap)(chapter.source());
    Chapter::inline(chapter.title.clone(), wrapped)
}

/// 统一行尾、压掉多余空行。**规范化必须在切章之前**，因为锚点坐标系就是这份
/// 规范化文本，改动它会让已存标注整体漂移 —— 所以这一份是两族共用的**唯一一份**。
pub(crate) fn normalize(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 1);
    let mut blanks = 0usize;
    for line in src.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
pub(crate) mod test_support {
    //! 两族测试共用的那几件：写原始字节、走公开入口读回来、取可见文本。
    //! 放在这里而不是各写一份，是因为「包装不改内容」这条不变量的验法必须两边同口径。

    use std::path::PathBuf;

    use crate::content::anchor::TextMap;
    use crate::content::dom::Dom;
    use crate::content::format::{Chapter, DocMeta, FlowDoc};

    /// 把内容写进临时文件，经公开的 `open()` 读回来，并按位置把每一章的正文取上。
    pub(crate) fn opened(name: &str, body: &str) -> (DocMeta, FlowDoc) {
        opened_bytes(name, body.as_bytes())
    }

    /// 把**原始字节**写进临时文件再走公开入口读回来。UTF-16 与 GBK 那几条门要的就是
    /// 这种不经手字符串的输入 —— `&str` 参数造不出带 BOM 的字节。
    pub(crate) fn opened_bytes(name: &str, body: &[u8]) -> (DocMeta, FlowDoc) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        let doc = crate::content::document::open(&path).expect("临时文件应能读入");
        // `open` 只给索引；这一层要断言的绝大多数是正文，按位置取回来。
        let mut flow = doc.flow().expect("该读成可重排").clone();
        for (ix, chapter) in flow.chapters.iter_mut().enumerate() {
            let text = doc.chapter_source(ix).expect("正文该取得到");
            *chapter = Chapter::inline(chapter.title.clone(), text);
        }
        (doc.meta().clone(), flow)
    }

    /// UTF-16LE 那份字节（BOM + 每两字节一个小端码元）。
    pub(crate) fn utf16le(text: &str) -> Vec<u8> {
        let mut out = b"\xff\xfe".to_vec();
        out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        out
    }

    /// 一段 XHTML 的可见文本：断言要看它，不看标记 —— 后者一改包装口径就红，
    /// 前者才钉得住真正的不变量。
    pub(crate) fn visible(xhtml: &str) -> String {
        let dom = Dom::parse(xhtml.as_bytes());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map_or(dom.document, |(id, _)| id);
        TextMap::build(&dom, body).text().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{opened, opened_bytes, utf16le, visible};
    use crate::content::format::Format;

    /// 认不出标记的一本：一整篇一章，章名与书名都是文件名主名。这是**降级**那一档，
    /// 不是这一族的目标形态（目标见下一条）。
    #[test]
    fn a_text_file_without_markers_is_one_chapter_named_after_the_file() {
        let (meta, flow) = opened("三体.txt", "正文第一行\n正文第二行\n");
        assert_eq!(flow.chapters.len(), 1);
        assert_eq!(flow.chapters[0].title, "三体");
        assert_eq!(meta.title, "三体");
    }

    /// 这一族真正要交付的那件事：一本有章标记的 `.txt` 按规则表切开，**并且目录跟着一起
    /// 交出去**（目录那屏从前对 TXT 是空的）。章标题行同时要走 `<h*>` 那一档 —— 屏上
    /// 看得出章从哪儿起，而不是「第一章」混在正文段落里。
    #[test]
    fn a_chaptered_text_file_splits_and_gets_a_table_of_contents() {
        let mut body = String::new();
        for n in 1..=6 {
            body.push_str(&format!("第{n}章 标题{n}\n{}\n\n", "雪".repeat(600)));
        }
        let body = format!("书名：雪夜关门\n作者：无名氏\n\n{body}");
        let (meta, flow) = opened("书.txt", &body);
        assert_eq!(meta.title, "雪夜关门", "头部那行「书名：」要用上");
        assert_eq!(
            meta.authors,
            vec!["无名氏".to_string()],
            "作者也要一起落进书目"
        );
        // 头部那两行（书名 / 作者）自己也占一章 —— 分章不吃字，就不许把它们的字吞掉。
        assert_eq!(
            flow.chapters.len(),
            7,
            "六章正文 + 头部那一块： {:?}",
            flow.chapters
                .iter()
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(flow.chapters[0].title, "书", "没认出目录页时那块就叫主名");
        assert_eq!(flow.chapters[1].title, "第1章 标题1");
        assert_eq!(flow.toc.len(), 7, "目录要跟章表一起交出来");
        assert_eq!(flow.toc[6].title, "第6章 标题6");
        // 章标题得画成标题，不是普通段落：这条是屏上「看得出章起点」的全部依赖。
        assert!(
            flow.chapters[2].source().starts_with("<h1>"),
            "章标题行要包成 `<h1>`：{}",
            flow.chapters[2].source()
        );
    }

    /// 没有 BOM、没有声明、也不是合法 UTF-8 的小说 `.txt`（简体 GBK）端到端地读得开。
    /// 改之前它的下场是 `read_to_string` 报错、而且那个错误在 `ImportReject` 的四个
    /// 变体里没有分类 —— 既不是「不支持」也不是「读不动」，是一句说不清的失败。
    /// 这一条走公开入口，所以它同时钉住三件事：判定放行、`read_plain` 接上了码页判定、
    /// 包装出来的正文是那串汉字。
    #[test]
    fn an_undeclared_gbk_novel_txt_reads_as_chinese_instead_of_holes() {
        let src = "大雪封山的那一夜，他终于推开那扇门。\n第二行也还是正文。\n";
        let bytes = encoding_rs::GBK.encode(src).0.to_vec();
        assert!(
            std::str::from_utf8(&bytes).is_err(),
            "这份字节该真的不是 UTF-8，否则这条测试没测到东西"
        );
        let (meta, flow) = opened_bytes("书.txt", &bytes);
        let body = visible(flow.chapters[0].source());
        assert!(
            body.contains("大雪封山的那一夜"),
            "GBK 的书要解成汉字：{body}"
        );
        assert!(!body.contains('\u{fffd}'), "解得通就不该留洞：{body}");
        assert_eq!(meta.format, Format::Text);
    }

    /// UTF-16LE 带 BOM 的 `.txt`：解出来的正文与同一份内容的 UTF-8 版逐字相同。
    /// 「两趟一致」本身不够，所以另断一次内容不是问号。
    #[test]
    fn a_utf16_text_file_reads_the_same_as_its_utf8_twin() {
        let src = "第一章标题那一行\n大雪封山的那一夜，他终于推开那扇门。\n";
        let (utf8_meta, utf8) = opened("d.txt", src);
        let (u16_meta, u16) = opened_bytes("d.txt", &utf16le(src));
        assert_eq!(u16_meta.title, utf8_meta.title, "书名要一样");
        assert_eq!(
            u16.chapters[0].source(),
            utf8.chapters[0].source(),
            "正文要逐字相同"
        );
        assert!(
            !u16.chapters[0].source().contains('\u{fffd}')
                && u16.chapters[0].source().contains("大雪封山"),
            "UTF-16 不该退成一排问号：{}",
            u16.chapters[0].source()
        );
    }

    /// 规范化只有一份，它得真的把 CRLF 与连续空行折成 `from_plain_text` 认的那个形状。
    /// 这一条断的是**具体串**而不是「两族一致」—— 一致也可能两族一起坏。
    #[test]
    fn normalize_collapses_crlf_and_blank_runs_into_one_paragraph_break() {
        let (_, flow) = opened("d.txt", "a\r\n\r\n\r\n\r\nb\n\n");
        assert_eq!(flow.chapters[0].source(), "<p>a</p>\n<p>b</p>\n");
    }

    /// 这一族只认 `.txt`，而且**不许把别族的扩展名抢过来**：`provider_for` 是同权重按
    /// 注册顺序先赢的，抢走的下场是同一本书两次导入判成两种格式。要加新扩展名，
    /// `supported_extensions()` 与 `sniff::bare_file` 那张表得同批改，不然粗筛与判定会漂。
    #[test]
    fn the_plain_adapter_claims_txt_and_nobody_elses_extension() {
        let claims = |name: &str| {
            crate::content::format::provider_for(std::path::Path::new(name))
                .is_some_and(|p| p.format() == Format::Text)
        };
        assert!(claims("a.txt"), "这一族认领的只有它");
        assert!(!claims("a.md"), "Markdown 有自己的 provider，不能被抢走");
        assert!(!claims("a.epub"), "容器不是纯文本");
    }
}
