//! 全文搜索的量具：三条路各自的代价，一次跑完摆在一起看。
//!
//! 1. **读全书正文**：把一本书每章的 XHTML 从容器里取出来，再折成可见文本，一共多久。
//!    这决定「不建索引、每次搜索现读一遍」能不能用。
//! 2. **不建索引直接扫**：把全书可见文本拼成一条，用 `str::find` 逐个子串匹配，多久。
//! 3. **FTS5 trigram**：同样的每章文本进一张 fts5 表，建库多久、文件多大、查一次多久，
//!    以及两个字的中文查询在里面是什么下场。
//!
//! ```sh
//! cargo run -p folio-core --example search-probe -- --tmp /tmp
//! cargo run -p folio-core --example search-probe -- tests/fixtures/local/shejie-mauriac.epub
//! ```
//!
//! 第四件事在 `--sweep <装着书的目录>` 那一档：把整架逐本现扫一遍，把「开书」与「扫正文」
//! 那两笔分开报，并外推到 50 / 100 / 300 本（全库那一档的墙在哪，见 `search.rs` 模块头）。
//!
//! ```sh
//! mkdir -p ~/tmp/folio-lib-probe && for i in $(seq 1 10); do ln tests/fixtures/local/*.epub ~/tmp/folio-lib-probe/ 2>/dev/null; done
//! cargo run --release -p folio-core --example search-probe -- --sweep ~/tmp/folio-lib-probe
//! ```
//!
//! 只在开发机上手动跑，不进 CI：它读的是本机那几本真书，换一台机器数就变。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use folio_core::BookDocument;
use folio_core::content::anchor::TextMap;
use folio_core::content::document::Document;
use folio_core::content::dom::Dom;
use folio_core::content::format::visible_text;
use folio_core::infra::task::{TaskEvent, TaskHandle, TaskManager};
use folio_core::search::{Found, LIBRARY_MAX_PER_BOOK, Options, Query, scan};

/// 一章的可见文本 + 它在整本书里的起点（字符）。
struct ChapterText {
    chars: usize,
    text: String,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(at) = args.iter().position(|a| a == "--sweep") {
        let dir = args
            .get(at + 1)
            .map(PathBuf::from)
            .expect("--sweep 要跟一个装着书的目录");
        return sweep_library(&dir);
    }
    let tmp = args
        .iter()
        .position(|a| a == "--tmp")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string()));
    let books: Vec<PathBuf> = if args.is_empty() {
        default_corpus()
    } else {
        args.iter()
            .filter(|a| !a.starts_with("--"))
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .collect()
    };
    if books.is_empty() {
        eprintln!("没有书可跑（给路径，或放到 tests/fixtures/local/ 里）");
        std::process::exit(2);
    }
    for book in books {
        if let Err(err) = probe(&book, Path::new(&tmp)) {
            eprintln!("{}: {err}", book.display());
        }
    }
}

/// 全库那一档的量法：把一整个目录里的书逐本现扫一遍，把「开书」与「读正文找命中」
/// 那两笔分开报 —— 这一刀要的判据就是这两笔各占多少。
///
/// ```sh
/// cargo run --release -p folio-core --example search-probe -- --sweep ~/tmp/folio-lib-probe
/// ```
///
/// 两趟查询各有各的分工。**冷**那一趟用的是从每本书自己身上摘下来的六个字：全本只命中
/// 一次，所以非扫到最后一章不可，量的是最坏那一档。**全库**那一趟用「一个」配上
/// `LIBRARY_MAX_PER_BOOK`（20 处）那一档 —— 也就是将来架面上真会用的那个配置：命中爆多，
/// 每本扫到 20 处就收手。
///
/// 每一趟都走作业池、拿真的 `StageCtx`（读数发进没人读的通道，代价是一次线程派发），
/// 所以这几行报的就是 app 那一侧将来真正会付的钱。
fn sweep_library(dir: &Path) {
    static POOL: std::sync::OnceLock<TaskManager> = std::sync::OnceLock::new();
    let pool = POOL.get_or_init(|| TaskManager::new(1));

    let mut books: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("目录读不了")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "epub"))
        .collect();
    books.sort();
    if books.is_empty() {
        eprintln!("{} 里没有 .epub", dir.display());
        std::process::exit(2);
    }
    println!(
        "全库逐本现扫：{} 本（同一台机、页缓存热的那一档）",
        books.len()
    );
    println!(
        "  {:<26} {:>8} {:>8} {:>8} {:>8} {:>6}",
        "书名", "大小", "开书", "冷查询", "每本20处", "章"
    );

    let mut cold_total = Duration::ZERO;
    let mut hot_total = Duration::ZERO;
    let mut open_total = Duration::ZERO;
    let mut bytes_total = 0u64;
    let mut done = 0usize;
    for path in &books {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let started = Instant::now();
        let book = match BookDocument::open(path) {
            Ok(book) => book,
            Err(err) => {
                println!("  开不了 {}：{err}", path.display());
                continue;
            }
        };
        let opened = started.elapsed();
        let doc: Arc<dyn Document> = Arc::new(book);
        // 从这本书中间那一章摘六个字：换一本书也照样命中，且只命中一次。
        let middle = doc.flow().map(|flow| flow.chapters.len() / 2).unwrap_or(0);
        let probe = doc
            .chapter_source(middle)
            .map(|source| {
                let text = visible_text(&source);
                let at = text.chars().count() / 2;
                text.chars().skip(at).take(6).collect::<String>()
            })
            .unwrap_or_default();
        let Some(cold) = Query::parse(&probe) else {
            println!("  摘不出查询：{}", path.display());
            continue;
        };
        let hot = Query::parse("一个").expect("前置：那两个字不可能是空的");
        let library = Options {
            max_hits: LIBRARY_MAX_PER_BOOK,
            ..Options::default()
        };
        let (cold_at, cold_found) = sweep_one(pool, &doc, &cold, Options::default());
        let (hot_at, hot_found) = sweep_one(pool, &doc, &hot, library);
        println!(
            "  {:<26} {:>8} {:>8} {:>8} {:>8} {:>6}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
            human(size),
            ms(opened),
            ms(cold_at),
            ms(hot_at),
            cold_found.chapters,
        );
        // 前置：冷那一趟必须扫完全本并且命中过，否则这一行不是「全本单价」。
        assert!(
            !cold_found.hits.is_empty() && !cold_found.truncated,
            "前置：{} 的冷查询扫了 {}/{} 章、命中 {} 处，这一档的数不能当全本单价读",
            path.display(),
            cold_found.scanned,
            cold_found.chapters,
            cold_found.hits.len(),
        );
        assert!(!hot_found.cancelled, "前置：全库那一趟不该被取消");
        assert_eq!(
            hot_found.hits.len(),
            LIBRARY_MAX_PER_BOOK,
            "前置：那一档就是每本 {LIBRARY_MAX_PER_BOOK} 处"
        );
        bytes_total += size;
        open_total += opened;
        cold_total += cold_at;
        hot_total += hot_at;
        done += 1;
    }
    let n = done.max(1) as f64;
    println!(
        "\n{done} 本合计：开书 {}   冷 {}   热 {}",
        ms(open_total),
        ms(cold_total),
        ms(hot_total),
    );
    // 「扫」与「开书」要合起来读才是全库那一档的钱：逐本扫之前，每一本都得先开一次容器。
    let per_open = open_total.as_secs_f64() * 1000.0 / n;
    let per_cold = (open_total + cold_total).as_secs_f64() * 1000.0 / n;
    let per_hot = (open_total + hot_total).as_secs_f64() * 1000.0 / n;
    println!(
        "平均每本：开书 {:>5.0} ms + 扫（冷）{:>5.0} ms = {:>5.0} ms   |   开书 + 扫（每本 20 处）{:>5.0} ms   |   书本身 {:.1} MB/本",
        per_open,
        cold_total.as_secs_f64() * 1000.0 / n,
        per_cold,
        per_hot,
        bytes_total as f64 / n / 1024.0 / 1024.0,
    );
    println!(
        "        ——  开书那一笔占冷那一档的 {:.0}%、占 20 处那一档的 {:.0}%：它不看书有多大，只看架上有几本",
        per_open / per_cold * 100.0,
        per_open / per_hot * 100.0,
    );
    for count in [50u64, 100, 300] {
        println!(
            "{count:>3} 本（按这一架的比例外推）：冷 CPU {:.1} s / 20 处那一档 CPU {:.1} s  ——  池里两条 worker 各半：{:.1} s / {:.1} s",
            per_cold * count as f64 / 1000.0,
            per_hot * count as f64 / 1000.0,
            per_cold * count as f64 / 2000.0,
            per_hot * count as f64 / 2000.0,
        );
    }
}

/// 一趟扫描走一次真的池作业：拿的是真的 `StageCtx`，读的也是真的结果。
fn sweep_one(
    pool: &TaskManager,
    doc: &Arc<dyn Document>,
    query: &Query,
    options: Options,
) -> (Duration, Found) {
    let doc = Arc::clone(doc);
    let query = query.clone();
    let started = Instant::now();
    let handle: TaskHandle<Found> = pool.submit(move |ctx| Ok(scan(&*doc, &query, &options, ctx)));
    let mut out = None;
    while let Ok(event) = handle.events.recv() {
        if let TaskEvent::Finished(Ok(found)) = event {
            out = Some(found);
        }
    }
    (started.elapsed(), out.expect("那一趟该跑完并交出结果"))
}
fn human(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1}M", bytes as f64 / 1024.0 / 1024.0)
    } else {
        format!("{}K", bytes / 1024)
    }
}

fn default_corpus() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/local");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "epub"))
        .collect();
    out.sort();
    out
}

fn probe(path: &Path, tmp: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let file_bytes = std::fs::metadata(path)?.len();
    println!("\n=== {} ===", path.display());
    println!("文件 {file_bytes} 字节");

    let t = Instant::now();
    let doc = BookDocument::open(path)?;
    let open_at = t.elapsed();
    let Some(flow) = doc.flow() else {
        println!("定版式，跳过");
        return Ok(());
    };
    let n = flow.chapters.len();

    // --- 1. 读全书 ---
    let t = Instant::now();
    let mut sources: Vec<String> = Vec::with_capacity(n);
    let mut read_fail = 0usize;
    for i in 0..n {
        match doc.chapter_source(i) {
            Ok(text) => sources.push(text),
            Err(_) => {
                read_fail += 1;
                sources.push(String::new());
            }
        }
    }
    let read_at = t.elapsed();
    let source_bytes: usize = sources.iter().map(|s| s.len()).sum();

    // --- 2. 折成可见文本（DOM + TextMap，与锚点同一把尺）---
    let t = Instant::now();
    let mut chapters: Vec<ChapterText> = Vec::with_capacity(n);
    let mut whole = String::new();
    for src in &sources {
        if src.is_empty() {
            chapters.push(ChapterText {
                chars: 0,
                text: String::new(),
            });
            continue;
        }
        let dom = Dom::parse(src.as_bytes());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map_or(dom.document, |(id, _)| id);
        let map = TextMap::build(&dom, body);
        let text = map.text().to_string();
        whole.push_str(&text);
        chapters.push(ChapterText {
            chars: text.chars().count(),
            text,
        });
    }
    let fold_at = t.elapsed();
    let total_chars: usize = chapters.iter().map(|c| c.chars).sum();
    println!(
        "开书 {:>8}   读 {} 章正文 {:>9}（{} 字节 XHTML，失败 {read_fail}）   折可见文本 {:>9}",
        ms(open_at),
        n,
        ms(read_at),
        source_bytes,
        ms(fold_at),
    );
    println!(
        "可见文本 {total_chars} 字 = {} 字节（{} 章是空的）",
        whole.len(),
        chapters.iter().filter(|c| c.chars == 0).count()
    );

    // --- 3. 扫一遍本身的单价（把读与折的账摘掉）---
    let sure: String = whole.chars().skip(total_chars / 2).take(6).collect();
    let raw = bench(8, || {
        let mut hits = 0usize;
        let mut at = 0usize;
        while let Some(i) = whole[at..].find("一个") {
            hits += 1;
            at += i + 6;
            if hits > 100_000 {
                break;
            }
        }
        hits
    });
    println!(
        "纯扫：{} 字节一条字符串找「一个」命中 {:>6}，{:>8}/次（{sure:?} 这类长查询只会更快）",
        whole.len(),
        raw.1,
        ms(raw.0),
    );

    // --- 3b. 真路：`folio_core::search::scan` 经作业池跑一遍 ---
    //
    // 这一格才是读者每一次改查询实际付的钱：按位置回容器取每章正文、折成可见文本、
    // 找命中、裁上下文，全在里面。四条查询各有分工：`sure` 保证命中，`一个` 是中文
    // 两个字的常见一档（FTS5 的 `MATCH` 在这一档静默给 0 行），`的` 是命中爆多的那一档，
    // `the` 逼出「两边折了再比」那条路 —— 中文书里它多半命中不了，但全本照样扫完，
    // 量的正是那个单价。
    let arc: Arc<dyn Document> = Arc::from(BookDocument::open(path)?);
    let pool = TaskManager::new(1);
    println!("真路（现读现扫，走作业池，含按位置取正文与折可见文本）：");
    for q in [sure.as_str(), "一个", "的", "the"] {
        let Some(query) = Query::parse(q) else {
            continue;
        };
        let doc = arc.clone();
        let folds = query.folds_case();
        let t = Instant::now();
        let handle: TaskHandle<Found> =
            pool.submit(move |ctx| Ok(scan(&*doc, &query, &Options::default(), ctx)));
        let mut out = None;
        while let Ok(event) = handle.events.recv() {
            if let TaskEvent::Finished(Ok(found)) = event {
                out = Some(found);
            }
        }
        let out = out.expect("作业该跑完并交出结果");
        assert_eq!(out.chapters, n, "前置：章数与上面那个数同一份");
        // 命中爆多的查询到 200 处就收手（那是上限那一档，不是漏扫）；其余该扫完整本。
        assert!(
            out.scanned == n || out.truncated,
            "前置：{q:?} 只扫了 {scanned} 章却没截断，这一趟的数不能当全本单价读",
            scanned = out.scanned
        );
        println!(
            "  {:<12} 命中 {:>4}   {:>8}   {}那一条路 · 扫 {}/{} 章 · 截断={}",
            format!("{q:?}"),
            out.hits.len(),
            ms(t.elapsed()),
            if folds { "折了再比" } else { "按字节" },
            out.scanned,
            out.chapters,
            out.truncated,
        );
    }

    // --- 4. FTS5 trigram ---
    fts_probe(tmp, &chapters, whole.len())?;
    Ok(())
}

/// 「一定命中」那条查询在按章扫那一趟里未必命中，所以按章那趟单独用它自己的词。
fn bench<F: FnMut() -> usize>(rounds: usize, mut f: F) -> (Duration, usize) {
    let mut best = Duration::MAX;
    let mut count = 0;
    for _ in 0..rounds {
        let t = Instant::now();
        count = f();
        let el = t.elapsed();
        if el < best {
            best = el;
        }
    }
    (best, count)
}

fn fts_probe(
    tmp: &Path,
    chapters: &[ChapterText],
    raw_bytes: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use rusqlite::Connection;

    std::fs::create_dir_all(tmp)?;
    let file = tmp.join(format!(
        "folio-fts-probe-{}-{}.db",
        std::process::id(),
        chapters.len()
    ));
    let _ = std::fs::remove_file(&file);
    let mut conn = Connection::open(&file)?;
    conn.execute_batch("PRAGMA page_size=4096; PRAGMA journal_mode=OFF;")?;
    conn.execute_batch(
        r#"
CREATE VIRTUAL TABLE chapter_fts USING fts5(
    text,
    tokenize='trigram case_sensitive 0'
);
"#,
    )?;

    let rows: Vec<&String> = chapters
        .iter()
        .map(|c| &c.text)
        .filter(|t| !t.is_empty())
        .collect();

    let t = Instant::now();
    {
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare("INSERT INTO chapter_fts(rowid, text) VALUES (?1, ?2)")?;
            for (i, text) in rows.iter().enumerate() {
                stmt.execute(rusqlite::params![i as i64 + 1, **text])?;
            }
        }
        tx.commit()?;
    }
    let build_at = t.elapsed();
    let bytes = std::fs::metadata(&file)?.len();
    conn.execute_batch("VACUUM;")?;
    let packed = std::fs::metadata(&file)?.len();

    println!(
        "FTS5 trigram：{} 行入库 {:>9}，文件 {bytes} 字节（VACUUM 后 {packed}）",
        rows.len(),
        ms(build_at)
    );
    println!(
        "  体积比：索引 {packed} / 正文明文 {raw_bytes} = {:.2}×",
        packed as f64 / raw_bytes.max(1) as f64
    );

    println!("  查询（MATCH 那一档看 trigram 的硬门槛，LIKE 那一档看全表扫的钱）：");
    for q in ["一个", "什么东西", "孙悟空"] {
        let hit = timed(
            &conn,
            "SELECT count(*) FROM chapter_fts WHERE chapter_fts MATCH ?1",
            q,
        );
        let like = timed(
            &conn,
            "SELECT count(*) FROM chapter_fts WHERE text LIKE ?1",
            &format!("%{q}%"),
        );
        println!(
            "    {q:?} MATCH {rows:>4} 行 {at}/次 · LIKE {lrows:>4} 行 {lat}/次",
            rows = hit.1,
            at = ms(hit.0),
            lrows = like.1,
            lat = ms(like.0),
        );
    }
    drop(conn);
    let _ = std::fs::remove_file(&file);
    Ok(())
}

fn timed(conn: &rusqlite::Connection, sql: &str, arg: &str) -> (Duration, i64) {
    use rusqlite::params;
    let mut best = Duration::MAX;
    let mut got = -1i64;
    for _ in 0..5 {
        let t = Instant::now();
        let n: rusqlite::Result<i64> = conn.query_row(sql, params![arg], |r| r.get(0));
        let el = t.elapsed();
        match n {
            Ok(v) => {
                got = v;
                if el < best {
                    best = el;
                }
            }
            Err(err) => {
                // 错误也要出声：trigram 的 2 字查询到底是 0 命中还是报错。
                println!("  {arg:?} 报错：{err}");
                return (Duration::ZERO, -1);
            }
        }
    }
    (best, got)
}

fn ms(d: Duration) -> String {
    format!("{:>7.1} ms", d.as_secs_f64() * 1000.0)
}
