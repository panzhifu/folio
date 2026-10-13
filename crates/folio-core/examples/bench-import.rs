//! 导入基准：一本书从「文件在手上」到「库里能打开」，钱花在哪一步。
//!
//! 与 `bench-layout.rs` 的分工：那边量的是排版（读那一侧），这边量入库（写这一侧）。
//!
//! ```sh
//! # 默认拿仓库里的fixture
//! cargo run --release -p folio-core --example bench-import
//! # 指定一本书，跑 5 轮取中位
//! cargo run --release -p folio-core --example bench-import -- /path/to/book.epub 5
//! ```
//!
//! 读结果的两条口径：
//!
//! - **计数器是主信号**。`infra::metrics` 那三个量（开容器、建条目名索引、解析 DOM）
//!   与机器无关，一本 N 章的书该解析几次有确定答案；计时会被调度噪声牵着走（±20%）。
//! - 计时用中位数 + 热页缓存，只用来回答「这一步占整体几成」，不用来比较两台的快慢。

/// 计数用的全局分配器：把「省掉大量分配」这种说法变成一个可断言的整数。
///
/// 只活在这个基准里（example 是自己的二进制，换 global allocator 不影响库与产物）。
/// 用原子计数而不是计时：分配次数与字节数与机器无关，峰值 RSS 却会被别的阶段盖住。
static ALLOC_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ALLOC_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct Counting;

unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        ALLOC_BYTES.fetch_add(layout.size() as u64, std::sync::atomic::Ordering::Relaxed);
        ALLOC_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, next: usize) -> *mut u8 {
        ALLOC_BYTES.fetch_add(
            next.saturating_sub(layout.size()) as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        ALLOC_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        unsafe { std::alloc::System.realloc(ptr, layout, next) }
    }
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        ALLOC_BYTES.fetch_add(layout.size() as u64, std::sync::atomic::Ordering::Relaxed);
        ALLOC_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// 当前的 (累计分配字节, 分配次数)。
fn alloc_shot() -> (u64, u64) {
    (
        ALLOC_BYTES.load(std::sync::atomic::Ordering::Relaxed),
        ALLOC_CALLS.load(std::sync::atomic::Ordering::Relaxed),
    )
}

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use folio_core::content::document::{self, ContentRef};
use folio_core::content::dom::Dom;
use folio_core::content::format::cover::{self, CoverRef};
use folio_core::content::format::{shrink_image, thumb};
use folio_core::infra::metrics::{Metric, global};
use folio_core::ingest::identity;
use folio_core::{ImportKind, PathsSource, Source, Store, TaskEvent, TaskManager, submit_import};

/// 把文件读进页缓存，但**不把它整个抱在手里**。
///
/// 原来用的是 `fs::read`，那会真分配一份与文件等长的缓冲：量时间没影响（首轮的磁盘
/// 延迟照样被摊掉），但一本 29 MB 的书会把峰值 RSS 读脏 —— 那一格要读的是导入自己占
/// 多少，不是预热占多少。
fn warm(page: &Path) {
    use std::io::Read as _;
    let mut file = std::fs::File::open(page).expect("开书预热");
    let mut buf = vec![0u8; 256 * 1024];
    while file.read(&mut buf).expect("读书") > 0 {}
}

/// 这一台机器的两个读数：**喂给并行公式的那个核数**，与整趟跑完的**峰值 RSS（MB）**。
///
/// 核数不是背景信息：`ingest::import::split_budget` 的输入就是它，而它不是「说明书上有
/// 几个核」—— 按本进程能用的亲和性掩码报（Meizu 20 在册 8 核，`nproc` 只报 6，于是
/// 同一份代码在两边切出来的层数不一样）。RSS 读 `/proc/self/status` 的 `VmHWM`：
/// 并行那一档多花的正是这一格，光看时间看不出来。
fn machine_probe() -> (usize, u64) {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let rss_kb = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
        })
        .unwrap_or(0);
    (cores, rss_kb / 1024)
}

/// 一批书走真管线的墙钟：同时导几本 × 每本几路，是唯一变量。
///
/// 每轮换一个全新的书库目录与库文件 —— 同一批第二次导会在判重那步短路，量到的就不是
/// 解析而是 `stat`。比较的办法是把 `ingest::import::process_budget` 依次钉成几组
/// `(books, per_book)` 各编一个二进制，然后**交替跑**：固定顺序下每轮先跑的那一支总在
/// DVFS 还没爬起来的频率上（本文件 `shrink_probe` 那段记着这个坑）。
fn batch_probe(books: &[PathBuf], rounds: usize) -> Vec<Duration> {
    let manager = TaskManager::new(1);
    let mut runs = Vec::with_capacity(rounds);
    let mut summary = String::new();
    for _ in 0..rounds {
        let root = tempfile::tempdir().expect("建临时书库");
        let lib = root.path().join("library");
        std::fs::create_dir_all(&lib).expect("书库目录");
        let store = Arc::new(Store::open_at(&lib.join("folio.db")).expect("开库"));
        // 先把每一本读进页缓存，别让首轮的磁盘延迟混进 CPU 账。
        for book in books {
            warm(book);
        }
        let source: Box<dyn Source> =
            Box::new(PathsSource::new(books.to_vec(), ImportKind::Picked));
        let handle = submit_import(&manager, source, lib, Arc::clone(&store));
        let t = Instant::now();
        let mut done = None;
        while let Ok(event) = handle.events.recv() {
            if let TaskEvent::Finished(result) = event {
                done = Some(result);
            }
        }
        let elapsed = t.elapsed();
        match done {
            Some(Ok(outcomes)) => {
                assert_eq!(outcomes.len(), books.len(), "报表条数必须等于候选数");
                let added = outcomes.iter().filter(|o| o.result.is_ok()).count();
                if added == 0 {
                    panic!("一本都没进库：{:?}", outcomes[0].result.as_ref().err());
                }
                summary = format!("成功 {added} / 共 {}", outcomes.len());
            }
            Some(Err(err)) => panic!("批次作业失败：{err}"),
            None => panic!("没有终局事件"),
        }
        runs.push(elapsed);
    }
    println!("  一批结清：{summary}");
    runs
}

/// 一本书的分阶段读数。
#[derive(Default)]
struct Stages {
    copy: Duration,
    sha256: Duration,
    open: Duration,
    cover_write: Duration,
    thumbs: Duration,
    add_book: Duration,
    save: Duration,
}

impl Stages {
    fn total(&self) -> Duration {
        self.copy
            + self.sha256
            + self.open
            + self.cover_write
            + self.thumbs
            + self.add_book
            + self.save
    }
}

fn mb(bytes: f64) -> String {
    format!("{:.1} MB", bytes / 1048576.0)
}

/// 探针的重复轮数：够取中位，又不让一次基准跑变成十分钟。
const PROBES: usize = 7;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn report(label: &str, d: Duration, of: Duration) {
    let share = if of.is_zero() {
        0.0
    } else {
        d.as_secs_f64() / of.as_secs_f64() * 100.0
    };
    println!("  {label:<26} {:>9.1} ms   {:>5.1}%", ms(d), share);
}

/// 端到端：走真管线（`submit_import` → 线程池 → `import_from`）。
///
/// 每轮换一个全新的书库目录与库文件 —— 同一本第二次导入会在判重那步短路，
/// 量到的就不是解析而是 `stat`。
fn end_to_end(book: &Path, rounds: usize) -> Vec<Duration> {
    let manager = TaskManager::new(1);
    let mut runs = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let root = tempfile::tempdir().expect("建临时书库");
        let lib = root.path().join("library");
        std::fs::create_dir_all(&lib).expect("书库目录");
        let store = Arc::new(Store::open_at(&lib.join("folio.db")).expect("开库"));
        // 先把文件读进页缓存，别让首轮的磁盘延迟混进 CPU 账。
        warm(book);

        let source: Box<dyn Source> = Box::new(PathsSource::new(
            vec![book.to_path_buf()],
            ImportKind::Picked,
        ));
        let handle = submit_import(&manager, source, lib, Arc::clone(&store));
        let t = Instant::now();
        let mut done = None;
        while let Ok(event) = handle.events.recv() {
            if let TaskEvent::Finished(result) = event {
                done = Some(result);
            }
        }
        runs.push(t.elapsed());
        match done {
            Some(Ok(outcomes)) => {
                let ok = outcomes.iter().filter(|o| o.result.is_ok()).count();
                if ok == 0 {
                    let why = outcomes
                        .first()
                        .map(|o| format!("{:?}", o.result.as_ref().err()))
                        .unwrap_or_default();
                    panic!("一本都没进库：{why}");
                }
            }
            Some(Err(err)) => panic!("作业失败：{err:?}"),
            None => panic!("没有终局事件"),
        }
    }
    runs
}

/// 手工重排一遍 `import_candidate` 的步子，逐段单独计时。
///
/// `sniff::identify` 是 `pub(crate)`，在这里够不着；它只做几次读头 + 一次 zip 开档，
/// 用计数器那栏的 `container_opens` 覆盖它。
fn stage_breakdown(book: &Path, rounds: usize) -> (Stages, CounterDelta, Shape) {
    let mut acc = Stages::default();
    let mut counters = CounterDelta::default();
    let mut shape = Shape::default();

    for _ in 0..rounds {
        let root = tempfile::tempdir().expect("建临时暂存");
        let staging = root.path().join(".staging");
        std::fs::create_dir_all(&staging).expect("暂存目录");
        let lib_root = root.path().join("library");
        std::fs::create_dir_all(&lib_root).expect("书库目录");
        let store = Store::open_at(&lib_root.join("folio.db")).expect("开库");

        let staged = staging.join("book.part");

        // 步 2：来源把字节写进暂存区。
        let t = Instant::now();
        std::fs::copy(book, &staged).expect("拷贝");
        let copy = t.elapsed();

        // 步 3 前半：整本 SHA-256。
        let t = Instant::now();
        let hash = identity::sha256_of_path(&staged).expect("哈希");
        let sha256 = t.elapsed();
        let hash_hex = hash.to_hex();

        // 步 5 前半：改成看得懂的名字再解析。
        let readable = staging.join(format!("{}.epub", &hash_hex[..8]));
        std::fs::rename(&staged, &readable).expect("改名");

        // 步 6：解析。计数器只圈这一段。
        //
        // 这里读**进程全局**而不是默认的 `take_local`：解析现在会按条目并行，
        // DOM 解析与容器打开发生在若干 worker 线程上，只读主线程的局部计数会
        // 把整段账读成 0。基准是独立进程，全局量不会混进别的测试。
        let before = [
            global(Metric::ContainerOpens),
            global(Metric::EntryNameTablePasses),
            global(Metric::DomParses),
        ];
        let alloc_before = alloc_shot();
        let t = Instant::now();
        let doc = document::open(&readable).expect("解析");
        let open = t.elapsed();
        let alloc_after = alloc_shot();
        println!(
            "  （这一趟 open 的分配：{:.1} MB / {} 次）",
            (alloc_after.0 - alloc_before.0) as f64 / 1048576.0,
            alloc_after.1 - alloc_before.1
        );
        let delta = CounterDelta {
            container_opens: global(Metric::ContainerOpens) - before[0],
            name_table_passes: global(Metric::EntryNameTablePasses) - before[1],
            dom_parses: global(Metric::DomParses) - before[2],
        };

        // 封面那一份字节是解析时缩好的；落盘 + 两档缩略图是 import 的下一步。
        let cover_bytes = doc.cover().map(<[u8]>::to_vec);
        let (mut cover_write, mut thumbs) = (Duration::ZERO, Duration::ZERO);
        if let Some(bytes) = &cover_bytes {
            let mime = "image/jpeg";
            let t = Instant::now();
            let written = cover::write(&lib_root, bytes, mime).expect("封面落盘");
            cover_write = t.elapsed();
            let desc = CoverRef {
                hash: written.to_hex(),
                mime: mime.to_string(),
            };
            // 全新书库 → 文件不存在 → 这一步必然真的编一遍图。走管线现在用的一条。
            let t = Instant::now();
            thumb::ensure_all(&lib_root, &desc, bytes);
            thumbs = t.elapsed();
            shape.cover_len = bytes.len();
        }

        // 步 7：落库。
        let dest = lib_root.join(format!("{}.epub", &hash_hex[..12]));
        std::fs::rename(&readable, &dest).expect("落地");
        let t = Instant::now();
        let book_id = store.add_book(&dest, doc.meta()).expect("加行");
        let add_book = t.elapsed();
        let t = Instant::now();
        store.save_document(book_id, &*doc).expect("落库");
        let save = t.elapsed();

        if let ContentRef::Reflow(flow) = doc.content() {
            shape.chapters = flow.chapters.len();
            shape.images = flow.images.len();
            shape.toc = flow.toc.len();
            shape.spine_href_missing = flow
                .chapters
                .iter()
                .filter(|c| c.spine_href.is_empty())
                .count();
            shape.visible_chars_total = flow
                .chapters
                .iter()
                .filter_map(|c| c.char_len())
                .sum::<usize>();
        }
        shape.file_bytes = std::fs::metadata(book).map(|m| m.len()).unwrap_or(0);
        shape.title = doc.meta().title.clone();
        counters.add(&delta);

        acc.copy += copy;
        acc.sha256 += sha256;
        acc.open += open;
        acc.cover_write += cover_write;
        acc.thumbs += thumbs;
        acc.add_book += add_book;
        acc.save += save;
    }

    let n = rounds as u32;
    acc.copy /= n;
    acc.sha256 /= n;
    acc.open /= n;
    acc.cover_write /= n;
    acc.thumbs /= n;
    acc.add_book /= n;
    acc.save /= n;
    counters.container_opens /= n as u64;
    counters.name_table_passes /= n as u64;
    counters.dom_parses /= n as u64;
    (acc, counters, shape)
}

#[derive(Default)]
struct CounterDelta {
    container_opens: u64,
    name_table_passes: u64,
    dom_parses: u64,
}

impl CounterDelta {
    fn add(&mut self, other: &Self) {
        self.container_opens += other.container_opens;
        self.name_table_passes += other.name_table_passes;
        self.dom_parses += other.dom_parses;
    }
}

#[derive(Default)]
struct Shape {
    title: String,
    file_bytes: u64,
    chapters: usize,
    images: usize,
    toc: usize,
    spine_href_missing: usize,
    visible_chars_total: usize,
    cover_len: usize,
}

/// 把 `read_with` 里面那层账拆开量：逐条目分「解压 / 解析 / 缩图」。
///
/// 为什么在管线外面重做一遍：`document::open` 从外面只有一个总时间，里面哪一步占大头
/// 看不见；而这几步用的都是公开的 `format::` 入口（`shrink_image` / `Dom::parse` /
/// `TextMap::build`），照 `format/epub.rs:507` 的顺序复现一遍就能与 `open` 对上账。
/// 条目归类按扩展名近似 —— 管线自己按 manifest 的 media-type 判，两者对常规书一致。
fn per_entry(book: &Path) -> EntryReport {
    let mut rep = EntryReport::default();
    let Ok(file) = std::fs::File::open(book) else {
        return rep;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return rep;
    };

    for i in 0..zip.len() {
        let name = {
            let Ok(entry) = zip.by_index(i) else {
                continue;
            };
            entry.name().to_string()
        };
        let lower = name.to_ascii_lowercase();

        // 取字节 = 管线里的 `zip.read` / `read_text`，整本 inflate 一遍。
        let t = Instant::now();
        let bytes = {
            let Ok(mut entry) = zip.by_index(i) else {
                continue;
            };
            let mut buf = Vec::new();
            if std::io::Read::read_to_end(&mut entry, &mut buf).is_err() {
                continue;
            }
            buf
        };
        rep.inflate += t.elapsed();
        rep.inflated_bytes += bytes.len() as u64;

        let is_chapter = (lower.ends_with(".xhtml")
            || lower.ends_with(".html")
            || lower.ends_with(".htm")
            || lower.ends_with(".xml"))
            && !lower.contains(".ncx")
            && !lower.ends_with(".opf")
            && !lower.contains("container.xml");
        let is_image = [".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp"]
            .iter()
            .any(|e| lower.ends_with(e));

        if is_chapter {
            // 完整解析一遍：html5ever → Dom → `assign_paths`（`visible_chars` 的前半）。
            let t = Instant::now();
            let dom = Dom::parse(&bytes);
            let parse = t.elapsed();
            // 数一遍可见字：`TextMap::build`（`visible_chars` 的后半）。
            let body = dom
                .elements()
                .find(|(id, _)| dom.tag_name(*id) == Some("body"))
                .map_or(dom.document, |(id, _)| id);
            let t = Instant::now();
            let map = folio_core::content::anchor::TextMap::build(&dom, body);
            rep.textmap += t.elapsed();

            rep.chapters += 1;
            rep.parse += parse;
            rep.chars += map.chars();
            rep.html_bytes += bytes.len() as u64;
            if parse > rep.worst_parse {
                rep.worst_parse = parse;
                rep.worst_name = name;
                rep.worst_len = bytes.len();
            }
        } else if is_image {
            // 全书每张图都只读尺寸头（`image_dims`），不解码 —— 先按这个口径量。
            let t = Instant::now();
            let dims = image::ImageReader::new(std::io::Cursor::new(&bytes))
                .with_guessed_format()
                .ok()
                .and_then(|r| r.into_dimensions().ok());
            rep.dims += t.elapsed();
            rep.images += 1;
            rep.image_bytes += bytes.len() as u64;

            // 封面是唯一被**完整解码 + 缩 + 重编码**的那一张（`shrink_image`）。
            if lower.contains("cover") && rep.cover_shrink.is_none() {
                let t = Instant::now();
                let shrunk = shrink_image(&bytes, 512);
                rep.cover_shrink = Some(t.elapsed());
                rep.cover_raw_len = bytes.len();
                rep.cover_raw_px = dims.map(|(w, h)| (w as usize, h as usize));
                rep.cover_out_len = shrunk.map(|(b, _)| b.len()).unwrap_or(0);
            }
        }
    }
    rep
}

#[derive(Default)]
struct EntryReport {
    chapters: usize,
    images: usize,
    inflate: Duration,
    inflated_bytes: u64,
    parse: Duration,
    textmap: Duration,
    dims: Duration,
    chars: usize,
    html_bytes: u64,
    image_bytes: u64,
    worst_parse: Duration,
    worst_name: String,
    worst_len: usize,
    cover_shrink: Option<Duration>,
    cover_raw_len: usize,
    cover_raw_px: Option<(usize, usize)>,
    cover_out_len: usize,
}

impl EntryReport {
    fn total(&self) -> Duration {
        self.inflate
            + self.parse
            + self.textmap
            + self.dims
            + self.cover_shrink.unwrap_or(Duration::ZERO)
    }
}

/// 章节摘要两条路对拍 + 计时：`content::scan`（一遍标记扫描）对比 `visible_chars`（DOM）。
///
/// 这条取代了早先那个自己写分词器 sink 的探针 —— 那个探针数出来的字数与 DOM **不一致**
/// （它没有照抄树构造那几条规则），所以它给的是「另一件事快多少」。这里比的是生产的
/// 两趟真实现：不一致就直接报出来，那条一致性由 `content::scan::tests` 的逐章对拍守门。
///
/// 逐章**换着先后跑**（奇偶交替）：固定顺序下每轮先跑的那一支总在 DVFS 还没爬起来的
/// 频率上，两趟的差会被放大或缩小（`thumb_probe` 那条实测过这种假象）。
/// 回 `(DOM 总时, 扫描总时, 章数, 字数一致的章数, DOM 总字数, 扫描总字数)`。
fn scan_probe(book: &Path) -> Option<(Duration, Duration, usize, usize, usize, usize)> {
    let doc = folio_core::content::document::open(book).ok()?;
    let count = doc.flow().map(|flow| flow.chapters.len()).unwrap_or(0);
    if count == 0 {
        return None;
    }
    let mut texts = Vec::with_capacity(count);
    for ix in 0..count {
        if let Ok(text) = doc.chapter_source(ix) {
            texts.push(text);
        }
    }
    if texts.is_empty() {
        return None;
    }

    let mut via_dom = Duration::ZERO;
    let mut via_scan = Duration::ZERO;
    let mut dom_chars = 0usize;
    let mut scan_chars = 0usize;
    let mut agreed = 0usize;
    for (ix, text) in texts.iter().enumerate() {
        // 交替：一半的章先跑 DOM，一半先跑扫描。
        if ix % 2 == 0 {
            let t = Instant::now();
            let n = folio_core::content::format::visible_chars(text).unwrap_or(0);
            via_dom += t.elapsed();
            dom_chars += n;
            let t = Instant::now();
            let sum = folio_core::content::scan::chapter_summary(text);
            via_scan += t.elapsed();
            scan_chars += sum.visible_chars;
            if sum.visible_chars == n {
                agreed += 1;
            }
        } else {
            let t = Instant::now();
            let sum = folio_core::content::scan::chapter_summary(text);
            via_scan += t.elapsed();
            scan_chars += sum.visible_chars;
            let t = Instant::now();
            let n = folio_core::content::format::visible_chars(text).unwrap_or(0);
            via_dom += t.elapsed();
            dom_chars += n;
            if sum.visible_chars == n {
                agreed += 1;
            }
        }
    }
    Some((
        via_dom,
        via_scan,
        texts.len(),
        agreed,
        dom_chars,
        scan_chars,
    ))
}

/// 拆开看两档缩略图那 20 ms 花在哪：解码、缩、还是编。
///
/// 决定「复用一次解码」这个改动值多少 —— 如果大头是缩放与编码，复用解码就省不了几毫秒。
fn thumb_probe(book: &Path) {
    let Ok(doc) = document::open(book) else {
        return;
    };
    let Some(bytes) = doc.cover().map(<[u8]>::to_vec) else {
        println!("  （这本没有封面）");
        return;
    };
    println!(
        "\n  缩略图那一步拆开（封面 {} KB 的缩过那份）:",
        bytes.len() / 1024
    );
    let decode = || {
        image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .ok()
            .and_then(|r| r.decode().ok())
    };
    let t = Instant::now();
    let img = decode().expect("封面该解得开");
    let one_decode = t.elapsed();
    let _ = decode();
    let two_decode = t.elapsed();

    let t = Instant::now();
    for size in thumb::SIZES {
        let scaled = img.thumbnail(size, size);
        let rgb = scaled.to_rgb8();
        let mut out = Vec::new();
        let _ = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80).encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        );
        std::hint::black_box(out);
    }
    let block_avg = t.elapsed();

    let t = Instant::now();
    for size in thumb::SIZES {
        let longest = img.width().max(img.height());
        let scale = size as f32 / longest as f32;
        let w = ((img.width() as f32 * scale).round() as u32).max(1);
        let h = ((img.height() as f32 * scale).round() as u32).max(1);
        let scaled = img.resize_exact(w, h, image::imageops::FilterType::Triangle);
        let rgb = scaled.to_rgb8();
        let mut out = Vec::new();
        let _ = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80).encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        );
        std::hint::black_box(out);
    }
    let triangle = t.elapsed();

    println!(
        "    解码 1 次 {:>7.1} ms · 2 次（现状）{:>7.1} ms",
        ms(one_decode),
        ms(two_decode)
    );
    println!(
        "    缩+编两档 thumbnail（块平均）{:>7.1} ms · Triangle（卷积）{:>7.1} ms",
        ms(block_avg),
        ms(triangle)
    );

    // 假的 CoverRef：这一档缩略图只用来做 A/B，不进任何库。
    let desc = CoverRef {
        hash: format!("{:0>64x}", bytes.len()),
        mime: "image/jpeg".to_string(),
    };
    // 同一进程里 A/B：逐档 ensure（两次解码）vs ensure_all（一次解码供两档）。
    //
    // **顺序必须交替**：每轮先跑的那一支总在 DVFS 还没爬起来的 CPU 上，固定顺序会把
    // 「少解一次图」的 2-5 ms 报成 20 ms+（在慢存储的机器上实测过这种假象）。
    let mut per_size = Vec::new();
    let mut shared = Vec::new();
    for round in 0..PROBES {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        // 两条各用一个新目录（档在位就跳过，那样量的不是生成）。
        let measure = |root: &Path, all: bool| {
            let t = Instant::now();
            if all {
                thumb::ensure_all(root, &desc, &bytes);
            } else {
                for size in thumb::SIZES {
                    thumb::ensure(root, size, &desc, &bytes);
                }
            }
            t.elapsed()
        };
        if round % 2 == 0 {
            per_size.push(measure(a.path(), false));
            shared.push(measure(b.path(), true));
        } else {
            shared.push(measure(b.path(), true));
            per_size.push(measure(a.path(), false));
        }
    }
    let old = per_size[per_size.len() / 2];
    let new = shared[shared.len() / 2];
    println!(
        "    两档实做：逐档 ensure {:>7.1} ms  vs  ensure_all {:>7.1} ms  → 省 {:.1} ms（{:.0}%）",
        ms(old),
        ms(new),
        ms(old) - ms(new),
        (ms(old) - ms(new)) / ms(old) * 100.0
    );
}

/// 要比的缩放档。`filter = None` 是 `DynamicImage::thumbnail` 那个**整数块平均**采样器
/// （`imageops::thumbnail`，每个源像素恰好贡献给一个目标像素）；给了 `filter` 就走
/// `resize(cap, cap, filter)` —— 同一套几何（都是 `resize_dimensions` 的 `fill = false`
/// 那一支、都保比例），换的只是卷积滤波的种类。
///
/// `blur > 0.0` 的那两档是**先做一道高斯前滤波再缩**：理论上这才是抑制混叠的正经做法，
/// 代价是全图像素多算一遍卷积，所以它进的是耗时那一栏，不是免费选项。
///
/// `two_step` 那一档是块平均**缩两次**（先到 2× 目标再到位）：教科书里的廉价路子，
/// 两次盒子摺起来近似一个帐篷核。**实测是反的**：棋盘混叠 σ 从 12.69 涨到 28.00 ——
/// 第一步的比例只有 1.56×，等于几乎没低通就先把混叠折进了通带，第二步再缩也分不出来。
/// 留着这一档是为了别再有人试同一条路。它的**耗时那一栏不作数**：源比 2× 目标还小时
/// 第一步走的是放大（538×720 那本 +432% 就是这么来的），不是同一次活的两种做法。
type ResizeArm = (&'static str, Option<image::imageops::FilterType>, f32, bool);

const RESIZE_ARMS: [ResizeArm; 9] = [
    ("thumbnail（块平均·产线）", None, 0.0, false),
    (
        "Nearest",
        Some(image::imageops::FilterType::Nearest),
        0.0,
        false,
    ),
    (
        "Triangle",
        Some(image::imageops::FilterType::Triangle),
        0.0,
        false,
    ),
    (
        "CatmullRom",
        Some(image::imageops::FilterType::CatmullRom),
        0.0,
        false,
    ),
    (
        "Gaussian",
        Some(image::imageops::FilterType::Gaussian),
        0.0,
        false,
    ),
    (
        "Lanczos3",
        Some(image::imageops::FilterType::Lanczos3),
        0.0,
        false,
    ),
    ("blur 1.6 → thumbnail", None, 1.6, false),
    (
        "blur 1.6 → Lanczos3",
        Some(image::imageops::FilterType::Lanczos3),
        1.6,
        false,
    ),
    ("thumbnail ×2（两步块平均）", None, 0.0, true),
];

fn scale_arm(
    img: &image::DynamicImage,
    cap: u32,
    filter: Option<image::imageops::FilterType>,
    blur: f32,
    two_step: bool,
) -> image::DynamicImage {
    let prepared = if blur > 0.0 {
        img.blur(blur)
    } else {
        img.clone()
    };
    // 第一步先退到 2× 目标，第二步才到位：两次块平均摺起来近似一个帐篷核。
    let step = if two_step { cap * 2 } else { cap };
    let once = match filter {
        Some(f) => prepared.resize(step, step, f),
        None => prepared.thumbnail(step, step),
    };
    if step == cap {
        return once;
    }
    match filter {
        Some(f) => once.resize(cap, cap, f),
        None => once.thumbnail(cap, cap),
    }
}

/// 一帧灰度图的标准差（0-255 刻度）。
fn std_dev(img: &image::GrayImage) -> f64 {
    let n = img.as_raw().len() as f64;
    let mean = img.as_raw().iter().map(|p| u64::from(*p)).sum::<u64>() as f64 / n;
    img.as_raw()
        .iter()
        .map(|p| {
            let d = f64::from(*p) - mean;
            d * d
        })
        .sum::<f64>()
        .sqrt()
        / n.sqrt()
}

/// 单像素亮线缩完的**等效宽度**（像素）与**峰值亮度**：各列均值之和除以峰值列。
/// 宽度 1.0 = 一点没摊开；越大就是滤波把边缘抹掉的程度。
///
/// 峰值必须一起报：`Nearest` 这类采样可能把一条 1 像素的线整个**跳过**，那样宽度看着
/// 最漂亮，实际是线没了 —— 只报宽度会把这种失败读成最优。
fn edge_width(img: &image::GrayImage) -> (f64, f64) {
    let (w, h) = (img.width(), img.height());
    let mut peak = 0.0f64;
    let mut total = 0.0f64;
    for x in 0..w {
        let mut sum = 0u64;
        for y in 0..h {
            sum += u64::from(img.get_pixel(x, y)[0]);
        }
        let col = sum as f64 / h as f64;
        total += col;
        peak = peak.max(col);
    }
    if peak <= 0.0 {
        (0.0, 0.0)
    } else {
        (total / peak, peak)
    }
}

/// 缩放档位的**质量**对照：抗混叠与保锐度，两个方向互相拆台，所以一起报。
///
/// 真书封面量不出质量（没有已知答案），这里喂两张合成图：
///
/// - **棋盘**（周期 2 像素的 0/255 交替，1200×1600 缩到长边 512）：这份内容高于缩小后的
///   奈奎斯特频率，理想低通之后应当变成一片平灰 → 输出的标准差就是**混叠残留**，越小越好。
///   棋盘是混叠的最坏情况，真实封面里细发丝、书名字、网点都属同一类。
/// - **一条单像素亮线**：缩完摊成多宽就是**锐度代价**，越小越利。
///
/// 耗时不在这里量（`shrink_probe` 用真封面逐档轮转量），也不与质量换算成「值不值」——
/// 那一步留给看的人。
fn resize_probe() {
    const CAP: u32 = 512;
    let board = image::DynamicImage::ImageLuma8(image::GrayImage::from_fn(1200, 1600, |x, y| {
        image::Luma([if (x + y) % 2 == 0 { 0 } else { 255 }])
    }));
    let line = image::DynamicImage::ImageLuma8(image::GrayImage::from_fn(1200, 1600, |x, _y| {
        image::Luma([if x == 600 { 255 } else { 0 }])
    }));

    println!("\n  == 缩放档位的质量对照（棋盘 1200×1600→512；亮线同尺寸）==");
    println!(
        "    {:<24} {:>10} {:>10} {:>10}",
        "算法", "混叠 σ", "线宽 px", "线峰值"
    );
    for (label, filter, blur, two_step) in RESIZE_ARMS {
        let scaled = scale_arm(&board, CAP, filter, blur, two_step).to_luma8();
        let moire = std_dev(&scaled);
        let (sharp, peak) = edge_width(&scale_arm(&line, CAP, filter, blur, two_step).to_luma8());
        println!("    {label:<24} {moire:>10.2} {sharp:>10.2} {peak:>10.1}");
    }
    println!(
        "    （棋盘理想答案是纯平灰 σ≈0；亮线理想答案是 1.0 px 宽、峰值不丢。两栏不可能同时最优。）"
    );
}

/// 拆 `shrink_image`：解码、缩放、转 RGB、编码各占多少，缩放八档一起量耗时。
///
/// 拆开才知道该动哪一刀，也才知道纯 Rust 还能省多少、什么时候非改结构不可。
///
/// 八档**必须同一个目标口径**才谈得上比较：都按长边缩到 512、保住比例（旧版把 Triangle
/// 那条硬编码成 357×512，那是某一本书的封面尺寸，换一本书就在比两件事）。而且每轮换起点
/// —— 固定顺序下每轮先跑的那一支总在 DVFS 还没爬起来的 CPU 上，几毫秒的差会被报成
/// 几十毫秒（`thumb_probe` 那条实测过这种假象）。质量那一侧的对照见 [`resize_probe`]。
fn shrink_probe(book: &Path) {
    use image::ImageDecoder as _;
    let Ok(mut zip) = zip::ZipArchive::new(std::fs::File::open(book).unwrap()) else {
        return;
    };
    let Some(idx) = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_ascii_lowercase()))
        .position(|n| {
            n.contains("cover") && [".jpg", ".jpeg", ".png"].iter().any(|e| n.ends_with(e))
        })
    else {
        println!("  （容器里找不到封面，跳过）");
        return;
    };
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut zip.by_index(idx).unwrap(), &mut raw).unwrap();
    drop(zip);

    let t = Instant::now();
    let mut decoder = image::ImageReader::new(std::io::Cursor::new(&raw))
        .with_guessed_format()
        .unwrap()
        .into_decoder()
        .unwrap();
    std::hint::black_box(decoder.orientation().unwrap());
    let img = image::DynamicImage::from_decoder(decoder).unwrap();
    let decode = t.elapsed();
    let (w, h) = (img.width(), img.height());

    // 与 `shrink_image` 同一个目标口径：长边缩到 512、比例不动。
    // `resize(cap, cap, f)` 与旧产线那条「自己算宽高再 resize_exact」是同一件活
    // （前者内部就走 `resize_dimensions(fill=false)` → `resize_exact`），所以可以直接比。
    let cap = 512u32;
    let mut costs: Vec<Vec<Duration>> = RESIZE_ARMS.iter().map(|_| Vec::new()).collect();
    for round in 0..PROBES {
        // 每轮换起点：八档各自轮到我做「冷频上第一个跑」那一次，
        // 固定顺序会把几毫秒的差报成几十毫秒（`thumb_probe` 实测过这种假象）。
        for k in 0..RESIZE_ARMS.len() {
            let at = (k + round) % RESIZE_ARMS.len();
            let (_, filter, blur, two_step) = RESIZE_ARMS[at];
            let t = Instant::now();
            let out = scale_arm(&img, cap, filter, blur, two_step);
            costs[at].push(t.elapsed());
            std::hint::black_box(out);
        }
    }
    for v in costs.iter_mut() {
        v.sort();
    }
    let produced = img.clone().thumbnail(cap, cap);
    let baseline = ms(costs[0][costs[0].len() / 2]);

    let t = Instant::now();
    let copy = img.to_rgb8();
    let copy_d = t.elapsed();
    let t = Instant::now();
    let mut out = Vec::new();
    let rgb = produced.to_rgb8();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82)
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    let enc = t.elapsed();

    println!(
        "\n  `shrink_image` 拆开（原图 {}×{}，{} KB，{} MB 像素，缩到 {}）:",
        w,
        h,
        raw.len() / 1024,
        w as usize * h as usize * 3 / 1048576,
        {
            let (pw, ph) = (produced.width(), produced.height());
            format!("{pw}×{ph}")
        }
    );
    println!("    完整解码                  {:>8.1} ms", ms(decode));
    println!("    缩放各档（{PROBES} 轮轮转取中位，产线那一档为基准）:");
    for (i, (label, _, _, _)) in RESIZE_ARMS.iter().enumerate() {
        let at = ms(costs[i][costs[i].len() / 2]);
        println!(
            "      {label:<24} {at:>8.1} ms   {:+7.1} ms（{:+4.0}%）",
            at - baseline,
            (at - baseline) / baseline.max(0.001) * 100.0
        );
    }
    println!("    to_rgb8（整份拷一遍像素）  {:>8.1} ms", ms(copy_d));
    println!(
        "    编 512 档 JPEG             {:>8.1} ms → {} KB",
        ms(enc),
        out.len() / 1024
    );
    std::hint::black_box(copy);
}

fn pick_books(args: &[String]) -> Vec<PathBuf> {
    if !args.is_empty() {
        return args.iter().map(PathBuf::from).collect();
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    ["local/shejie-mauriac.epub", "local/斗战狂潮校对版全本.epub"]
        .iter()
        .map(|p| root.join(p))
        .filter(|p| p.is_file())
        .collect()
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let rounds: usize = argv.last().and_then(|a| a.parse().ok()).unwrap_or(3);
    let books: Vec<String> = argv
        .iter()
        .filter(|a| !a.parse::<usize>().is_ok())
        .cloned()
        .collect();
    let books = pick_books(&books);
    if books.is_empty() {
        eprintln!("没有可测的书；把路径作为参数传进来");
        std::process::exit(2);
    }

    println!("== 导入基准（{rounds} 轮，release，热页缓存）==\n");
    // 给了一批（合成语料那种）就只算批次的账：单本那些探针与并行度无关，
    // 而 `resize_probe` 一次要跑九个缩放档位，交替比较时纯属拖累。
    if books.len() > 1 {
        let (cores, _) = machine_probe();
        println!("  这台机器给并行公式的核数：{cores}");
        let runs = batch_probe(&books, rounds);
        let mut sorted = runs.clone();
        sorted.sort();
        let median = sorted[sorted.len() / 2];
        println!(
            "  一批 {} 本          {:>9.1} ms   中位 / 全轮 {:?}   每本均 {:>7.1} ms",
            books.len(),
            ms(median),
            runs.iter()
                .map(|d| format!("{:.0}", ms(*d)))
                .collect::<Vec<_>>(),
            ms(median) / books.len() as f64,
        );
        let (_, rss) = machine_probe();
        println!("  峰值 RSS            {rss:>9.1} MB");
        return;
    }
    // 缩放档位的质量对照与具体的书无关（输入是合成图），整轮跑一次就够。
    resize_probe();
    for book in &books {
        let bytes = std::fs::metadata(book).map(|m| m.len()).unwrap_or(0);
        println!("── {book:?}  {}", mb(bytes as f64));

        let e2e = end_to_end(book, rounds);
        let mut sorted = e2e.clone();
        sorted.sort();
        let median = sorted[sorted.len() / 2];
        println!(
            "  端到端（真管线）     {:>9.1} ms   中位 / 全轮 {:?}",
            ms(median),
            e2e.iter()
                .map(|d| format!("{:.0}", ms(*d)))
                .collect::<Vec<_>>()
        );

        let (st, counters, shape) = stage_breakdown(book, rounds);
        let total = st.total();
        println!(
            "\n  分阶段（每轮平均，占手工重排合计 {:.1} ms）:",
            ms(total)
        );
        report("拷贝到暂存", st.copy, total);
        report("整本 SHA-256", st.sha256, total);
        report("解析 document::open", st.open, total);
        report("封面落盘", st.cover_write, total);
        report("两档缩略图", st.thumbs, total);
        report("add_book", st.add_book, total);
        report("save_document 落库", st.save, total);

        println!(
            "\n  书的形状: {} 章 · 目录 {} 项 · 图 {} 张 · 可见字 {} · 封面 {} KB",
            shape.chapters,
            shape.toc,
            shape.images,
            shape.visible_chars_total,
            shape.cover_len / 1024
        );
        println!(
            "  计数器（一段 document::open 里）: 开容器 {} 次 · 建条目名表 {} 次 · 解析 DOM {} 次",
            counters.container_opens, counters.name_table_passes, counters.dom_parses
        );
        if shape.chapters > 0 {
            println!(
                "  → 每章解析 {:.2} 次{} · 每章落库开销 ≈ {:.1} ms",
                counters.dom_parses as f64 / shape.chapters as f64,
                if counters.dom_parses >= 2 * shape.chapters as u64 {
                    "  ← 每章解析了两遍"
                } else {
                    ""
                },
                ms(st.open) / shape.chapters as f64
            );
        }

        let ch = per_entry(book);
        let inner = ch.total();
        println!(
            "\n  `open` 里面那层账（逐条目复现，合计 {:.1} ms 对比实测 open {:.1} ms）:",
            ms(inner),
            ms(st.open)
        );
        println!(
            "  正文 {} 章 / {} · 图 {} 张 / {}",
            ch.chapters,
            mb(ch.html_bytes as f64),
            ch.images,
            mb(ch.image_bytes as f64)
        );
        report("  inflate 全部条目", ch.inflate, inner);
        report("  Dom::parse 解析正文", ch.parse, inner);
        report("  TextMap 数字数", ch.textmap, inner);
        report("  image_dims 读图尺寸头", ch.dims, inner);
        if let Some(d) = ch.cover_shrink {
            let px = ch
                .cover_raw_px
                .map(|(w, h)| format!("{w}×{h}"))
                .unwrap_or_else(|| "尺寸未知".to_string());
            println!(
                "  封面完整解码+缩+重编 {:>9.1} ms   {:>5.1}%  ← {px}, {} KB → {} KB",
                ms(d),
                if inner.is_zero() {
                    0.0
                } else {
                    ms(d) / ms(inner) * 100.0
                },
                ch.cover_raw_len / 1024,
                ch.cover_out_len / 1024
            );
        }
        if ch.chapters > 0 {
            println!(
                "  最慢一章: {}（{} KB，解析 {:.1} ms）",
                ch.worst_name,
                ch.worst_len / 1024,
                ms(ch.worst_parse)
            );
        }
        let gap = ms(st.open) - ms(inner);
        if gap > 1.0 {
            println!("  对不上账的剩余: {gap:.1} ms（OPF/目录解析、分配、落盘准备）");
        }
        open_split_probe(book, 5);
        shrink_probe(book);
        thumb_probe(book);
        if let Some((via_dom, via_scan, chapters, agreed, dc, sc)) = scan_probe(book) {
            println!(
                "\n  章节摘要两条路（{chapters} 章，逐章交替）:整树 DOM {:>7.1} ms  vs  一遍扫描 {:>7.1} ms   → {:.1}×",
                ms(via_dom),
                ms(via_scan),
                ms(via_dom) / ms(via_scan).max(0.001)
            );
            println!("  字数逐章一致:{agreed}/{chapters} 章   总量 DOM {dc} 字 vs 扫描 {sc} 字");
        }
        println!();
    }

    println!(
        "口径提醒：计时只回答「哪一段占大头」，跨机器的比较看计数器。\n\
         手机类的 CPU 单线程大约是台式机的 1/2 ~ 1/3，而这条管线一本书内部没有并行。"
    );
}

/// `Zip::open` 拆开：zip-rs 自己读中央目录，与我们在其之上再建两张表，各占多少。
///
/// 这条是给「共享中央目录索引」那个提案量上限的：worker 每开一次容器要付两份账，
/// 换成「主线程解一次 + worker 各拿句柄按偏移 pread」只能省掉**这两份的全部**，
/// 所以先看这两份各多大、乘上 worker 数还剩多少。
fn open_split_probe(book: &Path, rounds: usize) {
    use std::io::BufReader;
    let mut entries = 0usize;
    let mut zip_rs = Vec::new();
    let mut ours = Vec::new();
    for _ in 0..rounds {
        let t = Instant::now();
        let archive =
            zip::ZipArchive::new(BufReader::new(std::fs::File::open(book).unwrap())).expect("开档");
        let names: Vec<String> = archive.file_names().map(str::to_string).collect();
        let index: std::collections::HashMap<String, usize> = names
            .iter()
            .enumerate()
            .map(|(at, name)| (name.replace('\\', "/"), at))
            .collect();
        entries = names.len();
        ours.push(t.elapsed());
        std::hint::black_box(&index);
        let t2 = Instant::now();
        drop(index);
        drop(names);
        drop(archive);
        let _ = t2;
        zip_rs.push(ours[ours.len() - 1]);
    }
    // 上面那份是「zip-rs + 我们的表」合起来的计时，再单独量一次只到 `ZipArchive::new`。
    let mut only_new = Vec::new();
    for _ in 0..rounds {
        let t = Instant::now();
        let archive =
            zip::ZipArchive::new(BufReader::new(std::fs::File::open(book).unwrap())).expect("开档");
        std::hint::black_box(archive.len());
        only_new.push(t.elapsed());
    }
    zip_rs.sort();
    only_new.sort();
    let all = ms(zip_rs[zip_rs.len() / 2]);
    let just_new = ms(only_new[only_new.len() / 2]);
    println!(
        "  开容器拆开（{} 个条目，一个 worker 一次）:`ZipArchive::new` 读中央目录 {just_new:.3} ms   再加我们两张表 {all:.3} ms   → 我们那部分 {:+.3} ms",
        entries,
        all - just_new
    );
}
