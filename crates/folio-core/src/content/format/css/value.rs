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
    BackgroundAxis, BackgroundEdge, BackgroundPosition, BackgroundRepeat, BackgroundSize,
    BackgroundTile, BorderStyle, BreakWhen, Clear, Color, Float, Length, ListStyle, TextShadow,
    WritingMode,
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
///
/// `avoid` 也从这一条表里划掉，理由与上面几条不同，值得单独记：它从前被解成
/// [`BreakWhen`] 的一个变体存进 `ComputedStyle`，于是 `examples/css-gap` 报 `Applied`
/// （「这条接住了」），而排版层从来没有一处读它 —— `BreakWhen::is_forced` 明写
/// 「`Avoid` 不是强制断」，`layout::fragment` 除了 `is_forced` 与 `parity` 没有第三个
/// 消费者。那是把「没做」记成「做了」，与 `background-attachment` 留在
/// `KNOWN_UNIMPLEMENTED` 而不是在 `apply` 里返回 `true` 同一个理由。
/// 本机四本书（哈利·波特 294607 条、斗战狂潮 708826 条、蛇结 4963 条、合成书 31 条）
/// 里 `break-before: avoid` 与 `break-after: avoid` 各 **0 处**，所以这一改动不会
/// 让任何一本真书的落差数变坏；它只让那张表说实话。
/// 注意 `break-inside: avoid` 是**另一条路**，它落在 `ComputedStyle::break_inside_avoid`
/// 那个布尔上并真被分片器读，不受这里影响。
pub(super) fn parse_break_when(value: &str) -> Option<BreakWhen> {
    match value.to_ascii_lowercase().as_str() {
        "auto" => Some(BreakWhen::Auto),
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

/// `min-width` / `min-height` 的值：只收长度，`auto` 与 `none` 判不认识。
///
/// 有意**不走** [`set_length`]：那条把 `auto` / `none` 折成「清掉显式值」并返回成功，
/// 因为 `width` / `height` 里这两个词是合法值；而 CSS 2.1 的 `<min-width>` /
/// `<min-height>` 不收它们（`min-*: auto` 是 CSS Sizing 3 给弹性盒的写法）。认下来等于
/// 把一条浏览器会整条丢掉的声明报成 `Applied` —— 与 [`set_edge`] 那一条纪律同源。
pub(super) fn set_min_length(
    value: &str,
    font_size: f32,
    root_font_size: f32,
    out: &mut Option<Length>,
) -> bool {
    match edge_length(value, font_size, root_font_size, false) {
        Some(length) => {
            *out = Some(length);
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

/// 一条边的**长度**（`border-width` 那一族）：`auto` 在这一族里是非法值，
/// 所以它只是一层 [`length_px`] 的包装。
///
/// 从前这条式子还带一个 `auto_is_zero` 档位，用来把 `margin: auto` 当场折成 0。
/// 那一档已经搬进排版期（[`crate::layout::collect`] 的 `solve_block_width`），
/// 因为「剩余宽度均分」取决于作者的 `width` 折出来是多少，而那是排版期才知道的数 ——
/// 留着这个参数就等于留着第二个「`auto` 是什么」的定义。
pub(super) fn edge_px(value: &str, font_size: f32, root_font_size: f32) -> Option<f32> {
    length_px(value, font_size, root_font_size)
}

/// 一条边的**长度值**：与 [`edge_px`] 同一套单位口径，多收两种写法 —— 百分比与 `auto`。
///
/// 外边距、内边距与 `text-indent` 的百分比折算自包含块宽，那是排版期才有的数，
/// 所以这里存 `Length::Percent` 而不是当场折。这一族在真书上不是边角料：多看转换的
/// 《哈利·波特》每一段都写着 `margin: 0% 1% 0% 1%`，全书 86441 个元素身上挂着它。
///
/// `auto_ok` 为真时 `auto` 存成 [`Length::Auto`]（只有 `margin` 那一路给真）：
/// CSS 里 `padding: auto` 与 `text-indent: auto` 都是非法值，认下来就是把一条写坏的
/// 声明显示成「已处理」。
pub(super) fn edge_length(
    value: &str,
    font_size: f32,
    root_font_size: f32,
    auto_ok: bool,
) -> Option<Length> {
    let v = value.trim().to_ascii_lowercase();
    if auto_ok && v == "auto" {
        return Some(Length::Auto);
    }
    if let Some(percent) = v.strip_suffix('%') {
        let n: f32 = percent.trim().parse().ok()?;
        return Some(Length::Percent(n / 100.0));
    }
    length_px(&v, font_size, root_font_size).map(Length::Px)
}

/// 单边 `margin`：折成长度值，`auto` 收成 [`Length::Auto`]（均分留给排版期）。
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
/// 这种写错的书看起来「已经处理了」。`auto_ok` 决定这一族吃不吃 `auto`
/// （`margin` 吃、`padding` 不吃），见 [`edge_length`]。
pub(super) fn box_sides(
    value: &str,
    font_size: f32,
    root_font_size: f32,
    auto_ok: bool,
) -> Option<[Length; 4]> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut parsed = Vec::with_capacity(parts.len());
    for part in parts {
        parsed.push(edge_length(part, font_size, root_font_size, auto_ok)?);
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
        parsed.push(edge_px(part, font_size, root_font_size)?);
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

/// `border-radius` 简写：1–4 个长度/百分比，展开后的四个格子按 **CSS 的角序**
/// `[左上, 右上, 右下, 左下]` 解释。
///
/// 展开规则与 `padding` 共用 [`box_sides`]（1 值四角同、2 值对角同、3 值左下随右上、
/// 4 值逐角），所以这里只补这一族自己的两条限制：
/// - **负值整条作废**（CSS 里半径不许为负）；
/// - **椭圆的斜杠写法整条作废**（`border-radius: 10px / 20px` 要给每个角存横纵两套半径，
///   而 `[Length; 4]` 只有一套 —— 半接不接比不接更难查。本机四本语料 0 处）。
///
/// 四个长手（`border-top-left-radius` 等）暂时不收：它们会落进 `apply` 的 `border*`
/// 前缀分支，`border_target` 认出三段尾巴就判不认识。
pub(super) fn parse_border_radius(
    value: &str,
    font_size: f32,
    root_font_size: f32,
) -> Option<[Length; 4]> {
    let corners = box_sides(value, font_size, root_font_size, false)?;
    for corner in &corners {
        match corner {
            Length::Px(v) if *v < 0.0 => return None,
            Length::Percent(p) if *p < 0.0 => return None,
            Length::Auto => return None,
            _ => {}
        }
    }
    Some(corners)
}

/// `text-shadow`：逗号分隔的图层，每层 `<偏移x> <偏移y> [模糊半径] [颜色]`。
///
/// 颜色按 CSS 可以写在长度前后任意位置，所以这里不按位置切，而是逐 token 认：
/// 能解成颜色就归颜色，否则当长度。两条边界：
/// - `none` 给出**空表**（与「这条没写」在层叠上是两件事：前者要清掉继承来的影子）；
/// - `currentcolor` 存成 `None`，绘制层落本 run 的文字色 —— 那正是 CSS 的语义，
///   不需要一个哨兵值。
///
/// 模糊半径不许为负；`transparent` 这一档暂不收（[`Color`] 没有 alpha 通道，
/// 存成不透明黑会把影子画成一块实心，比承认没接更难查）。
pub(super) fn parse_text_shadow(
    value: &str,
    font_size: f32,
    root_font_size: f32,
) -> Option<Vec<TextShadow>> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Some(Vec::new());
    }
    let layers = crate::content::format::css::selector::split_top_level(value, ',');
    if layers.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(layers.len());
    for layer in layers {
        let mut lengths: Vec<f32> = Vec::new();
        let mut color = None;
        let mut saw_color = false;
        let tokens = split_value_tokens(layer.trim());
        if tokens.is_empty() {
            return None;
        }
        for token in tokens {
            if token.eq_ignore_ascii_case("currentcolor") {
                if saw_color {
                    return None;
                }
                saw_color = true;
                color = None;
                continue;
            }
            if let Some(rgba) = parse_color(&token) {
                if saw_color {
                    return None;
                }
                saw_color = true;
                color = Some(rgba);
                continue;
            }
            lengths.push(length_px(&token, font_size, root_font_size)?);
        }
        if lengths.len() < 2 || lengths.len() > 3 {
            return None;
        }
        let blur = lengths.get(2).copied().unwrap_or(0.0);
        if blur < 0.0 {
            return None;
        }
        out.push(TextShadow {
            dx: lengths[0],
            dy: lengths[1],
            blur,
            color,
        });
    }
    Some(out)
}

/// `border-style` 的取值。见 [`BorderStyle`]：只认四种，其余按没接住计数。
/// `border-style` 的取值。见 [`BorderStyle`]：只认六档，其余按没接住计数。
pub(super) fn parse_border_style(value: &str) -> Option<BorderStyle> {
    match value.to_ascii_lowercase().as_str() {
        "none" | "hidden" => Some(BorderStyle::None),
        "solid" => Some(BorderStyle::Solid),
        "double" => Some(BorderStyle::Double),
        "dotted" => Some(BorderStyle::Dotted),
        "dashed" => Some(BorderStyle::Dashed),
        "inset" => Some(BorderStyle::Inset),
        "outset" => Some(BorderStyle::Outset),
        "groove" => Some(BorderStyle::Groove),
        "ridge" => Some(BorderStyle::Ridge),
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
    edge_px(value, own, root_px)
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

/// 支持 px / pt / pc / in / cm / mm / em / rem / 裸数字。`%` 不在这一条里：要留百分比的
/// 族走 [`edge_length`] 与 [`parse_length`]，它们把 `%` 存成 `Length::Percent` 等排版期按
/// **包含块宽**折算，层叠期猜不出那个数。`auto` / `calc()` 在这里一律判为不支持
/// （计入 ignored，值保持默认）。
///
/// 绝对单位按 CSS 的 96 dpi 口径折：`@page` 的边距最常写成 `1.5cm` / `0.5in`。
pub(super) fn length_px(value: &str, font_size: f32, root_font_size: f32) -> Option<f32> {
    let v = value.trim().to_ascii_lowercase();
    if v.is_empty() || v == "auto" || v.starts_with("calc(") || v.ends_with('%') {
        return None;
    }
    // `rem` 必须排在 `em` 前面，否则 `1rem` 会被当成 `1r` + `em` 的后缀。
    // `ex` 排在 `em` 前面同理（`1ex` 不能被当成 `1` + `x`）。`ch` / `q` 还不收（语料 0 处）。
    let (number, unit) = [
        ("rem", "rem"),
        ("ex", "ex"),
        ("px", "px"),
        ("pt", "pt"),
        ("pc", "pc"),
        ("in", "in"),
        ("cm", "cm"),
        ("mm", "mm"),
        ("em", "em"),
    ]
    .into_iter()
    .find_map(|(suffix, unit)| v.strip_suffix(suffix).map(|n| (n, unit)))
    .unwrap_or((v.as_str(), "none"));
    let n: f32 = number.trim().parse().ok()?;
    Some(match unit {
        "px" | "none" => n,
        "pt" => n * 96.0 / 72.0,
        "pc" => n * 16.0,
        "in" => n * 96.0,
        "cm" => n * 96.0 / 2.54,
        "mm" => n * 96.0 / 25.4,
        "em" => n * font_size,
        "rem" => n * root_font_size,
        // `ex` 是当前字体的 x-height。拿不到真度量，按 `0.5em` 近似 —— 这也是
        // 多数无衬线体的实测值（The Riddle 那本书用 `padding-left: 7.5ex` 缩进诗行）。
        "ex" => n * font_size * 0.5,
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

// ---------------------------------------------------------------------------
// 背景图一族：`background-image` / `-size` / `-position` / `-repeat` 与 `background` 简写。
//
// 这一族的单位口径与别处有一处**有意的不同**：`background-size` 与 `background-position`
// 的百分比折算是按**这一块自己的盒**（以及盒减图的那段余量），不是按包含块宽。基准在排版期
// 才拿得到，所以这里一律留成 `Length` / 分数，不做任何折算 —— 与 `margin` 那一族同一个理由。
// ---------------------------------------------------------------------------

/// 把一段值按括号与引号外的空白切开。`url(a b.png)` 里的那个空格不是分隔符，
/// 真书里带空格的图名（`body italic.otf` 那一族）就踩在这一条上。
pub(super) fn split_value_tokens(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in value.chars() {
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
            '(' => {
                depth += 1;
                buf.push(c);
            }
            ')' => {
                depth -= 1;
                buf.push(c);
            }
            c if c.is_whitespace() && depth <= 0 => {
                if !buf.is_empty() {
                    out.push(std::mem::take(&mut buf));
                }
            }
            _ => buf.push(c),
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

/// 取出 `url("…")` / `url(…)` 里的路径并去引号；不是 `url()` 形态给 `None`。
///
/// 到这里路径**已经是容器内的绝对路径**了：样式表拼进层叠前 `absolutize_css_urls`
/// 做过折算并统一加了引号（见 `content::format`），所以这一格只管剥壳，不再解相对路径。
pub(super) fn parse_url_token(token: &str) -> Option<String> {
    let t = token.trim();
    // `URL(` / `Url(` 都算：函数名在 CSS 里大小写不敏感，而转换器两种都写得出来。
    // 走 `get` 而不是直接切片：值可能以全角字符开头，切在非字符边界上会当场 panic。
    if !t
        .get(..4)
        .is_some_and(|head| head.eq_ignore_ascii_case("url("))
    {
        return None;
    }
    let rest = &t[4..];
    let inner = rest.strip_suffix(')')?.trim();
    let path = inner.trim_matches(|c| c == '"' || c == '\'').trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// 一个铺法关键字。`repeat-x` / `repeat-y` 是**两轴一起写**的那两条，所以只在这里出现。
fn tile_axis(token: &str) -> Option<(BackgroundTile, BackgroundTile)> {
    Some(match token {
        "repeat" => (BackgroundTile::Repeat, BackgroundTile::Repeat),
        "no-repeat" => (BackgroundTile::NoRepeat, BackgroundTile::NoRepeat),
        "repeat-x" => (BackgroundTile::Repeat, BackgroundTile::NoRepeat),
        "repeat-y" => (BackgroundTile::NoRepeat, BackgroundTile::Repeat),
        "space" => (BackgroundTile::Space, BackgroundTile::Space),
        "round" => (BackgroundTile::Round, BackgroundTile::Round),
        _ => return None,
    })
}

/// `background-repeat`：一到两个值（先横后纵）。`repeat-x` / `repeat-y` 只能单独写。
pub(super) fn parse_background_repeat(value: &str) -> Option<BackgroundRepeat> {
    let parts = split_value_tokens(&value.to_ascii_lowercase());
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    if parts.len() == 1 {
        let (x, y) = tile_axis(parts[0].as_str())?;
        // `repeat-x` 那一档本身就已经把两轴写全了，不能再当「只说了横轴」用。
        return Some(BackgroundRepeat { x, y });
    }
    let one = |t: &str| match t {
        "repeat" => Some(BackgroundTile::Repeat),
        "no-repeat" => Some(BackgroundTile::NoRepeat),
        "space" => Some(BackgroundTile::Space),
        "round" => Some(BackgroundTile::Round),
        _ => None,
    };
    Some(BackgroundRepeat {
        x: one(&parts[0])?,
        y: one(&parts[1])?,
    })
}

/// `background-size`：`cover` / `contain` / `auto`，或一到两个长度·百分比·`auto`。
pub(super) fn parse_background_size(value: &str) -> Option<BackgroundSize> {
    let parts = split_value_tokens(value);
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    if parts.len() == 1 {
        let head = parts[0].to_ascii_lowercase();
        return match head.as_str() {
            "cover" => Some(BackgroundSize::Cover),
            "contain" => Some(BackgroundSize::Contain),
            "auto" => Some(BackgroundSize::Auto),
            // 单个长度或百分比：CSS 规定第二条就是 `auto`。
            _ => Some(BackgroundSize::Explicit {
                width: Some(size_axis(&head)?),
                height: None,
            }),
        };
    }
    let axis = |t: &str| -> Option<Option<Length>> {
        let low = t.to_ascii_lowercase();
        if low == "auto" {
            return Some(None);
        }
        Some(Some(size_axis(&low)?))
    };
    Some(BackgroundSize::Explicit {
        width: axis(&parts[0])?,
        height: axis(&parts[1])?,
    })
}

/// `background-size` 一轴上的长度或百分比。`auto` 由调用方挡在外面。
fn size_axis(token: &str) -> Option<Length> {
    if let Some(percent) = token.strip_suffix('%') {
        let n: f32 = percent.trim().parse().ok()?;
        return Some(Length::Percent(n / 100.0));
    }
    let low = token.to_ascii_lowercase();
    if low.is_empty() || low.starts_with("calc(") {
        return None;
    }
    let (number, unit) = [
        ("rem", "rem"),
        ("vw", "vw"),
        ("vh", "vh"),
        ("px", "px"),
        ("pt", "pt"),
        ("em", "em"),
    ]
    .into_iter()
    .find_map(|(suffix, unit)| low.strip_suffix(suffix).map(|n| (n, unit)))
    .unwrap_or((low.as_str(), "px"));
    let n: f32 = number.trim().parse().ok()?;
    Some(match unit {
        "px" => Length::Px(n),
        "pt" => Length::Px(n * 96.0 / 72.0),
        // `em` / `rem` 在这里按**自己的**字号与读者字号折 —— 与别处同一口径。
        // `vw` / `vh` 这一版不接：版心宽是界面给的数，层叠期拿不到（真书里这一族
        // 只出现在 `background-size: 100%`，视口单位一次也没见过）。
        "em" | "rem" | "vw" | "vh" => return None,
        _ => Length::Px(n),
    })
}

/// 位置一轴上的长度或百分比。
fn position_axis(token: &str) -> Option<BackgroundAxis> {
    if let Some(percent) = token.strip_suffix('%') {
        let n: f32 = percent.trim().parse().ok()?;
        return Some(BackgroundAxis::Fraction(n / 100.0));
    }
    let length = size_axis(token)?;
    Some(BackgroundAxis::Length(length))
}

/// `background-position`：一到两个值（先横后纵），关键字、长度、百分比可以混着写。
///
/// **四值边缘语法（`left 10px top 20px`）这一版不接**，整条判不认识：那一族要的是
/// 「两边各给一个边缘锚点再往里推」的状态机，而哈利波特那 23 处写到的只有
/// `center`、`center 60%`、`center 80%`、`bottom right`、`90% 14%` 这五档，
/// 两值那一档已经盖住全部 —— 为四值再写一套状态机，风险是把现在这些写法判错。
pub(super) fn parse_background_position(value: &str) -> Option<BackgroundPosition> {
    let parts = split_value_tokens(&value.to_ascii_lowercase());
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let center = || BackgroundAxis::Edge(BackgroundEdge::Center, None);
    // 「横轴关键字 + 一条偏移」的写法（`center 20px`、`center 80%`）：两个 token 一起说
    // **一根横轴**，纵轴取 `center`。偏移是从这个关键字那一边往里推，不是从起始边量。
    if parts.len() == 2
        && let Some((edge, PositionAxis::Horizontal)) = position_keyword(&parts[0])
        && let Some(offset) = size_axis(&parts[1])
    {
        return Some(BackgroundPosition {
            x: BackgroundAxis::Edge(edge, Some(offset)),
            y: center(),
        });
    }
    let mut x: Option<BackgroundAxis> = None;
    let mut y: Option<BackgroundAxis> = None;
    for token in &parts {
        // 逐个归位。规则就三条：`left`/`right` 只说横轴，`top`/`bottom` 只说纵轴，
        // 剩下的（数字与 `center`）先填横轴、横轴有了就填纵轴 —— 所以 `0 0` 是合法的
        // 「横 0、纵 0」，而 `left right` 因为横轴被占了两次而判非法。
        let axis = match position_keyword(token) {
            Some((edge, PositionAxis::Horizontal)) => {
                if x.is_some() {
                    return None;
                }
                x = Some(BackgroundAxis::Edge(edge, None));
                continue;
            }
            Some((edge, PositionAxis::Vertical)) => {
                if y.is_some() {
                    return None;
                }
                y = Some(BackgroundAxis::Edge(edge, None));
                continue;
            }
            Some((edge, PositionAxis::Either)) => {
                if x.is_none() {
                    x = Some(BackgroundAxis::Edge(edge, None));
                } else if y.is_none() {
                    y = Some(BackgroundAxis::Edge(edge, None));
                } else {
                    return None;
                }
                continue;
            }
            None => position_axis(token)?,
        };
        if x.is_none() {
            x = Some(axis);
        } else if y.is_none() {
            y = Some(axis);
        } else {
            return None;
        }
    }
    // 只给了横轴时纵轴取 `center`（CSS 明文），只写了 `top`/`bottom` 时横轴取 `center`。
    Some(BackgroundPosition {
        x: x.unwrap_or_else(center),
        y: y.unwrap_or_else(center),
    })
}

/// 一个位置关键字能说哪根轴。`center` 两根都能说，所以单独一档。
enum PositionAxis {
    Horizontal,
    Vertical,
    Either,
}

/// 一个位置关键字落在哪一边、说的是哪根轴。
fn position_keyword(token: &str) -> Option<(BackgroundEdge, PositionAxis)> {
    Some(match token {
        "left" => (BackgroundEdge::Start, PositionAxis::Horizontal),
        "right" => (BackgroundEdge::End, PositionAxis::Horizontal),
        "top" => (BackgroundEdge::Start, PositionAxis::Vertical),
        "bottom" => (BackgroundEdge::End, PositionAxis::Vertical),
        "center" => (BackgroundEdge::Center, PositionAxis::Either),
        _ => return None,
    })
}

/// `background-image` 的一层：`none` → `None`，`url("…")` → `Some(路径)`。
///
/// 渐变（`linear-gradient(...)`）与 `element()` 一律 `None` 让调用方判整条不认识 ——
/// 这一版不会画渐变，把它折成「没有图」等于把作者的底色块悄悄画空。
pub(super) fn parse_background_image_layer(token: &str) -> Option<Option<String>> {
    let t = token.trim();
    if t.eq_ignore_ascii_case("none") {
        return Some(None);
    }
    parse_url_token(t).map(Some)
}
