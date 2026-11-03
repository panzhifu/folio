//! 格式矩阵的量具：把一批真文件逐本打开，报「认成什么格式、读出多少章 / 页 / 图、
//! 第一段正文是不是空」，一处汇总。
//!
//! 跑法：
//!
//! ```sh
//! cargo run -p folio-core --example format-probe -- tests/fixtures/local/formats
//! cargo run -p folio-core --example format-probe -- a.fb2 b.chm c.cbz
//! ```
//!
//! 判据不是「不报错就算过」：
//!
//! - 可重排的必须至少一章，前三章与最后一章的正文取得到且不空；
//! - 书里登记了图片的，前六张要真的从容器里取得出字节、解得开（SVG 只认标记）；
//! - 定版式的必须至少一页，首页与末页画得出图（PNG），或按页表的 `href` 取得到图；
//! - 书名不许是空串。
//!
//! 任何一条不成立就在末尾的失败栏里点名 —— 这一栏非空时进程退出码是 1，可以直接当门用。
//!
//! 只在开发机上手动跑，不进 CI：它读的是本机下载的真样本，换一台机器文件就没了。
//! 库里那 21 个 provider 每族都该至少有一份真样本，来源与许可写在
//! `tests/fixtures/README.md` 那一节。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use folio_core::content::document::{ContentRef, open};
use folio_core::content::format::{PageSource, provider_for};

/// 一本的读数。
struct Row {
    path: PathBuf,
    format: String,
    kind: &'static str,
    title: String,
    chapters: usize,
    pages: usize,
    chars: Option<usize>,
    toc: usize,
    images: usize,
    images_ok: usize,
    first_text: usize,
    note: String,
}

fn main() {
    let mut targets = Vec::new();
    for arg in std::env::args().skip(1) {
        collect(Path::new(&arg), &mut targets);
    }
    if targets.is_empty() {
        eprintln!("用法：format-probe <文件或目录>…");
        std::process::exit(2);
    }
    targets.sort();

    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for path in &targets {
        match catch_unwind(AssertUnwindSafe(|| probe(path))) {
            Ok(Ok((row, problems))) => {
                if !problems.is_empty() {
                    failures.push((path.clone(), problems));
                }
                rows.push(row);
            }
            Ok(Err(err)) => failures.push((path.clone(), vec![format!("打不开：{err}")])),
            Err(_) => failures.push((path.clone(), vec!["解析时 panic".to_string()])),
        }
    }

    println!(
        "{:<28} {:<9} {:<7} {:>4} {:>4} {:>8} {:>4} {:>6} {:>6}  书名",
        "文件", "格式", "家族", "章", "页", "字数", "目录", "图", "首章"
    );
    for row in &rows {
        let name = row
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let name = truncate(name, 28);
        println!(
            "{:<28} {:<9} {:<7} {:>4} {:>4} {:>8} {:>4} {:>6} {:>6}  {}",
            name,
            row.format,
            row.kind,
            row.chapters,
            row.pages,
            row.chars
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".to_string()),
            row.toc,
            if row.images == 0 {
                "—".to_string()
            } else {
                format!("{}/{}", row.images_ok, row.images)
            },
            row.first_text,
            truncate(&row.title, 32),
        );
        if !row.note.is_empty() {
            println!("  └─ {}", row.note);
        }
    }

    if !failures.is_empty() {
        println!("\n失败 {} 份：", failures.len());
        for (path, problems) in &failures {
            println!("- {}", path.display());
            for problem in problems {
                println!("    {problem}");
            }
        }
        std::process::exit(1);
    }
    println!("\n全部 {} 份通过。", rows.len());
}

/// 目录就收它下面认得出的文件，文件就直接收 —— 认不出扩展名的也收：
/// 「这一份不是书」本身也是一条要看的读数。
fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            collect(&entry.path(), out);
        }
    } else if path.is_file() {
        out.push(path.to_path_buf());
    }
}

fn probe(path: &Path) -> Result<(Row, Vec<String>), String> {
    let mut problems = Vec::new();
    let provider = provider_for(path)
        .ok_or_else(|| format!("没有 provider 认领（扩展名 {:?}）", path.extension()))?;
    let format = provider.format();
    let doc = open(path).map_err(|err| err.to_string())?;
    let meta = doc.meta();
    let mut row = Row {
        path: path.to_path_buf(),
        format: format.as_str().to_string(),
        kind: "",
        title: meta.title.clone(),
        chapters: 0,
        pages: 0,
        chars: None,
        toc: 0,
        images: 0,
        images_ok: 0,
        first_text: 0,
        note: String::new(),
    };

    match doc.content() {
        ContentRef::Reflow(flow) => {
            row.kind = "reflow";
            row.chapters = flow.chapters.len();
            row.chars = flow.char_len();
            row.toc = flow.toc.len();
            row.images = flow.images.len().min(6);
            if flow.chapters.is_empty() {
                problems.push("可重排却没有章".to_string());
            }
            let take = flow.chapters.len().min(3);
            for index in 0..take {
                check_chapter(doc.as_ref(), index, &mut row.first_text, &mut problems);
            }
            if let Some(last) = flow.chapters.len().checked_sub(1)
                && last >= take
            {
                check_chapter(doc.as_ref(), last, &mut row.first_text, &mut problems);
            }
            // 前六张：图片字节要取得到、解得开。这一格是「图在不在」的真问题 ——
            // EPUB 的 SVG、FB2 的 `<binary>`、RTF 的 `\pict`、CHM 的条目各有各的坐标。
            for image in flow.images.iter().take(6) {
                match doc.image_bytes(&image.href) {
                    Some(bytes) if image_is_decodable(&bytes, &image.media_type) => {
                        row.images_ok += 1;
                    }
                    Some(bytes) => problems.push(format!(
                        "图片 {} 解不开（{} 字节，声明 {}）",
                        image.href,
                        bytes.len(),
                        image.media_type
                    )),
                    None => problems.push(format!("图片 {} 取不到字节", image.href)),
                }
            }
            if flow.images.len() > 6 {
                row.note = format!("共 {} 张图，只查前 6", flow.images.len());
            }
            if meta.title.trim().is_empty() {
                problems.push("书名是空串".to_string());
            }
        }
        ContentRef::Fixed(fixed) => {
            row.kind = "fixed";
            row.pages = fixed.page_count();
            row.toc = fixed.toc.len();
            if fixed.pages.is_empty() {
                problems.push("定版式却没有页".to_string());
            }
            if fixed.width == 0 || fixed.height == 0 {
                problems.push(format!("画布尺寸是 {}×{}", fixed.width, fixed.height));
            }
            let last = fixed.pages.len().saturating_sub(1);
            for index in [0usize, last]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
            {
                check_page(
                    doc.as_ref(),
                    fixed,
                    index,
                    &mut row.images,
                    &mut row.images_ok,
                    &mut problems,
                );
            }
        }
    }

    if let Some(note) = meta.authors.first() {
        let author = format!("作者：{}", truncate(note, 24));
        row.note = if row.note.is_empty() {
            author
        } else {
            format!("{} · {author}", row.note)
        };
    }
    Ok((row, problems))
}

fn check_chapter(
    doc: &dyn folio_core::content::document::Document,
    index: usize,
    first_text: &mut usize,
    problems: &mut Vec<String>,
) {
    match doc.chapter_source(index) {
        Ok(text) => {
            if index == 0 {
                *first_text = text.chars().count();
            }
            if text.trim().is_empty() {
                problems.push(format!("第 {index} 章正文为空"));
            }
            // 乱码的硬信号：码页解错时 `charset::decode` 交回的是一串 U+FFFD。
            // 少数几个不算 —— 真书末尾被截断半个字（`liaozhai.txt` 就是）会只掉一个，
            // 而码页读错是成片掉。GBK / CP1252 / CP950 那几族真样本就是为这一条留的。
            let broken = text.chars().filter(|ch| *ch == '\u{FFFD}').count();
            if broken >= 3 {
                problems.push(format!("第 {index} 章有 {broken} 个替换字符"));
            }
        }
        Err(err) => problems.push(format!("第 {index} 章取不到：{err}")),
    }
}

/// 定版式的一页有两种来源：容器里的一张图（漫画包），或栅格化出来的一页
/// （PDF / AZW4 / DjVu）。按页表说哪一种问哪一条 —— 漫画包里没有
/// `render_page`，问它只会得到 `None`，那不是「画不出来」。
fn check_page(
    doc: &dyn folio_core::content::document::Document,
    fixed: &folio_core::content::format::FixedDoc,
    index: usize,
    images: &mut usize,
    images_ok: &mut usize,
    problems: &mut Vec<String>,
) {
    match fixed.pages.get(index).map(|page| &page.source) {
        Some(PageSource::Image { href }) => {
            *images += 1;
            match doc.image_bytes(href) {
                Some(bytes) if image_is_decodable(&bytes, "") => *images_ok += 1,
                Some(bytes) => problems.push(format!(
                    "第 {index} 页不是图片（{} 字节，头 {:02x?}）",
                    bytes.len(),
                    &bytes[..bytes.len().min(4)]
                )),
                None => problems.push(format!("第 {index} 页（{href}）取不到字节")),
            }
        }
        Some(PageSource::Render { index: at, .. }) => match doc.render_page(*at as usize, 512) {
            Some(bytes) if bytes.starts_with(b"\x89PNG") => {}
            Some(bytes) => problems.push(format!(
                "第 {index} 页不是 PNG（{} 字节，头 {:02x?}）",
                bytes.len(),
                &bytes[..bytes.len().min(4)]
            )),
            None => problems.push(format!("第 {index} 页画不出来")),
        },
        None => problems.push(format!("页表没有第 {index} 页")),
    }
}

/// 一张图能不能解开：位图交给 `image` 真解码，SVG 只认标记
/// （引擎那边是直接当矢量文档排的，不是位图解码器）。
fn image_is_decodable(bytes: &[u8], media_type: &str) -> bool {
    if media_type.contains("svg") {
        let head = &bytes[..bytes.len().min(512)];
        return head.windows(4).any(|w| w == b"<svg");
    }
    image::load_from_memory(bytes).is_ok()
}

fn truncate(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}
