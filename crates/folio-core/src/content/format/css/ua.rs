//! UA 样式表的最小副本。
//!
//! 浏览器不是一份「作者没写就什么都不是」的空白：`<span>` 天生是 inline、
//! `<h1>` 天生比正文大且加粗、`<p>` 天生有 1em 上下外边距。出版书的 XHTML
//! 通常**不写**这些，全靠 UA 表兜着，所以我们必须有这一层，否则
//! 「作者 CSS 子集」再怎么对，版面也是一坨竖着堆的大号字块。
//!
//! 只做 `layout` 用得上的属性。顺序永远是 `inherit_from(parent)` → 本文件的
//! UA 值 → 作者声明，所以作者写 `h2 { display: inline }` 时照样能盖掉这里。

use super::computed::{ComputedStyle, Display, Length, ListStyle, inherit_from};
use super::value::{sub_shift, super_shift};

/// 天生 inline 的标签。清单外的一律按 block 处理：
/// 新出现的容器标签按 block 降级不会丢内容，反过来会把整段撑散。
const INLINE_TAGS: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "big", "cite", "code", "data", "dfn", "em", "font", "i", "ins",
    "kbd", "label", "mark", "nobr", "q", "rp", "rt", "ruby", "s", "samp", "small", "span",
    "strike", "strong", "sub", "sup", "tt", "u", "var", "wbr",
];

/// (字号相对父元素的倍率, 上下外边距相对**自己**字号的倍率)。
const HEADINGS: &[(&str, f32, f32)] = &[
    ("h1", 2.0, 0.67),
    ("h2", 1.5, 0.83),
    ("h3", 1.17, 1.0),
    ("h4", 1.0, 1.33),
    ("h5", 0.83, 1.67),
    ("h6", 0.67, 2.33),
];

/// 遇到这些标签整块跳过：要么没有可渲染内容，要么是元数据。
const SKIPPED_TAGS: &[&str] = &[
    "br", "script", "style", "head", "title", "template", "meta", "link", "area", "base", "source",
    "track", "param",
];

pub fn is_inline_tag(tag: &str) -> bool {
    INLINE_TAGS.contains(&tag)
}

/// 这一类标签的**内容**整块不进排版。锚点坐标系要用它把 `<style>` / `<script>` 里
/// 的字符从「可见文本」里排除出去 —— 不然一章的「第 N 个字」会把内联 CSS 也算进去，
/// 而重定位搜文也就会在 CSS 里命中。
pub fn is_skipped_tag(tag: &str) -> bool {
    SKIPPED_TAGS.contains(&tag)
}

/// [`is_skipped_tag`] 的另一头：命中时给出名单里那份 `'static` 的名字。
///
/// 单开一条是为了让「要记这个名字」的调用方一次分配都不花 —— [`crate::content::scan`]
/// 的跳过栈要一路存标签名，用 `String` 就是每个跳过元素一次堆分配，
/// 而它必须和这张表是同一份，抄一份名单迟早分叉。
pub fn skipped_tag(tag: &str) -> Option<&'static str> {
    SKIPPED_TAGS.iter().copied().find(|known| *known == tag)
}

/// 替换元素：自己没有文本，尺寸由内容决定，只能作为行内对象出现。
pub fn is_replaced_inline(tag: &str) -> bool {
    matches!(tag, "img" | "svg" | "canvas" | "video" | "audio" | "iframe")
}

/// 父样式 + 标签 → 该元素的初始样式（含继承项）。
pub fn seed(parent: &ComputedStyle, tag: &str) -> ComputedStyle {
    let mut style = inherit_from(parent);
    // 替换元素（`<img>` 等）天生 inline：与浏览器一致，也让「独占一行的插图」
    // 通过父块的 `text-align` 居中，而不必额外写 `display: block`。
    style.display = if is_inline_tag(tag) || is_replaced_inline(tag) {
        Display::Inline
    } else if tag == "li" {
        Display::ListItem
    } else {
        Display::Block
    };

    if let Some((_, scale, margin)) = HEADINGS.iter().find(|(name, ..)| *name == tag) {
        style.font_size = parent.font_size * scale;
        style.font_weight = 700;
        // 外边距按自己的字号，这是规范的规定也是浏览器实际的效果。UA 表给的都是 px 数，
        // 没有百分比 —— 包不包裹这一层只影响读起来顺不顺。
        style.margin_top = Length::Px(style.font_size * margin);
        style.margin_bottom = Length::Px(style.font_size * margin);
        // `text-indent` 是可继承属性，而读者的「首行缩进」是挂在根样式上进来的
        // （`ui::reader::document`），照继承走会连章标题一起推进版心。浏览器渲染同一份
        // 作者 CSS 时标题不缩（那边没有这个继承源），真实阅读器把缩进注入成 `p { … }` 也是
        // 这个结果。作者自己写了 `text-indent` 仍在上面那一层胜出。
        style.text_indent = Length::ZERO;
    }

    match tag {
        "p" => {
            style.margin_top = Length::Px(style.font_size);
            style.margin_bottom = Length::Px(style.font_size);
        }
        "blockquote" => {
            style.margin_top = Length::Px(style.font_size);
            style.margin_bottom = Length::Px(style.font_size);
            style.margin_left = Length::Px(style.font_size * 2.5);
            style.margin_right = Length::Px(style.font_size * 2.5);
        }
        "ul" | "ol" | "dl" | "menu" => {
            style.margin_top = Length::Px(style.font_size);
            style.margin_bottom = Length::Px(style.font_size);
            style.padding_left = Length::Px(style.font_size * 2.5);
            // 标记的类型由 UA 表定：`ol` 十进制、`ul` / `menu` 圆点。两条都要写出来，
            // 不能只写 `ol` 再指望初值兜住 `ul` —— `list-style-type` 可继承，嵌套列表
            // （`ol` 里套 `ul`）靠的就是这一条把继承来的十进制改回圆点。
            match tag {
                "ol" => style.list_style_type = ListStyle::Decimal,
                "ul" | "menu" => style.list_style_type = ListStyle::Disc,
                _ => {}
            }
        }
        "pre" => {
            style.white_space_pre = true;
        }
        "small" => {
            style.font_size = parent.font_size / 1.2;
        }
        "big" => {
            style.font_size = parent.font_size * 1.17;
        }
        "b" | "strong" => {
            style.font_weight = 700;
        }
        "i" | "em" | "cite" | "dfn" => {
            style.font_italic = true;
        }
        "code" | "kbd" | "samp" | "tt" => {
            // 泛族名交给上层解析：`Measure` 的实现知道等宽字体是哪个 family。
            style.font_family = "monospace".into();
            style.font_weight = 400;
            style.font_italic = false;
        }
        "sub" | "sup" => {
            // 缩字号 + 位移。位移走 `super_shift` / `sub_shift`，与作者写
            // `vertical-align: super` 是同一条式子，所以裸 `<sup>` 与
            // `<span class=fn>` 带声明的上标一样高。
            style.font_size = parent.font_size / 1.5;
            style.baseline_shift = if tag == "sup" {
                super_shift(style.font_size)
            } else {
                sub_shift(style.font_size)
            };
        }
        "u" | "ins" => {
            style.underline = true;
        }
        // 链接要有可见性：浏览器默认给 `<a>` 加下划线，出版书里的目录页、脚注引注
        // 都靠它认。只加下划线、不写颜色 —— `color` 可继承，UA 里钉一个固定色在暗色
        // 主题上就是一行看不清的蓝字；留给以后按主题着墨。
        "a" => {
            style.underline = true;
        }
        "s" | "strike" | "del" => {
            style.line_through = true;
        }
        "hr" => {
            style.margin_top = Length::Px(style.font_size);
            style.margin_bottom = Length::Px(style.font_size);
        }
        _ if SKIPPED_TAGS.contains(&tag) => {
            style.display = Display::None;
        }
        _ => {}
    }
    style
}

#[cfg(test)]
mod tests {
    //! UA 层给的值：显示类型、标题的字号与边距，以及「哪些继承值到这一层为止」。

    use super::seed;
    use crate::content::dom::Dom;
    use crate::content::format::css::computed::{ComputedStyle, Length, cascade};
    use crate::content::format::css::{Origin, RuleSet};

    fn node_id(dom: &Dom, tag: &str) -> crate::content::dom::NodeId {
        dom.elements()
            .find(|(_, n)| {
                n.tag
                    .as_ref()
                    .map(|t| t.local.to_string())
                    .unwrap_or_default()
                    == tag
            })
            .map(|(id, _)| id)
            .expect("标签在该页内")
    }

    /// 读者的「首行缩进」是挂在根样式上进来的，而 `text-indent` 可继承：
    /// 标题要在 UA 这一层把它断掉，正文那一份留着。
    #[test]
    fn headings_stop_the_inherited_indent_while_paragraphs_keep_it() {
        let parent = ComputedStyle {
            text_indent: Length::Px(34.0),
            ..ComputedStyle::default()
        };
        assert_eq!(seed(&parent, "h1").text_indent, Length::ZERO);
        assert_eq!(seed(&parent, "h2").text_indent, Length::ZERO);
        assert_eq!(seed(&parent, "h6").text_indent, Length::ZERO);
        assert_eq!(
            seed(&parent, "p").text_indent,
            Length::Px(34.0),
            "正文的首行缩进不该被动到"
        );
        assert_eq!(seed(&parent, "div").text_indent, Length::Px(34.0));
    }

    /// 走完整层叠：UA 的归零在作者声明之下，书自己写了 `h2 { text-indent }` 时仍按书的。
    #[test]
    fn an_author_indent_on_a_heading_still_wins() {
        let base = ComputedStyle {
            text_indent: Length::Px(34.0),
            ..ComputedStyle::default()
        };
        let html = "<div><h2>章</h2><p>文</p></div>";

        let dom = Dom::parse(html.as_bytes());
        let styled = cascade(&dom, &RuleSet::default(), &base);
        assert_eq!(
            styled.get(node_id(&dom, "h2")).unwrap().text_indent,
            Length::ZERO
        );
        assert_eq!(
            styled.get(node_id(&dom, "p")).unwrap().text_indent,
            Length::Px(34.0)
        );

        let dom = Dom::parse(html.as_bytes());
        let rules = RuleSet::parse("h2 { text-indent: 40px }", Origin::Author);
        let styled = cascade(&dom, &rules, &base);
        assert_eq!(
            styled.get(node_id(&dom, "h2")).unwrap().text_indent,
            Length::Px(40.0),
            "UA 的归零只是初值，压不过作者声明"
        );
    }
}
