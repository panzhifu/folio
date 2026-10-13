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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use folio_core::content::document::{self, ContentRef};
use folio_core::content::dom::Dom;
use folio_core::content::format::cover::{self, CoverRef};
use folio_core::content::format::{shrink_image, thumb, visible_chars};
use folio_core::infra::metrics::{Metric, global};
use folio_core::ingest::identity;
use folio_core::{ImportKind, PathsSource, Source, Store, TaskEvent, TaskManager, submit_import};

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
        let _ = std::fs::read(book).expect("读书");

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
        let t = Instant::now();
        let doc = document::open(&readable).expect("解析");
        let open = t.elapsed();
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

/// 试探:数字数到底需不需要整棵树。
///
/// `visible_chars` 现在为了一章的字数建出完整 DOM(节点 + 子表 + 路径),再走一遍
/// `TextMap` 把所有文本复制进一个新 String。可这个数字只有一格 `usize`。
/// 这里只喂 html5ever 的**分词器**,对同一批正文数一遍可见字,量两件事:
/// 快多少、数出来的字数是否一模一样(一模一样才谈得上替换)。
fn tokenizer_probe(book: &Path) -> Option<(Duration, Duration, usize, usize, usize)> {
    use html5ever::tendril::StrTendril;
    use html5ever::tokenizer::{
        BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
    };
    use std::cell::Cell;
    use std::sync::Arc;

    #[derive(Default)]
    struct Counts {
        chars: Cell<usize>,
        skip: Cell<i32>,
        in_body: Cell<bool>,
        saw_body: Cell<bool>,
    }

    struct Sink(Arc<Counts>);

    impl Sink {
        fn bump(&self, delta: i32) {
            self.0.skip.set((self.0.skip.get() + delta).max(0));
        }
    }

    impl TokenSink for Sink {
        type Handle = ();
        fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
            let hidden = |n: &str| matches!(n, "script" | "style" | "head" | "title" | "template");
            match token {
                Token::TagToken(tag) => {
                    let name = &*tag.name;
                    if name == "body" && tag.kind == TagKind::StartTag {
                        self.0.in_body.set(true);
                        self.0.saw_body.set(true);
                    }
                    if hidden(name) {
                        self.bump(if tag.kind == TagKind::StartTag { 1 } else { -1 });
                    }
                }
                Token::CharacterTokens(text) => {
                    let visible =
                        self.0.skip.get() == 0 && (!self.0.saw_body.get() || self.0.in_body.get());
                    if visible {
                        self.0.chars.set(self.0.chars.get() + text.chars().count());
                    }
                }
                _ => {}
            }
            TokenSinkResult::Continue
        }
    }

    let texts: Vec<String> = {
        let Ok(file) = std::fs::File::open(book) else {
            return None;
        };
        let Ok(mut zip) = zip::ZipArchive::new(file) else {
            return None;
        };
        let mut out = Vec::new();
        for i in 0..zip.len() {
            let name = zip.by_index(i).ok()?.name().to_ascii_lowercase();
            let hit = (name.ends_with(".xhtml")
                || name.ends_with(".html")
                || name.ends_with(".htm")
                || name.ends_with(".xml"))
                && !name.contains(".ncx")
                && !name.ends_with(".opf")
                && !name.contains("container.xml");
            if !hit {
                continue;
            }
            if let Ok(mut e) = zip.by_index(i) {
                let mut buf = String::new();
                if std::io::Read::read_to_string(&mut e, &mut buf).is_ok() {
                    out.push(buf);
                }
            }
        }
        out
    };
    if texts.is_empty() {
        return None;
    }

    let mut via_dom = Duration::ZERO;
    let mut via_tok = Duration::ZERO;
    let mut dom_chars = 0usize;
    let mut tok_chars = 0usize;
    for text in &texts {
        let t = Instant::now();
        if let Some(n) = visible_chars(text) {
            dom_chars += n;
        }
        via_dom += t.elapsed();

        let counts = Arc::new(Counts::default());
        let t = Instant::now();
        let tokenizer = Tokenizer::new(Sink(Arc::clone(&counts)), TokenizerOpts::default());
        let queue = BufferQueue::default();
        queue.push_back(StrTendril::from(text.as_str()));
        let _ = tokenizer.feed(&queue);
        tokenizer.end();
        via_tok += t.elapsed();
        tok_chars += counts.chars.get();
    }
    Some((via_dom, via_tok, texts.len(), dom_chars, tok_chars))
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
        let scaled = img.thumbnail(size as u32, size as u32);
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
    let lanczos = t.elapsed();

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
        "    缩+编两档 Lanczos3 {:>7.1} ms · Triangle {:>7.1} ms",
        ms(lanczos),
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

/// 拆 `shrink_image` 那 187 ms：解码、缩放、转 RGB、编码各占多少。
///
/// 拆开才知道该动哪一刀，也才知道纯 Rust 还能省多少、什么时候非改结构不可。
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

    let t = Instant::now();
    let exact = img
        .clone()
        .resize_exact(357, 512, image::imageops::FilterType::Triangle);
    let exact_d = t.elapsed();
    let t = Instant::now();
    let thumb = img.clone().thumbnail(512, 512);
    let thumb_d = t.elapsed();
    let t = Instant::now();
    let copy = img.to_rgb8();
    let copy_d = t.elapsed();
    let t = Instant::now();
    let mut out = Vec::new();
    let rgb = exact.to_rgb8();
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
        "\n  `shrink_image` 拆开（原图 {}×{}，{} KB，{} MB 像素）:",
        w,
        h,
        raw.len() / 1024,
        w as usize * h as usize * 3 / 1048576
    );
    println!("    完整解码                  {:>8.1} ms", ms(decode));
    println!(
        "    缩到 512：现在这条 resize_exact(Triangle) {:>7.1} ms",
        ms(exact_d)
    );
    println!(
        "    缩到 512：换成 thumbnail(Lanczos3 带快路径) {:>7.1} ms  → {} 尺寸",
        ms(thumb_d),
        {
            let (tw, th) = (thumb.width(), thumb.height());
            format!("{tw}×{th}")
        }
    );
    println!("    to_rgb8（整份拷一遍像素）  {:>8.1} ms", ms(copy_d));
    println!(
        "    编 512 档 JPEG             {:>8.1} ms → {} KB",
        ms(enc),
        out.len() / 1024
    );
    std::hint::black_box((copy, thumb));
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
        shrink_probe(book);
        shrink_probe(book);
        thumb_probe(book);
        if let Some((via_dom, via_tok, n, dc, tc)) = tokenizer_probe(book) {
            println!(
                "\n  数字数两条路({n} 章):整树解析 {:>7.1} ms  vs  只走分词器 {:>7.1} ms   → {:.1}×",
                ms(via_dom),
                ms(via_tok),
                ms(via_dom) / ms(via_tok).max(0.001)
            );
            println!(
                "  字数是否一致: {} ({} vs {})",
                if dc == tc {
                    "一致 ✓"
                } else {
                    "不一致 ✗"
                },
                dc,
                tc
            );
        }
        println!();
    }

    println!(
        "口径提醒：计时只回答「哪一段占大头」，跨机器的比较看计数器。\n\
         手机类的 CPU 单线程大约是台式机的 1/2 ~ 1/3，而这条管线一本书内部没有并行。"
    );
}
