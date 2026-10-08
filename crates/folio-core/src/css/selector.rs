//! 选择器子集：解析、匹配、specificity。
//!
//! 不接 `selectors` crate 是有意为之：它的 `SelectorImpl` 要 10 个关联类型外加
//! `NonTSPseudoClass` / `PseudoElement` 两套 trait，那是为 Servo 的全量
//! CSS Selectors 4 准备的；本引擎承诺的范围见 docs/development-plan.md §4.4，
//! 背那套样板换不来用得上的能力。
//!
//! 支持：`*`、type、`.class`、`#id`、`[attr]`、`[attr=v]`、
//! 后代 / `>` 子代 / `+` 相邻兄弟 / `~` 一般兄弟、`,` 分组、
//! `:first-child`、`:last-child`、`:lang()`。
//! 清单外的构造让**整条选择器作废**，而不是部分生效 ——
//! 部分生效会让作者样式以不可预期的方式命中内容。

use crate::dom::{Dom, NodeId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Simple {
    Universal,
    Type(String),
    Class(String),
    Id(String),
    Attr { name: String, value: Option<String> },
    FirstChild,
    LastChild,
    Lang(String),
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Complex {
    pub left: Vec<(Combinator, Compound)>,
    pub right: Compound,
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
            Simple::Attr { name, value } => match dom.attr(node, name) {
                None => false,
                Some(actual) => value.as_ref().is_none_or(|want| *want == actual),
            },
            Simple::FirstChild => dom
                .get(node)
                .parent
                .is_some_and(|p| dom.element_children(p).next() == Some(node)),
            Simple::LastChild => dom
                .get(node)
                .parent
                .is_some_and(|p| dom.element_children(p).last() == Some(node)),
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
        self.complexes.iter().any(|c| c.matches(dom, node))
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

    while let Some(c) = cur.next() {
        if c.is_whitespace() {
            if pending.is_none() && !right.simples.is_empty() {
                pending = Some(Combinator::Descendant);
            }
            continue;
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
    Some(Complex { left, right })
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

fn parse_attr(text: &str) -> Option<Simple> {
    let text = text.trim();
    match text.split_once('=') {
        Some((name, value)) => Some(Simple::Attr {
            name: name.trim().to_ascii_lowercase(),
            value: Some(
                value
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string(),
            ),
        }),
        None if !text.is_empty() => Some(Simple::Attr {
            name: text.to_ascii_lowercase(),
            value: None,
        }),
        None => None,
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
