//! 在一本书的正文里找一段字：**现读现扫，不建索引**。
//!
//! ## 为什么不建索引（2026-10-19 实测，release 档，本机，仓库里那三本真书）
//!
//! 单位是「读者改一次查询实际付的钱」，走的就是下面 [`scan`] 那一条路（作业池里按位置
//! 回容器取每章正文 → 折可见文本 → 找命中 → 裁上下文）：
//!
//! | 书 | 章 | 可见字数 | 一次查询（全本扫完） | 命中爆多、到上限就收手的那些 | FTS5 trigram 建索引 | 索引 / 正文明文 |
//! |---|---|---|---|---|---|---|
//! | 《蛇结》0.67 MB | 28 | 10.1 万 | 7.6–9.0 ms | 1.2 ms（扫 7 章） | 52 ms | **4.38×** |
//! | 《哈利波特全集》29.7 MB | 301 | 276.9 万 | 154–188 ms | 2.5 ms（扫 10 章） | 1.46 s | **3.21×**（25 MB） |
//! | 《斗战狂潮》7.2 MB | 1225 | 432.8 万 | 274 ms | 5.0 ms（扫 2 章） | 2.50 s | **3.47×**（44 MB） |
//!
//! 拆开来是：读全书正文 1.6–77 ms、折可见文本 5.8–207 ms、纯扫 0.1–6.2 ms（同一本
//! 432 万字的书，12.8 MB 正文找「一个」6.2 ms）。最坏那一本现读现扫一趟 274 ms，
//! 典型一本不到 10 ms —— 索引要省的力气本来就在这里，而它要付的是正文的 3.2–4.4 倍
//! 体积，外加建索引那 1.5–2.5 s。判据与 `plans/text-out-of-db.md` 同一条：
//! **库里只放算出来贵、且原文件给不了的东西**，正文现取不贵。
//!
//! 索引那一档还有两条实测：
//!
//! - FTS5 的 `trigram` 分词器对**两个字的中文查询静默返回 0 行**（`MATCH '一个'` → 0，
//!   `LIKE '%一个%'` → 1210 行），而中文里两个字的词占一大半：那一档要么改走 `LIKE`
//!   全表扫，要么让读者的查询凭空落空。现扫没有这个门槛，一个字都搜得动。
//! - 带拉丁字母的查询要走「两边折了再比」那一圈，比按字节那一条慢 40 ms / 432 万字
//!   （同一本书、同一个只换大小写的查询：274 ms → 314 ms）。
//!
//! ## 全库那一档：钱花在「开每一本书」，不在扫正文
//!
//! 2026-10-20 实测：把本机那三本真书硬链成 30 本的架子（平均 12 MB/本，比真实书架偏大），
//! 逐本现扫，`--sweep` 那一档跑法，release、页缓存热：
//!
//! | 每本 | ms |
//! |---|---|
//! | 开书（`document::open`：认格式 + 解析 OPF/目录） | **74** |
//! | 扫正文，冷查询（全本只命中一次 → 非扫到最后一章不可） | +171 |
//! | 扫正文，每本 20 处就收手（[`LIBRARY_MAX_PER_BOOK`] 那一档） | +3 |
//!
//! 也就是说按 20 处那一档，**一本书的钱 95% 是开容器**，而那一笔与书多大无关：656 KB 的
//! 那本开一次 93 ms，28 MB 的那本只要 48 ms（它目录条目少、OPF 短）。外推整架：
//!
//! | 架上几本 | 逐本现扫（CPU） | 池里两条 worker 各半 |
//! |---|---|---|
//! | 50 | 3.9 s | 1.9 s |
//! | 100 | 7.7 s | 3.9 s |
//! | 300 | 23 s | 11.6 s |
//!
//! 这条数决定了全库那一档的优化方向**不是**倒排索引（FTS5 那一档见上文的表：建索引比现扫
//! 一遍贵十倍，体积 3.2–4.4× 正文，中文两字查询还静默给 0 行），而是**别为搜索开一整本书**：
//! 章节的位置清单本来就在库里（`Store::load_flow` → `chapters.locator`），而
//! [`crate::content::format::ChapterLocator`] 足够回容器取字节，所以 OPF 与目录那两趟解析
//! 在这一档是白付的。那一刀归下一轮（界面侧落点也归下一轮：读者 2026-10-20 定「先停在 core」）。
//!
//! ## 坐标是 `TextMap` 那一把尺
//!
//! 命中的 `start` / `end` 是**那一章可见文本**里的字符数，与 [`crate::Locator::offset`]、
//! `chapters.char_len`、锚点的 `start` 全用同一份 [`crate::content::anchor::TextMap`]。
//! 所以一处命中不必经过排版就能变成一个可跳的 [`crate::Locator`]。
//! 别在这条轴上另起一把尺：错了半个字就是「搜到『猫』、跳过去屏上是『狗』」。
//!
//! [`crate::content::format::visible_text`] 与 [`crate::content::format::visible_chars`]
//! 必须同源（一个数、一条串，同一个 `TextMap`）：同一章在两处折出不同的字数，
//! 进度条与搜索结果就指不到同一行。

use std::ops::Range;
use std::path::Path;

use crate::content::document::Document;
use crate::content::format::visible_text;
use crate::infra::task::StageCtx;

/// 一次扫描默认最多报多少处。
///
/// 定这个数的是「一屏摆得下多少」与「读者滚到第 200 条还要不要往下滚」，不是性能：
/// 扫到第 432 万字也才 6 ms，上限挡的是内存与那屏列表。
pub const DEFAULT_MAX_HITS: usize = 200;
/// 全库那一档：每本最多几处（读者 2026-10-20 定的那一档，「每本 ≤ 20 处、整架 ≤ 200 行」）。
///
/// 这一档是**扫**的上限而不只是摆法：一本在整架的列表里只占它那一截，二十处已经够看出
/// 「这本里这个词常见」，多扫的那些既没人看、也不是免费的 —— 实测同一本 433 万字的书，
/// 扫到 200 处收手要 11 ms（只读了 40 章），而越早上手就越便宜。
/// 到了这一档就同样报 `truncated`，界面要说「还有」，不能说「只有」。
pub const LIBRARY_MAX_PER_BOOK: usize = 20;
/// 整架那一屏的总行数预算（同一句约定的另一半）。扫是按本扫的，所以这一格归调用方
/// （界面侧）：它才知道那屏摆得下几行、以及架上那一片一共点到了第几本。
pub const LIBRARY_MAX_ROWS: usize = 200;
/// 上下文在命中前面留几个字。
pub const DEFAULT_BEFORE: usize = 12;
/// 命中后面留几个字（比前面长：读者从左往右读，往后那点才是那句话）。
pub const DEFAULT_AFTER: usize = 28;
/// 报一次进度、看一次取消的章数间隔。
///
/// 逐章报会给界面刷一千多帧（《斗战狂潮》1225 章），而那一屏只需要「扫到几成」。
const PROGRESS_EVERY: usize = 32;

/// 归一化后的查询词。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    text: String,
    /// 查询里有没有**有大小写之分**的字（见 [`cased`]）。有就得两边折了再比（见 [`fold`]）。
    ///
    /// 中文与数字的查询恒为 `false`，于是走按字节的 `match_indices`（SIMD，实测
    /// 12.8 MB 正文 6 ms）；带拉丁字母的查询走逐字符那一圈，同一本 432 万字的书
    /// 全本多付 40 ms。
    folds: bool,
}

impl Query {
    /// 归一化后的查询词。
    pub fn parse(raw: &str) -> Option<Self> {
        let text = raw.trim().to_string();
        if text.is_empty() {
            return None;
        }
        let folds = text.chars().any(cased);
        Some(Self { text, folds })
    }

    /// 这一趟要不要**两边都折了再比**。
    ///
    /// 判据是「查询里有没有一个有大小写之分的字」，不是「查询里有没有需要折的字」：
    /// 全小写的 `cat` 也得 match 句首的 `Cat`，而按字节那一条快路只会找写一样的那几个字。
    pub fn folds_case(&self) -> bool {
        self.folds
    }

    /// 读者写下的那几个字（已经去过两头空白）。
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

/// 一处命中。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// 章下标，与 `chapters.idx`、`Locator.chapter` 同一个数法。
    pub chapter: usize,
    /// 这一章可见文本里的字符区间：跳过去用的就是 `start`。
    pub start: usize,
    pub end: usize,
    /// 摆给读者看的那句上下文：命中前后各截一点，换行与缩进折成一个普通空格。
    pub snippet: String,
    /// `snippet` 里命中那一段的字符区间 —— 与 `start` 不是同一把尺，只用来上色。
    pub mark: Range<usize>,
}

/// 一趟扫描的读数。
#[derive(Clone, Debug, Default)]
pub struct Found {
    pub hits: Vec<Hit>,
    /// 这本书一共几章。
    pub chapters: usize,
    /// 实际扫到第几章就停了（取消或到上限都会提前停）。
    pub scanned: usize,
    /// 到了 `Options::max_hits` 就收的手：**后面还可能还有**，没数过。
    ///
    /// 这一格只能说「没扫完」，不能说「还有 N 处」—— 想知道就得把全书扫完，
    /// 那正是上限要省掉的力气。
    pub truncated: bool,
    /// 被读者（或下一次输入）取消。
    pub cancelled: bool,
}

/// 一次扫描的档位。
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub max_hits: usize,
    pub before: usize,
    pub after: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_hits: DEFAULT_MAX_HITS,
            before: DEFAULT_BEFORE,
            after: DEFAULT_AFTER,
        }
    }
}

/// 扫**磁盘上的某一本书**：自己开容器、自己读正文、自己收手。
///
/// 全库那一档的基本单位就是这一句 —— 它一次只拿一本书，所以逐本提交到作业池里，
/// 谁先扫完谁先把结果报出去（一屏的进度是这么长出来的），而读者改一个字就能把这些
/// 作业整个作废。库的连接、书的身份与「哪几本在架面上」都不在这一句里：那是调用方
/// （界面侧）的事，这一句只认一个路径。
///
/// 开不了（文件被换了、格式认不出、这一本坏了）回 `None` 而不是报错：全库那一趟不该
/// 因为架上有一本坏了就整个停 —— 「这一本搜不了」与「搜索坏了」是两件事，与
/// [`Document::chapter_source`] 取不到那一章同一个立场。
///
/// 一趟的代价里有一笔与书的大小无关的固定支出：**开容器**（2026-10-20 实测逐本 47–93 ms，
/// 656 KB 那本反而比 28 MB 那本慢，因为它是 OPF 与目录的解析而不是解压）。
/// 所以「整架有多少本」才是这一档真正的尺，不是「整架有多少字」。
pub fn scan_book_at(
    path: &Path,
    query: &Query,
    options: &Options,
    ctx: &StageCtx,
) -> Option<Found> {
    let doc = crate::content::document::open(path).ok()?;
    Some(scan(doc.as_ref(), query, options, ctx))
}

/// 现读现扫一整本。
///
/// 一章一章地流：取正文 → 折可见文本 → 扫 → 丢掉。峰值内存因此是一章的正文，
/// 与这本书有多大无关（《斗战狂潮》全本 12.8 MB 正文，一次只摊开其中一章）。
///
/// 取不到的那一章（源文件被换掉 / 那一章的 XHTML 坏了）跳过，不终止整趟：与
/// [`Document::chapter_source`] 那句「是 `ChapterUnavailable` 不是空串」同一口径 ——
/// 「这几章搜不了」与「这本书没了」是两件事。
pub fn scan(doc: &dyn Document, query: &Query, options: &Options, ctx: &StageCtx) -> Found {
    let chapters = doc.flow().map_or(0, |flow| flow.chapters.len());
    let mut out = Found {
        chapters,
        ..Default::default()
    };
    for index in 0..chapters {
        if ctx.cancel().triggered() {
            out.cancelled = true;
            break;
        }
        // 取不到的那一章（源文件被换掉 / 那一章的 XHTML 坏了）与空的那一章：不终止整趟，
        // 也不往里报命中。与 [`Document::chapter_source`] 那句「是 `ChapterUnavailable`
        // 不是空串」同一口径 ——「这几章搜不了」与「这本书没了」是两件事。
        match doc.chapter_source(index) {
            Ok(source) if !source.is_empty() => {
                let text = visible_text(&source);
                if scan_chapter(index, &text, query, options, &mut out.hits) {
                    out.truncated = true;
                }
            }
            _ => {}
        }
        out.scanned += 1;
        if out.truncated || out.hits.len() >= options.max_hits {
            out.truncated = true;
            break;
        }
        if index % PROGRESS_EVERY == 0 || index + 1 == chapters {
            ctx.item(out.scanned as u64, chapters as u64);
        }
    }
    out
}

/// 扫一章的可见文本，命中追加进 `hits`。返回 `true` = 到了上限，别再往下扫了。
///
/// 单列出来是给「全库那一档」复用：它自己负责开容器与取正文，命中这一件事两档共用
/// 同一份判据，两档各写一遍就会有一处漂移。
pub fn scan_chapter(
    chapter: usize,
    text: &str,
    query: &Query,
    options: &Options,
    hits: &mut Vec<Hit>,
) -> bool {
    let mut full = false;
    each_hit(text, query, &mut |from, to, start| {
        let (snippet, mark) = snippet(text, from, to, options.before, options.after);
        hits.push(Hit {
            chapter,
            start,
            end: start + text[from..to].chars().count(),
            snippet,
            mark,
        });
        if hits.len() >= options.max_hits {
            full = true;
            return false;
        }
        true
    });
    full
}

/// 逐个把命中交给 `on`（字节区间 + 起始字符数）。`on` 回 `false` 即收手。
///
/// 两条路都只报**不重叠**的命中，和 `str::match_indices` 的既有行为一致：
/// 「aa」在「aaa」里是一处而不是两处。
fn each_hit(text: &str, query: &Query, on: &mut dyn FnMut(usize, usize, usize) -> bool) {
    if !query.folds {
        let needle = query.text.as_str();
        let needle_chars = needle.chars().count();
        // 字符数按**增量**累加：从上一个命中的末尾数到这一个的开头，再把上一个命中
        // 自己那几个字补上。所有增量加起来就是一趟正文的长度，所以命中再多也不会变成
        // O(命中数 × 章长)（那一档在「的」这种字上实测会把 6 ms 扫成几百 ms）。
        // 落下命中自己那几个字就是每个偏移都比真值少几个字，而且越往后少得越多。
        let mut seen_byte = 0usize;
        let mut seen_chars = 0usize;
        for (at, _) in text.match_indices(needle) {
            let start = seen_chars + text[seen_byte..at].chars().count();
            if !on(at, at + needle.len(), start) {
                return;
            }
            seen_chars = start + needle_chars;
            seen_byte = at + needle.len();
        }
        return;
    }
    let needle: Vec<char> = query.text.chars().map(fold).collect();
    let mut chars = 0usize;
    let mut after = 0usize;
    for (at, first) in text.char_indices() {
        if at < after {
            chars += 1;
            continue;
        }
        if fold(first) == needle[0]
            && let Some(end) = match_folded(text, at, &needle)
        {
            if !on(at, end, chars) {
                return;
            }
            after = end;
        }
        chars += 1;
    }
}

/// 从 `byte` 起，`needle`（已折叠）能不能逐字对上。对上了给命中的**末字节**。
fn match_folded(text: &str, byte: usize, needle: &[char]) -> Option<usize> {
    let mut at = byte;
    for want in needle {
        let found = text[at..].chars().next()?;
        if fold(found) != *want {
            return None;
        }
        at += found.len_utf8();
    }
    Some(at)
}

/// 这个字**有没有大小写之分**。
///
/// 用它来决定走不走折叠那一条路，而不是「这个字折不折得动」：`cat` 三个字各自折出来
/// 还是自己，可正文里写的可能是 `Cat` —— 判据要看的是这一族字存不存在另一种写法。
fn cased(c: char) -> bool {
    c.to_lowercase().next() != Some(c) || c.to_uppercase().next() != Some(c)
}

/// 单字符的大小写折叠：**只有折过去还是一个字符**才折。
///
/// 这条限制是为了下标不挪位：`ß` 的小写是 `ss`（一个字变两个），折了就把「第 N 个字」
/// 挪成「第 N+1 个字」，而这一列是要进 `Locator.offset` 的。折不动的那几个字照原样比，
/// 代价只是极少数拉丁字母的大小写不敏感没生效。
fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(one), None) => one,
        _ => c,
    }
}

/// 从命中前后各截一点，裁成一句上下文；两端被裁掉的那一侧补一个 `…`。
///
/// 折回来的 `mark` 是这段上下文自己的字符区间（补上的 `…` 也算一个字），不是章内坐标。
fn snippet(
    text: &str,
    from: usize,
    to: usize,
    before: usize,
    after: usize,
) -> (String, Range<usize>) {
    let head = back(text, from, before);
    let tail = forward(text, to, after);
    let mut out = String::new();
    if head > 0 {
        out.push('…');
    }
    let pre = flatten(&text[head..from]).trim_start().to_string();
    let mid = flatten(&text[from..to]);
    let post = flatten(&text[to..tail]).trim_end().to_string();
    let mark_start = out.chars().count() + pre.chars().count();
    out.push_str(&pre);
    out.push_str(&mid);
    let mark_end = out.chars().count();
    out.push_str(&post);
    if tail < text.len() {
        out.push('…');
    }
    (out, mark_start..mark_end)
}

/// 从 `byte` 往回数 `n` 个字符，落回字符边界。
fn back(text: &str, byte: usize, n: usize) -> usize {
    let mut at = byte;
    for _ in 0..n {
        if at == 0 {
            break;
        }
        at -= 1;
        while at > 0 && !text.is_char_boundary(at) {
            at -= 1;
        }
    }
    at
}

/// 从 `byte` 往前走 `n` 个字符，回它的字节位置。
fn forward(text: &str, byte: usize, n: usize) -> usize {
    let mut at = byte;
    for c in text[byte..].chars().take(n) {
        at += c.len_utf8();
    }
    at
}

/// 把可见文本里的换行、缩进、全角空格折成普通空格，连着几处折成一个。
///
/// 正文里一章的可见文本是带 `\n` 与 `　`（中文书的段首缩进）的，列表那一行只摆得下一句；
/// 不折的话那一行会在中间断开，行高就不定了。
fn flatten(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_gap = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !in_gap {
                out.push(' ');
                in_gap = true;
            }
        } else {
            out.push(c);
            in_gap = false;
        }
    }
    out
}

#[cfg(test)]
mod tests;
