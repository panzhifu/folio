//! CSS 子集：样式表装配 + 层叠出 `ComputedStyle`。
//!
//! 范围就是 docs/development-plan.md §4.4 那份清单。清单外的声明**被忽略而不是报错**，
//! 因为真实出版书里总有些用不上的声明，为一两个 `transform` 让整本书排版失败
//! 是最糟的失败方式。

pub mod computed;
pub mod selector;
pub mod ua;

use selector::Selector;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Reader,
    Author,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub property: String,
    pub value: String,
    pub important: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub selector: Selector,
    pub declarations: Vec<Declaration>,
    pub origin: Origin,
    /// 同一层叠来源内的出现顺序，用于 specificity 打平时后者优先。
    pub order: u32,
}

/// 层叠级别，从低到高。
///
/// 注意 origin 与 `!important` 是**交错**的，不是「先比来源再比重要」：
/// 普通声明里作者盖读者，加了 `!important` 反过来读者盖作者。
/// 我们没有 UA 层，所以只有这四级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CascadeLevel {
    ReaderNormal,
    AuthorNormal,
    AuthorImportant,
    ReaderImportant,
}

fn level_of(origin: Origin, important: bool) -> CascadeLevel {
    match (origin, important) {
        (Origin::Reader, false) => CascadeLevel::ReaderNormal,
        (Origin::Author, false) => CascadeLevel::AuthorNormal,
        (Origin::Author, true) => CascadeLevel::AuthorImportant,
        (Origin::Reader, true) => CascadeLevel::ReaderImportant,
    }
}

impl Rule {
    /// 同一层叠级别内的排序键：specificity 优先，打平时后出现的赢。
    fn order_key(&self) -> u64 {
        (u64::from(self.selector.specificity()) << 32) | u64::from(self.order)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    pub rules: Vec<Rule>,
    /// 被丢弃的声明数，用于「这本书有多少样式我们不支持」的诊断与文档。
    pub ignored: usize,
}

impl RuleSet {
    pub fn parse(text: &str, origin: Origin) -> RuleSet {
        let mut set = RuleSet::default();
        set.consume(text, origin);
        set
    }

    /// 追加另一张表。EPUB 常有多份 `.css` 按 spine 顺序生效，
    /// 所以合并时必须重排 `order`，否则两张表的 specificity 打平时先后会错。
    pub fn extend(&mut self, mut other: RuleSet) {
        let base = self.rules.len() as u32;
        for (i, rule) in other.rules.iter_mut().enumerate() {
            rule.order = base + i as u32;
        }
        self.rules.append(&mut other.rules);
        self.ignored += other.ignored;
    }

    fn consume(&mut self, text: &str, origin: Origin) {
        let text = strip_comments(text);
        let mut rest = text.as_str();
        loop {
            let Some(open) = find_unquoted(rest, '{') else {
                return;
            };
            let prelude = rest[..open].trim();
            // 语句型 at-rule（`@charset` / `@import`）没有块，在分号处就结束了。
            // 不先单独吞掉它，prelude 会把下一条规则的整串选择器一起吃进去，
            // 于是 `@charset "utf-8"; body { … }` 里的 body 规则凭空消失。
            if prelude.starts_with('@')
                && let Some(semi) = find_unquoted(&rest[..open], ';')
            {
                self.ignored += 1;
                rest = &rest[semi + 1..];
                continue;
            }
            let Some(close) = matching_brace(&rest[open..]) else {
                return; // 括号不闭合，剩下的整体放弃
            };
            // matching_brace 返回的是配对的 `}` 之后一位（相对 open 的偏移）。
            let body = &rest[open + 1..open + close - 1];
            rest = &rest[open + close..];

            if let Some(at) = prelude.strip_prefix('@') {
                let name = at.split_whitespace().next().unwrap_or("");
                match name.to_ascii_lowercase().as_str() {
                    // 一期不分屏幕/打印媒体查询，一律当普通规则收进来。
                    "media" | "supports" => self.consume(body, origin),
                    // @font-face / @page / @keyframes 的声明不属于任何元素。
                    _ => self.ignored += 1,
                }
                continue;
            }
            let Some(selector) = Selector::parse(prelude) else {
                self.ignored += count_declarations(body);
                continue;
            };
            let mut declarations = Vec::new();
            for decl in split_top_level(body, ';') {
                match parse_declaration(&decl) {
                    Some(d) => declarations.push(d),
                    None => self.ignored += 1,
                }
            }
            if !declarations.is_empty() {
                self.rules.push(Rule {
                    selector,
                    declarations,
                    origin,
                    order: self.rules.len() as u32,
                });
            }
        }
    }

    /// 命中某元素的全部声明，按层叠优先级**从低到高**返回。
    ///
    /// 以「声明」而不是「规则」为单位排序：`!important` 是逐条声明的属性，
    /// 按规则聚合会让一条混合了重要/普通声明的规则整体错位。
    pub fn matching_declarations(
        &self,
        dom: &crate::dom::Dom,
        node: crate::dom::NodeId,
    ) -> Vec<&Declaration> {
        let hits: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|r| r.selector.matches(dom, node))
            .collect();
        let mut out = Vec::new();
        for level in [
            CascadeLevel::ReaderNormal,
            CascadeLevel::AuthorNormal,
            CascadeLevel::AuthorImportant,
            CascadeLevel::ReaderImportant,
        ] {
            let mut group: Vec<(&Rule, &Declaration)> = hits
                .iter()
                .flat_map(|r| r.declarations.iter().map(move |d| (*r, d)))
                .filter(|(r, d)| level_of(r.origin, d.important) == level)
                .collect();
            group.sort_by_key(|(r, _)| r.order_key());
            out.extend(group.into_iter().map(|(_, d)| d));
        }
        out
    }
}

fn parse_declaration(text: &str) -> Option<Declaration> {
    let (property, value) = text.split_once(':')?;
    let property = property.trim().to_ascii_lowercase();
    if property.is_empty() {
        return None;
    }
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    // `!important` 只可能出现在值的末尾。
    if let Some((head, tail)) = value.rsplit_once('!')
        && tail.trim().eq_ignore_ascii_case("important")
    {
        return Some(Declaration {
            property,
            value: head.trim().to_string(),
            important: true,
        });
    }
    Some(Declaration {
        property: property.to_string(),
        value: value.to_string(),
        important: false,
    })
}

fn count_declarations(body: &str) -> usize {
    split_top_level(body, ';').len()
}

/// 把 `/* … */` 换成等长空格。必须在上层做：注释常夹在选择器前面，
/// 留着它 `p` 就变成 `/* … */ p`，选择器解析直接失败。
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            out.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
            out.push(c);
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            loop {
                match chars.next() {
                    None => break,
                    Some('*') if chars.peek() == Some(&'/') => {
                        chars.next();
                        break;
                    }
                    // 换行保留，其余按空格填充，避免把两个 token 黏在一起。
                    Some('\n') => out.push('\n'),
                    Some(_) => out.push(' '),
                }
            }
            out.push(' ');
            continue;
        }
        out.push(c);
    }
    out
}

fn find_unquoted(text: &str, target: char) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            c if c == target => return Some(i),
            _ => {}
        }
    }
    None
}

/// 返回从 `text` 开头那个 `{` 到其配对 `}` 的结束偏移（不含）。
fn matching_brace(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level(text: &str, sep: char) -> Vec<String> {
    selector::split_top_level(text, sep)
}
