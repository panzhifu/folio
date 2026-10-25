//! 失效键与两级缓存的用例：`LayoutKey` 一处构造、整章 `LayoutCache`、块级 `BlockCache`。
//!
//! 从同目录的 `tests.rs` 切出来只为了一个文件能一次读完 —— 这三大块连着写有五百行，
//! 而它们钉的是同一件事：「输入没变就别重算，输入变了就必须真的重算」。
//! 共用符号（`Mono` 那份 mock 量器与上层的全部导入）都走 `use super::*`。

use super::*;
use crate::layout::{LayoutInputs, content_hash};

// ---- LayoutKey ----

/// `<body>` 节点；没有 `<body>` 的片段退回文档根，与界面那边 `body_of` 同一条规则。
fn body_of(dom: &Dom) -> NodeId {
    dom.elements()
        .find(|(id, _)| dom.tag_name(*id) == Some("body"))
        .map(|(id, _)| id)
        .unwrap_or(dom.document)
}

/// 测试用的键：只给章文本与栏宽，其余各格取「没有别的输入」那一档。
///
/// 章那一格递的是**内容哈希**而不是 `&str`：`LayoutKey::chapter` 存的是身份，而
/// 「正文已经在手上」那一条来路（`reader::document::ChapterSource::Text`）的身份就是
/// 内容哈希。这里按同一条来路取值，本文件那几条「换文本必须换键」的断言才仍然成立。
fn key_for(source: &str, width: f32) -> LayoutKey {
    LayoutKey::compose(
        LayoutInputs::compute("", &crate::infra::config::Reader::default()),
        content_hash(source),
        width,
        1,
        None,
        0,
        0,
    )
}

/// 同一份输入问两次必须得到同一把键，改一处输入必须只让键变一次。
///
/// 这是「一处构造、各级缓存共用」那条纪律的守门员：键若有了第二个构造点，
/// 两处就会算出两把不同的键，而现象是「某一格缓存永远不命中」。
#[test]
fn layout_key_is_a_pure_function_of_its_inputs() {
    let prefs = crate::infra::config::Reader::default();
    let make = |size: f32| {
        let mut p = prefs.clone();
        p.font_size = size;
        LayoutKey::compose(
            LayoutInputs::compute("p { margin: 0 }", &p),
            content_hash("<p>同一章</p>"),
            600.0,
            1,
            None,
            0,
            0,
        )
    };
    assert_eq!(make(18.0), make(18.0), "同一代内问两次该拿到同一把键");
    assert_ne!(make(18.0), make(19.0), "改字号必须换键");

    // 改字号只换 `prefs_hash` 那一格：其余各格必须逐格相等，
    // 否则「只重排样式真的变了的那些块」这条增量路径无从判断是谁失效了。
    let (a, b) = (make(18.0), make(19.0));
    assert_eq!(a.chapter, b.chapter);
    assert_eq!(a.author_css_hash, b.author_css_hash);
    assert_eq!(a.width, b.width);
    assert_eq!(a.context_rev(), b.context_rev(), "改字号不许扩大复用范围");
    assert_ne!(a.prefs_hash, b.prefs_hash);
}

/// 「动一下某项偏好」的写法：字段名 + 一个改它的函数。
///
/// 起个名字而不是把类型写在常量上：那个元组 + 函数指针的类型复杂到 clippy 会拦，
/// 而这张清单读起来比类型重要。
type PrefTweak = (&'static str, fn(&mut crate::infra::config::Reader));

/// 每项读者偏好的「动一下它」写法。清单与 `Reader` 的字段表一一对应
/// （下面那条断言守着），加字段时照着补一行。
const PREF_TWEAKS: [PrefTweak; 12] = [
    ("font_size", |p| p.font_size += 1.0),
    ("line_height", |p| p.line_height += 0.1),
    ("paragraph_gap_em", |p| p.paragraph_gap_em += 0.1),
    ("indent_em", |p| p.indent_em += 0.5),
    ("max_measure", |p| p.max_measure += 10.0),
    ("margin_x", |p| p.margin_x += 4.0),
    ("justify", |p| p.justify = !p.justify),
    ("font_family", |p| p.font_family = "serif".into()),
    ("mono_font_family", |p| p.mono_font_family = "mono2".into()),
    ("toc_density", |p| {
        p.toc_density = crate::infra::config::TocDensity::Compact
    }),
    ("toc_max_title_lines", |p| p.toc_max_title_lines += 1),
    ("toc_dim_read", |p| p.toc_dim_read = !p.toc_dim_read),
];

/// 每一项读者偏好都得进键：漏一项就是「改了设置版式不动」，而屏幕上看不出来。
///
/// 逐字段列一遍而不是只测 `font_size`：接线前那条手挑 `prefs_hash` 就漏了
/// `mono_font_family`，而等宽泛族名是折行的输入（`reader/measure.rs` 的 `gpui_font`）。
/// 现在改成「整份序列化后哈希」，这张清单的作用就变成**钉住字段表**：
/// 给 `Reader` 加了新字段而没在这儿列出来，最后那条断言会红。
#[test]
fn every_reader_preference_changes_the_key() {
    use crate::infra::config::Reader;
    let base = key_for("<p>偏好</p>", 600.0);
    for (name, tweak) in PREF_TWEAKS {
        let mut changed = Reader::default();
        tweak(&mut changed);
        let key = LayoutKey::compose(
            LayoutInputs::compute("", &changed),
            content_hash("<p>偏好</p>"),
            600.0,
            1,
            None,
            0,
            0,
        );
        assert_ne!(base.prefs_hash, key.prefs_hash, "偏好 {name} 没进键");
    }

    // 清单与 `Reader` 的序列化字段表必须一模一样（serde 的 derive 就是那张表本身）。
    let value = serde_json::to_value(Reader::default()).unwrap();
    let mut fields: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    let mut listed: Vec<&str> = PREF_TWEAKS.iter().map(|(name, _)| *name).collect();
    listed.sort_unstable();
    fields.sort_unstable();
    assert_eq!(
        listed, fields,
        "这张清单没跟上 `Reader` 的字段表：新增偏好字段要在这里补一行"
    );
}

/// 栏宽按 1 px 量化：亚像素的窗口抖动不该重排整章，但真变了 1 px 要立刻跟上。
#[test]
fn width_is_quantized_to_one_pixel() {
    let prefs = crate::infra::config::Reader::default();
    let at = |w: f32| {
        LayoutKey::compose(
            LayoutInputs::compute("", &prefs),
            content_hash("<p>宽</p>"),
            w,
            1,
            None,
            0,
            0,
        )
    };
    assert_eq!(at(600.2), at(600.4), "不到半像素的抖动不算换宽");
    assert_ne!(at(600.2), at(601.0), "整 1 px 必须换宽");
}

// ---- LayoutCache ----

#[test]
fn layout_cache_returns_cached_on_hit() {
    let source = "<p>缓存测试。</p>";
    let dom = Dom::parse(source.as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let body = body_of(&dom);
    let mut cache = LayoutCache::new();

    let opts = LayoutOptions {
        width: 600.0,
        light: false,
        allow_vertical: false,
        page_height: None,
    };
    let key = key_for(source, 600.0);

    // 第一次：未命中
    {
        let r1 = layout_cached(
            &mut cache, &key, &dom, &styles, body, opts, &Mono, &NoImages,
        );
        assert_eq!(r1.lines.len(), 1);
    }

    // 第二次：命中缓存
    let r2 = layout_cached(
        &mut cache, &key, &dom, &styles, body, opts, &Mono, &NoImages,
    );
    assert_eq!(r2.lines.len(), 1);
    assert!(cache.has(&key, false), "同一把键问过就该在里面");
}

#[test]
fn layout_cache_misses_on_width_change() {
    let source = "<p>宽度变化测试。</p>";
    let dom = Dom::parse(source.as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let body = body_of(&dom);
    let mut cache = LayoutCache::new();

    let _ = layout_cached(
        &mut cache,
        &key_for(source, 600.0),
        &dom,
        &styles,
        body,
        LayoutOptions {
            width: 600.0,
            light: false,
            allow_vertical: false,
            page_height: None,
        },
        &Mono,
        &NoImages,
    );
    let r2 = layout_cached(
        &mut cache,
        &key_for(source, 300.0),
        &dom,
        &styles,
        body,
        LayoutOptions {
            width: 300.0,
            light: false,
            allow_vertical: false,
            page_height: None,
        },
        &Mono,
        &NoImages,
    );
    assert_eq!(r2.width, 300.0 - 0.0); // 新宽度
}

/// `light` 与完整排版是两个产物：同一把键也不许互拿。
///
/// 轻量档不产行盒（[`measure_chapter`](crate::layout::measure_chapter) 那一路），
/// 界面要是拿它当完整排版用，屏幕上就是一个字都没有 —— 而缓存看起来是「命中」的。
#[test]
fn layout_cache_keeps_light_and_full_apart() {
    let source = "<p>两档。</p>";
    let dom = Dom::parse(source.as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let body = body_of(&dom);
    let mut cache = LayoutCache::new();
    let key = key_for(source, 600.0);

    let _ = layout_cached(
        &mut cache,
        &key,
        &dom,
        &styles,
        body,
        LayoutOptions {
            width: 600.0,
            light: true,
            allow_vertical: false,
            page_height: None,
        },
        &Mono,
        &NoImages,
    );
    assert!(cache.has(&key, true), "轻量档记的是自己那一格");
    assert!(!cache.has(&key, false), "完整档那格还不存在");

    let full = layout_cached(
        &mut cache,
        &key,
        &dom,
        &styles,
        body,
        LayoutOptions {
            width: 600.0,
            light: false,
            allow_vertical: false,
            page_height: None,
        },
        &Mono,
        &NoImages,
    );
    assert_eq!(full.lines.len(), 1, "轻量档的行盒是空的，这里必须真排过");

    cache.invalidate();
    assert!(!cache.has(&key, false));
}

/// 度量口径（[`METRICS_REV`]）变了，一切已缓存的几何都不再可信。
///
/// 这一格是**手工**维护的：改 `Measure` 的口径（断行规则、基线算法、shaping 规则）
/// 时忘了 +1，编译器不会提醒 —— 所以用这条断言把它钉在键上。
#[test]
fn layout_cache_misses_when_metrics_revision_differs() {
    let source = "<p>口径。</p>";
    let dom = Dom::parse(source.as_bytes());
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let body = body_of(&dom);
    let prefs = crate::infra::config::Reader::default();
    let mut cache = LayoutCache::new();
    let key = LayoutKey::compose(
        LayoutInputs::compute("", &prefs),
        content_hash(source),
        600.0,
        1,
        None,
        0,
        0,
    );
    assert_eq!(key.metrics_rev, METRICS_REV, "键必须带上当前口径版本");

    let _ = layout_cached(
        &mut cache,
        &key,
        &dom,
        &styles,
        body,
        LayoutOptions {
            width: 600.0,
            light: false,
            allow_vertical: false,
            page_height: None,
        },
        &Mono,
        &NoImages,
    );
    assert!(cache.has(&key, false));

    let mut bumped = key;
    bumped.metrics_rev = METRICS_REV + 1;
    assert!(
        !cache.has(&bumped, false),
        "口径变了就不许从整章缓存里拿旧行盒"
    );
}

/// 长中文文本进键不许 panic —— 按字节切会把一个汉字切成两半。
#[test]
fn long_non_ascii_source_hashes_without_panics() {
    let long = "字".repeat(500);
    let source = format!("<p>{long}</p>");
    // 不 panic 即通过；顺便钉一下两次哈希一致。
    let a = key_for(&source, 600.0).chapter;
    let b = key_for(&source, 600.0).chapter;
    assert_eq!(a, b, "同一份文本两次 hash 必须一样");
    // 尾部多一个字符就得换键：哈希不能只看前若干字节。
    assert_ne!(a, key_for(&format!("{source} "), 600.0).chapter);
}

/// 键的输入哈希：**建一次，问两百次键，一次都不许重算**。
///
/// 这条钉的是「哈希算一次存下来」这件事本身。改之前 `LayoutKey::compose` 收的是
/// `&str` 与 `&Reader`，于是界面的 `layout_key()` **每帧、每个进链的章**都要把
/// 「整本书的 CSS 走一遍 SipHash」与「整份 `Reader` 用 `serde_json` 序列化一遍再哈希
/// （带一次堆分配）」各重付一次 —— 而那两样输入根本不随帧变。
/// 把 `compose` 退回成收原文、在里面现算哈希，下面那一格增量就从 0 变成 200。
///
/// 另一半钉的是**不能省掉重算**：`set_prefs` 必须真的刷新 `prefs_hash`。缓存化之后
/// 新引入的唯一坏法就是「输入变了而缓存没跟着变」，那会表现成「改了字号而某一屏还是
/// 旧行盒」—— 屏幕上看不出来，只在进度与锚点上飘。界面侧的对头是
/// `reader::document::tests` 里那条「改字号必须换键」（它拿一个 `Document` 实例走
/// `set_prefs`，正是这一格的下游）。
#[test]
fn the_layout_inputs_hash_once() {
    use crate::infra::metrics::{Metric, take_local};

    let prefs = crate::infra::config::Reader::default();
    take_local(Metric::LayoutInputHashes);

    let mut inputs = LayoutInputs::compute("p { margin: 0 }", &prefs);
    assert_eq!(
        take_local(Metric::LayoutInputHashes),
        1,
        "建一份输入正好算一次哈希"
    );

    // 连问两百次键：一次哈希都不许再发生。
    for w in 0..200u32 {
        let _ = LayoutKey::compose(inputs, 7, 600.0 + w as f32, 1, None, 0, 0);
    }
    assert_eq!(
        take_local(Metric::LayoutInputHashes),
        0,
        "构图不许重算输入的哈希 —— 那两格算过一次就存下了"
    );

    // 输入真的变了就必须重算，而且要算得出来。
    let before = inputs.prefs_hash;
    let mut bigger = prefs.clone();
    bigger.font_size += 4.0;
    inputs.set_prefs(&bigger);
    assert_eq!(
        take_local(Metric::LayoutInputHashes),
        1,
        "`set_prefs` 是唯一的合法重算时机，且只重算偏好那一格"
    );
    assert_ne!(inputs.prefs_hash, before, "换了字号必须换出新的偏好哈希");
}

// ---- BlockCache ----

/// 一个平铺章节：`<body>` 下三个段落（第二个带 `.hot`），便于断言「只重排某一块」。
///
/// 源文本单独一条常量：`flat_three` 建的 DOM 与下面几条测试建的键都从它来，
/// 两处必须认同一份文本，否则键与 DOM 就不是同一章的。
const FLAT_SOURCE: &str = "<p>第一段</p><p class=\"hot\">第二段</p><p>第三段</p>";

/// 上面那章的 DOM 与 `<body>`。
fn flat_three() -> (Dom, NodeId) {
    let dom = Dom::parse(FLAT_SOURCE.as_bytes());
    let body = body_of(&dom);
    (dom, body)
}

/// 两份 `Layout` 的行盒几何与总高必须逐行一致。
fn assert_same_lines(a: &Layout, b: &Layout) {
    assert_eq!(a.lines.len(), b.lines.len(), "行数一致");
    for (x, y) in a.lines.iter().zip(&b.lines) {
        assert_eq!(
            (x.x, x.y, x.width, x.height),
            (y.x, y.y, y.width, y.height),
            "行几何一致"
        );
    }
    assert_eq!(a.height, b.height, "总高一致");
}

/// 同一份输入、同一个缓存连排两次：第二次全命中，输出与直接排版逐行一致。
#[test]
fn block_cache_reuses_and_matches_fresh_layout() {
    let (dom, body) = flat_three();
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let opts = LayoutOptions {
        width: 300.0,
        light: false,
        allow_vertical: false,
        page_height: None,
    };
    let key = key_for(FLAT_SOURCE, 300.0);
    let fresh = layout(&dom, &styles, body, opts, &Mono, &NoImages);

    let mut cache = BlockCache::new();
    let first = layout_with_blocks(
        &mut cache, &key, &dom, &styles, body, opts, &Mono, &NoImages,
    );
    assert_same_lines(&first, &fresh);
    assert_eq!(cache.misses, 3, "首次排版三个段落都没命中");

    let (hits0, misses0) = (cache.hits, cache.misses);
    let second = layout_with_blocks(
        &mut cache, &key, &dom, &styles, body, opts, &Mono, &NoImages,
    );
    assert_same_lines(&second, &fresh);
    assert_eq!(cache.misses, misses0, "第二次不该再有未命中");
    assert_eq!(cache.hits - hits0, 3, "第二次三块全命中");
}

/// 带浮动的块缓存必须把**浮动表一起搬回来**。
///
/// 命中那一趟不跑 `children()`，于是没人往浮动表里填 —— 症状是滚回去之后绕文消失，
/// 图旁边那几行又写回整幅宽。这里的形状是「一个容器，里面只有一张浮动的图」，
/// 容器本身被整块缓存，后面那段绕文就在容器之外：浮动表搬不回来，那段就直接重排。
#[test]
fn a_reused_block_replays_the_floats_it_placed() {
    let html = format!(
        "<div class=box><div class=i><img src=a></div></div><p>{}</p>",
        "字".repeat(40)
    );
    let css = ".i { float: left; margin-right: 20px } img { width: 100px }";
    let images = MockImages(vec![("a", 200.0, 100.0)]);
    let dom = crate::content::dom::Dom::parse(html.as_bytes());
    let styles = cascade(
        &dom,
        &RuleSet::parse(css, Origin::Author),
        &ComputedStyle::default(),
        "",
    );
    let body = body_of(&dom);
    let opts = LayoutOptions {
        width: 200.0,
        light: false,
        allow_vertical: false,
        page_height: None,
    };
    let key = key_for(&html, 200.0);
    let fresh = layout(&dom, &styles, body, opts, &Mono, &images);
    // 绕文确实存在，否则这条测试是在空转：压在图旁边的行从 120px 起。
    assert!(
        fresh
            .lines
            .iter()
            .any(|line| line.x == 120.0 && line.text_runs().next().is_some()),
        "没有一行绕开浮动图，测的就不是搬运"
    );

    let mut cache = BlockCache::new();
    let first = layout_with_blocks(&mut cache, &key, &dom, &styles, body, opts, &Mono, &images);
    assert_same_lines(&first, &fresh);
    let second = layout_with_blocks(&mut cache, &key, &dom, &styles, body, opts, &Mono, &images);
    assert_same_lines(&second, &fresh);
}

/// 只改一个段落的样式：只有那一个块失效（平铺结构下未命中恰好 +1），其余命中 ——
/// 这就是「改一处只重排一块」。第三段虽然因为第二段变高而整体下移，仍走命中路径
/// （复用时会按新落点平移）。
#[test]
fn block_cache_only_relayouts_the_changed_block() {
    let (dom, body) = flat_three();
    let opts = LayoutOptions {
        width: 300.0,
        light: false,
        allow_vertical: false,
        page_height: None,
    };
    let key = key_for(FLAT_SOURCE, 300.0);

    let plain = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let mut cache = BlockCache::new();
    layout_with_blocks(&mut cache, &key, &dom, &plain, body, opts, &Mono, &NoImages);

    // 只给 `.hot`（第二段）加字号。
    let hot = cascade(
        &dom,
        &RuleSet::parse(".hot { font-size: 28px }", Origin::Author),
        &ComputedStyle::default(),
        "",
    );
    let (hits0, misses0) = (cache.hits, cache.misses);
    let out = layout_with_blocks(&mut cache, &key, &dom, &hot, body, opts, &Mono, &NoImages);
    assert_eq!(cache.misses - misses0, 1, "只有样式变的那一块重排");
    assert_eq!(cache.hits - hits0, 2, "另外两块命中");

    // 输出的行数、总高应与「不缓存地按新样式实排」一致。
    let hot_fresh = layout(&dom, &hot, body, opts, &Mono, &NoImages);
    assert_same_lines(&out, &hot_fresh);
}

/// 复用范围变了就整清缓存：换章、换字体表、换度量口径三格都要能守住。
///
/// 这条是接线时补的一格。旧写法只比对 `dom_hash`（章文本），于是「注册了新的内嵌字体」
/// 或「断行口径改了」都不会清缓存，而块键里的样式哈希**没变** —— 复用出来的行宽是
/// 旧字体表算的，屏幕上就是字挤在一起或溢出。现在这三格都在
/// [`context_rev`](LayoutKey::context_rev) 里。
#[test]
fn block_cache_clears_when_the_context_changes() {
    let (dom, body) = flat_three();
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let opts = LayoutOptions {
        width: 300.0,
        light: false,
        allow_vertical: false,
        page_height: None,
    };
    let base = key_for(FLAT_SOURCE, 300.0);

    for (what, changed) in [
        ("章文本", {
            let mut k = base;
            k.chapter ^= 1;
            k
        }),
        ("图片索引", {
            let mut k = base;
            k.images_rev += 1;
            k
        }),
        ("字体集", {
            let mut k = base;
            k.fonts_rev += 1;
            k
        }),
        ("度量口径", {
            let mut k = base;
            k.metrics_rev += 1;
            k
        }),
    ] {
        let mut cache = BlockCache::new();
        layout_with_blocks(
            &mut cache, &base, &dom, &styles, body, opts, &Mono, &NoImages,
        );
        let misses0 = cache.misses;
        assert_eq!(misses0, 3, "第一趟三块都该实排：{what}");
        // 同一份 DOM、同一份样式，只换那一格 → 三块全部重排。
        layout_with_blocks(
            &mut cache, &changed, &dom, &styles, body, opts, &Mono, &NoImages,
        );
        assert_eq!(cache.misses - misses0, 3, "换了{what}就不许有块被复用");
        // 反面：那一格没变时不许清 —— 否则「改字号只重排样式变了的块」这条增量就假了。
        let misses1 = cache.misses;
        layout_with_blocks(
            &mut cache, &changed, &dom, &styles, body, opts, &Mono, &NoImages,
        );
        assert_eq!(
            cache.misses, misses1,
            "同一把键连排两次不该有未命中：{what}"
        );
    }
}

/// 改一次读者偏好：键换了（`prefs_hash` 那格），但**复用范围没换**，
/// 于是样式未变的块照样命中。这正是「改字号只重排受影响的那些块」在生产路径上的形状。
#[test]
fn block_cache_survives_a_preference_change() {
    let (dom, body) = flat_three();
    let opts = LayoutOptions {
        width: 300.0,
        light: false,
        allow_vertical: false,
        page_height: None,
    };
    let plain_prefs = crate::infra::config::Reader::default();
    let plain = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let key0 = LayoutKey::compose(
        LayoutInputs::compute("", &plain_prefs),
        content_hash(FLAT_SOURCE),
        300.0,
        1,
        None,
        0,
        0,
    );

    let mut cache = BlockCache::new();
    layout_with_blocks(
        &mut cache, &key0, &dom, &plain, body, opts, &Mono, &NoImages,
    );
    assert_eq!(cache.misses, 3);

    // 字号改了 → UA 表算出的每个块样式都变，三块都重排；但缓存**没被整清**：
    // 键的 `context_rev` 相同，块键是「节点 + 子树样式哈希 + 宽」，样式哈希变了自然未命中。
    let mut bigger = plain_prefs.clone();
    bigger.font_size += 4.0;
    let key1 = LayoutKey::compose(
        LayoutInputs::compute("", &bigger),
        content_hash(FLAT_SOURCE),
        300.0,
        1,
        None,
        0,
        0,
    );
    assert_eq!(
        key0.context_rev(),
        key1.context_rev(),
        "改偏好不许换复用范围"
    );
    let hot = cascade(
        &dom,
        &RuleSet::default(),
        &ComputedStyle {
            font_size: bigger.font_size,
            ..ComputedStyle::default()
        },
        "",
    );
    layout_with_blocks(&mut cache, &key1, &dom, &hot, body, opts, &Mono, &NoImages);
    assert_eq!(
        cache.misses, 6,
        "样式全变了就三块都重排，但不是因为缓存被清"
    );

    // 再来一次同键：三块全命中，说明上一次存进去的条目还在（没被整清）。
    let hits0 = cache.hits;
    layout_with_blocks(&mut cache, &key1, &dom, &hot, body, opts, &Mono, &NoImages);
    assert_eq!(cache.hits - hits0, 3, "同键重排该三块全命中");
}

/// 超容量就按 LRU 淘汰。
#[test]
fn block_cache_evicts_over_capacity() {
    let (dom, body) = flat_three();
    let styles = cascade(&dom, &RuleSet::default(), &ComputedStyle::default(), "");
    let opts = LayoutOptions {
        width: 300.0,
        light: false,
        allow_vertical: false,
        page_height: None,
    };
    let mut cache = BlockCache::with_capacity(2);
    layout_with_blocks(
        &mut cache,
        &key_for(FLAT_SOURCE, 300.0),
        &dom,
        &styles,
        body,
        opts,
        &Mono,
        &NoImages,
    );
    assert_eq!(cache.len(), 2, "三个块该挤到只剩容量上限的两个");
}
