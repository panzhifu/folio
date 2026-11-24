//! 属性值的解析辅助：简写展开、边框、背景、变换等。
//!
//! 从 `css::computed::apply` 切出来只因为 apply.rs 贴着行数上限；
//! 对外路径不变，仅供 `apply` 内部调用。

use std::sync::Arc;

use super::super::{
    BackgroundAttachment, BackgroundLayer, BackgroundPosition, BackgroundRepeat, BackgroundSize,
    BackgroundTile, BorderStyle, Color, ComputedStyle, EmphasisPosition, EmphasisStyle, GridTrack,
    Length, QuotePair, ShadowLayer, Transform,
};
use crate::css::value::{
    edge_length, parse_background_image_layer, parse_background_position, parse_background_repeat,
    parse_background_size, parse_border_style, parse_color, parse_font_family_list,
    parse_font_size, parse_font_weight, parse_line_height, parse_url_token, split_value_tokens,
};

/// `font` 简写：`[[style] [weight] size [/ line-height] family]`。
///
/// **有意不重置没写到的子属性。** CSS 规定 `font` 会把 `font-variant`、`font-stretch`、
/// `line-height`、`font-size`… 一律拉回初值，而我们的行距与字重是通过 `root_base`
/// 从读者偏好走进继承链的：一句 `font: 1em serif` 把读者的 1.75 行距改回 `normal`
/// 是产品级的错，不是排版细节。除此之外按 CSS：字号支持 `/行距`，
/// 字族取整条候选链并去引号（与 `font-family` 共用一个解析器）。
/// `small-caps` / `condensed` / `caption` 这类 token 我们不认，整条作废并计 `ignored`。
pub(super) fn apply_font_shorthand(
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

/// `flex: <grow> <shrink> <basis>` 简写。收 1–3 个 token：
/// - 单个长度（含 `%`）当 basis，grow=1、shrink=1（`flex: 34%` 的常见写法）；
/// - 单个数字当 grow，shrink=1、basis=auto（`flex: 1`）；
/// - `auto` 当 basis=grow=shrink 的初值组合（`flex: auto` → 1 1 auto）；
/// - 三值形态 `grow shrink basis`，basis 可为长度或 `auto`。
///
/// 任一段解析失败整条作废。
pub(super) fn apply_flex_shorthand(
    style: &mut ComputedStyle,
    own: f32,
    root_px: f32,
    value: &str,
) -> bool {
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.is_empty() || parts.len() > 3 {
        return false;
    }
    let num = |p: &str| -> Option<f32> {
        let n = p.parse::<f32>().ok()?;
        (n >= 0.0).then_some(n)
    };
    let basis = |p: &str| -> Option<Option<Length>> {
        if p.eq_ignore_ascii_case("auto") {
            Some(None)
        } else {
            edge_length(p, own, root_px, false).map(Some)
        }
    };
    // 单值：纯数字是 grow（`flex: 2`），带单位/`auto` 才是 basis（`flex: 34%`）。
    // 必须先判数字 —— `edge_length` 会把裸数字当 px 收下，反过来会把 `flex: 2` 误读成 basis。
    if parts.len() == 1 {
        if let Some(g) = num(parts[0]) {
            style.flex_grow = g;
            style.flex_shrink = 1.0;
            style.flex_basis = None;
            return true;
        }
        if let Some(b) = basis(parts[0]) {
            style.flex_grow = 1.0;
            style.flex_shrink = 1.0;
            style.flex_basis = b;
            return true;
        }
        return false;
    }
    // 两/三值：第一段 grow，第二段 shrink，第三段 basis（缺省 auto）。
    let grow = match num(parts[0]) {
        Some(v) => v,
        None => return false,
    };
    let shrink = match num(parts[1]) {
        Some(v) => v,
        None => return false,
    };
    let basis_val = if parts.len() == 3 {
        match basis(parts[2]) {
            Some(v) => v,
            None => return false,
        }
    } else {
        None
    };
    style.flex_grow = grow;
    style.flex_shrink = shrink;
    style.flex_basis = basis_val;
    true
}

/// `transform`：只收会建立层叠上下文、绘制侧能兑现的 2D 基本型。
/// `none` 清掉变换；`translate(x, y)` 与 `scale(s, [sy])` 各折一份。
/// 其余（rotate / matrix / 3D）整条判不认识，如实计入 ignored。
pub(super) fn apply_transform(
    style: &mut ComputedStyle,
    own: f32,
    root_px: f32,
    value: &str,
) -> bool {
    let v = value.trim();
    if v.eq_ignore_ascii_case("none") {
        style.transform = None;
        return true;
    }
    let lower = v.to_ascii_lowercase();
    // CSS 允许逗号或空白分隔函数参数（`translate(2px, 3px)` / `translate(2px 3px)`），
    // 逗号统一成空白再切，两种写法都收。
    let args = |inner: &str| -> Vec<String> {
        inner
            .replace(',', " ")
            .split_whitespace()
            .map(str::to_string)
            .collect()
    };
    if let Some(inner) = lower
        .strip_prefix("translate(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let parts = args(inner);
        if parts.is_empty() || parts.len() > 2 {
            return false;
        }
        let x = match edge_length(&parts[0], own, root_px, false) {
            Some(v) => v,
            None => return false,
        };
        let y = if parts.len() == 2 {
            match edge_length(&parts[1], own, root_px, false) {
                Some(v) => v,
                None => return false,
            }
        } else {
            Length::ZERO
        };
        style.transform = Some(Transform::Translate(x, y));
        return true;
    }
    if let Some(inner) = lower
        .strip_prefix("scale(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let parts = args(inner);
        if parts.is_empty() || parts.len() > 2 {
            return false;
        }
        let sx = match parts[0].parse::<f32>() {
            Ok(v) => v,
            Err(_) => return false,
        };
        let sy = if parts.len() == 2 {
            match parts[1].parse::<f32>() {
                Ok(v) => v,
                Err(_) => return false,
            }
        } else {
            sx
        };
        style.transform = Some(Transform::Scale(sx, sy));
        return true;
    }
    false
}

/// `text-emphasis` 简写：`<style> [color] [position]`，这里只收 style 段与颜色段。
pub(super) fn apply_text_emphasis(style: &mut ComputedStyle, value: &str) -> bool {
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.is_empty() {
        return false;
    }
    let mut consumed_color = false;
    apply_text_emphasis_style(style, parts[0]);
    for part in &parts[1..] {
        if let Some(c) = parse_color(part) {
            style.text_emphasis_color = Some(c);
            consumed_color = true;
        } else {
            match part.to_ascii_lowercase().as_str() {
                "under" => style.text_emphasis_position = EmphasisPosition::Under,
                "over" => style.text_emphasis_position = EmphasisPosition::Over,
                _ => {}
            }
        }
    }
    let _ = consumed_color;
    // 首段必须能被认成 style（含 none）。
    matches!(
        parts[0].to_ascii_lowercase().as_str(),
        "none" | "filled" | "open" | "dot" | "circle" | "double-circle" | "triangle" | "sesame"
    )
}

pub(super) fn apply_text_emphasis_style(style: &mut ComputedStyle, value: &str) -> bool {
    let low = value.to_ascii_lowercase();
    let emphasis = match low.as_str() {
        "none" => EmphasisStyle::None,
        "filled" | "filled dot" | "dot" => EmphasisStyle::FilledDot,
        "open" | "open dot" => EmphasisStyle::OpenDot,
        "filled circle" | "circle" => EmphasisStyle::FilledCircle,
        "triangle" | "filled triangle" => EmphasisStyle::Triangle,
        "sesame" | "filled sesame" => EmphasisStyle::Sesame,
        _ => return false,
    };
    style.text_emphasis = emphasis;
    true
}

/// `box-shadow`: 逗号分隔的多层，每层 `[inset] <dx> <dy> [blur] [color]`。
pub(super) fn apply_box_shadow(
    style: &mut ComputedStyle,
    value: &str,
    own: f32,
    root_px: f32,
) -> bool {
    if value.trim().eq_ignore_ascii_case("none") {
        style.box_shadows = Arc::from(Vec::new());
        return true;
    }
    let layers = crate::css::selector::split_top_level(value, ',');
    if layers.is_empty() {
        return false;
    }
    let mut out = Vec::with_capacity(layers.len());
    for layer in layers {
        let tokens = split_value_tokens(layer.trim());
        if tokens.len() < 2 {
            return false;
        }
        let mut inset = false;
        let mut nums = Vec::new();
        let mut color = None;
        for token in &tokens {
            if token.eq_ignore_ascii_case("inset") {
                inset = true;
                continue;
            }
            if let Some(length) = edge_length(token, own, root_px, false) {
                nums.push(length.resolve(own)); // em 按本元素字号折
                continue;
            }
            if let Some(c) = parse_color(token) {
                color = Some(c);
                continue;
            }
            return false;
        }
        if nums.len() < 2 {
            return false;
        }
        out.push(ShadowLayer {
            dx: nums[0],
            dy: nums[1],
            blur: nums.get(2).copied().unwrap_or(0.0),
            inset,
            color,
        });
    }
    style.box_shadows = Arc::from(out);
    true
}

/// `outline: <width> <style> <color>`。宽度不占几何，绘制时画在边框盒外。
pub(super) fn apply_outline(
    style: &mut ComputedStyle,
    value: &str,
    own: f32,
    root_px: f32,
) -> bool {
    if value.trim().eq_ignore_ascii_case("none") {
        style.outline = None;
        return true;
    }
    let tokens = split_value_tokens(value.trim());
    let mut width = 0.0f32;
    let mut border_style = BorderStyle::Solid;
    let mut color = None;
    let mut got_width = false;
    let mut got_style = false;
    for token in &tokens {
        let low = token.to_ascii_lowercase();
        // style 与 width 各自独立判（`1px solid red` 里 solid 在 1px 之后）。
        if !got_style && parse_border_style(&low).is_some_and(|s| s != BorderStyle::None) {
            border_style = parse_border_style(&low).unwrap();
            got_style = true;
            continue;
        }
        if !got_width && matches!(low.as_str(), "thin" | "medium" | "thick") {
            width = match low.as_str() {
                "thin" => 1.0,
                "thick" => 3.0,
                _ => 2.0,
            };
            got_width = true;
            continue;
        }
        if !got_width && let Some(l) = edge_length(&low, own, root_px, false) {
            width = l.resolve(own);
            got_width = true;
            continue;
        }
        if let Some(c) = parse_color(token) {
            color = Some(c);
            continue;
        }
        return false;
    }
    if !got_width {
        width = 2.0; // medium
    }
    match &mut style.outline {
        Some((_, _, _, offset)) => {
            let offset = *offset;
            style.outline = Some((width, color, border_style, offset));
        }
        None => style.outline = Some((width, color, border_style, 0.0)),
    }
    true
}

/// `quotes: "「" "」" "『" "』"` —— 1 或 2 对开合字符。
pub(super) fn apply_quotes(style: &mut ComputedStyle, value: &str) -> bool {
    if value.trim().eq_ignore_ascii_case("none") {
        style.quotes = None;
        return true;
    }
    let parts = crate::css::selector::split_top_level(value, ' ');
    let chars: Vec<char> = parts
        .iter()
        .filter_map(|p| {
            let t = p.trim().trim_matches('"').trim_matches('\'');
            t.chars().next()
        })
        .collect();
    if chars.len() < 2 {
        return false;
    }
    let pair = QuotePair {
        open: chars[0],
        close: chars[1],
    };
    let outer = if chars.len() >= 4 {
        QuotePair {
            open: chars[2],
            close: chars[3],
        }
    } else {
        pair
    };
    style.quotes = Some([outer, pair]);
    true
}

/// `grid-template-columns`: 空格分隔的轨道（px/%/fr/minmax(N,M)/auto）。
pub(super) fn apply_grid_tracks(
    style: &mut ComputedStyle,
    value: &str,
    own: f32,
    root_px: f32,
) -> bool {
    if value.trim().eq_ignore_ascii_case("none") {
        style.grid_template_columns = Arc::from(Vec::new());
        return true;
    }
    let tokens = split_value_tokens(value.trim());
    let mut tracks = Vec::with_capacity(tokens.len());
    for token in &tokens {
        let low = token.to_ascii_lowercase();
        if low == "auto" {
            tracks.push(GridTrack::Auto);
            continue;
        }
        if let Some(fr) = low
            .strip_suffix("fr")
            .and_then(|s| s.trim().parse::<f32>().ok())
        {
            tracks.push(GridTrack::Fr(fr));
            continue;
        }
        if low.starts_with("minmax(") && low.ends_with(')') {
            let inner = &low[7..low.len() - 1];
            let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
            if parts.len() != 2 {
                return false;
            }
            let min = edge_length(parts[0], own, root_px, false).unwrap_or(Length::Px(0.0));
            // max 一侧可能是 `1fr` / `max-content` / 普通长度。
            if let Some(fr) = parts[1]
                .strip_suffix("fr")
                .and_then(|s| s.trim().parse::<f32>().ok())
            {
                tracks.push(GridTrack::MinMaxFr(min, fr));
                continue;
            }
            let max = if parts[1] == "max-content" {
                Length::Px(1e6)
            } else {
                match edge_length(parts[1], own, root_px, false) {
                    Some(b) => b,
                    None => return false,
                }
            };
            tracks.push(GridTrack::MinMax(min, max));
            continue;
        }
        match edge_length(&low, own, root_px, false) {
            Some(l) => tracks.push(GridTrack::Length(l)),
            None => return false,
        }
    }
    style.grid_template_columns = Arc::from(tracks);
    true
}

/// `grid-column` / `grid-row`：`N`、`N / M`、`span N`、`N / span M`。
/// 只收正整数与 `span`；负线号与 `auto` 之外的自动关键字判不认识。
pub(super) fn apply_grid_line(style: &mut ComputedStyle, column: bool, value: &str) -> bool {
    let parts: Vec<&str> = value.split('/').map(str::trim).collect();
    if parts.is_empty() || parts.len() > 2 {
        return false;
    }
    let mut start: Option<u16> = None;
    let mut span: u16 = 1;
    let first = parts[0].to_ascii_lowercase();
    if let Some(n) = first.strip_prefix("span ") {
        match n.trim().parse::<u16>() {
            Ok(v) if v > 0 => span = v,
            _ => return false,
        }
    } else {
        match first.parse::<u16>() {
            Ok(v) if v > 0 => start = Some(v),
            _ => return false,
        }
    }
    if let Some(second) = parts.get(1) {
        let low = second.to_ascii_lowercase();
        if let Some(n) = low.strip_prefix("span ") {
            match n.trim().parse::<u16>() {
                Ok(v) if v > 0 => span = v,
                _ => return false,
            }
        } else {
            match low.parse::<u16>() {
                Ok(end) if end > start.unwrap_or(1) => span = end - start.unwrap_or(1),
                _ => return false,
            }
        }
    }
    if column {
        style.grid_column_start = start;
        style.grid_column_span = span;
    } else {
        style.grid_row_start = start;
        style.grid_row_span = span;
    }
    true
}

/// 一条背景长手属性（`background-size` 那一族）的**图层列表**：按顶层逗号切，
/// 每层各解一次。`url(a,b)` 里那个逗号在括号内，`split_top_level` 切不进去。
/// 任何一层解不出就整条判不认识 —— 把六层里的第三层悄悄丢掉，屏上看不出原因。
fn background_layer_list<T>(value: &str, parse: impl Fn(&str) -> Option<T>) -> Option<Vec<T>> {
    let parts = crate::css::selector::split_top_level(value, ',');
    if parts.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        out.push(parse(part)?);
    }
    Some(out)
}

pub(super) fn parse_background_image_list(value: &str) -> Option<Vec<Option<String>>> {
    background_layer_list(value, |part| {
        // 一层只有一个图源 token；`url(a) url(b)` 那种多写是非法值。
        let tokens = split_value_tokens(part);
        if tokens.len() != 1 {
            return None;
        }
        parse_background_image_layer(&tokens[0])
    })
}

/// `background-size` 长手的一层列表。em/rem 按本元素字号折（`own`），交给 [`edge_length`]。
pub(super) fn parse_background_size_list(
    value: &str,
    own: f32,
    root_px: f32,
) -> Option<Vec<BackgroundSize>> {
    background_layer_list(value, |part| {
        parse_background_size_fonted(part, own, root_px)
    })
}

/// 单层 `background-size`，em/rem 已按字号折。`auto` / `cover` / `contain` 与百分比/px 同旧语义。
pub(super) fn parse_background_size_fonted(
    value: &str,
    own: f32,
    root_px: f32,
) -> Option<BackgroundSize> {
    let parts = split_value_tokens(value);
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    let axis = |t: &str| -> Option<Option<Length>> {
        let low = t.to_ascii_lowercase();
        if low == "auto" {
            return Some(None);
        }
        Some(Some(edge_length(&low, own, root_px, false)?))
    };
    if parts.len() == 1 {
        let low = parts[0].to_ascii_lowercase();
        return match low.as_str() {
            "cover" => Some(BackgroundSize::Cover),
            "contain" => Some(BackgroundSize::Contain),
            "auto" => Some(BackgroundSize::Auto),
            _ => Some(BackgroundSize::Explicit {
                width: Some(edge_length(&low, own, root_px, false)?),
                height: None,
            }),
        };
    }
    Some(BackgroundSize::Explicit {
        width: axis(&parts[0])?,
        height: axis(&parts[1])?,
    })
}

pub(super) fn parse_background_position_list(value: &str) -> Option<Vec<BackgroundPosition>> {
    background_layer_list(value, parse_background_position)
}

pub(super) fn parse_background_repeat_list(value: &str) -> Option<Vec<BackgroundRepeat>> {
    background_layer_list(value, parse_background_repeat)
}

pub(super) fn parse_background_attachment_list(value: &str) -> Option<Vec<BackgroundAttachment>> {
    background_layer_list(value, |part| match part.to_ascii_lowercase().as_str() {
        "scroll" => Some(BackgroundAttachment::Scroll),
        "fixed" => Some(BackgroundAttachment::Fixed),
        "local" => Some(BackgroundAttachment::Local),
        _ => None,
    })
}

/// 把一条**长手**列表铺到图层表上：图层数取历史值与这条列表长度的较大者，
/// 短的那一条从头循环。
///
/// 循环是规范定的，不是顺手写的：`background-image: url(a), url(b)` 配
/// `background-repeat: no-repeat`（只有一个值），CSS 要的是两层都不重复。
/// 只把值给第一层、第二层落回初值 `repeat`，症状是第二张图在扉页上铺成一片马赛克。
pub(super) fn for_each_layer(
    style: &mut ComputedStyle,
    count: usize,
    mut set: impl FnMut(&mut BackgroundLayer, usize),
) {
    while style.background_layers.len() < count {
        style.background_layers.push(BackgroundLayer::default());
    }
    for (index, layer) in style.background_layers.iter_mut().enumerate() {
        set(layer, index % count);
    }
}

/// `background` 简写切出来的**一层**：摆法三件 + 这一层里那条颜色（可无）。
pub(super) struct BackgroundLayerSpec {
    layer: BackgroundLayer,
    color: Option<Color>,
}

/// 解简写的一层。语法是 `<'bg-image'> || <'bg-position'> [/ <'bg-size'>] || <repeat-style>
/// || <'bg-clip'> || <'box'> || <'color'>`，各项次序随意。
///
/// 认不出的走法：**只把这一层判失败**，由调用方让整条声明作废。分类靠「这个 token
/// 是不是某种已知值」而不是靠位置，因为出版书里两种次序都见得到
/// （`background: #f4f7f1 url(p0.png) no-repeat bottom right` 色在前，
/// `background: url(x) repeat center` 色也在前，而 `background: center url(x)` 图在后）。
pub(super) fn parse_background_layer_spec(text: &str) -> Option<BackgroundLayerSpec> {
    if text.is_empty() || text.eq_ignore_ascii_case("initial") || text.eq_ignore_ascii_case("unset")
    {
        return None;
    }
    // 尺寸挂在第一个顶层 `/` 后面：`center / cover`、`0 0/50% auto`。
    // 「顶层」= 跳过引号与括号，所以路径里那一串斜杠（`OEBPS/Images/v1.jpg`）不会被当成分隔符。
    let (main, size_part) = split_first_top_level(text, '/');
    let mut layer = BackgroundLayer::default();
    let mut color = None;
    let mut position: Vec<String> = Vec::new();
    let mut image_seen = false;
    for token in split_value_tokens(main) {
        let low = token.to_ascii_lowercase();
        if low == "none" {
            // 作者明确「这一层不要图」：`image` 留在 `None`，但这一层算已经写过图。
            if image_seen {
                return None;
            }
            image_seen = true;
            continue;
        }
        if let Some(path) = parse_url_token(&token) {
            if image_seen {
                // 一层里两个图源（`url(a) url(b)`）是非法值。
                return None;
            }
            layer.image = Some(path);
            image_seen = true;
            continue;
        }
        if let Some((x, y)) = repeat_keyword_pair(&low) {
            layer.repeat.x = x;
            layer.repeat.y = y;
            continue;
        }
        // attachment 与 clip / origin：都收下来（clip / origin 只按边框盒摆，见
        // `apply` 那一臂的注释）。这里负责**吃掉那个 token**，不让它掉进
        // 「剩下的都是位置」那一桶里被解成一条非法坐标。
        if matches!(low.as_str(), "scroll" | "fixed" | "local") {
            layer.attachment = match low.as_str() {
                "fixed" => BackgroundAttachment::Fixed,
                "local" => BackgroundAttachment::Local,
                _ => BackgroundAttachment::Scroll,
            };
            continue;
        }
        if matches!(low.as_str(), "border-box" | "padding-box" | "content-box") {
            continue;
        }
        if let Some(rgba) = parse_color(&low) {
            color = Some(rgba);
            continue;
        }
        position.push(low);
    }
    if !position.is_empty() {
        layer.position = parse_background_position(&position.join(" "))?;
    }
    layer.size = match size_part {
        Some(part) => parse_background_size(part)?,
        None => BackgroundSize::default(),
    };
    Some(BackgroundLayerSpec { layer, color })
}

/// 在第一个**顶层**分隔符处把值切成两半（尺寸那一截可能是空串，但存在）。
///
/// 不复用 `split_top_level` 再拼回去：那条只管切、不给字节位，靠「先切再拿 `find` 找回原串」
/// 定位，分隔符本身出现在值里（路径的斜杠、引号内的空格）就会指到错的那一处。
pub(super) fn split_first_top_level(text: &str, sep: char) -> (&str, Option<&str>) {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (index, ch) in text.char_indices() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c == sep && depth <= 0 => {
                let head = text[..index].trim();
                let tail = text[index + c.len_utf8()..].trim();
                return (head, Some(tail));
            }
            _ => {}
        }
    }
    (text.trim(), None)
}

/// `background` 简写：一层一条，全表重建。
pub(super) fn apply_background_shorthand(style: &mut ComputedStyle, value: &str) -> bool {
    let parts = crate::css::selector::split_top_level(value, ',');
    if parts.is_empty() {
        return false;
    }
    // 先把每一层解完，**全解得开才动手改表**。反过来的后果是这条声明明明被 judged
    // 不认识、计进了 ignored，却已经顺手把上一轮的底色与图层抹了个干净 ——
    // 「作废的声明不许有副作用」是层叠的一条硬规矩，`apply_font_shorthand` 也是这么写的。
    let mut parsed = Vec::with_capacity(parts.len());
    for part in &parts {
        let Some(spec) = parse_background_layer_spec(part.trim()) else {
            return false;
        };
        parsed.push(spec);
    }
    // 简写先把每一条长手拉回初值（CSS 明文）。不重置的后果很具体：
    // 转换器常写的 `.a{background:url(x) center/cover}` 后面再跟一条 `.a{background:#fff}`，
    // 不重置就是白底**叠在图上**、扉页那张底图变成灰蒙蒙一片。
    style.background_color = None;
    style.background_layers.clear();
    // 颜色归最底那一层（= 书写顺序的最后一条）。写在中间层上的色在浏览器里就是这个下场。
    let mut color = None;
    for BackgroundLayerSpec { layer, color: c } in parsed {
        if c.is_some() {
            color = c;
        }
        style.background_layers.push(layer);
    }
    style.background_color = color;
    true
}

/// 一个铺法关键字当成「两轴一起写」的那一档；`repeat-x` / `repeat-y` 只能这么出现。
pub(super) fn repeat_keyword_pair(low: &str) -> Option<(BackgroundTile, BackgroundTile)> {
    match low {
        "repeat" => Some((BackgroundTile::Repeat, BackgroundTile::Repeat)),
        "no-repeat" => Some((BackgroundTile::NoRepeat, BackgroundTile::NoRepeat)),
        "repeat-x" => Some((BackgroundTile::Repeat, BackgroundTile::NoRepeat)),
        "repeat-y" => Some((BackgroundTile::NoRepeat, BackgroundTile::Repeat)),
        "space" => Some((BackgroundTile::Space, BackgroundTile::Space)),
        "round" => Some((BackgroundTile::Round, BackgroundTile::Round)),
        _ => None,
    }
}
