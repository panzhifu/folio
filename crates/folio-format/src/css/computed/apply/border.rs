//! 边框一族声明的解析：写进 `style.border` 的哪几条边。
//!
//! 从 `css::computed::apply` 切出来只因为 apply.rs 贴着行数上限。

use super::super::{BorderEdge, BorderStyle, ComputedStyle};
use crate::css::value::{
    BorderElement, border_target, border_width_px, box_side_px, four_of, parse_border_style,
    parse_color,
};

/// 边框一族的一条声明 → 写进 `style.border`。入口是 [`apply`] 里那条 `border` 前缀分支。
pub(super) fn apply_border(
    style: &mut ComputedStyle,
    property: &str,
    value: &str,
    own: f32,
    root_px: f32,
) -> bool {
    let Some((side, element)) = border_target(property) else {
        return false;
    };
    // 按**函数与括号外**的空白切：`border: 1px solid rgba(0, 0, 0, 0.8)` 里的空格属于
    // 颜色函数，`split_whitespace` 会把它切成 `rgba(0,` / `0,` / `0,` —— `rgba` 那种写法
    // 于是整条判不认识（诡秘那本 37 个元素）。与样式表解析用的是同一把切法。
    let parts = crate::css::value::split_value_tokens(value);
    if parts.is_empty() {
        return false;
    }
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
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
