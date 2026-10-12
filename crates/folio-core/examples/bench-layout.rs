//! 排版引擎的**回归基准**：每次优化前后跑同一个数，看有没有真的变快。
//!
//! 跑法：
//! ```text
//! cargo run -p folio-core --release --example bench-layout
//! ```
//!
//! 用**固定语料 + 固定假度量**，所以两次运行之间的差异只可能来自代码改动。
//! 度量与 `layout::tests` 同一套（汉字 1 em、拉丁 0.5 em、空格 0.25 em）：
//! 不是真字体，但两边同源，比值就是那层优化的真实收益。
//!
//! **稳定性**：每个用例跑 `RUNS` 轮取**中位数**，并在前面加一轮预热。
//! 单次 `Instant` 在 release 下抖动可达 ±20%，中位数比平均值稳。

use std::time::{Duration, Instant};

use folio_core::config::Reader;
use folio_core::css::computed::{ComputedStyle, StyledDom, cascade};
use folio_core::css::{Origin, RuleSet};
use folio_core::dom::{Dom, NodeId};
use folio_core::layout::{
    BlockCache, FontKey, LayoutCache, LayoutKey, LayoutOptions, LineMetrics, Measure, NoImages,
    layout, layout_cached, layout_with_blocks, measure_chapter, render_window,
};

/// 每批跑多少次。批内取平均，摊平单次中断。
const BATCH: usize = 20;
/// 跑多少批，取最快的批。
const BATCHES: usize = 30;
/// 预热多少批。
const WARMUP: usize = 3;
const WIDTH: f32 = 600.0;

/// 与测试同源的假度量。
struct Mock;

fn width_of(ch: char, font_size: f32) -> f32 {
    if ch == ' ' {
        font_size * 0.25
    } else if (ch as u32) >= 0x2e80 {
        font_size
    } else {
        font_size * 0.5
    }
}

impl Measure for Mock {
    fn char_width(&self, ch: char, _font: &FontKey, font_size: f32) -> f32 {
        width_of(ch, font_size)
    }

    fn run_metrics(&self, text: &str, _font: &FontKey, font_size: f32) -> LineMetrics {
        LineMetrics {
            width: text.chars().map(|ch| width_of(ch, font_size)).sum(),
            ascent: font_size * 0.8,
            descent: font_size * 0.2,
        }
    }
}

/// 中位数（保留给需要时用）
#[allow(dead_code)]
fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

/// 最小值（保留给需要时用）
#[allow(dead_code)]
fn best(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[0]
}

/// 语料：`paragraphs` 段，每段约 `chars` 个汉字。
fn chapter(paragraphs: usize, chars: usize) -> String {
    let body = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。\
                他的梦很长，长得像一辈子；醒来时窗外的雨还没停，屋檐下滴水的声音一直没断。"
        .repeat(chars / 40 + 1);
    (0..paragraphs)
        .map(|i| format!("<p>第 {i} 段：{}</p>", &body[..body.len().min(chars * 3)]))
        .collect()
}

/// 块级缓存语料：同 [`chapter`]，但第 `hot` 段带 `.hot` 类 —— 只改这一段。
/// （类名比 `:nth-child` 稳：后者在这套选择器里到底匹配几个我们不好保证。）
fn chapter_with_hot(paragraphs: usize, chars: usize, hot: usize) -> String {
    let body = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。\
                他的梦很长，长得像一辈子；醒来时窗外的雨还没停，屋檐下滴水的声音一直没断。"
        .repeat(chars / 40 + 1);
    (0..paragraphs)
        .map(|i| {
            let class = if i == hot { " class=\"hot\"" } else { "" };
            format!(
                "<p{class}>第 {i} 段：{}</p>",
                &body[..body.len().min(chars * 3)]
            )
        })
        .collect()
}

/// 长拉丁段落：断点稀疏，专门压断行回退那一支。
fn latin_chapter(paragraphs: usize, words: usize) -> String {
    let body = "the quick brown fox jumps over the lazy dog while shadows lengthen \
                across the quiet valley and the river keeps its patient course "
        .repeat(words / 24 + 1);
    (0..paragraphs)
        .map(|i| format!("<p>Para {i}: {}</p>", body))
        .collect()
}

fn setup(html: &str) -> (Dom, StyledDom, NodeId) {
    let dom = Dom::parse(html.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse("", Origin::Author),
        &ComputedStyle::default(),
    );
    let root = dom
        .elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document);
    (dom, styles, root)
}

/// 排一遍 `options`，返回**每批平均**的最快值，以及行数。
///
/// 方法：先预热几批（让 CPU 频率与分支预测稳定），再逐批计时，
/// 每批 `BATCH` 次取平均，最后取**最快的批** —— 机器上有别的负载时只会让某批变慢，
/// 所以最快的那批最接近「这段代码本身有多快」。
fn time_layout(
    dom: &Dom,
    styles: &StyledDom,
    root: NodeId,
    options: LayoutOptions,
) -> (Duration, usize) {
    let measure = Mock;
    let lines = layout(dom, styles, root, options, &measure, &NoImages)
        .lines
        .len();

    let run_batch = || {
        let t = Instant::now();
        for _ in 0..BATCH {
            let out = layout(dom, styles, root, options, &measure, &NoImages);
            std::hint::black_box(out.height);
        }
        t.elapsed() / BATCH as u32
    };
    for _ in 0..WARMUP {
        run_batch();
    }
    let mut fastest = run_batch();
    for _ in 1..BATCHES {
        let d = run_batch();
        if d < fastest {
            fastest = d;
        }
    }
    (fastest, lines)
}

fn report(label: &str, d: Duration, extra: &str) {
    println!(
        "  {label:<34} {:>8.2} ms   {extra}",
        d.as_secs_f64() * 1000.0
    );
}

fn main() {
    println!("== 排版基准（每批 {BATCH} 次取平均，{BATCHES} 批取最快）==\n");

    // ---- 中文：CJK 每字可断，走「直接落在 k」那一支 ----
    let src = chapter(600, 800);
    let (dom, styles, root) = setup(&src);
    println!("中文章：{} 字节、{} 节点", dom.nodes.len(), dom.nodes.len());
    let (full, lines) = time_layout(
        &dom,
        &styles,
        root,
        LayoutOptions {
            width: WIDTH,
            light: false,
        },
    );
    report("完整排版", full, &format!("{lines} 行"));
    let (light, _) = time_layout(
        &dom,
        &styles,
        root,
        LayoutOptions {
            width: WIDTH,
            light: true,
        },
    );
    report(
        "轻量模式",
        light,
        &format!("快 {:.1}x", full.as_secs_f64() / light.as_secs_f64()),
    );
    {
        let fh = layout(
            &dom,
            &styles,
            root,
            LayoutOptions {
                width: WIDTH,
                light: false,
            },
            &Mock,
            &NoImages,
        )
        .height;
        let lh = layout(
            &dom,
            &styles,
            root,
            LayoutOptions {
                width: WIDTH,
                light: true,
            },
            &Mock,
            &NoImages,
        )
        .height;
        println!(
            "  高度：完整 {fh:.1} / 轻量 {lh:.1}（差 {:.1} px，占 {:.2}%）",
            (fh - lh).abs(),
            (fh - lh).abs() / fh * 100.0
        );
    }

    // ---- 两阶段排版：整章只量高 + 只排首屏 ----
    {
        let opts = LayoutOptions {
            width: WIDTH,
            light: false,
        };
        // 阶段一：整章量高（不建行盒）。
        let mut t = Duration::MAX;
        let mut map = None;
        for _ in 0..50 {
            let started = Instant::now();
            let m = measure_chapter(&dom, &styles, root, opts, &Mock, &NoImages);
            std::hint::black_box(m.height);
            t = t.min(started.elapsed());
            map = Some(m);
        }
        let map = map.expect("至少跑一次");
        report("两阶段 · 整章量高", t, &format!("整章高 {:.0}", map.height));

        // 阶段二：只排首屏（视口 800px）。
        let mut t = Duration::MAX;
        for _ in 0..50 {
            let started = Instant::now();
            let win = render_window(&map, &dom, &styles, &Mock, &NoImages, 0.0, 800.0);
            std::hint::black_box(win.lines.len());
            t = t.min(started.elapsed());
        }
        report("两阶段 · 首屏渲染", t, "首屏 ~800px");
    }

    // ---- 拉丁：断点稀疏，走「回退到词边界」那一支 ----
    let (dom_l, styles_l, root_l) = setup(&latin_chapter(300, 400));
    println!("\n拉丁章：{} 节点", dom_l.nodes.len());
    let (latin, ll) = time_layout(
        &dom_l,
        &styles_l,
        root_l,
        LayoutOptions {
            width: WIDTH,
            light: false,
        },
    );
    report("完整排版（拉丁）", latin, &format!("{ll} 行"));

    // ---- 混合 ----
    let mixed = format!("{}{}", chapter(300, 800), latin_chapter(150, 400));
    let (dom_m, styles_m, root_m) = setup(&mixed);
    println!("\n混排章：{} 节点", dom_m.nodes.len());
    let (mixed_t, ml) = time_layout(
        &dom_m,
        &styles_m,
        root_m,
        LayoutOptions {
            width: WIDTH,
            light: false,
        },
    );
    report("完整排版（混排）", mixed_t, &format!("{ml} 行"));

    // ---- 缓存命中 ----
    let reader = Reader::default();
    // 键按公开构造点来 —— 与生产路径（`folio-app` 的排版作业）同一把键的形状。
    let key = LayoutKey::compose(&src, "", &reader, WIDTH, 1, 0, 0);
    let mut cache = LayoutCache::new();
    println!();
    let hit = {
        let first = layout_cached(
            &mut cache,
            &key,
            &dom,
            &styles,
            root,
            LayoutOptions {
                width: WIDTH,
                light: false,
            },
            &Mock,
            &NoImages,
        );
        std::hint::black_box(first.height);
        let mut fastest = Duration::MAX;
        for _ in 0..BATCHES {
            let t = Instant::now();
            for _ in 0..BATCH {
                let out = layout_cached(
                    &mut cache,
                    &key,
                    &dom,
                    &styles,
                    root,
                    LayoutOptions {
                        width: WIDTH,
                        light: false,
                    },
                    &Mock,
                    &NoImages,
                );
                std::hint::black_box(out.height);
            }
            let d = t.elapsed() / BATCH as u32;
            if d < fastest {
                fastest = d;
            }
        }
        fastest
    };
    report("缓存命中", hit, "");

    // ---- 块级缓存：改一段的样式，只重排那一段 ----
    //
    // 上半是「无块缓存」：改一段的样式，整章的 `LayoutCache` 键都变了 → 整章重排。
    // 下半是「有块缓存」：把缓存填成「改之前」的样子，再排改之后的版本 —— 只有那一段
    // 重排，其余块命中复用（复用会按新落点平移，所以下面几段整体下移也不影响命中）。
    // 打印的「重排 N 块 / 命中 M 块」就是这套机制是否生效的直接证据。
    {
        let src_b = chapter_with_hot(600, 800, 300);
        let (dom_b, styles_b, root_b) = setup(&src_b);
        // 只改带 `.hot` 的那一段的字号。
        let hot = cascade(
            &dom_b,
            &RuleSet::parse(".hot { font-size: 24px }", Origin::Author),
            &ComputedStyle::default(),
        );
        let opts = LayoutOptions {
            width: WIDTH,
            light: false,
        };
        let key_b = LayoutKey::compose(&src_b, "", &Reader::default(), WIDTH, 1, 0, 0);

        let (full_hot, _) = time_layout(&dom_b, &hot, root_b, opts);
        report("改一段后整章重排（无缓存）", full_hot, "");

        let mut cache = BlockCache::new();
        let mut best = Duration::MAX;
        let (mut hits, mut misses) = (0u64, 0u64);
        for _ in 0..10 {
            // 不计时：把缓存填成「改之前」的那一版。
            cache.clear();
            layout_with_blocks(
                &mut cache, &key_b, &dom_b, &styles_b, root_b, opts, &Mock, &NoImages,
            );
            let (h0, m0) = (cache.hits, cache.misses);
            let t = Instant::now();
            let out = layout_with_blocks(
                &mut cache, &key_b, &dom_b, &hot, root_b, opts, &Mock, &NoImages,
            );
            std::hint::black_box(out.height);
            best = best.min(t.elapsed());
            hits = cache.hits - h0;
            misses = cache.misses - m0;
        }
        report(
            "改一段（块级缓存）",
            best,
            &format!("重排 {misses} 块 / 命中 {hits} 块"),
        );
    }

    println!("\n记下这几个数，优化后再跑一次对比。");
}
