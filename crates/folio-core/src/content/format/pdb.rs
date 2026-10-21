//! PalmDoc（`.pdb`）：PalmDB 容器里的文本记录。
//!
//! 这是本仓库第二种「PalmDB 里装东西」的格式（第一种是 MOBI/AZW4）。它没有章表、
//! 也没有码页声明位：记录 0 是 PalmDoc 头，正文是后面那几条记录按 PalmDOC 解压后
//! 拼起来的字节流。
//!
//! **正文可以是标记**：`TEXt`/`REAd` 只说得出「文本记录」，说不出那文本是纯字还是 HTML ——
//! 早期 PalmDOC 有一族文件（TextReader 那一类，libmobi 的 `sample-textread` 就是它）记录里
//! 直接写着 `<HTML><HEAD>…`。按纯文本收会把整份标记转义成屏上看得见的一堆尖括号，
//! 所以 [`read_palmdoc`] 解完先看头部，是标记就换 [`PDB_MARKUP`] 那一族：整篇一章、
//! 原样交给引擎，与单文件 `.html` 同一条立场。
//!
//! 分章与书目**不另写一份**：纯字的那一族交给 [`super::text`]，与 `.txt` 吃同一张规则表。
//! 差别只在「从哪儿读」——一个从文件读，一个从 PalmDB 记录读。
//!
//! 书名取 **PalmDB 头里的数据库名**（偏移 0..32，与 [`super::azw4`] 同一格），缺了才退到
//! 文件名主名：那一名是制作者写进去的，比读者改过的文件名更接近书名。标记那一族另有
//! 更硬的一条线索 —— **早期 Mobipocket（OEB1）没有 `EXTH`，书名 / 著者 / 出版者写在正文
//! 头部的 `<dc:*>` 块里**，那一块按 Dublin Core 的术语逐条落进 [`DocMeta`] 同名的那几格。
//!
//! 加密方式那一格（记录 0 头偏移 12）也读：非 0 就报 [`crate::Error::Encrypted`]，不去解压。
//! 与 [`super::mobi`] 读的是同一格，以前只有 MOBI 那一路认它。
//!
//! 编码：PalmDoc 规范里正文是 cp1252，但中文 PDB 满地是 GBK/Big5。没有声明可听，
//! 所以走 [`super::charset::decode_detected`]：合法 UTF-8 先收，否则问一次白名单内的
//! 探测器，认不出就按 UTF-8 收留看得见的洞。

use std::path::Path;

use super::palmdb::PalmDb;
use super::text::{self, PlainFamily};
use super::{Content, DocMeta, DocumentProvider, Format, stem_of};
use crate::Result;

/// PalmDoc 头至少要有这么多字节（压缩方式 + 未用 + 正文长度 + 记录数 + 记录大小 + 加密方式 + 未用）。
const HEADER_MIN: usize = 16;

/// 加密方式那一格在记录 0 头里的偏移（**非 0 就是加过密**）。
///
/// 与 [`super::mobi::header`] 读的是同一格：PalmDOC 与 Mobipocket 共用这张头，MOBI 那一族
/// 早就认它，PDB 这一路以前没读，于是加密的本子被当成 PalmDOC 解压了一遍，
/// 交出来的是一屏乱码而不是「这本有 DRM」。
const ENCRYPTION_AT: usize = 12;

/// 纯文本那一族的四个差异点：切章 / 书目 / 目录 / 包装全部与 `.txt` 共用。
const PDB: PlainFamily = PlainFamily {
    format: Format::Pdb,
    split: text::split_by_rules,
    info: text::book_info_by_rules,
    toc: super::novel::detect::toc_of,
    wrap: super::xhtml::from_plain_text,
};

/// 正文其实是标记的那一族：整篇一章、原样交给引擎。
///
/// 只有 `wrap` 与 `split` 真的不同；目录仍从章表建（一章一条），书目仍只认 `<title>` /
/// `<h1>` 那一格 —— 标记里的「作者：某某」那种行是正文的一部分，拿规则表去扫它会把
/// 一句引文当成书目（与 [`super::novel::detect::book_info`] 只看头部同一个理由，
/// 而这里连头部都没有：标记的头部是 `<HEAD>`）。
const PDB_MARKUP: PlainFamily = PlainFamily {
    format: Format::Pdb,
    split: markup_chapters,
    info: markup_info,
    toc: super::novel::detect::toc_of,
    wrap: keep_markup,
};

/// 一本 PalmDOC 读出来的两格。
struct PalmDoc {
    /// 书名：PalmDB 的数据库名，缺了是文件名主名。
    title: String,
    /// 解压并解完码的正文（还没切章、还没包装）。
    text: String,
}

pub struct PdbAdapter;

impl DocumentProvider for PdbAdapter {
    fn format(&self) -> Format {
        Format::Pdb
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(super::extension_of(path).as_deref(), Some("pdb"))
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let doc = read_palmdoc(path)?;
        let markup = starts_as_markup(&doc.text);
        let (mut meta, content) = text::content_from_source(
            &doc.text,
            &doc.title,
            if markup { &PDB_MARKUP } else { &PDB },
            false,
        )?;
        // 都柏林核心那一块只在**标记**的payload 里问：纯文本的本子若正文明明写着一句
        // `<dc:title>…</dc:title>`，按 HTML 解一遍就会把它当成书目 —— 那是「提到了什么」
        // 冒充「这是什么」，与 `starts_as_markup` 锚在开头同一个理由。
        if markup {
            apply_dublin_core(&doc.text, &mut meta);
        }
        Ok((meta, content))
    }

    fn chapter_source(&self, path: &Path, at: &super::ChapterLocator) -> Result<String> {
        let doc = read_palmdoc(path)?;
        let markup = starts_as_markup(&doc.text);
        text::section_from_source(
            &doc.text,
            &doc.title,
            path,
            if markup { &PDB_MARKUP } else { &PDB },
            at,
        )
    }

    fn read(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let doc = read_palmdoc(path)?;
        let markup = starts_as_markup(&doc.text);
        let (mut meta, content) = text::content_from_source(
            &doc.text,
            &doc.title,
            if markup { &PDB_MARKUP } else { &PDB },
            true,
        )?;
        if markup {
            apply_dublin_core(&doc.text, &mut meta);
        }
        Ok((meta, content))
    }

    /// 整本只解一次：PalmDoc 的每条记录解压后要拼起来才切得动章，逐章重解等于
    /// 把同一份活干 N 遍。
    fn read_texts(&self, path: &Path) -> Option<Vec<String>> {
        let (_, content) = self.read(path).ok()?;
        let Content::Reflow(flow) = content else {
            return None;
        };
        Some(
            flow.chapters
                .into_iter()
                .map(|chapter| chapter.source().to_string())
                .collect(),
        )
    }
}

/// 头部（跳过空白与 BOM）是不是以文档标记开头。
///
/// **必须锚在开头**，不能「整本里含 `<html` 就算」：一本讲 HTML 的纯文本小说会含这几个
/// 字符，而它按纯文本收才是对的（与 `format::sniff::starts_with_rtf` 同一个立场 ——
/// 判据是「这本文档是什么」，不是「它提到了什么」）。
fn starts_as_markup(text: &str) -> bool {
    let head = text.trim_start().trim_start_matches('\u{feff}');
    // 取前 10 个**字符**再转小写比对：按字节切会在「第一章」那种多字节开头上直接 panic，
    // 而这一格判的恰恰是「开头是不是标记」。
    let probe: String = head.chars().take(10).collect();
    ["<!doctype", "<html", "<?xml"]
        .iter()
        .any(|magic| probe.to_ascii_lowercase().starts_with(magic))
}

/// 标记那一族：整篇一章。
fn markup_chapters(source: &str, stem: &str) -> Vec<super::Chapter> {
    let title = super::html::document_title(source).unwrap_or_else(|| stem.to_string());
    vec![super::Chapter::inline(title, source)]
}

/// 标记那一族的书目：只认 `<title>` / `<h1>`，其余格子留空。
fn markup_info(source: &str, stem: &str) -> super::novel::detect::BookInfo {
    super::novel::detect::BookInfo {
        title: super::html::document_title(source).unwrap_or_else(|| stem.to_string()),
        authors: Vec::new(),
        description: String::new(),
    }
}

/// 标记已经是引擎就绪的那一份，包装这一格只剩「不许动它」。
///
/// 与 [`super::html`] 的 `chapter_source` 同一条立场：正文原样透传，坐标系的稳定性靠
/// **不转换**（`plans/Locator.md`：锚点就建在源文本上）。
fn keep_markup(source: &str) -> String {
    source.to_string()
}

/// 都柏林核心那一块能填满的书目格子（`<dc:*>` 一族标签，按文档序收）。
#[derive(Default)]
struct DublinCore {
    title: String,
    authors: Vec<String>,
    contributors: Vec<String>,
    subjects: Vec<String>,
    description: String,
    publisher: String,
    published: String,
    language: String,
    isbn: String,
    rights: String,
}

/// 只扫头部这么多**字符**。与 [`super::novel::detect::book_info`] 只看头部几十行同一个理由：
/// 整份去找，正文里一句提到 `<dc:title>` 的话就会被当成书目。
const DC_HEAD: usize = 4000;

/// 从头部那一块 `<dc:*>` 里收书目。
///
/// 判据是**带 `dc:` 前缀的标签名**（html5ever 在 HTML 解析里把 `<dc:Title>` 记成本地名
/// `dc:title`，实测）：裸 `<title>` 是**页面**标题，与这本书的书名不是同一件事，
/// 那一格交给 [`super::html::document_title`] 去问。
///
/// 早期 Mobipocket（OEB1）没有 `EXTH`，书名 / 著者 / 出版者就写在这块里 —— 真样本
/// `sample-textread` 的 `<dc-metadata>` 摆着 Title / Publisher / Creator / Language 四条，
/// 而它的 `<h1>` 写的是另一句话。
fn dublin_core(source: &str) -> DublinCore {
    let head: String = source.chars().take(DC_HEAD).collect();
    let dom = crate::content::dom::Dom::parse(head.as_bytes());
    let mut out = DublinCore::default();
    for (id, _) in dom.elements() {
        let Some(tag) = dom.tag_name(id).filter(|tag| tag.starts_with("dc:")) else {
            continue;
        };
        let value = dom.raw_text_of_subtree(id).trim().to_string();
        if value.is_empty() {
            continue;
        }
        match &tag[3..] {
            "title" => out.title = value,
            "creator" => out.authors.push(value),
            "contributor" => out.contributors.push(value),
            "subject" => out.subjects.push(value),
            "description" => out.description = value,
            "publisher" => out.publisher = value,
            "date" => out.published = value,
            "language" => out.language = value,
            "rights" => out.rights = value,
            // `dc:Identifier` 要按 scheme 筛：`uid` / DOI / Goodreads id 都不是 ISBN。
            // 真样本的第一条就是 `<dc:Identifier id="uid">`。
            "identifier"
                if dom
                    .attr(id, "scheme")
                    .or_else(|| dom.attr(id, "id"))
                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("isbn")) =>
            {
                out.isbn = value;
            }
            _ => {}
        }
    }
    out
}

/// 把那一块写进书目：**有值才覆盖**（块里没说的格子留着，让 `<title>` → `<h1>` →
/// 容器名那条回落链说话）。
fn apply_dublin_core(source: &str, meta: &mut DocMeta) {
    let core = dublin_core(source);
    // 书名那一格与 EPUB / MOBI 同一条：块里写了才算，空串不许把已有的名字抹掉。
    if !core.title.is_empty() {
        meta.title = core.title;
    }
    if !core.authors.is_empty() {
        meta.authors = core.authors;
    }
    if !core.contributors.is_empty() {
        meta.contributors = core.contributors;
    }
    if !core.subjects.is_empty() {
        meta.subjects = core.subjects;
    }
    if !core.description.is_empty() {
        // 带标签的简介到这里已经是纯文本了：`raw_text_of_subtree` 收的是子树里的**字**，
        // 标签本身在 DOM 里不占文本（DocMeta 那条「界面这一行是标签行」的契约自动满足）。
        meta.description = core.description;
    }
    if !core.publisher.is_empty() {
        meta.publisher = core.publisher;
    }
    if !core.published.is_empty() {
        meta.published = core.published;
    }
    if !core.language.is_empty() {
        meta.language = core.language;
    }
    if !core.isbn.is_empty() {
        meta.isbn = core.isbn;
    }
    if !core.rights.is_empty() {
        meta.rights = core.rights;
    }
}

/// 记录 0 的 PalmDoc 头是不是我们认得的那一种。
///
/// 判定层用它把「PalmDB 里的文本」与「别的 PalmDB 数据库」分开：类型字段说是书还不够，
/// 记录 0 得真的有一张像样的 PalmDoc 头。
pub(crate) fn is_palmdoc(db: &PalmDb) -> bool {
    let Some(header) = db.read(0) else {
        return false;
    };
    if header.len() < HEADER_MIN {
        return false;
    }
    let compression = u16::from_be_bytes([header[0], header[1]]);
    let count = u16::from_be_bytes([header[8], header[9]]) as usize;
    matches!(compression, 1 | 2 | 17480)
        && db.record_count() > 1
        && (count == 0 || count < db.record_count())
}

/// 把一本 PalmDOC 读成「书名 + 解压解完码的正文」。
///
/// DRM 那一格排在解压**前面**：加过密的正文永远解不开，先解压再把一屏乱码交出去，
/// 读者的动作是「等我们修」，而真正的动作是换一本没加密的（`Error::Encrypted` 存在的
/// 全部理由，见它的文档）。
fn read_palmdoc(path: &Path) -> Result<PalmDoc> {
    let db = PalmDb::open(path)?;
    let header = db
        .read(0)
        .ok_or_else(|| unsupported(path, "PalmDoc 头读不出来"))?;
    if header.len() < HEADER_MIN {
        return Err(unsupported(path, "PalmDoc 头不够长"));
    }
    let compression = u16::from_be_bytes([header[0], header[1]]);
    let text_len = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let declared = u16::from_be_bytes([header[8], header[9]]) as usize;
    let encryption = u16::from_be_bytes([header[ENCRYPTION_AT], header[ENCRYPTION_AT + 1]]);
    let available = db.record_count().saturating_sub(1);
    if available == 0 {
        return Err(unsupported(path, "PalmDoc 一条正文记录都没有"));
    }
    let name = {
        let stored = db.db_name().trim();
        if stored.is_empty() {
            stem_of(path)
        } else {
            stored.to_string()
        }
    };
    if encryption != 0 {
        return Err(crate::Error::Encrypted {
            name,
            method: format!("mobipocket-encryption-{encryption}"),
        });
    }
    // 头里的记录数可能说谎（截断的书、转换器写坏）。按实际记录数夹住：能读多少读多少，
    // 而不是因为一个数不对就整本打不开。
    let count = if declared == 0 || declared > available {
        available
    } else {
        declared
    };

    let mut bytes = Vec::with_capacity(text_len.max(1024));
    for index in 1..=count {
        let record = db
            .read(index)
            .ok_or_else(|| unsupported(path, "PalmDoc 正文记录读不出来"))?;
        match compression {
            1 => bytes.extend_from_slice(&record),
            2 | 17480 => super::mobi::codec::palmdoc_decompress(&record, &mut bytes),
            other => {
                return Err(unsupported(
                    path,
                    &format!("PalmDoc 压缩方式 {other} 这一版不认"),
                ));
            }
        }
    }
    if text_len > 0 {
        bytes.truncate(text_len);
    } else {
        // 少数转换器把 text_length 写成 0。没有这把尺就按记录尾部的 NUL 填充剥：
        // PalmOS 会把记录补到 2/4 字节边界，填充不属于正文。
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
    }
    if bytes.is_empty() {
        return Err(unsupported(path, "PalmDoc 正文是空的"));
    }
    Ok(PalmDoc {
        title: name,
        text: super::charset::decode_detected(&bytes),
    })
}

fn unsupported(path: &Path, why: &str) -> crate::Error {
    crate::Error::UnsupportedFormat(format!(
        "{}：{why}",
        path.file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use super::{ENCRYPTION_AT, PdbAdapter, is_palmdoc};
    use crate::content::format::{Content, DocumentProvider, Format, palmdb::PalmDb};

    const HEADER: usize = 78;

    /// 摆一个最小 PalmDB：头 + 记录表 + 记录内容。
    ///
    /// `book` 写进偏移 0..32 那格（真 PalmDB 那里就是**数据库名**，不是「PalmDB」这个
    /// 内存里的签名）—— 书名那一格就取自它，所以测试里的名字必须是参数而不是常量。
    fn db_at(dir: &Path, name: &str, book: &str, kind: [u8; 4], records: &[&[u8]]) -> PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        let mut head = vec![0u8; HEADER];
        head[..book.len().min(32)].copy_from_slice(&book.as_bytes()[..book.len().min(32)]);
        head[60..64].copy_from_slice(&kind);
        head[64..68].copy_from_slice(b"REAd");
        head[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
        file.write_all(&head).unwrap();
        let mut start = HEADER + records.len() * 8;
        let mut table = Vec::with_capacity(records.len() * 8);
        for record in records {
            table.extend_from_slice(&(start as u32).to_be_bytes());
            table.push(0);
            table.extend_from_slice(&[0, 0, 0]);
            start += record.len();
        }
        file.write_all(&table).unwrap();
        for record in records {
            file.write_all(record).unwrap();
        }
        path
    }

    /// PalmDoc 头 + 一条不压缩的正文记录。
    fn palm_doc(compression: u16, text: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut header = vec![0u8; 16];
        header[0..2].copy_from_slice(&compression.to_be_bytes());
        header[4..8].copy_from_slice(&(text.len() as u32).to_be_bytes());
        header[8..10].copy_from_slice(&1u16.to_be_bytes());
        header[10..12].copy_from_slice(&4096u16.to_be_bytes());
        (header, text.to_vec())
    }

    #[test]
    fn it_reads_an_uncompressed_palmdoc() {
        let dir = tempfile::tempdir().unwrap();
        let (header, body) = palm_doc(
            1,
            "第一章 开始\n\n正文一\n\n第二章 继续\n\n正文二\n".as_bytes(),
        );
        let path = db_at(
            dir.path(),
            "书.pdb",
            "头里的书名",
            *b"TEXt",
            &[&header, &body],
        );
        let db = PalmDb::open(&path).unwrap();
        assert!(is_palmdoc(&db));

        let (meta, content) = PdbAdapter.read(&path).unwrap();
        assert_eq!(meta.format, Format::Pdb);
        let Content::Reflow(flow) = content else {
            panic!("PalmDoc 该读成可重排");
        };
        assert!(!flow.chapters.is_empty());
        let joined: String = flow
            .chapters
            .iter()
            .map(|chapter| crate::content::format::visible_text(chapter.source()))
            .collect();
        assert!(joined.contains("正文一"), "正文不该丢：{joined}");
        assert!(joined.contains("正文二"), "正文不该丢：{joined}");
    }

    /// 不压缩的那一档按原字节拼；GBK 文本走探测器，不能被当成 UTF-8 的替换字符。
    #[test]
    fn gbk_bytes_are_decoded_by_the_detector() {
        let dir = tempfile::tempdir().unwrap();
        let text = encoding_rs::GBK.encode("第一章 开始\n\n中文正文\n").0;
        let (header, body) = palm_doc(1, &text);
        let path = db_at(dir.path(), "gbk.pdb", "GBK 本", *b"TEXt", &[&header, &body]);
        let (_, content) = PdbAdapter.read(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("该读成可重排");
        };
        let joined: String = flow
            .chapters
            .iter()
            .map(|chapter| crate::content::format::visible_text(chapter.source()))
            .collect();
        assert!(joined.contains("中文正文"), "GBK 没解对：{joined}");
    }

    /// 头里说两条、实际只有一条：读实际有的那条，不整本失败。
    #[test]
    fn a_lying_record_count_is_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let (mut header, body) = palm_doc(1, "正文".as_bytes());
        header[8..10].copy_from_slice(&9u16.to_be_bytes());
        let path = db_at(
            dir.path(),
            "lie.pdb",
            "说谎的头",
            *b"TEXt",
            &[&header, &body],
        );
        let (_, content) = PdbAdapter.read(&path).unwrap();
        assert!(matches!(content, Content::Reflow(_)));
    }

    #[test]
    fn a_bad_header_is_not_a_palmdoc() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_at(dir.path(), "bad.pdb", "坏头", *b"TEXt", &[b"short", b"x"]);
        let db = PalmDb::open(&path).unwrap();
        assert!(!is_palmdoc(&db));
        assert!(PdbAdapter.read(&path).is_err());
    }

    /// 书名取 PalmDB 头里那一名（偏移 0..32），**不是**文件名主名。
    ///
    /// 撤掉这一格（退回 `stem_of`）这一条就红：那一名是制作者写进文件的，而文件名
    /// 是读者手上那个可能被改过、可能被下载工具切成 `download(1).pdb` 的东西。
    #[test]
    fn the_title_is_the_palmdb_name_not_the_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let (header, body) = palm_doc(1, "第一章\n\n正文\n".as_bytes());
        let named = db_at(
            dir.path(),
            "download(1).pdb",
            "三体",
            *b"TEXt",
            &[&header, &body],
        );
        assert_eq!(PdbAdapter.read(&named).unwrap().0.title, "三体");

        // 头里那一格是空的（转换器不写）才退到文件名主名。
        let blank = db_at(dir.path(), "无名.pdb", "", *b"TEXt", &[&header, &body]);
        assert_eq!(PdbAdapter.read(&blank).unwrap().0.title, "无名");
    }

    /// 加密方式那一格（记录 0 头偏移 12）非 0 就说「这本有 DRM」，不去解压。
    ///
    /// 这条门的价值全在**说法**上：以前这一本照样解得出东西，交出来的是一屏乱码，
    /// 而屏幕上看着「有正文」—— 读者会以为是我们的实现坏了，等一个永远不会来的修复。
    /// 类别也要是 `Encrypted` 而不是 `UnsupportedFormat`，导入报表那一条分得开这两件事。
    #[test]
    fn an_encrypted_palmdoc_is_refused_as_encrypted() {
        use crate::infra::error::ImportReject;

        let dir = tempfile::tempdir().unwrap();
        let (mut header, body) = palm_doc(2, "正文".as_bytes());
        header[ENCRYPTION_AT..ENCRYPTION_AT + 2].copy_from_slice(&1u16.to_be_bytes());
        let path = db_at(dir.path(), "drm.pdb", "加密本", *b"TEXt", &[&header, &body]);

        let err = PdbAdapter.read(&path).unwrap_err();
        let crate::Error::Encrypted { name, method } = &err else {
            panic!("该是 Encrypted，实际 {err:?}");
        };
        assert_eq!(name, "加密本", "报错要报得出是哪一本");
        assert_eq!(method, "mobipocket-encryption-1", "哪一家 DRM 要看得见");
        assert_eq!(err.import_reject(), Some(ImportReject::Encrypted));
        // 判定层仍然认它是 PalmDOC：拒绝的话由 provider 说，而不是「这不是书」。
        assert!(is_palmdoc(&PalmDb::open(&path).unwrap()));
    }

    /// 正文是标记的那一族（TextReader 一类早期 PalmDOC）：不许把标记转义成一屏尖括号。
    ///
    /// 判据锚在**开头**，所以「整本里含 `<html` 的一篇纯文本小说」还是走纯文本那一条
    /// —— 反例那半条钉的就是这个，它比正例更容易被写坏（`contains` 一顺手就中）。
    #[test]
    fn a_markup_payload_is_handed_to_the_engine_unescaped() {
        let dir = tempfile::tempdir().unwrap();
        let doc = "<html><head><title>标记本</title></head><body><h1>第一章</h1><p>正文里写着 &lt;tag&gt;</p></body></html>";
        let (header, body) = palm_doc(1, doc.as_bytes());
        let path = db_at(dir.path(), "mark.pdb", "", *b"TEXt", &[&header, &body]);

        let (meta, content) = PdbAdapter.read(&path).unwrap();
        assert_eq!(meta.title, "标记本", "书名要从 <title> 取");
        let Content::Reflow(flow) = content else {
            panic!("该读成可重排");
        };
        assert_eq!(
            flow.chapters.len(),
            1,
            "标记那一条与单文件 HTML 同档：整篇一章"
        );
        let source = flow.chapters[0].source();
        assert!(source.contains("<p>正文"), "标记该原样交给引擎：{source}");
        assert!(
            !source.contains("&lt;p&gt;"),
            "按纯文本收会把标记转义成屏上看得见的一堆尖括号：{source}"
        );

        // 反例：正文提到 `<html` 的纯文本小说仍按纯文本切章。
        let plain = "第一章 开始\n\n他写道：<html> 就是那个标签\n\n第二章 继续\n\n正文二\n";
        let (header, body) = palm_doc(1, plain.as_bytes());
        let path = db_at(dir.path(), "essay.pdb", "", *b"TEXt", &[&header, &body]);
        let (_, content) = PdbAdapter.read(&path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("该读成可重排");
        };
        let first = super::super::visible_text(flow.chapters[0].source());
        assert!(
            first.contains("<html>"),
            "纯文本里的尖括号是**内容**：转义后屏上就该看得见，而标记那一族会把它当标签吃掉：{first}"
        );
    }

    /// 早期 Mobipocket（OEB1）没有 `EXTH`，书目写在正文头部那块 `<dc:*>` 里。
    ///
    /// 两条回落都在这一条里一起验：`uid` 那条 identifier **不许**当成 ISBN，
    /// 而 `<title>` / `<h1>` 那一条链只在 DC 块没说话时才轮到它。
    #[test]
    fn the_oeb1_dublin_core_block_fills_the_shelf_fields() {
        let dir = tempfile::tempdir().unwrap();
        let doc = concat!(
            "<HTML><HEAD><metadata><dc-metadata>",
            "<dc:Title>都柏林核心本</dc:Title>",
            "<dc:Creator>著者甲</dc:Creator><dc:Creator>著者乙</dc:Creator>",
            "<dc:Contributor>译者</dc:Contributor>",
            "<dc:Subject>科幻</dc:Subject><dc:Subject>测试</dc:Subject>",
            "<dc:Description>带<b>标签</b>的简介</dc:Description>",
            "<dc:Publisher>某社</dc:Publisher><dc:Date>1905</dc:Date>",
            "<dc:Language>en-us</dc:Language><dc:Rights>公版</dc:Rights>",
            "<dc:Identifier id=\"uid\">urn:uuid:1</dc:Identifier>",
            "<dc:Identifier scheme=\"ISBN\">9787000000000</dc:Identifier>",
            "</dc-metadata></metadata><title>网页标题</title></HEAD>",
            "<BODY><h1>正文大标题</h1><p>正文</p></BODY></HTML>",
        );
        let (header, body) = palm_doc(1, doc.as_bytes());
        let path = db_at(
            dir.path(),
            "oeb1.pdb",
            "容器名",
            *b"TEXt",
            &[&header, &body],
        );
        let (meta, _) = PdbAdapter.read(&path).unwrap();
        assert_eq!(meta.title, "都柏林核心本", "DC 那块要赢过 <title> 与容器名");
        assert_eq!(meta.authors, ["著者甲", "著者乙"], "多条 creator 都要收");
        assert_eq!(meta.contributors, ["译者"]);
        assert_eq!(meta.subjects, ["科幻", "测试"]);
        assert_eq!(meta.description, "带标签的简介", "简介要剥成纯文本");
        assert_eq!(meta.publisher, "某社");
        assert_eq!(meta.published, "1905");
        assert_eq!(meta.language, "en-us");
        assert_eq!(meta.rights, "公版");
        assert_eq!(meta.isbn, "9787000000000", "按 scheme 筛，uid 那条不算");

        // DC 块什么也没写时，回落链照旧：`<title>` → `<h1>` → 容器名。
        let bare = "<HTML><HEAD></HEAD><BODY><h1>只有大标题</h1><p>正文</p></BODY></HTML>";
        let (header, body) = palm_doc(1, bare.as_bytes());
        let path = db_at(
            dir.path(),
            "bare.pdb",
            "容器名",
            *b"TEXt",
            &[&header, &body],
        );
        assert_eq!(PdbAdapter.read(&path).unwrap().0.title, "只有大标题");
    }

    /// 真样本那一遍：`reference/libmobi/tests/samples` 里有两本 `TEXt`/`REAd` 的 PalmDOC
    /// （文件名后缀是 `.mobi`，容器**类型字段**说的才是它是什么 —— 判定层就按那一格走）。
    /// 它们教的正是上面三条：书名在数据库名那一格、正文可以是标记、加密那本要说「有 DRM」。
    ///
    /// 跑法：`cargo test -p folio-core --lib -- --ignored the_two_real_palmdoc`
    #[test]
    #[ignore = "要仓库外的 reference/libmobi 克隆（怎么拉见 reference/README.md）"]
    fn the_two_real_palmdoc_books_keep_their_three_facts() {
        let dir = Path::new("../../reference/libmobi/tests/samples");
        let text = dir.join("sample-textread.mobi");
        let drm = dir.join("sample-drm-v1.mobi");
        for path in [&text, &drm] {
            assert!(path.exists(), "缺样本：{}", path.display());
        }

        // 一本正文写着标记的 PalmDOC：书名 / 著者 / 出版者 / 语种都在 `<dc:*>` 那块里，
        // 标记不许转义（mobitool 报的 Title 就是那一格，两条路该给同一个答案）。
        let (meta, content) = PdbAdapter.read(&text).unwrap();
        assert_eq!(meta.title, "Libmobi test sample", "书名要从 DC 块取");
        assert_eq!(meta.authors, ["Bartek Fabiszewski"], "著者同样在块里");
        assert_eq!(meta.publisher, "Libmobi project");
        assert_eq!(meta.language, "en-us");
        let Content::Reflow(flow) = content else {
            panic!("该读成可重排");
        };
        assert_eq!(flow.toc.len(), 1, "标记那一族整篇一章，目录跟着章表");
        let body = flow.chapters[0].source();
        assert!(
            body.trim_start().to_ascii_lowercase().starts_with("<html"),
            "正文开头该是看得见的标记，不是转义后的一堆尖括号：{}",
            body.chars().take(80).collect::<String>()
        );
        assert!(
            !body.contains("&lt;html"),
            "转义就是把标记变成屏上看得见的字：{}",
            body.chars().take(80).collect::<String>()
        );

        // 一本加过密的：要说「有 DRM」，不是交一屏乱码。
        let err = PdbAdapter.read(&drm).unwrap_err();
        assert!(
            matches!(&err, crate::Error::Encrypted { method, .. } if method == "mobipocket-encryption-1"),
            "{err:?}"
        );
    }
}
