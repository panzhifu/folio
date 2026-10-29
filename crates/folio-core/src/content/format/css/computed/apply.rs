//! 一条声明如何变成一个计算值：属性分发、简写展开、继承、边框。
//!
//! 从 `css::computed` 切出来只因为那台属性分发的 switch 本身就六百行；类型定义与层叠
//! 入口留在父模块，这里的东西只对整个 `css` 内部开放，不属于对外公开面。

use super::{
    BackgroundLayer, BackgroundPosition, BackgroundRepeat, BackgroundSize, BackgroundTile,
    BorderEdge, BorderStyle, BoxSizing, CellVAlign, Color, ComputedStyle, Direction, Display,
    Hyphens, KNOWN_UNIMPLEMENTED, Positioning, TextAlign, TextJustify, WordBreak,
};
use crate::content::format::css::Declaration;
use crate::content::format::css::value::{
    BorderElement, border_target, border_width_px, box_side_px, box_sides, four_of,
    parse_background_image_layer, parse_background_position, parse_background_repeat,
    parse_background_size, parse_baseline_shift, parse_border_radius, parse_border_style,
    parse_break_when, parse_clear, parse_color, parse_count, parse_decoration, parse_float,
    parse_font_family_list, parse_font_size, parse_font_weight, parse_line_height,
    parse_list_style, parse_text_shadow, parse_url_token, parse_writing_mode, relative_weight,
    set_edge, set_length, set_margin, set_min_length, set_px, split_value_tokens,
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
    // 四批：font-size → line-height → `color` → 其余。
    // `color` 单独一批是因为 `currentColor` 要拿它：`background-color: currentcolor` 写在
    // `color` **前面**也得折对 —— 照书写顺序的话，先算的两句拿到的是上一轮的色。
    for group in 0..4 {
        for decl in decls {
            let is_font_size = decl.property == "font-size";
            let is_line_height = decl.property == "line-height";
            let is_color = decl.property == "color";
            let mine = match group {
                0 => is_font_size,
                1 => is_line_height,
                2 => is_color,
                _ => !is_font_size && !is_line_height && !is_color,
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
///
/// `base_dir` 是**这一章文档**在容器里的目录（`ChapterImages` 手里那一个），进来先走一遍
/// `absolutize_css_urls`。这一折不是可有可无的整洁：样式表里那句 `url(../Images/x.png)`
/// 在拼进层叠之前就按 CSS 自己的目录折成了容器内路径，而行内那一份从前**没人折过**，
/// 于是同一张图在两处是两个形状。症状写在 `layout::tests::index` 那一条真书门上 ——
/// 哈利波特的七张册扉页写的正是 `<body style="background: url(../Images/v2.jpg)">`：
/// 图层表里躺着一条 `../Images/v2.jpg`，图片索引按容器路径精确查，查不到，
/// 屏上仍是「白底上一片看不见的淡金字」，而 CSS 那一侧一切正常、`css-gap` 也报不出问题。
pub(super) fn parse_inline_style(text: &str, base_dir: &str) -> Vec<Declaration> {
    let text = crate::content::format::absolutize_css_urls(text, base_dir);
    crate::content::format::css::selector::split_top_level(&text, ';')
        .into_iter()
        .filter_map(|decl| crate::content::format::css::parse_declaration(&decl))
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
        // `text-justify` 在 CSS 里可继承：书里在 `<body>` 上写一次 `inter-ideograph`
        // 就管全书，靠的正是这一行。
        text_justify: parent.text_justify,
        // `direction` 在 CSS 里可继承，而且它是 `start`/`end` 折边的唯一依据，
        // 所以继承断了就等于整棵子树回到左起 —— RTL 书通常在 `<html>` 或 `<body>`
        // 上写一次，靠的就是这一行。
        direction: parent.direction,
        // `writing-mode` 在 CSS 里同样可继承，理由与上面那一行一模一样：竖排的本子只在
        // `<html>` 或 `<body>` 上写一次，整棵子树靠这一行拿到那一根推进轴。
        writing_mode: parent.writing_mode,
        color: parent.color,
        // 「在一条链接里」按继承传播：链接里的 `<span>`、`<b>` 与其中的文本都算链接的一部分，
        // 与下面装饰线那一格同一个理由。它不是 CSS 属性，判据见 `ComputedStyle::link`。
        link: parent.link,
        text_indent: parent.text_indent,
        word_spacing: parent.word_spacing,
        // 字距在 CSS 里可继承（`letter-spacing` 是 inheritable），书里常在 `<body>` 上写一次。
        letter_spacing: parent.letter_spacing,
        // `word-break` 在 CSS 里可继承：书里在 `<body>` 上写一次就管全书。
        word_break: parent.word_break,
        // `border-collapse` 在 CSS 里可继承：写在 `<table>` 上就管到每个格子，
        // 而合并要**按格**逐边判（见 `Layout::decor_seeds`），所以它得跟着继承下来。
        border_collapse: parent.border_collapse,
        // `border-spacing` 同样可继承（CSS 2.1 §17.6.1），且只在 `separate` 下生效。
        border_spacing: parent.border_spacing,
        // `text-shadow` 在 CSS 里可继承，书里一般在 `<body>` 或标题规则上写一次就管一片。
        // 克隆的是一份 `Arc<[TextShadow]>`：没有影子的节点只加一次引用计数。
        text_shadows: parent.text_shadows.clone(),
        // `list-style-type` 在 CSS 2.1 里可继承，`<ol>` 的十进制就是靠这一行才落到
        // 每一个 `<li>` 上的（浏览器 UA 表写的是 `ol { list-style-type: decimal }`）。
        list_style_type: parent.list_style_type,
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

/// 一条背景长手属性（`background-size` 那一族）的**图层列表**：按顶层逗号切，
/// 每层各解一次。`url(a,b)` 里那个逗号在括号内，`split_top_level` 切不进去。
/// 任何一层解不出就整条判不认识 —— 把六层里的第三层悄悄丢掉，屏上看不出原因。
fn background_layer_list<T>(value: &str, parse: impl Fn(&str) -> Option<T>) -> Option<Vec<T>> {
    let parts = crate::content::format::css::selector::split_top_level(value, ',');
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

fn parse_background_image_list(value: &str) -> Option<Vec<Option<String>>> {
    background_layer_list(value, |part| {
        // 一层只有一个图源 token；`url(a) url(b)` 那种多写是非法值。
        let tokens = split_value_tokens(part);
        if tokens.len() != 1 {
            return None;
        }
        parse_background_image_layer(&tokens[0])
    })
}

fn parse_background_size_list(value: &str) -> Option<Vec<BackgroundSize>> {
    background_layer_list(value, parse_background_size)
}

fn parse_background_position_list(value: &str) -> Option<Vec<BackgroundPosition>> {
    background_layer_list(value, parse_background_position)
}

fn parse_background_repeat_list(value: &str) -> Option<Vec<BackgroundRepeat>> {
    background_layer_list(value, parse_background_repeat)
}

/// 把一条**长手**列表铺到图层表上：图层数取历史值与这条列表长度的较大者，
/// 短的那一条从头循环。
///
/// 循环是规范定的，不是顺手写的：`background-image: url(a), url(b)` 配
/// `background-repeat: no-repeat`（只有一个值），CSS 要的是两层都不重复。
/// 只把值给第一层、第二层落回初值 `repeat`，症状是第二张图在扉页上铺成一片马赛克。
fn for_each_layer(
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
struct BackgroundLayerSpec {
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
fn parse_background_layer_spec(text: &str) -> Option<BackgroundLayerSpec> {
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
        // attachment 与 clip / origin：认得这些写法，但都按最常用那一档摆，
        // 判据写在 `apply` 里那两臂的注释上。这里只负责**吃掉那个 token**，
        // 不让它掉进「剩下的都是位置」那一桶里被解成一条非法坐标。
        if matches!(
            low.as_str(),
            "scroll" | "fixed" | "local" | "border-box" | "padding-box" | "content-box"
        ) {
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
fn split_first_top_level(text: &str, sep: char) -> (&str, Option<&str>) {
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
fn apply_background_shorthand(style: &mut ComputedStyle, value: &str) -> bool {
    let parts = crate::content::format::css::selector::split_top_level(value, ',');
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
fn repeat_keyword_pair(low: &str) -> Option<(BackgroundTile, BackgroundTile)> {
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
        "direction" => match value.to_ascii_lowercase().as_str() {
            "ltr" => {
                style.direction = Direction::Ltr;
                true
            }
            "rtl" => {
                style.direction = Direction::Rtl;
                true
            }
            // 只认这两个值。CSS 里 `direction` 没有别的关键词（`auto` 不是 CSS 值，
            // 它是 HTML 的 `dir` 属性那一侧的东西，见 `css::computed` 的层叠入口），
            // 所以写 `auto` 进样式表算「这条没接住」，进 ignored 计数。
            _ => false,
        },
        "writing-mode" => match parse_writing_mode(value) {
            Some(mode) => {
                style.writing_mode = mode;
                true
            }
            None => false,
        },
        "text-align" => match value.to_ascii_lowercase().as_str() {
            // `start` / `end` 与 `left` / `right` 分开存：前者跟着 `direction` 翻边，
            // 后者是作者钉死的物理方向。合并成一档就是把 RTL 的书全左对齐的那处错。
            "start" => {
                style.text_align = TextAlign::Start;
                true
            }
            "end" => {
                style.text_align = TextAlign::End;
                true
            }
            "left" => {
                style.text_align = TextAlign::Left;
                true
            }
            "right" => {
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
        // `text-justify`：两端对齐时余量往哪种隙摊。它的行为影响在排版层兑现（见
        // `layout::collect::emit_line`）；这一层只记作者要什么。`-epub-` 前缀是 EPUB
        // 风格规范点名的旧写法，一起认，别把认得的写成「不认识」。
        "text-justify" | "-epub-text-justify" => match value.to_ascii_lowercase().as_str() {
            "auto" => {
                style.text_justify = TextJustify::Auto;
                true
            }
            // `inter-word` 只拉空格；`inter-character` / `inter-ideograph` / `distribute`
            // 都是「在字间摊」，折进同一档（见 [`TextJustify`]）。
            "inter-word" => {
                style.text_justify = TextJustify::InterWord;
                true
            }
            "inter-character" | "inter-ideograph" | "distribute" => {
                style.text_justify = TextJustify::InterCharacter;
                true
            }
            // `none` 在这一层不取消 `text-align`（那是另一条声明的事），只关掉摊余量。
            "none" => {
                style.text_justify = TextJustify::None;
                true
            }
            _ => false,
        },
        // `duokan-text-indent` 是多看转换器写在我们标准属性**旁边**的那一份副本：
        // 《哈利·波特》全书 21 个规则块里它都与同一条规则的标准 `text-indent` 成对出现
        // （20 块同值，1 块 `2em` vs `1em`）。收它不是为了新行为，是为了让
        // `examples/css-gap` 不再把这 5,149 个元素报成落差 —— 那 91% 的「10.9%」本来
        // 就是同一个数的两遍。与已经收下的 `-webkit-hyphens` / `-epub-*` 同类。
        "text-indent" | "duokan-text-indent" => {
            set_edge(value, own, root_px, &mut style.text_indent)
        }
        // 词距：CSS 初值 `normal` 折成 0，其余按长度。它可继承（`inherit_from` 里带着）。
        "word-spacing" => {
            if value.eq_ignore_ascii_case("normal") {
                style.word_spacing = 0.0;
                return true;
            }
            set_px(value, own, root_px, &mut style.word_spacing)
        }
        // 字距：与词距同一档解析（`normal` → 0，其余按长度），可继承。负值合法（收紧），
        // 所以不夹下限 —— `set_px` 允许负数，而排版与绘制对负字距同样成立。
        "letter-spacing" => {
            if value.eq_ignore_ascii_case("normal") {
                style.letter_spacing = 0.0;
                return true;
            }
            set_px(value, own, root_px, &mut style.letter_spacing)
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
        // `vertical-align` 有两副面孔：关键字是**表格格**的贴边（这一格不继承），
        // 长度 / 百分比 / `super` / `sub` 是行内盒相对基线的位移（`baseline_shift`）。
        "vertical-align" => match value.to_ascii_lowercase().as_str() {
            "top" => {
                style.cell_valign = CellVAlign::Top;
                true
            }
            "middle" => {
                style.cell_valign = CellVAlign::Middle;
                true
            }
            "bottom" => {
                style.cell_valign = CellVAlign::Bottom;
                true
            }
            "baseline" => {
                style.cell_valign = CellVAlign::Top;
                style.baseline_shift = 0.0;
                true
            }
            _ => match parse_baseline_shift(value, own, root_px, style.line_height) {
                Some(px) => {
                    style.baseline_shift = px;
                    true
                }
                None => false,
            },
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
        // 四条边都收百分比（含 `auto` 折 0 那两条），折算点见 `edge_length`；
        // 左右边 `auto` 折成 0 的理由在那条函数上头。
        "margin-top" => set_margin(value, own, root_px, &mut style.margin_top),
        "margin-bottom" => set_margin(value, own, root_px, &mut style.margin_bottom),
        "margin-left" => set_margin(value, own, root_px, &mut style.margin_left),
        "margin-right" => set_margin(value, own, root_px, &mut style.margin_right),
        "padding-top" => set_edge(value, own, root_px, &mut style.padding_top),
        "padding-bottom" => set_edge(value, own, root_px, &mut style.padding_bottom),
        "padding-left" => set_edge(value, own, root_px, &mut style.padding_left),
        "padding-right" => set_edge(value, own, root_px, &mut style.padding_right),
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
        // 背景一族。`transparent` 是「有这条声明、但不画」，与「书里没写过」在层叠上
        // 是两件事（后者可能被更高 specificity 的一条颜色盖住），所以两条都要认。
        "background-color" => {
            // `currentColor` = 本元素自己的 `color`（没写就取继承来的那份）。`color` 那一批
            // 已经先落地，所以这里折得出数 —— 与它写在 `color` 前面还是后面无关。
            if value.eq_ignore_ascii_case("currentcolor") {
                style.background_color = style.color.or(parent.color);
                return true;
            }
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
        "background-image" => match parse_background_image_list(value) {
            Some(list) => {
                let count = list.len();
                for_each_layer(style, count, |layer, i| layer.image = list[i].clone());
                true
            }
            None => false,
        },
        "background-size" => match parse_background_size_list(value) {
            Some(list) => {
                let count = list.len();
                for_each_layer(style, count, |layer, i| layer.size = list[i]);
                true
            }
            None => false,
        },
        "background-position" => match parse_background_position_list(value) {
            Some(list) => {
                let count = list.len();
                for_each_layer(style, count, |layer, i| layer.position = list[i]);
                true
            }
            None => false,
        },
        "background-repeat" => match parse_background_repeat_list(value) {
            Some(list) => {
                let count = list.len();
                for_each_layer(style, count, |layer, i| layer.repeat = list[i]);
                true
            }
            None => false,
        },
        // 铺法之外的三条不在这里接：`background-attachment` / `-clip` / `-origin` 走
        // `KNOWN_UNIMPLEMENTED` 那一档（认得、不生效）。为什么简写里仍然**吃掉**那些词：
        // `background: url(x) no-repeat fixed center` 里的 `fixed` 是真语法，不认下来
        // 整条简写就作废，底色与图一起消失 —— 那是比「fixed 没生效」大得多的错。
        // 而长手单写那三条时屏上确实没有任何变化，所以缺口量具必须说真话。
        //
        // `background` 简写：颜色、图、铺法、位置、尺寸一条全写。
        //
        // 接住里面的**颜色**那一项是 2026-10-14 那一刀，理由仍然成立：出版书里「深色底块 +
        // 白字标题」用的是 `background: #b71c1c; color: #ffffff`，只认 `background-color`
        // 的话底色不画、白字照画 —— 标题变成白纸上的白字，整条消失（《罗生门》每一章的
        // 标题正是这么没的）。2026-10-23 这一轮把图那一截也接上了：哈利波特的七张册扉页
        // 整页背景是一张 792×1400 的照片，上面压的两张 PNG 全带透明通道，
        // 底图一丢就成「白底上一片看不见的淡金字」。
        //
        // **简写没写到的子属性要拉回初值**，这是 CSS 明文；为什么必须整表重置，
        // 判据写在 `apply_background_shorthand` 里面那条注释上。
        // CSS 的 `initial`：把这一个属性的每一格拉回初值。整条走通用那条路太大
        // （每个属性都要知道自己的初值），这里只收**简写**这一档 —— 它出现的形态就是
        // 重置块里的一句（The Riddle 那本书 5 个元素）。
        "background" if value.eq_ignore_ascii_case("initial") => {
            style.background_color = None;
            style.background_layers.clear();
            true
        }
        "background" => apply_background_shorthand(style, value),
        // 影子：整表重建，颜色写 `currentcolor`（或干脆不写）时存 `None`，绘制层落本 run
        // 的文字色。落地程度写在 [`TextShadow`] 上 —— 模糊是偏移副本近似，不是高斯。
        "text-shadow" => match parse_text_shadow(value, own, root_px) {
            Some(list) => {
                style.text_shadows = std::sync::Arc::from(list);
                true
            }
            None => false,
        },
        // `all: initial`：把**每一个**属性拉回初值（含继承来的那些）。
        //
        // 近似一处：`ComputedStyle::default()` 的 `display` 是 `Block`，而 CSS 的初值
        // 是 `inline` —— 对 `li` 这类靠 UA 表拿到 `list-item` 的元素，`all: initial`
        // 在浏览器里会把标记也一起收掉，我们退回「块」。真书里这条只出现在
        // Project Gutenberg 的页眉页脚重置里（`#pg-footer li { all: initial; display: block; … }`），
        // 而那条规则自己就跟了 `display: block`，差不到屏上。
        // `link` 不是 CSS 属性（是「这段在不在 `<a>` 里」），重置时留着。
        "all" if value.eq_ignore_ascii_case("initial") => {
            let link = style.link;
            *style = ComputedStyle::default();
            style.link = link;
            true
        }
        // 定位。`static` 是初值；`absolute` / `fixed` 脱流并按四个偏移摆（见 [`Positioning`]）。
        "position" => match value.to_ascii_lowercase().as_str() {
            "static" => {
                style.position = Positioning::Static;
                true
            }
            "absolute" => {
                style.position = Positioning::Absolute;
                true
            }
            "fixed" => {
                style.position = Positioning::Fixed;
                true
            }
            _ => false,
        },
        "left" => set_length(value, own, root_px, &mut style.left),
        "right" => set_length(value, own, root_px, &mut style.right),
        "top" => set_length(value, own, root_px, &mut style.top),
        "bottom" => set_length(value, own, root_px, &mut style.bottom),
        // `border-spacing`：一到两个长度（横, 纵），不收百分比（CSS 也拒）。
        "border-spacing" if value.split_whitespace().count() > 2 => false,
        "border-spacing" => match box_side_px(value, own, root_px) {
            Some([h, _, v, _]) => {
                style.border_spacing = [h, v];
                true
            }
            None => false,
        },
        // `border-collapse` 同样要排在 `border*` 前缀分支之前（`collapse` 既不是边名、
        // 也不是 width/style/color 三要素）。两个值都收；`separate` 就是现在的默认行为。
        "border-collapse" => match value.to_ascii_lowercase().as_str() {
            "collapse" => {
                style.border_collapse = true;
                true
            }
            "separate" => {
                style.border_collapse = false;
                true
            }
            _ => false,
        },
        // `border-radius` 必须在 `border*` 那条前缀分支**之前**：否则它会被当作
        // 「边名 + 要素名」的两段尾巴（`radius` 不是 width/style/color）判不认识。
        "border-radius" => match parse_border_radius(value, own, root_px) {
            Some(corners) => {
                style.border_radius = corners;
                true
            }
            None => false,
        },
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
            // `inline-block` 落到下面的 `_ => false`（计入 ignored），是刻意的：排版层
            // 只分「`None` 跳过 / `Inline` 走行内 / `ListItem` 画标记 / 其余按块」这四档
            // （`layout::collect` 的 `is_inline_flow` 与它给 `is_item` 那一个判据），所以
            // 从前存进去的 `InlineBlock` 没有人读 —— 屏上与 `display: block` 一模一样，而
            // `examples/css-gap` 报 `Applied`。行内块要的是 shrink-to-fit 的宽与「占一行内
            // 位置」两件事，那一档还没做（见 `doc/css-没接住.md` 里 max-content 那条），
            // 所以这里宁可将它报成没接住。本机四本真书 `display: inline-block` 0 处。
            //
            // 表格四档 2026-10-29 接上：`layout::table` 抽网格、分列宽、摆格子。
            // `inline-table` 仍不接（要的是行内块那一套 shrink-to-fit，同上）。
            "table" => {
                style.display = Display::Table;
                true
            }
            "table-row" => {
                style.display = Display::TableRow;
                true
            }
            "table-cell" => {
                style.display = Display::TableCell;
                true
            }
            "table-row-group" | "table-header-group" | "table-footer-group" => {
                style.display = Display::TableRowGroup;
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
        // `color: currentColor` 就是 `inherit`（自引用按继承折，CSS 明文）。
        "color" if value.eq_ignore_ascii_case("currentcolor") => {
            style.color = parent.color;
            true
        }
        "color" => match parse_color(value) {
            Some(c) => {
                style.color = Some(c);
                true
            }
            None => false,
        },
        // `word-break`：只认 `normal` 与 `break-all`。`keep-all` 判不认识（见 [`WordBreak`]）。
        "word-break" => match value.to_ascii_lowercase().as_str() {
            "normal" => {
                style.word_break = WordBreak::Normal;
                true
            }
            "break-all" => {
                style.word_break = WordBreak::BreakAll;
                true
            }
            _ => false,
        },
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
        // 两个 `min-*`：值只收长度（`auto` / `none` 整条作废，判据写在 `set_min_length`）。
        // 生效点在排版层，而且**夹序**是它们最难写错的一格：`max-*` 先夹、`min-*` 后夹，
        // 所以「`max-width: 40px` 与 `min-width: 60px` 同时写」这种自相矛盾的声明里
        // 赢的是 `min-width`（CSS 2.1 §10.4 / §10.7），反过来就与浏览器不一致。
        // 见 `layout::collect::solve_block_width` 与 `used_content_height`。
        "min-width" => set_min_length(value, own, root_px, &mut style.min_width),
        "min-height" => set_min_length(value, own, root_px, &mut style.min_height),
        // 这一格决定上面六格的数**含不含**边框与内边距。生效点只有
        // `ComputedStyle::content_size` 一个式子（四个落点共用），所以这里只管存哪一档。
        // `padding-box` 不认（见 [`BoxSizing`]）：认成 `border-box` 会把边框也扣掉。
        "box-sizing" => match value.to_ascii_lowercase().as_str() {
            "content-box" => {
                style.box_sizing = BoxSizing::ContentBox;
                true
            }
            "border-box" => {
                style.box_sizing = BoxSizing::BorderBox;
                true
            }
            _ => false,
        },
        // 这两个属性我们**本来就按初值在做**：`list-style-image: none`（标记只有文字、
        // 没有图源）与 `list-style-position: outside`（标记挂在流动之外）正是现在画的样子。
        // 所以这两个值判「接住了」，其余值（`url(…)`、`inside`）判不认识 —— 与
        // `background-clip: border-box` 那一档同一条规矩：值的效果等于默认行为才算接住。
        // 《蛇结》51 个 `li` 写的正是这两个初值，从前被记成 102 条「认得不生效」。
        // 字体特性那两档的**初值**就是我们做的事（不启用任何特性），所以只收 `normal`，
        // 其余值判不认识 —— 与 `list-style` 的先例同一条规矩。
        // 《乌合之众》157 个元素写的正是 `font-variant: normal`。
        "font-variant" | "font-variant-caps" => value.eq_ignore_ascii_case("normal"),
        // OEB 1.x 的栏数（`columns` 的前身）。`1` 就是不分栏 —— 正是我们的行为；
        // 其余值要的是作者块级多栏，还没接。《西游记》2 个元素写的 `oeb-column-number: 1`。
        "oeb-column-number" => value.trim() == "1",
        "list-style-image" => value.eq_ignore_ascii_case("none"),
        "list-style-position" => value.eq_ignore_ascii_case("outside"),
        "list-style-type" => match parse_list_style(value) {
            Some(kind) => {
                style.list_style_type = kind;
                true
            }
            None => false,
        },
        // `list-style` 简写：`<position> || <image> || <type>` 三项，顺序随意。
        // 这一条只取其中的**类型**（书里写它九成就是 `list-style: none`），位置与图
        // 两项听得出、这一轮不生效；其余认不出的词整条判不认识。
        // 与 `background` 简写同一个口径：接住能接的那一项，不假装接住了整条。
        "list-style" => {
            let mut recognized = false;
            for part in value.split_whitespace() {
                if let Some(kind) = parse_list_style(part) {
                    style.list_style_type = kind;
                    recognized = true;
                    continue;
                }
                let word = part.to_ascii_lowercase();
                if word == "inside" || word == "outside" || word.starts_with("url(") {
                    recognized = true;
                } else {
                    return false;
                }
            }
            recognized
        }
        // 浮动与净空。两者都**不继承**，所以 `inherit_from` 里没有它们。
        "float" => match parse_float(value) {
            Some(side) => {
                style.float = side;
                true
            }
            None => false,
        },
        "clear" => match parse_clear(value) {
            Some(which) => {
                style.clear = which;
                true
            }
            None => false,
        },
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
                } else if part.eq_ignore_ascii_case("transparent")
                    || part.eq_ignore_ascii_case("currentcolor")
                {
                    // 两者在 `BorderEdge::color` 里是同一个记号 `None`（绘制层取这一块自己的
                    // `color`）。`currentColor` 从前没被认，于是 `border-top: currentColor
                    // none 0` 整条被丢掉 —— 《世界简史》5 个元素正是这么写的。
                    // `transparent` 严格说该是「有边但不画」，这里也折进 `None`：
                    // 边框色本来就没有 alpha 通道，画出来是实色，比丢掉整条更错。
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
        // 边框宽度走 px 版：CSS 的 `<border-width>` 不接受百分比，`box_side_px` 里
        // 百分比折不出就整条作废，与另外三条边的 `<line-width>` 关键字一起判。
        BorderElement::Width => match box_side_px(value, own, root_px) {
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
                if part.eq_ignore_ascii_case("transparent")
                    || part.eq_ignore_ascii_case("currentcolor")
                {
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
