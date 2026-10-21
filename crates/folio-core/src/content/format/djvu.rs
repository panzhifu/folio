//! DjVu：扫描书 / 漫画的定版式格式。
//!
//! 一页是既定的画布，和 PDF 同族：页表里存 `PageSource::Render`，真正的像素由
//! [`DocumentProvider::render_page`] 现画。解码器是纯 Rust 的 `djvu-rs`（MIT，无 GPL），
//! 不引入 DjVuLibre 的 C 库。
//!
//! `render_page` 每次重开文档 —— 与 PDF 同一条取舍：没有全局句柄表，代价是重读文件
//! 字节，换来的是 `Document` 可以跨线程、不需要清理生命周期。画一页的尺寸由调用方定
//! 长边，`RenderOptions::fit_to_box` 保住比例并处理页内 `INFO` 旋转。
//!
//! 两种容器形状都读：**bundled**（自包含的 `FORM:DJVM`）与 **indirect**（索引 + 一页一个
//! 外部文件）—— 入口走 `open_dir`。改法的理由与那条「本机没有一份真 indirect 样本」的
//! 诚实记录都在 [`open_document`] 上。
//!
//! 接上的两格：NAVM 书签 → 目录（[`bookmarks`]，落点是页号）与 `(metadata …)` 那块 →
//! 书目（[`shelf`]）。书签那份 71 页的公版 spec 样本实测 100 条，两种 url 写法各占
//! 29 / 69（`#64` 是 **1 起**的页号；`#p0064.djvu` 要按 DIRM 里 `P` 条目的顺序换页号），
//! 而**不猜组件名里的数字**：真样本里第一页可以叫 `p0001_1.djvu`，另一本 Internet Archive
//! 的页组件从 `_0000` 起，按数字减一在后者上会整整差一页。
//!
//! 书目那一格的门是**合成**的：手边四份真 DjVu（含两本 Internet Archive 公版扫描）
//! `metadata()` 全回 `None` —— IA 那一族的元数据写在旁边的 `.meta` 文件里，不在 DjVu 内。
//! 所以那条门拿上游编码器（`djvu_mut::set_metadata`）往真样本上加一块再读回来：钉的是
//! 「我们怎么把那一块落到书目的格子里」，不是「那一块的字节长什么样」。
//!
use std::path::Path;

use djvu_rs::Document;

use super::pdf::split_keywords;
use super::{
    Content, DocMeta, DocumentProvider, FixedDoc, FixedPage, Format, PageChar, PageRect,
    PageSource, PageText, TocEntry, stem_of,
};
use crate::Result;

/// 封面缩略图长边，与 EPUB / FB2 / CBZ / PDF 同一口径。
const COVER_EDGE: u32 = 512;

pub struct DjvuAdapter;

impl DocumentProvider for DjvuAdapter {
    fn format(&self) -> Format {
        Format::Djvu
    }

    fn supports(&self, path: &Path) -> bool {
        // `.djv` 不是另一种格式：DjVu 的间接文档习惯把索引叫 `.djv`、单页的叫 `.djvu`，
        // 而解码器不看后缀。**两个名字指向同一族内容**，只认一个的结果是读者那一整批书
        // 在扫描与拖进来的清单里凭空看不见。
        matches!(super::extension_of(path).as_deref(), Some("djvu" | "djv"))
    }

    fn index(&self, path: &Path) -> Result<(DocMeta, Content)> {
        let document = open_document(path)?;
        let pages = document.page_count();
        if pages == 0 {
            return Err(unsupported_text(path, "这份 DjVu 一页都没有"));
        }

        let mut table = Vec::with_capacity(pages);
        for index in 0..pages {
            let page = document
                .page(index)
                .map_err(|err| unsupported(path, &err))?;
            table.push(FixedPage {
                source: PageSource::Render {
                    index: index as u32,
                },
                width: page.display_width(),
                height: page.display_height(),
                // DjVu 的一页是合成好的整页，没有「主图之外再摆一张」这一档。
                extra_images: Vec::new(),
            });
        }
        let (width, height) = table
            .iter()
            .find(|page| page.width > 0 && page.height > 0)
            .map(|page| (page.width, page.height))
            .unwrap_or((0, 0));
        let cover = document.page(0).ok().and_then(|page| render_cover(&page));

        let fixed = FixedDoc {
            pages: table,
            toc: bookmarks(&document, pages),
            cover,
            cover_ref: None,
            images: Vec::new(),
            width,
            height,
        };
        tracing::debug!("djvu: {} 读到 {} 页", path.display(), fixed.page_count());
        Ok((shelf(path, &document), Content::Fixed(fixed)))
    }

    fn render_page(&self, path: &Path, page: usize, edge: u32) -> Option<Vec<u8>> {
        let document = open_document(path).ok()?;
        let page = document.page(page).ok()?;
        let (width, height) = fit_size(&page, edge);
        let pixmap = page.render_to_size(width, height).ok()?;
        encode_png(&pixmap)
    }

    /// 这一页的文字层（`TXTz` / `TXTa` 那一块；扫描本是 OCR 的产物）。
    ///
    /// 与 PDF 那一族差两件事，都在 [`page_text`] 里：单位是 DjVu 的**原生像素**而不是 pt，
    /// 而盒已经是 y 朝下（引擎在解那一段时就按页高翻过一次）。
    fn page_text(&self, path: &Path, page: usize) -> Option<PageText> {
        let document = open_document(path).ok()?;
        let page = document.page(page).ok()?;
        page_text(&page)
    }
}

/// 一页 DjVu → 文字层。PDF 那一路交 pt 与翻过的 y，这里交原生像素与**没翻**的 y
/// （`djvu_rs::Rect` 的文档写明 top-left origin，而它的 `parse_text_layer` 收了页高做翻转）
/// —— `PageText` 带着自己的尺寸，所以两套单位在绘制侧是同一把尺。
///
/// **逐字盒这一族没有**（zone 只到词级）：一个词里的字按词盒均分。这一格近似写在这里
/// 而不是藏在算法里 —— 选中一个词的一部分时，色块边界与实际字形差半个到一个字宽，
/// 而 OCR 本来的盒就不齐。
pub(crate) fn page_text(page: &djvu_rs::Page<'_>) -> Option<PageText> {
    let layer = page.text_layer().ok()??;
    let width = page.width() as f32;
    let height = page.height() as f32;
    let mut chars: Vec<PageChar> = Vec::new();
    let mut counters = Walk {
        line: 0,
        chars: &mut chars,
    };
    for zone in &layer.zones {
        collect(zone, &mut counters);
    }
    if chars.is_empty() {
        // 层级不是 `Page → … → Line → Word` 那一种（或这一页根本没字）：
        // 至少**不丢字** —— 整页一行，盒就是页本身。选中能拿到全部的字，只是色块铺满一页。
        return (!layer.text.is_empty()).then(|| PageText {
            width,
            height,
            chars: uniform(
                &layer.text,
                PageRect {
                    x: 0.0,
                    y: 0.0,
                    width,
                    height,
                },
                0,
            ),
        });
    }
    Some(PageText {
        width,
        height,
        chars,
    })
}

struct Walk<'a> {
    line: u32,
    chars: &'a mut Vec<PageChar>,
}

/// 往下走 zone。**一行**的定义：`Line` 那一种，或（生产者没写 Line 层时）直接挂着词的
/// 那一格。词的盒均分给它的字。
fn collect(zone: &djvu_rs::TextZone, out: &mut Walk<'_>) {
    let words: Vec<&djvu_rs::TextZone> = zone
        .children
        .iter()
        .filter(|child| child.kind == djvu_rs::TextZoneKind::Word)
        .collect();
    let has_lines = zone
        .children
        .iter()
        .any(|child| child.kind == djvu_rs::TextZoneKind::Line);
    if has_lines || (!words.is_empty() && zone.kind != djvu_rs::TextZoneKind::Word) {
        if has_lines {
            for child in &zone.children {
                collect(child, out);
            }
            return;
        }
        // 这一格就是「一行」：它挂着词，自己没有 Line 孩子。
        out.line += 1;
        let line = out.line - 1;
        if words.is_empty() {
            push_word(zone, line, out);
            return;
        }
        for word in words {
            push_word(word, line, out);
        }
        return;
    }
    for child in &zone.children {
        collect(child, out);
    }
}

fn push_word(zone: &djvu_rs::TextZone, line: u32, out: &mut Walk<'_>) {
    let rect = PageRect {
        x: zone.rect.x as f32,
        y: zone.rect.y as f32,
        width: zone.rect.width as f32,
        height: zone.rect.height as f32,
    };
    out.chars.extend(uniform(&zone.text, rect, line));
}

/// 一个盒摊给一串字（每个字等宽）。
fn uniform(text: &str, rect: PageRect, line: u32) -> Vec<PageChar> {
    let chars: Vec<char> = text.chars().collect();
    let count = chars.len() as f32;
    if count == 0.0 {
        return Vec::new();
    }
    let step = rect.width / count;
    chars
        .into_iter()
        .enumerate()
        .map(|(index, ch)| PageChar {
            ch,
            rect: PageRect {
                x: rect.x + step * index as f32,
                y: rect.y,
                width: step,
                height: rect.height,
            },
            line,
        })
        .collect()
}

fn render_cover(page: &djvu_rs::Page<'_>) -> Option<Vec<u8>> {
    let (width, height) = fit_size(page, COVER_EDGE);
    let pixmap = page.render_to_size(width, height).ok()?;
    encode_png(&pixmap)
}

/// 打开一份 DjVu。
///
/// 走 `open_dir` 而不是 `open`：上游那句文档注释写得很清楚 —— `open` 只吃 **bundled**
/// （自包含的 `FORM:DJVM`），indirect（索引 + 一页一个外部文件）要 `open_dir`。而
/// `open_dir` 对 bundled 同样给得出正确答案（两份真样本各开一次，页数一致），所以一条路
/// covers 两种形状，不必先探一种再退回另一种。
///
/// **已知没验到的一格**：本机没有一份真的 indirect DjVu（`djvu encode` 与 `split` 都只
/// 产 bundled，手边四份真样本也都是），所以这一条改法的 indirect 那半边是按上游文档 +
/// `open_dir` 在 bundled 上的实测推的，不是撞到的。撞到之前别当成已验过。
fn open_document(path: &Path) -> Result<Document> {
    Document::open_dir(path).map_err(|err| unsupported(path, &err))
}

/// 文件自带的书目（`(metadata …)` 那一块，也就是 ANTT / AGINT）→ 书目的那几格。
///
/// 书名优先给块里那一条，缺了才退文件名主名 —— 与 [`super::azw4`]（PalmDB 的数据库名）、
/// [`super::pdf`]（`/Title`）同一条次序：**文档自己说的 > 容器给的名字 > 文件名**。
fn shelf(path: &Path, document: &Document) -> DocMeta {
    let mut meta = DocMeta {
        title: stem_of(path),
        format: Format::Djvu,
        size: std::fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
        ..DocMeta::default()
    };
    // `metadata()` 解不动（那一块是坏的）就当没有：这一格是装饰性的，与 `fonts` /
    // `image_bytes` 同一立场 —— 读不到不等于这本书读不动。
    let Ok(Some(core)) = document.inner().metadata() else {
        return meta;
    };
    let keep = |text: Option<String>| {
        text.map(|text| text.trim().to_string())
            .filter(|t| !t.is_empty())
    };
    if let Some(title) = keep(core.title) {
        meta.title = title;
    }
    if let Some(author) = keep(core.author) {
        // **不按逗号切**：与 `format::pdf` 那一条同一个理由 —— `Chen, Xiaoming` 这种
        // 「姓, 名」写法真在文件里，切了会把一个人变成两个人。
        meta.authors = vec![author];
    }
    if let Some(subject) = keep(core.subject) {
        // 上游把 `subject` 与 `description` 折成了同一格，语义是简介。
        meta.description = subject;
    }
    if let Some(publisher) = keep(core.publisher) {
        meta.publisher = publisher;
    }
    if let Some(year) = keep(core.year) {
        // 原样存：那一格里写「1905」与写一整串日期的都有，折成日期会把给的粒度抹掉。
        meta.published = year;
    }
    if let Some(keywords) = core.keywords {
        meta.subjects = split_keywords(&keywords);
    }
    // 上游没建模的键按文档序躺在 `extra` 里，只收这三条有界面去处的。
    for (key, value) in core.extra {
        let Some(value) = keep(Some(value)) else {
            continue;
        };
        match key.to_ascii_lowercase().as_str() {
            "language" => meta.language = value,
            "isbn" => meta.isbn = value,
            "type" => meta.dctype = value,
            _ => {}
        }
    }
    meta
}

/// NAVM 的书签树 → 目录，落点是**页号**（与 `format::pdf::outline` 同一个形状：
/// `TocEntry::page`，可重排那一路永远不填那一格）。
///
/// 认不出落点的条目**自己**留着（`page: None`）而不是整条丢掉：真样本上根那一条
/// （`url` 是空的）就是一个只管分组的节点，丢掉它会把下面一整块层级抹平。
/// 界面对 `chapter: None` 的处理是「这一行点不动」，那是正确表现。
fn bookmarks(document: &Document, pages: usize) -> Vec<TocEntry> {
    let Ok(list) = document.bookmarks() else {
        return Vec::new();
    };
    if list.is_empty() {
        return Vec::new();
    }
    // 组件名的对照表：`DIRM` 里 `P` 条目按顺序就是页的顺序，所以「第几页」= 它排第几个。
    let components: Vec<String> = document
        .inner()
        .component_directory()
        .map(|dir| {
            dir.into_iter()
                .filter(|entry| entry.kind == 'P')
                .map(|entry| entry.id)
                .collect()
        })
        .unwrap_or_default();
    list.into_iter()
        .map(|item| bookmark(item, &components, pages))
        .collect()
}

fn bookmark(entry: djvu_rs::Bookmark, components: &[String], pages: usize) -> TocEntry {
    TocEntry {
        title: entry.title.trim().to_string(),
        href: String::new(),
        fragment: None,
        page: page_of_url(&entry.url, components, pages),
        children: entry
            .children
            .into_iter()
            .map(|child| bookmark(child, components, pages))
            .collect(),
    }
}

/// 书签的 url → 引擎那一侧的页号（0 起）。
///
/// 三种写法都来自真样本（那份 71 页的公版 spec：100 条书签里 `#数字` 29 条、
/// `#组件名` 69 条、空 2 条）：
/// - `#64`：**1 起**的页号。fragment 也可以是坐标串 `页,上,下,左,右`，只有第一段是页号；
/// - `#p0064.djvu`：组件名，按 `DIRM` 里 `P` 条目的顺序查；
/// - 空的或认不出的：`None`。
///
/// **不猜组件名里的数字**：真样本里第一页可以叫 `p0001_1.djvu`，另一本 Internet Archive
/// 的页组件是从 `_0000` 起的 —— 「把名字里的数字减一」那条规则在后者上会整整差一页，
/// 而那正是最难查的一种错（目录看着全对，点下去全偏）。查不到就交 `None`。
fn page_of_url(url: &str, components: &[String], pages: usize) -> Option<usize> {
    let target = url.trim().trim_start_matches('#');
    if target.is_empty() {
        return None;
    }
    let head = target.split(',').next().unwrap_or(target).trim();
    let index = match head.parse::<usize>() {
        // 1 起 → 0 起。写 0 的算坏数据：夹到第一页，而不是把这一条丢掉。
        Ok(number) => number.saturating_sub(1),
        Err(_) => components
            .iter()
            .position(|name| name == head)
            // 名字带路径前缀的写法（`pages/p0064.djvu`）：两头都退到文件名再比一次。
            .or_else(|| {
                components.iter().position(|name| {
                    name.rsplit(['/', '\\'])
                        .next()
                        .is_some_and(|last| last.eq_ignore_ascii_case(head))
                })
            })
            .or_else(|| {
                components.iter().position(|name| {
                    head.rsplit(['/', '\\'])
                        .next()
                        .is_some_and(|last| name.eq_ignore_ascii_case(last))
                })
            })?,
    };
    (index < pages).then_some(index)
}

/// 长边不超过 `edge`，比例按页面显示尺寸（已含 `INFO` 旋转）算。
fn fit_size(page: &djvu_rs::Page<'_>, edge: u32) -> (u32, u32) {
    let (width, height) = (page.display_width().max(1), page.display_height().max(1));
    if width >= height {
        (
            edge,
            ((height as u64 * edge as u64) / width as u64).max(1) as u32,
        )
    } else {
        (
            ((width as u64 * edge as u64) / height as u64).max(1) as u32,
            edge,
        )
    }
}

fn encode_png(pixmap: &djvu_rs::Pixmap) -> Option<Vec<u8>> {
    use image::ImageEncoder as _;

    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            &pixmap.data,
            pixmap.width,
            pixmap.height,
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(out)
}

fn unsupported(path: &Path, err: &djvu_rs::Error) -> crate::Error {
    unsupported_text(path, &format!("DjVu 读不出来（{err}）"))
}

fn unsupported_text(path: &Path, why: &str) -> crate::Error {
    crate::Error::UnsupportedFormat(format!(
        "{}：{why}",
        path.file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    ))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{DjvuAdapter, open_document, page_of_url};
    use crate::content::format::{Content, DocumentProvider, Format, PageSource};

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/djvu")
            .join(name)
    }

    #[test]
    fn a_single_page_djvu_becomes_a_render_page_table() {
        let path = fixture("boy_jb2.djvu");
        let (meta, content) = DjvuAdapter.index(&path).unwrap();
        assert_eq!(meta.format, Format::Djvu);
        let Content::Fixed(fixed) = content else {
            panic!("DjVu 该读成定版式");
        };
        assert_eq!(fixed.page_count(), 1);
        assert!(fixed.pages[0].width > 0 && fixed.pages[0].height > 0);
        assert!(matches!(
            fixed.pages[0].source,
            PageSource::Render { index: 0 }
        ));
        assert!(fixed.cover.is_some(), "首页该栅格化成封面");
    }

    /// 真样本那一族带文字层：`DjVu3Spec_bundled.djvu` 的 71 页每页都有一条 `TXTz`。
    ///
    /// 钉三件事，各是一种会画歪的错法：
    /// - **盒都在页内**（越界就是单位或翻转搞错了 —— 这一族的盒是原生像素而且引擎
    ///   已经按页高翻过，与 PDF 那一路的 pt + 自己翻正好是两套规矩）；
    /// - **行从上到下**（`line` 递增时 `y` 也必须递增，否则选中色块从下往上长）；
    /// - **一个字都不丢**：与引擎自己的 `page.text()` 比，去掉空白后必须逐字相同 ——
    ///   zone 层级不是 `Page → … → Line → Word` 那一种时宁可整页一行，也不能少字。
    #[test]
    fn the_djvu_text_layer_is_inside_the_page_and_loses_no_char() {
        let path = fixture("DjVu3Spec_bundled.djvu");
        let text = DjvuAdapter
            .page_text(&path, 0)
            .expect("这份真样本的每一页都带 OCR 的文字层");
        assert!(
            text.char_count() > 200,
            "一页 spec 正文不可能只有几个字：{}",
            text.char_count()
        );
        assert!(text.width > 0.0 && text.height > 0.0, "框就是这一页本身");
        for item in &text.chars {
            assert!(
                item.rect.x >= 0.0
                    && item.rect.y >= 0.0
                    && item.rect.x + item.rect.width <= text.width + 1.0
                    && item.rect.y + item.rect.height <= text.height + 1.0,
                "盒 {item:?} 掉到页外了（页 {}×{}）",
                text.width,
                text.height,
            );
        }
        let line_of = |index: usize| text.chars[index].line;
        let second = text
            .chars
            .iter()
            .position(|item| item.line > 0)
            .expect("这一页该有第二行");
        assert!(
            text.chars[second].rect.y >= text.chars[0].rect.y,
            "行序必须从上到下：第 {second} 格的 y 比第一格小"
        );
        assert_eq!(line_of(second), 1);
        // 不丢字：拿引擎自己交回的整页纯文本比（去掉空白，逐字）。
        let document = open_document(&path).unwrap();
        let want: String = document
            .page(0)
            .unwrap()
            .text()
            .unwrap()
            .unwrap_or_default()
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        let got: String = text
            .text()
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert_eq!(got.len(), want.len(), "少一个字都是丢字");
        assert_eq!(got, want);
        // 没有文字层的那一本（`boy_jb2.djvu` 是单页 JB2 图，没有 TXTz）：交 `None`。
        assert_eq!(
            DjvuAdapter.page_text(&fixture("boy_jb2.djvu"), 0),
            None,
            "没带文字层就不该凭空造字",
        );
    }

    #[test]
    fn a_multi_page_djvu_lists_every_page() {
        let path = fixture("DjVu3Spec_bundled.djvu");
        let (_, content) = DjvuAdapter.index(&path).unwrap();
        let Content::Fixed(fixed) = content else {
            panic!("DjVu 该读成定版式");
        };
        assert!(fixed.page_count() > 1, "样本是多页文档");
    }

    /// NAVM 书签 → 目录。用那份 71 页的公版 spec 真样本（不是合成的）：
    /// 它的书签树实测 100 条、最深 5 层，两种 url 形态各占 29 / 69 条，根那一条是空的。
    ///
    /// 钉的是**两种形态算出同一个页号**——`#64` 与 `#p0064.djvu` 都该给 63。那一条是
    /// 这一格最容易悄悄错的地方：名字里的数字看着像页号，真样本里第一页却叫
    /// `p0001_1.djvu`，而另一本 IA 的组件从 `_0000` 起（见 `page_of_url` 的注释）。
    #[test]
    fn a_real_navm_outline_lands_on_pages() {
        let path = fixture("DjVu3Spec_bundled.djvu");
        let (_, content) = DjvuAdapter.index(&path).unwrap();
        let Content::Fixed(fixed) = content else {
            panic!("DjVu 该读成定版式");
        };
        assert_eq!(fixed.toc.len(), 1, "根只有一条（那一本是一份文档）");
        let root = &fixed.toc[0];
        assert_eq!(
            root.page, None,
            "根那一条的 url 是空的：它只管分组，没有自己的落点"
        );
        let flat: Vec<&crate::content::format::TocEntry> = walk(root);
        assert_eq!(flat.len(), 100, "书签总数按真样本实测");
        assert!(flat.iter().any(|e| e.page.is_some()), "得有人有落点");

        // 「Appendix 4: BZZ coding」在两种写法上都是第 64 页（0 起是 63）。
        let appendix = flat
            .iter()
            .find(|entry| entry.title.contains("BZZ coding"))
            .expect("样本里有那条附录");
        assert_eq!(appendix.page, Some(63), "数字式 url");
        let by_name = appendix
            .children
            .iter()
            .find(|entry| entry.title.ends_with(".djvu"))
            .expect("附录下面那条组件名书签");
        assert_eq!(by_name.page, Some(63), "组件名式 url 要算出同一页");
    }

    fn walk(entry: &crate::content::format::TocEntry) -> Vec<&crate::content::format::TocEntry> {
        let mut out = vec![entry];
        for child in &entry.children {
            out.extend(walk(child));
        }
        out
    }

    /// 页号换算的三条边界，单钉一处（不靠真样本也能跑的那一半）。
    #[test]
    fn a_bookmark_target_is_resolved_by_number_then_component_name() {
        let components = vec!["p0001_1.djvu".to_string(), "p0002.djvu".to_string()];
        assert_eq!(page_of_url("#2", &components, 2), Some(1), "1 起 → 0 起");
        assert_eq!(page_of_url("2", &components, 2), Some(1), "没写 `#` 也认");
        assert_eq!(page_of_url("#1", &components, 2), Some(0));
        assert_eq!(
            page_of_url("#0", &components, 2),
            Some(0),
            "写 0 的夹到第一页"
        );
        assert_eq!(
            page_of_url("#p0002.djvu", &components, 2),
            Some(1),
            "按 DIRM 顺序"
        );
        assert_eq!(
            page_of_url("#pages/p0002.djvu", &components, 2),
            Some(1),
            "带路径前缀的写法"
        );
        assert_eq!(page_of_url("#7", &components, 2), None, "越界的不进目录");
        assert_eq!(page_of_url("", &components, 2), None, "空 url 没有落点");
        // **不猜名字里的数字**：`p0009` 不在这本的两页里，页号写 9 也不给。
        assert_eq!(page_of_url("#p0009.djvu", &components, 2), None);
        // 坐标式 fragment（`页,上,下,左,右`）只有第一段是页号。
        assert_eq!(page_of_url("#2,0,600,0,800", &components, 2), Some(1));
    }

    /// ANTT / AGINT 那一块 → 书目。
    ///
    /// 本机四份真 DjVu 的 `metadata()` 全回 `None`（Internet Archive 把元数据写在旁边的
    /// `.meta` 文件里，不在 DjVu 内），所以这一条只能拿**上游编码器写出的字节**当门：
    /// 由 `djvu-rs` 自己的 `set_metadata` 往那份 279 字节的真样本上加一块，再读回来。
    /// 编码器不是我们的实现，所以「我们怎么把那一块落到 `DocMeta` 的格子里」这一问
    /// 仍然被钉住了；而「那一块的字节长什么样」是上游的事。
    #[test]
    fn the_shared_metadata_block_lands_on_the_shelf_fields() {
        let dir = tempfile::tempdir().unwrap();
        let source = std::fs::read(fixture("boy_jb2.djvu")).unwrap();
        let mut document =
            djvu_rs::djvu_mut::DjVuDocumentMut::from_bytes(&source).expect("那份样本该能改");
        document
            .set_metadata(&djvu_rs::metadata::DjVuMetadata {
                title: Some("带书目的 DjVu".into()),
                author: Some("陈, 小明".into()),
                subject: Some("一份简介".into()),
                publisher: Some("某社".into()),
                year: Some("1905".into()),
                keywords: Some("扫描, 测试, (公版)".into()),
                extra: vec![
                    ("language".into(), "ru".into()),
                    ("isbn".into(), "0-553-21311-3".into()),
                    ("type".into(), "text".into()),
                    ("edition".into(), "第二版 —— 没有去处".into()),
                ],
            })
            .unwrap();
        let path = dir.path().join("meta.djvu");
        std::fs::write(&path, document.into_bytes()).unwrap();

        let (meta, content) = DjvuAdapter.index(&path).unwrap();
        assert_eq!(meta.title, "带书目的 DjVu", "块里那一条要赢过文件名主名");
        assert_eq!(
            meta.authors,
            ["陈, 小明"],
            "「姓, 名」不许切成两个人（与 PDF 同一口径）"
        );
        assert_eq!(meta.description, "一份简介");
        assert_eq!(meta.publisher, "某社");
        assert_eq!(meta.published, "1905");
        assert_eq!(
            meta.subjects,
            ["扫描", "测试", "公版"],
            "括号包着的词组要剥掉括号"
        );
        assert_eq!(meta.language, "ru");
        assert_eq!(meta.isbn, "0-553-21311-3");
        assert_eq!(meta.dctype, "text");
        assert_eq!(meta.format, Format::Djvu);
        let Content::Fixed(fixed) = content else {
            panic!("DjVu 该读成定版式");
        };
        assert_eq!(fixed.page_count(), 1, "加一块不许改变页");
    }

    /// `.djv` 与 `.djvu` 是同一族内容：provider 两个都认，而**落地副本仍写 `.djvu`**
    /// （那一格问的是注册表，注册表里 `djvu` 排在 `djv` 前面才成立 —— 反过来的话
    /// 导进来的书会改一次扩展名，而下一次按名字认领它的正是这张表）。
    #[test]
    fn the_djv_alias_is_claimed_without_moving_the_copy_extension() {
        assert!(DjvuAdapter.supports(Path::new("书.djv")));
        assert!(DjvuAdapter.supports(Path::new("书.DJVU")));
        assert_eq!(Format::Djvu.copy_extension(), "djvu");
    }

    #[test]
    fn a_rendered_page_is_png_at_the_requested_edge() {
        let path = fixture("boy_jb2.djvu");
        let png = DjvuAdapter.render_page(&path, 0, 256).unwrap();
        let image = image::load_from_memory(&png).unwrap();
        assert!(image.width().max(image.height()) <= 256);
    }
}
