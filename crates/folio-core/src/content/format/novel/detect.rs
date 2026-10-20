//! 纯文本小说的**引擎**：吃一份编译好的规则表，扫一遍规范化文本，切出章表与书目。
//! 这一份里没有任何「第X章」的字面形状 —— 那些都在 `assets/novel-rules.*.json` 里。
//!
//! ## 一趟扫描干三件事
//!
//! 找标记（[`find_hits`]）、判目录页（[`drop_toc_page`]）、抽书目（[`book_info`]）。它们
//! 共用同一趟逐行遍历是有原因的：文件头那一块**同时**是目录页与书名作者的出处，分两趟
//! 就会在两处各判一次「这是不是头部」，而两边的答案迟早要分叉。
//!
//! ## 四条否决门，顺序是写死的
//!
//! 1. **整行判据**（在规则的形状里）：标记必须在行首，标题尾巴限长。挡住「他想起第三章
//!    那句话」被认成一章。
//! 2. **目录页整块跳过**（[`drop_toc_page`]）：从头部往后连续若干枚标记、每两枚之间几乎
//!    没正文，那一块是目录。假阳性最大的来源就是它，而它同时是免费的 ground truth。
//! 3. **序号递增**（[`keep_monotonic`]）：有号的标记必须基本递增。小说的章号是强约束，
//!    这一条把正文里重复出现的那枚一次筛掉，也是唯一能把准确率写成数字的判据。
//! 4. **数量与间隔门**（[`looks_chaptered`]）：少于 `min_chapters` 枚、或相邻两枚之间的
//!    中位字数低于 `min_median_chapter_chars`，就当**这本没分章**，回落一整篇一章。
//!
//! 顺序不能换：门 4 要用门 2、3 处理过的命中数来判，否则一本带目录页的书会因为
//! 「命中翻倍」而通过所有门，屏上却是每个章名出现两次。
//!
//! ## 分章不许吃字
//!
//! 切点是**注入**一个 `# ` 前缀，不是删改那一行：包装会把 `#` 剥掉，所以可见文本一个字
//! 都不变，而 `Locator.offset` 数的就是可见字符 —— 锚点与批注不漂。标记后面粘着的超长
//! 尾巴（真书上见过 618 字一行）会被截进标题，**多出来的字另起一行留在正文开头**，
//! 加的那个换行是空白，同样不动可见文本。

use super::rules::{Compiled, CompiledRules, MetaKeys};
use crate::content::format::{Chapter, TocEntry};

/// 一枚命中的章标记。
#[derive(Clone)]
struct Hit {
    /// 行号（规范化文本的行，0 起）。
    line: usize,
    /// 规则里那条目的名字（诊断与目录注记用它）。
    marker: String,
    /// 读出来的号；`None` = 这一族不带号（楔子、番外）。
    number: Option<usize>,
    /// 标记 + 标题尾巴（不超过 `title_max_chars`），用于章名与目录。
    title: String,
    /// 那一行超出标题上限、被留在正文开头的余下部分。空 = 整行都是标题。
    overflow: String,
}

/// 扫一遍行，找出所有命中的章标记。顺序 = 文件序；同一行只取第一条碰到的规则（顺序即优先级）。
fn find_hits(lines: &[&str], rules: &CompiledRules) -> Vec<Hit> {
    let max = rules.gates().title_max_chars;
    let mut hits = Vec::new();
    for (ix, raw) in lines.iter().enumerate() {
        let line = raw.trim_matches(|c: char| c.is_whitespace());
        if line.is_empty() {
            continue;
        }
        for (marker, compiled) in rules.markers() {
            let matched = match compiled {
                Compiled::Literal => {
                    // 整行等于它（允许尾随标点与空白：「楔子」「楔子：」都算）。
                    let head = line.split(['：', ':', '　', ' ']).next().unwrap_or(line);
                    marker.text.as_deref() == Some(head)
                }
                Compiled::Regex(re) => re.is_match(line),
                Compiled::Rejected => false,
            };
            if !matched {
                continue;
            }
            let number = extracted_number(marker, compiled, line, rules);
            let (title, overflow) = split_heading(line, max);
            hits.push(Hit {
                line: ix,
                marker: marker.name.clone(),
                number,
                title,
                overflow,
            });
            break;
        }
    }
    hits
}

/// 从捕获组一（或整行）读那个号。读不出就 `None` —— 读不出号的命中仍然**是**一枚命中，
/// 只是不参与递增校验；把它当假阳性丢掉会误伤「第X章」用了表外字符的书。
fn extracted_number(
    marker: &super::rules::Marker,
    compiled: &Compiled,
    line: &str,
    rules: &CompiledRules,
) -> Option<usize> {
    use super::rules::NumberingStyle as S;
    if marker.numbering == S::None {
        return None;
    }
    let Compiled::Regex(re) = compiled else {
        return None;
    };
    let text = re
        .captures(line)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str())
        .unwrap_or(line);
    parse_number(text, rules)
}

/// 中文数字 / 阿拉伯 / 全角数字 / 明刻本省略式都能读。读不出回 `None`。
///
/// 表外字符不算（`〇〇一` 那种没登记的写法直接放弃这一枚的号），因为「哪些字符算数字」
/// 是**语言**的事，写在规则表的 `numbering.digits` 里，而不是问 Unicode 属性。
pub(crate) fn parse_number(text: &str, rules: &CompiledRules) -> Option<usize> {
    let s = text.trim();
    // 先试纯数字：全角那一族先折成半角，因为 `parse::<usize>` 只认半角。
    let half: String = s
        .chars()
        .map(|c| {
            if ('０'..='９').contains(&c) {
                char::from(b'0' + (c as u32 - '０' as u32) as u8)
            } else {
                c
            }
        })
        .collect();
    if !half.is_empty() && half.chars().all(|c| c.is_ascii_digit()) {
        return half.parse().ok();
    }
    let digits = rules.digits();
    if s.is_empty() || !s.chars().all(|c| digits.contains(c)) {
        return None;
    }
    let value = |c: char| -> Option<usize> {
        match c {
            // 白圈 `○`（U+25CB）与表意零 `〇`（U+3007）都得当 0：那个 PG 西游本用的就是
            // 前者，只认后者的话 `第一○回` 的号读不出来、章却已经切出来了 —— 号连不起来
            // 比丢章更难查，因为屏上看着是对的。
            '〇' | '○' | '零' => Some(0),
            '一' => Some(1),
            '二' => Some(2),
            '三' => Some(3),
            '四' => Some(4),
            '五' => Some(5),
            '六' => Some(6),
            '七' => Some(7),
            '八' => Some(8),
            '九' => Some(9),
            '两' => Some(2),
            _ => None,
        }
    };
    let unit = |c: char| match c {
        '十' => Some(10),
        '百' => Some(100),
        '千' => Some(1000),
        '万' | '萬' => Some(10_000),
        _ => None,
    };
    let mut total = 0usize;
    let mut current = 0usize;
    let mut saw_unit = false;
    for c in s.chars() {
        if let Some(power) = unit(c) {
            let mult = if current == 0 && !saw_unit {
                1
            } else {
                current
            };
            total = if power == 10_000 {
                (total + mult) * power
            } else {
                total + mult * power
            };
            current = 0;
            saw_unit = true;
        } else {
            current = value(c)?;
        }
    }
    // 有单位就是标准写法（`三十五`、`一百零八`）。
    if saw_unit {
        return Some(total + current);
    }
    // 没有单位：单个字符就是它自己；两位以上是**按位值读**（`一○` = 10、`一一` = 11、
    // `一○五` = 105、`二一` = 21）—— 那既是明刻本省略「十」的写法，也是 PG 那个本子用
    // 白圈 `○` 当零的写法，两件事在这一条上合成一条规则。
    let chars: Vec<char> = s.chars().collect();
    if chars.len() == 1 {
        return value(chars[0]).filter(|v| *v > 0);
    }
    positional(&chars, rules, &value)
}

/// 连续位数字按位值读：`一○`=10、`一一`=11、`一○五`=105、`二一`=21。
///
/// 这一档要规则表显式开着（`colophon_style`），因为「二一」在别的写法里可能是编号而不是
/// 21；开了之后同时覆盖两件事 —— 明刻本省略「十」（第一一回 = 11）与那个 PG 西游本用
/// 白圈当零（`第一○回` = 10）。
fn positional(
    chars: &[char],
    rules: &CompiledRules,
    value: &impl Fn(char) -> Option<usize>,
) -> Option<usize> {
    if !rules.colophon_style() {
        return None;
    }
    let mut out = 0usize;
    for c in chars {
        out = out * 10 + value(*c)?;
    }
    (out > 0).then_some(out)
}

/// 标题 = **整行**（标记 + 它后面那句），只有超过上限时才切下来。
///
/// 两条理由合成这一格：读者在屏上看到的章标题就该是原文那一行；而
/// `xhtml::from_plain_text` 会把 ATX 标题那一**整行**都渲染成标题，所以「存进章名的截了、
/// 画出来的没截」是不一致 —— 真书上那 618 字一行会画成一条占满三屏的标题。因此切下来的
/// 余下部分另起一行留在正文开头（加的是换行，可见文本不变，那条「不吃字」的门才还成立）。
///
/// 切点优先落在空白上（章回体写 `第X回　对句`，切在空格处最不难看），找不到空白才硬夹在
/// 上限处。
fn split_heading(line: &str, max: usize) -> (String, String) {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= max {
        return (line.to_string(), String::new());
    }
    // 在上限之内找最后一个空白当切点；至少留 2 个字，别切出一个空标题。
    let cut_at = chars[..max]
        .iter()
        .rposition(|c| c.is_whitespace())
        .filter(|at| *at >= 2)
        .unwrap_or(max);
    let head: String = chars[..cut_at].iter().collect();
    let rest: String = chars[cut_at..].iter().collect();
    (head.trim_end().to_string(), rest.trim_start().to_string())
}

/// 丢掉头部那块目录页。返回剩下的命中。
///
/// 判据两条**都要**成立，缺一就会错杀：
///
/// - 相邻两枚之间正文 ≤ `toc_max_gap_chars`（目录行是一条挨一条的）；
/// - 而且后一枚的号比前一枚大（号还在往前走）。
///
/// 第二条是这一刀的关键，也是只数间隔为什么会输的原因：目录页最后一行与正文第一章
/// 之间常常**只隔一个空行**，间隔为 0 —— 单看间隔会把正文的第一章当成目录的最后一行
/// 吃掉（实测这么错过：六条目录 + 六章正文，切出来五章）。加上「号必须前进」之后，
/// 正文第一章那个回到 1 的号就是目录块的结束线。
///
/// 不带号的标记（楔子、番外）不参与第二条，退回收紧到只看间隔。
fn drop_toc_page(hits: &[Hit], lines: &[&str], rules: &CompiledRules) -> Vec<Hit> {
    let gates = rules.gates();
    let gap = |a: &Hit, b: &Hit| -> usize { super::body_chars(lines, a.line + 1, b.line) };
    let advances = |a: &Hit, b: &Hit| match (a.number, b.number) {
        (Some(prev), Some(next)) => next > prev,
        _ => true,
    };
    // 只从**最前面**那一串里找：正文中间的短间隔是「连续引用」，由递增那条门处理。
    let mut run = 0usize;
    while run + 1 < hits.len()
        && gap(&hits[run], &hits[run + 1]) <= gates.toc_max_gap_chars
        && advances(&hits[run], &hits[run + 1])
    {
        run += 1;
    }
    // `run` 是这条短间隔链的**末端下标**，链长 = run + 1。
    if run + 1 >= gates.toc_min_run {
        hits[run + 1..].to_vec()
    } else {
        hits.to_vec()
    }
}

/// 序号递增那一档：解得出号的命中必须比上一个已知号大，否则丢掉（那是正文里的引用）。
///
/// 容一次「同回两半」：`第X回` 里同一号连出两次且中间正文很少，是章回体某些版本的
/// 「上/下」分半，不当假阳性。
fn keep_monotonic(hits: &[Hit]) -> Vec<Hit> {
    let mut out: Vec<Hit> = Vec::with_capacity(hits.len());
    let mut last: Option<usize> = None;
    for hit in hits {
        match hit.number {
            // 不带号的（楔子、番外）不参与递增判据：它们本来就没有顺序可言。
            None => out.push(hit.clone()),
            Some(n) if last.is_none_or(|prev| n > prev) => {
                last = Some(n);
                out.push(hit.clone());
            }
            // 丢掉的那一枚不计数也不 warn：warn 要留到分章结果定下来之后再说，
            // 否则一本书几百条「正文里提到第X章」会把日志刷满。
            Some(_) => {}
        }
    }
    out
}

/// 门：这批命中像不像真的分过章。
fn looks_chaptered(hits: &[Hit], lines: &[&str], rules: &CompiledRules) -> bool {
    let gates = rules.gates();
    if hits.len() < gates.min_chapters {
        return false;
    }
    let mut gaps: Vec<usize> = hits
        .windows(2)
        .map(|w| super::body_chars(lines, w[0].line + 1, w[1].line))
        .collect();
    if gaps.is_empty() {
        return false;
    }
    gaps.sort_unstable();
    let median = gaps[gaps.len() / 2];
    median >= gates.min_median_chapter_chars
}

/// 按命中切章，并把每章的标记行写成 ATX 标题（`# `）。
///
/// 注入而不是删改：`xhtml::from_plain_text` 包装时会把 `#` 剥掉，所以可见文本一字不变；
/// 标题超长那种「标记后粘着整段正文」另起一行，加的是空白，同样不动可见文本。
/// 这两件事合起来就是那条「分章不许吃字」的门能断言相等的原因。
///
/// 第一个标记之前那一段（书名页、目录页、序言）仍然**留着**当一章 —— 目录那些行是
/// 书里的字，不该被吞掉（吞了就断了上面那条不变量），只是它的章名要说出它是什么。
fn cut(lines: &[&str], hits: &[Hit], fallback_title: &str, toc_dropped: bool) -> Vec<Chapter> {
    let mut out: Vec<Chapter> = Vec::new();
    let mut starts: Vec<usize> = hits.iter().map(|h| h.line).collect();
    if starts.first() != Some(&0) {
        starts.insert(0, 0);
    }
    for (ix, start) in starts.iter().enumerate() {
        let end = starts.get(ix + 1).copied().unwrap_or(lines.len());
        let hit = hits.iter().find(|h| h.line == *start);
        let title = match hit {
            None if toc_dropped => "目录".to_string(),
            None => fallback_title.to_string(),
            Some(h) => h.title.clone(),
        };
        let mut body = String::new();
        for (offset, line) in lines[*start..end].iter().enumerate() {
            let is_marker = hit.is_some_and(|h| h.line == *start + offset);
            if is_marker {
                // 标题行写成 ATX，余下的部分（超长那种粘连）另起一行回到正文。
                let head = hit.expect("is_marker 是由 hit 定出来的");
                body.push_str("# ");
                body.push_str(head.title.trim_start());
                body.push('\n');
                if !head.overflow.is_empty() {
                    body.push_str(head.overflow.trim_start());
                    body.push('\n');
                }
            } else {
                body.push_str(line);
                body.push('\n');
            }
        }
        if body.trim().is_empty() {
            continue;
        }
        out.push(Chapter::inline(title, body));
    }
    out
}

/// 头部那一块读出来的书目。三个字段都是「书自己说的」，说不清就留空 —— 空在信息屏上
/// 是「不摆那一行」，不是「这本书没有作者」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BookInfo {
    pub title: String,
    pub authors: Vec<String>,
    pub description: String,
}

/// 只读头部那几十行，按规则表的别名表取书名 / 作者 / 简介。
///
/// **只看头部是有意的**：`作者：` 那一行长在文件开头，而整本去找就会把正文里那句
/// 「他写道：作者：某某」当成书目 —— 那与「正文里提到第三章」是同一类假阳性。行数上限
/// 在规则表里（`meta.head_lines`）：40 行够覆盖「书名 / 作者 / 简介 / 目录标题」这一组，
/// 真书上它们都挤在最前面，而换语言这一组未必一样长。
pub(crate) fn book_info(source: &str, stem: &str, rules: &CompiledRules) -> BookInfo {
    let meta = rules.meta();
    let head: Vec<&str> = source.lines().take(meta.head_lines.max(1)).collect();
    let value_after = |line: &str, keys: &[String]| -> Option<String> {
        let t = line.trim_matches(|c: char| c.is_whitespace());
        keys.iter().find_map(|key| {
            let rest = t.strip_prefix(key.as_str())?;
            let value = rest
                .trim_start_matches([' ', '\t', '　', ':', '：', '=', '＝', ' ', '　'])
                .trim_end();
            (!value.is_empty() && value.chars().count() <= meta.value_max_chars)
                .then(|| value.to_string())
        })
    };
    let mut out = BookInfo::default();
    for line in &head {
        if out.title.is_empty()
            && let Some(title) = value_after(line, &meta.title_keys)
        {
            out.title = title;
        }
        if out.authors.is_empty()
            && let Some(author) = value_after(line, &meta.author_keys)
        {
            // 一行里可能写「甲、乙 合著」：按常见的分隔切开，逐个去噪。
            out.authors = author
                .split(['、', '，', ',', '；', ';', '　', ' '])
                .map(|s| s.trim())
                .filter(|s| !s.is_empty() && *s != "合著" && !s.contains("著"))
                .map(String::from)
                .collect::<Vec<_>>();
        }
        if out.description.is_empty()
            && let Some(desc) = value_after(line, &meta.description_keys)
        {
            out.description = desc;
        }
    }
    // 括号裹着书名的那一行（`《书名》`）：网文导出最常见，它不写「书名：」这个键。
    // 认哪几对括号是规则表的事 —— 英文那套交空表，就是「不认括号」。
    if out.title.is_empty() {
        out.title = head
            .iter()
            .filter_map(|line| {
                let t = line.trim();
                meta.title_brackets.iter().find_map(|bracket| {
                    let (open, close) = bracket.pair();
                    let inner = t.strip_prefix(open)?.split(close).next()?;
                    (!inner.is_empty() && inner.chars().count() <= meta.value_max_chars)
                        .then(|| inner.to_string())
                })
            })
            .next()
            .unwrap_or_default();
    }
    if out.title.is_empty() {
        out.title = denoise(stem, meta);
    }
    out
}

/// 文件名去噪：剥掉 `《》`、扩展名残留与「TXT下载」那一类词。
fn denoise(stem: &str, meta: &MetaKeys) -> String {
    let trimmed = stem
        .trim_matches(|c: char| c.is_whitespace())
        .trim_matches(['《', '》', '【', '】', '_', '-', ' ']);
    let mut out = trimmed.to_string();
    for noise in &meta.filename_noise {
        out = out.replace(noise.as_str(), " ");
    }
    // 剥噪声词之后可能又露出括号来（`《书名》TXT下载` 去掉 `TXT下载` 才轮到尾部那个 `》`），
    // 所以再夹一次；顺序反过来就会留下一个半个括号的书名。
    let joined = out.split_whitespace().collect::<Vec<_>>().join(" ");
    joined
        .trim_matches(['《', '》', '【', '】', '_', '-', ' '])
        .to_string()
}

/// 引擎的入口：规范化文本 → 章表。判定不通过就回落成一整篇一章。
pub(crate) fn split_with(
    source: &str,
    fallback_title: &str,
    rules: &CompiledRules,
) -> Vec<Chapter> {
    let lines: Vec<&str> = source.split('\n').collect();
    let found = find_hits(&lines, rules);
    let hits = drop_toc_page(&found, &lines, rules);
    let toc_dropped = hits.len() < found.len();
    let hits = keep_monotonic(&hits);
    if toc_dropped {
        tracing::debug!(
            "纯文本头部认出 {} 枚目录行，整块跳过不当章",
            found.len() - hits.len()
        );
    }
    if !looks_chaptered(&hits, &lines, rules) {
        if !hits.is_empty() {
            tracing::debug!(
                "纯文本认出 {} 枚章标记，但过不了数量与间隔门，按一整篇一章排",
                hits.len()
            );
        }
        return vec![Chapter::inline(fallback_title, source.to_owned())];
    }
    let chapters = cut(&lines, &hits, fallback_title, toc_dropped);
    // 靠哪几条规则切开的要说得出来：读者反馈「这本分错了」时，这一行就是第一个要看的东西
    // —— 是模式没认到、还是目录页判错、还是递增把真章丢了，三件事在屏上长得一模一样。
    let mut by_marker: std::collections::BTreeMap<&str, usize> = Default::default();
    for hit in &hits {
        *by_marker.entry(hit.marker.as_str()).or_default() += 1;
    }
    tracing::debug!(
        "纯文本按规则切出 {} 章（规则 {}@{}，条目分布 {:?}，已作废条目 {:?}）",
        chapters.len(),
        rules.locale(),
        rules.content_hash(),
        by_marker,
        rules.bad_entries()
    );
    if chapters.len() < 2 {
        // 切完只剩一章（标记全挤在开头或全被丢空）：与过不了门同处置，别交出一本
        // 「有一章目录却只有一页」的书。
        return vec![Chapter::inline(fallback_title, source.to_owned())];
    }
    chapters
}

/// 章表 → 目录（现在是平的一层：卷那族认出来但不切，第二层是将来的一步）。
pub(crate) fn toc_of(chapters: &[Chapter]) -> Vec<TocEntry> {
    chapters
        .iter()
        .map(|chapter| TocEntry {
            title: chapter.title.clone(),
            href: String::new(),
            fragment: None,
            children: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    //! 引擎的门。规则表本身（坏条目降级、退回出厂、内容哈希）在 `rules` 那侧。
    //!
    //! 这里的样本是**合成的**，形状照真书（PG 公版八本量出来的分布写在
    //! `assets/novel-rules.zh-Hans.json` 的 `gates.comment`）。真书那批不在仓库里，
    //! 由 `examples/novel-probe.rs` 逐本打印章数与丢字数来验，不在这里当门。

    use super::{book_info, parse_number, split_with};
    use crate::content::format::novel::rules::CompiledRules;
    use crate::content::format::text::normalize;
    use crate::content::format::{visible_text, xhtml};

    fn prose(n: usize) -> String {
        "雪".repeat(n)
    }

    /// 刻本省略式与标准写法、阿拉伯、全角一起过一遍。
    /// 少这一档的话，《西游记》那种本子的 90 枚标记只落出 14 个号，整本连不起来。
    #[test]
    fn numerals_from_four_writing_systems_chain_up() {
        let rules = CompiledRules::builtin();
        for (text, want) in [
            ("一", 1),
            ("九", 9),
            ("十", 10),
            ("十一", 11),
            ("一一", 11),
            ("二十", 20),
            ("二一", 21),
            ("三五", 35),
            ("一百零八", 108),
            ("12", 12),
            ("１２", 12),
            // 白圈当零的那个本子：`一○`=10、`一○五`=105。少了这一条，西游整本的整十回
            // 都会「切出来了但号读不出」—— 屏上看着对、目录跳不对，比丢章更难查。
            ("一○", 10),
            ("一○五", 105),
            ("二○五", 205),
        ] {
            assert_eq!(
                parse_number(text, &rules),
                Some(want),
                "{text} 该读成 {want}"
            );
        }
        // 表外的字符不算号：认不出就当这一枚不带号，而不是猜一个。
        assert_eq!(parse_number("壹", &rules), None, "繁体大写数字不在表里");
    }

    /// 一本带目录页的网文：目录那一整块**不许**被当成章。
    /// 这块同时是递增校验的放大器 —— 目录里的 1..6 会把正文的 1..6 全判成回退，
    /// 于是整本一章都分不出来，所以两件事必须在同一道门里断言。
    #[test]
    fn a_contents_page_is_skipped_and_does_not_poison_the_sequence() {
        let mut body =
            String::from("书名：雪夜关门\n作者：无名氏\n内容简介：一句话说完。\n\n目 录\n");
        for n in 1..=6 {
            body.push_str(&format!("第{n}章 标题{n}\t{n}\n"));
        }
        body.push('\n');
        for n in 1..=6 {
            body.push_str(&format!("第{n}章 风雪夜关门\n{}\n\n", prose(600)));
        }
        let source = normalize(&body);
        let chapters = split_with(&source, "雪夜关门", &CompiledRules::builtin());
        // 第一个标记之前那一段（书名页 + 目录页）留成一章，名字说出它是什么 ——
        // 目录那些行是书里的字，吞掉就断了「分章不许吃字」那条不变量。
        assert_eq!(
            chapters.len(),
            7,
            "六章正文 + 头部那一块： {:?}",
            chapters
                .iter()
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(chapters[0].title, "目录", "认出目录页就该这么叫它");
        assert_eq!(chapters[1].title, "第1章 风雪夜关门");
        for (ix, chapter) in chapters[1..].iter().enumerate() {
            assert!(
                chapter.title.starts_with(&format!("第{}章", ix + 1)),
                "目录不许污染递增：{}",
                chapter.title
            );
        }
        // 目录行本身留在第一章之前的那块里，一个字没丢。
        assert!(chapters[0].source().contains("标题3"), "目录页的字要留着");
    }

    /// **分章不许吃字**：各章可见文本拼回去，去掉空白后必须等于原文去掉空白。
    /// 这条是整个引擎最要紧的不变量 —— 锚点与进度就建在可见文本上。
    #[test]
    fn splitting_eats_no_characters() {
        let mut body = String::new();
        for n in 1..=5 {
            body.push_str(&format!("第{n}章 标题\n{}\n\n", prose(400)));
        }
        let source = normalize(&body);
        let chapters = split_with(&source, "书名", &CompiledRules::builtin());
        assert_eq!(chapters.len(), 5);
        let joined: String = chapters
            .iter()
            .map(|c| visible_text(&xhtml::from_plain_text(c.source())))
            .collect();
        let strip = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
        assert_eq!(
            strip(&joined),
            strip(&source),
            "分章既不能丢字也不能造字（注入的 `#` 与换的行必须是空白级的改动）"
        );
        // 独立断一次 `#` 真的没进可见文本：两趟都坏也会相等。
        assert!(!joined.contains('#'), "注入的 ATX 标记该被包装剥掉");
    }

    /// 标记后面粘着整段正文（转档噪声，真书上见过 618 字一行）：标题截断，
    /// **多出来的字必须留在正文开头**，不能跟着进章名而消失。
    /// 「编号」那一族曾经吃掉一整本：红楼第十七回之后有一句正文
    /// `五六．又值人來回，有雨村處遣人回話．…` 被它命中、读成 56，于是第十八到五十六回
    /// 全被判成「号回退」而丢掉（60/99）。修法是给那条模式加**尾巴必须短**的上限。
    /// 把 `title_max` 那一段上限从模式里去掉，这一条就红。
    #[test]
    fn a_prose_line_starting_with_two_numerals_is_not_a_chapter() {
        let mut body = String::new();
        for n in 1..=12 {
            body.push_str(&format!("第{n}章 开\n{}\n\n", "雪".repeat(400)));
            if n == 5 {
                // 行首是两个数字加一个全角句号，后面是一整句正文。
                body.push_str("五六．又值人來回，有雨村處遣人回話．賈政笑道此數處不能游了\n");
            }
        }
        let source = normalize(&body);
        let split = split_with(&source, "书名", &CompiledRules::builtin());
        assert_eq!(
            split.len(),
            12,
            "那一句正文不该被当成第五十六章：{:?}",
            split
                .iter()
                .map(|c| c.title.chars().take(6).collect::<String>())
                .collect::<Vec<_>>()
        );
        assert!(
            split
                .iter()
                .all(|c| c.title.chars().take(3).collect::<String>() != "五六．"),
            "正文行不许成为一章"
        );
    }

    /// 头部那一块把书名与作者说出来：这是「按内容提取」那一问的正身。
    /// 三个来源各断一次（`书名：` 键、`《…》` 那一行、以及两者都没有时**不许**瞎猜）。
    #[test]
    fn the_head_block_names_the_book_the_author_and_the_blurb() {
        use crate::content::format::novel::rules::CompiledRules;
        let rules = CompiledRules::builtin();
        let head =
            "书名：雪夜关门\n作者：无名氏、补刀居士\n内容简介：一夜之间，门里门外两个人。\n\n";
        let body: String = (1..=4)
            .map(|n| format!("第{n}章 关门\n{}\n\n", "雪".repeat(400)))
            .collect();
        let info = book_info(&(head.to_string() + &body), "随手起的文件名", &rules);
        assert_eq!(info.title, "雪夜关门", "「书名：」那一行要用上");
        assert_eq!(
            info.authors,
            vec!["无名氏", "补刀居士"],
            "两个作者都要收到：{:?}",
            info.authors
        );
        assert_eq!(info.description, "一夜之间，门里门外两个人。");

        // 只写《书名》那一行（网文导出最常见，不写「书名：」这个键）。
        let info = book_info(&format!("《雪夜关门》\n\n{body}"), "随手起的文件名", &rules);
        assert_eq!(info.title, "雪夜关门", "《…》 那一行也要认");
        assert!(info.authors.is_empty(), "头部没说作者就是空，不是猜一个");
    }

    /// 头部什么都没说：回落**去噪后的文件名**，而且作者留空。
    /// 空在信息屏上是「不摆那一行」；填一个猜出来的作者比空着更糟。
    #[test]
    fn a_silent_head_falls_back_to_the_denoised_file_name() {
        let rules = CompiledRules::builtin();
        let body: String = (1..=4)
            .map(|n| format!("第{n}章 开门\n{}\n\n", "雪".repeat(400)))
            .collect();
        let info = book_info(&body, "《雪夜关门》TXT下载", &rules);
        assert_eq!(
            info.title, "雪夜关门",
            "文件名里的书名括号与噪声词都要剥掉：{:?}",
            info.title
        );
        assert!(
            info.authors.is_empty() && info.description.is_empty(),
            "没说过的一律留空"
        );
    }

    #[test]
    fn a_glued_long_tail_keeps_every_character() {
        let tail = prose(300);
        let mut body = String::new();
        for n in 1..=4 {
            // 标记行粘着整段（真书见过 618 字一行），后面另有正常的正文行。
            body.push_str(&format!("第{n}章{tail}\n{}\n\n", prose(400)));
        }
        let source = normalize(&body);
        let chapters = split_with(&source, "书名", &CompiledRules::builtin());
        assert_eq!(chapters.len(), 4, "粘连不该让整本分不出章");
        let title_chars = chapters[0].title.chars().count();
        assert!(
            title_chars <= CompiledRules::builtin().gates().title_max_chars,
            "章名该被夹住：{title_chars} 字"
        );
        let joined: String = chapters
            .iter()
            .map(|c| visible_text(&xhtml::from_plain_text(c.source())))
            .collect();
        let strip = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
        assert_eq!(strip(&joined), strip(&source), "截标题不许把正文截走");
        assert!(
            chapters[0].source().contains(&tail[0..60]),
            "尾巴上那些字要留在正文里，不能跟着章名消失"
        );
    }

    /// 正文里那句「他想起第三章那句话」不该成为一章：号回退了就被递增那一档丢掉。
    #[test]
    fn a_citation_in_the_body_does_not_become_a_chapter() {
        let mut body = String::new();
        for n in 1..=5 {
            body.push_str(&format!("第{n}章 开\n{}\n", prose(400)));
            if n == 3 {
                // 行首、整行几乎就是标记，但号往回走 —— 这是引用不是章。
                body.push_str("第二章 回忆里的那一句\n");
            }
            body.push('\n');
        }
        let source = normalize(&body);
        let chapters = split_with(&source, "书名", &CompiledRules::builtin());
        assert_eq!(
            chapters.len(),
            5,
            "回退的那枚该被丢掉： {:?}",
            chapters
                .iter()
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            !chapters.iter().any(|c| c.title.contains("回忆")),
            "引用行不该变成一章"
        );
    }

    /// 通篇没有标记 → 回落一整篇。而「楔子 / 第一章 / 第二章 / 尾声」这种**固定名与
    /// 编号混着**的形态要真的切开（不带号的那两枚也各成一章）。
    #[test]
    fn no_markers_falls_back_but_named_markers_do_cut() {
        let rules = CompiledRules::builtin();
        let none = normalize(&format!("通篇正文。\n{}\n", prose(2000)));
        assert_eq!(split_with(&none, "书名", &rules).len(), 1);

        let mixed = normalize(&format!(
            "楔子\n{}\n\n第一章 起\n{}\n\n第二章 承\n{}\n\n尾声\n{}\n",
            prose(500),
            prose(500),
            prose(500),
            prose(500)
        ));
        let split = split_with(&mixed, "书名", &rules);
        assert_eq!(
            split.len(),
            4,
            "四章：{:?}",
            split.iter().map(|c| c.title.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(split[0].title, "楔子", "不带号的那几枚也该切成章");
        assert_eq!(split[1].title, "第一章 起");
        assert_eq!(split[3].title, "尾声");
    }

    /// 只有两枚标记的书（不到 `min_chapters`）按一整篇排 —— 宁可不分，不可分错。
    #[test]
    fn fewer_markers_than_the_minimum_stays_one_chapter() {
        let source = normalize(&format!("楔子\n{}\n\n正文\n{}\n", prose(900), prose(900)));
        let chapters = split_with(&source, "书名", &CompiledRules::builtin());
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].title, "书名", "回落时章名就是文件名主名");
    }

    /// 真书那一遍：语料在 `tests/fixtures/local/txt/`（Project Gutenberg 公版全文，不在
    /// 仓库里），有就跑、没有就安静跳过。断言三条：**号连得起来**、**没有重号**、
    /// **一个字都不丢**。
    ///
    /// 为什么这几条值得钉成门而不是打印出来看一眼：它们是「这本能不能看」的判据本身。
    /// 号断了就是递增校验把真章判成了引用，重号就是目录页没被跳干净，丢字就是切点动了正文。
    #[test]
    #[ignore = "需要仓库外的真语料：从 Project Gutenberg 下几本公版小说进 tests/fixtures/local/txt"]
    fn real_novels_split_with_a_continuous_numbering_and_lose_no_characters() {
        use crate::content::format::{Content, provider_for};
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/local/txt");
        if !dir.is_dir() {
            eprintln!("{} 不存在，跳过", dir.display());
            return;
        }
        let mut books: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "txt"))
            .collect();
        books.sort();
        assert!(!books.is_empty(), "目录在但一本 .txt 都没有");
        let builtin = crate::content::format::novel::rules::CompiledRules::builtin();
        let mut checked = 0usize;
        for path in &books {
            let Some(provider) = provider_for(path) else {
                continue;
            };
            let (meta, content) = provider
                .read(path)
                .unwrap_or_else(|e| panic!("{} 读不动：{e}", path.display()));
            let Content::Reflow(flow) = content else {
                continue;
            };
            // 只有真分出多章的书才断言号序；认不出标记而回落一整篇的那些归另一条门管。
            if flow.chapters.len() < 5 {
                eprintln!(
                    "{}：认出 {} 章（回落或未认到），跳过号序断言",
                    meta.title,
                    flow.chapters.len()
                );
                continue;
            }
            let numbers: Vec<usize> = flow
                .chapters
                .iter()
                .filter_map(|c| {
                    let head = c.title.strip_prefix('第')?;
                    // 只取**号本身**：碰到非数字字符就停。整串前缀塞进去会被解析器拒收
                    // （「一回　甄士隱」里的「回」不在数字表里），那样 numbers 全空、
                    // 缺号与重号两条断言就变成永远绿的假灯 —— 这一条门差点就这么废了。
                    let digits: String = head
                        .chars()
                        .take_while(|c| builtin.digits().contains(*c) || c.is_ascii_digit())
                        .collect();
                    parse_number(&digits, &builtin)
                })
                .collect();
            let max = numbers.iter().copied().max().unwrap_or(0);
            let missing: Vec<usize> = (1..=max).filter(|n| !numbers.contains(n)).collect();
            // 这条**不是**「不许缺号」，是一条覆盖率门：断在中间就把名单打出来，让人看见。
            // 现在的实测数（八本公版）：红楼 120/120、西游 100/100、三国 120/120、水浒 70/70、
            // 老残 20/20、呐喊 9/9 全连；只有儒林外史 40/56 —— 那是**那本电子书自己少了
            // 第二到第十一回**（整篇搜「第二回」零命中），不是引擎丢的。所以下限钉 0.7 而不是
            // 1.0：再往上是拿语料的缺陷冤枉引擎，再往下就放得过真 regress。
            // 历史上这一条门第一次跑是 60/99：毒源是一条正文行「五六．又值人來回，…」被
            // 「编号」那族吃成 56，把第十八到五十六回全判成回退 —— 修法是收紧那条模式的
            // 长度上限，不是把校验调松。
            let coverage = numbers.len() as f64 / max as f64;
            assert!(
                coverage >= 0.7,
                "{}：{} 章里号只连起 {}/{}（缺 {:?}",
                meta.title,
                flow.chapters.len(),
                numbers.len(),
                max,
                &missing[..missing.len().min(8)]
            );
            eprintln!(
                "{}：{} 章，号 {}/{}，缺号 {} 处 {:?}",
                meta.title,
                flow.chapters.len(),
                numbers.len(),
                max,
                missing.len(),
                &missing[..missing.len().min(6)]
            );
            let mut sorted = numbers.clone();
            sorted.sort_unstable();
            let dups = sorted.windows(2).filter(|w| w[0] == w[1]).count();
            assert_eq!(
                dups, 0,
                "{}：有 {} 处重号 —— 目录页没跳干净",
                meta.title, dups
            );
            let kept: usize = flow
                .chapters
                .iter()
                .map(|c| {
                    crate::content::format::visible_text(c.source())
                        .chars()
                        .filter(|ch| !ch.is_whitespace())
                        .count()
                })
                .sum();
            let bytes = std::fs::read(path).unwrap();
            let raw = String::from_utf8_lossy(&bytes);
            let want: usize = raw.chars().filter(|ch| !ch.is_whitespace()).count();
            assert!(
                want.abs_diff(kept) <= 4,
                "{}：丢字 {} vs {}",
                meta.title,
                want,
                kept
            );
            eprintln!(
                "{}：{} 章，号到 {max}，非空白字符 {kept} / {want}",
                meta.title,
                flow.chapters.len()
            );
            checked += 1;
        }
        assert!(
            checked >= 3,
            "真分出多章的书太少（{checked} 本），这批语料不足以说明问题"
        );
    }
}
