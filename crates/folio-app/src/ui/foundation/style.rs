//! 引擎与主题之间的颜色换算。
//!
//! 判据是**这段逻辑两端会不会各写一遍然后写歪**：`folio-core` 的 `Color` 是 0–255 的
//! u8 三元组，gpui 要 0–1 的浮点 `Rgba`，这个换算一旦有两份，某一端的作者样式颜色
//! 就会整体偏亮或偏暗，而这种差别只有在并排看两本书时才发现。
//!
//! 颜色换算本身**不取主题**：调用方把主题里那一格作为 fallback 递进来，这里只做
//! 「引擎的 0–255 → gpui 的 0–1」这一步，以及「作者色 / 链接色 / 正文色」三档优先级。

use folio_core::content::format::css::computed::Color;
use gpui_kit::Hsla;

/// 行样式的颜色 → gpui 颜色。样式没写 `color` 时用调用方给的正文色 ——
/// 也就是**当前主题**的那个，而不是一个写死的墨色。
pub fn run_color(color: Option<Color>, fallback: Hsla) -> Hsla {
    color.map_or(fallback, |c| {
        gpui_kit::Rgba {
            r: c.0 as f32 / 255.0,
            g: c.1 as f32 / 255.0,
            b: c.2 as f32 / 255.0,
            a: 1.0,
        }
        .into()
    })
}

/// 同上，再多问一格「这一段在不在链接里」。
///
/// 三档优先级：作者写了 `color` 就照作者的（`Some(_)` 那一支一字不变）；作者没写而这一段在
/// 一条 `<a href>` 里，就落主题的链接色；两者都没有才是正文色。链接色由调用方从**当前主题**
/// 取（`cx.theme().primary`），引擎与这一层都不嵌十六进制数。
pub fn run_color_link(color: Option<Color>, foreground: Hsla, link: Hsla, in_link: bool) -> Hsla {
    match color {
        Some(color) => run_color(Some(color), foreground),
        None if in_link => link,
        None => foreground,
    }
}

#[cfg(test)]
mod tests {
    // 这个 crate 里 `use super::*` 会把 lib test 目标弄挂（`plans/testing.md`），逐条引。
    use super::{run_color, run_color_link};
    use folio_core::content::format::css::computed::Color;
    use gpui_kit::{Hsla, Rgba, black};

    /// 一个明显不同于主题前景的「链接色」替身：那一格由调用方从当前主题取，
    /// 测试里自己造一个，只为验三档优先级的走法。
    fn link() -> Hsla {
        Rgba {
            r: 0.2,
            g: 0.4,
            b: 0.9,
            a: 1.0,
        }
        .into()
    }

    /// 三档：作者的 `color` > 主题链接色（仅当这一段在链接里）> 主题正文色。
    ///
    /// 这一格是「目录那一栏看着像能点的」的全部来源：出版工具的目录页不给链接写颜色
    /// （本机那本基督山伯爵的 `stylesheet.css` 全文一个 `color` 都没有），而引擎不许嵌色值 ——
    /// 色由主题给，判据由 `ComputedStyle::link` 带。
    #[test]
    fn the_author_color_wins_then_the_link_slot_then_the_foreground() {
        let ink = black();
        let author = Color(12, 34, 56);
        assert_eq!(
            run_color_link(Some(author), ink, link(), true),
            run_color(Some(author), ink),
            "作者写明了色，主题那一格让位"
        );
        assert_eq!(
            run_color_link(None, ink, link(), true),
            link(),
            "作者没写而在链接里 —— 落链接色"
        );
        assert_eq!(
            run_color_link(None, ink, link(), false),
            ink,
            "作者没写也不在链接里 —— 落正文色"
        );
        assert_eq!(
            run_color_link(Some(author), ink, link(), false),
            run_color(Some(author), ink),
            "非链接的 run 走原来那条路，一字不变"
        );
    }
}
