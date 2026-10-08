//! 把命中的声明算成 `ComputedStyle`：值解析 + 继承 + 沿树展开。
//!
//! 支持的属性就是 docs/development-plan.md §4.4 那份清单。清单外的声明**被忽略而不是报错**，
//! 因为真实出版书里总有些用不上的声明，为一两个 `transform` 让整本书排版失败
//! 是最糟的失败方式。

use crate::dom::{Dom, NodeId, NodeKind};

use super::{Declaration, RuleSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Right,
    Center,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Display {
    #[default]
    Block,
    Inline,
    InlineBlock,
    ListItem,
    /// `display: none`：排版层负责整块跳过。
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u8, pub u8, pub u8);

/// 一个尺寸值。`auto` / `none` 用 `Option::None` 表示，两种「没有显式值」在这里
/// 是一回事（都用内在尺寸或默认值兜底）。百分比不在此刻折算 —— 它要按**包含块
/// 的宽度**来算，那是排版期才知道的数，见 `Length::resolve`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    Px(f32),
    /// 比例，`50%` 存成 `0.5`。
    Percent(f32),
}

impl Length {
    /// 按包含块基准长度折算成 px。
    pub fn resolve(self, basis: f32) -> f32 {
        match self {
            Length::Px(v) => v,
            Length::Percent(p) => p * basis,
        }
    }
}

/// 排版层需要的计算后样式。长度一律已折算成 px，`line_height` 是倍数。
#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    pub font_size: f32,
    pub line_height: f32,
    pub font_weight: u16,
    pub font_italic: bool,
    pub font_family: String,
    pub text_align: TextAlign,
    pub text_indent: f32,
    pub margin_top: f32,
    pub margin_bottom: f32,
    pub margin_left: f32,
    pub margin_right: f32,
    pub padding_top: f32,
    pub padding_bottom: f32,
    pub padding_left: f32,
    pub padding_right: f32,
    pub display: Display,
    pub color: Option<Color>,
    pub letter_spacing: f32,
    pub white_space_pre: bool,
    /// 替换元素（图片）的尺寸。块级元素的 width 一期不做，只有图片用得上。
    pub width: Option<Length>,
    pub height: Option<Length>,
    pub max_width: Option<Length>,
    pub max_height: Option<Length>,
}

impl Default for ComputedStyle {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            line_height: 1.75,
            font_weight: 400,
            font_italic: false,
            font_family: String::new(),
            text_align: TextAlign::Left,
            text_indent: 0.0,
            margin_top: 0.0,
            margin_bottom: 0.0,
            margin_left: 0.0,
            margin_right: 0.0,
            padding_top: 0.0,
            padding_bottom: 0.0,
            padding_left: 0.0,
            padding_right: 0.0,
            display: Display::Block,
            color: None,
            letter_spacing: 0.0,
            white_space_pre: false,
            width: None,
            height: None,
            max_width: None,
            max_height: None,
        }
    }
}

/// 认识但一期不实现：接受、不生效、不计入 ignored。
///
/// 这一份和下面 `inherit_from` 的区别在于：这些属性**真实出版书里满地都是**
/// （`border`、`width`、`text-decoration`），若按不支持计数，ignored 会被刷到
/// 几百条，真正需要关注的「我们没预料到的写法」就淹没了。
const KNOWN_UNIMPLEMENTED: &[&str] = &[
    "text-decoration",
    "background",
    "background-color",
    "min-height",
    "border",
    "border-left",
    "border-right",
    "border-top",
    "border-bottom",
    "text-shadow",
    "word-spacing",
    "direction",
    "writing-mode",
    "orphans",
    "widows",
    "page-break-before",
    "page-break-after",
    "break-inside",
    "float",
    "clear",
    "columns",
    "column-gap",
    "vertical-align",
    "list-style",
    "list-style-type",
    "text-transform",
    // `font: 1em serif` 简写在老 EPUB 里不少，一期只认长写法。
    "font",
];

#[derive(Debug, Default)]
pub struct StyledDom {
    /// 按 `Dom::nodes` 下标索引；文本节点也有一条（整份继承父元素）。
    pub styles: Vec<ComputedStyle>,
    /// 认识不了的值。`KNOWN_UNIMPLEMENTED` 里的属性不计入。
    pub ignored: usize,
}

impl StyledDom {
    pub fn get(&self, id: NodeId) -> Option<&ComputedStyle> {
        self.styles.get(id as usize)
    }
}

/// 层叠整棵树。`root_base` 是读者偏好折算出来的根样式（字号、行距、字体等）。
pub fn cascade(dom: &Dom, rules: &RuleSet, root_base: &ComputedStyle) -> StyledDom {
    let mut styles = vec![root_base.clone(); dom.nodes.len()];
    let mut ignored = 0usize;

    // 先序遍历：算任一节点时它的父样式必定已算完，继承只需往上走一步。
    for id in document_order(dom) {
        let node = dom.get(id);
        let parent = match node.parent {
            Some(p) => styles[p as usize].clone(),
            None => root_base.clone(),
        };
        if node.kind != NodeKind::Element {
            // 文本节点没有选择器可言，直接沿用父元素算完的样式。
            styles[id as usize] = parent;
            continue;
        }
        // 空标签名命中不到任何 UA 规则，等价于「只继承」。
        let mut style = crate::css::ua::seed(&parent, dom.tag_name(id).unwrap_or_default());
        let decls = rules.matching_declarations(dom, id);

        // font-size 先算：em / % 都按**元素自己的**字号折算，
        // 若按声明书写顺序来，`text-indent: 2em; font-size: 20px` 会算错。
        // line-height 其次，理由相同（百分比与 px 都要除以自己的字号）。
        for group in 0..3 {
            for decl in &decls {
                let is_font_size = decl.property == "font-size";
                let is_line_height = decl.property == "line-height";
                let mine = match group {
                    0 => is_font_size,
                    1 => is_line_height,
                    _ => !is_font_size && !is_line_height,
                };
                if mine && !apply(&mut style, &parent, root_base.font_size, decl) {
                    ignored += 1;
                }
            }
        }
        styles[id as usize] = style;
    }
    StyledDom { styles, ignored }
}

/// 元素节点：可继承属性从父样式带下来，其余从默认值起算。
///
/// 可继承的就是下面抄走的这几项 —— font-* / color / text-align / letter-spacing /
/// text-indent，另有 line-height 也在其中。margin / padding / display 不继承，
/// 从 `ComputedStyle::default()` 起算。
pub(crate) fn inherit_from(parent: &ComputedStyle) -> ComputedStyle {
    ComputedStyle {
        font_size: parent.font_size,
        line_height: parent.line_height,
        font_weight: parent.font_weight,
        font_italic: parent.font_italic,
        font_family: parent.font_family.clone(),
        text_align: parent.text_align,
        color: parent.color,
        letter_spacing: parent.letter_spacing,
        text_indent: parent.text_indent,
        ..Default::default()
    }
}

/// 返回 false 表示这条声明不认识（或值解析失败），由调用方计入 ignored。
///
/// `root_px` 是根元素（读者偏好）的字号，rem 只按它折算；
/// `parent` 提供 font-size 的相对基准。em 在 `font-size` 里按父字号、
/// 在别处按自己的字号 —— 这是 CSS 的规定，不是我们的选择。
fn apply(
    style: &mut ComputedStyle,
    parent: &ComputedStyle,
    root_px: f32,
    decl: &Declaration,
) -> bool {
    let value = decl.value.trim();
    if value.is_empty() {
        return false;
    }
    if value.eq_ignore_ascii_case("inherit") {
        return true; // 继承值已由 inherit_from 处理好
    }
    if KNOWN_UNIMPLEMENTED.contains(&decl.property.as_str()) {
        return true;
    }
    let own = style.font_size;
    match decl.property.as_str() {
        "font-size" => match parse_font_size(value, parent.font_size, root_px) {
            Some(px) => {
                style.font_size = px;
                true
            }
            None => false,
        },
        "line-height" => match parse_line_height(value, own, root_px) {
            Some(lh) => {
                style.line_height = lh;
                true
            }
            None => false,
        },
        "font-weight" => match parse_font_weight(value) {
            Some(w) => {
                style.font_weight = w;
                true
            }
            None => false,
        },
        "font-style" => match value.to_ascii_lowercase().as_str() {
            "normal" => {
                style.font_italic = false;
                true
            }
            "italic" | "oblique" => {
                style.font_italic = true;
                true
            }
            _ => false,
        },
        "font-family" => {
            let first = value.split(',').next().unwrap_or("").trim();
            if first.is_empty() {
                return false;
            }
            style.font_family = first.trim_matches('"').trim_matches('\'').to_string();
            true
        }
        "text-align" => match value.to_ascii_lowercase().as_str() {
            "left" | "start" => {
                style.text_align = TextAlign::Left;
                true
            }
            "right" | "end" => {
                style.text_align = TextAlign::Right;
                true
            }
            "center" => {
                style.text_align = TextAlign::Center;
                true
            }
            "justify" => {
                style.text_align = TextAlign::Justify;
                true
            }
            _ => false,
        },
        "text-indent" => set_px(value, own, root_px, &mut style.text_indent),
        "margin-top" => set_px(value, own, root_px, &mut style.margin_top),
        "margin-bottom" => set_px(value, own, root_px, &mut style.margin_bottom),
        "margin-left" => set_px(value, own, root_px, &mut style.margin_left),
        "margin-right" => set_px(value, own, root_px, &mut style.margin_right),
        "padding-top" => set_px(value, own, root_px, &mut style.padding_top),
        "padding-bottom" => set_px(value, own, root_px, &mut style.padding_bottom),
        "padding-left" => set_px(value, own, root_px, &mut style.padding_left),
        "padding-right" => set_px(value, own, root_px, &mut style.padding_right),
        "padding" => {
            let Some(px) = length_px(value, own, root_px) else {
                return false;
            };
            style.padding_top = px;
            style.padding_bottom = px;
            style.padding_left = px;
            style.padding_right = px;
            true
        }
        "margin" => {
            let Some(px) = length_px(value, own, root_px) else {
                return false;
            };
            style.margin_top = px;
            style.margin_bottom = px;
            style.margin_left = px;
            style.margin_right = px;
            true
        }
        "display" => match value.to_ascii_lowercase().as_str() {
            "block" => {
                style.display = Display::Block;
                true
            }
            "inline" => {
                style.display = Display::Inline;
                true
            }
            "inline-block" => {
                style.display = Display::InlineBlock;
                true
            }
            "list-item" => {
                style.display = Display::ListItem;
                true
            }
            "none" => {
                style.display = Display::None;
                true
            }
            _ => false,
        },
        "color" => match parse_color(value) {
            Some(c) => {
                style.color = Some(c);
                true
            }
            None => false,
        },
        "letter-spacing" => {
            if value.eq_ignore_ascii_case("normal") {
                style.letter_spacing = 0.0;
                return true;
            }
            set_px(value, own, root_px, &mut style.letter_spacing)
        }
        "white-space" => match value.to_ascii_lowercase().as_str() {
            "normal" => {
                style.white_space_pre = false;
                true
            }
            "pre" | "pre-wrap" => {
                style.white_space_pre = true;
                true
            }
            // nowrap / pre-line 一期不区分，按不支持计数而不是假装成功。
            _ => false,
        },
        "width" => set_length(value, own, root_px, &mut style.width),
        "height" => set_length(value, own, root_px, &mut style.height),
        "max-width" => set_length(value, own, root_px, &mut style.max_width),
        "max-height" => set_length(value, own, root_px, &mut style.max_height),
        _ => false,
    }
}

fn set_length(value: &str, font_size: f32, root_font_size: f32, out: &mut Option<Length>) -> bool {
    match parse_length(value, font_size, root_font_size) {
        Some(v) => {
            *out = v;
            true
        }
        None => false,
    }
}

/// 外层 `None` = 值不认识（计入 ignored）；内层 `None` = `auto` / `none`（清掉显式值）。
fn parse_length(value: &str, font_size: f32, root_font_size: f32) -> Option<Option<Length>> {
    let v = value.trim().to_ascii_lowercase();
    if v.is_empty() {
        return None;
    }
    if v == "auto" || v == "none" {
        return Some(None);
    }
    if let Some(percent) = v.strip_suffix('%') {
        let n: f32 = percent.trim().parse().ok()?;
        return Some(Some(Length::Percent(n / 100.0)));
    }
    length_px(&v, font_size, root_font_size).map(|px| Some(Length::Px(px)))
}

fn set_px(value: &str, font_size: f32, root_font_size: f32, out: &mut f32) -> bool {
    match length_px(value, font_size, root_font_size) {
        Some(px) => {
            *out = px;
            true
        }
        None => false,
    }
}

fn parse_font_size(value: &str, parent_px: f32, root_px: f32) -> Option<f32> {
    let v = value.to_ascii_lowercase();
    if let Some(percent) = v.strip_suffix('%') {
        let n: f32 = percent.trim().parse().ok()?;
        return Some(n / 100.0 * parent_px);
    }
    let keyword = match v.as_str() {
        "xx-small" => Some(9.0),
        "x-small" => Some(10.0),
        "small" => Some(13.0),
        "medium" => Some(16.0),
        "large" => Some(18.0),
        "x-large" => Some(24.0),
        "xx-large" => Some(32.0),
        "smaller" => Some(parent_px / 1.2),
        "larger" => Some(parent_px * 1.2),
        _ => None,
    };
    keyword.or_else(|| length_px(&v, parent_px, root_px))
}

/// `normal` 与无单位倍数直接当倍数；带单位的换算成 px 再除以**自己的**字号。
fn parse_line_height(value: &str, font_size: f32, root_px: f32) -> Option<f32> {
    let v = value.trim().to_ascii_lowercase();
    if v == "normal" {
        return Some(1.2);
    }
    if let Some(percent) = v.strip_suffix('%') {
        let n: f32 = percent.trim().parse().ok()?;
        return Some(n / 100.0);
    }
    if let Ok(multiplier) = v.parse::<f32>() {
        return Some(multiplier);
    }
    length_px(&v, font_size, root_px).map(|px| px / font_size)
}

fn parse_font_weight(value: &str) -> Option<u16> {
    match value.to_ascii_lowercase().as_str() {
        "normal" => Some(400),
        "bold" | "bolder" => Some(700),
        "lighter" => Some(300),
        other => other
            .parse::<f32>()
            .ok()
            .map(|n| n.clamp(1.0, 1000.0) as u16),
    }
}

/// 支持 px / em / rem / pt / 裸数字。`%` 不在这里：外边距和缩进的百分比
/// 要按**包含块的宽度**折算，那是排版期才知道的数，层叠期猜不出来，
/// 所以 `auto` / `calc()` / `%` 一律判为不支持（计入 ignored，值保持默认）。
fn length_px(value: &str, font_size: f32, root_font_size: f32) -> Option<f32> {
    let v = value.trim().to_ascii_lowercase();
    if v.is_empty() || v == "auto" || v.starts_with("calc(") || v.ends_with('%') {
        return None;
    }
    let (number, unit) = [("rem", "rem"), ("px", "px"), ("pt", "pt"), ("em", "em")]
        .into_iter()
        .find_map(|(suffix, unit)| v.strip_suffix(suffix).map(|n| (n, unit)))
        .unwrap_or((v.as_str(), "none"));
    let n: f32 = number.trim().parse().ok()?;
    Some(match unit {
        "px" | "none" => n,
        "pt" => n * 96.0 / 72.0,
        "em" => n * font_size,
        "rem" => n * root_font_size,
        _ => return None,
    })
}

fn parse_color(value: &str) -> Option<Color> {
    let v = value.trim().to_ascii_lowercase();
    if let Some(hex) = v.strip_prefix('#') {
        let parts: Vec<u32> = match hex.len() {
            3 => hex
                .chars()
                .filter_map(|c| u32::from_str_radix(&c.to_string(), 16).ok())
                .collect(),
            6 => (0..3)
                .filter_map(|i| u32::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
                .collect(),
            _ => return None,
        };
        if parts.len() != 3 {
            return None;
        }
        let scale = if hex.len() == 3 { 17 } else { 1 };
        return Some(Color(
            (parts[0] * scale) as u8,
            (parts[1] * scale) as u8,
            (parts[2] * scale) as u8,
        ));
    }
    if let Some(inner) = v.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        let parts: Vec<f32> = inner
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse::<f32>().ok())
            .collect();
        return if parts.len() >= 3 {
            Some(Color(parts[0] as u8, parts[1] as u8, parts[2] as u8))
        } else {
            None
        };
    }
    let named = match v.as_str() {
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "gray" | "grey" => (128, 128, 128),
        "silver" => (192, 192, 192),
        _ => return None,
    };
    Some(Color(named.0, named.1, named.2))
}

fn document_order(dom: &Dom) -> Vec<NodeId> {
    let mut out = Vec::with_capacity(dom.nodes.len());
    let mut stack = vec![dom.document];
    while let Some(id) = stack.pop() {
        out.push(id);
        let children = dom.get(id).children.clone();
        stack.extend(children.iter().rev().copied());
    }
    out
}
