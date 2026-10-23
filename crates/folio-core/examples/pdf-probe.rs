//! PDF 取页那一趟的钱花在哪：重开、栅格化、编码、解码各自多少。
//!
//! 与 `bench-import.rs` 的分工：那边量入库一趟（index），这边量**读的一页**——
//! 翻页时屏幕上等一下才出图的那一趟。
//!
//! 两件事在这份工具里量出来过，现在都写进了 `format::pdf`：**每次取页重开一次文档**
//! （`open` / `reopen` / `keep` 三臂的差）与**图片页按源图密度封顶**（`capped` 那一臂
//! 是候选方案，已落地成 `native_edge`）。后一条是这本 13.9 MB 的扫描件量出来的：
//! 2048 那一档每页 816 ms、封到 1252 之后 89 ms，全书 45 s → 4.1 s。
//!
//! 下面这些臂的 `fit_box` 是**生产口径的拷贝**（含封顶）：`native_edge` 是私有实现，
//! 而 `raw` / `jpeg` / `scale` 这些对照臂要能自由指定格式与目标框。两边一旦漂移，
//! `sweep` 与 `reopen` 的数就会对不上 —— 那也是信号，不是噪音。
//!
//! ```sh
//! # 一条一趟，每个 arm 都是干净进程（VmHWM 是进程级峰值，混在一条里会互相盖住）
//! for a in open index reopen keep raw jpeg decode; do
//!   cargo run --release -p folio-core --example pdf-probe -- \
//!     tests/fixtures/local/pdf/hugG.pdf $a 8
//! done
//! ```
//!
//! 读结果的口径：中位数为主信号（尾延迟被页缓存与调度牵着走），`VmHWM` 是这一趟
//! 进程真的驻留过多少，两者都要看 —— 重开那一档省下的是驻留，多付的是时间。

use std::path::PathBuf;
use std::time::Instant;

use folio_core::content::format::DocumentProvider;
use folio_core::content::format::pdf::{PdfAdapter, RENDER_EDGE};
use pdf_oxide::document::PdfDocument;
use pdf_oxide::rendering::{ImageFormat, RenderOptions, render_page_fit};

/// 进程级峰值驻留（kB）。Linux 才有，别的平台报 0 而不是假装量到了。
fn peak_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("VmRSS:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

/// 一组计时 → 中位 / 最小 / 最大（毫秒）+ 字节合计。
fn report(label: &str, times: &[f64], bytes: &[usize]) {
    if times.is_empty() {
        println!("{label}: 没量到");
        return;
    }
    let mut sorted = times.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = sorted[sorted.len() / 2];
    let sum: usize = bytes.iter().sum();
    println!(
        "{label}: 中位 {median:.1} ms  min {:.1}  max {:.1}  n={}  产物合计 {} KB (均 {} KB)",
        sorted[0],
        sorted[sorted.len() - 1],
        sorted.len(),
        sum / 1024,
        if bytes.is_empty() {
            0
        } else {
            sum / bytes.len() / 1024
        },
    );
}

/// 按可见框反推目标框，与 `pdf.rs` 的 `fit_box` 同一算法（这里要绕开私有函数）。
///
/// `edge` 先过一遍封顶（与生产的 `render_edge` 同一口径），再落到目标框上。
fn fit_box(document: &PdfDocument, page: usize, edge: u32) -> (u32, u32) {
    let (x0, y0, x1, y1) = document.get_page_visible_box(page).unwrap();
    let long_pt = (x1 - x0).abs().max((y1 - y0).abs());
    let edge = native_cap(document, page, long_pt)
        .map(|cap| cap.clamp(360.min(edge), edge))
        .unwrap_or(edge);
    fit_box_exact(document, page, edge)
}

/// 不经封顶的那一份：量的就是「要这一档，画出来多大」。
fn fit_box_exact(document: &PdfDocument, page: usize, edge: u32) -> (u32, u32) {
    let (x0, y0, x1, y1) = document.get_page_visible_box(page).unwrap();
    let (mx0, my0, mx1, my1) = document.get_page_media_box(page).unwrap();
    let swap = matches!(
        document
            .get_page_rotation(page)
            .unwrap_or(0)
            .rem_euclid(360),
        90 | 270
    );
    let extent = |ax: f32, ay: f32, bx: f32, by: f32| {
        let (w, h) = ((bx - ax).abs(), (by - ay).abs());
        if swap { (h, w) } else { (w, h) }
    };
    let (vw, vh) = extent(x0, y0, x1, y1);
    let (mw, mh) = extent(mx0, my0, mx1, my1);
    let scale = edge as f32 / vw.max(vh);
    (
        (mw * scale).round().max(1.0) as u32,
        (mh * scale).round().max(1.0) as u32,
    )
}

/// `format::pdf::native_edge` 的拷贝（那条是私有实现）。口径与它逐句一致，
/// 改那边要跟着改这里，否则这一份工具量到的就不是生产。
fn native_cap(document: &PdfDocument, page: usize, long_pt: f32) -> Option<u32> {
    if document
        .get_page_rotation(page)
        .unwrap_or(0)
        .rem_euclid(360)
        != 0
    {
        return None;
    }
    let handles = document.page_image_handles(page).ok()?;
    let (x0, y0, x1, y1) = document.get_page_visible_box(page).ok()?;
    let page_area = (x1 - x0).abs() * (y1 - y0).abs();
    if page_area <= 0.0 {
        return None;
    }
    let mut covered = 0.0f32;
    let mut density = 0.0f32;
    for handle in &handles {
        let (w, h) = (handle.bbox.width.abs(), handle.bbox.height.abs());
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        density = density.max((handle.width as f32 / w).max(handle.height as f32 / h) * 72.0);
        covered += (w * h / page_area).min(1.0);
    }
    if density <= 0.0 || covered < 0.85 {
        return None;
    }
    Some((0.88 * long_pt * density / 72.0).round() as u32)
}

fn render_with(document: &PdfDocument, page: usize, edge: u32, format: ImageFormat) -> Vec<u8> {
    let mut options = RenderOptions::default();
    options.format = format;
    let (w, h) = fit_box(document, page, edge);
    render_page_fit(document, page, w, h, &options)
        .expect("这一页该画得出来")
        .data
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("用法：pdf-probe <书.pdf> [arm] [页数]"));
    let arm = args.next().unwrap_or_else(|| "all".to_string());
    // 第三个参数：一个数 = 「取前 N 页」，逗号或空格分隔的一串 = 「就取这几页」。
    let selector = args.next();
    let count: usize = selector
        .as_deref()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    assert!(path.exists(), "样本不在：{}", path.display());
    let file_bytes = std::fs::metadata(&path).unwrap().len();

    let pages: Vec<usize> = match selector.as_deref() {
        Some(list) if list.contains([',', ' ']) => list
            .split([',', ' '])
            .filter_map(|v| v.parse().ok())
            .collect(),
        _ => (0..count).collect(),
    };
    println!(
        "书 {}  {} MB  arm={arm}  取 {} 页  RENDER_EDGE={RENDER_EDGE}",
        path.file_name().unwrap().to_string_lossy(),
        file_bytes / 1024 / 1024,
        pages.len(),
    );

    match arm.as_str() {
        // 只重开：这一档把「重开一次到底多贵」单独摊出来，不掺栅格化。
        "open" => {
            let mut times = Vec::new();
            for _ in &pages {
                let at = Instant::now();
                let document = PdfDocument::open(&path).unwrap();
                let _ = document.page_count().unwrap();
                times.push(at.elapsed().as_millis() as f64);
                std::mem::forget(document);
            }
            report("open 一次", &times, &[]);
        }
        // 入库那一趟：页表 + 书签 + 书目 + 封面。
        "index" => {
            let at = Instant::now();
            let (meta, content) = PdfAdapter.index(&path).unwrap();
            let ms = at.elapsed().as_millis() as f64;
            let folio_core::content::format::Content::Fixed(fixed) = content else {
                panic!("PDF 该读成定版式");
            };
            println!(
                "index: {ms:.1} ms  标题 {:?}  {} 页  目录 {} 条  封面 {} KB  首屏 {}×{}  驻留 {} MB",
                meta.title,
                fixed.pages.len(),
                fixed.toc.len(),
                fixed.cover.as_ref().map_or(0, |c| c.len() / 1024),
                fixed.width,
                fixed.height,
                peak_kb() / 1024,
            );
        }
        // 现状：每页重开一次 PdfDocument（`PdfAdapter::render_page`）。
        "reopen" => {
            let mut times = Vec::new();
            let mut sizes = Vec::new();
            for page in &pages {
                let at = Instant::now();
                let bytes = PdfAdapter
                    .render_page(&path, *page, RENDER_EDGE)
                    .expect("画得出来");
                times.push(at.elapsed().as_millis() as f64);
                sizes.push(bytes.len());
            }
            report("render_page(重开+PNG)", &times, &sizes);
        }
        // 对照：留住一份句柄逐页画，PNG 编码照旧。
        "keep" => {
            let document = PdfDocument::open(&path).unwrap();
            let mut times = Vec::new();
            let mut sizes = Vec::new();
            for page in &pages {
                let at = Instant::now();
                let bytes = render_with(&document, *page, RENDER_EDGE, ImageFormat::Png);
                times.push(at.elapsed().as_millis() as f64);
                sizes.push(bytes.len());
            }
            report("留住句柄+PNG", &times, &sizes);
        }
        // 对照：留住句柄 + 不编码（直接交 RGBA 像素）。
        "raw" => {
            let document = PdfDocument::open(&path).unwrap();
            let mut times = Vec::new();
            let mut sizes = Vec::new();
            for page in &pages {
                let at = Instant::now();
                let bytes = render_with(&document, *page, RENDER_EDGE, ImageFormat::RawRgba8);
                times.push(at.elapsed().as_millis() as f64);
                sizes.push(bytes.len());
            }
            report("留住句柄+RawRgba8", &times, &sizes);
        }
        "jpeg" => {
            let document = PdfDocument::open(&path).unwrap();
            for quality in [70u8, 85] {
                let mut times = Vec::new();
                let mut sizes = Vec::new();
                for page in &pages {
                    let at = Instant::now();
                    let mut options = RenderOptions::default();
                    options.format = ImageFormat::Jpeg;
                    options.jpeg_quality = quality;
                    let (w, h) = fit_box(&document, *page, RENDER_EDGE);
                    let bytes = render_page_fit(&document, *page, w, h, &options)
                        .unwrap()
                        .data;
                    times.push(at.elapsed().as_millis() as f64);
                    sizes.push(bytes.len());
                }
                report(&format!("留住句柄+JPEG q{quality}"), &times, &sizes);
            }
        }
        // 界面那一侧：PNG 字节解回位图要多久（图集把它交给 gpui 之前的一步）。
        "decode" => {
            let document = PdfDocument::open(&path).unwrap();
            let pngs: Vec<Vec<u8>> = pages
                .iter()
                .map(|page| render_with(&document, *page, RENDER_EDGE, ImageFormat::Png))
                .collect();
            let mut times = Vec::new();
            let mut px = 0u64;
            for png in &pngs {
                let at = Instant::now();
                let image = image::load_from_memory(png).expect("PNG 解得开");
                times.push(at.elapsed().as_millis() as f64);
                px += image.as_bytes().len() as u64;
            }
            report("PNG→位图", &times, &[]);
            println!(
                "  位图合计 {} MB，PNG 合计 {} MB",
                px / 1024 / 1024,
                pngs.iter().map(|b| b.len() as u64).sum::<u64>() / 1024 / 1024
            );
        }
        "all" => {
            for a in ["open", "reopen", "keep", "raw", "decode"] {
                println!("--- arm {a} 需要独立进程，见文件头的跑法");
            }
            let at = Instant::now();
            let _ = PdfAdapter.index(&path).unwrap();
            println!("index {} ms（其余 arm 逐个跑）", at.elapsed().as_millis());
        }
        // 同一页按不同长边各画一次：看代价是不是跟着**输出像素**走。
        // 跟着走 ⇒ 降 `RENDER_EDGE` 是有效杠杆；不跟着走 ⇒ 钱花在解源图，降档白降。
        "scale" => {
            let document = PdfDocument::open(&path).unwrap();
            for edge in [360u32, 512, 720, 1024, 1448, 2048, 2896] {
                let mut times = Vec::new();
                let mut sizes = Vec::new();
                for round in 0..3 {
                    let at = Instant::now();
                    let bytes = render_with(
                        &document,
                        pages[round % pages.len()],
                        edge,
                        ImageFormat::Png,
                    );
                    times.push(at.elapsed().as_millis() as f64);
                    sizes.push(bytes.len());
                }
                let (w, h) = fit_box(&document, pages[0], edge);
                report(&format!("edge {edge} (框 {w}x{h})"), &times, &sizes);
            }
        }
        // 全书每页各画一次：中位数好说，但「哪几页是 outlier」只有逐页才看得出。
        "sweep" => {
            let document = PdfDocument::open(&path).unwrap();
            let pages = (0..document.page_count().unwrap()).collect::<Vec<_>>();
            let mut times = Vec::new();
            let mut sizes = Vec::new();
            for page in &pages {
                let at = Instant::now();
                let bytes = render_with(&document, *page, RENDER_EDGE, ImageFormat::Png);
                times.push(at.elapsed().as_millis() as f64);
                sizes.push(bytes.len());
            }
            report("全书逐页", &times, &sizes);
            let mut pairs: Vec<(f64, usize, usize)> = times
                .iter()
                .copied()
                .zip(pages.clone())
                .zip(sizes.clone())
                .map(|((t, p), s)| (t, p, s))
                .collect();
            pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
            for (ms, page, size) in pairs.iter().take(6) {
                println!(
                    "   第 {page:>2} 页 {:7.1} ms  PNG {:>4} KB",
                    ms,
                    size / 1024
                );
            }
            let total: f64 = times.iter().sum();
            println!("   {} 页合计 {:.1} s", pages.len(), total / 1000.0);
        }
        // 细扫某几页的 edge→ms 曲线，找那个断层的**位置会不会随页移动**。
        // 断层跟着页内位图的原始宽度走 ⇒ 钱花在「解源图 + 上采样」，
        // 按内容自己的分辨率封顶就免费；固定在某个绝对值 ⇒ 另有原因。
        "cliff" => {
            let document = PdfDocument::open(&path).unwrap();
            for page in &pages {
                let mut row = format!("第 {page:>2} 页:");
                for edge in [720u32, 1024, 1200, 1300, 1448, 1600, 1800, 2048] {
                    let mut best = f64::MAX;
                    for _ in 0..2 {
                        let at = Instant::now();
                        let _ = render_with(&document, *page, edge, ImageFormat::RawRgba8);
                        best = best.min(at.elapsed().as_millis() as f64);
                    }
                    let (w, _) = fit_box(&document, *page, edge);
                    row.push_str(&format!(" {edge}/{w}w={best:.0}ms"));
                }
                println!("{row}");
            }
        }
        // 同一页同一档连画 6 趟，逐趟都打出来：分得开「每次都要重做的固定代价」
        // 与「跟着输出像素走的逐像素代价」。
        "repeat" => {
            let document = PdfDocument::open(&path).unwrap();
            for edge in [512u32, 2048] {
                let mut row = format!("  edge {edge}:");
                for _ in 0..6 {
                    let at = Instant::now();
                    let bytes = render_with(&document, pages[0], edge, ImageFormat::RawRgba8);
                    row.push_str(&format!(" {:.0}ms", at.elapsed().as_millis() as f64));
                    std::hint::black_box(&bytes);
                }
                println!("{row}");
            }
            let info = std::process::Command::new("pdfinfo")
                .args(["-f", "1", "-l", "1", &path.to_string_lossy()])
                .output();
            if let Ok(out) = info {
                print!("{}", String::from_utf8_lossy(&out.stdout));
            }
            if let Ok(out) = std::process::Command::new("pdffonts")
                .args(["-f", "1", "-l", "1", &path.to_string_lossy()])
                .output()
            {
                print!("{}", String::from_utf8_lossy(&out.stdout));
            }
        }
        // 按页问「这一页自己有多少像素」：`page_image_handles` 只读结构不解码，
        // 拿 width / (bbox 宽 / 72) 就是那张图印在页上的原生 ppi。
        // 逐页打「引擎快路允许的最高档」与「实测断层」对不对得上，
        // 对得上才谈得上把它当 index 期就能算出来的常数用。
        "handles" => {
            let document = PdfDocument::open(&path).unwrap();
            let pages = (0..document.page_count().unwrap()).collect::<Vec<_>>();
            let at = Instant::now();
            let mut rows = Vec::new();
            for page in &pages {
                let handles = document.page_image_handles(*page).unwrap_or_default();
                let (_, _, vw, vh) = document.get_page_visible_box(*page).unwrap();
                let (pw, ph) = (vw.abs(), vh.abs());
                let mut best = 0f32;
                let mut covered = 0f32;
                for h in &handles {
                    let bw = h.bbox.width.abs();
                    let bh = h.bbox.height.abs();
                    if bw <= 0.0 || bh <= 0.0 {
                        continue;
                    }
                    // 这张图印在页上的原生分辨率（ppi），取长短边里更高那一档。
                    let ppi = (h.width as f32 / bw * 72.0).max(h.height as f32 / bh * 72.0);
                    best = best.max(ppi);
                    covered += (bw * bh / (pw * ph)).min(1.0);
                }
                rows.push((*page, handles.len(), best, covered.min(1.0), pw, ph));
            }
            let ms = at.elapsed().as_millis() as f64;
            println!(
                "  {ms:.1} ms 问完全书 {count} 页的图柄（单页 {:.1} ms）",
                ms / count.max(1) as f64
            );
            for (page, n, ppi, covered, pw, ph) in rows.iter().copied().take(8) {
                // 画布长边 E ⇒ 每 pt 有 E/页长边(pt) 个像素。一张「源 n px 印在 b pt 上」
                // 的图，缩比 sx = b·E/(页长·n) = E·72/(页长·ppi)。快路要 sx < 0.9 ⇒
                // E < 0.9 × 页长边(pt) × ppi / 72。
                let long_pt = pw.max(ph);
                let cap = 0.9 * long_pt * ppi / 72.0;
                println!(
                    "   第 {page:>2} 页 {n} 张图  源 {:.0} ppi  占面 {:.0}%  ⇒ 快路上限 edge≈{:.0}（封顶 2048 后取 {:.0}）",
                    ppi,
                    covered * 100.0,
                    cap,
                    cap.min(2048.0),
                );
            }
        }
        // 生产那一趟的形状：index 期把每页的档位算进页表，读的时候按页表的档位现画。
        // 「留住句柄」是这一臂的简化（生产每页重开一次，那笔差见 `reopen` 与 `keep`）；
        // 要量的是**封顶本身**省了多少，以及全书逐页的档位长什么样。
        "capped" => {
            let at = Instant::now();
            let (_, content) = PdfAdapter.index(&path).unwrap();
            let folio_core::content::format::Content::Fixed(fixed) = content else {
                panic!("PDF 该读成定版式");
            };
            let index_ms = at.elapsed().as_millis() as f64;
            let edges: Vec<u32> = fixed
                .pages
                .iter()
                .map(|page| page.width.max(page.height))
                .collect();
            let document = PdfDocument::open(&path).unwrap();
            let mut times = Vec::new();
            let mut sizes = Vec::new();
            for (page, edge) in edges.iter().enumerate() {
                let at = Instant::now();
                let bytes = render_with(&document, page, *edge, ImageFormat::Png);
                times.push(at.elapsed().as_millis() as f64);
                sizes.push(bytes.len());
            }
            let total: f64 = times.iter().sum();
            println!("  逐页档位：{edges:?}");
            report("按页表档位画全书", &times, &sizes);
            println!(
                "  全书 {} 页合计 {:.1} s；入库（页表 + 档位）{index_ms:.0} ms",
                edges.len(),
                total / 1000.0,
            );
            let bytes_of = |edge: u32| {
                let (w, h) = fit_box_exact(&document, 0, edge);
                w as usize * h as usize * 4
            };
            let capped = bytes_of(edges[0]);
            let uncapped = bytes_of(RENDER_EDGE);
            println!(
                "  单页 RGBA 最大 {:.1} MB（不封顶那一档 {:.1} MB）；图集 24 MB 装得下 {} 页 vs {} 页",
                capped as f64 / 1024.0 / 1024.0,
                uncapped as f64 / 1024.0 / 1024.0,
                24 * 1024 * 1024 / capped.max(1),
                24 * 1024 * 1024 / uncapped.max(1),
            );
        }
        // 一页的图柄逐张摆出来：算封顶时那三个数（源像素、印刷框、密度）各自是多少。
        // 0.9 那个余量正好压在引擎的「缩放快路」阈值上，差几个像素就是五倍时间 ——
        // 要调这个数，先在这一臂里看清它离阈值有多远。
        "inspect" => {
            let document = PdfDocument::open(&path).unwrap();
            for page in &pages {
                let (x0, y0, x1, y1) = document.get_page_visible_box(*page).unwrap();
                let long_pt = (x1 - x0).abs().max((y1 - y0).abs());
                println!(
                    "第 {page} 页 可见 {:.2} x {:.2} pt",
                    (x1 - x0).abs(),
                    (y1 - y0).abs()
                );
                for handle in document.page_image_handles(*page).unwrap_or_default() {
                    let (w, h) = (handle.bbox.width.abs(), handle.bbox.height.abs());
                    let ppi = (handle.width as f32 / w).max(handle.height as f32 / h) * 72.0;
                    println!(
                        "   {}×{} px 印在 {w:.2}×{h:.2} pt  ⇒ {ppi:.3} ppi  盖面 {:.1}%",
                        handle.width,
                        handle.height,
                        (w * h) / ((x1 - x0).abs() * (y1 - y0).abs()) * 100.0
                    );
                }
                println!(
                    "   长边 {long_pt:.2} pt ⇒ 封顶 {:?}",
                    native_cap(&document, *page, long_pt)
                );
            }
        }
        other => panic!("不认识这个 arm：{other}"),
    }
    println!(
        "峰值驻留 {} MB（当前 {} MB）",
        peak_kb() / 1024,
        rss_kb() / 1024
    );
}
