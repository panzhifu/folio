//! 纯文本小说的**规则表**：形状、出厂默认、读取与校验。这一份里一个字都不认得「扫描」。
//!
//! ## 为什么规则是数据
//!
//! 章标记的模式族要按语言换一套（简中的「第X章／楔子／番外」、日轻的「第X話／プロローグ」、
//! 英文的「Chapter II」），而同一语言内也常有读者手里那本书用的写法不在表里。写死在代码里
//! 就是「换一套 = 发一版」；写成数据之后引擎只有一份，加一门语言是加一份文件。
//!
//! ## 每条规则自带匹配方式
//!
//! [`MatchKind::Literal`]（整行比对）与 [`MatchKind::Regex`]（编译模式）分开，只有后者
//! 编译一次并存进 [`CompiledRules`] —— 出厂那 15 条里只有 3 条是正则，其余走 `==`。
//! 一条模式解不开**只作废那一条**并留一句 warn，其余照用：整份退回出厂的下场是读者改坏
//! 一个字符就看不见目录，而那本来是一次说得清的局部失效。
//!
//! ## 规则变成数据之后，有两件事没人替我们守了
//!
//! **一、分章规则决定 `ChapterLocator::Section` 的序号含义。** 库里存的不是字节区间而是
//! 第几章；规则一改（换一份默认表、或读者改了一份），老库的进度与批注会**安静地**指到隔壁
//! 那一章。所以生效那套表的内容哈希要进失效判据 —— 见 [`CompiledRules::content_hash`]。
//!
//! **二、出厂默认永远在场。** 读不到、解不开、校验不过 → 交回 [`CompiledRules::builtin`]，
//! 症状是「按出厂那样分章」，绝不是一本书打不开。见 [`compile`]。

use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

/// 出厂那份简体中文规则（阈值在真书上量过，出处写在文件里那格 `comment`）。
const BUILTIN_ZH_HANS: &str = include_str!("../../../../assets/novel-rules.zh-Hans.json");

/// 一份规则表。字段就是 JSON 里的字段，一一对应，不另存第二份形状。
///
/// `meta` 有读者（`detect::book_info` 按那几串别名读头部那一块，所以它进内容哈希：别名
/// 改了，架上的书名就是旧名字）。`volumes` 此刻只有**数据**在场、没有读者 —— 分卷的第二层
/// 是后续那一刀。形状现在就定下来是为了别让 JSON 以后再改形状 ——
/// 那会让读者手里那份表忽然解不开。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rules {
    /// 表的**形状**版本：字段一改就 +1，旧表解不开时能说出是哪一版不匹配。
    /// 分章**内容**的变更不靠这一格，靠 [`CompiledRules::content_hash`]。
    pub version: u32,
    /// BCP 47 原样，与 `DocMeta::language` 同一把尺。
    pub locale: String,
    /// **必填**：省了它就等于没有数字表，那时所有号都读不出、递增校验静默失效 ——
    /// 一份「看起来能用、其实什么都判不了」的规则表比解不开更坏，所以宁可解不开。
    pub numbering: Numbering,
    /// 切成一章的那些标记。顺序即优先级：先碰到的赢。
    pub markers: Vec<Marker>,
    /// 认得出来但**不切**的那些（分卷），留给目录的第二层。
    #[serde(default)]
    pub volumes: Vec<Marker>,
    #[serde(default)]
    pub gates: Gates,
    #[serde(default)]
    pub meta: MetaKeys,
}

/// 一条规则怎么匹配一行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MatchKind {
    /// 剥掉首尾空白（含全角空格）后整行等于 [`Marker::text`]。
    #[default]
    Literal,
    /// [`Marker::pattern`] 那条正则。只有这一档编译。
    Regex,
}

/// 这一条的号怎么读（递增校验只对有号的标记生效）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NumberingStyle {
    /// 不带号（楔子、番外那一族），不参与递增校验。
    None,
    Chinese,
    Arabic,
    /// 两种都行 —— 真书上混用常见（同一本里「第十一回」与「第11回」并存）。
    #[default]
    Either,
}

/// 中文数字的两张表：**哪些字符算位数字、值多少，哪些是单位**。
///
/// 整张表住在数据里，代码一份都不留。理由是被撞出来的：上次给《西游记》补那个白圈
/// `○`（U+25CB，那个本子写它而不是 `〇` U+3007）时，要同时改数据的字符集**和**代码里
/// 的 `value()` 分支 —— 一份事实抄两处，而第二处漏改的症状是「章切出来了、号读不出」，
/// 比丢章更难查，因为屏上看着是对的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Numbering {
    /// 位数字（`一` = 1、`○` = 0、`两` = 2）。
    pub digits: Vec<(char, usize)>,
    /// 单位（`十` = 10、`万` = 10000）。繁简两套各是一份表，不是这一份的多字符。
    pub units: Vec<(char, usize)>,
    /// 认「没有单位的连续位数字」：明刻本省略式 `一一` = 11、白圈式 `一○` = 10。
    ///
    /// 不是趣味：实测《西游记》那 90 枚标记按标准写法解析只落出 14 个不同的号，整本
    /// 连不起来，递增校验就会把它们大面积判死 —— 症状是「一本 100 回的书一章没分开」。
    #[serde(default = "default_true")]
    pub colophon_style: bool,
}

fn default_true() -> bool {
    true
}

/// 一条规则。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// 给人看的名字：warn 与诊断里指名道姓用它，不用下标。
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: MatchKind,
    /// `kind = regex` 时的模式；捕获组一（若有）就是那个号。
    #[serde(default)]
    pub pattern: Option<String>,
    /// `kind = literal` 时的整行内容。
    #[serde(default)]
    pub text: Option<String>,
    /// 将来做目录第二层时用（1 = 章）。
    #[serde(default = "default_level")]
    pub level: u8,
    #[serde(default)]
    pub numbering: NumberingStyle,
    /// 这条规则自己的正例与反例。`compile()` 加载时当场拿去自测。
    #[serde(default)]
    pub examples: Examples,
}

/// 一条规则的自检样本。
///
/// 为什么样本住在数据里而不是测试里：这次「编号」那族吃掉红楼一整本（正文一句
/// `五六．又值人來回，…` 被读成第 56 章，于是第十八到五十六回全被递增校验判成回退）——
/// 那种事故**属于这条规则自己的性质**，写在 Rust 测试里就只有改 Rust 的人会想起来跑它，
/// 而读者换一份规则文件时没有任何东西提醒他这一条会吃正文。写进数据，加载就红。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Examples {
    /// 必须认下来的行。撞不上 = 这条规则做不成它声称做的事。
    #[serde(default)]
    pub must_match: Vec<String>,
    /// 必须**不**认下来的行。撞上了 = 这条规则太松，会吃正文。
    #[serde(default)]
    pub must_not_match: Vec<String>,
}

fn default_level() -> u8 {
    1
}

/// 四条否决门的阈值。**它们在真书上量过**（出处见 `assets/novel-rules.zh-Hans.json`
/// 里 `gates.comment`），而且换语言本该换数：英文「Chapter 3」出现在正文句子里的概率与
/// 中文「第三章」不是一个量级。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gates {
    /// 认出的标记少于这么多就当**没分章**（回落整篇一章）。
    #[serde(default = "default_min_chapters")]
    pub min_chapters: usize,
    /// 标题尾巴的上限：标记之后那一段超过它就**截断**（多出来的字留在正文开头），
    /// 因为整行当标题会把转档粘连的整段正文吃进章名 —— 真见过 618 字的一行。
    #[serde(default = "default_title_max")]
    pub title_max_chars: usize,
    /// 相邻两章之间的中位正文字数低于它就整条规则判回去（那是目录页或引用堆）。
    /// 真书量出来的中位数在 2600–7400 字，这里取 200 = 三倍余量。
    #[serde(default = "default_median")]
    pub min_median_chapter_chars: usize,
    /// 文件头往后连续这么多枚标记、且每两枚之间正文不超过 [`Self::toc_max_gap_chars`]，
    /// 那一块就是**目录页**：整块跳过，不当章。
    #[serde(default = "default_toc_run")]
    pub toc_min_run: usize,
    #[serde(default = "default_toc_gap")]
    pub toc_max_gap_chars: usize,
}

fn default_min_chapters() -> usize {
    3
}
fn default_title_max() -> usize {
    40
}
fn default_median() -> usize {
    200
}
fn default_toc_run() -> usize {
    5
}
fn default_toc_gap() -> usize {
    40
}

impl Default for Gates {
    fn default() -> Self {
        Self {
            min_chapters: default_min_chapters(),
            title_max_chars: default_title_max(),
            min_median_chapter_chars: default_median(),
            toc_min_run: default_toc_run(),
            toc_max_gap_chars: default_toc_gap(),
        }
    }
}

/// 书目的字段别名表与文件名去噪词。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MetaKeys {
    #[serde(default)]
    pub title_keys: Vec<String>,
    #[serde(default)]
    pub author_keys: Vec<String>,
    #[serde(default)]
    pub description_keys: Vec<String>,
    #[serde(default)]
    pub completion_marks: Vec<String>,
    #[serde(default)]
    pub filename_noise: Vec<String>,
    /// 只读头部这么多行找书目（见 `book_info` 那条「只看头部」的理由）。
    #[serde(default = "default_head_lines")]
    pub head_lines: usize,
    /// 键后面的值最长收多少字：超过就说明那一行不是字段，而是一句正文。
    #[serde(default = "default_value_max")]
    pub value_max_chars: usize,
    /// 作者那一行的分隔字符（「甲、乙」是两个人），**一个字符串而不是数组**：
    /// 它是要喂给 `str::split` 的字符集，写成数组反而多一次转换。
    #[serde(default = "default_author_separators")]
    pub author_separators: String,
    /// 按分隔切完之后要扔掉的碎片（`合著`、单独一个 `著`）。
    #[serde(default)]
    pub author_noise: Vec<String>,
    /// 书名可能裹在哪对括号里（中文《》与【】；英文那套规则交空表）。
    #[serde(default)]
    pub title_brackets: Vec<TitleBracket>,
}

/// 一对括号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitleBracket {
    open: char,
    close: char,
}

impl TitleBracket {
    pub(crate) fn pair(&self) -> (char, char) {
        (self.open, self.close)
    }
}

fn default_head_lines() -> usize {
    40
}
fn default_value_max() -> usize {
    60
}
fn default_author_separators() -> String {
    "、，,;；　 ".to_string()
}

/// 一条规则编译后的样子。`Rejected` 存在这里而不是被删掉，是为了让 `markers()` 的下标
/// 与 `compiled` 一一对应 —— 两条平行数组靠约定等长，是这一层最不该再犯的形状。
#[derive(Debug)]
pub(crate) enum Compiled {
    Literal,
    Regex(regex::Regex),
    /// 声明了正则却没有模式，或模式解不开。只作废这一条。
    Rejected,
}

/// 一份**编译好**的规则表：正则已解、坏条目已剔、内容哈希已算。
///
/// `Arc` 包住是因为它随配置的换页一起换（`infra::config` 那层的做法），逐行扫描时不再
/// 解字符串、也不再判「这条 pattern 能不能用」。
#[derive(Debug)]
pub struct CompiledRules {
    rules: Rules,
    /// 与 `rules.markers` **同长同序**。
    compiled: Vec<Compiled>,
    /// 生效那套表**内容**的 SHA-256 前 12 位。它进失效判据的理由见模块头，
    /// 它为什么不算原始字节见 [`content_hash`]。
    content_hash: String,
    /// 加载时剔掉的那几条的名字，供诊断与界面说话。空 = 整份都好用。
    bad_entries: Vec<String>,
}

impl CompiledRules {
    /// 出厂那份（简体中文）。进程内一份；用 `OnceLock` 而不是再引一个惰性原语。
    pub(crate) fn builtin() -> Arc<Self> {
        static BUILTIN: OnceLock<Arc<CompiledRules>> = OnceLock::new();
        BUILTIN.get_or_init(|| compile(BUILTIN_ZH_HANS).0).clone()
    }

    pub(crate) fn content_hash(&self) -> &str {
        &self.content_hash
    }

    pub(crate) fn bad_entries(&self) -> &[String] {
        &self.bad_entries
    }

    pub(crate) fn locale(&self) -> &str {
        &self.rules.locale
    }

    pub(crate) fn gates(&self) -> &Gates {
        &self.rules.gates
    }

    /// 书目那几个字段的别名表与去噪词（`book_info` 用它读头部那一块）。
    pub(crate) fn meta(&self) -> &MetaKeys {
        &self.rules.meta
    }

    /// 某个字符是不是位数字、值多少。表在数据里（见 `Numbering` 的理由）。
    pub(crate) fn digit(&self, c: char) -> Option<usize> {
        self.rules
            .numbering
            .digits
            .iter()
            .find(|(ch, _)| *ch == c)
            .map(|(_, v)| *v)
    }

    /// 某个字符是不是单位（十百千万）、值多少。
    pub(crate) fn unit(&self, c: char) -> Option<usize> {
        self.rules
            .numbering
            .units
            .iter()
            .find(|(ch, _)| *ch == c)
            .map(|(_, v)| *v)
    }

    pub(crate) fn colophon_style(&self) -> bool {
        self.rules.numbering.colophon_style
    }

    /// 章标记与其编译结果，同序成对给出（下标配不上是编程错误，不是运行期的事）。
    /// 章标记与其编译结果，同序成对（判一行用 [`marker_matches`]，两条路共用一份）。
    pub(crate) fn markers(&self) -> impl Iterator<Item = (&Marker, &Compiled)> {
        self.rules.markers.iter().zip(&self.compiled)
    }
}

/// 解一份 JSON、校验、编译正则、算内容哈希。
///
/// 返回 `(那份表, 给读者看的警告)`。**整份解不开**才退回出厂那份；**单条坏**只剔那一条。
pub(crate) fn compile(json: &str) -> (Arc<CompiledRules>, Vec<String>) {
    let fallback = |reason: String| -> (Arc<CompiledRules>, Vec<String>) {
        tracing::warn!("{reason}");
        if std::ptr::eq(
            json.as_bytes().as_ptr(),
            BUILTIN_ZH_HANS.as_bytes().as_ptr(),
        ) {
            // 出厂那份自己也有问题时不能再递归退回自己（会无限循环），直接 panic 在前头：
            // 那种情况是仓库里的数据坏了，不该装作「读者写坏了」。
            panic!("出厂那份小说规则解不开，这是仓库内数据的问题：{reason}");
        }
        let (builtin, mut warnings) = compile(BUILTIN_ZH_HANS);
        warnings.insert(0, reason);
        (builtin, warnings)
    };
    let rules: Rules = match serde_json::from_str(json) {
        Ok(rules) => rules,
        Err(err) => return fallback(format!("小说规则文件解不开，已按出厂那份分章：{err}")),
    };
    if rules.markers.is_empty() {
        // 一条标记都没有的表等于「这书不切」，那是配置写坏了，不是「这本书真的没章」。
        return fallback(
            "小说规则里 markers 是空的，已按出厂那份分章（要关掉分章请别交回一份空表）".to_string(),
        );
    }
    let mut compiled = rules.markers.iter().map(compile_marker).collect::<Vec<_>>();
    // 自检：拿每条规则自己的正例与反例当场面具。判「认不认」用的是
    // [`CompiledRules::marker_matches`] —— 与真扫描同一个函数，不是它的简化版。
    // 先记下要作废哪几条，再统一改：边遍历边按下标改 `compiled` 既过不了借用检查，
    // 也容易留下「循环变量只用来当下标」那种读不懂的形状。
    let mut rejected: Vec<(usize, String)> = Vec::new();
    for (ix, (marker, slot)) in rules.markers.iter().zip(compiled.iter()).enumerate() {
        if matches!(slot, Compiled::Rejected) {
            continue;
        }
        let mut dropped = None;
        for line in &marker.examples.must_match {
            if !marker_matches(marker, slot, line) {
                dropped = Some(format!(
                    "小说规则里那条「{}」连自己的正例都不认，已作废：{line:?}",
                    marker_name(marker)
                ));
                break;
            }
        }
        if dropped.is_none() {
            for line in &marker.examples.must_not_match {
                if marker_matches(marker, slot, line) {
                    dropped = Some(format!(
                        "小说规则里那条「{}」会吃掉它自己的反例（那通常是正文），已作废：{line:?}",
                        marker_name(marker)
                    ));
                    break;
                }
            }
        }
        if let Some(warn) = dropped {
            rejected.push((ix, warn));
        }
    }
    let warnings: Vec<String> = rejected.iter().map(|(_, w)| w.clone()).collect();
    for (ix, _) in rejected {
        compiled[ix] = Compiled::Rejected;
    }
    let bad_entries = rules
        .markers
        .iter()
        .zip(&compiled)
        .filter(|(_, c)| matches!(c, Compiled::Rejected))
        .map(|(m, _)| marker_name(m))
        .collect::<Vec<_>>();
    for warn in &warnings {
        tracing::warn!("{warn}");
    }
    if bad_entries.is_empty() {
        tracing::debug!("小说规则自检通过：{} 条", compiled.len());
    }
    (
        Arc::new(CompiledRules {
            content_hash: content_hash(&rules, &compiled),
            rules,
            compiled,
            bad_entries,
        }),
        warnings,
    )
}

/// 「这套表切出来的章会不会变」这一问题的**唯一**答案。
///
/// 喂给哈希的只有下面这些：活着的章标记与分卷标记的形状、读号那两张表、五条阈值、
/// 书目那几串别名。表里其余的格一概不进 —— `name` 与 JSON 里那格 `comment` 是给人看的，
/// `version` 说的是形状，`locale` 只是个标签，`examples` 在 [`compile`] 里当完场面具
/// 之后就不再生成分章结果。按**原始字节**算的话，读者改一个错别字就会让整库为它重扫
/// 一遍，那是把「改了个注释」的成本收成「换了一套规则」。
///
/// 被剔掉的条目不进清单，所以这条链自己也是活的：有人删掉一条反例让那条规则复活，
/// 清单里就多一条，哈希跟着动 —— 而那正是该动的时刻，多一条规则就是多切一些章。
///
/// 编码成一行一个 JSON 值：`to_string` 出来的标记里没有裸换行（字符串里的换行必被转义
/// 成 `\n` 两个字符），所以换行是安全的分隔符，拼不出「两组不同的表得到同一串」那种歧义。
fn content_hash(rules: &Rules, compiled: &[Compiled]) -> String {
    let mut buf = String::new();
    put(&mut buf, "markers");
    for (marker, slot) in rules.markers.iter().zip(compiled) {
        if !matches!(slot, Compiled::Rejected) {
            put_marker(&mut buf, marker);
        }
    }
    put(&mut buf, "volumes");
    for marker in &rules.volumes {
        put_marker(&mut buf, marker);
    }
    put(&mut buf, "numbering");
    put(&mut buf, &rules.numbering);
    put(&mut buf, "gates");
    put(&mut buf, &rules.gates);
    // 书目那几串也落库（`books.title` / `authors` / `description`），所以改了别名表
    // 同样该重扫 —— 同一趟重扫顺手把架上那一格改成新名字，而不是让它停在旧结果上。
    put(&mut buf, "meta");
    put(&mut buf, &rules.meta);
    short_hash(buf.as_bytes())
}

/// 一条规则里**参与匹配**的那几格。`name` 与 `examples` 不在其中，理由见 [`content_hash`]。
fn put_marker(out: &mut String, marker: &Marker) {
    put(out, &marker.kind);
    put(out, &marker.pattern);
    put(out, &marker.text);
    put(out, &marker.level);
    put(out, &marker.numbering);
}

fn put(out: &mut String, value: &(impl Serialize + ?Sized)) {
    // 这几个类型（字符串、整数、布尔、枚举、它们的 `Vec`）序列化不会失败：会失败的
    // 只有 `f64` 那种非有限浮点，而这份表里一个都没有。
    let _ = writeln!(out, "{}", serde_json::to_string(value).unwrap_or_default());
}

/// **一条规则认不认这一行 —— 全仓库只有这一份定义。**
///
/// 真扫描（`detect::find_hits`）与加载时的自检（[`compile`]）都调它。分成两份的后果很
/// 具体：自检全绿、真书上却漏章，因为两份「什么算命中」会在某一种行上不一致。行首尾的
/// 空白与全角空白在这里统一剥掉；`literal` 那族对的是**整行**，所以允许尾随一个冒号类
/// 分隔（「楔子」与「楔子：」都算）。
pub(crate) fn marker_matches(marker: &Marker, slot: &Compiled, line: &str) -> bool {
    let line = line.trim_matches(|c: char| c.is_whitespace());
    if line.is_empty() {
        return false;
    }
    match slot {
        Compiled::Literal => {
            let head = line.split(['：', ':', '　', ' ']).next().unwrap_or(line);
            marker.text.as_deref() == Some(head)
        }
        Compiled::Regex(re) => re.is_match(line),
        Compiled::Rejected => false,
    }
}

fn compile_marker(marker: &Marker) -> Compiled {
    match marker.kind {
        MatchKind::Literal => Compiled::Literal,
        MatchKind::Regex => match marker.pattern.as_deref() {
            None => {
                tracing::warn!(
                    "小说规则里那条「{}」声明是正则却没有 pattern，作废这一条",
                    marker_name(marker)
                );
                Compiled::Rejected
            }
            Some(pattern) => match regex::Regex::new(pattern) {
                Ok(re) => Compiled::Regex(re),
                Err(err) => {
                    tracing::warn!(
                        "小说规则里那条「{}」的模式解不开，作废这一条：{err}（pattern={pattern}）",
                        marker_name(marker)
                    );
                    Compiled::Rejected
                }
            },
        },
    }
}

fn marker_name(marker: &Marker) -> String {
    if !marker.name.is_empty() {
        return marker.name.clone();
    }
    match (
        marker.kind,
        marker.pattern.as_deref(),
        marker.text.as_deref(),
    ) {
        (MatchKind::Regex, Some(pattern), _) => pattern.to_string(),
        (_, _, Some(text)) => text.to_string(),
        _ => "(无名条目)".to_string(),
    }
}

/// 内容哈希的前 12 个十六进制字符。用 `sha2` 而不是另起一个校验：它就是书的身份那一格
/// （`ingest::identity`）已经在用的那一个。
fn short_hash(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(bytes);
    let mut out = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        // 一位都不会丢：一字节写两位十六进制，取前 6 字节 = 12 个字符。
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Compiled, CompiledRules, compile, marker_name};

    #[test]
    fn the_builtin_rules_compile_every_entry() {
        let rules = CompiledRules::builtin();
        assert!(
            rules.bad_entries().is_empty(),
            "出厂那份规则里不该有解不开的一条：{:?}",
            rules.bad_entries()
        );
        // 出厂那份的标记条数与哈希是**这份数据的样子**，改数据就要同时改这里，
        // 让「有人动了出厂表」这件事在 CI 上出声，而不是静默换掉所有人的分章结果。
        assert_eq!(rules.markers().count(), 15);
        assert_eq!(rules.content_hash().len(), 12);
        // 自检要有牙：一条没有正例的规则等于一条不来自检的规则，那这整层就是空转的。
        let toothless = rules
            .markers()
            .filter(|(m, _)| {
                m.examples.must_match.is_empty() || m.examples.must_not_match.is_empty()
            })
            .map(|(m, _)| marker_name(m))
            .collect::<Vec<_>>();
        assert!(
            toothless.is_empty(),
            "出厂表里这些条目没带正反例，自检对它们形同虚设：{toothless:?}"
        );
    }

    /// 一条会吃正文的规则，**加载时就该被自己的反例踢掉**，而不是等到真书上少三十七章才发现。
    /// 这条也是「出厂表那条 `\s*\S{0,20}$` 上限不许被拿掉」的证人：把它拿掉，
    /// `the_builtin_rules_compile_every_entry` 就红。
    #[test]
    fn a_rule_that_eats_a_prose_line_is_rejected_at_load_time() {
        let json = r#"{
            "version": 1, "locale": "zh-Hans",
            "numbering": {"digits": [["一",1],["五",5],["六",6]], "units": [["十",10]]},
            "markers": [
                {"name": "编号（不限长）", "kind": "regex",
                 "pattern": "^\\s*([0-9一二三四五六七八九十]{1,4})\\s*[、.．]",
                 "examples": {"must_match": ["一、初见"],
                              "must_not_match": ["五六．又值人來回，有雨村處遣人回話．"]}},
                {"name": "章", "kind": "regex",
                 "pattern": "^\\s*第\\s*([0-9一二三]{1,4})\\s*章",
                 "examples": {"must_match": ["第三章 起"], "must_not_match": ["他说第三章写得太慢"]}}
            ]
        }"#;
        let (rules, warnings) = compile(json);
        assert_eq!(
            rules.bad_entries(),
            vec!["编号（不限长）".to_string()],
            "只该踢掉那一条，别的一律照用"
        );
        assert_eq!(
            rules.markers().count(),
            2,
            "作废是逐条降级，条目还在原位（下标不能漂）"
        );
        assert!(
            warnings.iter().any(|w| w.contains("反例")),
            "要说出为什么作废：{warnings:?}"
        );
    }

    /// 逐条降级：坏模式只作废那一条，好的那条照样能用。
    /// 把 `compile_marker` 改成「有一条坏就整份作废」，这一条就红。
    #[test]
    fn a_bad_pattern_rejects_only_its_own_entry() {
        let json = r#"{
            "version": 1, "locale": "zh-Hans",
            "numbering": {"digits": [["一",1],["二",2]], "units": [["十",10]]},
            "markers": [
                {"name": "好的一条", "kind": "regex", "pattern": "^\\s*第\\s*([0-9一二三]{1,4})\\s*章"},
                {"name": "坏的一条", "kind": "regex", "pattern": "^\\s*(第[章+"},
                {"name": "没写模式的", "kind": "regex"}
            ]
        }"#;
        let (rules, warnings) = compile(json);
        assert!(
            warnings.is_empty(),
            "单条坏不该整份退回出厂，所以不该有退回那一句：{warnings:?}"
        );
        assert_eq!(rules.bad_entries().len(), 2, "该剔两条：坏模式 + 缺模式");
        let states = rules
            .markers()
            .map(|(_, c)| matches!(c, Compiled::Regex(_)))
            .collect::<Vec<_>>();
        assert_eq!(states, vec![true, false, false], "第一条必须还活着");
    }

    /// 整份解不开 → 退回出厂那份，并留下一句读者看得见的话。这条守的是
    /// 「规则文件坏了的下场是分章退回，不是一本书打不开」。
    #[test]
    fn an_unreadable_file_falls_back_to_the_builtin_one() {
        let (rules, warnings) = compile("{ 这不是 JSON ");
        assert_eq!(rules.markers().count(), 15, "该是出厂那份那 15 条");
        // 第一句必须是「为什么退回」；后面可能还有出厂表自己的逐条降级声。
        assert!(!warnings.is_empty(), "该留下一句话");
        assert!(
            warnings[0].contains("出厂"),
            "第一句要说清退回的原因：{warnings:?}"
        );
    }

    /// 空表不等于「这本书没章」，而是配置写坏了 —— 它同样退回出厂。
    #[test]
    fn an_empty_marker_list_is_not_taken_as_a_request_to_cut_nothing() {
        // 带上 `numbering`：否则这份样本会先因为「少了必填的一格」而解不开，那条断言
        // 就成了在验另一件事（红的是对的门、报的是错的因）。
        let json = r#"{"version":1,"locale":"zh-Hans","numbering":{"digits":[["一",1]],"units":[["十",10]]},"markers":[]}"#;
        let (rules, warnings) = compile(json);
        assert_eq!(rules.markers().count(), 15);
        assert!(
            warnings.iter().any(|w| w.contains("markers")),
            "要说出「空表被拒」这件事，而不是含糊一句解不开：{warnings:?}"
        );
    }

    /// 内容哈希必须**跟着内容动**：它是「规则换了没有」的唯一判据，两个不同的表撞出
    /// 同一个哈希就会让老库的进度静默指到隔壁那一章。
    #[test]
    fn the_content_hash_follows_the_content() {
        let base = CompiledRules::builtin();
        let a = base.content_hash().to_string();
        let changed = String::from(
            super::BUILTIN_ZH_HANS
                .replace("\"title_max_chars\": 40", "\"title_max_chars\": 41")
                .as_str(),
        );
        let (rules, warnings) = compile(&changed);
        assert!(
            warnings.is_empty(),
            "只改了一个数，不该退回出厂：{warnings:?}"
        );
        assert_ne!(rules.content_hash(), a.as_str(), "阈值动了，哈希必须跟着动");
    }

    /// 书目那几串别名也进清单：`book_info` 用的是同一张表，别名改了，架上的
    /// `books.title` 那一格就是旧名字 —— 同一趟重扫顺手改过来，而不是让它停在旧结果上。
    #[test]
    fn a_bibliography_alias_edit_also_moves_the_hash() {
        let base = CompiledRules::builtin();
        let a = base.content_hash().to_string();
        let changed = super::BUILTIN_ZH_HANS.replace("\"head_lines\": 40", "\"head_lines\": 60");
        let (rules, warnings) = compile(&changed);
        assert!(warnings.is_empty(), "只改了一格读数：{warnings:?}");
        assert_ne!(
            rules.content_hash(),
            a.as_str(),
            "只读头部 40 行与 60 行读出来的书名可能不同，哈希该动"
        );
    }

    /// 哈希答的是「切出来的章会不会变」，**不是**「文件字节变没变」。
    /// 只给人看的那些格 —— JSON 里那格 `comment`（它压根不是 `Rules` 的字段）、条目的
    /// `name`、`locale`、以及自检用完就丢的 `examples` —— 改了都不该让整库重扫一遍；
    /// 连字节布局（整份压成一行）也一样，因为按原始字节算的话，排个版就是一次「换规则」。
    ///
    /// 这条门是给 `store::stale_reason` 的：那边按哈希判重扫，读者改一个错别字就要
    /// 把每一本 `.txt` 重读一遍 —— 那是把「改了个注释」的代价收成「换了一套规则」。
    #[test]
    fn the_human_only_cells_do_not_move_the_hash() {
        let want = CompiledRules::builtin().content_hash().to_string();
        let mut value: serde_json::Value =
            serde_json::from_str(super::BUILTIN_ZH_HANS).expect("出厂那份该解得开");
        value["comment"] = serde_json::json!("改过说明的出厂规则");
        value["locale"] = serde_json::json!("zh-Hant");
        {
            let first = &mut value["markers"][0];
            first["name"] = serde_json::json!("给这一条另起的名字");
            first["comment"] = serde_json::json!("改过的条目说明");
            // 加一条**本来就认得**的正例：自检照过，清单不多不少。
            first["examples"]["must_match"]
                .as_array_mut()
                .expect("出厂那份的正例是数组")
                .push(serde_json::json!("第3章 起风了"));
        }
        // `to_string` 交出的是一份没有缩进的字节，与出厂文件不是一样东西。
        let (rules, warnings) = compile(&value.to_string());
        assert!(
            warnings.is_empty(),
            "只动了给人看的几格，不该有任何一条被作废：{warnings:?}"
        );
        assert_eq!(
            rules.content_hash(),
            want.as_str(),
            "同一套规则换了排版与注释，哈希不许动 —— 动一次就是整库白重扫"
        );
    }

    /// 被作废的规则**不进清单**，所以「有人删掉那条反例、这条规则活了」一定动着哈希。
    /// 这一档要是安静地不变，症状就是老库的序号整体漂：多一条规则等于多切一些章。
    #[test]
    fn reviving_a_rule_rejected_by_its_own_example_moves_the_hash() {
        fn table(anti: &str) -> String {
            format!(
                r#"{{
                  "version": 1, "locale": "zh-Hans",
                  "numbering": {{"digits": [["一",1],["二",2],["三",3]], "units": [["十",10]]}},
                  "markers": [
                    {{"name": "章", "kind": "regex", "pattern": "^\\s*第([一二三])章",
                      "examples": {{"must_match": ["第一章"], "must_not_match": ["{anti}"]}}}}
                  ]
                }}"#
            )
        }
        // 反例「第三章」自己撞得上那条模式 → 整条作废，清单里没有它。
        let (dead, warnings) = compile(&table("第三章"));
        assert_eq!(
            dead.bad_entries(),
            vec!["章".to_string()],
            "反例撞成正例，该只作废这一条：{warnings:?}"
        );
        // 换成一句正文 → 这条活了，会多切章。
        let (alive, warnings) = compile(&table("他说第三章写得太慢"));
        assert!(warnings.is_empty(), "这一份该整条能用：{warnings:?}");
        assert!(alive.bad_entries().is_empty());
        assert_ne!(
            dead.content_hash(),
            alive.content_hash(),
            "一条规则从作废变成生效，哈希必须动"
        );
    }
}
