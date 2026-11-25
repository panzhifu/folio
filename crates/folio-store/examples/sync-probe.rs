//! 同步快照与库锁的实测入口：先有数，再谈阈值（计划 §9）。
//!
//! 用法：`cargo run --release -p folio-store --example sync-probe`
//! 书数用 `FOLIO_PROBE_BOOKS=200,1000,3000` 覆盖，默认三档。
//!
//! 两件事分开量：
//! - 导出：`collect_sync` 每档书库的墙钟 p50/p95；
//! - 锁等待：后台一直导出时，界面那条读路径（`pref_number`）要等多久。
//!
//! 这不是 CI 闸门，也不证明真实设备上限；它记的是本机、本次输入的重复性。

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use folio_format::{DocMeta, Format};
use folio_model::TextAnchor;
use folio_model::position::Locator;
use folio_store::{AnnotationStyle, Store};

fn main() -> folio_store::Result<()> {
    let sizes: Vec<usize> = std::env::var("FOLIO_PROBE_BOOKS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![200, 1000, 3000]);
    println!("rustc {}", rustc_version());
    for size in sizes {
        run_case(size)?;
    }
    Ok(())
}

fn rustc_version() -> String {
    std::process::Command::new("rustc")
        .arg("-V")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn run_case(books: usize) -> folio_store::Result<()> {
    let dir = std::env::temp_dir().join(format!("folio-sync-probe-{books}"));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(Store::open_at(&dir.join("probe.db"))?);
    seed(&store, books)?;

    let _ = store.collect_sync("probe", 1)?;
    let export = timed(5, || store.collect_sync("probe", 1).map(|_| ()))?;
    let wait = lock_wait(&store)?;
    println!(
        "books={books} export_p50={:.2}ms export_p95={:.2}ms wait_p50={:.2}ms wait_p95={:.2}ms wait_max={:.2}ms wait_over1ms={}/{}",
        pct(&export, 0.50),
        pct(&export, 0.95),
        pct(&wait, 0.50),
        pct(&wait, 0.95),
        wait.last().copied().unwrap_or(0.0),
        wait.iter().filter(|ms| **ms > 1.0).count(),
        wait.len(),
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn seed(store: &Store, books: usize) -> folio_store::Result<()> {
    for i in 0..books {
        let book = store.add_book(
            Path::new(&format!("/books/{i:05}.epub")),
            &DocMeta {
                title: format!("书 {i:05}"),
                format: Format::Epub,
                size: 4096,
                ..DocMeta::default()
            },
        )?;
        store.set_content_hash(book, &format!("hash-{i:05}"), "sha256")?;
        store.set_position(
            book,
            "paged",
            &Locator {
                chapter: (i % 5) as u32,
                offset: 100,
            },
        )?;
        store.add_annotation(
            book,
            &anchor(0, 0, 4),
            "一",
            "amber",
            AnnotationStyle::Highlight,
        )?;
        store.add_bookmark(book, &anchor(0, 10, 10))?;
    }
    Ok(())
}

/// 后台连续导出，主线程量一次只为读一个偏好值的等待。
fn lock_wait(store: &Arc<Store>) -> folio_store::Result<Vec<f64>> {
    let background = {
        let store = Arc::clone(store);
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                let _ = store.collect_sync("probe", 2);
            }
        })
    };
    std::thread::sleep(Duration::from_millis(50));
    let mut waits = Vec::new();
    for _ in 0..30 {
        let start = Instant::now();
        let _ = store.pref_number("probe")?;
        waits.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let _ = background.join();
    waits.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(waits)
}

fn timed<F>(runs: usize, mut f: F) -> folio_store::Result<Vec<f64>>
where
    F: FnMut() -> folio_store::Result<()>,
{
    let mut samples = Vec::new();
    for _ in 0..runs {
        let start = Instant::now();
        f()?;
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(samples)
}

fn pct(samples: &[f64], p: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let index = ((samples.len() - 1) as f64 * p).round() as usize;
    samples[index]
}

fn anchor(chapter: u32, start: usize, end: usize) -> TextAnchor {
    TextAnchor {
        chapter,
        start,
        end,
        exact: "正文".into(),
        prefix: String::new(),
        suffix: String::new(),
        path: String::new(),
    }
}
