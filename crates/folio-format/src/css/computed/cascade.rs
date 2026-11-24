//! 层叠：把规则表按 DOM 树折出每节点的 `ComputedStyle`。
//!
//! 从 `css::computed` 切出来只因为 computed.rs 贴着行数上限；
//! `pub use` 保持 `computed::cascade` 的对外路径不变。

use super::apply::{apply_in_cascade_order, parse_inline_style};
use super::{
    ComputedStyle, Direction, GeneratedContent, Length, RuleSet, StyledDom, TextAlign,
    first_strong_direction, resolve_generated,
};
use crate::css::Declaration;
use crate::dom::{Dom, NodeId, NodeKind};

/// 层叠整棵树。`root_base` 是读者偏好折算出来的根样式（字号、行距、字体等）。
///
/// `base_dir` 是**这一份文档**在容器里的目录（EPUB 就是 `OEBPS/Text` 那一段），
/// 只被一件事用到：把元素 `style="…"` 里的 `url(...)` 折成容器内路径 ——
/// 样式表那一侧早就由 `content::format::absolutize_css_urls` 折过了，两边必须同一个形状，
/// 判据写在 `apply::parse_inline_style` 的注释上。用不到那一件事的调用方传空串即可。
pub fn cascade(dom: &Dom, rules: &RuleSet, root_base: &ComputedStyle, base_dir: &str) -> StyledDom {
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
        // 「在一条链接里」这一格来自 DOM 而不是样式表（判据与理由见 [`ComputedStyle::link`]）：
        // 空 `href`（`<a name="x">` 那种老式锚点）不算链接 —— 它点不动，染蓝等于骗读者。
        if dom.is_element(id, "a")
            && dom
                .attr(id, "href")
                .is_some_and(|href| !href.trim().is_empty())
        {
            style.link = true;
        }
        // HTML 的 `dir="rtl|ltr"` 是一条**呈现提示**（presentational hint）：优先级在作者
        // 样式表之下、UA 默认之上，所以落在 `apply_in_cascade_order` 之前 —— 作者真写了
        // `direction` 就把这一格盖掉。RTL 的书在 EPUB 里几乎都靠根元素这一个属性标注，
        // 只认样式表等于整本不认：出版工具很少往 RTL 书里另外写 `direction` 声明。
        //
        // `auto` 有意不当 RTL 用（也不当 LTR 用 —— 它就落回继承值）。那档要按「第一个强
        // 方向字符」判，而我们没有那份字符表：`unicode-bidi` 只在依赖图里（gpui 的文本栈
        // 带进来的），没转成直接依赖。猜错的方向比不猜更难查，所以宁可少接一档。
        if let Some(dir) = dom.attr(id, "dir") {
            match dir.trim().to_ascii_lowercase().as_str() {
                "ltr" => style.direction = Direction::Ltr,
                "rtl" => style.direction = Direction::Rtl,
                "auto" => {
                    // `auto` 要**看内容**才能定：第一个强方向字符是哪一边。判得出就订下来，
                    // 一个强方向字符都没有就留继承值 —— 「不知道」不等于「左」。
                    // 这一档常见到不能不做：聊天与社媒导出的 HTML 给每一段写 `<p dir="auto">`。
                    if let Some(found) = first_strong_direction(&dom.text_of_subtree(id)) {
                        style.direction = found;
                    }
                }
                _ => {}
            }
        }
        // `<img width="40%" height="300">` 与 `dir` 同属 HTML 的呈现提示：落在作者样式
        // 之前，作者（或行内 `style`）真写了 `width` / `height` 就把它盖掉。老书给插图
        // 定尺寸几乎全靠这两个属性，只认样式表等于整条丢掉（《哈利·波特》作者简介页
        // 的 `<img width="40%">` 就是这么被无视、按固有宽顶满版心的）。
        // 只认 `<img>`：SVG 的 `width` / `height` 是几何属性，走 `svg_aspect` 那条路。
        if dom.is_element(id, "img") {
            for (attr, field) in [("width", &mut style.width), ("height", &mut style.height)] {
                if let Some(length) = dom.attr(id, attr).and_then(parse_dimension_attribute) {
                    *field = Some(length);
                }
            }
        }
        let decls = rules.matching_declarations(dom, id);
        // 元素的 `style="…"`。CSS 把它排在「作者普通声明」之上、`!important` 那两档之下：
        // 整份追加到最后会让行内普通声明盖掉书里的 `color: red !important`，
        // 放在最前又等于作者的行内样式没有优先权。
        // `matching_declarations` 已经按层叠级别从低到高排好，而**普通声明全在重要声明
        // 之前**，所以按 `important` 一刀切成两半，中间插入行内那两半，就是那个位置。
        let inline = dom
            .attr(id, "style")
            .map(|text| parse_inline_style(text, base_dir))
            .unwrap_or_default();
        let mut ordered: Vec<&Declaration> = Vec::with_capacity(decls.len() + inline.len());
        ordered.extend(decls.iter().filter(|d| !d.important));
        ordered.extend(inline.iter().filter(|d| !d.important));
        ordered.extend(inline.iter().filter(|d| d.important));
        ordered.extend(decls.iter().filter(|d| d.important));

        ignored += apply_in_cascade_order(&mut style, &parent, root_base.font_size, &ordered);

        // 居中不缩：`text-indent` 只缩首行，居中时它会把整行推离中线（半个缩进）。
        // 无论是读者注入的（《未来简史》扉页的出版社，2em 让整行右偏 1em），还是
        // 作者自己写在内联样式里的（同一页的副标题 `text-indent:2em`），结果都是
        // 「看起来没居中」。居中是展示文字的决定，首行缩进是正文段落的，两者同时
        // 出现几乎总是转换工具留下的，按居中优先处理。标题那一档在 UA seed 里也归零。
        if style.text_align == TextAlign::Center {
            style.text_indent = Length::ZERO;
        }

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
        // `::before` / `::after`：生成盒的样式挂在元素自己上（父盒就是它），
        // 继承的 quotes 已在 `style` 里折好。open/close-quote 在这里折成字面字符，
        // 这样排版层只需读 `content` 的一种形状。
        // 作者规则盖到伪元素上：没写 `content` 时保留 UA（如 `q`）给的占位，
        // `content: none` 则清空槽位。
        let before_decls = rules.matching_before(dom, id);
        if !before_decls.is_empty() {
            let mut generated = style.clone();
            generated.before = None;
            generated.after = None;
            generated.content = style
                .before
                .as_ref()
                .map_or(GeneratedContent::None, |s| s.content.clone());
            ignored +=
                apply_in_cascade_order(&mut generated, &style, root_base.font_size, &before_decls);
            generated.content = resolve_generated(generated.content.clone(), generated.quotes);
            style.before =
                (!matches!(generated.content, GeneratedContent::None)).then(|| Box::new(generated));
        }
        let after_decls = rules.matching_after(dom, id);
        if !after_decls.is_empty() {
            let mut generated = style.clone();
            generated.before = None;
            generated.after = None;
            generated.content = style
                .after
                .as_ref()
                .map_or(GeneratedContent::None, |s| s.content.clone());
            ignored +=
                apply_in_cascade_order(&mut generated, &style, root_base.font_size, &after_decls);
            generated.content = resolve_generated(generated.content.clone(), generated.quotes);
            style.after =
                (!matches!(generated.content, GeneratedContent::None)).then(|| Box::new(generated));
        }
        // UA 表给 `q` 的占位（content 仍是 Open/CloseQuote）按最终 quotes 折成字面。
        let quotes = style.quotes;
        for generated in [&mut style.before, &mut style.after].into_iter().flatten() {
            generated.content = resolve_generated(generated.content.clone(), quotes);
        }
        styles[id as usize] = style;
    }
    StyledDom { styles, ignored }
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

/// HTML 尺寸属性（`<img width="40%" height="300">`）→ [`Length`]。
///
/// 不走 CSS 的长度表：这两个属性的值是「非负整数或百分比」，单位后缀按 HTML 的尺寸
/// 解析规则被忽略 —— 浏览器实测 `width="400px"` 是 400px、`width="2em"` 是 2px。
/// 百分比照收（HTML 3.2 起就这么写，浏览器一直认），进 [`Length::Percent`]，
/// 等排版期按包含块宽折。
///
/// `%` 判的是**数字前缀后面紧跟的那一个字符**，不是整串的末尾：浏览器量完数字只看
/// 下一格是不是 `%`，其后还剩什么一概不管。多看导出的《藩屏》封面页就写着
/// `width="100%;"`（百分号后拖一条分号），按「整串以 % 结尾」判会落进像素那一支，
/// 把 100% 读成 100px —— 1230×1756 的封面在手机上因此只画出 100×143 一小块。
/// 现在这一条与浏览器同解：100% 折成栏宽，封面铺满。
fn parse_dimension_attribute(value: &str) -> Option<Length> {
    let v = value.trim();
    let end = v
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit() && *ch != '.')
        .map_or(v.len(), |(ix, _)| ix);
    let n: f32 = v[..end].parse().ok()?;
    if v[end..].starts_with('%') {
        Some(Length::Percent(n / 100.0))
    } else {
        Some(Length::Px(n))
    }
}
