//! 继承链：元素节点从父样式带下哪些属性，其余从默认值起算。
//!
//! 从 `css::computed::apply` 切出来只因为 apply.rs 贴着行数上限。

use super::super::ComputedStyle;

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
        font_stretch: parent.font_stretch,
        // `font-variant` 在 CSS 里可继承：书里常在 `<body>` 或整段上写一次。
        small_caps: parent.small_caps,
        font_family: parent.font_family.clone(),
        font_families: parent.font_families.clone(),
        // 字体的三个 OpenType 开关在 CSS 里都**可继承**：书里通常在 `<body>` 上写一次
        // `font-kerning: none` 或 `font-feature-settings: "palt" 1` 就管全书。
        font_kerning: parent.font_kerning,
        font_features: parent.font_features.clone(),
        text_rendering: parent.text_rendering,
        text_align: parent.text_align,
        // `text-justify` 在 CSS 里可继承：书里在 `<body>` 上写一次 `inter-ideograph`
        // 就管全书，靠的正是这一行。
        text_justify: parent.text_justify,
        image_scale: parent.image_scale,
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
        // `cursor` 在 CSS 里可继承：写在 `<body>` 或链接规则上一次就管一片。
        cursor: parent.cursor,
        text_indent: parent.text_indent,
        word_spacing: parent.word_spacing,
        // 字距在 CSS 里可继承（`letter-spacing` 是 inheritable），书里常在 `<body>` 上写一次。
        letter_spacing: parent.letter_spacing,
        // `word-break` 在 CSS 里可继承：书里在 `<body>` 上写一次就管全书。
        word_break: parent.word_break,
        // ruby 两条在 CSS 里都可继承：写在 `<ruby>` 上管到里面的 `<rt>`，写在 `<body>`
        // 上就管全书。落点读的是容器自己那一份，继承断了等于初值兜场。
        ruby_align: parent.ruby_align,
        ruby_position: parent.ruby_position,
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
        // 可继承的排版属性：white-space / overflow-wrap / text-emphasis / visibility / quotes
        // 写在 `<body>` 上一次就管全书，靠 inherit_from 这一条链带下去。
        white_space_nowrap: parent.white_space_nowrap,
        overflow_wrap_break: parent.overflow_wrap_break,
        text_emphasis: parent.text_emphasis,
        text_emphasis_color: parent.text_emphasis_color,
        text_emphasis_position: parent.text_emphasis_position,
        visibility: parent.visibility,
        // `text-transform` 在 CSS 里可继承：写在 `<body>` 上一次就管全书。
        text_transform: parent.text_transform,
        // `text-combine-upright` / `-webkit-text-combine` 同样可继承。
        text_combine: parent.text_combine,
        quotes: parent.quotes,
        // SVG 呈现属性按 CSS/SVG 规定可继承：写在 `<svg>` 根上的 stroke 要落到每个 `<path>`。
        svg_fill: parent.svg_fill,
        svg_stroke: parent.svg_stroke,
        svg_stroke_width: parent.svg_stroke_width,
        svg_stroke_linecap: parent.svg_stroke_linecap,
        svg_stroke_linejoin: parent.svg_stroke_linejoin,
        // 装饰线在 CSS 里**不是继承属性**，但会向后代**传播**（`text-decoration: none`
        // 写在子元素上并不会去掉父元素的下划线，这是那条规则最有名的坑）。对我们的
        // 行内/块级盒来说，「传播」与「继承」算出来的结果一致，所以这里按继承实现，
        // 差别只留在原子行内盒（inline-block、图片）上 —— 那种盒一期没有内容会吃到
        // 父级装饰。
        underline: parent.underline,
        line_through: parent.line_through,
        // 形状与颜色跟着同一条「传播按继承近似」走：子元素只继承了下划线布尔，
        // 而线还是子元素那份样式画的，不带上这两格就会把波浪线画成实线、红线变字色。
        decoration_style: parent.decoration_style,
        decoration_color: parent.decoration_color,
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
