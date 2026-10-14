//! 选择器子集：解析、匹配、specificity。
//!
//! 不接 `selectors` crate 是有意为之：它的 `SelectorImpl` 要 10 个关联类型外加
//! `NonTSPseudoClass` / `PseudoElement` 两套 trait，那是为 Servo 的全量
//! CSS Selectors 4 准备的；本引擎承诺的范围见 `css/computed.rs` 顶部的清单，
//! 背那套样板换不来用得上的能力。
//!
//! 支持：`*`、type、`.class`、`#id`、`[attr]`、`[attr=v]`、
//! 后代 / `>` 子代 / `+` 相邻兄弟 / `~` 一般兄弟、`,` 分组、
//! `:first-child`、`:last-child`、`:nth-child(An+B)`、`:lang()`。
//! 清单外的构造让**整条选择器作废**，而不是部分生效 ——
//! 部分生效会让作者样式以不可预期的方式命中内容。

use crate::content::dom::{Dom, NodeId};

/// 属性选择器的算子。CSS Selectors 4 的一整套：存在、全等、词包含（`~=`）、
/// 破折号前缀（`|=`，给 `lang` 一类「`en-US` 命中 `en`」的值用）、前缀 / 后缀 / 子串。
///
/// 出版书里的真写法几乎只有 `~=` 与 `*=`（`[class~="solid"]`、`[class*="biaoti"]`），
/// 但五条算子解析成本一样，缺哪条都是「选择器静默命中 0 个元素」那种查不出来的错。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrOp {
    /// `[attr]` —— 只要有这个属性（值不参与）。
    Exists,
    /// `[attr=v]`。
    Equals,
    /// `[attr~=v]` —— 属性值按 ASCII 空白切词，命中其一（`[class~=v]` 与 `.v` 同义）。
    Includes,
    /// `[attr|=v]` —— 全等，或以 `v-` 开头（`en` 命中 `en` 与 `en-US`）。
    DashMatch,
    /// `[attr^=v]`。
    Prefix,
    /// `[attr$=v]`。
    Suffix,
    /// `[attr*=v]`。
    Substring,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Simple {
    Universal,
    Type(String),
    Class(String),
    Id(String),
    Attr {
        name: String,
        op: AttrOp,
        /// `Exists` 时为 `None`；其余为已去引号的值。
        value: Option<String>,
        /// `[attr=v i]` 的大小写不敏感标志。`s`（显式敏感）折成 `false`。
        case_insensitive: bool,
    },
    FirstChild,
    LastChild,
    /// `:nth-child(An+B)`，`a` / `b` 已折成整数：`odd` = `(2,1)`、`even` = `(2,0)`、
    /// 裸整数 `3` = `(0,3)`、`-n+3` = `(-1,3)`。
    NthChild {
        a: i32,
        b: i32,
    },
    Lang(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PseudoElement {
    /// 首字下沉。清单里只有它：`::before` / `::after` 要的是「凭空造一个盒」，
    /// 而我们的盒全部来自 DOM 节点，锚点坐标系也建在那些节点的字节偏移上 ——
    /// 造出来的字符没有地方可锚。
    FirstLetter,
}

/// 一串紧挨着的简单选择器，如 `p.intro#first`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compound {
    pub simples: Vec<Simple>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    Descendant,
    Child,
    NextSibling,
    SubsequentSibling,
}

/// 复合选择器：`left` 里每一项带着它与自己右侧之间的组合器。
///
/// `pseudo` 是这条规则**作用在哪个盒**上的标记：`Some(FirstLetter)` 的声明不属于
/// 元素自己，而是排版层给首字符单独算的那一份。所以 `matches` 只认 `None`，
/// 伪元素的声明由 `matches_pseudo` 取走 —— 混在一起会让 `p::first-letter { color: red }`
/// 把整段正文都涂红。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Complex {
    pub left: Vec<(Combinator, Compound)>,
    pub right: Compound,
    pub pseudo: Option<PseudoElement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    pub complexes: Vec<Complex>,
}

/// (id, class/attr/伪类, type) 三段各占 8/8/8 位打包，可直接比大小。
fn pack(ids: u32, classes: u32, types: u32) -> u32 {
    (ids << 16) | (classes << 8) | types
}

impl Compound {
    fn specificity(&self) -> (u32, u32, u32) {
        let (mut ids, mut classes, mut types) = (0, 0, 0);
        for simple in &self.simples {
            match simple {
                Simple::Id(_) => ids += 1,
                Simple::Class(_)
                | Simple::Attr { .. }
                | Simple::FirstChild
                | Simple::LastChild
                | Simple::NthChild { .. }
                | Simple::Lang(_) => classes += 1,
                Simple::Type(_) => types += 1,
                Simple::Universal => {}
            }
        }
        (ids, classes, types)
    }

    fn matches(&self, dom: &Dom, node: NodeId) -> bool {
        // 只有元素能被选择器命中；文本与注释节点走父元素的样式继承。
        let Some(tag) = dom.get(node).tag.as_ref() else {
            return false;
        };
        self.simples.iter().all(|simple| match simple {
            Simple::Universal => true,
            Simple::Type(name) => {
                let local: &str = tag.local.as_ref();
                local.eq_ignore_ascii_case(name)
            }
            Simple::Class(c) => dom.classes(node).any(|k| k == c),
            Simple::Id(id) => dom.id_attr(node) == Some(id.as_str()),
            Simple::Attr {
                name,
                op,
                value,
                case_insensitive,
            } => match (dom.attr(node, name), value.as_deref()) {
                // `[attr]`：有属性即可。
                (Some(_), None) => true,
                (Some(actual), Some(want)) => attr_matches(*op, actual, want, *case_insensitive),
                (None, _) => false,
            },
            Simple::FirstChild => dom
                .get(node)
                .parent
                .is_some_and(|p| dom.element_children(p).next() == Some(node)),
            Simple::LastChild => dom
                .get(node)
                .parent
                .is_some_and(|p| dom.element_children(p).last() == Some(node)),
            // 数的是**元素**子节点里的 1 基位置：文本与注释节点不算一个孩子，
            // 这与 `:first-child` / `:last-child` 走 `element_children` 同一口径。
            Simple::NthChild { a, b } => {
                let Some(parent) = dom.get(node).parent else {
                    return false;
                };
                let index = dom
                    .element_children(parent)
                    .take_while(|c| *c != node)
                    .count() as i32
                    + 1;
                nth_matches(*a, *b, index)
            }
            Simple::Lang(prefix) => {
                nearest_lang(dom, node).is_some_and(|l| lang_matches(&l, prefix))
            }
        })
    }
}

impl Complex {
    fn specificity(&self) -> u32 {
        let (mut ids, mut classes, mut types) = self.right.specificity();
        for (_, compound) in &self.left {
            let (i, c, t) = compound.specificity();
            ids += i;
            classes += c;
            types += t;
        }
        // 伪元素按一个 type 计（CSS Selectors §17），`p::first-letter` 因此与 `p em` 同级。
        if self.pseudo.is_some() {
            types += 1;
        }
        pack(ids, classes, types)
    }

    fn matches(&self, dom: &Dom, node: NodeId) -> bool {
        self.right.matches(dom, node) && self.matches_left(dom, node, self.left.len())
    }

    fn matches_left(&self, dom: &Dom, node: NodeId, depth: usize) -> bool {
        if depth == 0 {
            return true;
        }
        let (combinator, compound) = &self.left[depth - 1];
        let candidates: Vec<NodeId> = match combinator {
            Combinator::Descendant => dom.ancestors(node).collect(),
            Combinator::Child => dom.get(node).parent.into_iter().collect(),
            Combinator::NextSibling => dom.prev_element_sibling(node).into_iter().collect(),
            Combinator::SubsequentSibling => dom.prev_element_siblings(node).collect(),
        };
        candidates
            .into_iter()
            .any(|c| compound.matches(dom, c) && self.matches_left(dom, c, depth - 1))
    }
}

impl Selector {
    /// 分组选择器按 CSS 规则取最高 specificity。
    pub fn specificity(&self) -> u32 {
        self.complexes
            .iter()
            .map(|c| c.specificity())
            .max()
            .unwrap_or(0)
    }

    pub fn matches(&self, dom: &Dom, node: NodeId) -> bool {
        self.matches_pseudo(dom, node, None)
    }

    /// 命中这个元素、且声明属于 `want` 那个盒吗。
    ///
    /// 按**每一条**复合选择器判，不按整条规则：`p::first-letter, h1 { color: red }`
    /// 里 `h1` 那条属于 h1 自己、`p::first-letter` 那条属于 p 的首字符，
    /// 整条规则只归一类会把两种落点之一丢掉。
    pub fn matches_pseudo(&self, dom: &Dom, node: NodeId, want: Option<PseudoElement>) -> bool {
        self.complexes
            .iter()
            .any(|c| c.pseudo == want && c.matches(dom, node))
    }

    pub fn parse(text: &str) -> Option<Selector> {
        let mut complexes = Vec::new();
        for part in split_top_level(text, ',') {
            complexes.push(parse_complex(&part)?);
        }
        (!complexes.is_empty()).then_some(Selector { complexes })
    }
}

fn parse_complex(text: &str) -> Option<Complex> {
    let chars: Vec<char> = text.trim().chars().collect();
    let mut cur = Cursor {
        chars: &chars,
        at: 0,
    };
    let mut left: Vec<(Combinator, Compound)> = Vec::new();
    let mut right = Compound::default();
    let mut pending: Option<Combinator> = None;
    let mut pseudo: Option<PseudoElement> = None;

    while let Some(c) = cur.next() {
        if c.is_whitespace() {
            if pending.is_none() && !right.simples.is_empty() {
                pending = Some(Combinator::Descendant);
            }
            continue;
        }
        // 伪元素挂在最右侧那个复合选择器之后，声明落在这个元素**之外**的一个盒上。
        // 单冒号的 `:first-letter` 是遗留写法，这里不认 —— 认了就得在每个伪类名上
        // 再判一次「它到底是不是伪元素」，而清单里的伪类一共才三个。
        if c == ':' && cur.peek() == Some(':') {
            cur.next();
            let name = read_ident(&mut cur)?;
            pseudo = match name.to_ascii_lowercase().as_str() {
                "first-letter" => Some(PseudoElement::FirstLetter),
                _ => return None,
            };
            // 之后不许再挂选择器内容：`p::first-letter em` 在 CSS 里就是非法的。
            while let Some(rest) = cur.peek() {
                if rest.is_whitespace() {
                    cur.next();
                    continue;
                }
                return None;
            }
            break;
        }
        let explicit = match c {
            '>' => Some(Combinator::Child),
            '+' => Some(Combinator::NextSibling),
            '~' => Some(Combinator::SubsequentSibling),
            _ => None,
        };
        if let Some(combinator) = explicit {
            if right.simples.is_empty() {
                return None; // 组合器左边没有东西
            }
            left.push((combinator, std::mem::take(&mut right)));
            pending = None;
            continue;
        }
        // 遇到下一个简单选择器，先把之前攒下的 descendant 组合器落实。
        if matches!(pending, Some(Combinator::Descendant)) && !right.simples.is_empty() {
            left.push((Combinator::Descendant, std::mem::take(&mut right)));
        }
        pending = None;
        right.simples.push(parse_simple(&mut cur, c)?);
    }
    if right.simples.is_empty() {
        return None;
    }
    Some(Complex {
        left,
        right,
        pseudo,
    })
}

fn parse_simple(cur: &mut Cursor, first: char) -> Option<Simple> {
    match first {
        '*' => Some(Simple::Universal),
        '.' => read_ident(cur).map(Simple::Class),
        '#' => read_ident(cur).map(Simple::Id),
        ':' => {
            cur.eat(':'); // 允许 :: 写法，但下面只认清单内的
            let name = read_ident(cur)?;
            match name.to_ascii_lowercase().as_str() {
                "first-child" => Some(Simple::FirstChild),
                "last-child" => Some(Simple::LastChild),
                "nth-child" => {
                    if !cur.eat('(') {
                        return None;
                    }
                    let arg = read_until(cur, ')')?;
                    let (a, b) = parse_nth_arg(&arg)?;
                    Some(Simple::NthChild { a, b })
                }
                "lang" => {
                    if !cur.eat('(') {
                        return None;
                    }
                    let arg = read_until(cur, ')')?;
                    let arg = arg.trim().trim_matches('"').trim_matches('\'').to_string();
                    (!arg.is_empty()).then_some(Simple::Lang(arg))
                }
                _ => None,
            }
        }
        '[' => {
            let inner = read_until(cur, ']')?;
            parse_attr(&inner)
        }
        c if is_ident_start(c) => {
            let mut name = String::new();
            name.push(c);
            read_ident_into(cur, &mut name);
            Some(Simple::Type(name.to_ascii_lowercase()))
        }
        _ => None,
    }
}

/// `An+B` 的参数 → `(A, B)`。认得的形态：`odd` / `even` / 裸整数 / `An+B`
/// （A、B 可带符号，符号与数字之间的空白允许，`2 n + 1` 也算）。
///
/// `of S` 后缀不认：它要的是「先按 S 过滤兄弟再数」那套命中逻辑，而写它的书
/// 基本都在给表格用。整条选择器作废，与清单外的其它构造同一纪律。
fn parse_nth_arg(text: &str) -> Option<(i32, i32)> {
    // 语法里所有空白都是可选的，先整段去掉，剩下的就只有三种形状：
    // 关键字、纯数字、`[…]n[…]`。
    let s: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    match s.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    let Some(at) = s.find('n') else {
        let b: i32 = s.parse().ok()?;
        return Some((0, b));
    };
    // `n` 左边：空串和单符号是「省略的 1 / -1」，即 `n` 与 `-n`。
    let a = match &s[..at] {
        "" | "+" => 1,
        "-" => -1,
        other => other.parse::<i32>().ok()?,
    };
    let b = match &s[at + 1..] {
        "" => 0,
        // CSS 要求 `n` 后面的 `B` **带符号**：`2n+1` 是第 3、5、7……，
        // 而 `2n3` 是写坏了的语法。悄悄当成 `(2,3)` 会让一条本应作废的规则
        // 命中一批谁也没预期的元素。
        other => {
            if !other.starts_with(['+', '-']) {
                return None;
            }
            other.parse::<i32>().ok()?
        }
    };
    Some((a, b))
}

/// 第 `index`（1 基）个元素是否命中 `An+B`（`n` 只能取非负整数）。
fn nth_matches(a: i32, b: i32, index: i32) -> bool {
    if a == 0 {
        return index == b;
    }
    let d = index - b;
    d % a == 0 && d / a >= 0
}

/// `[attr]` / `[attr op value]` / `[attr op value i]` 的**括号内**文本。
///
/// 算子是 `=` 前的那一个字符（`~` `|` `^` `$` `*`），所以先找 `=` 再回看一位 ——
/// 这比按 `~=` `|=` … 逐条 `find` 更稳：不会把 `[title="a=b"]` 里的第二个 `=` 当成算子。
/// 认不出写法（缺属性名、多余的尾令牌、未闭合引号）一律 `None`，让**整条选择器作废** ——
/// 悄悄当成 `[attr=…]` 命中一批没预期的元素，比作废难查得多。
fn parse_attr(text: &str) -> Option<Simple> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let Some(eq) = text.find('=') else {
        return Some(Simple::Attr {
            name: text.to_ascii_lowercase(),
            op: AttrOp::Exists,
            value: None,
            case_insensitive: false,
        });
    };
    let (name, op) = match text[..eq].chars().last() {
        Some('~') => (&text[..eq - 1], AttrOp::Includes),
        Some('|') => (&text[..eq - 1], AttrOp::DashMatch),
        Some('^') => (&text[..eq - 1], AttrOp::Prefix),
        Some('$') => (&text[..eq - 1], AttrOp::Suffix),
        Some('*') => (&text[..eq - 1], AttrOp::Substring),
        _ => (&text[..eq], AttrOp::Equals),
    };
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let (value, case_insensitive) = split_attr_value(text[eq + 1..].trim())?;
    Some(Simple::Attr {
        name: name.to_ascii_lowercase(),
        op,
        value: Some(value),
        case_insensitive,
    })
}

/// `=` 右侧：值（可带引号）+ 可选的大小写标志。返回 `None` 表示语法不认得。
fn split_attr_value(rest: &str) -> Option<(String, bool)> {
    let (value, trailing) = match rest.chars().next() {
        Some(q @ ('"' | '\'')) => {
            let after = &rest[1..];
            let end = after.find(q)?;
            (after[..end].to_string(), after[end + 1..].trim())
        }
        Some(_) => {
            let token = rest.split_ascii_whitespace().next().unwrap_or("");
            (token.to_string(), rest[token.len()..].trim())
        }
        // `[attr=]`：值就是空串（只有 `Equals` 可能命中一个空属性）。
        None => (String::new(), ""),
    };
    let case_insensitive = match trailing.to_ascii_lowercase().as_str() {
        "" | "s" => false,
        "i" => true,
        // 认不出的尾令牌（`[attr=v x]`）不是合法的属性选择器。
        _ => return None,
    };
    Some((value, case_insensitive))
}

/// 一条属性值是否命中算子。`Includes` / `DashMatch` / 三种子串算子对**空值**一律不命中
/// （CSS 规定）—— 否则 `[class^=""]` 会命中每一个带 `class` 的元素。
fn attr_matches(op: AttrOp, actual: &str, want: &str, ci: bool) -> bool {
    // 大小写不敏感时把两侧都折成 ASCII 小写再比；这条路上 `ci` 为假时**不做任何分配**。
    let (actual, want) = if ci {
        (actual.to_ascii_lowercase(), want.to_ascii_lowercase())
    } else {
        (actual.to_string(), want.to_string())
    };
    let (actual, want) = (actual.as_str(), want.as_str());
    match op {
        AttrOp::Exists => true,
        AttrOp::Equals => actual == want,
        AttrOp::Includes => {
            !want.is_empty() && actual.split_ascii_whitespace().any(|word| word == want)
        }
        AttrOp::DashMatch => {
            !want.is_empty()
                && (actual == want
                    || actual
                        .strip_prefix(want)
                        .is_some_and(|rest| rest.starts_with('-')))
        }
        AttrOp::Prefix => !want.is_empty() && actual.starts_with(want),
        AttrOp::Suffix => !want.is_empty() && actual.ends_with(want),
        AttrOp::Substring => !want.is_empty() && actual.contains(want),
    }
}

/// 只吃标识符字符，遇到别的就停（不消费），让主循环去判断组合器或空白。
fn read_ident(cur: &mut Cursor) -> Option<String> {
    let mut out = String::new();
    read_ident_into(cur, &mut out);
    (!out.is_empty()).then_some(out)
}

fn read_ident_into(cur: &mut Cursor, out: &mut String) {
    while let Some(c) = cur.peek() {
        if is_ident_char(c) {
            out.push(c);
            cur.at += 1;
        } else {
            break;
        }
    }
}

fn read_until(cur: &mut Cursor, close: char) -> Option<String> {
    let mut out = String::new();
    loop {
        match cur.next() {
            Some(c) if c == close => return Some(out),
            Some(c) => out.push(c),
            None => return None, // 括号未闭合
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

struct Cursor<'a> {
    chars: &'a [char],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.at += 1;
        }
        c
    }

    fn eat(&mut self, want: char) -> bool {
        if self.peek() == Some(want) {
            self.at += 1;
            true
        } else {
            false
        }
    }
}

/// 最近的 `lang` 声明，含 `xml:lang`。
fn nearest_lang(dom: &Dom, node: NodeId) -> Option<String> {
    dom.ancestors_including_self(node).find_map(|id| {
        dom.attr(id, "lang")
            .or_else(|| dom.attr(id, "xml:lang"))
            .map(String::from)
    })
}

/// `zh` 命中 `zh`、`zh-CN`、`zh_Hans`。
fn lang_matches(actual: &str, prefix: &str) -> bool {
    let actual = actual.trim();
    actual.eq_ignore_ascii_case(prefix)
        || actual
            .split(['-', '_'])
            .next()
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// 按分隔符切开，忽略括号与引号内部的分隔符。
pub(crate) fn split_top_level(text: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in text.chars() {
        if let Some(q) = quote {
            buf.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                buf.push(c);
            }
            '(' | '[' => {
                depth += 1;
                buf.push(c);
            }
            ')' | ']' => {
                depth -= 1;
                buf.push(c);
            }
            c if c == sep && depth <= 0 => out.push(std::mem::take(&mut buf)),
            _ => buf.push(c),
        }
    }
    out.push(buf);
    out.iter_mut().for_each(|s| *s = s.trim().to_string());
    out.retain(|s| !s.is_empty());
    out
}

#[cfg(test)]
mod tests;
