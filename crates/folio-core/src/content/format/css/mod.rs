//! CSS 子集：样式表装配 + 层叠出 `ComputedStyle`。
//!
//! 三个模块各管一段：`selector` 认「这条规则命中哪些元素」，`value` 认「一个值是什么」，
//! `computed` 管「一条声明写进计算样式的哪里」并把结果沿 DOM 树展开；`ua` 是没命中时的初值表。
//!
//! 范围就是 `computed/apply.rs` 里那批 match 臂（属性名 → 计算样式的哪几个字段），
//! 加上 `computed.rs` 里那份 `KNOWN_UNIMPLEMENTED`（认得、这一版不生效）。
//! 清单外的声明**被忽略而不是报错**，
//! 因为真实出版书里总有些用不上的声明，为一两个 `transform` 让整本书排版失败
//! 是最糟的失败方式。

pub mod computed;
pub mod selector;
pub mod ua;
pub(crate) mod value;

use crate::content::dom::{Dom, NodeId};
use selector::Selector;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Reader,
    Author,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub property: String,
    pub value: String,
    pub important: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub selector: Selector,
    pub declarations: Vec<Declaration>,
    pub origin: Origin,
    /// 同一层叠来源内的出现顺序，用于 specificity 打平时后者优先。
    pub order: u32,
}

/// `@font-face` 的一条声明。**只存声明，不存字节** —— 字节由 `format` 那层按
/// `src` 里的容器路径去读，CSS 这层不该知道 ZIP 的存在。
#[derive(Debug, Clone, PartialEq)]
pub struct FontFaceRule {
    /// `font-family` 描述符，已去引号。一条 `@font-face` 写了逗号列表时**每个名字一条**。
    pub family: String,
    /// `src`，按书写顺序，第一个是首选。
    pub src: Vec<FontSrc>,
    /// `font-weight`。写了区间（`400 700`）取第一个值。缺省 400。
    pub weight: u16,
    /// `font-style`：`italic` / `oblique` 为 `Some(true)`，`normal` 为 `Some(false)`，
    /// **没写是 `None`**，不等于 `normal` —— 没写时书的意图是「这套字体的常规面」，
    /// 而注册进来的内部名自己带面信息。
    pub italic: Option<bool>,
}

/// `src` 描述符里的一项。`format()` / `tech()` 这些修饰下一个来源的函数认得但不影响
/// 来源本身，解析时跳过。
#[derive(Debug, Clone, PartialEq)]
pub enum FontSrc {
    /// 容器内的规范路径。EPUB 的 CSS 在拼接前把 `url(../Fonts/a.otf)` 改写成了绝对路径
    /// （见 `format::epub`），所以这里的串可以直接当查表键。
    Url(String),
    /// 设备上已装的字体名：只查注册名单，不注册字节。
    Local(String),
}

/// 层叠级别，从低到高。
///
/// 注意 origin 与 `!important` 是**交错**的，不是「先比来源再比重要」：
/// 普通声明里作者盖读者，加了 `!important` 反过来读者盖作者。
/// 我们没有 UA 层，所以只有这四级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CascadeLevel {
    ReaderNormal,
    AuthorNormal,
    AuthorImportant,
    ReaderImportant,
}

fn level_of(origin: Origin, important: bool) -> CascadeLevel {
    match (origin, important) {
        (Origin::Reader, false) => CascadeLevel::ReaderNormal,
        (Origin::Author, false) => CascadeLevel::AuthorNormal,
        (Origin::Author, true) => CascadeLevel::AuthorImportant,
        (Origin::Reader, true) => CascadeLevel::ReaderImportant,
    }
}

impl Rule {
    /// 同一层叠级别内的排序键：specificity 优先，打平时后出现的赢。
    fn order_key(&self) -> u64 {
        (u64::from(self.selector.specificity()) << 32) | u64::from(self.order)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    pub rules: Vec<Rule>,
    /// 这本书声明的内嵌字体，按出现顺序。它不参与层叠（没有选择器可言），
    /// 是**给平台层看的清单**：谁去注册字节、注册成哪个名字。
    pub font_faces: Vec<FontFaceRule>,
    /// 被丢弃的声明数，用于「这本书有多少样式我们不支持」的诊断与文档。
    pub ignored: usize,
}

impl RuleSet {
    pub fn parse(text: &str, origin: Origin) -> RuleSet {
        let mut set = RuleSet::default();
        set.consume(text, origin);
        set
    }

    /// 追加另一张表。EPUB 常有多份 `.css` 按 spine 顺序生效，
    /// 所以合并时必须重排 `order`，否则两张表的 specificity 打平时先后会错。
    pub fn extend(&mut self, mut other: RuleSet) {
        let base = self.rules.len() as u32;
        for (i, rule) in other.rules.iter_mut().enumerate() {
            rule.order = base + i as u32;
        }
        self.rules.append(&mut other.rules);
        // 字体清单同样靠后出现者胜：两份 CSS 声明同一个 `font-family` 时，
        // 后那份才是作者要的（与规则层的 `order` 一个道理）。
        self.font_faces.append(&mut other.font_faces);
        self.ignored += other.ignored;
    }

    fn consume(&mut self, text: &str, origin: Origin) {
        let text = strip_comments(text);
        let mut rest = text.as_str();
        loop {
            let Some(open) = find_unquoted(rest, '{') else {
                return;
            };
            let prelude = rest[..open].trim();
            // 语句型 at-rule（`@charset` / `@import`）没有块，在分号处就结束了。
            // 不先单独吞掉它，prelude 会把下一条规则的整串选择器一起吃进去，
            // 于是 `@charset "utf-8"; body { … }` 里的 body 规则凭空消失。
            if prelude.starts_with('@')
                && let Some(semi) = find_unquoted(&rest[..open], ';')
            {
                self.ignored += 1;
                rest = &rest[semi + 1..];
                continue;
            }
            let Some(close) = matching_brace(&rest[open..]) else {
                return; // 括号不闭合，剩下的整体放弃
            };
            // matching_brace 返回的是配对的 `}` 之后一位（相对 open 的偏移）。
            let body = &rest[open + 1..open + close - 1];
            rest = &rest[open + close..];

            if let Some(at) = prelude.strip_prefix('@') {
                let name = at.split_whitespace().next().unwrap_or("");
                match name.to_ascii_lowercase().as_str() {
                    // 一期不分屏幕/打印媒体查询，一律当普通规则收进来。
                    "media" | "supports" => self.consume(body, origin),
                    // @font-face 不属于任何元素，但**要留下来**：它是「这本书带了自己的字体」
                    // 的唯一线索，落到 ignored 就等于对着一份 30MB 的内嵌字体装看不见。
                    "font-face" => {
                        let (faces, dropped) = parse_font_face(body);
                        self.font_faces.extend(faces);
                        self.ignored += dropped;
                    }
                    // @page / @keyframes 的声明确实没有去处。
                    _ => self.ignored += 1,
                }
                continue;
            }
            let Some(selector) = Selector::parse(prelude) else {
                self.ignored += count_declarations(body);
                continue;
            };
            let mut declarations = Vec::new();
            for decl in split_top_level(body, ';') {
                match parse_declaration(&decl) {
                    Some(d) => declarations.push(d),
                    // 空值声明（`font-family:;`、`padding-left:;`）是作者导出时的残渣，
                    // 连声明都算不上 —— 静默丢掉，**不计入 `ignored`**，否则
                    // 「拼不出来的块」那一格会被这种残渣灌水。
                    None if is_blank_declaration(&decl) => {}
                    None => self.ignored += 1,
                }
            }
            if !declarations.is_empty() {
                self.rules.push(Rule {
                    selector,
                    declarations,
                    origin,
                    order: self.rules.len() as u32,
                });
            }
        }
    }

    /// 命中某元素的全部声明，按层叠优先级**从低到高**返回。
    ///
    /// 以「声明」而不是「规则」为单位排序：`!important` 是逐条声明的属性，
    /// 按规则聚合会让一条混合了重要/普通声明的规则整体错位。
    pub fn matching_declarations(
        &self,
        dom: &crate::content::dom::Dom,
        node: crate::content::dom::NodeId,
    ) -> Vec<&Declaration> {
        self.matched_in(dom, node, None)
    }

    /// 命中该元素、且声明属于**首字符那个盒**的声明（`p::first-letter { … }`）。
    /// 层叠排序与 [`Self::matching_declarations`] 完全同一份，所以两条路径不会分叉。
    pub fn matching_first_letter(
        &self,
        dom: &crate::content::dom::Dom,
        node: crate::content::dom::NodeId,
    ) -> Vec<&Declaration> {
        self.matched_in(dom, node, Some(selector::PseudoElement::FirstLetter))
    }

    fn matched_in(
        &self,
        dom: &crate::content::dom::Dom,
        node: crate::content::dom::NodeId,
        part: Option<selector::PseudoElement>,
    ) -> Vec<&Declaration> {
        let hits: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|r| r.selector.matches_pseudo(dom, node, part))
            .collect();
        let mut out = Vec::new();
        for level in [
            CascadeLevel::ReaderNormal,
            CascadeLevel::AuthorNormal,
            CascadeLevel::AuthorImportant,
            CascadeLevel::ReaderImportant,
        ] {
            let mut group: Vec<(&Rule, &Declaration)> = hits
                .iter()
                .flat_map(|r| r.declarations.iter().map(move |d| (*r, d)))
                .filter(|(r, d)| level_of(r.origin, d.important) == level)
                .collect();
            group.sort_by_key(|(r, _)| r.order_key());
            out.extend(group.into_iter().map(|(_, d)| d));
        }
        out
    }
}

/// 这个元素若是 `<style>`，给出它的正文；空正文与非 `<style>` 都给 `None`。
///
/// 取子树的**原始**文本：`Dom::text_of_subtree` 为锚点坐标系服务，故意剔掉 `style`
/// 这一族；这里要的恰恰是它们的字本身。
///
/// 出口按**单个元素**给，而不是只给「整份文档拼好」那一条：调用点必须保持文档序，
/// `<link>` 与 `<style>` 在一份书里会交错出现，先把所有 `<style>` 收完再接所有外链，
/// 等于把两边 specificity 打平时的那一刀反着落。
pub fn style_text_of(dom: &Dom, id: NodeId) -> Option<String> {
    if dom.tag_name(id) != Some("style") {
        return None;
    }
    let text = dom.raw_text_of_subtree(id);
    (!text.trim().is_empty()).then_some(text)
}

/// 一份文档里所有 `<style>` 的正文，按文档顺序拼好。**不含** `<link>` 那一份 ——
/// 外链的正文要按它自己的目录折 `url(...)`，那是调用方的事。
///
/// 宿主排版时给「章自己带样式」那一档建的规则表与量缺口的工具走这一条；
/// `format::html` / `format::htmlz` 在同一趟里还要收 `<link>` 与 `<img>`，逐元素走
/// [`style_text_of`]。从前这件事在 `format::html`、`format::htmlz` 与宿主各有一份
/// 近乎同款的写法，于是「量 CSS 缺口的工具只读清单里那几份样式表、排版却连章内那块
/// 一起读」这种分叉没人拦得住：报表会说「这本书全部接住」，而屏上真正生效的规则表里
/// 明明有它没读到的那一块。
pub fn style_tag_text(dom: &Dom) -> String {
    let mut css = String::new();
    for (id, _) in dom.elements() {
        if let Some(text) = style_text_of(dom, id) {
            css.push_str(&text);
            css.push('\n');
        }
    }
    css
}

/// 一章自己写的 `<style>` → 作者层规则表。`base_dir` 是**这一章**在容器里的目录。
///
/// 样式表**文件**按位置清单回容器现取（见 `content::document::Document::stylesheet`），
/// 而写在章里的这一份随正文一起到达 —— 作者把样式写在哪一份里，层叠结果都不该有差别
/// （EasyPub 那一族工具的封面页就是这么写的）。
///
/// 基准目录与元素上那句 `style="…"` 同一个数；外链那份的基准是它自己的目录，两者本来
/// 就不同。漏折的表现是 `url(...)` 在图层表里躺着一条 `../Images/x.png`，图片索引按
/// 容器路径精确查查不到 —— 屏上「有样式、没底色」，而 CSS 那侧一切正常。
pub fn chapter_style_rules(dom: &Dom, base_dir: &str) -> RuleSet {
    let css = style_tag_text(dom);
    if css.trim().is_empty() {
        return RuleSet::default();
    }
    RuleSet::parse(
        &crate::content::format::absolutize_css_urls(&css, base_dir),
        Origin::Author,
    )
}

/// `@font-face` 的块体 → face 清单。
///
/// `font-family` 写逗号列表时**每个名字出一条**（CSS 就是这个语义：同一份字体数据
/// 挂了两个别名），因为下游是按声明名查表的。缺 `font-family` 的整块作废：那样一条
/// 谁也引用不到，留着只会让日志里的「这本书有字体」变成假象。
/// 认不出的声明计 `ignored`，与 `font` 简写同一套纪律；`unicode-range` /
/// `font-stretch` / 三个 `*-override` 认得但一期用不上，不计数。
fn parse_font_face(body: &str) -> (Vec<FontFaceRule>, usize) {
    let mut family = String::new();
    let mut src: Vec<FontSrc> = Vec::new();
    let mut weight = 400u16;
    let mut italic = None;
    let mut ignored = 0usize;

    for decl in split_top_level(body, ';') {
        let Some(d) = parse_declaration(&decl) else {
            ignored += 1;
            continue;
        };
        match d.property.as_str() {
            // 原样留着：逗号列表要**先切再去引号**，先去引号会把 `"A", "B"` 削成 `A", "B`。
            "font-family" => family = d.value.clone(),
            "src" => src = parse_font_src(&d.value),
            "font-weight" => {
                // 区间写法 `400 700` 取第一个值：一期一个 face 只匹配一个字重，
                // 认区间要的是「同一声明名下的多份数据怎么挑」那套算法。
                let head = d.value.split_whitespace().next().unwrap_or("");
                match value::parse_font_weight(head) {
                    Some(w) => weight = w,
                    None => ignored += 1,
                }
            }
            "font-style" => {
                italic = match d.value.to_ascii_lowercase().as_str() {
                    "normal" => Some(false),
                    "italic" | "oblique" => Some(true),
                    _ => None,
                };
                if italic.is_none() {
                    ignored += 1;
                }
            }
            "font-stretch"
            | "font-variant"
            | "font-feature-settings"
            | "unicode-range"
            | "ascent-override"
            | "descent-override"
            | "line-gap-override" => {}
            _ => ignored += 1,
        }
    }
    // 认不出来的写法只是少一条线索，不该让整块作废 —— 除了缺 font-family，
    // 那条让这块声明无处可查。
    if family.is_empty() {
        return (Vec::new(), ignored);
    }
    let faces = split_top_level(&family, ',')
        .into_iter()
        .map(|name| unquote(&name))
        .filter(|name| !name.is_empty())
        .map(|name| FontFaceRule {
            family: name,
            src: src.clone(),
            weight,
            italic,
        })
        .collect::<Vec<_>>();
    (faces, ignored)
}

/// `src: url(a.otf) format("opentype"), local("Book Serif")` → 来源列表。
///
/// `format()` / `tech()` 是修饰**前一个**来源的，认得就跳过；`url()` 里协议开头的
/// 一律丢（远程字体一期不取，也不该让读者以为取了）。引号与空格都清掉，
/// 因为下游拿这个串直接当容器路径查表。
fn parse_font_src(value: &str) -> Vec<FontSrc> {
    let mut out = Vec::new();
    for part in split_top_level(value, ',') {
        let Some((head, rest)) = split_function(&part) else {
            continue;
        };
        let inner = rest.trim().trim_matches(|c| c == '"' || c == '\'');
        match head.to_ascii_lowercase().as_str() {
            "url" => {
                if !is_remote(inner) {
                    out.push(FontSrc::Url(inner.to_string()));
                }
            }
            "local" => out.push(FontSrc::Local(unquote(inner))),
            // format / tech 与其余函数：修饰或暂不支持，不动已收到的来源。
            _ => {}
        }
    }
    out
}

/// `url(...)` / `local(...)` 这类函数 token → `(函数名, 括号内)`。
fn split_function(text: &str) -> Option<(String, &str)> {
    let open = text.find('(')?;
    let name = text[..open].trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphabetic() || c == '-') {
        return None;
    }
    let body = &text[open + 1..];
    let close = matching_paren(body)?;
    Some((name.to_string(), &body[..close]))
}

/// 返回第一个配对 `)` 的结束偏移（不含）。括号里只可能有引号包裹的路径，
/// 所以按引号跳过就行，不必再数一层嵌套。
fn matching_paren(text: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            ')' => return Some(i),
            _ => {}
        }
    }
    None
}

/// 协议开头的 url（`http://…` / `//…` / `data:…`）。一期不取外部资源，也不假装取过。
/// 盘符（`C:/`）不算协议：单字母的 scheme 一律当路径看待。
///
/// `format::epub` 改写 `url()` 时用的是同一条判据 —— 两边各判一次迟早会在
/// `data:` 这种「有冒号但不是协议」的写法上分叉。
pub(crate) fn is_remote(url: &str) -> bool {
    if url.starts_with("//") {
        return true;
    }
    let Some(scheme_end) = url.find(':') else {
        return false;
    };
    let scheme = &url[..scheme_end];
    scheme.len() > 1
        && scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn unquote(text: &str) -> String {
    text.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}

/// 一条 run 的字族名是不是在引用某个 `@font-face` 声明的名字。
///
/// **整串同名**才算，不是子串：`font-family: "Book Serif", serif` 在层叠期已经折成
/// 第一个候选（`computed` 的 `font-family` 分支），所以这里比较的是完整家族名。
/// 用子串会误认 —— `"Book Serif"` 里就含 `serif`。
pub fn font_face_matches(run_family: &str, declared_family: &str) -> bool {
    !run_family.is_empty()
        && !declared_family.is_empty()
        && run_family.eq_ignore_ascii_case(declared_family)
}

/// 一条声明：`property: value`，末尾的 `!important` 折成标志位。
///
/// `computed` 解析元素的 `style="…"` 属性时走的是这同一个函数 —— 行内写法与样式表
/// 写法对 `!important`、大小写、空值的处理必须一字不差，否则同一句话写在两处结果不同。
pub(crate) fn parse_declaration(text: &str) -> Option<Declaration> {
    let (property, value) = text.split_once(':')?;
    let property = property.trim().to_ascii_lowercase();
    if property.is_empty() {
        return None;
    }
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    // `!important` 只可能出现在值的末尾。
    if let Some((head, tail)) = value.rsplit_once('!')
        && tail.trim().eq_ignore_ascii_case("important")
    {
        return Some(Declaration {
            property,
            value: head.trim().to_string(),
            important: true,
        });
    }
    Some(Declaration {
        property: property.to_string(),
        value: value.to_string(),
        important: false,
    })
}

fn count_declarations(body: &str) -> usize {
    split_top_level(body, ';').len()
}

/// 一条「有属性名、值为空」的声明（`padding-left:;`）。`padding-left` 也自成一条，
/// 所以判据是「冒号右侧 trim 后为空」。这类残渣该静默丢掉，不该计入「我们不支持的声明」。
fn is_blank_declaration(text: &str) -> bool {
    match text.split_once(':') {
        Some((property, value)) => !property.trim().is_empty() && value.trim().is_empty(),
        None => false,
    }
}

/// 把 `/* … */` 换成等长空格。必须在上层做：注释常夹在选择器前面，
/// 留着它 `p` 就变成 `/* … */ p`，选择器解析直接失败。
///
/// 等长是刻意的：`format::epub` 在拼接前拿同一份文本做过一遍 `url()` 改写，
/// 注释若改变了长度，那一步算出的字节区间就全错位了。
pub(crate) fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            out.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
            out.push(c);
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            loop {
                match chars.next() {
                    None => break,
                    Some('*') if chars.peek() == Some(&'/') => {
                        chars.next();
                        break;
                    }
                    // 换行保留，其余按空格填充，避免把两个 token 黏在一起。
                    Some('\n') => out.push('\n'),
                    Some(_) => out.push(' '),
                }
            }
            out.push(' ');
            continue;
        }
        out.push(c);
    }
    out
}

fn find_unquoted(text: &str, target: char) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            c if c == target => return Some(i),
            _ => {}
        }
    }
    None
}

/// 返回从 `text` 开头那个 `{` 到其配对 `}` 的结束偏移（不含）。
fn matching_brace(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level(text: &str, sep: char) -> Vec<String> {
    selector::split_top_level(text, sep)
}

#[cfg(test)]
mod tests;
