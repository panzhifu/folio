//! 纯文本小说分章规则的**读者差量**（`.txt` 分章那套数据词汇）。
//!
//! 出厂规则表与合成/编译引擎住在 folio-format 的 `novel::rules`；这里只放**跨 crate
//! 共用的数据形状**：core 的配置存这一层差量（`novel.edits`，`#[serde(flatten)]` 进
//! 配置文件），app 的设置页按它编辑，format 的引擎拿它合成生效表 —— 三个消费方，
//! 按 folio-model 的规矩进来。**serde 表示是契约**：换 crate 不换形状，配置文件里的
//! `novel` 一节逐字节不动。
//!
//! 存差量而不是整份表，三条理由：以后出厂表加了新条目，读者那份不会把表冻在他改动
//! 那一刻（整份存就得有人手动合并）；「恢复出厂」= 清空这一格，不用另存一份出厂的
//! 副本；屏上每一条也都答得出「这条是出厂的还是我加的、我关过哪条」。

use serde::{Deserialize, Serialize};

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
    /// 这条规则自己的正例与反例。format 加载时当场拿去自测。
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

/// 读者那一层改动：叠在出厂表之上的**差量**，不是整份表。
///
/// 关条目按**名字**而不是下标：出厂表重排或插一条，不会因此关掉读者本意之外的另一条。
/// 自定义条目追加在表尾 —— `markers` 的顺序就是优先级，出厂那几条先碰到的赢。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edits {
    /// 关掉的那些条目的 `name`（出厂的与自定义的一律适用）。
    #[serde(default)]
    pub off: Vec<String>,
    /// 读者自己加的那些条。
    #[serde(default)]
    pub extra: Vec<Marker>,
    /// 改了的那几格阈值（`None` = 用出厂那个数）。
    #[serde(default)]
    pub gates: GateEdits,
}

/// 阈值那五格的差量。逐格 `Option` 而不是整份出厂阈值：整份存的话，出厂表以后调了某个
/// 读者压根没碰过的数，他那份会按老数把它覆盖回去。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateEdits {
    pub min_chapters: Option<usize>,
    pub title_max_chars: Option<usize>,
    pub min_median_chapter_chars: Option<usize>,
    pub toc_min_run: Option<usize>,
    pub toc_max_gap_chars: Option<usize>,
}

impl Edits {
    /// 这一层等于没改过吗。空的时候生效那份直接是出厂那份，连合成都不用走。
    pub fn is_empty(&self) -> bool {
        self.off.is_empty() && self.extra.is_empty() && self.gates == GateEdits::default()
    }

    /// 名字是不是这一层占着的（关过的、或自己加的）。界面上「另起一个名字」要避免撞它，
    /// 因为 `off` 按名字作用 —— 撞了就会有一条关不掉的条目。
    pub fn holds(&self, name: &str) -> bool {
        self.off.iter().any(|off| off == name) || self.extra.iter().any(|m| m.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::{Edits, GateEdits, Marker, MatchKind, NumberingStyle};

    /// serde 表示是契约（model 的第三条规矩）：core 的配置把这一层 `#[serde(flatten)]`
    /// 进 `novel` 一节，这里的键名与省略行为一变，读者手里的配置文件就解不开。
    /// 这条对拍锁的是 flatten 之后的**实际形状**：缺省格省略、自定义条目整段展开。
    #[test]
    fn the_flattened_config_shape_round_trips() {
        let edits = Edits {
            off: vec!["番外".into()],
            extra: vec![Marker {
                name: "卷".into(),
                kind: MatchKind::Regex,
                pattern: Some(r"^第[0-9]+卷".into()),
                text: None,
                level: 1,
                numbering: NumberingStyle::Arabic,
                examples: Default::default(),
            }],
            gates: GateEdits {
                min_chapters: Some(5),
                ..Default::default()
            },
        };
        let json = serde_json::json!({
            "off": ["番外"],
            "extra": [{
                "name": "卷",
                "kind": "regex",
                "pattern": "^第[0-9]+卷",
                "level": 1,
                "numbering": "arabic",
            }],
            "gates": { "min_chapters": 5 },
        });
        let parsed: Edits = serde_json::from_value(json).expect("flatten 后的配置形状要解得开");
        assert_eq!(parsed, edits, "逐字段一致，缺省格靠 serde(default) 补齐");
        let back = serde_json::to_value(&edits).unwrap();
        assert_eq!(
            back["extra"][0]["kind"], "regex",
            "小写键名是配置文件里看得见的那一格"
        );
        assert!(
            back["extra"][0].get("text").is_none(),
            "None 不写进 JSON（default 空档在文件里不占格子）"
        );
    }

    /// 空差量答得出「没改过」；`holds` 认关过的与自定义的两族名字 ——
    /// 界面上「另起一个名字」靠它避免撞出一条关不掉的条目。
    #[test]
    fn emptiness_and_name_ownership() {
        let mut edits = Edits::default();
        assert!(edits.is_empty());
        edits.off.push("楔子".into());
        edits.extra.push(Marker {
            name: "卷一".into(),
            kind: MatchKind::Literal,
            pattern: None,
            text: None,
            level: 1,
            numbering: NumberingStyle::Either,
            examples: Default::default(),
        });
        assert!(!edits.is_empty());
        assert!(edits.holds("楔子"));
        assert!(edits.holds("卷一"));
        assert!(!edits.holds("第三章"));
    }
}
