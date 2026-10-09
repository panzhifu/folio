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

#[cfg(test)]
mod tests {
    //! CSS 子集：规则装配、层叠与 specificity。只依赖公开 API。

    use crate::css::{Origin, RuleSet};
    use crate::dom::{Dom, NodeId};

    fn rules(text: &str) -> RuleSet {
        RuleSet::parse(text, Origin::Author)
    }

    fn first_p(dom: &Dom) -> NodeId {
        dom.elements()
            .find(|(_, n)| {
                n.tag
                    .as_ref()
                    .map(|t| t.local.to_string())
                    .unwrap_or_default()
                    == "p"
            })
            .map(|(id, _)| id)
            .expect("应有 p")
    }

    /// `@charset` / `@import` 没有块，在分号处结束。把它们和后面那条规则一起当成
    /// 「at-rule 前奏」吞掉，是最典型的「样式表明明在、body 规则却凭空没了」。
    #[test]
    fn statement_at_rules_end_at_the_semicolon() {
        let set = rules(
            "@charset \"utf-8\"; body { font-family: serif } @import \"x\"; p { color: red }",
        );
        assert_eq!(set.rules.len(), 2, "两条普通规则都要活下来");
        assert_eq!(set.rules[0].declarations[0].property, "font-family");
        assert_eq!(set.rules[1].declarations[0].property, "color");
        assert_eq!(set.ignored, 2, "@charset 与 @import 各计一条不支持");
    }

    #[test]
    fn parses_plain_rules() {
        let set = rules("p { color: red; font-size: 14px } .intro { text-indent: 2em; }");
        assert_eq!(set.rules.len(), 2);
        assert_eq!(set.rules[0].declarations.len(), 2);
        assert_eq!(set.rules[0].declarations[0].property, "color");
        assert_eq!(set.rules[0].declarations[0].value, "red");
        assert_eq!(set.rules[1].declarations[0].value, "2em");
        assert_eq!(set.ignored, 0);
    }

    #[test]
    fn trailing_semicolon_and_missing_one_are_both_fine() {
        for text in ["p { color: red }", "p { color: red; }", "p{color:red;}"] {
            let set = rules(text);
            assert_eq!(set.rules.len(), 1, "{text}");
            assert_eq!(set.rules[0].declarations.len(), 1, "{text}");
        }
    }

    #[test]
    fn comments_do_not_break_rule_splitting() {
        let set = rules("/* p { color: blue } */ p { color: red } /* 尾巴 */");
        assert_eq!(set.rules.len(), 1, "注释里的规则不该被当真规则");
        assert_eq!(set.rules[0].declarations[0].value, "red");
    }

    #[test]
    fn braces_inside_strings_are_balanced_correctly() {
        let set = rules(r#"p[content="{"] { color: red }"#);
        assert_eq!(set.rules.len(), 1);
    }

    #[test]
    fn media_queries_are_inlined_and_other_at_rules_dropped() {
        let set = rules(
            "@media screen { p { color: red } } @font-face { font-family: X } @page { margin: 1cm }",
        );
        assert_eq!(set.rules.len(), 1, "只有 @media 里的规则该被收进来");
        assert_eq!(set.rules[0].declarations[0].property, "color");
        assert_eq!(set.ignored, 2, "@font-face 与 @page 应计为忽略");
    }

    #[test]
    fn important_flag_is_recognised() {
        let set = rules("p { color: red !important; font-size: 12px }");
        assert!(set.rules[0].declarations[0].important);
        assert!(!set.rules[0].declarations[1].important);
    }

    #[test]
    fn unsupported_selector_discards_the_whole_rule_body() {
        let set = rules("p:hover { color: red; font-size: 1px } p { color: blue }");
        assert_eq!(set.rules.len(), 1, "带 :hover 的整条应被丢掉");
        assert_eq!(set.rules[0].declarations[0].value, "blue");
        assert_eq!(set.ignored, 2, "两条声明都该被计成被忽略");
    }

    #[test]
    fn malformed_tail_does_not_panic() {
        for text in [
            "p { color: red",
            "}",
            "{ color: red }",
            "p {",
            "",
            "p { }",
            "p { : red }",
            "@media",
        ] {
            let _ = rules(text);
        }
    }

    fn winner(dom: &Dom, set: &RuleSet, property: &str) -> String {
        let p = first_p(dom);
        set.matching_declarations(dom, p)
            .into_iter()
            .rfind(|d| d.property == property)
            .map(|d| d.value.clone())
            .unwrap_or_default()
    }

    #[test]
    fn author_normal_beats_reader_normal() {
        let dom = Dom::parse(b"<p id='b'>x</p>");
        let mut set = RuleSet::parse("p { color: black }", Origin::Reader);
        set.extend(RuleSet::parse("p { color: red }", Origin::Author));
        assert_eq!(winner(&dom, &set, "color"), "red");
    }

    #[test]
    fn four_cascade_levels_follow_the_css_order() {
        let dom = Dom::parse(b"<p id='b'>x</p>");

        // 普通声明：作者盖读者。
        let mut normal = RuleSet::parse("p { color: black }", Origin::Reader);
        normal.extend(RuleSet::parse("p { color: red }", Origin::Author));
        assert_eq!(winner(&dom, &normal, "color"), "red");

        // 加了 !important 反过来：读者盖作者。
        let mut flipped = RuleSet::parse("p { color: green !important }", Origin::Reader);
        flipped.extend(RuleSet::parse("p { color: red }", Origin::Author));
        assert_eq!(winner(&dom, &flipped, "color"), "green");

        // 两边都 !important 时仍是读者赢 —— CSS Cascade 从低到高是
        // UA normal < user normal < author normal < author !important < user !important。
        let mut both = RuleSet::parse("p { color: green !important }", Origin::Reader);
        both.extend(RuleSet::parse(
            "p { color: red !important }",
            Origin::Author,
        ));
        assert_eq!(winner(&dom, &both, "color"), "green");
    }

    #[test]
    fn important_is_ordered_per_declaration_not_per_rule() {
        // 一条规则里混了重要与普通声明：按规则聚合排序会把两条一起抬到
        // AuthorImportant 级，让普通的 font-size 错误地盖掉后面的作者规则。
        let dom = Dom::parse(b"<p>x</p>");
        let mut set = RuleSet::parse(
            "p { color: red !important; font-size: 99px }",
            Origin::Author,
        );
        set.extend(RuleSet::parse("p { font-size: 20px }", Origin::Author));
        assert_eq!(
            winner(&dom, &set, "font-size"),
            "20px",
            "普通声明不该被同规则里的 !important 抬级"
        );
        assert_eq!(winner(&dom, &set, "color"), "red");
    }

    #[test]
    fn higher_specificity_wins_when_both_are_author() {
        let dom = Dom::parse(b"<p class='a' id='b'>x</p>");
        let set = rules("p { color: one } .a { color: two } #b { color: three }");
        assert_eq!(winner(&dom, &set, "color"), "three");
    }

    #[test]
    fn equal_specificity_later_rule_wins() {
        let dom = Dom::parse(b"<p>x</p>");
        let set = rules("p { color: one } p { color: two }");
        assert_eq!(winner(&dom, &set, "color"), "two");
    }
}
