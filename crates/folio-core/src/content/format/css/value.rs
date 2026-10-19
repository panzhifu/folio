//! CSS 的值 → Rust 值：单位换算、关键字表、四边展开、颜色与字族的解析。
//!
//! 这一层**不认识 `ComputedStyle`**：每条式子拿的都是「这条声明的值 + 手头的基准
//! （自己的字号、根字号、父字号）」，产出一个 Rust 值或 `None`。`None` 之后怎么算由调用方
//! 决定 —— `computed::apply` 把它计进 ignored。反过来，「一条声明写进结构的哪几个字段」
//! 全在 `computed.rs`，含 `font` 与 `border` 这两个简写的派发。
//!
//! 单位口径集中记在这儿，因为它们是几处共用一套式子的原因：
//! - `rem` 一律按**读者偏好字号**（`root_px`），不让书自己的 `html { font-size }` 改全局。
//! - `em` 只在 `font-size` 里按**父**字号，其余长度按**自己**的字号 —— CSS 的规定。
//! - `%` 在 `font-size` / `line-height` / `vertical-align` 里当场能算（基准已知）；
//!   外边距、内边距与 `text-indent` 的百分比折算自**包含块宽度**，那是排版期才有的数，
//!   所以折不成 px 就存成 `Length::Percent`，由 `layout` 拿基准宽折算（见 [`edge_length`]）。
//! - 带单位的 `line-height` 折成倍数（除以**自己的**字号），代价是改字号时不重算。
//! - `pt` 按 96/72 折算，裸数字按 `px`。

use super::computed::{
    BorderStyle, BreakWhen, Clear, Color, Float, Length, ListStyle, WritingMode,
};

/// `list-style-type` 的关键字表。认不出的关键字返回 `None`（计进 ignored），
/// **不降级成 `disc`** —— 把作者要的 `hebrew` 画成一个黑点，比承认没做更难被发现。
///
/// `lower-latin` / `upper-latin` 是 CSS3 的别名，书里两种都见得到，一起收。
pub(super) fn parse_list_style(value: &str) -> Option<ListStyle> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "disc" => ListStyle::Disc,
        "circle" => ListStyle::Circle,
        "square" => ListStyle::Square,
        "decimal" => ListStyle::Decimal,
        "lower-alpha" | "lower-latin" => ListStyle::LowerAlpha,
        "upper-alpha" | "upper-latin" => ListStyle::UpperAlpha,
        "lower-roman" => ListStyle::LowerRoman,
        "upper-roman" => ListStyle::UpperRoman,
        "cjk-ideographic" => ListStyle::CjkIdeographic,
        "hiragana" => ListStyle::Hiragana,
        "hiragana-iroha" => ListStyle::HiraganaIroha,
        "katakana" => ListStyle::Katakana,
        "katakana-iroha" => ListStyle::KatakanaIroha,
        "none" => ListStyle::None,
        _ => return None,
    })
}

/// `float` 的取值。CSS 3 的 `inline-start` / `inline-end` / `float-inherit` 不接：
/// 那一族要的是方向敏感的左右，而出版书里真正出现的写法（插图、章首下沉）都写死
/// `left` / `right`。认不出就整条判不认识，不悄悄当 `none`。
pub(super) fn parse_float(value: &str) -> Option<Float> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "none" => Float::None,
        "left" => Float::Left,
        "right" => Float::Right,
        _ => return None,
    })
}

/// `clear` 的取值。`left` / `right` 真的按那一侧算，不都折成 `both`。
pub(super) fn parse_clear(value: &str) -> Option<Clear> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "none" => Clear::None,
        "left" => Clear::Left,
        "right" => Clear::Right,
        "both" => Clear::Both,
        _ => return None,
    })
}

/// `break-before` / `break-after` 的取值。
///
/// `recto` / `verso` / `avoid-column` 这些变体一期不认：按不支持计数，而不是悄悄
/// 当成 `auto` —— 后者会让作者写的意图静默消失，比不认识更难查。
pub(super) fn parse_break_when(value: &str) -> Option<BreakWhen> {
    match value.to_ascii_lowercase().as_str() {
        "auto" => Some(BreakWhen::Auto),
        "avoid" => Some(BreakWhen::Avoid),
        "page" => Some(BreakWhen::Page),
        "left" => Some(BreakWhen::Left),
        "right" => Some(BreakWhen::Right),
        "always" => Some(BreakWhen::Always),
        _ => None,
    }
}

/// `writing-mode` 的取值。
///
/// `sideways-rl` / `sideways-lr` 折进 `vertical-*` 那一两格：两者的差别**只在拉丁**
/// （sideways 连拉丁一起转 90°，vertical 让汉字直立、只转拉丁），而这台渲染器根本没有
/// 字形旋转通道（`Window::paint_glyph` 没有旋转参数），于是竖排里的拉丁本来就显式降级成
/// 「直立」—— 那一档降级见 `plans/README.md` M8。降级之后两档在屏上是同一个画面，
/// 所以合起来报 `Applied` 比按不认识计数更诚实：竖排的那一本照样竖，写法没被丢。
///
/// SVG 遗留的那一族（`tb-rl`、`tb-lr`、`lr`、`lr-tb`、`rl`）不接：本机书目里没出现过，
/// 而它们是「先转整页再转字」的另一套语义，猜错的症状比不猜难查。按不认识计数，
/// `examples/css-gap` 的表会告诉我们哪一本真用了它。
pub(super) fn parse_writing_mode(value: &str) -> Option<WritingMode> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "horizontal-tb" => WritingMode::HorizontalTb,
        "vertical-rl" | "sideways-rl" => WritingMode::VerticalRl,
        "vertical-lr" | "sideways-lr" => WritingMode::VerticalLr,
        _ => return None,
    })
}

/// `orphans` / `widows` 的取值：整数，`0` **夹到 1** 而不是判不认识。
///
/// CSS Fragmentation 的合法域是 `[1,∞)`，浏览器把 `0` 当无效；但《斗战狂潮》这类
/// 转换本会在 `body` 与 `p` 上写 `orphans: 0; widows: 0`，全书 138336 条 —— 作者的
/// 意思很清楚：「别管孤行寡行，随便切」。判不认识等于把它推回引擎默认的 2/2，
/// 症状是这一本的段落比我们自己的设置在页底多切两行。夹到 1 就是「不设约束」，
/// 与作者意图同向，也和 `hyphens: all` 那种「超出合法域就往最近的合法值夹」一致。
/// 负数与折不出整数的仍然判不认识：那不是约束松紧，是写坏了。
pub(super) fn parse_count(value: &str) -> Option<u16> {
    let n: i64 = value.trim().parse().ok()?;
    (n >= 0).then(|| n.clamp(1, u16::MAX as i64) as u16)
}

/// 上标抬多高、下标压多低。这两个数**是我们定的**，不是浏览器的实现值：
/// 这台机器上没有浏览器可量，抄一个「大家都这么写」的常数等于把猜测写成标准。
/// 三分之一 em / 四分之一 em 是中文排版里注号常见的观感尺度，且与字号成比例，
/// 读者改字号时偏移跟着走。
const SUPER_SHIFT_EM: f32 = 1.0 / 3.0;
const SUB_SHIFT_EM: f32 = 0.25;

/// 上标的位移，按**自己的**字号。UA 表给 `<sup>` / `<sub>` 用的就是这两个式子，
/// 与作者写 `vertical-align: super` 走的是同一条路 —— 两条路径给不同的数，
/// 就意味着「带 class 的上标」和「裸 `<sup>`」排出来不一样高。
pub(super) fn super_shift(font_size: f32) -> f32 {
    font_size * SUPER_SHIFT_EM
}

/// 下标的位移（负数 = 往下）。见 [`super_shift`]。
pub(super) fn sub_shift(font_size: f32) -> f32 {
    -font_size * SUB_SHIFT_EM
}

/// `vertical-align` 折成基线偏移（px，正数往上）。
///
/// - `baseline` → 0，也就是「不偏移」，与初值同义，所以它算接住了。
/// - `super` / `sub` → 见上面那两个常数的说明。
/// - `<length>` → 直接按长度；`<percentage>` → 按 CSS 相对**自己的 line-height**
///   （`line_height` 是倍数，所以基准是 `font_size × line_height`）。
/// - `top` / `bottom` / `middle` / `text-top` / `text-bottom` → 返回 `None`：
///   它们要的是行盒对齐机制（拿整行的 tallest box 对齐），悄悄当 0 会把作者的
///   表格基线写成正常位置还查不出来。
pub(super) fn parse_baseline_shift(
    value: &str,
    own: f32,
    root_px: f32,
    line_height: f32,
) -> Option<f32> {
    let v = value.trim().to_ascii_lowercase();
    match v.as_str() {
        "baseline" => Some(0.0),
        "super" => Some(super_shift(own)),
        "sub" => Some(sub_shift(own)),
        "top" | "bottom" | "middle" | "text-top" | "text-bottom" => None,
        _ => {
            if let Some(percent) = v.strip_suffix('%') {
                let n: f32 = percent.trim().parse().ok()?;
                return Some(n / 100.0 * own * line_height);
            }
            length_px(&v, own, root_px)
        }
    }
}

/// `text-decoration` / `text-decoration-line` → `(下划线, 删除线)`。
///
/// 简写在 CSS 里还能带线型与颜色（`underline wavy #f00`）。那些 token **认得但不改变
/// 画法**：一期一律按当前文字颜色画 1px 实线。`overline` 与 `blink` 整条不接 ——
/// 画不出来的东西不该在统计里显示成「接住了」。
pub(super) fn parse_decoration(value: &str) -> Option<(bool, bool)> {
    let mut underline = false;
    let mut through = false;
    for token in value.split_whitespace() {
        match token.to_ascii_lowercase().as_str() {
            "underline" => underline = true,
            "line-through" => through = true,
            "none" => return Some((false, false)),
            "overline" | "blink" => return None,
            // 线型、厚度、`from-font`：认得，画法不变。
            "solid" | "double" | "dotted" | "dashed" | "wavy" | "thick" | "thin" | "from-font" => {}
            other => {
                // 颜色可以，别的都不行 —— 猜一个作者没写的线型比丢掉这条更糟。
                if parse_color(other).is_none() && !other.starts_with('#') {
                    return None;
                }
            }
        }
    }
    // 空值（或只剩空白）不算接住：`text-decoration: ;` 那种写坏的声明要有痕迹。
    value.split_whitespace().next()?;
    Some((underline, through))
}

/// `font-family` 的值 → 候选链：逗号分组、去引号、丢空项，顺序就是作者的优先级。
///
/// 逗号要在去引号**之前**切：`"A, B", serif` 里那个逗号属于第一个家族名，
/// 先去引号会把它切成 `"A` 与 ` B"` 两半。`split_top_level` 认引号，所以交给它。
/// 链整个交给平台层逐个试（只有它知道这台设备注册了哪些家族）。
pub(super) fn parse_font_family_list(value: &str) -> Vec<String> {
    crate::content::format::css::selector::split_top_level(value, ',')
        .into_iter()
        .map(|name| name.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

pub(super) fn set_length(
    value: &str,
    font_size: f32,
    root_font_size: f32,
    out: &mut Option<Length>,
) -> bool {
    match parse_length(value, font_size, root_font_size) {
        Some(v) => {
            *out = v;
            true
        }
        None => false,
    }
}

/// 外层 `None` = 值不认识（计入 ignored）；内层 `None` = `auto` / `none`（清掉显式值）。
pub(super) fn parse_length(
    value: &str,
    font_size: f32,
    root_font_size: f32,
) -> Option<Option<Length>> {
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

pub(super) fn set_px(value: &str, font_size: f32, root_font_size: f32, out: &mut f32) -> bool {
    match length_px(value, font_size, root_font_size) {
        Some(px) => {
            *out = px;
            true
        }
        None => false,
    }
}

/// 一条边的长度。`auto_in_zero` 为真时 `auto` 折成 0，而不是当不认识。
///
/// 为什么 `auto` 取 0 不是降级而是当前盒模型下的正确结果：CSS 里块级盒的
/// `auto` 前后边距是「均分剩余宽度」，而我们的正文块使用宽度就等于可用宽度
/// （`layout` 里 `content_width = width − padding − margin`，块级不吃 `max-width`
/// 的收窄），剩余宽度恒为 0。等排版层真按 `max-width` 收窄正文时，这里要改成均分，
/// 块级居中才会出现 —— 那一条改动的位置就在这儿，不在别处。
///
/// 但**不能整条作废**：`margin: 5px auto` 是书里真实出现的写法（这本《蛇结》就有），
/// 拒掉它等于连那 5px 的上下边距一起丢，那才是把作者意图静默抹掉。
pub(super) fn edge_px(
    value: &str,
    font_size: f32,
    root_font_size: f32,
    auto_is_zero: bool,
) -> Option<f32> {
    if auto_is_zero && value.eq_ignore_ascii_case("auto") {
        return Some(0.0);
    }
    length_px(value, font_size, root_font_size)
}

/// 一条边的**长度值**：与 [`edge_px`] 同一套单位口径，多收一种写法 —— 百分比。
///
/// 外边距、内边距与 `text-indent` 的百分比折算自包含块宽，那是排版期才有的数，
/// 所以这里存 `Length::Percent` 而不是当场折。这一族在真书上不是边角料：多看转换的
/// 《哈利·波特》每一段都写着 `margin: 0% 1% 0% 1%`，全书 86441 个元素身上挂着它。
pub(super) fn edge_length(
    value: &str,
    font_size: f32,
    root_font_size: f32,
    auto_is_zero: bool,
) -> Option<Length> {
    let v = value.trim().to_ascii_lowercase();
    if auto_is_zero && v == "auto" {
        return Some(Length::Px(0.0));
    }
    if let Some(percent) = v.strip_suffix('%') {
        let n: f32 = percent.trim().parse().ok()?;
        return Some(Length::Percent(n / 100.0));
    }
    length_px(&v, font_size, root_font_size).map(Length::Px)
}

/// 单边 `margin`：折成长度值，`auto` 当 0（`margin` 认 `auto`）。
pub(super) fn set_margin(
    value: &str,
    font_size: f32,
    root_font_size: f32,
    out: &mut Length,
) -> bool {
    match edge_length(value, font_size, root_font_size, true) {
        Some(px) => {
            *out = px;
            true
        }
        None => false,
    }
}

/// 单边 `padding` / `text-indent`：折成长度值，`auto` 不算接住（CSS 里这两个都不吃）。
pub(super) fn set_edge(value: &str, font_size: f32, root_font_size: f32, out: &mut Length) -> bool {
    match edge_length(value, font_size, root_font_size, false) {
        Some(px) => {
            *out = px;
            true
        }
        None => false,
    }
}

/// `margin` / `padding` 简写的四边展开，返回 `[上, 右, 下, 左]` 的**长度值**。
///
/// 分配的规则是 CSS 的：1 值四边相同；2 值「上下 左右」；3 值「上 左右 下」；
/// 4 值顺时针。值数不对（0 个或 5 个以上）或任一个解析失败，**整条作废** ——
/// 半套边距比没边距更难解释，而且静默只吃前两个值会让 `margin: 1px 2px 3px 4px 5px`
/// 这种写错的书看起来「已经处理了」。
pub(super) fn box_sides(
    value: &str,
    font_size: f32,
    root_font_size: f32,
    auto_is_zero: bool,
) -> Option<[Length; 4]> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut parsed = Vec::with_capacity(parts.len());
    for part in parts {
        parsed.push(edge_length(part, font_size, root_font_size, auto_is_zero)?);
    }
    four_of(&parsed)
}

/// [`box_sides`] 的 px 版，给 `border-width`：CSS 的 `<border-width>` 不接受百分比，
/// 所以这一族继续走 [`edge_px`]，折不出就整条作废。
pub(super) fn box_side_px(value: &str, font_size: f32, root_font_size: f32) -> Option<[f32; 4]> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut parsed = Vec::with_capacity(parts.len());
    for part in parts {
        parsed.push(edge_px(part, font_size, root_font_size, false)?);
    }
    four_of(&parsed)
}

/// 1/2/3/4 值的四边展开，顺序 `[上, 右, 下, 左]` —— 分配规则与 `box_sides` 相同，
/// 但值的类型不限（边框样式与颜色也要这一套）。
pub(super) fn four_of<T: Copy>(values: &[T]) -> Option<[T; 4]> {
    let top = *values.first()?;
    let right = values.get(1).copied().unwrap_or(top);
    let bottom = values.get(2).copied().unwrap_or(top);
    let left = values.get(3).copied().unwrap_or(right);
    Some([top, right, bottom, left])
}

/// `border-style` 的取值。见 [`BorderStyle`]：只认四种，其余按没接住计数。
pub(super) fn parse_border_style(value: &str) -> Option<BorderStyle> {
    match value.to_ascii_lowercase().as_str() {
        "none" | "hidden" => Some(BorderStyle::None),
        "solid" => Some(BorderStyle::Solid),
        "double" => Some(BorderStyle::Double),
        _ => None,
    }
}

/// `border-width` 的取值：CSS 的三个关键字，或任何普通长度。
pub(super) fn border_width_px(value: &str, own: f32, root_px: f32) -> Option<f32> {
    match value.to_ascii_lowercase().as_str() {
        "thin" => return Some(1.0),
        "medium" => return Some(3.0),
        "thick" => return Some(5.0),
        _ => {}
    }
    edge_px(value, own, root_px, false)
}

/// `border` 一族里一条声明指向哪儿：哪几条边、给三要素中的哪一个。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BorderElement {
    /// 简写：一条声明同时给宽度、样式、颜色。
    All,
    Width,
    Style,
    Color,
}

/// 边名的顺序与 `box_sides` 一致：`[上, 右, 下, 左]`。
const BORDER_SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

/// 把属性名拆成 (哪条边, 哪个要素)。
///
/// 认得：`border`、`border-{四边}`、`border-{width,style,color}`、`border-{四边}-{width,style,color}`。
/// 认不得的 `border-*`（`border-radius`、`border-image`、`border-top-left-radius`、
/// `border-spacing`）返回 `None`，由调用方按「没接住」计数 —— 把圆角悄悄画成直角，
/// 比承认没做更难被发现。
pub(super) fn border_target(property: &str) -> Option<(Option<usize>, BorderElement)> {
    let rest = property.strip_prefix("border")?;
    if rest.is_empty() {
        return Some((None, BorderElement::All));
    }
    let tail = rest.strip_prefix('-')?;
    let mut bits = tail.split('-');
    let first = bits.next()?;
    let second = bits.next();
    if bits.next().is_some() {
        return None;
    }
    let side = BORDER_SIDES.iter().position(|name| *name == first);
    match (side, second) {
        // `border-top` / `border-top-width` / …
        (Some(ix), None) => Some((Some(ix), BorderElement::All)),
        (Some(ix), Some(element)) => Some((Some(ix), border_element(element)?)),
        // `border-width` / `border-style` / `border-color`：没有边名，first 就是要素名。
        (None, None) => Some((None, border_element(first)?)),
        (None, Some(element)) => Some((None, border_element(element)?)),
    }
}

pub(super) fn border_element(word: &str) -> Option<BorderElement> {
    match word.to_ascii_lowercase().as_str() {
        "width" => Some(BorderElement::Width),
        "style" => Some(BorderElement::Style),
        "color" => Some(BorderElement::Color),
        _ => None,
    }
}

pub(super) fn parse_font_size(value: &str, parent_px: f32, root_px: f32) -> Option<f32> {
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
pub(super) fn parse_line_height(value: &str, font_size: f32, root_px: f32) -> Option<f32> {
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

/// 字重：数字 1–1000，或 `normal` / `bold` 那三个关键字。
/// `@font-face` 的 `font-weight` 描述符走同一个式子，不另定一套（`css::RuleSet::parse`）。
pub(crate) fn parse_font_weight(value: &str) -> Option<u16> {
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

/// `bolder` / `lighter` 相对**父字重**查表。这两行数字是 CSS 2.1 §15.6 那张表的
/// bolder / lighter 两列（父 100…900 逐档），不是「往上/往下走一档」的直觉式子：
/// 父 400 的 `bolder` 直接到 700 而不是 500，父 400 的 `lighter` 到 100 而不是 300。
/// 按直觉各走一档会把强调做得太软，父 400 的 `<strong>` 类写法在书里到处都是。
///
/// 父值先归到最近的百位档再查（`font-weight: 450` 这种写法真实存在），两端夹住。
pub(super) fn relative_weight(parent: u16, bolder: bool) -> u16 {
    // 下标 = 档位 / 100，第 0 位只是让下标从 1 起对得上。
    const BOLDER: [u16; 10] = [400, 400, 400, 400, 700, 700, 800, 900, 900, 900];
    const LIGHTER: [u16; 10] = [100, 100, 100, 100, 100, 300, 400, 500, 700, 700];
    let bucket = ((parent as usize + 50) / 100).clamp(1, 9);
    if bolder {
        BOLDER[bucket]
    } else {
        LIGHTER[bucket]
    }
}

/// 支持 px / em / rem / pt / 裸数字。`%` 不在这一条里：要留百分比的族走
/// [`edge_length`] 与 [`parse_length`]，它们把 `%` 存成 `Length::Percent` 等排版期按
/// **包含块宽**折算，层叠期猜不出那个数。`auto` / `calc()` 在这里一律判为不支持
/// （计入 ignored，值保持默认）。
pub(super) fn length_px(value: &str, font_size: f32, root_font_size: f32) -> Option<f32> {
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

pub(super) fn parse_color(value: &str) -> Option<Color> {
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
