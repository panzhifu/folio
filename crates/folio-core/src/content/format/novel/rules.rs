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

use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

/// 出厂那份简体中文规则（阈值在真书上量过，出处写在文件里那格 `comment`）。
const BUILTIN_ZH_HANS: &str = include_str!("../../../../assets/novel-rules.zh-Hans.json");

/// 一份规则表。字段就是 JSON 里的字段，一一对应，不另存第二份形状。
///
/// `meta` 与 `volumes` 那两格今天只有**数据**在场、没有读者：书名与作者的提取（那两串
/// 别名表）与分卷的第二层是下一刀，它们各自的读取器到时和这俩字段一起用。形状现在就定
/// 下来是为了别让 JSON 以后再改形状 —— 那会让读者手里那份表忽然解不开。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rules {
    /// 表的**形状**版本：字段一改就 +1，旧表解不开时能说出是哪一版不匹配。
    /// 分章**内容**的变更不靠这一格，靠 [`CompiledRules::content_hash`]。
    pub version: u32,
    /// BCP 47 原样，与 `DocMeta::language` 同一把尺。
    pub locale: String,
    #[serde(default)]
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

/// 中文数字那两张表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Numbering {
    /// 参与解析的字符集。**跟着语言走，不是 Unicode 属性**，所以写在数据里。
    #[serde(default = "default_digits")]
    pub digits: String,
    /// 认明刻本式的省略写法：`一一` = 11、`二一` = 21、`三五` = 35。
    ///
    /// 这一条不是趣味：实测《西游记》（PG 本）那 90 枚标记按标准写法解析只落出 14 个
    /// 不同的号，整本的序号连不起来，递增校验就会把它们大面积判死 —— 症状是「一本
    /// 100 回的书一章都没分开」。
    #[serde(default = "default_true")]
    pub colophon_style: bool,
}

impl Default for Numbering {
    fn default() -> Self {
        Self {
            digits: default_digits(),
            colophon_style: true,
        }
    }
}

fn default_digits() -> String {
    "〇○零一二三四五六七八九十百千万两".to_string()
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
    /// 生效那套表的字节 SHA-256 前 12 位。它进失效判据的理由见模块头。
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

    pub(crate) fn digits(&self) -> &str {
        &self.rules.numbering.digits
    }

    pub(crate) fn colophon_style(&self) -> bool {
        self.rules.numbering.colophon_style
    }

    /// 章标记与其编译结果，同序成对给出（下标配不上是编程错误，不是运行期的事）。
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
    let compiled = rules.markers.iter().map(compile_marker).collect::<Vec<_>>();
    let bad_entries = rules
        .markers
        .iter()
        .zip(&compiled)
        .filter(|(_, c)| matches!(c, Compiled::Rejected))
        .map(|(m, _)| marker_name(m))
        .collect::<Vec<_>>();
    for name in &bad_entries {
        tracing::warn!("小说规则里那条「{name}」不可用，已作废；其余条目照旧生效");
    }
    (
        Arc::new(CompiledRules {
            content_hash: short_hash(json.as_bytes()),
            rules,
            compiled,
            bad_entries,
        }),
        Vec::new(),
    )
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
        use std::fmt::Write as _;
        // 一位都不会丢：一字节写两位十六进制，取前 6 字节 = 12 个字符。
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Compiled, CompiledRules, compile};

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
    }

    /// 逐条降级：坏模式只作废那一条，好的那条照样能用。
    /// 把 `compile_marker` 改成「有一条坏就整份作废」，这一条就红。
    #[test]
    fn a_bad_pattern_rejects_only_its_own_entry() {
        let json = r#"{
            "version": 1, "locale": "zh-Hans",
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
        assert_eq!(warnings.len(), 1, "该留下一句话：{warnings:?}");
        assert!(warnings[0].contains("出厂"));
    }

    /// 空表不等于「这本书没章」，而是配置写坏了 —— 它同样退回出厂。
    #[test]
    fn an_empty_marker_list_is_not_taken_as_a_request_to_cut_nothing() {
        let (rules, warnings) = compile(r#"{"version":1,"locale":"zh-Hans","markers":[]}"#);
        assert_eq!(rules.markers().count(), 15);
        assert!(!warnings.is_empty(), "要说出「空表被拒」这件事");
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
}
