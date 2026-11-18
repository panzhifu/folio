//! 在复制样式表或递归解析 @media/@supports 前限制实际输入。

use crate::limits::{MAX_CSS_BLOCKS, MAX_CSS_BYTES, MAX_CSS_DECLARATIONS, MAX_CSS_DEPTH};

pub(super) fn allows(text: &str) -> bool {
    if text.len() > MAX_CSS_BYTES {
        return false;
    }
    let bytes = text.as_bytes();
    let (mut depth, mut blocks, mut declarations) = (0usize, 0usize, 0usize);
    let mut quote = None;
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if let Some(delimiter) = quote {
            if byte == b'\\' {
                at += 2;
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
        } else if byte == b'/' && bytes.get(at + 1) == Some(&b'*') {
            at += 2;
            while at + 1 < bytes.len() && &bytes[at..at + 2] != b"*/" {
                at += 1;
            }
            at = (at + 2).min(bytes.len());
            continue;
        } else {
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'{' => {
                    depth += 1;
                    blocks += 1;
                    if depth > MAX_CSS_DEPTH || blocks > MAX_CSS_BLOCKS {
                        return false;
                    }
                }
                b'}' => depth = depth.saturating_sub(1),
                b';' => {
                    declarations += 1;
                    if declarations > MAX_CSS_DECLARATIONS {
                        return false;
                    }
                }
                _ => {}
            }
        }
        at += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use crate::css::selector::Selector;
    use crate::css::{Origin, RuleSet};
    use crate::dom::Dom;
    use crate::limits::{MAX_CSS_BLOCKS, MAX_CSS_BYTES, MAX_CSS_DECLARATIONS};

    #[test]
    fn css_budget_rejects_deep_real_rules_before_recursive_parsing() {
        let css = include_str!("../../../../tests/fixtures/synthetic/deep-media.css");
        let rules = RuleSet::parse(css, Origin::Author);
        assert!(rules.rules.is_empty());
        assert_eq!(rules.ignored_rules[0].kind, "budget");
        let ordinary = RuleSet::parse(
            "@media screen { @supports (display: block) { p { color: red } } }",
            Origin::Author,
        );
        assert_eq!(ordinary.rules.len(), 1);
    }

    #[test]
    fn css_budget_bounds_bytes_blocks_and_declarations() {
        for css in [
            " ".repeat(MAX_CSS_BYTES + 1),
            "p{color:red}".repeat(MAX_CSS_BLOCKS + 1),
            format!(
                "p{{{}color:blue}}",
                "color:red;".repeat(MAX_CSS_DECLARATIONS + 1)
            ),
        ] {
            let rules = RuleSet::parse(&css, Origin::Author);
            assert!(rules.rules.is_empty());
            assert_eq!(rules.ignored_rules[0].kind, "budget");
        }
    }

    #[test]
    fn css_budget_does_not_count_braces_in_comments_or_quoted_values() {
        let braces = "{".repeat(100);
        let css = format!("/*{braces}*/ p {{ font-family: \"{braces}\"; color: red }}");
        let rules = RuleSet::parse(&css, Origin::Author);
        assert_eq!(rules.rules.len(), 1);
        assert!(!rules.ignored_rules.iter().any(|rule| rule.kind == "budget"));
    }

    #[test]
    fn css_budget_stops_selector_backtracking_and_preserves_ordinary_matches() {
        let dom = Dom::parse(
            format!(
                "{}<p>content</p>{}",
                "<div>".repeat(80),
                "</div>".repeat(80)
            )
            .as_bytes(),
        );
        let paragraph = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("p"))
            .unwrap()
            .0;
        let absent = Selector::parse(&format!("article {}p", "div ".repeat(24))).unwrap();
        assert!(!absent.matches(&dom, paragraph));
        assert!(
            Selector::parse("body div div p")
                .unwrap()
                .matches(&dom, paragraph)
        );
        assert!(Selector::parse(&format!("{}p", "div ".repeat(140))).is_none());
    }

    #[test]
    fn css_budget_also_guards_inline_style_before_url_rewriting() {
        use crate::css::computed::{ComputedStyle, cascade};
        let source = format!(
            "<p style='{}color:red'>text</p>",
            "color:blue;".repeat(MAX_CSS_DECLARATIONS + 1)
        );
        let dom = Dom::parse(source.as_bytes());
        let paragraph = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("p"))
            .unwrap()
            .0;
        let base = ComputedStyle::default();
        let styled = cascade(&dom, &RuleSet::default(), &base, "");
        assert_eq!(styled.get(paragraph).unwrap().color, base.color);
    }
}
