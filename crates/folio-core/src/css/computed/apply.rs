//! 一条声明如何变成一个计算值：属性分发、简写展开、继承、边框。
//!
//! 从 `css::computed` 切出来只因为那台属性分发的 switch 本身就六百行；类型定义与层叠
//! 入口留在父模块，这里的东西只对整个 `css` 内部开放，不属于对外公开面。

use super::{
    BorderEdge, BorderStyle, ComputedStyle, Display, Hyphens, KNOWN_UNIMPLEMENTED, TextAlign,
};
use crate::css::Declaration;
use crate::css::value::{
    BorderElement, border_target, border_width_px, box_sides, four_of, parse_baseline_shift,
    parse_border_style, parse_break_when, parse_color, parse_count, parse_decoration,
    parse_font_family_list, parse_font_size, parse_font_weight, parse_line_height, relative_weight,
    set_length, set_margin, set_px,
};

/// 按「font-size → line-height → 其余」三批算完一组已排好序的声明，返回没接住的条数。
///
/// 分批而不是照书写顺序：`em` / `%` 都按**元素自己的**字号折算，
/// 若照书写顺序，`text-indent: 2em; font-size: 20px` 会按继承字号算错一档。
/// 同一批内部仍严格按传进来的层叠顺序。
pub(super) fn apply_in_cascade_order(
    style: &mut ComputedStyle,
    parent: &ComputedStyle,
    root_px: f32,
    decls: &[&Declaration],
) -> usize {
    let mut ignored = 0usize;
    for group in 0..3 {
        for decl in decls {
            let is_font_size = decl.property == "font-size";
            let is_line_height = decl.property == "line-height";
            let mine = match group {
                0 => is_font_size,
                1 => is_line_height,
                _ => !is_font_size && !is_line_height,
            };
            if mine && !apply(style, parent, root_px, decl) {
                ignored += 1;
            }
        }
    }
    ignored
}

/// 元素的 `style="…"` 属性 → 声明列表，按书写顺序（同一属性后面那条盖前面那条）。
///
/// 分号落在 `url(…)` 或引号里时不是分隔符，所以复用样式表那一份 `split_top_level`；
/// 单条声明也复用 `css::parse_declaration` —— 行内写法与样式表写法对 `!important`、
/// 大小写、空值的处理必须一字不差，否则同一句话写在两处结果不同。
/// 认不出的整条丢掉，由调用方在 `apply` 那一步按「不认识」计数。
pub(super) fn parse_inline_style(text: &str) -> Vec<Declaration> {
    crate::css::selector::split_top_level(text, ';')
        .into_iter()
        .filter_map(|decl| crate::css::parse_declaration(&decl))
        .collect()
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
        font_families: parent.font_families.clone(),
        text_align: parent.text_align,
        color: parent.color,
        letter_spacing: parent.letter_spacing,
        text_indent: parent.text_indent,
        word_spacing: parent.word_spacing,
        // `hyphens` 在 CSS 里可继承，写法与 `word_spacing` 同一档：书里通常在
        // `<body>` 或 `html` 上声明一次「这本书不许在连字符处断行」。
        hyphens: parent.hyphens,
        // 装饰线在 CSS 里**不是继承属性**，但会向后代**传播**（`text-decoration: none`
        // 写在子元素上并不会去掉父元素的下划线，这是那条规则最有名的坑）。对我们的
        // 行内/块级盒来说，「传播」与「继承」算出来的结果一致，所以这里按继承实现，
        // 差别只留在原子行内盒（inline-block、图片）上 —— 那种盒一期没有内容会吃到
        // 父级装饰，见 `plans/facts.md`。
        underline: parent.underline,
        line_through: parent.line_through,
        // `vertical-align` **不**继承（CSS 规定），所以 `baseline_shift` 不在这里出现：
        // 上标只该抬高它自己那一小段，而不是让整段正文跟着上浮。
        // orphans / widows 在 CSS 里是**可继承**的（分片属性里只有这两个继承），
        // 所以书里在 `<body>` 上写一次就够；`break-*` 不继承，它们是「这一块怎么断」
        // 而不是「这一带的默认值」。
        orphans: parent.orphans,
        widows: parent.widows,
        ..Default::default()
    }
}

/// `font` 简写：`[[style] [weight] size [/ line-height] family]`。
///
/// **有意不重置没写到的子属性。** CSS 规定 `font` 会把 `font-variant`、`font-stretch`、
/// `line-height`、`font-size`… 一律拉回初值，而我们的行距与字重是通过 `root_base`
/// 从读者偏好走进继承链的：一句 `font: 1em serif` 把读者的 1.75 行距改回 `normal`
/// 是产品级的错，不是排版细节。除此之外按 CSS：字号支持 `/行距`，
/// 字族取整条候选链并去引号（与 `font-family` 共用一个解析器）。
/// `small-caps` / `condensed` / `caption` 这类 token 我们不认，整条作废并计 `ignored`。
fn apply_font_shorthand(
    style: &mut ComputedStyle,
    parent: &ComputedStyle,
    root_px: f32,
    value: &str,
) -> bool {
    if value.eq_ignore_ascii_case("initial") || value.eq_ignore_ascii_case("unset") {
        return false;
    }
    let parts: Vec<&str> = value.split_whitespace().collect();
    // 字号是这条声明里唯一的必需项，也是「前面是 style/weight、后面是 family」的分界。
    let Some(at) = parts.iter().position(|part| {
        let head = part.split('/').next().unwrap_or(part);
        parse_font_size(head, parent.font_size, root_px).is_some()
    }) else {
        return false;
    };
    for part in &parts[..at] {
        let token = part.to_ascii_lowercase();
        match token.as_str() {
            "normal" => {}
            "italic" | "oblique" => style.font_italic = true,
            _ => {
                let Some(weight) = parse_font_weight(&token) else {
                    return false;
                };
                style.font_weight = weight;
            }
        }
    }
    let size_token = parts[at];
    let (size_part, leading) = match size_token.split_once('/') {
        Some((size, leading)) => (size, Some(leading)),
        None => (size_token, None),
    };
    let Some(px) = parse_font_size(size_part, parent.font_size, root_px) else {
        return false;
    };
    style.font_size = px;
    if let Some(leading) = leading {
        let Some(lh) = parse_line_height(leading, px, root_px) else {
            return false;
        };
        style.line_height = lh;
    }
    // family 可能带空格（`"Times New Roman", serif`），所以把剩下的 token 重新拼回去。
    let family = parts[at + 1..].join(" ");
    let list = parse_font_family_list(&family);
    let Some(first) = list.first().cloned() else {
        return false;
    };
    style.font_family = first;
    style.font_families = list;
    true
}

/// 返回 false 表示这条声明不认识（或值解析失败），由调用方计入 ignored。
///
/// `root_px` 是根元素（读者偏好）的字号，rem 只按它折算；`parent` 提供 font-size 的
/// 相对基准 —— `em` 在 `font-size` 里按父字号，在别处按自己的字号，这是 CSS 的规定，
/// 不是我们的选择。`break-before` / `break-after` 与它们的 CSS2.1 同名前缀版本
/// （`page-break-*`）共用一条解析。
pub(super) fn apply(
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
        "font-weight" => {
            // `bolder` / `lighter` 是**相对**父字重的，不是两个固定档位：`<strong>` 套在
            // 一段已经 700 的标题里应当变成 900，写成常量就等于全书所有嵌套强调同粗。
            // 这两个词在这里截走，其余仍走 `parse_font_weight`（`@font-face` 的描述符共用它）。
            let value = value.to_ascii_lowercase();
            let weight = match value.as_str() {
                "bolder" => Some(relative_weight(parent.font_weight, true)),
                "lighter" => Some(relative_weight(parent.font_weight, false)),
                _ => parse_font_weight(&value),
            };
            match weight {
                Some(w) => {
                    style.font_weight = w;
                    true
                }
                None => false,
            }
        }
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
            let list = parse_font_family_list(value);
            let Some(first) = list.first().cloned() else {
                return false;
            };
            style.font_family = first;
            style.font_families = list;
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
        // 词距：CSS 初值 `normal` 折成 0，其余按长度。它可继承（`inherit_from` 里带着）。
        "word-spacing" => {
            if value.eq_ignore_ascii_case("normal") {
                style.word_spacing = 0.0;
                return true;
            }
            set_px(value, own, root_px, &mut style.word_spacing)
        }
        // 连字符。EPUB3 的样式表里三条拼法都见得到（`-webkit-` 是历史包袱，
        // `-epub-` 是 EPUB 风格规范点名的那条），所以一起认，别把认得的写法
        // 报成「不认识」。
        "hyphens" | "-webkit-hyphens" | "-epub-hyphens" | "-moz-hyphens" => {
            match value.to_ascii_lowercase().as_str() {
                "manual" => {
                    style.hyphens = Hyphens::Manual;
                    true
                }
                "none" => {
                    style.hyphens = Hyphens::None;
                    true
                }
                "auto" => {
                    style.hyphens = Hyphens::Auto;
                    true
                }
                _ => false,
            }
        }
        "vertical-align" => match parse_baseline_shift(value, own, root_px, style.line_height) {
            Some(px) => {
                style.baseline_shift = px;
                true
            }
            None => false,
        },
        "text-decoration" | "text-decoration-line" => match parse_decoration(value) {
            Some((underline, line_through)) => {
                style.underline = underline;
                style.line_through = line_through;
                true
            }
            None => false,
        },
        "font" => apply_font_shorthand(style, parent, root_px, value),
        "break-before" | "page-break-before" => match parse_break_when(value) {
            Some(when) => {
                style.break_before = when;
                true
            }
            None => false,
        },
        "break-after" | "page-break-after" => match parse_break_when(value) {
            Some(when) => {
                style.break_after = when;
                true
            }
            None => false,
        },
        "break-inside" | "page-break-inside" => {
            // 两个值都是「设定」而不是「查询」：`auto` 显式取消前面的 `avoid`
            // （同一份样式表里先 `avoid` 后 `auto` 是合法写法）。认不出的值不改任何东西。
            let avoid = value.eq_ignore_ascii_case("avoid");
            let auto = value.eq_ignore_ascii_case("auto");
            if avoid {
                style.break_inside_avoid = true;
            }
            if auto {
                style.break_inside_avoid = false;
            }
            avoid || auto
        }
        "orphans" => match parse_count(value) {
            Some(n) => {
                style.orphans = n;
                true
            }
            None => false,
        },
        "widows" => match parse_count(value) {
            Some(n) => {
                style.widows = n;
                true
            }
            None => false,
        },
        "margin-top" => set_margin(value, own, root_px, &mut style.margin_top),
        "margin-bottom" => set_margin(value, own, root_px, &mut style.margin_bottom),
        // 左右边的 `auto` 折成 0，理由见 `edge_px`：当前盒模型里这就是 CSS 的结果。
        "margin-left" => set_margin(value, own, root_px, &mut style.margin_left),
        "margin-right" => set_margin(value, own, root_px, &mut style.margin_right),
        "padding-top" => set_px(value, own, root_px, &mut style.padding_top),
        "padding-bottom" => set_px(value, own, root_px, &mut style.padding_bottom),
        "padding-left" => set_px(value, own, root_px, &mut style.padding_left),
        "padding-right" => set_px(value, own, root_px, &mut style.padding_right),
        // 两个四边简写。`padding` 不吃 `auto`（CSS 里 `padding: auto` 是非法值），
        // `margin` 吃 —— 差别只在 `box_sides` 的那一个参数上，展开逻辑共用一份。
        "padding" => match box_sides(value, own, root_px, false) {
            Some([top, right, bottom, left]) => {
                style.padding_top = top;
                style.padding_right = right;
                style.padding_bottom = bottom;
                style.padding_left = left;
                true
            }
            None => false,
        },
        "margin" => match box_sides(value, own, root_px, true) {
            Some([top, right, bottom, left]) => {
                style.margin_top = top;
                style.margin_right = right;
                style.margin_bottom = bottom;
                style.margin_left = left;
                true
            }
            None => false,
        },
        // 背景色。`transparent` 是「有这条声明、但不画」，与「书里没写过」在层叠上
        // 是两件事（后者可能被更高 specificity 的一条颜色盖住），所以两条都要认。
        "background-color" => {
            if value.eq_ignore_ascii_case("transparent") {
                style.background_color = None;
                return true;
            }
            match parse_color(value) {
                Some(color) => {
                    style.background_color = Some(color);
                    true
                }
                None => false,
            }
        }
        // 边框一族（`border` / `border-top` / `border-left-width` / `border-color` …）
        // 用一条前缀分支收：三要素 × 四边写开是二十个 match 臂，而解析规则只有一条。
        name if name.starts_with("border") => apply_border(style, name, value, own, root_px),
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

/// 边框一族的一条声明 → 写进 `style.border`。入口是 [`apply`] 里那条 `border` 前缀分支。
fn apply_border(
    style: &mut ComputedStyle,
    property: &str,
    value: &str,
    own: f32,
    root_px: f32,
) -> bool {
    let Some((side, element)) = border_target(property) else {
        return false;
    };
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.is_empty() {
        return false;
    }
    // `border-top-width: 1px 2px` 这种是错的：边已经定死一条，值只能有一个。
    if side.is_some() && element != BorderElement::All && parts.len() != 1 {
        return false;
    }
    // 这条声明落到哪几条边上（`None` = 四条都要）。
    let targets: [usize; 4] = match side {
        Some(ix) => [ix, ix, ix, ix],
        None => [0, 1, 2, 3],
    };
    match element {
        // 简写把三要素**全部**写一遍：缺的那一项按 CSS 初值重置，而不是保持原样。
        // 书里 `border: 2px` 因此等于「宽 2px、样式 none、颜色 currentColor」，
        // 而样式是 none 就没有边可画（`BorderEdge::drawn` 随之给 0）—— 这是那条规则
        // 最有名的坑，照抄比「看起来更合理」的保留原样要对。
        BorderElement::All => {
            let mut width = 3.0; // `medium`
            let mut line = BorderStyle::None;
            let mut color = None;
            for part in &parts {
                if let Some(px) = border_width_px(part, own, root_px) {
                    width = px;
                } else if let Some(kind) = parse_border_style(part) {
                    line = kind;
                } else if part.eq_ignore_ascii_case("transparent") {
                    color = None;
                } else if let Some(parsed) = parse_color(part) {
                    color = Some(parsed);
                } else {
                    return false;
                }
            }
            for ix in targets {
                style.border[ix] = BorderEdge {
                    width,
                    style: line,
                    color,
                };
            }
            true
        }
        BorderElement::Width => match box_sides(value, own, root_px, false) {
            Some(each) => {
                for ix in targets {
                    style.border[ix].width = each[ix];
                }
                true
            }
            None => false,
        },
        BorderElement::Style => {
            let mut each = Vec::with_capacity(parts.len());
            for part in &parts {
                match parse_border_style(part) {
                    Some(kind) => each.push(kind),
                    None => return false,
                }
            }
            let Some(sides) = four_of(&each) else {
                return false;
            };
            for ix in targets {
                style.border[ix].style = sides[ix];
            }
            true
        }
        BorderElement::Color => {
            let mut each = Vec::with_capacity(parts.len());
            for part in &parts {
                if part.eq_ignore_ascii_case("transparent") {
                    each.push(None);
                } else {
                    match parse_color(part) {
                        Some(color) => each.push(Some(color)),
                        None => return false,
                    }
                }
            }
            let Some(sides) = four_of(&each) else {
                return false;
            };
            for ix in targets {
                style.border[ix].color = sides[ix];
            }
            true
        }
    }
}
