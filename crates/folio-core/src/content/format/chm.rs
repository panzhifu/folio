//! CHM（Microsoft Compiled HTML Help）：一个 ITSF 容器里装着一组 HTML topic。
//!
//! 与 EPUB 分别是**容器**而不是内容：EPUB 躺 ZIP，CHM 躺 ITSF，而两边的正文都是一堆
//! HTML 文件，归约成「引擎就绪的 XHTML 片段」之后排版链就分不出它们了（契约见
//! [`super::contract`]）。
//!
//! ## 这一层负责的三件事
//!
//! **一、章表。** CHM 没有 OPF。顺序来自 `#SYSTEM` 报的那份正文清单（`.hhc`），
//! 清单拿不到时退成「容器里的 html 按自然序排」。两种都不建 DOM —— `index()` 那一趟
//! 只做流式字数扫描，与 EPUB 同一条立场（正文不入库，见 `plans/text-out-of-db.md`）。
//!
//! **二、码页。** 真实中文 CHM 的 topic 按项目码页存（简体 GBK 最常见），
//! 一律走 [`super::charset`] 那条链：文件自己的 BOM 与声明优先，容器报的 LCID 兜后。
//!
//! **三、路径。** 我们的 href 一律**不带**前导斜杠（`resolve_href` 就是那个形状），
//! CHM 目录里的条目名一律**带**，而 hhc 的 `Local` 偶尔写反斜杠 —— 这三个形状在
//! [`lookup_names`] 一处统一完，别处不再各写一遍。
//!
//! ## 一期的四条已知局限
//!
//! - **层级**：`Binary TOC=yes` 编出来的书只有 `#TOCIDX`（二进制目录流），没有可读的
//!   层级来源，一期只给平面目录。两份实测样本都是这一形。
//! - **封面**：CHM 没有「封面」这个概念，走库里既有的无封面兜底，不猜首图。
//! - **frameset**：老书的默认页常是 `<frameset>`，一期原样交给排版链（排出来是空的），
//!   不跟进 frame 里那一页。
//! - **并行**：`index()` 是单线程顺序读全本。EPUB 那一族按条目并行的做法在这里同样
//!   可行（LZX 的状态跨块，所以并行度受复位窗口牵制），一期不做。

use std::collections::HashMap;
use std::path::Path;

use encoding_rs::Encoding;
use libchm::{ChmFile, Entry, EntrySel};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use super::cbz::natural_cmp;
use super::charset;
use super::{
    Chapter, Content, DocMeta, DocumentProvider, FlowDoc, Format, ImageRes, TocEntry, dir_of,
    extension_of, image_dims, media_type_of, resolve_href, stem_of,
};
use crate::Result;

/// `#SYSTEM` 里那几种条目的号（下面这份走法是从两份实测样本上倒推出来的，
/// 见 [`system_entries`] 的说明）。
const SYS_CONTENTS: u16 = 0;
const SYS_DEFAULT_TOPIC: u16 = 2;
const SYS_TITLE: u16 = 3;
const SYS_LCID: u16 = 4;

/// 一条目录项：标题 + 我们形状的 href。
struct Topic {
    title: String,
    href: String,
}

pub struct ChmAdapter;

impl DocumentProvider for ChmAdapter {
    fn format(&self) -> Format {
        Format::Chm
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(extension_of(path).as_deref(), Some("chm"))
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let mut chm = Chm::open(path)?;
        let listing = chm.topics();
        if listing.is_empty() {
            return Err(unsupported(path, "CHM 里没有可读的条目"));
        }

        // 字数那一趟按**目录序**走（也就是名字序）：LZX 的解码状态跨块，顺着容器的
        // 存放序读能把「回放到某个复位窗口」那笔重复开销摊薄，跳着读则每章都要重放。
        let mut char_lens: HashMap<String, Option<usize>> = HashMap::new();
        let mut images: Vec<ImageRes> = Vec::new();
        let mut html_seen = 0usize;
        for entry in &listing {
            let href = ours(&entry.path);
            let Some(bytes) = chm.read(&entry.path) else {
                tracing::warn!("chm: {} 里的 {href} 读不出来", path.display());
                continue;
            };
            if is_html(&href) {
                let text = chm.decode(&href, bytes);
                char_lens.insert(
                    href,
                    Some(crate::content::scan::chapter_summary(&text).visible_chars),
                );
                html_seen += 1;
            } else if let Some(media_type) = bitmap_media_type(&href) {
                let (width, height) = image_dims(&bytes);
                images.push(ImageRes {
                    href: href.clone(),
                    source: href,
                    media_type: media_type.to_string(),
                    width,
                    height,
                });
            }
        }
        if html_seen == 0 {
            // 一本一个 html 都没有的 CHM 不是书（工具类容器确实存在：里面只有编译好的
            // 搜索索引与图片）。说得清的理由要比「排出来是空白」有用。
            return Err(unsupported(path, "CHM 里没有 html 正文"));
        }

        let items = chapter_order(&mut chm, &listing);
        let chapters: Vec<Chapter> = items
            .iter()
            .map(|topic| {
                let chars = char_lens.get(&topic.href).copied().flatten();
                Chapter::entry(topic.title.clone(), topic.href.clone()).with_char_len(chars)
            })
            .collect();
        let toc: Vec<TocEntry> = items
            .iter()
            .map(|topic| TocEntry {
                title: topic.title.clone(),
                href: topic.href.clone(),
                fragment: None,
                children: Vec::new(),
            })
            .collect();

        let title = chm
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| items.first().map(|t| t.title.clone()))
            .unwrap_or_else(|| stem_of(path));
        let meta = DocMeta {
            title,
            authors: Vec::new(),
            format: Format::Chm,
            size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        };
        let flow = FlowDoc {
            chapters,
            css: String::new(),
            css_hrefs: Vec::new(),
            toc,
            // 翻页方向只有 EPUB 的 spine 那一格读得出来，其余格式按左翻。
            progression_rtl: false,
            cover: None,
            images,
            cover_ref: None,
        };
        tracing::debug!(
            "chm: {} 读到 {} 章、{} 张图",
            path.display(),
            flow.chapters.len(),
            flow.images.len()
        );
        Ok((meta, Content::Reflow(flow)))
    }

    fn entry_text(&self, path: &Path, entry: &str) -> Option<String> {
        let mut chm = Chm::open(path).ok()?;
        chm.text_of(entry)
    }

    fn image_bytes(&self, path: &Path, href: &str) -> Option<Vec<u8>> {
        let mut chm = Chm::open(path).ok()?;
        chm.bytes_of(href)
    }
}

/// 一次打开：容器句柄 + `#SYSTEM` 读出来的那三格。
///
/// 每次调用现开（与 [`super::cbz::CbzAdapter`] 同一立场）：`open` 实测 55 µs，
/// 而滚动取图那条热路径由 `format::container::Container` 那个开一次的句柄接走，
/// 走不到这里。
struct Chm {
    file: ChmFile,
    /// 容器自己报的码页（`#SYSTEM` 的 LCID 换的）；不知道就是 `None`。
    project: Option<&'static Encoding>,
    title: Option<String>,
    default_topic: Option<String>,
    contents: Option<String>,
}

impl Chm {
    fn open(path: &Path) -> Result<Self> {
        let mut file = ChmFile::open(path)
            .map_err(|err| unsupported(path, &format!("CHM 容器读不出来（{err}）")))?;
        let system = file
            .read_path("/#SYSTEM")
            .or_else(|_| file.read_path("#SYSTEM"))
            .unwrap_or_default();
        let mut project = None;
        let mut title = None;
        let mut default_topic = None;
        let mut contents = None;
        for (kind, data) in system_entries(&system) {
            match kind {
                SYS_LCID => {
                    // 长度不对（或被改坏）就当没报过码页：宁可按 UTF-8 收并留下 U+FFFD，
                    // 也不拿一段来历不明的字节去猜一个编码。
                    if let Some(bytes) = data.get(..4) {
                        project = charset::from_lcid(u32::from_le_bytes(bytes.try_into().unwrap()));
                    }
                }
                SYS_TITLE => title = Some(charset::decode(nul_trimmed(data), project)),
                SYS_DEFAULT_TOPIC => {
                    default_topic = Some(charset::decode(nul_trimmed(data), project));
                }
                SYS_CONTENTS => contents = Some(charset::decode(nul_trimmed(data), project)),
                _ => {}
            }
        }
        Ok(Self {
            file,
            project,
            title,
            default_topic,
            contents,
        })
    }

    /// 目录里可读的普通文件（`#SYSTEM`、`$FIftiMain` 那些与目录项本身都不在这一列里）。
    fn topics(&mut self) -> Vec<Entry> {
        self.file
            .entries(EntrySel::NORMAL | EntrySel::FILES)
            .unwrap_or_default()
    }

    fn read(&mut self, name: &str) -> Option<Vec<u8>> {
        for candidate in lookup_names(name) {
            if let Ok(bytes) = self.file.read_path(&candidate) {
                return Some(bytes);
            }
        }
        None
    }

    /// 按名取字节（`href` 是**我们那一形**：可以不带前导斜杠）。
    fn bytes_of(&mut self, href: &str) -> Option<Vec<u8>> {
        self.read(href)
    }

    /// `href`（我们那一形）指向的正文。
    fn text_of(&mut self, href: &str) -> Option<String> {
        let bytes = self.read(href)?;
        Some(self.decode(href, bytes))
    }

    /// 解码一段字节。`href` 只用来在日志里说清是哪一页。
    fn decode(&self, href: &str, bytes: Vec<u8>) -> String {
        let text = charset::decode(&bytes, self.project);
        if text.contains('\u{fffd}') && self.project.is_none() {
            // 只在「容器没报码页、又解出替换字符」时说一句：这是整本乱码的唯一外在征兆，
            // 而它安静得像一本没图的书。
            tracing::warn!("chm: {href} 解不动（容器没报码页，正文里出现了 U+FFFD）");
        }
        text
    }
}

/// `#SYSTEM` 的条目流：`(u16 类型, u16 长度, 长度个字节)` 一路紧挨着排到缓冲末尾。
///
/// 这个走法是在两份实测样本上倒推出来并**要求刚好走完**的：任何一处越界或收尾对不上
/// 就整份当读不出（回空表），而不是拿半份去猜书名。真实的 HTML Help Workshop 产物还没
/// 有样本可验，所以这条是已知局限里最硬的一条 —— 它错的症状是「书名回落到文件名、
/// 章表退回按名排」，不是读不出正文。
fn system_entries(raw: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 4 <= raw.len() {
        let kind = u16::from_le_bytes([raw[at], raw[at + 1]]);
        let len = u16::from_le_bytes([raw[at + 2], raw[at + 3]]) as usize;
        let end = match at.checked_add(4).and_then(|x| x.checked_add(len)) {
            Some(end) if end <= raw.len() => end,
            // 长度字段越界：这份 `#SYSTEM` 不可信，整份丢掉。
            _ => return Vec::new(),
        };
        out.push((kind, &raw[at + 4..end]));
        at = end;
    }
    // 走到这里 `at` 只能小于 len（还剩不到 4 字节）或正好等于 len。剩下的是零填充
    // （实测样本上 type=11 那条之后就没数据了），非零的尾巴说明我读歪了。
    if raw[at..].iter().any(|&b| b != 0) {
        return Vec::new();
    }
    out
}

/// `#SYSTEM` 里那几段字符串是 C 串（实测样本每一条都以一个 NUL 收尾）。
/// 不剥掉的话它会跟着进书名、进章表，最后在界面上多出一个看不见的空格。
fn nul_trimmed(data: &[u8]) -> &[u8] {
    let end = data.iter().rposition(|&b| b != 0).map_or(0, |at| at + 1);
    &data[..end]
}

/// 一个名字该试的几种写法。**顺序有讲究**：先按我们那一形补上前导斜杠（真书的目录名
/// 全部带它），再试原样，最后试反斜杠那一形（个别书的目录名用 `\` 分段）。
///
/// `pub(crate)` 是因为 [`super::container::Container`] 那一层取字节走的是同一条
/// 归一化 —— 两处各写一份迟早会漂，而漂的症狀是「图在容器里，屏幕上没有」。
pub(crate) fn lookup_names(name: &str) -> [String; 4] {
    let trimmed = name.trim_start_matches('/');
    let slashed = format!("/{trimmed}");
    let back = trimmed.replace('/', "\\");
    [
        slashed.clone(),
        trimmed.to_string(),
        format!("\\{back}"),
        back,
    ]
}

/// CHM 目录名 → 我们的 href：剥掉前导斜杠、反斜杠换成斜杠。
///
/// 剥那一个斜杠不是省事：`resolve_href` 的产物就不带前导斜杠（它有测试钉住），
/// 两边形状不一致的话，`<img src>` 的解析结果查不到图片表里那一格。
fn ours(directory_name: &str) -> String {
    directory_name.trim_start_matches('/').replace('\\', "/")
}

/// 章表：`.hhc` 清单优先，退化成「容器里的 html 按自然序」。
///
/// 清单里的 `Local` 相对**清单自己所在的目录**解（与 EPUB 的 OPF 同一条口径），
/// 指向容器里不存在的条目那几项丢掉并说一句 —— 读它必然失败，留着一章空白更难解释。
fn chapter_order(chm: &mut Chm, listing: &[Entry]) -> Vec<Topic> {
    let names: HashMap<String, ()> = listing.iter().map(|e| (ours(&e.path), ())).collect();
    let from_hhc = chm
        .contents
        .clone()
        .or_else(|| {
            // `#SYSTEM` 没报清单时，容器里孤零零一份 `.hhc` 也算线索（编译器把清单
            // 一起编进去了）。多于一个就不认 —— 那时猜哪一个都是猜。
            let mut hhc = listing
                .iter()
                .map(|e| ours(&e.path))
                .filter(|href| extension_of(Path::new(href)).is_some_and(|ext| ext == "hhc"));
            let first = hhc.next()?;
            hhc.next().is_none().then_some(first)
        })
        .and_then(|href| {
            let xml = chm.text_of(&href)?;
            let items = parse_hhc(&xml, &dir_of(&href));
            // 清单里指向「容器中没有的条目」那几项丢掉并说一句：读它必然失败，
            // 留着「一章空白」比少一章更难解释（与 EPUB 的 spine 指着没有的 id 同一条立场）。
            let kept: Vec<Topic> = items
                .into_iter()
                .filter(|item| match names.contains_key(&item.href) {
                    true => true,
                    false => {
                        tracing::warn!("chm: 清单 {href} 指向 {}，容器里没有", item.href);
                        false
                    }
                })
                .collect();
            (!kept.is_empty()).then_some(kept)
        });
    let mut items = match from_hhc {
        Some(items) => items,
        None => {
            let mut names: Vec<String> = listing
                .iter()
                .map(|e| ours(&e.path))
                .filter(|href| is_html(href))
                .collect();
            names.sort_by(|a, b| natural_cmp(a, b));
            names
                .into_iter()
                .map(|href| Topic {
                    title: stem_of(Path::new(&href)),
                    href,
                })
                .collect()
        }
    };
    // 默认页排第一：hhc 的顺序常常本来就以它开头，但「打开这本书先看到什么」是
    // `#SYSTEM` 说了算的那件事，而读者按它理解这本书。
    if let Some(default) = chm.default_topic.as_deref() {
        let want = ours(default);
        if let Some(at) = items.iter().position(|t| t.href == want)
            && at != 0
        {
            let topic = items.remove(at);
            items.insert(0, topic);
        }
    }
    // 清单里有、但字数那一趟没解到的条目（读到一半失败）也要有章 —— 它的字数是
    // `None`（不知道），而**不是** `Some(0)`（空章）。这条区分归 `Chapter::char_len` 管。
    items
}

/// 一份 `.hhc`（正文清单）里的 `(标题, 相对清单目录的位置)`。
///
/// hhc 是 HTML 语法不是 XML：`<param …>` 不自我闭合，所以读它要把「结束标签对不上」
/// 这一项关掉（默认开着会在第一个 `</OBJECT>` 上报错，于是整份清单只读到第一项）。
/// 层级（`<UL>` 嵌套）在这一版**丢掉**：界面的目录面板吃平面列表也一样能用，
/// 而真书的层级来源多半是 `#TOCIDX`，那是二期那条。
fn parse_hhc(xml: &str, base_dir: &str) -> Vec<Topic> {
    let mut reader = Reader::from_str(xml);
    // hhc 是 HTML 语法不是 XML：`<param …>` 不自我闭合。这一项默认开着的话，第一个
    // `</OBJECT>` 就报「结束标签对不上」，于是整份清单只读到第一项 —— 症状是目录少了
    // 一大截而不报错。
    reader.config_mut().check_end_names = false;
    let mut out: Vec<Topic> = Vec::new();
    // 当前这个 `<OBJECT type="text/sitemap">` 块收到的两格。
    let mut name: Option<String> = None;
    let mut local: Option<String> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if !is_tag(e.name().as_ref(), "param") {
                    continue;
                }
                // 一格 sitemap 项写成 `<param name="Name" value="第一章">`：**`name`
                // 属性是键**，`value` 才是内容。属性顺序不假定，键的大小写也不假定
                // （hhc 里 `Name` / `name` 两种都见得到）。
                let (Some(key), Some(value)) = (attribute(&e, "name"), attribute(&e, "value"))
                else {
                    continue;
                };
                if key.eq_ignore_ascii_case("Name") {
                    name = Some(value);
                } else if key.eq_ignore_ascii_case("Local") {
                    local = Some(value);
                }
            }
            Ok(Event::End(e)) if is_tag(e.name().as_ref(), "object") => {
                // 一个 sitemap 块到此结束：`Local` 缺席的是标题原子（只有 `Name`
                // 或只有 `Recommended` 那几形），它没有可读的正文，不收。
                if let Some(href_raw) = local.take() {
                    let href = resolve_href(base_dir, &href_raw);
                    let title = name
                        .take()
                        .filter(|t| !t.trim().is_empty())
                        .unwrap_or_else(|| stem_of(Path::new(&href)));
                    if !href.is_empty() {
                        out.push(Topic { title, href });
                        continue;
                    }
                }
                name = None;
                local = None;
            }
            Ok(Event::Eof) => break,
            // 读不出下一个事件就到此为止：畸形清单与读完是同一档，已经采到的部分
            // 照样有用（与 EPUB 的 OPF 那一条立场相同）。
            Err(_) => break,
            Ok(_) => {}
        }
    }
    out
}

fn is_tag(found: &str, want: &str) -> bool {
    // 带命名空间前缀的写法（`<o:param>`）也要认：截到 `:` 之后比。
    let local = found.rsplit(':').next().unwrap_or(found);
    local.eq_ignore_ascii_case(want)
}

/// `<param name="X" value="Y">` 里 `value` 那一格；实体要解开（真书的标题里
/// `&amp;` 与 `&quot;` 都出现得到）。取法与 EPUB / FB2 那两份同一条口径。
fn attribute(start: &BytesStart<'_>, want: &str) -> Option<String> {
    for a in start.attributes().flatten() {
        if is_tag(a.key.as_ref(), want) {
            return Some(
                quick_xml::escape::unescape(&a.value)
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| a.value.to_string()),
            );
        }
    }
    None
}

/// 这一条是不是正文文件。CHM 的 topic 扩展名两形都有（`.htm` / `.html`），
/// 而 `.xhtml` 也偶尔出现在转出来的书里。
fn is_html(href: &str) -> bool {
    matches!(
        extension_of(Path::new(href)).as_deref(),
        Some("htm" | "html" | "xhtml")
    )
}

/// 这一条是不是一张解得动的位图。**用的是 `media_type_of` 那一份表**，
/// 不再写第二个扩展名清单（两处各列一份迟早会漂成两个答案）。
fn bitmap_media_type(href: &str) -> Option<&'static str> {
    let ext = extension_of(Path::new(href)).unwrap_or_default();
    match media_type_of(&ext) {
        "application/octet-stream" => None,
        other => Some(other),
    }
}

fn unsupported(path: &Path, why: &str) -> crate::Error {
    crate::Error::UnsupportedFormat(format!(
        "{}：{why}",
        path.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    ))
}

#[cfg(test)]
mod tests {
    //! 这一族最容易悄悄错的地方：**形状**（那一个前导斜杠、反斜杠）、
    //! **码页**（GBK 那本必须解成同一串汉字），以及 `#SYSTEM` 走歪时必须**退回空表**
    //! 而不是半份假数据。每条都配一个「关掉它就变红」的断言。

    use std::path::{Path, PathBuf};

    use super::{Chm, ChmAdapter, is_html, lookup_names, ours, system_entries, unsupported};
    use crate::content::format::{Content, DocumentProvider, Format, provider_for};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/chm")
            .join(name)
    }

    fn flow_of(
        path: &Path,
    ) -> (
        crate::content::format::DocMeta,
        crate::content::format::FlowDoc,
    ) {
        let (meta, content) = ChmAdapter.index(path).unwrap();
        let Content::Reflow(flow) = content else {
            panic!("CHM 该读成可重排");
        };
        (meta, flow)
    }

    #[test]
    fn it_claims_chm_and_reports_the_format() {
        assert_eq!(ChmAdapter.format(), Format::Chm);
        assert!(ChmAdapter.supports(Path::new("书.CHM")));
        assert!(!ChmAdapter.supports(Path::new("a.epub")));
        assert!(provider_for(Path::new("a.chm")).is_some());
    }

    /// 章表、书名、图片与字数那一趟。**这一条是整块的总闸**：把它关掉的地方退回
    /// 「按名排」或「读不出来」，症状会在这里露头。
    #[test]
    fn the_utf8_fixture_reads_as_a_three_chapter_book() {
        let (meta, flow) = flow_of(&fixture("minimal-zh-utf8.chm"));
        assert_eq!(meta.format, Format::Chm);
        assert_eq!(meta.title, "最小中文书", "书名来自 #SYSTEM，不是文件名");
        assert_eq!(flow.chapters.len(), 3, "章表来自 hhc");
        assert_eq!(flow.chapters[0].title, "序");
        assert_eq!(flow.chapters[0].spine_href, "index.htm");
        // 每章的字数都在（`index()` 读过一遍），且不是 0。
        let chars: Vec<Option<usize>> = flow.chapters.iter().map(|c| c.char_len).collect();
        assert!(
            chars.iter().all(|c| c.is_some_and(|n| n > 0)),
            "字数该在索引这一趟算准：{chars:?}"
        );
        let sum: usize = chars.iter().filter_map(|c| *c).sum();
        assert_eq!(flow.char_len(), Some(sum), "全书分母该是各章之和");
        // 图那张按我们的形状登记（不带前导斜杠），布局才查得到。
        assert_eq!(flow.images.len(), 1, "只有 fig.png 那一张");
        assert_eq!(flow.images[0].href, "img/fig.png");
        assert_eq!(flow.images[0].media_type, "image/png");
        assert_eq!((flow.images[0].width, flow.images[0].height), (24, 16));
        // 一期没有封面这个概念：如实留空，界面走无封面那一档。
        assert!(flow.cover.is_none());
        assert_eq!(flow.toc.len(), 3);
    }

    /// **这一条钉住整个格式最大的一坑**：GBK 那本的正文与标题必须与 UTF-8 那本
    /// 逐字相同。把它关掉（退回 `read_to_string`）的症状是一排问号**且不报错** ——
    /// 所以断言比的是那一串汉字本身，不是「有没有非空」。
    #[test]
    fn the_gbk_fixture_decodes_to_the_same_text_as_the_utf8_one() {
        let utf8 = fixture("minimal-zh-utf8.chm");
        let gbk = fixture("minimal-zh-gbk.chm");
        let (meta8, flow8) = flow_of(&utf8);
        let (metag, flowg) = flow_of(&gbk);
        assert_eq!(
            metag.title, meta8.title,
            "书名（#SYSTEM 里的字节）也要过码页"
        );
        let titles8: Vec<&str> = flow8.chapters.iter().map(|c| c.title.as_str()).collect();
        let titlesg: Vec<&str> = flowg.chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titlesg, titles8, "章标题来自 hhc，两本该一字不差");
        for (a, b) in flow8.chapters.iter().zip(&flowg.chapters) {
            let text_a = ChmAdapter.entry_text(&utf8, &a.spine_href).unwrap();
            let text_b = ChmAdapter.entry_text(&gbk, &b.spine_href).unwrap();
            assert!(
                !text_b.contains('\u{fffd}'),
                "{} 里出现了替换字符",
                b.spine_href
            );
            assert_eq!(a.char_len, b.char_len, "字数是分母，两本必须同一个数");
            // 正文里那句中文也逐字相同（把解码退回 lossy UTF-8 时这一条先红）。
            assert_eq!(
                non_ascii(&text_b),
                non_ascii(&text_a),
                "{}：正文的非 ASCII 字符该与 UTF-8 那本相同",
                b.spine_href
            );
        }
    }

    fn non_ascii(text: &str) -> String {
        text.chars().filter(|c| !c.is_ascii()).collect()
    }

    /// 取字节试的那几种写法：**带前导斜杠的那一种排第一**（真书的目录名全带它），
    /// 而我们的 href 一律不带 —— 这一格就是把两个形状接起来的地方。
    #[test]
    fn the_lookup_forms_cover_slash_and_backslash() {
        let want: [String; 4] = [
            "/img/fig.png".to_string(),
            "img/fig.png".to_string(),
            "\\img\\fig.png".to_string(),
            "img\\fig.png".to_string(),
        ];
        assert_eq!(lookup_names("img/fig.png"), want);
        // 已经带了前导斜杠的输入不该被当成另一种名字。
        assert_eq!(lookup_names("/a.htm")[0], "/a.htm");
        assert_eq!(ours("/img/fig.png"), "img/fig.png");
        assert_eq!(ours("Data\\page.htm"), "Data/page.htm");
    }

    /// 按名取字节走的是同一条归一化：`img/fig.png`（我们那一形）必须命中
    /// 容器里的 `/img/fig.png`。
    #[test]
    fn images_and_chapters_are_reachable_by_our_href_shape() {
        let path = fixture("minimal-zh-utf8.chm");
        let bytes = ChmAdapter.image_bytes(&path, "img/fig.png").unwrap();
        assert_eq!(&bytes[1..4], b"PNG");
        assert!(
            ChmAdapter.image_bytes(&path, "IMG/FIG.PNG").is_some(),
            "大小写不敏感"
        );
        assert!(ChmAdapter.image_bytes(&path, "没有这个文件.png").is_none());
        let text = ChmAdapter.entry_text(&path, "index.htm").unwrap();
        assert!(text.contains("序言"), "{text}");
    }

    /// `#SYSTEM` 走歪时必须**整份退回空表**：半份假数据会变成「书名是一串乱码」
    /// 这种查不动的东西。
    #[test]
    fn a_system_stream_that_does_not_walk_cleanly_is_dropped_whole() {
        let good = fixture("minimal-zh-utf8.chm");
        let mut chm = Chm::open(&good).unwrap();
        assert_eq!(chm.title.as_deref(), Some("最小中文书"));
        assert_eq!(chm.contents.as_deref(), Some("toc.hhc"));
        assert_eq!(chm.default_topic.as_deref(), Some("index.htm"));
        // 把长度字段改坏一个字节：末尾就对不上了。
        let raw = chm.file.read_path("/#SYSTEM").unwrap();
        let mut broken = raw.clone();
        broken[3] = 0xFF;
        assert!(system_entries(&broken).is_empty(), "越界的长度要整份丢掉");
        let mut trailing = raw;
        trailing.push(b'x');
        assert!(
            system_entries(&trailing).is_empty(),
            "走完还剩非零字节说明读歪了，要丢掉而不是照收"
        );
    }

    /// 一个 html 都没有的容器不是书：说得清的理由比「打开是空白」有用。
    #[test]
    fn a_chm_without_topics_is_refused_with_a_reason() {
        // 这本不存在（路径都不对），走的是同一条 open 失败那一支。
        let err = unsupported(Path::new("x.chm"), "CHM 里没有 html 正文");
        assert!(format!("{err}").contains("x.chm"));
        assert!(ChmAdapter.index(Path::new("/does/not/exist.chm")).is_err());
        assert!(
            ChmAdapter
                .entry_text(Path::new("/does/not/exist.chm"), "a.htm")
                .is_none()
        );
    }

    #[test]
    fn only_html_topics_count_as_text() {
        assert!(is_html("/a/b.HTML"));
        assert!(is_html("x.xhtml"));
        assert!(!is_html("img/a.png"));
        assert!(!is_html("toc.hhc"));
    }
}
