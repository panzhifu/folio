//! 把命中的声明算成 `ComputedStyle`：一条声明进哪几个字段 + 继承 + 沿树展开。
//!
//! 单个值怎么解成 Rust 在 [`super::value`]；这里只管属性名到字段的对应关系。
//!
//! 支持的属性就是 `css/computed.rs` 顶部那份清单。清单外的声明**被忽略而不是报错**，
//! 因为真实出版书里总有些用不上的声明，为一两个 `transform` 让整本书排版失败
//! 是最糟的失败方式。

use crate::dom::{Dom, NodeId, NodeKind};

use super::value::{
    BorderElement, border_target, border_width_px, box_sides, four_of, parse_baseline_shift,
    parse_border_style, parse_break_when, parse_color, parse_count, parse_decoration,
    parse_font_family_list, parse_font_size, parse_font_weight, parse_line_height, relative_weight,
    set_length, set_margin, set_px,
};
use super::{Declaration, RuleSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Right,
    Center,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Display {
    #[default]
    Block,
    Inline,
    InlineBlock,
    ListItem,
    /// `display: none`：排版层负责整块跳过。
    None,
}

/// 一条边的画法。CSS 的 `border-style` 有八种实线以外的样式；这里只接
/// `none` / `hidden`（都不画）与 `solid` / `double` —— 出版书的章标题色带左边那道
/// 双线就是 `double`，正文插图框是 `solid`。`dashed` / `dotted` / `groove` 这些
/// 解析失败、按「没接住」计数：把它们悄悄画成实线，书里的虚线框就变成了另一回事。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderStyle {
    #[default]
    None,
    Solid,
    Double,
}

/// 一条边的三要素。CSS 的 `border-style: none` 时宽度**按 0 参与盒模型**，
/// 所以「画不画」不能只看 `width`：`border-width: 5px` 而没写 style 的声明
/// 在浏览器里就是一条不存在的边。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BorderEdge {
    pub width: f32,
    pub style: BorderStyle,
    /// 没写就是 `currentColor`：留 `None`，由绘制层取这一块的 `color`
    /// （`color` 可继承，所以那一色在层叠期不一定写在本元素上）。
    pub color: Option<Color>,
}

impl BorderEdge {
    /// 这一边在盒模型里占多宽：不画就是 0。
    pub fn drawn(&self) -> f32 {
        match self.style {
            BorderStyle::None => 0.0,
            _ if self.width > 0.0 => self.width,
            _ => 0.0,
        }
    }
}

impl ComputedStyle {
    /// 四条边各自在盒模型里占多宽，顺序 `[上, 右, 下, 左]`。
    ///
    /// 走 [`BorderEdge::drawn`] 而不是直接取 `width`：`border-style` 是 `none` 时这一边
    /// 按 0 参与计算，所以书里那句只写了 `border-width: 20px` 而没配 style 的声明，
    /// 不该把正文挤窄 40px。
    pub fn border_drawn(&self) -> [f32; 4] {
        self.border.map(|edge| edge.drawn())
    }
}

/// 连字符相关的折行意图，来自 `hyphens`（含 EPUB 的 `-epub-` / `-webkit-` 前缀写法）。
///
/// 一期没有连字词典，所以 `Auto` 与 `Manual` 的**行为**相同：都只在已有的连字符处断，
/// 不会把 `sais` 拆成 `sa-is`。差别留在「作者的意图有没有被记下来」——词典接上时不用
/// 回头改书的样式表。这条边界记在 `plans/facts.md` 的 CSS 一节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hyphens {
    /// CSS 初值：已有连字符可断，不主动造连字符。
    #[default]
    Manual,
    /// 连字符**不是**断点：`je-ne-sais-quoi` 整个留着。
    None,
    /// 请求词典连字；一期按 `Manual` 执行。
    Auto,
}

/// 分片（分页 / 分栏）时的断点意图。
///
/// `Left` / `Right` 会补出空片占位，「双页对开」的左右页才不会错位；
/// `Always` 是 CSS2.1 的旧写法，与 `Page` 同义 —— 老 EPUB 里满地都是，
/// 认不出它就等于把这些书的作者意图丢掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BreakWhen {
    /// 没有声明：由剩余可用高度决定。
    #[default]
    Auto,
    /// 这里尽量不断（`break-after: avoid` 常见于标题，意思是别把标题孤零零留在页尾）。
    Avoid,
    Page,
    Left,
    Right,
    Always,
}

impl BreakWhen {
    /// 是不是「强制断在这里」的意图。`Avoid` 与 `Auto` 都不是。
    pub fn is_forced(self) -> bool {
        matches!(self, Self::Page | Self::Left | Self::Right | Self::Always)
    }

    /// 强制断之后应当落在哪一侧。`None` = 不关心奇偶。
    pub fn parity(self) -> Option<bool> {
        match self {
            Self::Left => Some(false),
            Self::Right => Some(true),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// `font-family` 的**整条候选链**，按书写顺序；`font_family` 就是它的第一个。
    ///
    /// 只留第一个是以前的一处真落差：书里写 `font-family: "MyFont", "Noto Serif CJK SC"`
    /// 时，设备上没装 `MyFont` 就直接落回读者的字体，作者排在第二位的那一份明明装着。
    /// 逐个试的工作在平台层做（只有它知道这台设备注册了哪些家族，见 `ui::reader::measure`）。
    pub font_families: Vec<String>,
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
    /// `background-color`。`None` = 书里没写过这一条（CSS 的初值是 `transparent`，
    /// 与「写了一个颜色」在绘制上是两件事：前者什么都不画，后者要盖住读者的主题底色）。
    /// 背景色在 CSS 里**不继承**，所以 `inherit_from` 不带它。
    pub background_color: Option<Color>,
    /// 四边边框，顺序 `[上, 右, 下, 左]`（与 `box_sides` 同一套展开规则）。
    /// 布局拿它算盒宽与盒高，绘制拿它画线；不继承。
    pub border: [BorderEdge; 4],
    pub letter_spacing: f32,
    /// 词间额外宽度（px）。CSS 初值 `normal` 折成 0；它叠在**每个空格之后**，
    /// 所以折行与行宽都要算它（见 `layout::run_width`）。
    pub word_spacing: f32,
    /// 基线偏移，px，**正数往上**。`vertical-align: super` 折出来的就是它。
    ///
    /// 0 意味着「贴基线」（CSS 的 `baseline`）。`top` / `bottom` / `middle` 要的是
    /// 行盒对齐机制，一期不折成这个数，并且按不认识计数 —— 悄悄当 0 会把作者的
    /// 上标写成正常位置还查不出来。
    pub baseline_shift: f32,
    /// `text-decoration` 的下划线与删除线。绘制层用它画线；CSS 里装饰线**不计入行盒高**，
    /// 所以排版层只用它透传到 run 上。
    pub underline: bool,
    pub line_through: bool,
    pub white_space_pre: bool,
    /// 替换元素（图片）的尺寸。块级元素的 width 一期不做，只有图片用得上。
    pub width: Option<Length>,
    pub height: Option<Length>,
    pub max_width: Option<Length>,
    pub max_height: Option<Length>,
    /// 分片意图。见 `fragment` 模块：连续滚动 / 分页 / 双页对开共用同一套分片代码，
    /// 差别只是「可用高度」这一个数。
    pub break_before: BreakWhen,
    pub break_after: BreakWhen,
    /// `break-inside: avoid`：整块尽量不被切开。装不下一整页时仍然必须切开 ——
    /// 排不出版来比违反作者意图更糟。
    pub break_inside_avoid: bool,
    /// 一段被切开后，前一片至少要留几行。CSS 初值 2。
    pub orphans: u16,
    /// 一段被切开后，后一片至少要留几行。CSS 初值 2。
    pub widows: u16,
    /// 连字符能不能当断点。见 [`Hyphens`]；CSS 里它是**可继承**的，
    /// 所以书在 `<body>` 上写一次 `hyphens: none` 就管全书。
    pub hyphens: Hyphens,
    /// `::first-letter` 算出来的那一份，只在书写过首字符样式时才有。
    ///
    /// 装的是**完整的 ComputedStyle** 而不是几个字段：首字符要的东西就是「一套字体」，
    /// 以后 `font-family` 的候选链、字距这些加进来不用再动这里。
    /// 排版层拿它给首字符单成一个 run，并据此决定它压住几行（`layout` 里的 `drop_cap`）。
    pub first_letter: Option<Box<ComputedStyle>>,
}

impl Default for ComputedStyle {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            line_height: 1.75,
            font_weight: 400,
            font_italic: false,
            font_family: String::new(),
            font_families: Vec::new(),
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
            background_color: None,
            border: [BorderEdge::default(); 4],
            letter_spacing: 0.0,
            word_spacing: 0.0,
            baseline_shift: 0.0,
            underline: false,
            line_through: false,
            white_space_pre: false,
            width: None,
            height: None,
            max_width: None,
            max_height: None,
            break_before: BreakWhen::Auto,
            break_after: BreakWhen::Auto,
            break_inside_avoid: false,
            // 2 是 CSS 的初值，不是我们挑的：一行的孤行（或寡行）在任何排版传统里都算缺陷。
            orphans: 2,
            widows: 2,
            hyphens: Hyphens::Manual,
            first_letter: None,
        }
    }
}

/// 手写 `Hash`：块级排版缓存要按「**这个块子树的样式**」判命中（改一个块的样式，
/// 只有那一块失效），所以需要一份能覆盖**每个字段**的样式哈希。`ComputedStyle` 带
/// `f32`，不能 derive `Hash`，于是逐字段手写。
///
/// **新增字段时必须在这里补一行**，否则漏掉的字段改了而哈希不变，块级缓存会读到过期结果。
/// 浮点走 `to_bits`：`0.0` 与 `-0.0` 会被当成不同的值，最坏也只是多一次未命中，不会错命中。
impl std::hash::Hash for ComputedStyle {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        fn f<H: std::hash::Hasher>(x: f32, h: &mut H) {
            x.to_bits().hash(h);
        }
        fn opt_len<H: std::hash::Hasher>(l: &Option<Length>, h: &mut H) {
            match l {
                None => 0u8.hash(h),
                Some(Length::Px(v)) => {
                    1u8.hash(h);
                    f(*v, h);
                }
                Some(Length::Percent(p)) => {
                    2u8.hash(h);
                    f(*p, h);
                }
            }
        }
        f(self.font_size, h);
        f(self.line_height, h);
        self.font_weight.hash(h);
        self.font_italic.hash(h);
        self.font_family.hash(h);
        self.font_families.hash(h);
        (self.text_align as u8).hash(h);
        f(self.text_indent, h);
        f(self.margin_top, h);
        f(self.margin_bottom, h);
        f(self.margin_left, h);
        f(self.margin_right, h);
        f(self.padding_top, h);
        f(self.padding_bottom, h);
        f(self.padding_left, h);
        f(self.padding_right, h);
        (self.display as u8).hash(h);
        self.color.hash(h);
        self.background_color.hash(h);
        for edge in &self.border {
            f(edge.width, h);
            (edge.style as u8).hash(h);
            edge.color.hash(h);
        }
        f(self.letter_spacing, h);
        f(self.word_spacing, h);
        f(self.baseline_shift, h);
        self.underline.hash(h);
        self.line_through.hash(h);
        self.white_space_pre.hash(h);
        opt_len(&self.width, h);
        opt_len(&self.height, h);
        opt_len(&self.max_width, h);
        opt_len(&self.max_height, h);
        (self.break_before as u8).hash(h);
        (self.break_after as u8).hash(h);
        self.break_inside_avoid.hash(h);
        self.orphans.hash(h);
        self.widows.hash(h);
        (self.hyphens as u8).hash(h);
        match &self.first_letter {
            None => 0u8.hash(h),
            Some(inner) => {
                1u8.hash(h);
                inner.hash(h);
            }
        }
    }
}

/// 认识但一期不实现：接受、不生效、不计入 ignored。
///
/// 这一份和下面 `inherit_from` 的区别在于：这些属性**真实出版书里满地都是**
/// （`width`、`text-decoration`、`float`），若按不支持计数，ignored 会被刷到
/// 几百条，真正需要关注的「我们没预料到的写法」就淹没了。
const KNOWN_UNIMPLEMENTED: &[&str] = &[
    // `background` 简写还能带图（`background: #fff url(x.png)`），图那条一期不接；
    // 单条 `background-color` 已经生效，不再在这份名单里。
    "background",
    "min-height",
    "text-shadow",
    "direction",
    "writing-mode",
    // `orphans` / `widows` / `break-inside` / `page-break-*` 以前在这份名单里
    // （「接受、不生效」），M4 的分片器把它们变成真生效的属性，所以移出去了。
    // `columns` / `column-gap` 留下：分片器认的是**视口**栏数（`FragmentOptions::columns`，
    // 由界面或读者给），作者写的块级 `column-count` 要改排版期的折行宽度，还没接。
    // 详见 `plans/facts.md` 与 `css/computed.rs` 的忽略清单。
    "float",
    "clear",
    "columns",
    "column-gap",
    // 列表标记一期根本不画，所以 `list-style-type: none` 恰好与我们的输出一致，而
    // `disc` / `decimal` 是真落差。`list-style-image` / `list-style-position` 以前
    // **不在这份名单里**，于是被当成不认识的属性计进 `ignored` —— 那会让统计说
    // 「这些属性我们听都没听过」，而事实是听得出、只是没有标记可画。
    "list-style",
    "list-style-type",
    "list-style-image",
    "list-style-position",
    // 它会改变字符本身，而锚点与进度都建立在**源文本字节偏移**上（`Locator` 的坐标系）：
    // `capitalize` / `ß → SS` 这类一改就让偏移对不上。要接就得连 M5 的锚点映射一起定，
    // 不是加一条 `to_uppercase()` 的事。
    "text-transform",
    // 字体的 OpenType 层：小写大写（`font-variant` / `font-variant-caps`）、横向压缩
    // （`font-stretch`）、字距调整开关（`font-kerning`）、特性开关表（`font-feature-settings`）。
    // 这四条以前**不在这份名单里**，于是被算成「不认识」，`examples/css-gap` 的表就会说
    // 「这些属性我们听都没听过」—— 事实是认得，但要的是 gpui 的 `FontFeatures` 通道
    // （见开发方案 §3.2），那是接字体文件那一层的事，不是层叠期能折出来的数。
    "font-variant",
    "font-variant-caps",
    "font-stretch",
    "font-kerning",
    "font-feature-settings",
];

#[derive(Debug, Default)]
pub struct StyledDom {
    /// 按 `Dom::nodes` 下标索引；文本节点也有一条（整份继承父元素）。
    pub styles: Vec<ComputedStyle>,
    /// 认识不了的值。`KNOWN_UNIMPLEMENTED` 里的属性不计入。
    pub ignored: usize,
}

/// 一条声明在我们引擎里的落地情况。
///
/// 判定走的是 [`declaration_status`]，而它内部就是层叠用的同一个 `apply` ——
/// 所以「这本书有多少样式还没接」可以从外部统计出来，不会和实际渲染行为分叉。
/// 这份统计是 `examples/css-gap.rs` 的唯一依据，也是它 before/after 可比的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclStatus {
    /// 认得，且写进了 `ComputedStyle`。
    Applied,
    /// 属性名认得，但一期不生效（在 `KNOWN_UNIMPLEMENTED` 名单里）。
    Inert,
    /// 属性名或值不认识；`cascade` 会把这一条计入 `ignored`。
    Unparsed,
}

/// 判定一条声明的落地情况，不碰任何长期状态：在一份临时的 `ComputedStyle` 上试算。
///
/// `parent` 与 `root_px` 只影响 `em` / `%` 折算成多少像素，**不影响认不认得**，
/// 所以纯统计时可以一律传 `ComputedStyle::default()`。
/// `inherit` 这个关键字算 `Applied`：它是合法值，继承到的值由 `inherit_from` 负责给。
/// 名单里的属性不看值 —— 一期整体不生效，值写对写错都不改变「有没有生效」这件事。
pub fn declaration_status(decl: &Declaration, parent: &ComputedStyle, root_px: f32) -> DeclStatus {
    let value = decl.value.trim();
    if value.is_empty() {
        return DeclStatus::Unparsed;
    }
    if value.eq_ignore_ascii_case("inherit") {
        return DeclStatus::Applied;
    }
    if KNOWN_UNIMPLEMENTED.contains(&decl.property.as_str()) {
        return DeclStatus::Inert;
    }
    let mut scratch = parent.clone();
    if apply(&mut scratch, parent, root_px, decl) {
        DeclStatus::Applied
    } else {
        DeclStatus::Unparsed
    }
}

impl StyledDom {
    pub fn get(&self, id: NodeId) -> Option<&ComputedStyle> {
        self.styles.get(id as usize)
    }

    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &ComputedStyle)> {
        self.styles
            .iter()
            .enumerate()
            .map(|(id, style)| (id as NodeId, style))
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
        // 元素的 `style="…"`。CSS 把它排在「作者普通声明」之上、`!important` 那两档之下：
        // 整份追加到最后会让行内普通声明盖掉书里的 `color: red !important`，
        // 放在最前又等于作者的行内样式没有优先权。
        // `matching_declarations` 已经按层叠级别从低到高排好，而**普通声明全在重要声明
        // 之前**，所以按 `important` 一刀切成两半，中间插入行内那两半，就是那个位置。
        let inline = dom
            .attr(id, "style")
            .map(parse_inline_style)
            .unwrap_or_default();
        let mut ordered: Vec<&Declaration> = Vec::with_capacity(decls.len() + inline.len());
        ordered.extend(decls.iter().filter(|d| !d.important));
        ordered.extend(inline.iter().filter(|d| !d.important));
        ordered.extend(inline.iter().filter(|d| d.important));
        ordered.extend(decls.iter().filter(|d| d.important));

        ignored += apply_in_cascade_order(&mut style, &parent, root_base.font_size, &ordered);

        // `::first-letter`：首字符的盒要等这个元素自己算完才能起算（`3em` 的基准
        // 就是它），所以放在后面。一份都没命中时留 `None`，排版层因此完全不走下沉那条路。
        let cap_decls = rules.matching_first_letter(dom, id);
        if !cap_decls.is_empty() {
            let mut cap = style.clone();
            // `parent` 传的是**这个元素自己**：`::first-letter` 的父盒就是它，
            // 所以 `font-size: 3em` 的 em 按段落自己的字号折，不是按段落的父。
            ignored += apply_in_cascade_order(&mut cap, &style, root_base.font_size, &cap_decls);
            style.first_letter = Some(Box::new(cap));
        }
        styles[id as usize] = style;
    }
    StyledDom { styles, ignored }
}

/// 按「font-size → line-height → 其余」三批算完一组已排好序的声明，返回没接住的条数。
///
/// 分批而不是照书写顺序：`em` / `%` 都按**元素自己的**字号折算，
/// 若照书写顺序，`text-indent: 2em; font-size: 20px` 会按继承字号算错一档。
/// 同一批内部仍严格按传进来的层叠顺序。
fn apply_in_cascade_order(
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
fn parse_inline_style(text: &str) -> Vec<Declaration> {
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

#[cfg(test)]
mod tests {
    //! `ComputedStyle` 计算：单位换算、继承、ignored 计数、层叠端到端。
    //!
    //! 只依赖公开 API。选择器命中和 specificity / `!important` 的排序在 `css::tests` 已覆盖，
    //! 这里验的是「拿到有序声明之后算得对不对」。

    use crate::css::computed::{
        BorderEdge, BorderStyle, Color, ComputedStyle, Display, Hyphens, Length, StyledDom,
        TextAlign, cascade,
    };
    use crate::css::{Origin, RuleSet};
    use crate::dom::{Dom, NodeId};

    struct Page {
        dom: Dom,
        styled: StyledDom,
    }

    fn page(html: &str, sheets: &[(&str, Origin)]) -> Page {
        let dom = Dom::parse(html.as_bytes());
        let mut rules = RuleSet::default();
        for (css, origin) in sheets {
            rules.extend(RuleSet::parse(css, *origin));
        }
        let styled = cascade(&dom, &rules, &ComputedStyle::default());
        Page { dom, styled }
    }

    fn author(html: &str, css: &str) -> Page {
        page(html, &[(css, Origin::Author)])
    }

    fn style(page: &Page, id: NodeId) -> &ComputedStyle {
        page.styled.get(id).expect("样式表应覆盖每个节点")
    }

    fn tag(page: &Page, tag: &str) -> NodeId {
        page.dom
            .elements()
            .find(|(_, n)| {
                n.tag
                    .as_ref()
                    .map(|t| t.local.to_string())
                    .unwrap_or_default()
                    == tag
            })
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("文档里应有 <{tag}>"))
    }

    fn class(page: &Page, want: &str) -> NodeId {
        page.dom
            .elements()
            .find(|(id, _)| page.dom.classes(*id).any(|c| c == want))
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("文档里应有 .{want}"))
    }

    #[test]
    fn no_rules_yields_the_root_base_everywhere() {
        let page = page("<div><p>正文</p></div>", &[]);
        let root = page.dom.document;
        assert_eq!(style(&page, root).font_size, 16.0);
        assert_eq!(style(&page, tag(&page, "p")).font_size, 16.0);
        assert_eq!(page.styled.ignored, 0);
    }

    /// em 按**元素自己**的字号折算，与声明书写顺序无关。
    #[test]
    fn em_resolves_against_the_elements_own_font_size() {
        let first = author("<p>甲</p>", "p { font-size: 20px; text-indent: 2em }");
        let second = author("<p>乙</p>", "p { text-indent: 2em; font-size: 20px }");
        assert_eq!(style(&first, tag(&first, "p")).text_indent, 40.0);
        assert_eq!(
            style(&second, tag(&second, "p")).text_indent,
            40.0,
            "font-size 写在后面也要先算"
        );
    }

    #[test]
    fn rem_uses_the_root_font_size_not_the_parent() {
        let page = author(
            "<div><p>文</p></div>",
            "div { font-size: 30px } p { margin-top: 2rem }",
        );
        // root_base 字号是 16，rem 与父元素的 30px 无关。
        assert_eq!(style(&page, tag(&page, "p")).margin_top, 32.0);
    }

    #[test]
    fn pt_converts_at_the_css_ratio() {
        let page = author("<p>字</p>", "p { font-size: 12pt }");
        assert_eq!(style(&page, tag(&page, "p")).font_size, 16.0);
    }

    #[test]
    fn bare_number_is_treated_as_px() {
        let page = author("<p>字</p>", "p { text-indent: 3 }");
        assert_eq!(style(&page, tag(&page, "p")).text_indent, 3.0);
    }

    #[test]
    fn percent_font_size_is_relative_to_the_parent() {
        let page = author(
            "<div><p>文</p></div>",
            "div { font-size: 20px } p { font-size: 50% }",
        );
        assert_eq!(style(&page, tag(&page, "p")).font_size, 10.0);
    }

    #[test]
    fn font_size_keywords_and_relative_keywords() {
        let page = author(
            "<div><p>一</p><h2>二</h2></div>",
            "div { font-size: 20px } p { font-size: smaller } h2 { font-size: large }",
        );
        assert_eq!(style(&page, tag(&page, "p")).font_size, 20.0 / 1.2);
        assert_eq!(style(&page, tag(&page, "h2")).font_size, 18.0);
    }

    #[test]
    fn line_height_accepts_unitless_percent_and_px() {
        let page = author(
            "<p class=a>一</p><p class=b>二</p><p class=c>三</p>",
            ".a { line-height: 2 } \
         .b { font-size: 20px; line-height: 150% } \
         .c { font-size: 20px; line-height: 30px }",
        );
        let find = |c: &str| class(&page, c);
        assert_eq!(style(&page, find("a")).line_height, 2.0);
        assert_eq!(style(&page, find("b")).line_height, 1.5);
        assert_eq!(
            style(&page, find("c")).line_height,
            1.5,
            "px 行高要折算成倍数，排版层只认倍数"
        );
    }

    /// 可继承的往下传，不可继承的从默认值起算。
    #[test]
    fn only_inheritable_properties_flow_down() {
        let page = author(
            "<div><p>文</p></div>",
            "div { color: red; text-align: center; font-weight: 700; font-style: italic; \
         letter-spacing: 2px; margin-bottom: 12px; padding-top: 8px; display: inline }",
        );
        let p = style(&page, tag(&page, "p"));
        assert_eq!(p.color, Some(Color(255, 0, 0)));
        assert_eq!(p.text_align, TextAlign::Center);
        assert_eq!(p.font_weight, 700);
        assert!(p.font_italic);
        assert_eq!(p.letter_spacing, 2.0);
        assert_eq!(
            p.margin_bottom, 16.0,
            "p 的 1em 下边距来自 UA 表，不是继承父元素的 12px"
        );
        assert_eq!(p.padding_top, 0.0, "padding 不该继承");
        assert_eq!(p.display, Display::Block, "display 不该继承");
    }

    #[test]
    fn text_align_and_indent_survive_several_levels() {
        let page = author(
            "<div class=root><section><p>文</p></section></div>",
            ".root { text-align: justify; text-indent: 2em }",
        );
        let p = style(&page, tag(&page, "p"));
        assert_eq!(p.text_align, TextAlign::Justify);
        assert_eq!(p.text_indent, 32.0, "缩进继承下来但已在根元素折算成 px");
    }

    #[test]
    fn display_none_is_recorded_not_dropped() {
        let page = author("<p>隐</p>", "p { display: none }");
        assert_eq!(style(&page, tag(&page, "p")).display, Display::None);
        assert_eq!(page.styled.ignored, 0);
    }

    #[test]
    fn text_nodes_carry_their_parents_computed_style() {
        let page = author("<p>正文</p>", "p { font-size: 20px; color: #123456 }");
        let p = tag(&page, "p");
        let text = page
            .dom
            .get(p)
            .children
            .first()
            .copied()
            .expect("p 应有文本子节点");
        assert_eq!(page.styled.get(text), page.styled.get(p));
    }

    #[test]
    fn color_forms() {
        let page = author(
            "<p class=a>一</p><p class=b>二</p><p class=c>三</p><p class=d>四</p>",
            ".a{color:#abc} .b{color:#112233} .c{color:rgb(10, 20, 30)} .d{color:Silver}",
        );
        let find = |c: &str| class(&page, c);
        assert_eq!(style(&page, find("a")).color, Some(Color(0xAA, 0xBB, 0xCC)));
        assert_eq!(style(&page, find("b")).color, Some(Color(0x11, 0x22, 0x33)));
        assert_eq!(style(&page, find("c")).color, Some(Color(10, 20, 30)));
        assert_eq!(style(&page, find("d")).color, Some(Color(192, 192, 192)));
        assert_eq!(page.styled.ignored, 0);
    }

    /// 认不得的值计入 ignored；认识但一期不做的不计。
    #[test]
    fn unknown_values_count_as_ignored_but_known_lazily_supported_do_not() {
        let page = author(
            "<p>文</p>",
            "p { transform: rotate(3deg); border: 1px solid red; \
         text-decoration: underline; font: 1em serif; width: 100px }",
        );
        assert_eq!(page.styled.ignored, 1, "只有 transform 是清单外属性");
    }

    /// 百分比外边距要按包含块宽度算，层叠期不知道那个数，所以宁可判不支持。
    /// 用 div 而不是 p：div 的 UA 外边距是 0，能看清「被拒的声明什么都没留下」。
    #[test]
    fn percentage_boxes_are_rejected_rather_than_guessed() {
        let page = author("<div>文</div>", "div { margin-top: 10%; text-indent: 5% }");
        let div = style(&page, tag(&page, "div"));
        assert_eq!(div.margin_top, 0.0);
        assert_eq!(div.text_indent, 0.0);
        assert_eq!(page.styled.ignored, 2);
    }

    /// `auto` 边距折成 **0**，并且不再计进 `ignored`。
    ///
    /// 这条改动是因为仓库里那本《蛇结》写了 `margin: 5px auto`：旧规则把整条声明判为不认识，
    /// 于是连那 5px 的上下边距一起丢掉 —— 静默丢一半比折成 0 糟得多。折成 0 也不是降级：
    /// CSS 的 `auto` 前后边距是均分剩余宽度，而我们的正文块使用宽度就等于可用宽度
    /// （块级不吃 `max-width` 收窄），剩余宽度本来就是 0。等排版层支持块级 `max-width`
    /// 时这里要改成均分，那时才会出现块级居中。
    #[test]
    fn auto_margin_computes_to_zero() {
        let page = author("<p>文</p>", "p { margin-left: auto }");
        let p = style(&page, tag(&page, "p"));
        assert_eq!(p.margin_left, 0.0);
        assert_eq!(p.margin_top, 16.0, "UA 给 <p> 的 1em 不该被这条声明碰坏");
        assert_eq!(page.styled.ignored, 0, "认得的合法值不该计进 ignored");
    }

    /// 四边简写是这本书里最大的一处落差：917 个元素身上有胜出的 `margin` 简写，
    /// 而旧实现只认单值形式（`margin: 1em`），`margin: 1em 0` 这种最常见的写法整条作废。
    #[test]
    fn the_margin_shorthand_expands_one_to_four_values() {
        // em 按元素自己的字号折算，12pt 按 CSS 的 96/72 比例 —— 两种单位都出现过。
        let page = author(
            "<div class=\"a\"></div><div class=\"b\"></div>\
         <div class=\"c\"></div><div class=\"d\"></div>",
            ".a { margin: 12pt 0 } \
         .b { margin: 5px 0 1em } \
         .c { margin: 1px 2px 3px 4px } \
         .d { margin: 0.5em 0 } \
         .d { font-size: 30px }",
        );
        let a = style(&page, class(&page, "a"));
        assert_eq!(
            (a.margin_top, a.margin_right, a.margin_bottom, a.margin_left),
            (16.0, 0.0, 16.0, 0.0),
            "两值 = 「上下 左右」"
        );
        let b = style(&page, class(&page, "b"));
        assert_eq!(
            (b.margin_top, b.margin_right, b.margin_bottom, b.margin_left),
            (5.0, 0.0, 16.0, 0.0),
            "三值 = 「上 左右 下」"
        );
        let c = style(&page, class(&page, "c"));
        assert_eq!(
            (c.margin_top, c.margin_right, c.margin_bottom, c.margin_left),
            (1.0, 2.0, 3.0, 4.0),
            "四值顺时针"
        );
        let d = style(&page, class(&page, "d"));
        assert_eq!(
            d.margin_top, 15.0,
            "0.5em 按它自己的 30px 字号，不是根的 16px"
        );
        assert_eq!(page.styled.ignored, 0, "这四种写法都该被接住");
    }

    /// `padding` 简写走同一份展开，但**不**吃 `auto`：CSS 里 `padding: auto` 是非法值。
    #[test]
    fn the_padding_shorthand_expands_without_auto() {
        let page = author(
            "<div class=\"a\"></div><div class=\"b\"></div>",
            ".a { padding: 0 1em } .b { padding: 4px auto }",
        );
        let a = style(&page, class(&page, "a"));
        assert_eq!(
            (
                a.padding_top,
                a.padding_right,
                a.padding_bottom,
                a.padding_left
            ),
            (0.0, 16.0, 0.0, 16.0)
        );
        let b = style(&page, class(&page, "b"));
        assert_eq!(
            (b.padding_top, b.padding_right),
            (0.0, 0.0),
            "`padding: auto` 非法，整条作废而不是半信半疑"
        );
        assert_eq!(page.styled.ignored, 1, "只有那一条该计 ignored");
    }

    /// 值数超出 1–4、或任一个值不认识，整条作废。静默只取前两个值是最难查的错法。
    #[test]
    fn a_shorthand_with_too_many_or_bad_values_is_dropped_whole() {
        let page = author(
            "<div class=\"a\"></div><div class=\"b\"></div><div class=\"c\"></div>",
            ".a { margin: 1px 2px 3px 4px 5px } \
         .b { margin: 1px nonsense } \
         .c { margin: 5% }",
        );
        for name in ["a", "b", "c"] {
            let s = style(&page, class(&page, name));
            assert_eq!(
                (s.margin_top, s.margin_left),
                (0.0, 0.0),
                ".{name} 的写法都该整条不生效"
            );
        }
        assert_eq!(
            page.styled.ignored, 3,
            "三条都要计进 ignored，落差不然会被静默吞掉"
        );
    }

    #[test]
    fn bad_values_fall_back_to_the_inherited_or_default_number() {
        let page = author("<p>文</p>", "p { font-size: huge; line-height: tall }");
        let p = style(&page, tag(&page, "p"));
        assert_eq!(p.font_size, 16.0);
        assert_eq!(p.line_height, 1.75);
        assert_eq!(page.styled.ignored, 2);
    }

    #[test]
    fn white_space_pre_is_tracked() {
        let page = author("<pre>文</pre>", "pre { white-space: pre }");
        assert!(style(&page, tag(&page, "pre")).white_space_pre);
        assert_eq!(page.styled.ignored, 0);
    }

    /// `hyphens` 三个值 + EPUB 前缀拼法 + 可继承。认不出的值回 CSS 初值 `manual`
    /// 并计一条 ignored —— 静悄悄变成 `none` 会把整本书的折行改掉还没人知道。
    #[test]
    fn hyphens_parses_the_three_values_and_inherits() {
        let page = author(
            "<div><p class=a>一</p></div><p class=b>二</p><p class=c>三</p>",
            "div { hyphens: none } .b { -epub-hyphens: auto } .c { hyphens: tall }",
        );
        assert_eq!(
            style(&page, class(&page, "a")).hyphens,
            Hyphens::None,
            "子元素继承到 div 的 none"
        );
        assert_eq!(
            style(&page, class(&page, "b")).hyphens,
            Hyphens::Auto,
            "`-epub-hyphens` 这种拼法也认"
        );
        assert_eq!(
            style(&page, class(&page, "c")).hyphens,
            Hyphens::Manual,
            "认不出的值回到 CSS 初值"
        );
        assert_eq!(page.styled.ignored, 1, "`hyphens: tall` 要留一条痕迹");
    }

    /// 元素的 `style="…"` 是作者写的、压在作者样式表之上那一档（CSS Cascade §6.4：
    /// 作者普通 < 行内普通 < 行内 `!important` < 作者 `!important`）。
    /// 追加到最后是常见错法 —— 那会让行内的普通声明盖掉书里的 `color: blue !important`。
    #[test]
    fn inline_style_beats_author_normal_but_not_important() {
        let plain = author(r#"<p style="color: red">文</p>"#, "p { color: blue }");
        assert_eq!(
            style(&plain, tag(&plain, "p")).color,
            Some(Color(255, 0, 0)),
            "行内的普通声明该压过样式表的普通声明"
        );

        let important = author(
            r#"<p style="color: red">文</p>"#,
            "p { color: blue !important }",
        );
        assert_eq!(
            style(&important, tag(&important, "p")).color,
            Some(Color(0, 0, 255)),
            "行内的普通声明不该盖掉作者的 !important"
        );

        let both = author(
            r#"<p style="color: red !important">文</p>"#,
            "p { color: blue !important }",
        );
        assert_eq!(
            style(&both, tag(&both, "p")).color,
            Some(Color(0, 0, 255)),
            "两边都 !important 时仍是样式表那份赢"
        );
    }

    /// 读者的普通声明（偏好折算出来的那一份）在作者行内样式之下，
    /// 但读者的 `!important` 仍在所有作者声明之上。
    #[test]
    fn inline_style_sits_between_reader_normal_and_reader_important() {
        let over_reader = page(
            r#"<p style="font-size: 30px">文</p>"#,
            &[("p { font-size: 12px }", Origin::Reader)],
        );
        assert_eq!(style(&over_reader, tag(&over_reader, "p")).font_size, 30.0);

        let under_reader = page(
            r#"<p style="font-size: 30px">文</p>"#,
            &[("p { font-size: 12px !important }", Origin::Reader)],
        );
        assert_eq!(
            style(&under_reader, tag(&under_reader, "p")).font_size,
            12.0,
            "读者的 !important 是最高一档，行内也拦不住"
        );
    }

    /// 行内那份同样讲「font-size 先算」：`text-indent: 2em` 的 em 要按这条声明
    /// 自己的 20px 折，不是按继承来的 16px。
    #[test]
    fn inline_em_resolves_against_the_inline_font_size() {
        let page = author(r#"<p style="text-indent: 2em; font-size: 20px">文</p>"#, "");
        assert_eq!(style(&page, tag(&page, "p")).text_indent, 40.0);
    }

    /// 行内样式不进任何样式表，所以 `ignored` 是它唯一的痕迹 ——
    /// 没有这个计数，「这本书写了我们不支持的东西」就彻底没人说了。
    #[test]
    fn unsupported_inline_declarations_are_counted_as_ignored() {
        let page = author(
            r#"<p style="transform: rotate(3deg); color: green">文</p>"#,
            "",
        );
        assert_eq!(style(&page, tag(&page, "p")).color, Some(Color(0, 128, 0)));
        assert_eq!(page.styled.ignored, 1, "transform 那条要留痕");
    }

    /// 引号里的分号不是声明分隔符：切错了会把 `color: green` 削成半条，
    /// 而半条与整条在 ignored 上看不出区别，所以断言落在颜色上。
    #[test]
    fn semicolon_inside_quotes_is_not_a_declaration_separator() {
        let page = author(r#"<p style='content: "a;b"; color: green'>文</p>"#, "");
        assert_eq!(
            style(&page, tag(&page, "p")).color,
            Some(Color(0, 128, 0)),
            "引号里的分号不该把 color 切开"
        );
        assert_eq!(page.styled.ignored, 1, "只有 content 那一条是不认识的");
    }

    /// `p::first-letter` 的声明只进首字符那一份，元素自己一点不受影响。
    /// 两边混在一起是最典型的错法：`p::first-letter { color: red }` 会把整段涂红。
    #[test]
    fn first_letter_declarations_land_on_the_cap_not_the_element() {
        let page = author(
            "<p>文</p><h1>题</h1>",
            "p { font-size: 20px } p::first-letter { font-size: 3em; color: red } h1 { color: blue }",
        );
        let p = style(&page, tag(&page, "p"));
        assert_eq!(p.font_size, 20.0, "元素自己的字号不该被首字符那条改掉");
        assert_eq!(p.color, None, "元素自己不该拿到首字符的颜色");

        let cap = p.first_letter.as_deref().expect("应有首字符那一份");
        assert_eq!(cap.font_size, 60.0, "3em 按元素自己的 20px 折");
        assert_eq!(cap.color, Some(Color(255, 0, 0)));
        assert_eq!(cap.font_weight, 400, "没写就沿用元素的值");

        // 没写 `::first-letter` 的元素必须是 `None` —— 排版层靠这一个判走不走下沉。
        assert!(style(&page, tag(&page, "h1")).first_letter.is_none());
        assert_eq!(page.styled.ignored, 0);
    }

    /// 首字符那份**不继承**给后代：子元素自己没写 `::first-letter` 就是 `None`，
    /// 否则一个块的下沉会顺着树一路复制到每个嵌套容器。
    #[test]
    fn first_letter_does_not_inherit() {
        let page = author(
            "<div><p>文</p></div>",
            "div::first-letter { font-size: 3em } p::first-letter { font-size: 2em }",
        );
        let cap = style(&page, tag(&page, "p"))
            .first_letter
            .as_deref()
            .expect("p 自己写过");
        assert_eq!(cap.font_size, 32.0, "2em 按 p 的 16px，不是按 div 的 3em");
    }

    /// 候选链整条留下来（只留第一个的话，设备上没装第一名就等于作者的第二名也没了），
    /// 引号与空白都清掉，逗号在引号里的不算分隔符。
    #[test]
    fn font_family_keeps_the_whole_candidate_chain() {
        let page = author(
            "<p class=a>一</p><p class=b>二</p><p class=c>三</p>",
            ".a { font-family: \"LXGW WenKai\", 'Noto Serif CJK SC', serif } \
         .b { font-family: easypub, \"Times New Roman\" } \
         .c { font: 1em \"Book Body\", serif }",
        );
        assert_eq!(
            style(&page, class(&page, "a")).font_families,
            ["LXGW WenKai", "Noto Serif CJK SC", "serif"]
        );
        assert_eq!(
            style(&page, class(&page, "b")).font_families,
            ["easypub", "Times New Roman"]
        );
        assert_eq!(
            style(&page, class(&page, "c")).font_families,
            ["Book Body", "serif"],
            "`font` 简写的字族部分同样留整条链"
        );
        assert_eq!(
            style(&page, class(&page, "a")).font_family,
            "LXGW WenKai",
            "第一个候选仍是 `font_family`"
        );
    }

    /// 引号里的逗号属于家族名，不是候选分隔符。
    #[test]
    fn comma_inside_quotes_is_not_a_candidate_separator() {
        let page = author("<p>文</p>", "p { font-family: \"A, B\", serif }");
        let s = style(&page, tag(&page, "p"));
        assert_eq!(s.font_families, ["A, B", "serif"]);
        assert_eq!(s.font_family, "A, B");
    }

    /// 候选链可继承：`body { font-family: X, Y }` 之后每个段落都带着这两名。
    #[test]
    fn the_candidate_chain_inherits() {
        let page = author(
            "<div><p>文</p></div>",
            "div { font-family: \"My Font\", \"Noto Serif\" }",
        );
        let p = style(&page, tag(&page, "p"));
        assert_eq!(p.font_families, ["My Font", "Noto Serif"]);
        assert_eq!(page.styled.ignored, 0);
    }

    #[test]
    fn font_family_takes_the_first_entry_and_strips_quotes() {
        let page = author("<p>文</p>", "p { font-family: \"LXGW WenKai\", serif }");
        assert_eq!(style(&page, tag(&page, "p")).font_family, "LXGW WenKai");
    }

    /// `bolder` / `lighter` 相对**父字重**查表，不是两个常量。
    /// 这条真落差在标题里：`<h1>` 已经是 700，里面再写 `bolder` 应当到 900，
    /// 写成常量就与外层一样粗 —— 读者看到的是「强调没生效」。
    #[test]
    fn bolder_and_lighter_are_relative_to_the_parent_weight() {
        let page = author(
            "<div class=deep><p class=a>一</p></div><p class=b>二</p><p class=c>三</p>",
            "div { font-weight: 700 } \
         .a { font-weight: bolder } \
         .b { font-weight: bolder } \
         .c { font-weight: lighter }",
        );
        assert_eq!(
            style(&page, class(&page, "a")).font_weight,
            900,
            "父 700 → 900"
        );
        assert_eq!(
            style(&page, class(&page, "b")).font_weight,
            700,
            "父 400 → 700"
        );
        assert_eq!(
            style(&page, class(&page, "c")).font_weight,
            100,
            "父 400 的 lighter → 100（表就是这数）"
        );
    }

    /// 书里写非整档字重（`font-weight: 450`）是常态，查表前先归到最近的百位档。
    #[test]
    fn relative_weight_buckets_a_non_round_parent() {
        let page = author(
            "<div class=five><p class=a>一</p></div><div class=six><p class=b>二</p></div>",
            ".five { font-weight: 450 } .six { font-weight: 550 } \
         .a { font-weight: bolder } .b { font-weight: bolder }",
        );
        assert_eq!(
            style(&page, class(&page, "a")).font_weight,
            700,
            "450 归到 500 档"
        );
        assert_eq!(
            style(&page, class(&page, "b")).font_weight,
            800,
            "550 归到 600 档"
        );
    }

    /// 名单里的字体 OpenType 属性报 `Inert`，不再算「不认识」——
    /// `examples/css-gap` 的表要是把认得的属性写成「听都没听过」，
    /// 看的人就会去补根本没有落差的地方。
    #[test]
    fn opentype_font_properties_report_inert_not_unknown() {
        use crate::css::Declaration;
        use crate::css::computed::{DeclStatus, declaration_status};

        let base = ComputedStyle::default();
        let status = |property: &str, value: &str| {
            declaration_status(
                &Declaration {
                    property: property.into(),
                    value: value.into(),
                    important: false,
                },
                &base,
                16.0,
            )
        };
        assert_eq!(status("font-variant", "small-caps"), DeclStatus::Inert);
        assert_eq!(status("font-variant-caps", "small-caps"), DeclStatus::Inert);
        assert_eq!(status("font-stretch", "condensed"), DeclStatus::Inert);
        assert_eq!(status("font-kerning", "none"), DeclStatus::Inert);
        assert_eq!(
            status("font-feature-settings", "\"liga\" 0"),
            DeclStatus::Inert
        );
        // 值写什么都不改变「不生效」这件事，所以不认识的值也不该把它们打成 Unparsed。
        assert_eq!(status("font-variant", "全角"), DeclStatus::Inert);
    }

    /// 端到端：读者的 `!important` 压过作者的 `!important`。
    #[test]
    fn reader_important_wins_through_the_whole_pipeline() {
        let page = page(
            "<p>文</p>",
            &[
                ("p { font-size: 30px !important }", Origin::Author),
                ("p { font-size: 18px !important }", Origin::Reader),
            ],
        );
        assert_eq!(style(&page, tag(&page, "p")).font_size, 18.0);
    }

    #[test]
    fn author_beats_reader_without_important() {
        let page = page(
            "<p>文</p>",
            &[
                ("p { font-size: 30px }", Origin::Author),
                ("p { font-size: 18px }", Origin::Reader),
            ],
        );
        assert_eq!(style(&page, tag(&page, "p")).font_size, 30.0);
    }

    /// 多张表合并后，后出现的那张在同 specificity 时胜出。
    #[test]
    fn later_stylesheet_wins_after_extend() {
        let page = page(
            "<p>文</p>",
            &[
                ("p { text-indent: 10px }", Origin::Author),
                ("p { text-indent: 99px }", Origin::Author),
            ],
        );
        assert_eq!(style(&page, tag(&page, "p")).text_indent, 99.0);
    }

    #[test]
    fn root_base_from_reader_prefs_is_the_inheritance_seed() {
        let dom = Dom::parse("<div><p>文</p></div>".as_bytes());
        let rules = RuleSet::parse("p { margin-top: 1em }", Origin::Author);
        let base = ComputedStyle {
            font_size: 24.0,
            line_height: 2.0,
            font_family: "Noto Serif CJK SC".into(),
            ..ComputedStyle::default()
        };
        let styled = cascade(&dom, &rules, &base);
        let p = dom
            .elements()
            .find(|(_, n)| {
                n.tag
                    .as_ref()
                    .map(|t| t.local.to_string())
                    .unwrap_or_default()
                    == "p"
            })
            .map(|(id, _)| id)
            .unwrap();
        let s = styled.get(p).unwrap();
        assert_eq!(s.font_size, 24.0, "读者字号要能穿到最深层");
        assert_eq!(s.line_height, 2.0);
        assert_eq!(s.font_family, "Noto Serif CJK SC");
        assert_eq!(s.margin_top, 24.0, "1em 按偏好字号而不是 16px");
    }

    /// 图片尺寸用的长度：px / em / rem 折成 px，% 留到排版期，auto 清空。
    #[test]
    fn width_and_max_width_parse_units_percent_and_auto() {
        let page = author(
            "<div class=\"a\"></div><div class=\"b\"></div>\
         <div class=\"c\"></div><div class=\"d\"></div>",
            ".a { width: 120px } \
         .b { width: 100% } \
         .c { width: auto; max-width: 50% } \
         .d { width: 2em; font-size: 20px; max-height: 3rem }",
        );
        assert_eq!(
            style(&page, class(&page, "a")).width,
            Some(Length::Px(120.0))
        );
        assert_eq!(
            style(&page, class(&page, "b")).width,
            Some(Length::Percent(1.0))
        );
        let c = style(&page, class(&page, "c"));
        assert_eq!(c.width, None, "auto 清空显式宽度");
        assert_eq!(c.max_width, Some(Length::Percent(0.5)));
        let d = style(&page, class(&page, "d"));
        assert_eq!(d.width, Some(Length::Px(40.0)), "2em 按自己的 20px 字号");
        assert_eq!(d.max_height, Some(Length::Px(48.0)), "3rem 按根 16px 字号");
        assert_eq!(page.styled.ignored, 0);

        assert_eq!(Length::Px(30.0).resolve(200.0), 30.0);
        assert_eq!(Length::Percent(0.5).resolve(200.0), 100.0);
    }

    /// `width` 不再是「认识但不做」：能算的声明不该被计进 ignored。
    #[test]
    fn implemented_width_is_not_counted_as_ignored() {
        let page = author("<div></div>", "div { width: 100px; max-width: 100% }");
        assert_eq!(page.styled.ignored, 0);
        assert_eq!(
            style(&page, tag(&page, "div")).width,
            Some(Length::Px(100.0))
        );
    }

    /// `vertical-align` 折成基线偏移。这本《蛇结》的注号用的就是 `super`（51 个元素），
    /// 而旧实现把它整个丢掉 —— 上标写成正文位置，读者看不出那是个脚注引用。
    #[test]
    fn vertical_align_becomes_a_baseline_shift() {
        let page = author(
            "<div class=\"a\"></div><div class=\"b\"></div>\
         <div class=\"c\"></div><div class=\"d\"></div>",
            ".a { vertical-align: super } \
         .b { vertical-align: sub } \
         .c { vertical-align: 4px } \
         .d { vertical-align: baseline }",
        );
        let a = style(&page, class(&page, "a"));
        assert!(
            (a.baseline_shift - 16.0 / 3.0).abs() < 0.01,
            "super 抬三分之一 em（自己的 16px 字号），实际 {}",
            a.baseline_shift
        );
        assert_eq!(
            style(&page, class(&page, "b")).baseline_shift,
            -4.0,
            "sub 压 0.25em"
        );
        assert_eq!(style(&page, class(&page, "c")).baseline_shift, 4.0);
        assert_eq!(
            style(&page, class(&page, "d")).baseline_shift,
            0.0,
            "`baseline` 就是初值，不该计进 ignored"
        );
        assert_eq!(page.styled.ignored, 0);
    }

    /// 百分比相对**自己的 line-height**（CSS 的规定），长度可以是 em；
    /// `top` / `middle` 这些要行盒对齐机制的不接，也不假装接住。
    #[test]
    fn vertical_align_percent_is_against_own_line_height_and_unkowns_are_counted() {
        let page = author(
            "<div class=\"a\"></div><div class=\"b\"></div>",
            ".a { vertical-align: 50%; font-size: 20px; line-height: 2 } \
         .b { vertical-align: middle }",
        );
        assert_eq!(
            style(&page, class(&page, "a")).baseline_shift,
            20.0,
            "50% × (20px × 2)"
        );
        assert_eq!(
            style(&page, class(&page, "b")).baseline_shift,
            0.0,
            "`middle` 要的是行盒对齐，一期不折成偏移"
        );
        assert_eq!(
            page.styled.ignored, 1,
            "不认的取值必须计 ignored，否则落差被静默吞掉"
        );
    }

    /// `text-decoration` 的简写与 `text-decoration-line` 长写法：同一件事的两个入口。
    #[test]
    fn text_decorations_are_recorded_per_line_kind() {
        let page = author(
            "<div class=\"a\"></div><div class=\"b\"></div>\
         <div class=\"c\"></div><div class=\"d\"></div>",
            ".a { text-decoration: underline } \
         .b { text-decoration: underline line-through } \
         .c { text-decoration-line: line-through } \
         .d { text-decoration: none }",
        );
        let a = style(&page, class(&page, "a"));
        assert!(a.underline && !a.line_through);
        let b = style(&page, class(&page, "b"));
        assert!(b.underline && b.line_through, "多值简写要两条都算");
        let c = style(&page, class(&page, "c"));
        assert!(!c.underline && c.line_through);
        let d = style(&page, class(&page, "d"));
        assert!(!d.underline && !d.line_through);
        assert_eq!(page.styled.ignored, 0);
    }

    /// 线型与颜色 token 认得但不改画法；`overline` 画不出来就整条不接。
    #[test]
    fn decoration_line_style_and_color_tokens_are_recognised_but_overline_is_not() {
        let page = author(
            "<div class=\"a\"></div><div class=\"b\"></div>",
            ".a { text-decoration: underline wavy #f00 } .b { text-decoration: overline }",
        );
        let a = style(&page, class(&page, "a"));
        assert!(a.underline, "线型与颜色不改变「有没有下划线」这件事");
        assert!(
            !style(&page, class(&page, "b")).underline,
            "overline 画不出来，不该显示成接住了"
        );
        assert_eq!(page.styled.ignored, 1, "只有 overline 那条计 ignored");
    }

    /// 装饰线在 CSS 里不是继承属性，但会向后代**传播**；词距则老老实实是可继承的。
    #[test]
    fn decoration_propagates_and_word_spacing_inherits() {
        let page = author(
            "<div class=outer><p><span class=inner>文</span></p></div>",
            ".outer { text-decoration: underline; word-spacing: 6px }",
        );
        let inner = style(&page, class(&page, "inner"));
        assert!(inner.underline, "父元素的下划线要盖到子元素的文字上");
        assert_eq!(inner.word_spacing, 6.0, "`word-spacing` 在 CSS 里可继承");
        // `vertical-align` **不**继承：上标只抬自己那一小段，不能让整段正文上浮。
        let page = author(
            "<div class=outer><p>文</p></div>",
            ".outer { vertical-align: super }",
        );
        let p = style(&page, tag(&page, "p"));
        assert_eq!(p.baseline_shift, 0.0, "`vertical-align` 不该跟着继承下来");
    }

    #[test]
    fn word_spacing_normal_is_zero_and_units_resolve() {
        let page = author(
            "<div class=\"a\"></div><div class=\"b\"></div><div class=\"c\"></div>",
            ".a { word-spacing: normal } \
         .b { word-spacing: 12pt } \
         .c { word-spacing: 0.5em; font-size: 20px }",
        );
        assert_eq!(style(&page, class(&page, "a")).word_spacing, 0.0);
        assert_eq!(
            style(&page, class(&page, "b")).word_spacing,
            16.0,
            "12pt 按 96/72"
        );
        assert_eq!(
            style(&page, class(&page, "c")).word_spacing,
            10.0,
            "0.5em 按自己的 20px"
        );
        assert_eq!(page.styled.ignored, 0);
    }

    /// `font` 简写：老 EPUB 满地都是。**有意不重置没写到的子属性** —— 读者偏好走的是
    /// `root_base` 的继承链，一句 `font: 1em serif` 把人家的行距改回 normal 是产品级的错。
    #[test]
    fn the_font_shorthand_sets_what_it_names_and_resets_nothing() {
        let page = author(
            "<div class=\"a\"></div><div class=\"b\"></div>",
            ".a { font: italic bold 12pt/1.5 \"Noto Serif CJK SC\", serif } \
         .b { font-size: 22px; line-height: 2; font: 1em serif }",
        );
        let a = style(&page, class(&page, "a"));
        assert!(a.font_italic);
        assert_eq!(a.font_weight, 700, "bold");
        assert_eq!(a.font_size, 16.0, "12pt");
        assert_eq!(a.line_height, 1.5, "斜杠后是行距");
        assert_eq!(a.font_family, "Noto Serif CJK SC", "带空格的家族名要整个取");
        let b = style(&page, class(&page, "b"));
        assert_eq!(b.font_size, 16.0, "1em 按它自己的 22px？不，em 按父字号");
        assert_eq!(
            b.line_height, 2.0,
            "简写没写行距就不动它：读者设的行距不该被一句 `font` 抹掉"
        );
        assert_eq!(b.font_family, "serif");
        assert_eq!(page.styled.ignored, 0);
    }

    /// 认不出的 token（`small-caps`、系统字体关键字）整条作废，不做半套。
    #[test]
    fn a_font_shorthand_with_unsupported_tokens_is_dropped_whole() {
        let page = author(
            "<div class=\"a\"></div><div class=\"b\"></div>",
            ".a { font: small-caps 12pt serif } .b { font: caption }",
        );
        for name in ["a", "b"] {
            let s = style(&page, class(&page, name));
            assert_eq!(s.font_family, "", ".{name} 整条都不该生效");
            assert_eq!(s.font_size, 16.0);
        }
        assert_eq!(page.styled.ignored, 2);
    }

    /// `declaration_status` 是 `examples/css-gap.rs` 的唯一判定口，所以它必须和层叠一致：
    /// 三种状态各钉一条，错一个统计就会把「没接」说成「接住」。
    #[test]
    fn declaration_status_agrees_with_the_cascade() {
        use crate::css::Declaration;
        use crate::css::computed::{DeclStatus, declaration_status};

        let base = ComputedStyle::default();
        let decl = |property: &str, value: &str| Declaration {
            property: property.into(),
            value: value.into(),
            important: false,
        };
        assert_eq!(
            declaration_status(&decl("font-size", "20px"), &base, 16.0),
            DeclStatus::Applied
        );
        assert_eq!(
            declaration_status(&decl("text-transform", "uppercase"), &base, 16.0),
            DeclStatus::Inert,
            "名单里的属性该报 Inert，而不是不认识"
        );
        assert_eq!(
            declaration_status(&decl("font-size", "huge"), &base, 16.0),
            DeclStatus::Unparsed
        );
        assert_eq!(
            declaration_status(&decl("margin", "1em auto"), &base, 16.0),
            DeclStatus::Applied,
            "`auto` 折成 0 之后这条就是接住的，不该再出现在落差表里"
        );
        assert_eq!(
            declaration_status(&decl("color", "inherit"), &base, 16.0),
            DeclStatus::Applied,
            "`inherit` 是合法值，继承由 inherit_from 负责"
        );
        assert_eq!(
            declaration_status(&decl("color", ""), &base, 16.0),
            DeclStatus::Unparsed
        );
    }

    /// UA 表那一份最小副本负责「作者没写、浏览器会给」的默认值。上标与装饰线都在这里，
    /// 因为出版书的 XHTML 通常只写 `<sup>`、`<u>`、`<del>` 而不带 class。
    #[test]
    fn the_ua_sheet_carries_superscripts_and_decorations() {
        let page = author(
            "<p><sup>a</sup><sub>b</sub><u>c</u><del>d</del><s>e</s></p>",
            "",
        );
        let sup = style(&page, tag(&page, "sup"));
        assert_eq!(sup.font_size, 16.0 / 1.5);
        assert!(
            (sup.baseline_shift - 16.0 / 1.5 / 3.0).abs() < 0.01,
            "位移按缩过之后的自己字号折，实际 {}",
            sup.baseline_shift
        );
        let sub = style(&page, tag(&page, "sub"));
        assert!(
            (sub.baseline_shift + 16.0 / 1.5 * 0.25).abs() < 0.01,
            "下标是负偏移（往下），实际 {}",
            sub.baseline_shift
        );
        assert!(style(&page, tag(&page, "u")).underline, "<u> 天生有下划线");
        assert!(style(&page, tag(&page, "del")).line_through);
        assert!(style(&page, tag(&page, "s")).line_through);
        assert!(!sup.underline, "上标不该顺手带上下划线");
        assert_eq!(
            page.styled.ignored, 0,
            "UA 值不来自声明，本就不该计 ignored"
        );
    }

    /// 同一个式子：作者写 `vertical-align: super` 与 UA 给 `<sup>` 的偏移，
    /// 在**同样字号**下必须相等 —— 否则带 class 的注号和裸 `<sup>` 会排成两种高度。
    #[test]
    fn ua_superscript_and_the_author_declaration_agree() {
        let page = author(
            "<p><sup>a</sup><span class=v>b</span></p>",
            ".v { vertical-align: super; font-size: 10.666666px }",
        );
        let sup = style(&page, tag(&page, "sup"));
        let v = style(&page, class(&page, "v"));
        assert!(
            (sup.font_size - v.font_size).abs() < 0.01,
            "先确认两处字号对齐：{} vs {}",
            sup.font_size,
            v.font_size
        );
        assert!(
            (sup.baseline_shift - v.baseline_shift).abs() < 0.01,
            "同一字号下 UA 的 `<sup>` 与作者的 `vertical-align: super` 该给同一个偏移，\
         实际 {} vs {}",
            sup.baseline_shift,
            v.baseline_shift
        );
    }

    /// 边框一族：四边简写要把三要素一次写全，颜色认 hex 与 `rgb()`。
    #[test]
    fn border_shorthand_fills_all_four_edges() {
        let page = author(
            "<p>正文</p>",
            "p { border: 2px solid #586357; background-color: rgb(193, 204, 192) }",
        );
        let style = style(&page, tag(&page, "p"));
        assert_eq!(style.border_drawn(), [2.0, 2.0, 2.0, 2.0]);
        assert_eq!(style.border[0].style, BorderStyle::Solid);
        assert_eq!(style.border[0].color, Some(Color(88, 99, 87)));
        assert_eq!(style.background_color, Some(Color(193, 204, 192)));
    }

    /// `border: 20px` 没写样式 —— CSS 里这一条等于「这条边不存在」，宽度按 0 参与盒模型。
    /// 把它当成「20px 的实线」来画，或者反过来保留上一条声明的样式，都是改作者的意思。
    #[test]
    fn a_border_width_without_a_style_draws_nothing() {
        let page = author("<p>正文</p>", "p { border: 20px }");
        assert_eq!(
            style(&page, tag(&page, "p")).border_drawn(),
            [0.0; 4],
            "样式是 none 时宽度不占位"
        );
    }

    /// 出版书章标题那一条：四值宽度 + 四值样式 + 一个颜色，左边一道 20px 双线。
    #[test]
    fn four_value_border_shorthands_land_on_the_matching_edges() {
        let page = author(
            "<h2 class=t>章</h2>",
            ".t { border-width: 0px 5px 0px 20px; border-style: none double none solid; border-color: #586357 }",
        );
        let style = style(&page, class(&page, "t"));
        assert_eq!(style.border_drawn(), [0.0, 5.0, 0.0, 20.0]);
        assert_eq!(
            style.border.map(|edge| edge.style),
            [
                BorderStyle::None,
                BorderStyle::Double,
                BorderStyle::None,
                BorderStyle::Solid
            ]
        );
    }

    /// 单边长写只动那一条边；一条边给两个宽度是错的，整条作废而不是取第一个。
    #[test]
    fn single_side_longhand_touches_one_edge_only() {
        let page = author(
            "<p>正文</p>",
            "p { border: 1px solid black; border-left-width: 6px }",
        );
        assert_eq!(
            style(&page, tag(&page, "p")).border_drawn(),
            [1.0, 1.0, 1.0, 6.0]
        );
        let wrong = author("<p>正文</p>", "p { border-top-width: 1px 2px }");
        assert_eq!(wrong.styled.ignored, 1, "一条边只能给一个宽度");
    }

    /// 认不得的边框样式与属性按「没接住」计数，而不是悄悄画成实线。
    #[test]
    fn unsupported_border_styles_count_as_the_gap() {
        let dashed = author("<p>正文</p>", "p { border: 1px dashed black }");
        assert_eq!(
            dashed.styled.ignored, 1,
            "dashed 还没做，画成 solid 是另一回事"
        );
        let radius = author("<p>正文</p>", "p { border-radius: 4px }");
        assert_eq!(radius.styled.ignored, 1, "border-radius 不在认得的名单里");
    }

    /// 背景色与边框都**不**继承（CSS 规定）；`transparent` 是「有这条声明、但不画」。
    #[test]
    fn background_and_border_are_not_inherited() {
        let page = author(
            "<div><p>正文</p></div>",
            "div { background-color: #C1CCC0; border: 4px solid black } p { background-color: transparent }",
        );
        let div = style(&page, tag(&page, "div"));
        assert_eq!(div.background_color, Some(Color(193, 204, 192)));
        assert_eq!(div.border_drawn(), [4.0; 4]);
        let p = style(&page, tag(&page, "p"));
        assert_eq!(p.background_color, None, "子元素不继承父元素的背景色");
        assert_eq!(p.border_drawn(), [0.0; 4], "也不继承父元素的边框");
    }

    /// 手写 `Hash` 要覆盖**每个字段** —— 块级排版缓存靠它判「这个块的样式变了没」，
    /// 漏一个字段就会把过期结果当成命中。这里抽几个代表字段（含浮点、枚举、子结构）
    /// 钉住「改了就变」，并钉住「不改就一样」。
    #[test]
    fn style_hash_covers_fields() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        fn h(s: &ComputedStyle) -> u64 {
            let mut hasher = DefaultHasher::new();
            s.hash(&mut hasher);
            hasher.finish()
        }
        let base = ComputedStyle::default();
        assert_eq!(h(&base), h(&base.clone()), "同一份样式两次哈希要一样");

        let mut bigger = base.clone();
        bigger.font_size = 20.0;
        assert_ne!(h(&base), h(&bigger), "字号变了哈希该变");

        let mut spaced = base.clone();
        spaced.word_spacing = 3.0;
        assert_ne!(h(&base), h(&spaced), "词距变了哈希该变");

        let mut centered = base.clone();
        centered.text_align = TextAlign::Center;
        assert_ne!(h(&base), h(&centered), "对齐变了哈希该变");

        let mut family = base.clone();
        family.font_families = vec!["Noto Serif".to_string()];
        assert_ne!(h(&base), h(&family), "候选字体链变了哈希该变");

        let mut bordered = base.clone();
        bordered.border[0] = BorderEdge {
            width: 2.0,
            style: BorderStyle::Solid,
            color: None,
        };
        assert_ne!(h(&base), h(&bordered), "边框变了哈希该变");
    }
}
