//! 搜索这一层的门。
//!
//! 钉的全是**坐标**那一件事：命中报出来的那几个字，跳过去必须正好是那几个字。
//! 体积与耗时都验不出来这个错 —— 偏移挪半格，屏上指到的就是旁边那个词，
//! 而列表里那句上下文照样读得通（它是另一把尺量出来的）。

use std::ops::Range;
use std::path::{Path, PathBuf};

use super::{Found, Hit, Options, Query, fold, scan, scan_book_at, scan_chapter};
use crate::content::document::{Document, from_content};
use crate::content::format::epub::tests::published_shape_epub;
use crate::content::format::{Chapter, Content, DocMeta, FlowDoc, visible_text};
use crate::infra::task::StageCtx;

/// 一本正文就在手上的书：每对 `(章名, XHTML)` 一章。
fn book(chapters: &[(&str, &str)]) -> Box<dyn Document> {
    let flow = FlowDoc {
        chapters: chapters
            .iter()
            .map(|(title, source)| Chapter::inline(*title, *source))
            .collect(),
        ..Default::default()
    };
    from_content(
        DocMeta {
            title: "测试书".into(),
            ..Default::default()
        },
        Content::Reflow(flow),
    )
}

/// 跑一趟扫描（没人看进度、没按取消的那一档）。
fn sweep(doc: &dyn Document, raw: &str) -> Found {
    sweep_with(doc, raw, &Options::default(), &StageCtx::detached())
}

fn sweep_with(doc: &dyn Document, raw: &str, options: &Options, ctx: &StageCtx) -> Found {
    let query = Query::parse(raw).expect("前置：查询不该是空的");
    scan(doc, &query, options, ctx)
}

/// 按 `Hit` 报出来的那两个数从可见文本里切回去。切的应当正是命中的那几个字。
fn slice(text: &str, hit: &Hit) -> String {
    between(text, hit.start..hit.end)
}

/// 按**字符**区间取一段（`String` 的切片是字节，中文一处差半个字就 panic）。
fn between(text: &str, chars: Range<usize>) -> String {
    text.chars()
        .skip(chars.start)
        .take(chars.end - chars.start)
        .collect()
}

/// 命中报的偏移是**可见文本**的字符数，不是源文本的字节数。
///
/// 这一条是整个搜索层的地基：`Locator.offset`、`chapters.char_len`、锚点的 `start`
/// 全在这一把尺上（见 `plans/Locator.md`）。它错了，症状是「搜『猫』、跳过去屏上是
/// 『狗』」—— 而列表里那句上下文照样读得通，因为它是从同一章折出来的。
#[test]
fn a_hit_lands_on_the_visible_char_axis() {
    let doc = book(&[("第一章", "<p>她的猫，睡了。</p>")]);
    let text = visible_text("<p>她的猫，睡了。</p>");
    assert_eq!(text, "她的猫，睡了。", "前置：可见文本");

    let out = sweep(doc.as_ref(), "猫");
    assert_eq!(out.hits.len(), 1);
    let hit = &out.hits[0];
    assert_eq!((hit.chapter, hit.start, hit.end), (0, 2, 3));
    assert_eq!(
        slice(text.as_str(), hit),
        "猫",
        "按报出来的偏移切回去，就是那一个字"
    );
}

/// 同一段话，标记写得繁简不同，命中不许挪地方。
///
/// 走源文本字节那条路一定会挪：多一个 `<span>` 就多几个字节，而读者看到的字一个没变。
#[test]
fn markup_does_not_move_a_hit() {
    let plain = "<p>她的猫睡了</p>";
    let fussy = "<div><p><span class=\"a\">她的</span><em>猫</em>睡了</p></div>";
    let a = sweep(book(&[("第一章", plain)]).as_ref(), "猫");
    let b = sweep(book(&[("第一章", fussy)]).as_ref(), "猫");
    assert_eq!(a.hits, b.hits, "两段的可见文本逐字相同");
    assert_eq!(b.hits[0].start, 2);
}

/// 两个字的中文查询照样命中 —— 这一条是「不建索引」那条路白拿的东西。
///
/// FTS5 的 `trigram` 对 `MATCH '一个'` 实测**静默返回 0 行**（不报错），而中文里两个字的
/// 词占一大半：那一条路要么改成 `LIKE` 全表扫，要么让读者的查询凭空落空。
#[test]
fn two_chinese_characters_match() {
    let doc = book(&[("第一章", "<p>一个人来了，又一个人在等。</p>")]);
    let out = sweep(doc.as_ref(), "一个");
    assert_eq!(out.hits.len(), 2, "两处『一个』都该报出来");
    assert_eq!(out.hits[0].chapter, 0);
}

/// 拉丁的大小写不必写对，而偏移仍按原文那几个字算。
#[test]
fn latin_case_is_not_part_of_the_query() {
    let doc = book(&[("ch", "<p>The cat sat on the mat.</p>")]);
    for raw in ["cat", "Cat", "CAT"] {
        let out = sweep(doc.as_ref(), raw);
        assert_eq!(out.hits.len(), 1, "{raw:?} 该命中那一只猫");
        assert_eq!(
            (out.hits[0].start, out.hits[0].end),
            (4, 7),
            "{raw:?} 落在第 4 个字，而报出来的区间是原文那三个小写字母"
        );
    }
    // 同一句里两处 the：一处句首大写、一处全小写，都算命中。
    let out = sweep(doc.as_ref(), "the");
    let starts: Vec<usize> = out.hits.iter().map(|h| h.start).collect();
    assert_eq!(starts, vec![0, 15]);
}

/// 折过去会多出一个字符的那些字**不折**，为的是下标不挪位。
#[test]
fn a_fold_that_would_add_a_char_is_left_alone() {
    assert_eq!(fold('A'), 'a', "一对一的照常折");
    assert_eq!(fold('İ'), 'İ', "小写是『i + 组合点』两个字符，折了就挪位");
    assert_eq!(fold('猫'), '猫', "没有大小写的字原样");
    // 查询里带这种折不动的字，报出来的偏移仍要切得回去。
    let doc = book(&[("ch", "<p>İstanbul 见 İ 一面。</p>")]);
    let out = sweep(doc.as_ref(), "İ");
    let text = visible_text("<p>İstanbul 见 İ 一面。</p>");
    assert_eq!(
        out.hits.len(),
        2,
        "两处 İ 都该命中（'I' 那三个拉丁字母不算）"
    );
    for hit in &out.hits {
        assert_eq!(slice(&text, hit), "İ");
    }
}

/// 两条路（按字节 / 逐字符折叠）报的都是**不重叠**的命中，位置一致。
#[test]
fn hits_do_not_overlap_on_both_paths() {
    let doc = book(&[("ch", "<p>aaaa</p>")]);
    let exact = sweep(doc.as_ref(), "aa");
    let folded = sweep(doc.as_ref(), "AA");
    let exact_starts: Vec<usize> = exact.hits.iter().map(|h| h.start).collect();
    let folded_starts: Vec<usize> = folded.hits.iter().map(|h| h.start).collect();
    assert_eq!(exact_starts, vec![0, 2], "『aa』在『aaaa』里两处，不是三处");
    assert_eq!(
        folded_starts, exact_starts,
        "折叠那一条与按字节那一条同一份结果"
    );
}

/// 到上限就收手，并且**只说**「没扫完」—— 不说还剩几处。
#[test]
fn the_cap_stops_the_sweep_and_says_so() {
    let doc = book(&[
        ("一", "<p>猫猫猫猫猫</p>"),
        ("二", "<p>猫猫猫猫猫</p>"),
        ("三", "<p>猫猫</p>"),
    ]);
    let out = sweep_with(
        doc.as_ref(),
        "猫",
        &Options {
            max_hits: 2,
            ..Options::default()
        },
        &StageCtx::detached(),
    );
    assert_eq!(out.hits.len(), 2);
    assert!(out.truncated, "到上限收的手该报出来");
    assert_eq!(out.scanned, 1, "第一章就满了，第二、三章压根没读");
    assert_eq!(
        out.chapters, 3,
        "总章数照报 —— 界面要说『前 2 处 / 共 3 章』"
    );
}

/// 取消在**每一章的开头**看一眼，于是它既停得下来，也不会停在半章上。
#[test]
fn a_triggered_cancel_stops_at_the_first_chapter() {
    let doc = book(&[("一", "<p>猫</p>"), ("二", "<p>猫</p>")]);
    let ctx = StageCtx::detached();
    ctx.cancel().trigger();
    let out = sweep_with(doc.as_ref(), "猫", &Options::default(), &ctx);
    assert!(out.cancelled, "置了旗标就该以取消收尾");
    assert_eq!((out.scanned, out.hits.len()), (0, 0), "一章都不该扫");
}

/// 没置旗标时取消那一格是 false —— 上面那条把判定写反了才拦得住。
#[test]
fn an_uncancelled_sweep_says_so() {
    let doc = book(&[("一", "<p>猫</p>")]);
    let out = sweep(doc.as_ref(), "猫");
    assert!(!out.cancelled && !out.truncated);
    assert_eq!((out.scanned, out.chapters), (1, 1));
}

/// 全库那一档自己取正文，用的就是 [`scan_chapter`] 这一句：判据必须与整本那一趟同一份。
#[test]
fn one_chapter_scans_on_its_own() {
    let text = visible_text("<p>她的猫睡了</p>");
    let mut hits = Vec::new();
    let full = scan_chapter(
        7,
        &text,
        &Query::parse("猫").unwrap(),
        &Options::default(),
        &mut hits,
    );
    assert!(!full, "没到上限");
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].chapter, hits[0].start), (7, 2), "章号由调用方给");
    assert_eq!(slice(&text, &hits[0]), "猫");
}

/// 上下文里命中的那一段要能单独上色，两端裁掉处补 `…`。
#[test]
fn a_snippet_marks_the_match_and_narrows_the_ends() {
    let long = format!("<p>前{}猫后{}</p>", "一".repeat(40), "二".repeat(40));
    let doc = book(&[("第一章", &long)]);
    let hit = &sweep(doc.as_ref(), "猫").hits[0];
    assert_eq!(
        between(&hit.snippet, hit.mark.clone()),
        "猫",
        "mark 切回来正是命中的那几个字"
    );
    assert!(
        hit.snippet.starts_with('…') && hit.snippet.ends_with('…'),
        "两头都裁了：{}",
        hit.snippet
    );
    let budget = Options::default();
    assert!(
        hit.snippet.chars().count() <= budget.before + 1 + 1 + budget.after + 1,
        "一句上下文不该长过那两档之和：{}",
        hit.snippet.chars().count()
    );

    // 整章短到裁不动时不补 `…`，也不该把首尾的字吃掉。
    let short = book(&[("第一章", "<p>她的猫睡了</p>")]);
    let hit = &sweep(short.as_ref(), "猫").hits[0];
    assert_eq!(hit.snippet, "她的猫睡了", "没裁就不假装有省略号");
    assert_eq!(between(&hit.snippet, hit.mark.clone()), "猫");
}

/// 段首缩进（全角空格）与换行折成一个普通空格：列表那一行只摆得下一句。
#[test]
fn whitespace_in_a_snippet_is_flattened() {
    let doc = book(&[("第一章", "<p>　　夜来了。\n　　猫睡了。</p>")]);
    let hit = &sweep(doc.as_ref(), "猫").hits[0];
    assert_eq!(hit.snippet, "夜来了。 猫睡了。", "换行与缩进都成一个空格");
    assert!(!hit.snippet.contains("  "), "连着几处空白折成一个");
}

/// 只有空白的查询算「没在搜」，而不是一趟把所有正文读一遍。
#[test]
fn an_empty_query_is_not_a_search() {
    assert!(Query::parse("").is_none());
    assert!(Query::parse("  \n 　").is_none(), "全角空格也是空白");
    let q = Query::parse(" 猫 ").expect("两头空白去掉之后还有字");
    assert_eq!(q.as_str(), "猫", "查询去过两头空白");
    assert!(!q.folds_case(), "中文查询走按字节那一条快路");
    assert!(Query::parse(" Cat ").expect("有字").folds_case());
}

/// 全库那一档的基本单位：给一个路径，自己开容器扫一本。
///
/// 这里摆两本真的书（各在一个临时目录里），为的是钉住**逐本**这件事的形状：每一本各自的
/// `Found`、各自的章号与偏移，而不是把两本的字混成一条串。偏移的切回验与整本那一档共用
/// 同一条判据（`Locator.offset` 那把尺）。
#[test]
fn one_book_per_path_with_its_own_hits() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let paths = [published_shape_epub(&first), published_shape_epub(&second)];
    let query = Query::parse("正文").expect("前置：查询不可能是空的");
    for path in &paths {
        let found = scan_book_at(path, &query, &Options::default(), &StageCtx::detached())
            .expect("合成书该扫得动");
        assert_eq!(found.chapters, 28, "前置：一本 28 章的合成书");
        assert_eq!(found.hits.len(), 28, "每章那句『第 N 章的正文。』都该命中");
        // 章号与偏移属于**这一本**：与另一本的行号毫无关系，所以逐本提交是干净的。
        assert_eq!(found.hits[0].chapter, 0);
        assert_eq!(found.hits[27].chapter, 27);
    }

    // 坏掉的那一本（路径不存在 = 文件被换掉 / 同步走了）回 `None`：架上有一本读不了，
    // 不该让整架的搜索停下来。
    let gone = paths[0].parent().unwrap().join("no-such-book.epub");
    assert!(
        scan_book_at(&gone, &query, &Options::default(), &StageCtx::detached()).is_none(),
        "开不了的那一本该回 None，而不是 panic 也不是空命中"
    );
}

/// 跨格式的一扇真门：同一本 CHM 的两份码页（UTF-8 与 GBK），搜同一个词要给出**同一个答案**。
///
/// CHM 那一层的最大一坑写在 `tests/fixtures/oss/chm/README.md` 里：真实中文书九成按 GBK
/// 存，而 `String::from_utf8` 那一族的失败方式是一排问号**且不报错** —— 那种书能打开、能排版、
/// 读者看得见字，只有搜索会露馅（问号串里没有那几个字）。这一条把「搜得到」与「两码页同一个
/// 答案」钉在同一处，比只断言「解出来是不是同一串汉字」更接近读者的那一下。
#[test]
fn the_same_book_on_two_codepages_answers_the_same() {
    let query = Query::parse("天地玄黄").expect("前置：查询不可能是空的");
    let mut answers: Vec<(usize, usize)> = Vec::new();
    for name in ["minimal-zh-utf8.chm", "minimal-zh-gbk.chm"] {
        let path = fixture(&format!("../../tests/fixtures/oss/chm/{name}"));
        let found = scan_book_at(&path, &query, &Options::default(), &StageCtx::detached())
            .unwrap_or_else(|| panic!("{name} 该扫得动"));
        assert_eq!(found.hits.len(), 1, "{name}：那句该只命中一次");
        let hit = &found.hits[0];
        // 偏移切回去必须正是查询本身 —— 判据与整本那一档同一份。
        let doc = crate::content::document::open(&path).expect("合成容器应能打开");
        let text = visible_text(&doc.chapter_source(hit.chapter).unwrap());
        assert_eq!(slice(&text, hit), "天地玄黄", "{name}：切回去不是那四个字");
        answers.push((hit.chapter, hit.start));
    }
    assert_eq!(
        answers[0], answers[1],
        "两码页给的是同一本书，章号与偏移不该分家"
    );
}

/// 定版式（CBZ / 逐页 SVG 的漫画）没有「正文」这一列字：回空的读数，不 panic。
///
/// 全库那一档会把架上每一本都交给这一趟，所以「这一本没有字可搜」必须是一个能安静回答的
/// 状态 —— 那一档的症状若是崩或挂，整架的搜索就废了。
#[test]
fn a_fixed_book_answers_with_nothing_to_read() {
    let flow = crate::content::format::FixedDoc::default();
    let doc = from_content(
        DocMeta {
            title: "漫画包".into(),
            ..Default::default()
        },
        Content::Fixed(flow),
    );
    let out = sweep(doc.as_ref(), "猫");
    assert_eq!((out.chapters, out.scanned, out.hits.len()), (0, 0, 0));
    assert!(!out.cancelled && !out.truncated, "没扫过就该是干净的读数");
}

/// 仓库里那本合成的出版书形状：一次 `cargo test` 就要跑到的真容器门。
///
/// 这一条走的是读者那一条完整的路：开容器 → 按位置回 ZIP 取每章正文 → 折可见文本 →
/// 扫 → 报章号与偏移。前面那几条用的是正文就在手上的夹具，测不到 ZIP 与位置那两环。
#[test]
fn the_synthetic_published_book_answers_with_chapter_and_offset() {
    let dir = tempfile::tempdir().unwrap();
    let path = published_shape_epub(&dir);
    let doc = crate::content::document::open(&path).expect("合成容器应能打开");
    let out = sweep(doc.as_ref(), "正文");
    assert_eq!(out.chapters, 28, "前置：28 章");
    assert_eq!(out.hits.len(), 28, "每一章那句『第 N 章的正文。』都该命中");
    assert!(!out.truncated && !out.cancelled);
    assert_eq!(out.scanned, 28, "一章都不该落下");
    // 单数码那章「第1章第1章的正文。」里『正文』前正好七个字。多位数码的要长两个，
    // 所以这里只钉第一章，其余各章切回去验（同一个判据，不依赖位数）。
    assert_eq!(out.hits[0].start, 7, "第 1 章的『正文』在第 7 个字上");
    assert_eq!(out.hits[0].chapter, 0);
    for hit in &out.hits {
        let text = visible_text(&doc.chapter_source(hit.chapter).unwrap());
        assert_eq!(
            slice(&text, hit),
            "正文",
            "第 {} 章切回去不是那两个字",
            hit.chapter
        );
    }
    // `<head><title>` 里的字不在正文坐标系里：搜它应当搜不到（搜得到就等于把 head 数进去了）。
    assert!(
        sweep(doc.as_ref(), "合成出版书").hits.is_empty(),
        "书名只写在 <title> 与 OPF 里"
    );
}

/// 三本真书上的对拍 + 墙钟（`#[ignore]`：开一次容器实测 77–102 ms、全本折一遍最坏 231 ms，
/// debug 档还要再虚高数倍。
/// 跑法 `cargo test -p folio-core --lib --release -- --ignored --nocapture search`）。
///
/// 为什么值得单独一条：合成夹具钉的是**判据**，这三本验的是**判据在真书的排版产物上成立** ——
/// 带 `　` 段首缩进、带 `<h1>`、一章一文件那种。断言只有一条：报出来的偏移切回去必须正是
/// 查询本身；其余是读数。
#[test]
#[ignore = "全本现读现扫，几秒"]
fn the_local_books_answer_on_the_same_axis() {
    for name in [
        "../../tests/fixtures/local/shejie-mauriac.epub",
        "../../tests/fixtures/local/斗战狂潮校对版全本.epub",
        "../../tests/fixtures/local/hali-bote-quanji.epub",
    ] {
        let path = fixture(name);
        if !path.exists() {
            println!("跳过 {name}（这台机器上没有）");
            continue;
        }
        let started = std::time::Instant::now();
        let doc = crate::content::document::open(&path).expect("真书该打得开");
        let opened = started.elapsed();
        // 查询从这本书自己的正文里摘：换一本书也不会红在「那句话不在书里」上。
        let middle = doc.flow().unwrap().chapters.len() / 2;
        let probe = visible_text(&doc.chapter_source(middle).unwrap());
        let query: String = probe
            .chars()
            .skip(probe.chars().count() / 2)
            .take(6)
            .collect();
        let query = Query::parse(&query).expect("前置：摘出来的那六个字不该是空白");
        let started = std::time::Instant::now();
        let out = scan(
            doc.as_ref(),
            &query,
            &Options::default(),
            &StageCtx::detached(),
        );
        let swept = started.elapsed();
        assert!(
            out.scanned == out.chapters || out.truncated,
            "没取消也没截断，就该一章不落地扫完（实扫 {} / {} 章）",
            out.scanned,
            out.chapters
        );
        assert!(!out.hits.is_empty(), "查询摘自这本书的正文，不可能搜不到");
        for hit in out.hits.iter().take(20) {
            let text = visible_text(&doc.chapter_source(hit.chapter).unwrap());
            assert_eq!(
                slice(&text, hit),
                query.as_str(),
                "第 {} 章第 {} 个字切回去不是查询本身",
                hit.chapter,
                hit.start
            );
        }
        println!(
            "{}：{} 章 · 开书 {:>6.0} ms · 现读现扫 {:>7.0} ms · 命中 {} 处",
            path.file_name().and_then(|n| n.to_str()).unwrap_or(name),
            out.chapters,
            opened.as_secs_f64() * 1000.0,
            swept.as_secs_f64() * 1000.0,
            out.hits.len(),
        );
    }
}

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

// ---- 定版式那一族：一页是一段字，不是一章 ----

/// 那本**带文字层的真样本**：Kingsoft Office（WPS）写的两页论文模板。
///
/// 为什么用它而不是仓库里那本 `three-pages.pdf`：后者的内容流只有一个灰矩形，
/// 一个字都没有 —— 拿它测「扫得出命中」就是让断言空跑。它的形状正适合下面那条
/// 「没有字的一页」那一道。
fn wps() -> (PathBuf, Box<dyn Document>) {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/pdf/wps-paper.pdf");
    let doc = crate::content::document::open(&path).expect("WPS 那本该开得动");
    (path, doc)
}

/// 带拉丁字母的那一条查询**对空白不敏感**——PDF 把词缝补错了，搜索这一侧要兜得住。
///
/// 钉四件事，每一件都对应一种会静默错法：
/// 1. 打 `COPYRIGHT` 找得到屏上写的 `COP YRIGHT`（`content::format::pdf::text_layer`
///    那条几何判据补出来的空格，量过的两组数是 3.29 pt 词内 vs 2.49 pt 真缝，分不开）；
/// 2. 报出来的字符区间**盖住**那一个空格 —— 少盖一格，跳过去选中的是前半截；
/// 3. 查询自己写的空白不算数：`THE  PAPER`（两个空格）与正文的 `THEPAPER` 同样找得着；
/// 4. 中文那一条快路**不改口径**（它走按字节的 `match_indices`，正文里也没有需要跳的
///    词缝空格）——「第一 段」里打「第一段」照旧找不到，这一条写在这里是为了让下一次
///    有人把两条路一起改的时候先看见这个决定。
#[test]
fn a_latin_query_ignores_the_spaces_pdf_inserted() {
    let taken = |text: &str, raw: &str| {
        let query = Query::parse(raw).expect("查询不该是空的");
        let mut hits = Vec::new();
        assert!(
            !scan_chapter(0, text, &query, &Options::default(), &mut hits),
            "这么短的样本不该撞到命中上限"
        );
        hits.iter()
            .map(|hit| (hit.start, hit.end, chars(text, hit.start, hit.end)))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        taken("10. COP YRIGHT FORMS", "COPYRIGHT"),
        vec![(4, 14, "COP YRIGHT".into())],
        "补错的那一个空格要被盖进命中区间，否则跳过去只选中前半截"
    );
    assert_eq!(taken("THE  PAPER", "the paper").len(), 1, "查询里多打一个空格不该找不着");
    assert_eq!(taken("THEPAPER", "THE PAPER").len(), 1, "正文里丢了空格也该找得着");
    assert_eq!(taken("Title Case", "title").len(), 1, "折叠那一条不许因为这次改动退化");
    assert!(taken("第一 段", "第一段").is_empty(), "中文那条快路故意不跳空白（见门头的第 4 条）");
    assert_eq!(taken("第一段正文", "第一").len(), 1, "而正常中文查询不受影响");
}

/// 那一列字的第 `from..to` 个（按**字符**数，与 `Hit.start` / `Hit.end` 同一把尺）。
fn chars(text: &str, from: usize, to: usize) -> String {
    text.chars()
        .skip(from)
        .take(to.saturating_sub(from))
        .collect()
}

/// **契约门**：整本一次取齐的那一列，与逐页问的每一页**逐字节相同**。
///
/// 这条不是把契约抄一遍当测试。搜索命中的 `start` / `end` 是按那一列数的，而选中、
/// 批注锚点按逐页问那一问的字序数 —— 两把尺一旦分叉，症状是「搜到了 `ABSTRACT`、
/// 跳过去挑中的是别处那几个字」，而两处各自都仍然自洽，没有任何别的门会红。
/// 没字的那一页：整本那一列给空串，逐页问给 `None` —— 这一条差在契约里写明了，
/// 所以在这里也按那条折起来比。
#[test]
fn the_batched_pages_are_the_per_page_ones_byte_for_byte() {
    let (path, doc) = wps();
    let provider = crate::content::format::provider_for(&path).expect("PDF 该有 provider");
    let batch = doc.page_texts().expect("带字的 PDF 该给得出整本");
    let pages = doc.fixed().expect("这一本该是定版式").pages.len();
    assert_eq!(batch.len(), pages, "整本那一列的长度就是页数");
    for (page, text) in batch.iter().enumerate() {
        let taken = provider
            .page_text(&path, page)
            .map(|layer| layer.text())
            .unwrap_or_default();
        assert_eq!(
            &taken,
            text,
            "第 {} 页两处不是一个序（整本 {} 字 vs 逐页 {} 字）",
            page,
            text.chars().count(),
            taken.chars().count()
        );
    }
}

/// 扫得动，而报出来的那两个偏移**就是选中那套的偏移**。
#[test]
fn a_fixed_layout_book_searches_its_pages() {
    let (_, doc) = wps();
    let query = Query::parse("ABSTRACT").expect("有词");
    let out = scan(&*doc, &query, &Options::default(), &StageCtx::detached());
    assert!(
        !out.hits.is_empty(),
        "第一页写着 ABSTRACT，扫不出命中就是搜索坏了"
    );
    let hit = &out.hits[0];
    assert_eq!(hit.chapter, 0, "那一格在这一族是页号：它在第一页");
    let text = doc.page_texts().expect("整本")[hit.chapter].clone();
    assert_eq!(
        chars(&text, hit.start, hit.end),
        "ABSTRACT",
        "偏移摊回那一页的字序，念出来的就是查的那几个字"
    );
    assert_eq!(out.chapters, 2, "读数里的单位在这一族是页");
    assert_eq!(out.scanned, 2, "两页都扫到了");
    assert!(!out.truncated && !out.cancelled, "一趟干净的扫描");
}

/// 命中在**第二页**上也归第二页：那一趟真按页走，不是只扫首页。
///
/// 只钉首页的那一版实现（把整本拼成一串再扫、或 `break` 得太早）在这一条上红，
/// 而它上面那两条照样绿 —— 所以这一条不是重复。
#[test]
fn a_hit_on_a_later_page_is_attributed_to_that_page() {
    let (_, doc) = wps();
    let query = Query::parse("COPYRIGHT").expect("有词");
    let out = scan(&*doc, &query, &Options::default(), &StageCtx::detached());
    let hit = out.hits.first().expect("第二页有 COPYRIGHT 那一行");
    assert_eq!(hit.chapter, 1, "命中该归第二页，而不是被算到第一页头上");
    assert!(
        out.scanned == 2 || out.truncated,
        "要扫到第二页才可能报出它：读数就该说扫到第二页（实际 {}）",
        out.scanned
    );
}

/// **大小写不敏感**那一圈在定版式上同样成立：WPS 那一行写的是 `COPYRIGHT FORMS`，
/// 读者打的可能是 `forms`。这一族没有 DOM、没有可见文本的折法，折叠那一条路
/// 走的是同一份 `each_hit`，所以它不该因为换了族就退化成区分大小写。
#[test]
fn a_folded_query_still_hits_a_typeset_page() {
    let (_, doc) = wps();
    let query = Query::parse("forms").expect("有词");
    assert!(query.folds_case(), "带拉丁小写的查询要走折叠那一条");
    let out = scan(&*doc, &query, &Options::default(), &StageCtx::detached());
    assert!(!out.hits.is_empty(), "正文里写着 FORMS，`forms` 也该找得着");
}

/// 到上限就收手，并且把「后面可能还有」说出来。
#[test]
fn the_page_scan_stops_at_the_limit_and_says_so() {
    let (_, doc) = wps();
    let query = Query::parse("A").expect("一个字母也有得搜");
    let tight = Options {
        max_hits: 1,
        ..Default::default()
    };
    let out = scan(&*doc, &query, &tight, &StageCtx::detached());
    assert_eq!(out.hits.len(), 1, "到一档就收");
    assert!(out.truncated, "收的手要说「没扫完」，界面才敢说「还有」");
    assert_eq!(out.scanned, 1, "只扫过第一页");
}

/// 旗标置了就停，且以「取消」收尾而不是「扫完」。
#[test]
fn a_triggered_cancel_stops_the_page_scan() {
    let (_, doc) = wps();
    let ctx = StageCtx::detached();
    ctx.cancel().trigger();
    let out = scan(
        &*doc,
        &Query::parse("A").unwrap(),
        &Options::default(),
        &ctx,
    );
    assert!(out.cancelled, "置了旗标就该以取消收尾");
    assert_eq!(out.scanned, 0, "一页都不该扫");
}

/// **没有文字层的那一本**：命中是空的，而页数照实报。
///
/// 用仓库里那本 `three-pages.pdf` —— 三页、内容流只有一个灰矩形、一个字都没有，
/// 正是扫描件那一档的形状。这一条与上面几条配成一对：缺了它，「这本书没字可搜」
/// 与「搜索坏了」「那个词不在书里」在界面上是同一句话，而读者的动作完全不同。
#[test]
fn a_wordless_book_scans_to_nothing_and_still_counts_its_pages() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/pdf/three-pages.pdf");
    let doc = crate::content::document::open(&path).expect("入库的那本夹具该读得开");
    let out = scan(
        &*doc,
        &Query::parse("猫").unwrap(),
        &Options::default(),
        &StageCtx::detached(),
    );
    assert!(out.hits.is_empty(), "一个字都没有，不该有命中");
    assert_eq!(out.chapters, 3, "三页照实报 —— 界面念得出「这一本没有字」");
    assert_eq!(out.scanned, 3, "三页都扫过（每页都是空串）");
    assert!(!out.truncated && !out.cancelled);
}
