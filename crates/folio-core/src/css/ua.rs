//! UA 样式表的最小副本。
//!
//! 浏览器不是一份「作者没写就什么都不是」的空白：`<span>` 天生是 inline、
//! `<h1>` 天生比正文大且加粗、`<p>` 天生有 1em 上下外边距。出版书的 XHTML
//! 通常**不写**这些，全靠 UA 表兜着，所以我们必须有这一层，否则
//! 「作者 CSS 子集」再怎么对，版面也是一坨竖着堆的大号字块。
//!
//! 只做 `layout` 用得上的属性。顺序永远是 `inherit_from(parent)` → 本文件的
//! UA 值 → 作者声明，所以作者写 `h2 { display: inline }` 时照样能盖掉这里。

use super::computed::{ComputedStyle, Display, inherit_from, sub_shift, super_shift};

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
        // 外边距按自己的字号，这是规范的规定也是浏览器实际的效果。
        style.margin_top = style.font_size * margin;
        style.margin_bottom = style.font_size * margin;
    }

    match tag {
        "p" => {
            style.margin_top = style.font_size;
            style.margin_bottom = style.font_size;
        }
        "blockquote" => {
            style.margin_top = style.font_size;
            style.margin_bottom = style.font_size;
            style.margin_left = style.font_size * 2.5;
            style.margin_right = style.font_size * 2.5;
        }
        "ul" | "ol" | "dl" | "menu" => {
            style.margin_top = style.font_size;
            style.margin_bottom = style.font_size;
            style.padding_left = style.font_size * 2.5;
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
        "s" | "strike" | "del" => {
            style.line_through = true;
        }
        "hr" => {
            style.margin_top = style.font_size;
            style.margin_bottom = style.font_size;
        }
        _ if SKIPPED_TAGS.contains(&tag) => {
            style.display = Display::None;
        }
        _ => {}
    }
    style
}
