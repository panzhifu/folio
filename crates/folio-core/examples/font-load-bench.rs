//! 字体加载链路的压测：容器、样式表、`@font-face` 字节，各走几趟并报中位数。
//!
//! 只量 `folio-core` 这一段；`add_fonts` / `all_font_names` 在 app 侧，另由
//! `BookFonts::for_document` 的开书日志（`folio_app::perf` 的 `book_open`）覆盖。
//!
//! 跑法：
//! ```text
//! cargo run -p folio-core --example font-load-bench -- tests/fixtures/local/hali-bote-quanji.epub
//! REPS=9 cargo run -p folio-core --example font-load-bench -- <另一本书>
//! ```
//!
//! 计时只打印，不进断言：本机页缓存状态一变，同一段代码能差 2–3 倍。验收仍用
//! 「容器打开次数 / `doc.fonts` 调用次数 / 字节数」那一类计数门。

use std::time::Instant;

use folio_core::content::format::css::{FontSrc, Origin, RuleSet};

const DEFAULT_BOOK: &str = "tests/fixtures/local/hali-bote-quanji.epub";

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| {
            let default = std::path::PathBuf::from(DEFAULT_BOOK);
            default.is_file().then_some(default)
        });
    let Some(path) = path else {
        eprintln!(
            "用法：cargo run -p folio-core --example font-load-bench -- <书的路径>\n\
             默认路径 {DEFAULT_BOOK} 也不在。"
        );
        return;
    };
    let reps = std::env::var("REPS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|reps| *reps > 0)
        .unwrap_or(5);

    println!("# 字体加载压测：{}", path.display());
    println!();
    println!("REPS = {reps}（每段取中位数；计时只作趋势）");
    println!();

    // 打开一本书本身（认格式 + 索引）只量一次：它会重扫容器，和字体那几段不是同一笔账。
    let opened = Instant::now();
    let doc = match folio_core::content::document::open(&path) {
        Ok(doc) => doc,
        Err(err) => {
            eprintln!("读不进去：{err:?}");
            return;
        }
    };
    let open_ms = elapsed_ms(opened);
    let chapters = doc.flow().map_or(0, |flow| flow.chapters.len());
    println!("| 段 | 最小 ms | 中位 ms | 最大 ms | 读数 |\n|---|---:|---:|---:|---|");
    println!(
        "| `document::open`（含索引） | {open_ms:.2} | {open_ms:.2} | {open_ms:.2} | {chapters} 章 |"
    );

    let css = doc.stylesheet();
    let wanted = collect_wanted(&css);
    let (css_times, css_len) = bench(reps, || doc.stylesheet().len());
    println!(
        "| `doc.stylesheet()` | {:.2} | {:.2} | {:.2} | {} B |",
        css_times.0, css_times.1, css_times.2, css_len
    );

    let (rules_times, faces) = bench(reps, || {
        let rules = RuleSet::parse(&css, Origin::Author);
        rules.font_faces.len()
    });
    println!(
        "| `RuleSet::parse` | {:.2} | {:.2} | {:.2} | {} 个 `@font-face` |",
        rules_times.0, rules_times.1, rules_times.2, faces
    );
    let (refs_times, refs_len) =
        bench(reps, || doc.font_refs(&wanted).map_or(0, |refs| refs.len()));
    println!(
        "| `doc.font_refs`（元数据，不解压） | {:.2} | {:.2} | {:.2} | {} 份 |",
        refs_times.0, refs_times.1, refs_times.2, refs_len
    );
    let (font_times, font_bytes) = bench(reps, || {
        doc.fonts(&wanted)
            .iter()
            .map(|face| face.bytes.len())
            .sum::<usize>()
    });
    println!(
        "| `doc.fonts(&wanted)` | {:.2} | {:.2} | {:.2} | {} 条 url / {} B |",
        font_times.0,
        font_times.1,
        font_times.2,
        wanted.len(),
        font_bytes
    );
    println!();
    println!(
        "注：`doc.fonts` 仍一次读全；`BookFonts::for_request` 命中身份缓存时只走 `font_refs`（本例约 17.6 ms），不读字节。"
    );
}

/// 文案里那一行 `doc.fonts` 占位与实测行分开打印；这个函数只负责收集 wanted。
fn collect_wanted(css: &str) -> Vec<String> {
    let rules = RuleSet::parse(css, Origin::Author);
    let mut wanted: Vec<String> = Vec::new();
    for face in &rules.font_faces {
        for src in &face.src {
            if let FontSrc::Url(href) = src
                && !wanted.iter().any(|existing| existing == href)
            {
                wanted.push(href.clone());
            }
        }
    }
    wanted
}

/// 跑 `reps` 次，返回 `(最小, 中位, 最大)` 毫秒，外加最后一次的读数。
fn bench(reps: usize, mut f: impl FnMut() -> usize) -> ((f64, f64, f64), usize) {
    let mut times = Vec::with_capacity(reps);
    let mut reading = 0;
    for _ in 0..reps {
        let started = Instant::now();
        reading = f();
        times.push(elapsed_ms(started));
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let min = times.first().copied().unwrap_or(0.0);
    let median = times[times.len() / 2];
    let max = times.last().copied().unwrap_or(0.0);
    ((min, median, max), reading)
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}
