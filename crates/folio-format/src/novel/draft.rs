//! 「这本书该怎么切」那一条路：取样 → 把模型答的那一条解成 [`Marker`] → 按它先切一遍给
//! 读者看后果。
//!
//! 三件事同住一格，是因为它们同属一个判断：**模型给的那条规则能不能用在真书上**。取样决定
//! 它看见什么，解析决定它说什么都算，预览决定读者在按下「用上」之前看得见什么。
//!
//! ## 预览必须由引擎自己算，不能让模型估
//!
//! `min_chapters`、递增校验、目录页那三道门互相牵制（红楼那本被「编号」那族吃掉一整本，
//! 修的是收紧那条模式的尾巴 —— 见 `assets/novel-rules.zh-Hans.json` 里那条的 `examples`），
//! 而模型只看得见行面上像不像标题。这一格是白拿的（[`detect::split_with`] 本来就在，
//! 一次全扫而已），而它是唯一说得出「按它切只剩 1 章」的东西 —— 那正是拒掉一条草案要的那句
//! 话。所以 [`preview`] 交的是**数**，不是置信度。
//!
//! ## 取样取的是形状，不是判断
//!
//! [`sample`] 那串候选行的词表只用来**挑行**，不参与「这是不是章」的判定 —— 判定全在生效
//! 那张表里。因此词表漏了某一族写法（`—— 一 ——`）的代价只是模型少看见几行，头部那 60 行
//! 照旧在场。
//!
//! ## 这一格不生效任何东西
//!
//! 草案落不落进 `Edits.extra` 由界面那一步决定（`doc/AI 计划.md` 的硬约束：不变量不进数据，
//! 模型没有眼睛）。生效那一张表换不换仍是 [`super::rules::install`] 那一次，只在开机。

use std::path::Path;

use serde::Deserialize;

use super::detect;
use super::rules::{self, Edits, Marker, MatchKind, NumberingStyle};
use crate::Result; // 只给 [`source_at`] 用；`DraftError` 那一档写全 `std::result::Result`。

/// 取样取的头部行数。一本书的头部那一块同时挂着书名、作者、目录页与版权噪声，
/// 60 行是「目录页一定在里面、正文第一页也还在里面」的那个长度。
pub const HEAD_LINES: usize = 60;

/// 候选标记行的条数上限。
pub const CANDIDATE_LINES: usize = 30;

/// 标记本身那一截留给多长（字符）：「第三百二十七回」这种汉字号加前缀，20 个字用得完。
const MARK_ALLOWANCE: usize = 20;

/// 每一行送出去的长度上限（字符）= **出厂那格 `title_max_chars` + 标记那一截**。
///
/// 不从别处另编一个数：`title_max_chars` 是「真章标题最长 35 字（红楼梦那一族的对句）」
/// 量出来的那一格（`assets/novel-rules.zh-Hans.json`），一行比「标题 + 标记」还长就不是
/// 标题，而是转档噪声粘着整段正文（真见过 618 字的一行）。截在这里省下的是载荷，
/// 留下的是行的开头 —— 判形状要看的正是开头。
fn line_cap() -> usize {
    rules::builtin_table().gates.title_max_chars + MARK_ALLOWANCE
}

/// 一行「像不像候选标记行」用的那些字与词。**只用来挑行**，见模块头。
///
/// 中英日三族混在一格里是有意的：取样不问这本书是什么语言（`.txt` 的书名与正文可能三种
/// 都有），而漏一族的代价只是少看几行候选。
const HEADING_MARKS: &[&str] = &[
    "第", "章", "回", "节", "篇", "卷", "部", "幕", "折", "讲", "辑", "则", "序", "楔", "番外",
    "后记", "尾声", "chapter", "part", "section", "prologue", "epilogue", "volume",
];

/// 送给模型看的那两份样本。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sample {
    /// 文件头部那几行，原序。
    pub head: Vec<String>,
    /// 全本书里挑出来的候选标记行，按**文件里的位置均匀铺开**（见 [`even_spread`]）。
    pub candidates: Vec<String>,
}

/// 读一遍并按 `.txt` 导入同一条路折好（`charset::read_plain` → `normalize`）。
///
/// 必须是同一份规范化文本：预览与取样吃的是**同一把尺**，而 `ChapterLocator::Section`
/// 数的也是它。从别的入口拿一份没折过的文本，按它算出来的章数与入库的那一套就对不上。
pub fn source_at(path: &Path) -> Result<String> {
    crate::text::read_normalized(path)
}

/// 按这份文本取样。
pub fn sample(source: &str) -> Sample {
    let lines: Vec<&str> = source.split('\n').collect();
    let head: Vec<String> = lines
        .iter()
        .take(HEAD_LINES)
        .map(|line| clip_line(line.trim()))
        .collect();
    let picked: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| looks_like_heading_candidate(line))
        .map(|(index, _)| index)
        .collect();
    Sample {
        head,
        candidates: even_spread(&picked)
            .into_iter()
            .map(|index| clip_line(lines[index].trim()))
            .collect(),
    }
}

impl Sample {
    /// 样本空到没法问了吗（一本连一行字都挑不出、头部也全是空行的文件）。
    ///
    /// 界面用它决定**这一趟要不要发出去**：拿一份空样本去问，模型只能编一条规则，
    /// 而那条规则看着像答案 —— 比拒掉更坏。
    pub fn is_useless(&self) -> bool {
        self.head.iter().all(|line| line.is_empty())
            && self.candidates.iter().all(|line| line.is_empty())
    }
}

/// 候选行太多时**均匀铺开**取 [`CANDIDATE_LINES`] 条，而不是取前 30 条。
///
/// 取前 30 条只看得到这本书的开头，而这一刀要防的正是「前半本用「第一章」、后半本换成
/// 「52。」」那种混写：样本只给开头，模型就照开头那条答，预览再拿全本书一算 —— 章数会
/// 说出来，但读者已经白等一趟。首尾都必取（`include_endpoints`），因为那是最常换写法的地方。
fn even_spread(indexes: &[usize]) -> Vec<usize> {
    if indexes.len() <= CANDIDATE_LINES {
        return indexes.to_vec();
    }
    // 步长取整会漏掉最后几条，所以按 `i * (n-1) / (k-1)` 直接落到两端的位置上。
    let last = indexes.len() - 1;
    (0..CANDIDATE_LINES)
        .map(|slot| indexes[slot * last / (CANDIDATE_LINES - 1)])
        .collect()
}

/// 这一行值不值得让模型看一眼。**判断不在这里** —— 见模块头那一句。
fn looks_like_heading_candidate(line: &str) -> bool {
    let text = line.trim();
    if text.is_empty() || text.chars().count() > line_cap() {
        return false;
    }
    // 带号的行优先：绝大多数章标记都含数字（阿拉伯或汉字）。
    let has_number = text
        .chars()
        .any(|c| c.is_ascii_digit() || "〇○零一二三四五六七八九十百千万两".contains(c));
    let lower = text.to_lowercase();
    has_number || HEADING_MARKS.iter().any(|mark| lower.contains(mark))
}

/// 一行收到 [`line_cap`]，收的时候留一个省略号（收顶要看得见，不然样本里全是「正好那么长」
/// 的行，模型以为那就是上限的形状）。
fn clip_line(text: &str) -> String {
    let cap = line_cap();
    let count = text.chars().count();
    if count <= cap {
        return text.to_string();
    }
    let mut out: String = text.chars().take(cap - 1).collect();
    out.push('…');
    out
}

/// 模型那一份答案解出来的草案：一条规则 + 一句给人看的理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub marker: Marker,
    pub reason: String,
}

/// 草案解不开的档位。分档的判据与 `net::ai::AiError` 同一条：**读者能不能自己处理**。
/// 「答案不是 JSON」与「JSON 里没给模式」在界面上是两句话 —— 前者该重问一次，
/// 后者是这一档模型根本没说清该切哪儿，重问也是白花钱。
#[derive(Debug, thiserror::Error)]
pub enum DraftError {
    #[error("答案里找不到那段 JSON")]
    NoJson,

    #[error("那段 JSON 解不开：{0}")]
    Malformed(String),

    #[error("这条规则说自己是「{0}」那一档，而这一档不在建议的范围里")]
    BadKind(String),

    #[error("答案没给出{0}")]
    MissingField(&'static str),
}

/// 答案里那段 JSON 的形状。**只有这一格出现在网络上**，与 `Marker` 的 serde 形状故意分开：
/// `Marker` 里 `pattern`/`text` 是 `Option` 且按 `kind` 二选一，那种「看情况必填」的形状
/// 让模型自己面对只会多出几种坏答案，而 `examples`、`level` 这些格它没有理由碰。
#[derive(Debug, Deserialize)]
struct Wire {
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    pattern: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    numbering: Option<String>,
    #[serde(default)]
    reason: String,
}

/// 把模型答的那一段解成一条 [`Marker`]。
///
/// 剥代码围栏与前后废话（```` ```json ```` 是新一代模型的默认包法），再解 JSON；
/// 解不出就按 [`DraftError`] 报，**不猜一条**：一条猜出来的规则会静悄悄改一整本书的分章，
/// 而章序号是落库的东西（见 `rules` 模块头那条链）。
pub fn from_answer(answer: &str) -> std::result::Result<Draft, DraftError> {
    let json = extract_json(answer).ok_or(DraftError::NoJson)?;
    let wire: Wire =
        serde_json::from_str(json).map_err(|err| DraftError::Malformed(err.to_string()))?;
    let name = name_of(&wire.name);
    let kind = match wire.kind.trim().to_lowercase().as_str() {
        "" | "regex" => MatchKind::Regex,
        "literal" => MatchKind::Literal,
        other => return Err(DraftError::BadKind(other.to_string())),
    };
    let body = match kind {
        MatchKind::Regex => {
            if wire.pattern.trim().is_empty() {
                return Err(DraftError::MissingField("那条模式"));
            }
            (Some(wire.pattern.trim().to_string()), None)
        }
        MatchKind::Literal => {
            if wire.text.trim().is_empty() {
                return Err(DraftError::MissingField("整行的内容"));
            }
            (None, Some(wire.text.trim().to_string()))
        }
    };
    Ok(Draft {
        marker: Marker {
            name,
            kind,
            pattern: body.0,
            text: body.1,
            level: 1,
            numbering: numbering_of(wire.numbering.as_deref()),
            examples: Default::default(),
        },
        reason: wire.reason.trim().to_string(),
    })
}

/// 名字：模型给的空白与超长都收掉，空的那一档给一个能指名道姓的替身。
///
/// 为什么不留空：`off` 那一格按**名字**作用，一条没有名字的规则关不掉也认不出，而诊断里
/// 要指名道姓它（见 `Marker::name` 的文档）。
fn name_of(raw: &str) -> String {
    let trimmed = raw.trim();
    let count = trimmed.chars().count();
    if trimmed.is_empty() {
        return "建议的规则".to_string();
    }
    if count > 24 {
        return trimmed.chars().take(24).collect();
    }
    trimmed.to_string()
}

/// `numbering` 认 `none`/`chinese`/`arabic`/`either`；其余（模型爱写 `"auto"`、`"numeric"`）
/// 一律退回 `Either` —— 它是出厂表里最宽的那一档，也是最不会因误判而丢章的那一档。
fn numbering_of(raw: Option<&str>) -> NumberingStyle {
    match raw.unwrap_or_default().trim().to_lowercase().as_str() {
        "none" => NumberingStyle::None,
        "chinese" => NumberingStyle::Chinese,
        "arabic" => NumberingStyle::Arabic,
        _ => NumberingStyle::Either,
    }
}

/// 取答案里第一个 `{` 到最后一个 `}` 那一段。围栏、前言、后语都在这两笔之间被剪掉。
///
/// 不做严格的括号配平扫描：模型给的多余花括号极少，而真给坏了时 `from_str` 会报
/// [`DraftError::Malformed`]，界面上说的还是那句「这次没帮上」—— 不是静默，也不是猜。
fn extract_json(answer: &str) -> Option<&str> {
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    if end < start {
        return None;
    }
    Some(&answer[start..=end])
}

/// 按一条草案切一遍这份文本，交回**数**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// 按它切会得到几章。`1` 就是「这门没过」—— 引擎在过不了数量与间隔门时回落成一整篇。
    pub chapters: usize,
    /// 各章正文可见字数的中位数（与库里 `chapters.char_len` 同一把尺）。
    pub median_body_chars: usize,
    /// 这一份预览**按的那道门**是多少（合成那张表的 `min_median_chapter_chars`）。
    ///
    /// 带出来而不是让调用方去查：判定与测量必须同一趟，否则读者改过阈值之后界面上按的是
    /// 新数、算的是旧表，两句都对不上。
    pub min_median_chapter_chars: usize,
    /// 切出来的前几章的标题，给读者**肉眼过一遍**用。
    ///
    /// 为什么不只给数：214 章、中位 2300 字两个数都可能是「切错了但切得多」，而读者认得
    /// 自己那本书的第一章该叫什么。这一格买的是「不用信模型也能拒」。
    pub titles: Vec<String>,
    /// 这一条在编译期就被作废了（正则解不开）时，给它的那个名字。`None` = 编得过。
    ///
    /// 自检那一道（每条规则自己的正反例）在这里**不适用**：读者手加的那条与模型给的那条
    /// 都没有 `examples`（见 `Marker::examples` 与设置页那一屏收的格），所以能钉住草案的
    /// 只有「编得过」与「按它切得动」两道。
    pub rejected: Option<String>,
}

/// 给一条草案**先切一遍**。不碰生效那一张表（[`rules::compiled`] 合成的那份只活在这一次调用里）。
pub fn preview(source: &str, marker: &Marker) -> Preview {
    let edits = Edits {
        extra: vec![marker.clone()],
        ..Default::default()
    };
    let table = rules::compiled(&edits);
    let rejected = if table.bad_entries().contains(&marker.name) {
        Some(marker.name.clone())
    } else {
        None
    };
    let chapters = detect::split_with(source, "", &table);
    let min_median_chapter_chars = table.gates().min_median_chapter_chars;
    let mut sizes: Vec<usize> = chapters
        .iter()
        .filter_map(|chapter| chapter.char_len)
        .collect();
    sizes.sort_unstable();
    let median_body_chars = match sizes.len() {
        0 => 0,
        // 偶数档取中间偏左那一个：这一格只给人看数量级，取哪一半都不改变「200 字那条门
        // 过没过」的判定，而 `split_with` 自己判间隔门用的也是同一个中位数口径。
        n => sizes[n / 2],
    };
    Preview {
        chapters: chapters.len(),
        median_body_chars,
        min_median_chapter_chars,
        titles: chapters
            .iter()
            .take(3)
            .map(|chapter| chapter.title.clone())
            .collect(),
        rejected,
    }
}

impl Preview {
    /// 这条草案能不能按下去「用上」。判据全在**引擎自己算出来的数**上，一条都不问模型：
    ///
    /// - 编得过（模式解得开）；
    /// - 切得动（多于 1 章 —— 只剩 1 章就是 `split_with` 回落成一整篇，等于没切）；
    /// - 切得不碎（中位数达到 `min_median_chapter_chars` 那条门：一条太松的规则会把正文
    ///   每几行切一刀，章数很大而每章很短，那正是红楼那一族的反面）。
    pub fn is_usable(&self) -> bool {
        self.rejected.is_none()
            && self.chapters > 1
            && self.median_body_chars >= self.min_median_chapter_chars
    }
}

/// 给草案挑一个**不撞名**的名字。
///
/// 为什么这一格不能撞：`off` 那一层按**名字**作用（见 [`Edits::holds`]），两条同名条目在
/// 界面上长得一模一样，而按下「关掉」会一起没 —— 读者以为自己关的是 AI 给的那条，实际
/// 把出厂那条也关了。后缀从 2 起，因为不带后缀那一档就是模型给的原名。
pub fn free_name(edits: &Edits, wanted: &str) -> String {
    let taken = |name: &str| {
        edits.holds(name)
            || rules::builtin_table()
                .markers
                .iter()
                .chain(rules::builtin_table().volumes.iter())
                .any(|marker| marker.name == name)
    };
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return free_name(edits, "建议的规则");
    }
    if !taken(wanted) {
        return wanted.to_string();
    }
    (2..)
        .map(|suffix| format!("{wanted} · {suffix}"))
        .find(|candidate| !taken(candidate))
        .expect("后缀一直加下去总有没被占用的名字")
}

#[cfg(test)]
mod tests {
    use super::rules::{self, Edits, Marker, MatchKind, NumberingStyle};
    use super::{
        Draft, DraftError, HEAD_LINES, Preview, clip_line, even_spread, extract_json, free_name,
        from_answer, line_cap, looks_like_heading_candidate, numbering_of, preview, sample,
    };
    use crate::novel::detect;
    use crate::novel::rules::{compiled, effective_hash};

    /// 一本用「== 01 ==」切章的书 —— 那一族写法不在出厂表里，正是这一刀要接的样本。
    ///
    /// 每章正文是**多行**（8 行），因为取样按行工作：单行长正文的书测不出头部那 60 行。
    fn book() -> String {
        let mut text = String::from("书名：测试本\n作者：某人\n\n");
        for index in 1..=12 {
            text.push_str(&format!("== {:02} ==\n", index));
            for _ in 0..8 {
                text.push_str(&"正文一句话。".repeat(8));
                text.push('\n');
            }
        }
        text
    }

    /// 取样给的是**行**，不是整本书：头部那 60 行在场，候选行按形状挑。
    #[test]
    fn the_sample_carries_lines_not_the_whole_book() {
        let source = book();
        let taken = sample(&source);
        assert_eq!(
            taken.head.len(),
            HEAD_LINES,
            "这本书有一百多行，头部该取满 {} 行",
            HEAD_LINES
        );
        assert!(
            taken.candidates.iter().any(|line| line.starts_with("==")),
            "候选行里该看得见那种不在出厂表里的写法：{:?}",
            taken.candidates
        );
        let chars: usize = taken
            .head
            .iter()
            .chain(taken.candidates.iter())
            .map(|line| line.chars().count())
            .sum();
        // 载荷的上限与书的长度**无关** —— 这一条才是「成本约一千多字」那笔账的形状：
        // 行数封顶（头部 60 + 候选 30）、每行收顶（标题上限 + 标记那一截）。
        let bound = (HEAD_LINES + super::CANDIDATE_LINES) * line_cap();
        assert!(chars <= bound, "样本超出封顶：{chars} > {bound}");
        // 一本十倍的长书交出**一样大**的样本：取样不随书长涨，问一次的钱是固定的。
        let long = source.repeat(10);
        let longer = sample(&long);
        let long_chars: usize = longer
            .head
            .iter()
            .chain(longer.candidates.iter())
            .map(|line| line.chars().count())
            .sum();
        assert!(
            long.chars().count() > source.chars().count() * 8
                && long_chars <= bound
                && longer.candidates.len() <= super::CANDIDATE_LINES,
            "样本跟着书长涨了：{} → {long_chars}",
            source.chars().count()
        );
    }

    /// 一行太长就是转档噪声粘着正文，不是章标题：收顶并留一个省略号。
    #[test]
    fn a_long_line_is_clipped_at_its_head() {
        let cap = line_cap();
        let clipped = clip_line(&"啊".repeat(cap * 2));
        assert_eq!(clipped.chars().count(), cap);
        assert!(clipped.ends_with('…'));
        assert!(looks_like_heading_candidate("第一章 风雪夜"));
        assert!(!looks_like_heading_candidate(""));
        assert!(
            !looks_like_heading_candidate(&"正文没有号".repeat(cap)),
            "比「标题 + 标记」还长的行不是候选"
        );
    }

    /// 候选太多时**均匀铺开**，首尾各取一条：只看开头就看不出后半本换了写法。
    #[test]
    fn the_candidates_reach_the_end_of_the_book() {
        let indexes: Vec<usize> = (0..600).step_by(3).collect();
        let spread = even_spread(&indexes);
        assert_eq!(spread.len(), super::CANDIDATE_LINES);
        assert_eq!(spread.first(), indexes.first(), "开头那条丢了");
        assert_eq!(spread.last(), indexes.last(), "结尾那条丢了");
        // 严格递增 = 真按位置铺开，不是取满 30 条同一个区块。
        assert!(spread.windows(2).all(|pair| pair[0] < pair[1]));
    }

    /// 答案的三种坏形状各自一档，**没有一种被猜成一条规则**。
    #[test]
    fn a_bad_answer_is_refused_not_guessed() {
        let prose = "我觉得可以按 == 01 == 这种行来切。";
        assert!(matches!(from_answer(prose), Err(DraftError::NoJson)));

        let broken = r#"{"name": "等号", "kind": "regex", "pattern": }"#;
        assert!(matches!(from_answer(broken), Err(DraftError::Malformed(_))));

        let no_pattern = r#"{"name": "等号", "kind": "regex"}"#;
        assert!(matches!(
            from_answer(no_pattern),
            Err(DraftError::MissingField("那条模式"))
        ));

        let other = r#"{"name": "等号", "kind": "substring", "text": "== 01 ="}"#;
        assert!(matches!(from_answer(other), Err(DraftError::BadKind(_))));
    }

    /// 围栏与前言后语都剪得掉；给的 JSON 干净时也一样解。
    #[test]
    fn the_json_is_found_through_the_fence() {
        let wrapped = "好的，建议如下：\n```json\n{\"name\":\"等号\",\"kind\":\"regex\",\"pattern\":\"^\\\\s*==\\\\s*\\\\d+\\\\s*==\",\"numbering\":\"arabic\",\"reason\":\"每章一行等号包着两位号\"}\n```\n希望有帮助。";
        let draft = from_answer(wrapped).expect("围栏里的 JSON 该解得开");
        assert_eq!(draft.marker.name, "等号");
        assert_eq!(draft.marker.kind, MatchKind::Regex);
        assert_eq!(
            draft.marker.pattern.as_deref().unwrap(),
            r"^\s*==\s*\d+\s*=="
        );
        assert_eq!(draft.reason, "每章一行等号包着两位号");
        assert!(extract_json("没有花括号").is_none());
    }

    /// 名字与档位那两个「模型会自己发明」的格子都有兜底，不许留空、不许 panic。
    #[test]
    fn the_nameless_and_the_invented_fall_back_instead_of_vanishing() {
        let draft = from_answer(r#"{"kind":"regex","pattern":"^x","numbering":"auto"}"#)
            .expect("没给名字也该成一条规则");
        assert!(
            !draft.marker.name.trim().is_empty(),
            "空名的规则关不掉也认不出"
        );
        assert_eq!(
            draft.marker.numbering,
            NumberingStyle::Either,
            "认不得的档位退回最宽那一档，而不是最窄"
        );
        assert_eq!(numbering_of(Some("CHINESE")), NumberingStyle::Chinese);
        let long = from_answer(&format!(
            r#"{{"name":"{}","kind":"regex","pattern":"^x"}}"#,
            "名".repeat(40)
        ))
        .unwrap();
        assert!(
            long.marker.name.chars().count() <= 24,
            "名字要能在界面上一行摆下"
        );
    }

    /// 预览切得动：那条草案按在这本书上是 12 章（出厂表认不得 `== 01 ==`，一整篇一章）。
    #[test]
    fn the_preview_counts_the_chapters_the_draft_would_cut() {
        let source = book();
        let marker = from_answer(
            r#"{"name":"等号","kind":"regex","pattern":"^\\s*==\\s*([0-9]{1,4})\\s*=="}"#,
        )
        .unwrap()
        .marker;
        let cut = preview(&source, &marker);
        // 13 而不是 12：第一枚标记之前那一块（书名、作者）自己成一章 —— 这是 `cut()` 在
        // 真书上的既有行为（静读天下那一族也这么摆），预览照实报引擎会交出的章数。
        assert_eq!(cut.chapters, 13, "按它切出来的章数不对：{:?}", cut.titles);
        assert!(
            cut.median_body_chars > 200,
            "每章字数中位 {}",
            cut.median_body_chars
        );
        assert_eq!(cut.titles.len(), 3, "标题只带前三章给读者肉眼过");
        assert_eq!(cut.titles[1], "== 01 ==", "第二条该是第一章的标记行");
        assert_eq!(cut.rejected, None);
        assert_eq!(
            cut.min_median_chapter_chars,
            rules::builtin_table().gates.min_median_chapter_chars
        );

        // 同一份文本按出厂表切：只剩一整篇 —— 这条就是这一刀要解决的病。
        let builtin = detect::split_with(&source, "", &compiled(&Edits::default()));
        assert_eq!(builtin.len(), 1, "出厂表本来就该切不动这本，不然对照没意义");
    }

    /// 解不开的正则**在预览里就说自己废**，而不是交出「0 章」那种像结论的数。
    #[test]
    fn an_uncompilable_pattern_says_so() {
        let marker = from_answer(r#"{"name":"坏","kind":"regex","pattern":"^(("}"#)
            .unwrap()
            .marker;
        let cut = preview(&book(), &marker);
        assert_eq!(cut.rejected.as_deref(), Some("坏"), "作废的那条要指名道姓");
        assert_eq!(cut.chapters, 1, "作废之后按出厂切 = 一整篇");
    }

    /// 「能不能按用上」三档判据都要拦住各自的坏情况。
    #[test]
    fn only_a_draft_that_cuts_and_does_not_shatter_is_usable() {
        let good = Preview {
            chapters: 214,
            median_body_chars: 2_300,
            min_median_chapter_chars: 200,
            titles: vec![],
            rejected: None,
        };
        assert!(good.is_usable());
        let one = Preview {
            chapters: 1,
            ..good.clone()
        };
        assert!(!one.is_usable(), "只剩一章 = 没切");
        let shattered = Preview {
            chapters: 4_000,
            median_body_chars: 12,
            ..good.clone()
        };
        assert!(
            !shattered.is_usable(),
            "每章十几字那种「切得很碎」必须拒掉：一条太松的模式会吃正文"
        );
        let voided = Preview {
            rejected: Some("坏".to_string()),
            ..good.clone()
        };
        assert!(!voided.is_usable());
    }

    /// 一条草案与出厂表**同场**时不能互相顶名：预览只合成，不落 `off`，所以名字撞了
    /// 也是两条各切各的 —— 这一条钉的是「预览不改生效那一份」，而不是命名规则。
    #[test]
    fn the_preview_does_not_install_anything() {
        let before = effective_hash();
        let marker = from_answer(
            r#"{"name":"等号","kind":"regex","pattern":"^\\s*==\\s*([0-9]{1,4})\\s*=="}"#,
        )
        .unwrap()
        .marker;
        let _ = preview(&book(), &marker);
        assert_eq!(effective_hash(), before, "预览把生效那张表改了");
    }

    /// 空样本要能被界面认出来：拿一份什么都没有的文本去问，模型只能编。
    #[test]
    fn an_empty_sample_is_marked_as_useless() {
        assert!(sample("\n\n   \n").is_useless());
        assert!(!sample("第一章 开端\n正文。").is_useless());
    }

    /// 名字撞了要加后缀：`off` 按名字作用，两条同名会让「关掉一条」变成关掉两条
    /// —— 而界面上它们长得一模一样。
    #[test]
    fn a_taken_name_gets_a_suffix_instead_of_shadowing() {
        let edits = Edits::default();
        assert_eq!(free_name(&edits, "等号"), "等号");
        assert_eq!(free_name(&edits, "章"), "章 · 2", "出厂那条也叫「章」");
        let with_one = Edits {
            extra: vec![Marker {
                name: "等号".to_string(),
                kind: MatchKind::Regex,
                pattern: Some("^x".to_string()),
                text: None,
                level: 1,
                numbering: NumberingStyle::Either,
                examples: Default::default(),
            }],
            ..Default::default()
        };
        assert_eq!(free_name(&with_one, "等号"), "等号 · 2");
        assert!(
            !free_name(&Edits::default(), "   ").trim().is_empty(),
            "空名也要有个能指名道姓的替身"
        );
    }

    /// 真语料那一档（样本在仓库外，与 `detect` 那几条同一条理由）。量两件要说清的事：
    /// **载荷到底有多少字**（那笔成本的账按它写），以及**预览按在这些本上切得动吗**。
    ///
    /// 断言只钉得住的两条：样本不超上限、候选行不为空（一本挑不出候选行的真书意味着
    /// 取样那一族的词表漏了，那是这一刀自己的缺陷）。耗时与章数打出来，不进断言 ——
    /// 那两个数随机器变，而「切得动」在合成样本那几条里已经钉过。
    #[test]
    #[ignore = "需要仓库外的真语料：tests/fixtures/local/txt/"]
    fn the_sample_and_the_preview_on_real_novels() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/local/txt");
        let marker = from_answer(
            r#"{"name":"回数","kind":"regex","pattern":"^\\s*第\\s*([0-9〇○零一二三四五六七八九十百千万]{1,12})\\s*回"}"#,
        )
        .unwrap()
        .marker;
        let mut books = 0;
        for entry in std::fs::read_dir(&dir).expect("真语料目录不在") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("txt") {
                continue;
            }
            let started = std::time::Instant::now();
            let source = super::source_at(&path).expect("读不开");
            let read_us = started.elapsed().as_micros();
            let started = std::time::Instant::now();
            let taken = sample(&source);
            let sample_us = started.elapsed().as_micros();
            let chars: usize = taken
                .head
                .iter()
                .chain(taken.candidates.iter())
                .map(|line| line.chars().count())
                .sum();
            assert!(chars <= (HEAD_LINES + super::CANDIDATE_LINES) * line_cap());
            assert!(
                !taken.candidates.is_empty(),
                "{} 挑不出一行候选：取样的词表漏了这一本",
                path.file_name().unwrap().to_string_lossy()
            );
            let started = std::time::Instant::now();
            let cut = preview(&source, &marker);
            let preview_us = started.elapsed().as_micros();
            println!(
                "{}：全文 {} 字｜样本 {} 字（读 {} µs + 取样 {} µs）｜预览 {} 章·中位 {} 字·{} µs",
                path.file_name().unwrap().to_string_lossy(),
                source.chars().count(),
                chars,
                read_us,
                sample_us,
                cut.chapters,
                cut.median_body_chars,
                preview_us,
            );
            books += 1;
        }
        assert!(books >= 6, "真语料只有 {books} 本，不足以说这句话");
    }

    /// `Draft` 的 PartialEq 是给上面这些断言用的；这里顺带钉住它比的是 marker 全等。
    #[test]
    fn a_draft_compares_by_its_rule_and_its_reason() {
        let a = from_answer(r#"{"name":"x","kind":"regex","pattern":"^a","reason":"一"}"#).unwrap();
        let b: Draft = a.clone();
        assert_eq!(a, b);
    }
}
