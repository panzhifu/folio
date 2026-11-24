//! 一条声明如何变成一个计算值：属性分发、简写展开、继承、边框。只对整个 `css` 内部开放。

mod border;
mod inherit;
mod values;
use border::apply_border;
pub(crate) use inherit::inherit_from;
use values::*;

use super::{
    AlignItems, BorderStyle, BoxSizing, CellVAlign, ComputedStyle, DecorationStyle, Direction,
    Display, EmphasisPosition, FlexDirection, FlexWrap, Hyphens, InlineVAlign, JustifyContent,
    JustifySelf, KNOWN_UNIMPLEMENTED, Length, ObjectFit, Overflow, Positioning, RubyAlign,
    RubyPosition, SvgLineCap, SvgLineJoin, SvgPaint, TextAlign, TextJustify, TextTransform,
    Visibility, WordBreak,
};
use crate::css::Declaration;
use crate::css::value::{
    box_side_px, box_sides, edge_length, parse_baseline_shift, parse_bleed, parse_border_radius,
    parse_break_when, parse_clear, parse_color, parse_count, parse_decoration, parse_float,
    parse_font_family_list, parse_font_size, parse_font_weight, parse_line_height,
    parse_list_style, parse_text_shadow, parse_writing_mode, relative_weight, set_edge, set_length,
    set_margin, set_min_length, set_px,
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
/// 分隔符与声明解析复用样式表入口，容量超限时整份忽略。
///
/// `base_dir` 是这一章文档在容器里的目录；进来先按它把 `url(...)` 折成容器内路径，
/// 与样式表那一份同形，否则同一张图在两处是两个形状、图片索引按容器路径查不到。
pub(super) fn parse_inline_style(text: &str, base_dir: &str) -> Vec<Declaration> {
    if !crate::css::budget::allows(text) {
        tracing::warn!("行内 CSS 超过读取预算，忽略此属性");
        return Vec::new();
    }
    let text = crate::href::absolutize_css_urls(text, base_dir);
    crate::css::selector::split_top_level(&text, ';')
        .into_iter()
        .filter_map(|decl| crate::css::parse_declaration(&decl))
        .collect()
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
        // 三个前缀都写同一格：EPUB / WebKit 的书常写 `-epub-` 或 `-webkit-`，
        // 认不出前缀的后果是**整本竖排书横着排**（日文那本样本就是 `-webkit-`）。
        "writing-mode" | "-epub-writing-mode" | "-webkit-writing-mode" => {
            match parse_writing_mode(value) {
                Some(mode) => {
                    style.writing_mode = mode;
                    true
                }
                None => false,
            }
        }
        // `mixed` 是初值（横排字在竖排里不转、竖排标点按竖排摆），接受它等于没改任何东西；
        // `upright` / `sideways` 要逐字形旋转，排版层还没有那条路，照实计不认识。
        "text-orientation" | "-epub-text-orientation" | "-webkit-text-orientation" => {
            value.eq_ignore_ascii_case("mixed")
        }
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
        // `text-top` / `text-bottom` 要对齐父盒的字体上下沿，层叠期拿不到那份度量，
        // 所以只记关键字，排版层折成像素（见 `ComputedStyle::inline_valign`）。
        "vertical-align" => match value.to_ascii_lowercase().as_str() {
            "top" => {
                style.cell_valign = CellVAlign::Top;
                style.inline_valign = InlineVAlign::Baseline;
                style.baseline_shift = 0.0;
                true
            }
            "middle" => {
                style.cell_valign = CellVAlign::Middle;
                style.inline_valign = InlineVAlign::Baseline;
                style.baseline_shift = 0.0;
                true
            }
            "bottom" => {
                style.cell_valign = CellVAlign::Bottom;
                style.inline_valign = InlineVAlign::Baseline;
                style.baseline_shift = 0.0;
                true
            }
            "text-top" => {
                style.inline_valign = InlineVAlign::TextTop;
                style.baseline_shift = 0.0;
                true
            }
            "text-bottom" => {
                style.inline_valign = InlineVAlign::TextBottom;
                style.baseline_shift = 0.0;
                true
            }
            "baseline" => {
                style.cell_valign = CellVAlign::Top;
                style.baseline_shift = 0.0;
                style.inline_valign = InlineVAlign::Baseline;
                true
            }
            _ => match parse_baseline_shift(value, own, root_px, style.line_height) {
                Some(px) => {
                    style.baseline_shift = px;
                    style.inline_valign = InlineVAlign::Baseline;
                    true
                }
                None => false,
            },
        },
        // ruby 一族：注音排在基字的哪一侧（`ruby-position`）与横向贴哪一段（`ruby-align`）。
        // 收下来的值由 `layout::wrap` 折 ruby 容器那一段消费，所以这两条不是「记成接住」。
        // EPUB 与 WebKit 的前缀写法折成同一个数 —— 转换器两种都写，分成两条属性就是让
        // 同一本书的注音在两种前缀下走两条路。
        "ruby-position" | "-epub-ruby-position" | "-webkit-ruby-position" => {
            match value.to_ascii_lowercase().as_str() {
                "over" => {
                    style.ruby_position = RubyPosition::Over;
                    true
                }
                "under" => {
                    style.ruby_position = RubyPosition::Under;
                    true
                }
                // `inter-character` 要的是「沿字轴错开半格」的另一套几何，不收。
                _ => false,
            }
        }
        "ruby-align" | "-epub-ruby-align" | "-webkit-ruby-align" => {
            match value.to_ascii_lowercase().as_str() {
                "auto" => {
                    style.ruby_align = RubyAlign::Auto;
                    true
                }
                "start" => {
                    style.ruby_align = RubyAlign::Start;
                    true
                }
                "center" => {
                    style.ruby_align = RubyAlign::Center;
                    true
                }
                "end" => {
                    style.ruby_align = RubyAlign::End;
                    true
                }
                // `justify` / `distribute` / `space-between` / `space-around` 改的是注音自己
                // 逐字之间的字距，判不认识（理由写在 [`RubyAlign`] 的文档上）。
                _ => false,
            }
        }
        // `-webkit-` 前缀那两条是同一个属性的旧拼法（Kindle 与多看导出时都写它）。读客那
        // 几本书每个块上都写一遍 `-webkit-text-decoration: none`，不认它们等于把 660 个
        // 元素记成「落差」。
        "text-decoration"
        | "text-decoration-line"
        | "-webkit-text-decoration"
        | "-webkit-text-decoration-line" => match parse_decoration(value) {
            Some((underline, line_through)) => {
                style.underline = underline;
                style.line_through = line_through;
                // 简写里的线型与颜色（`underline wavy #f00`）只在真简写上认：
                // `text-decoration-line` 按规范只收线种，不能顺手把 `wavy` 吃掉。
                if matches!(
                    decl.property.as_str(),
                    "text-decoration" | "-webkit-text-decoration"
                ) && (underline || line_through)
                {
                    // 简写没写的部分按初值重置（`solid` / 跟字色），与 CSS 一致；
                    // 不然前一条 `text-decoration-style: wavy` 会漏到这一条后面。
                    style.decoration_style = DecorationStyle::Solid;
                    style.decoration_color = None;
                    for token in value.split_whitespace() {
                        match token.to_ascii_lowercase().as_str() {
                            "double" => style.decoration_style = DecorationStyle::Double,
                            "dotted" => style.decoration_style = DecorationStyle::Dotted,
                            "dashed" => style.decoration_style = DecorationStyle::Dashed,
                            "wavy" => style.decoration_style = DecorationStyle::Wavy,
                            "solid" => style.decoration_style = DecorationStyle::Solid,
                            _ => {}
                        }
                        if let Some(color) = parse_color(token) {
                            style.decoration_color = Some(color);
                        }
                    }
                }
                true
            }
            None => false,
        },
        "font" => apply_font_shorthand(style, parent, root_px, value),
        // `object-fit` 只影响绘制层怎么把图采样进盒里；排版期用它不到（盒由 `image_size` 定）。
        // `duokan-bleed`：多看/掌阅的**出血**扩展，意思是「这张图铺到屏幕边、不要页边距」。
        // 落成 [`ComputedStyle::bleed`]，由排版期按屏幕宽摆（与「大图铺满」同一套尺）；
        // 词表可连写也可带空格。
        "duokan-bleed" => match parse_bleed(value) {
            Some(edge) => {
                style.bleed = edge;
                true
            }
            None => false,
        },
        "object-fit" => match value.trim().to_ascii_lowercase().as_str() {
            "fill" => {
                style.object_fit = ObjectFit::Fill;
                true
            }
            "contain" => {
                style.object_fit = ObjectFit::Contain;
                true
            }
            "cover" => {
                style.object_fit = ObjectFit::Cover;
                true
            }
            "none" => {
                style.object_fit = ObjectFit::None;
                true
            }
            "scale-down" => {
                style.object_fit = ObjectFit::ScaleDown;
                true
            }
            _ => false,
        },
        // `box-shadow: none` 是**初值**，认它等于什么都不做 —— 但「什么都不做」与「不认识」
        // 在落差表上是两栏：读客那几本书在 664 个元素上写了一遍 `none`（还有 `-webkit-`
        // 那份），不认就凭空多出 664 条「落差」。真阴影这一版不画（它不进 `ComputedStyle`），
        // 所以非 `none` 的值判不认识，交给 [`super::KNOWN_UNIMPLEMENTED`] 之外的统计。
        "box-shadow" | "-webkit-box-shadow" if value.trim().eq_ignore_ascii_case("none") => true,
        "break-before" | "page-break-before" | "-webkit-page-break-before" => {
            match parse_break_when(value) {
                Some(when) => {
                    style.break_before = when;
                    true
                }
                None => false,
            }
        }
        "break-after" | "page-break-after" | "-webkit-page-break-after" => {
            match parse_break_when(value) {
                Some(when) => {
                    style.break_after = when;
                    true
                }
                None => false,
            }
        }
        "break-inside" | "page-break-inside" | "-webkit-page-break-inside" => {
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
        "background-size" | "-webkit-background-size" => {
            match parse_background_size_list(value, own, root_px) {
                Some(list) => {
                    let count = list.len();
                    for_each_layer(style, count, |layer, i| layer.size = list[i]);
                    true
                }
                None => false,
            }
        }
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
        // `background-attachment`：`fixed` 只对**章节根**有排版含义（整页底图至少一屏高，
        // 见 `layout::layout_inner`），其余元素上它与 `scroll` 同效；`local` 要的是
        // 「按这一块的内容盒滚动」，而流式块没有自己的滚动区。这里照样收下：
        // 记成「不生效」会把根上那一条真正生效的声明也一起埋掉。
        "background-attachment" => match parse_background_attachment_list(value) {
            Some(list) => {
                let count = list.len();
                for_each_layer(style, count, |layer, i| layer.attachment = list[i]);
                true
            }
            None => false,
        },
        // `background-clip` / `background-origin` 按边框盒画就够（判据在
        // `KNOWN_UNIMPLEMENTED`）。简写里仍**吃掉**那些词：`background: url(x) … fixed`
        // 的 `fixed` 是真语法，不认整条简写就作废、底色与图一起消失；长手单写时屏上无变化，
        // 量具因此必须说真话。
        // `background` 简写接住**颜色**与**图**两截，丢掉就整块消失；没写到的子属性拉回初值
        // （判据在 `apply_background_shorthand`）。`initial` 只收简写这一档：整条通用路太大。
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
        // 近似一处：我们的 `display` 初值是 `Block`（CSS 是 `inline`），`li` 会退回块、
        // 浏览器会收掉标记。真书里这条只出现在页眉页脚重置里且紧跟 `display: block`，差不到屏上。
        // `link` 不是 CSS 属性（是「这段在不在 `<a>` 里」），重置时留着。
        "all" if value.eq_ignore_ascii_case("initial") => {
            let link = style.link;
            *style = ComputedStyle::default();
            style.link = link;
            true
        }
        // 定位。`static` 是初值；`absolute` / `fixed` 脱流并按四个偏移摆（见 [`Positioning`]）；
        // `relative` 留在流内，只把自己那一坨整体挪一下（偏移读在 `layout::collect`）。
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
            "relative" => {
                style.position = Positioning::Relative;
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
            // 表格四档由 `layout::table` 抽网格、分列宽、摆格子；`inline-table` 与
            // `inline-block` 一样按行内流摊平（行内原子盒的 shrink-to-fit 未做）。
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
            // `display: flex` / `inline-flex` 2026-11-23 接上：走 `layout::flex` 那套
            // 主轴 + 交叉轴布局。inline-flex 一期没有行内块，落点仍按块排，但子项的
            // 排列交给 Flex 引擎。
            "flex" => {
                style.display = Display::Flex;
                true
            }
            "inline-flex" => {
                style.display = Display::InlineFlex;
                true
            }
            "inline-block" => {
                style.display = Display::InlineBlock;
                true
            }
            "inline-table" => {
                style.display = Display::InlineTable;
                true
            }
            "grid" => {
                style.display = Display::Grid;
                true
            }
            "inline-grid" => {
                style.display = Display::InlineGrid;
                true
            }
            // `contents` 仍是块：排版层没有「子元素代父」那一档，按块摆最接近。
            "contents" => {
                style.display = Display::Block;
                true
            }
            _ => false,
        },
        // ---- Flex 容器属性。CSS 规定这些不继承，值写错整条作废 ----
        "flex-direction" => match value.to_ascii_lowercase().as_str() {
            "row" => {
                style.flex_direction = FlexDirection::Row;
                true
            }
            "column" => {
                style.flex_direction = FlexDirection::Column;
                true
            }
            _ => false,
        },
        "flex-wrap" => match value.to_ascii_lowercase().as_str() {
            "nowrap" => {
                style.flex_wrap = FlexWrap::NoWrap;
                true
            }
            "wrap" => {
                style.flex_wrap = FlexWrap::Wrap;
                true
            }
            _ => false,
        },
        "align-self" => match value.to_ascii_lowercase().as_str() {
            "auto" => {
                style.align_self = None;
                true
            }
            "stretch" => {
                style.align_self = Some(AlignItems::Stretch);
                true
            }
            "start" => {
                style.align_self = Some(AlignItems::Start);
                true
            }
            "end" => {
                style.align_self = Some(AlignItems::End);
                true
            }
            "center" => {
                style.align_self = Some(AlignItems::Center);
                true
            }
            _ => false,
        },
        "align-items" => match value.to_ascii_lowercase().as_str() {
            "stretch" => {
                style.align_items = AlignItems::Stretch;
                true
            }
            "flex-start" | "start" => {
                style.align_items = AlignItems::Start;
                true
            }
            "flex-end" | "end" => {
                style.align_items = AlignItems::End;
                true
            }
            "center" => {
                style.align_items = AlignItems::Center;
                true
            }
            _ => false,
        },
        "justify-content" => match value.to_ascii_lowercase().as_str() {
            "flex-start" | "start" => {
                style.justify_content = JustifyContent::Start;
                true
            }
            "flex-end" | "end" => {
                style.justify_content = JustifyContent::End;
                true
            }
            "center" => {
                style.justify_content = JustifyContent::Center;
                true
            }
            "space-between" => {
                style.justify_content = JustifyContent::SpaceBetween;
                true
            }
            _ => false,
        },
        // `gap: <row> [<column>]`：一列值时横纵相同，两列值时分别给 row/column。
        // `normal` 是初值，落到 `None`（不插缝）。
        "gap" => {
            let parts: Vec<&str> = value.split_whitespace().collect();
            if parts.is_empty() || parts.len() > 2 {
                return false;
            }
            let parse_one = |p: &str| -> Option<Option<Length>> {
                if p.eq_ignore_ascii_case("normal") {
                    Some(None)
                } else {
                    edge_length(p, own, root_px, false).map(Some)
                }
            };
            let row = match parse_one(parts[0]) {
                Some(v) => v,
                None => return false,
            };
            let col = if parts.len() == 2 {
                match parse_one(parts[1]) {
                    Some(v) => v,
                    None => return false,
                }
            } else {
                row
            };
            style.row_gap = row;
            style.column_gap = col;
            true
        }
        "row-gap" => match edge_length(value, own, root_px, false) {
            Some(v) => {
                style.row_gap = Some(v);
                true
            }
            None => false,
        },
        "column-gap" => match edge_length(value, own, root_px, false) {
            Some(v) => {
                style.column_gap = Some(v);
                true
            }
            None => false,
        },
        // ---- Flex 子项属性 ----
        "flex-grow" => match value.trim().parse::<f32>() {
            Ok(v) if v >= 0.0 => {
                style.flex_grow = v;
                true
            }
            _ => false,
        },
        "flex-shrink" => match value.trim().parse::<f32>() {
            Ok(v) if v >= 0.0 => {
                style.flex_shrink = v;
                true
            }
            _ => false,
        },
        "flex-basis" if value.eq_ignore_ascii_case("auto") => {
            style.flex_basis = None;
            true
        }
        "flex-basis" => match set_length(value, own, root_px, &mut style.flex_basis) {
            true => true,
            false => false,
        },
        // `flex: <grow> <shrink> <basis>` 简写。只收 1–3 个 token 的常见形态：
        // 单值若是长度当 basis，若是数字当 grow（shrink=1、basis=auto）。
        "flex" => apply_flex_shorthand(style, own, root_px, value),
        "z-index" => match value.trim() {
            "auto" => {
                style.z_index = 0;
                true
            }
            v => match v.parse::<i32>() {
                Ok(n) => {
                    style.z_index = n;
                    true
                }
                Err(_) => false,
            },
        },
        "opacity" => match value.trim().parse::<f32>() {
            Ok(v) if (0.0..=1.0).contains(&v) => {
                style.opacity = v;
                true
            }
            _ => false,
        },
        "transform" => apply_transform(style, own, root_px, value),
        // ---- text-decoration-style / color（2026-11-23）----
        "text-decoration-style" | "-webkit-text-decoration-style" => {
            match value.to_ascii_lowercase().as_str() {
                "solid" => {
                    style.decoration_style = DecorationStyle::Solid;
                    true
                }
                "double" => {
                    style.decoration_style = DecorationStyle::Double;
                    true
                }
                "dotted" => {
                    style.decoration_style = DecorationStyle::Dotted;
                    true
                }
                "dashed" => {
                    style.decoration_style = DecorationStyle::Dashed;
                    true
                }
                "wavy" => {
                    style.decoration_style = DecorationStyle::Wavy;
                    true
                }
                _ => false,
            }
        }
        "text-decoration-color" => match parse_color(value.trim()) {
            Some(c) => {
                style.decoration_color = Some(c);
                true
            }
            None => false,
        },
        // ---- text-emphasis 族 ----
        "text-emphasis" | "-webkit-text-emphasis" | "-epub-text-emphasis" => {
            apply_text_emphasis(style, value)
        }
        "text-emphasis-style" | "-webkit-text-emphasis-style" => {
            apply_text_emphasis_style(style, value)
        }
        "text-emphasis-color" => match parse_color(value.trim()) {
            Some(c) => {
                style.text_emphasis_color = Some(c);
                true
            }
            None => false,
        },
        "text-emphasis-position"
        | "-webkit-text-emphasis-position"
        | "-epub-text-emphasis-position" => {
            let low = value.to_ascii_lowercase();
            // `under [right]` / `over [left]`：只看第一个定位词，左右是竖排的次轴。
            if low.starts_with("under") {
                style.text_emphasis_position = EmphasisPosition::Under;
                true
            } else if low.starts_with("over") {
                style.text_emphasis_position = EmphasisPosition::Over;
                true
            } else {
                false
            }
        }
        // ---- box-shadow（含 inset / 多值）----
        "box-shadow" | "-webkit-box-shadow" => apply_box_shadow(style, value, own, root_px),
        // ---- outline / outline-offset ----
        "outline" => apply_outline(style, value, own, root_px),
        "outline-offset" => {
            match edge_length(value.trim(), own, root_px, false) {
                Some(l) => {
                    let offset = l.resolve(own);
                    // 没写过 outline 时先存一个**宽 0** 的占位：宽 0 不画，但保住了 offset，
                    // 后面的 `outline` 会沿用。这样 `outline-offset` 单独一条也算接住。
                    match &mut style.outline {
                        Some((_, _, _, o)) => *o = offset,
                        None => style.outline = Some((0.0, None, BorderStyle::Solid, offset)),
                    }
                    true
                }
                None => false,
            }
        }
        // ---- overflow / overflow-x / overflow-wrap ----
        "overflow" | "overflow-x" | "overflow-y" => {
            let which = if decl.property.eq_ignore_ascii_case("overflow-x") {
                0
            } else if decl.property.eq_ignore_ascii_case("overflow-y") {
                1
            } else {
                2
            };
            let overflow = match value.to_ascii_lowercase().as_str() {
                "visible" => Overflow::Visible,
                "hidden" => Overflow::Hidden,
                "clip" => Overflow::Clip,
                "scroll" => Overflow::Scroll,
                "auto" => Overflow::Auto,
                _ => return false,
            };
            match which {
                0 => style.overflow_x = overflow,
                1 => style.overflow_y = overflow,
                _ => {
                    style.overflow_x = overflow;
                    style.overflow_y = overflow;
                }
            }
            true
        }
        "overflow-wrap" | "word-wrap" => match value.to_ascii_lowercase().as_str() {
            "break-word" | "anywhere" => {
                style.overflow_wrap_break = true;
                true
            }
            "normal" => {
                style.overflow_wrap_break = false;
                true
            }
            _ => false,
        },
        // ---- visibility / table-layout / text-size-adjust ----
        "visibility" => match value.to_ascii_lowercase().as_str() {
            "visible" => {
                style.visibility = Visibility::Visible;
                true
            }
            "hidden" => {
                style.visibility = Visibility::Hidden;
                true
            }
            "collapse" => {
                style.visibility = Visibility::Collapse;
                true
            }
            _ => false,
        },
        "table-layout" => match value.to_ascii_lowercase().as_str() {
            "fixed" => {
                style.table_layout_fixed = true;
                true
            }
            "auto" => {
                style.table_layout_fixed = false;
                true
            }
            _ => false,
        },
        // `text-transform`：只收三档不改字节长度的。其余值判不认识。
        "text-transform" => match value.to_ascii_lowercase().as_str() {
            "none" => {
                style.text_transform = TextTransform::None;
                true
            }
            "uppercase" => {
                style.text_transform = TextTransform::Uppercase;
                true
            }
            "lowercase" => {
                style.text_transform = TextTransform::Lowercase;
                true
            }
            "capitalize" => {
                style.text_transform = TextTransform::Capitalize;
                true
            }
            _ => false,
        },
        "text-size-adjust" | "-webkit-text-size-adjust" => {
            match value.trim() {
                "auto" | "none" => {
                    // auto/none 是不缩放的提示，落 None（初值）。
                    style.text_size_adjust = None;
                    true
                }
                v => match v.trim_end_matches('%').parse::<f32>() {
                    Ok(pct) => {
                        style.text_size_adjust = Some(pct / 100.0);
                        true
                    }
                    Err(_) => false,
                },
            }
        }
        // ---- quotes（两对开合字符）----
        "quotes" => apply_quotes(style, value),
        // `content`：生成盒的文字。open-quote/close-quote 在 cascade 里按继承的
        // quotes 折成字面字符（见 `computed::cascade`），这里先记下意图。
        "content" => apply_content(style, value),
        // ---- grid 定位（轨道定义 + 子项显式线号）----
        "grid-template-columns" => apply_grid_tracks(style, value, own, root_px),
        "grid-column" => apply_grid_line(style, true, value),
        "grid-row" => apply_grid_line(style, false, value),
        "justify-self" => match value.to_ascii_lowercase().as_str() {
            "auto" => {
                style.justify_self = JustifySelf::Auto;
                true
            }
            "start" => {
                style.justify_self = JustifySelf::Start;
                true
            }
            "end" => {
                style.justify_self = JustifySelf::End;
                true
            }
            "center" => {
                style.justify_self = JustifySelf::Center;
                true
            }
            "stretch" => {
                style.justify_self = JustifySelf::Stretch;
                true
            }
            _ => false,
        },
        // `color: currentColor` 就是 `inherit`（自引用按继承折，CSS 明文）。
        "color" if value.eq_ignore_ascii_case("currentcolor") => {
            style.color = parent.color;
            true
        }
        // `initial` 是 CSS 的初始值。这里落成 `None` 而不是写死黑色：`None` 的意思
        // 正是「读者主题的正文色」，暗色主题下写死黑字等于把这一行藏掉。
        "color" if value.eq_ignore_ascii_case("initial") => {
            style.color = None;
            true
        }
        "color" => match parse_color(value) {
            Some(c) => {
                style.color = Some(c);
                true
            }
            None => false,
        },
        // ---- SVG 呈现属性。CSS 的 `fill` 与 `object-fit` 的 `fill` 值同名，但这里
        //      分发的是属性名，不冲突。只在序列化内联 `<svg>` 时消费。 ----
        "fill" | "stroke" => {
            let paint = if value.eq_ignore_ascii_case("none") {
                Some(SvgPaint::None)
            } else {
                parse_color(value).map(SvgPaint::Color)
            };
            match paint {
                Some(paint) => {
                    if decl.property == "fill" {
                        style.svg_fill = paint;
                    } else {
                        style.svg_stroke = paint;
                    }
                    true
                }
                None => false,
            }
        }
        "stroke-width" => match value.trim_end_matches("px").trim().parse::<f32>() {
            Ok(width) if width >= 0.0 => {
                style.svg_stroke_width = width;
                true
            }
            _ => false,
        },
        "stroke-linecap" => match value.to_ascii_lowercase().as_str() {
            "butt" => {
                style.svg_stroke_linecap = SvgLineCap::Butt;
                true
            }
            "round" => {
                style.svg_stroke_linecap = SvgLineCap::Round;
                true
            }
            "square" => {
                style.svg_stroke_linecap = SvgLineCap::Square;
                true
            }
            _ => false,
        },
        "stroke-linejoin" => match value.to_ascii_lowercase().as_str() {
            "miter" => {
                style.svg_stroke_linejoin = SvgLineJoin::Miter;
                true
            }
            "round" => {
                style.svg_stroke_linejoin = SvgLineJoin::Round;
                true
            }
            "bevel" => {
                style.svg_stroke_linejoin = SvgLineJoin::Bevel;
                true
            }
            _ => false,
        },
        // `word-break`：只认 `normal` 与 `break-all`。`keep-all` 判不认识（见 [`WordBreak`]）。
        "word-break" => match value.to_ascii_lowercase().as_str() {
            "normal" => {
                style.word_break = WordBreak::Normal;
                true
            }
            "break-all" | "break-word" => {
                // `break-word` 是 WebKit 旧写法，等价于 `overflow-wrap: break-word` +
                // `word-break: normal`；这里折成 break-all 是最接近的既有断档。
                style.word_break = WordBreak::BreakAll;
                true
            }
            _ => false,
        },
        "white-space" => match value.to_ascii_lowercase().as_str() {
            "normal" => {
                style.white_space_pre = false;
                style.white_space_nowrap = false;
                true
            }
            "pre" | "pre-wrap" => {
                style.white_space_pre = true;
                style.white_space_nowrap = false;
                true
            }
            "nowrap" => {
                // 合并空白但不换行：与 pre 的正交位，断行阶段据此整段不折。
                style.white_space_pre = false;
                style.white_space_nowrap = true;
                true
            }
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
        "box-sizing" | "-webkit-box-sizing" => match value.to_ascii_lowercase().as_str() {
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
        // 这两个属性**本来就按初值在做**（`list-style-image: none` 无图源、
        // `list-style-position: outside` 标记挂在流动外），所以初值判接住、其余判不认识 ——
        // 判据是「效果等于默认行为才算接住」。
        // `font-variant` 只收 `normal`（初值）与 `small-caps`（排版层大写 + 0.8 字号），
        // 其余（`petite-caps` 等）判不认识，同一条规矩。
        "font-variant" | "font-variant-caps" => match value.to_ascii_lowercase().as_str() {
            "normal" => {
                style.small_caps = false;
                true
            }
            "small-caps" => {
                style.small_caps = true;
                true
            }
            _ => false,
        },
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
