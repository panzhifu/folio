//! PDF 取页那一趟的钱花在哪：重开、栅格化、编码、解码各自多少。
//!
//! 与 `bench-import.rs` 的分工：那边量入库一趟（index），这边量**读的一页**——翻页时
//! 屏幕上等一下才出图的那一趟。
//!
//! 换成一台引擎（hayro）之后，这份工具能量的东西少了一截：从前 `open` / `reopen` /
//! `keep` 三臂的差量的是「pdf_oxide 重开一次多贵」，`raw` / `jpeg` 两臂量的是「换个输出
//! 格式省多少」—— 那两样都随 pdf_oxide 一起走了。留下来的臂量**现在这条路**：
//!
//! - `index`：入库一趟（页表 + 书签 + 书目 + 封面）；
//! - `reopen` / `keep`：`PdfAdapter` 每次从头取页 vs 句柄已经在手上；
//! - `scale`：同一页按不同长边各画一次，看代价跟不跟输出像素走；
//! - `capped`：图片页封顶之后的实际尺寸，与不封顶那一档并排；
//! - `decode`：PNG 字节解回位图要多久（图集把它交给 gpui 之前的一步）；
//! - `parallel`：同时画 1/2/3/4 页的墙钟与并行度（页间并行到底成不成立）；
//! - `region`：放大那一档 —— 只重画看得见的那一块 vs 整页按 `REGION_EDGE` 画；
//! - `text`：一页抽字要多久（它是第二遍解释同一页内容流）；
//! - `inspect`：页框、每张图的像素与印面 —— 封顶的输入。
//!
//! ```sh
//! # 一条一趟，每个 arm 都是干净进程（VmHWM 是进程级峰值，混在一条里会互相盖住）
//! for a in index reopen keep scale capped decode text; do
//!   cargo run --release -p folio-core --example pdf-probe -- \
//!     tests/fixtures/local/pdf/hugG.pdf $a 8
//! done
//! ```
//!
//! 读结果的口径：中位数为主信号（尾延迟被页缓存与调度牵着走），`VmHWM` 是这一趟
//! 进程真的驻留过多少，两者都要看。

use std::path::PathBuf;
use std::time::Instant;

use folio_core::content::format::DocumentProvider;
use folio_core::content::format::PageRegion;
use folio_core::content::format::pdf::{PdfAdapter, REGION_EDGE, RENDER_EDGE};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf as RasterPdf;

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

/// 按生产那条路画一页（`PdfAdapter::render_page` 的同一份实现）。
fn paint(document: &RasterPdf, page: usize, edge: u32, settings: &InterpreterSettings) -> Vec<u8> {
    folio_core::content::format::pdf::render_probe(document, page, edge, settings)
        .expect("画得出来")
}

fn open(path: &std::path::Path) -> RasterPdf {
    let bytes = std::fs::read(path).expect("读得开");
    RasterPdf::new(bytes).expect("解得开")
}

/// 按可见框反推目标框，与 `pdf.rs` 的 `raster_size` 同一口径（那边是私有实现）。
fn fit_box(document: &RasterPdf, page: usize, edge: u32) -> (u32, u32) {
    let target = document.pages().get(page).expect("页在书里");
    let crop = target.intersected_crop_box();
    let swap = matches!(
        target.rotation(),
        hayro::hayro_syntax::page::Rotation::Horizontal
            | hayro::hayro_syntax::page::Rotation::FlippedHorizontal
    );
    let (cw, ch) = (
        (crop.x1 - crop.x0).abs() as f32,
        (crop.y1 - crop.y0).abs() as f32,
    );
    let (vw, vh) = if swap { (ch, cw) } else { (cw, ch) };
    let long_pt = vw.max(vh);
    let edge = native_cap(document, page, long_pt)
        .map(|cap| cap.clamp(360.min(edge), edge))
        .unwrap_or(edge);
    let scale = edge as f32 / long_pt;
    (
        (vw * scale).round().max(1.0) as u32,
        (vh * scale).round().max(1.0) as u32,
    )
}

/// `format::pdf::native_edge` 的拷贝（那条是私有实现）。口径与它逐句一致，
/// 改那边要跟着改这里，否则这一份工具量到的就不是生产。
fn native_cap(document: &RasterPdf, page: usize, long_pt: f32) -> Option<u32> {
    let target = document.pages().get(page)?;
    if !matches!(target.rotation(), hayro::hayro_syntax::page::Rotation::None) {
        return None;
    }
    let crop = target.intersected_crop_box();
    let page_area = (crop.x1 - crop.x0).abs() * (crop.y1 - crop.y0).abs();
    if page_area <= 0.0 {
        return None;
    }
    let mut covered = 0.0f32;
    let mut density = 0.0f32;
    for handle in &target.placed_images() {
        let (w, h) = (
            (handle.bbox.x1 - handle.bbox.x0) as f32,
            (handle.bbox.y1 - handle.bbox.y0) as f32,
        );
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        density = density.max((handle.width as f32 / w).max(handle.height as f32 / h) * 72.0);
        covered += (w * h / page_area as f32).min(1.0);
    }
    if density <= 0.0 || covered < 0.85 {
        return None;
    }
    Some((0.88 * long_pt * density / 72.0).round() as u32)
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
        // 只重开：这一档把「打开一次到底多贵」单独摊出来，不掺栅格化。
        "open" => {
            let mut times = Vec::new();
            for _ in &pages {
                let at = Instant::now();
                let document = open(&path);
                let _ = document.pages().len();
                times.push(at.elapsed().as_millis() as f64);
                std::mem::forget(document);
            }
            report("open 一次", &times, &[]);
        }
        // 文字层那一趟：每次重开一次文档再抽整页字。
        // 与 `reopen` 的分工：那边量像素，这边量字 —— 界面的首帧在 UI 线程上等的是这一趟
        // （`FixedBook::cached_page_text` 第一次问某页时走的就是生产这条）。
        "text" => {
            let mut times = Vec::new();
            let mut chars = Vec::new();
            for page in &pages {
                let at = Instant::now();
                let text = PdfAdapter::default().page_text(&path, *page);
                times.push(at.elapsed().as_millis() as f64);
                chars.push(text.map_or(0, |t| t.text().chars().count()));
            }
            report("page_text(重开+抽字)", &times, &[]);
            println!("  每页字数 {chars:?}");
        }
        // 入库那一趟：页表 + 书签 + 书目 + 封面。
        "index" => {
            let at = Instant::now();
            let (meta, content) = PdfAdapter::default().index(&path).unwrap();
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
        // 每页重开一次：每一趟一个新 adapter（`PdfAdapter::default()`），量的是「无状态」那一头。
        "reopen" => {
            let mut times = Vec::new();
            let mut sizes = Vec::new();
            for page in &pages {
                let at = Instant::now();
                let bytes = PdfAdapter::default()
                    .render_page(&path, *page, RENDER_EDGE)
                    .expect("画得出来");
                times.push(at.elapsed().as_millis() as f64);
                sizes.push(bytes.len());
            }
            report("render_page(重开+PNG)", &times, &sizes);
        }
        // 对照：留住一份句柄逐页画，PNG 编码照旧。
        "keep" => {
            let document = open(&path);
            let settings = InterpreterSettings::default();
            let mut times = Vec::new();
            let mut sizes = Vec::new();
            for page in &pages {
                let at = Instant::now();
                let bytes = paint(&document, *page, RENDER_EDGE, &settings);
                times.push(at.elapsed().as_millis() as f64);
                sizes.push(bytes.len());
            }
            report("留住句柄+PNG", &times, &sizes);
        }
        // 界面那一侧：PNG 字节解回位图要多久（图集把它交给 gpui 之前的一步）。
        "decode" => {
            let document = open(&path);
            let settings = InterpreterSettings::default();
            let pngs: Vec<Vec<u8>> = pages
                .iter()
                .map(|page| paint(&document, *page, RENDER_EDGE, &settings))
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
        // 同时画几页到底并并行：同一批页，先串行量一遍每页自己的代价，再按 1/2/3/4 条
        // 线程各起一次，比的是**墙钟**。只有一条线程在干活的写法下，2 页的墙钟 ≈ 2 × 1 页；
        // 真并行应当接近「最慢那一页」。
        //
        // 这一臂存在的理由：作业池的 worker 数（手机 3 / 桌面 6）与交互/预热各封顶 4
        // 都是纸面数 —— 量出来不并行就是白改（`plans` 里那一档「页间并行、页内不并行」
        // 的正确性证据就是这个数）。这里用裸线程而不是我们的池，为的是把「引擎在多核上
        // 能不能同时画」与「我们的排队策略对不对」分开口径。
        // 放大那一档：把「看得见的那一块」按 `REGION_EDGE` 重画，比整页按同一档画便宜多少。
        //
        // 三条并排：手里那张整页 2048（现状）、假想中整页 4096（要是每次放大都重画整页）、
        // 以及真正走的那条 —— 只取四分之一屏那块（捏到 2 倍时屏幕上剩的就是这半宽半高），
        // 按 `REGION_EDGE` 折算输出尺寸（与 app 侧 `region_output` 同一个算式）。
        // 看的是毫秒与产物大小两个数：像素面积差 4 倍，栅格化那一趟就该差差不多 4 倍。
        "region" => {
            let document = open(&path);
            let settings = InterpreterSettings::default();
            let mut full_2048 = Vec::new();
            let mut full_4096 = Vec::new();
            let mut quarter = Vec::new();
            let mut quarter_bytes = Vec::new();
            for page in &pages {
                let (table_w, table_h) = fit_box(&document, *page, RENDER_EDGE);
                let long = table_w.max(table_h).max(1);
                let k = REGION_EDGE as f32 / long as f32;
                // 中间那块半宽半高（与那条门里的取法同形状）。
                let block = PageRegion {
                    x0: table_w / 4,
                    y0: table_h / 4,
                    x1: table_w * 3 / 4,
                    y1: table_h * 3 / 4,
                };
                let (w, h) = block
                    .size()
                    .map(|(bw, bh)| {
                        (
                            (bw as f32 * k).round() as u32,
                            (bh as f32 * k).round() as u32,
                        )
                    })
                    .expect("那块非空");
                let at = Instant::now();
                paint(&document, *page, RENDER_EDGE, &settings);
                full_2048.push(at.elapsed().as_millis() as f64);
                let at = Instant::now();
                paint(&document, *page, REGION_EDGE, &settings);
                full_4096.push(at.elapsed().as_millis() as f64);
                let at = Instant::now();
                let png = folio_core::content::format::pdf::render_region_probe(
                    &document, *page, block, w, h, &settings,
                )
                .expect("区域画得出来");
                quarter_bytes.push(png.len());
                quarter.push(at.elapsed().as_millis() as f64);
                println!(
                    "  第 {page} 页：整页 {table_w}x{table_h} -> 那块 {w}x{h}（按 {REGION_EDGE} 折算）"
                );
            }
            report("整页 @2048（现状）", &full_2048, &[]);
            report("整页 @4096（假想）", &full_4096, &[]);
            report("四分之一块 @4096（走的这条）", &quarter, &quarter_bytes);
        }
        "parallel" => {
            let document = open(&path);
            let take = pages.len().min(4);
            // 先串行跑一遍取基线。顺带说一句口径：这一趟也把页缓存与引擎内部的第一次
            // 分配预热掉了，后面的并行轮因此**偏乐观** —— 看的是比值随线程数涨不涨，
            // 不是绝对毫秒。
            let solo: Vec<f64> = pages[..take]
                .iter()
                .map(|page| {
                    let at = Instant::now();
                    paint(
                        &document,
                        *page,
                        RENDER_EDGE,
                        &InterpreterSettings::default(),
                    );
                    at.elapsed().as_millis() as f64
                })
                .collect();
            report("串行逐页（同批）", &solo, &[]);
            for wide in 1..=take {
                let batch = &pages[..wide];
                let at = Instant::now();
                std::thread::scope(|scope| {
                    for page in batch {
                        let document = &document;
                        scope.spawn(move || {
                            paint(
                                document,
                                *page,
                                RENDER_EDGE,
                                &InterpreterSettings::default(),
                            );
                        });
                    }
                });
                let wall = at.elapsed().as_millis() as f64;
                let serial: f64 = solo[..wide].iter().sum();
                println!(
                    "  同时 {wide} 页: 墙钟 {wall:.1} ms  串行同批 {serial:.1} ms  \
                     并行度 {:.2}×（1.0× 就是完全没并行）",
                    serial / wall.max(0.1)
                );
            }
        }
        "all" => {
            for a in [
                "index", "reopen", "keep", "scale", "capped", "decode", "parallel", "region",
                "text",
            ] {
                println!("--- arm {a} 需要独立进程，见文件头的跑法");
            }
            let at = Instant::now();
            let _ = PdfAdapter::default().index(&path).unwrap();
            println!("index {} ms（其余 arm 逐个跑）", at.elapsed().as_millis());
        }
        // 同一页按不同长边各画一次：看代价是不是跟着**输出像素**走。
        // 跟着走 ⇒ 降 `RENDER_EDGE` 是有效杠杆；不跟着走 ⇒ 钱花在解源图，降档白降。
        "scale" => {
            let document = open(&path);
            let settings = InterpreterSettings::default();
            for edge in [360u32, 512, 720, 1024, 1448, 2048, 2896] {
                let mut times = Vec::new();
                let mut sizes = Vec::new();
                for round in 0..3 {
                    let at = Instant::now();
                    let bytes = paint(&document, pages[round % pages.len()], edge, &settings);
                    times.push(at.elapsed().as_millis() as f64);
                    sizes.push(bytes.len());
                }
                let (w, h) = fit_box(&document, pages[0], edge);
                report(&format!("edge {edge} (框 {w}x{h})"), &times, &sizes);
            }
        }
        // 全书每页各画一次：中位数好说，但「哪几页是 outlier」只有逐页才看得出。
        "sweep" => {
            let document = open(&path);
            let settings = InterpreterSettings::default();
            let pages = (0..document.pages().len()).collect::<Vec<_>>();
            let mut times = Vec::new();
            let mut sizes = Vec::new();
            for page in &pages {
                let at = Instant::now();
                let bytes = paint(&document, *page, RENDER_EDGE, &settings);
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
        // 同一页同一档连画 6 趟，逐趟都打出来：分得开「每次都要重做的固定代价」
        // 与「跟着输出像素走的逐像素代价」。
        "repeat" => {
            let document = open(&path);
            let settings = InterpreterSettings::default();
            for edge in [512u32, 2048] {
                let mut row = format!("  edge {edge}:");
                for _ in 0..6 {
                    let at = Instant::now();
                    let bytes = paint(&document, pages[0], edge, &settings);
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
        // 要量的是**封顶本身**省了多少，以及全书逐页的档位长什么样。
        "capped" => {
            let at = Instant::now();
            let (_, content) = PdfAdapter::default().index(&path).unwrap();
            let folio_core::content::format::Content::Fixed(fixed) = content else {
                panic!("PDF 该读成定版式");
            };
            let index_ms = at.elapsed().as_millis() as f64;
            let edges: Vec<u32> = fixed
                .pages
                .iter()
                .map(|page| page.width.max(page.height))
                .collect();
            let document = open(&path);
            let settings = InterpreterSettings::default();
            let mut times = Vec::new();
            let mut sizes = Vec::new();
            for (page, edge) in edges.iter().enumerate() {
                let at = Instant::now();
                let bytes = paint(&document, page, *edge, &settings);
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
                let (w, h) = fit_box(&document, 0, edge.max(1));
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
        // 一页的图逐张摆出来：算封顶时那三个数（源像素、印刷框、密度）各自是多少。
        // 0.88 那个余量正好压在引擎的「缩放快路」阈值上，差几个像素就是五倍时间 ——
        // 要调这个数，先在这一臂里看清它离阈值有多远。
        "inspect" => {
            let document = open(&path);
            for page in &pages {
                let target = document.pages().get(*page).unwrap();
                let crop = target.intersected_crop_box();
                let (cw, ch) = ((crop.x1 - crop.x0).abs(), (crop.y1 - crop.y0).abs());
                let long_pt = cw.max(ch);
                println!("第 {page} 页 可见 {cw:.2} x {ch:.2} pt");
                for handle in target.placed_images() {
                    let (w, h) = (
                        (handle.bbox.x1 - handle.bbox.x0) as f32,
                        (handle.bbox.y1 - handle.bbox.y0) as f32,
                    );
                    let ppi = (handle.width as f32 / w).max(handle.height as f32 / h) * 72.0;
                    println!(
                        "   {}×{} px 印在 {w:.2}×{h:.2} pt  ⇒ {ppi:.3} ppi  盖面 {:.1}%",
                        handle.width,
                        handle.height,
                        (w * h) as f64 / (cw * ch) * 100.0
                    );
                }
                println!(
                    "   长边 {long_pt:.2} pt ⇒ 封顶 {:?}",
                    native_cap(&document, *page, long_pt as f32)
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
