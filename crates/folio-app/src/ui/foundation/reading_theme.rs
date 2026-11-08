//! 正文取色的**唯一入口**：阅读界面画什么颜色，都从这一处问。
//!
//! 壳与正文分离之后，正文配色有两条来源：
//!
//! - 读者选了正文主题（[`Appearance::reading_theme`] 非空）：纸/墨/链接按内置表的
//!   **日夜配对**取侧 —— 浅深轴（[`Mode`]，含水墨）翻的是同一套主题的深浅两侧；
//! - `None`（跟随软件）：回落壳主题**按侧解出来**的同四格 —— 外观说深就取壳那一对的
//!   深侧，哪怕壳此刻还停在浅档（离场转场里就是这样，见 [`fallback`]）。
//!
//! 所有正文消费点（`body.rs` / `bands.rs` / `reading_page.rs` / `reflowable.rs`）都
//! 走 [`resolve`]，不许自己再摸 `cx.theme()`：那等于把「纸色跟着壳主题」从后门接回来。
//! 水墨在这里折算 —— [`effective_mode`] 认出它是 `Mode::Ink` 就取浅侧再逐格去饱和，
//! 与壳那边 `theme.rs` 的 `desaturate_active_theme` 同一把亮度尺（`style.rs::desaturate`）。
//!
//! **取侧自持**：翻哪一侧由外观自己折（[`side_of`]），不读 `cx.theme().mode` —— 壳先换肤、
//! 正文后画曾经只是帧序上的现状，离场改造之后壳在离场起步那一帧就翻回软件外观，正文
//! 还要按自己的夜里画完最后一段动画，侧必须与壳的档位解耦。
//!
//! 这一层不做 IO（背景图的读盘与解码在消费方）。配色每帧现问现算，查一张
//! 常量表与几次浮点换算，量级与壳每帧读主题相同；**唯一例外**是 [`set_page_bg`]
//! 那一格「当前这本书自己声明的整页底色」—— 它由开书同步设一次，必须让
//! [`resolve`] 的每一个消费点（根底、版心、捕获清屏色、读数带）同时看到，
//! 否则作者写的底色会在排版结果回来后从半路盖上来，读者先看见自己选的纸色。

use folio_core::infra::config::{Appearance, Mode};
use folio_core::infra::reading_theme::{BUILT_IN, Rgba8};
use gpui_kit::App;
use gpui_kit::Hsla;
use gpui_kit::component::{ActiveTheme, Theme, ThemeColor, ThemeMode};

use crate::ui::foundation::style::desaturate;
use crate::ui::foundation::theme::resolve_mode;

/// 淡墨（页眉页脚的章名、进度）在正文主题下取 `fg` 的这一层 alpha。
///
/// anx-reader 的页眉页脚用 `textColor.withAlpha(150/255 ≈ 0.59)`；壳主题那侧不用
/// 这个数 —— 走 `cx.theme().muted_foreground` 自己的档，回落路径逐位保持老行为。
const MUTED_ALPHA: f32 = 0.55;

/// 一帧正文要的全部颜色。字段与壳主题那四格一一对应，消费点换起来不动形状。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReadingPalette {
    pub bg: Hsla,
    pub fg: Hsla,
    pub link: Hsla,
    pub muted: Hsla,
}

/// 阅读页生效的那一档：这一本书单独设过 [`Appearance::reading_mode`] 就用它，
/// 否则跟软件。原本 shell 的 `themed_appearance` 与 `body.rs` 的水墨判断各写一份，
/// 注释里自己警告「两处各写一份就会漂」—— 现在收成一份，三处都调它。
pub fn effective_mode(appearance: &Appearance) -> Mode {
    appearance.reading_mode.unwrap_or(appearance.mode)
}

/// 这一帧正文翻哪一侧：外观自己折，不问壳。
///
/// [`effective_mode`] 折出阅读页的浅深档（含书差量与全局默认那两层），[`resolve_mode`]
/// 再把 `System` 折到这台机器当下的外观上、把水墨钉到浅侧 —— 与壳换肤用的是同一条折叠，
/// 但输入是**外观**而不是壳此刻的档位：壳在转场里翻面，正文不动。
///
/// `System` 档需要的操作系统浅深从 `cx.window_appearance()` 现问。渲染帧不是外观观察者
/// 的回调，`theme::repaint` 注释里那条 Linux 借用规矩（观察者持借时再查会 panic）不碍事。
fn side_of(appearance: &Appearance, cx: &App) -> ThemeMode {
    resolve_mode(effective_mode(appearance), cx.window_appearance())
}

/// 引擎的 0–255 RGBA → gpui 颜色。与 `style.rs::run_color` 同一个换算、同一个理由：
/// 两份就会整体偏亮偏暗。
pub fn to_hsla(color: Rgba8) -> Hsla {
    gpui_kit::Rgba {
        r: color.r as f32 / 255.0,
        g: color.g as f32 / 255.0,
        b: color.b as f32 / 255.0,
        a: color.a as f32 / 255.0,
    }
    .into()
}

/// 当前这一本**自己声明**的整页底色。
///
/// 见模块头：这是本模块唯一的一格状态，由 `ReaderSession::open_book` 在开书时
/// 从 `books.css` 同步扫出来设一次（换书覆盖、没有声明就设 `None`）。
static PAGE_BG: std::sync::RwLock<Option<Hsla>> = std::sync::RwLock::new(None);

/// 开书 / 换书时推一次；`None` = 这本没在 CSS 里写整页底色，回落读者选的纸。
pub fn set_page_bg(bg: Option<Hsla>) {
    if let Ok(mut slot) = PAGE_BG.write() {
        *slot = bg;
    }
}

fn page_bg() -> Option<Hsla> {
    PAGE_BG.read().ok().and_then(|slot| *slot)
}

/// 这一帧正文用哪套颜色。外观由调用方递进来（生效值含本书差量），这里不摸配置层
/// —— 本模块住在 `foundation`，不 import 上面任何一层；壳主题那一侧仍从 `cx` 现取。
///
/// 查无此名回落「跟随软件」：手改坏的名字让纸回到壳主题，而不是变白屏。
/// 书自己声明了整页底色时，**纸这一格以书为准**：作者写了 `body { background: … }`
/// 就是不让读者那套纸盖上去（层叠本来也是如此），而首帧就得是这个色。
pub fn resolve(appearance: &Appearance, cx: &App) -> ReadingPalette {
    let mut palette = pick(appearance.reading_theme.as_deref(), appearance, cx);
    if let Some(bg) = page_bg() {
        // 水墨档把作者色也压成灰（与 `pick` 里那几格同一把尺），否则一本棕底
        // 书在水墨下会突然冒出一块彩色。
        palette.bg = if effective_mode(appearance) == Mode::Ink {
            desaturate(bg)
        } else {
            bg
        };
    }
    palette
}

fn pick(name: Option<&str>, appearance: &Appearance, cx: &App) -> ReadingPalette {
    let ink = effective_mode(appearance) == Mode::Ink;
    let Some(name) = name else {
        return fallback(appearance, cx);
    };
    let Some(theme) = folio_core::infra::reading_theme::find(name) else {
        return fallback(appearance, cx);
    };
    // 浅深轴由外观自己折（[`side_of`]）：壳先不先换肤、换到哪一档，正文都不看。
    let colors = match side_of(appearance, cx) {
        ThemeMode::Dark => theme.dark,
        _ => theme.light,
    };
    let mut palette = ReadingPalette {
        bg: to_hsla(colors.bg),
        fg: to_hsla(colors.fg),
        link: to_hsla(colors.link),
        muted: to_hsla(colors.fg).alpha(MUTED_ALPHA),
    };
    if ink {
        palette.bg = desaturate(palette.bg);
        palette.fg = desaturate(palette.fg);
        palette.link = desaturate(palette.link);
        palette.muted = desaturate(palette.muted);
    }
    palette
}

/// 跟随软件：壳主题那对主题**按外观折出的那一侧**的四格。
///
/// 侧与壳当下生效的档一致时直接用现成的四格（绝大多数帧都走这条，零开销）；不一致的
/// 只有转场里那几帧 —— 离场起步壳已翻回软件外观，正文还要按本书的夜里画完最后一段
/// 动画 —— 那就用壳存着的另一侧配置现解一份（`appearance.rs::swatch_of` 同一条路）。
/// 「跟随软件」的契约因此是**跟壳的那对主题**，不是跟壳此刻的档位：转场翻面牵不动正文。
///
/// 链接色取 `blue` 那一格、不是语义上的 `link`：框架自带的两套默认主题里 `link` 与
/// `foreground` 是同一个值（2026-10-16 在 `TestAppContext` 上量的），拿它上色等于没接。
/// 选了正文主题的那条路不吃这一手 —— 表里每侧各有一格真的链接色。
fn fallback(appearance: &Appearance, cx: &App) -> ReadingPalette {
    let theme = cx.theme();
    let side = side_of(appearance, cx);
    if side == theme.mode {
        return ReadingPalette {
            bg: theme.background,
            fg: theme.foreground,
            link: theme.blue,
            muted: theme.muted_foreground,
        };
    }
    let config = match side {
        ThemeMode::Dark => &theme.dark_theme,
        _ => &theme.light_theme,
    };
    let base = if config.mode.is_dark() {
        ThemeColor::dark()
    } else {
        ThemeColor::light()
    };
    let mut resolved = Theme::from(base.as_ref());
    resolved.apply_config(config);
    let colors = &resolved.colors;
    ReadingPalette {
        bg: colors.background,
        fg: colors.foreground,
        link: colors.blue,
        muted: colors.muted_foreground,
    }
}

/// 当前生效的是不是**深侧**。正文主题的日/夜背景图选哪张、样卡画哪一侧，都问这一格；
/// 与 [`pick`] 里取侧是同一套判断（[`side_of`]），水墨按口径落浅侧所以恒为「否」。
pub fn dark_side(appearance: &Appearance, cx: &App) -> bool {
    side_of(appearance, cx) == ThemeMode::Dark
}

/// 选择器用的样色：一套主题某一侧的三格原色（不做水墨折算 —— 样卡展示的是纸的本色）。
pub struct SwatchColors {
    pub bg: Hsla,
    pub fg: Hsla,
    pub link: Hsla,
}

/// 按名字与侧取一套样色；查无此名回 `None`（「跟随软件」那张卡不用它）。
pub fn swatch(name: &str, dark: bool) -> Option<SwatchColors> {
    let theme = folio_core::infra::reading_theme::find(name)?;
    let colors = if dark { theme.dark } else { theme.light };
    Some(SwatchColors {
        bg: to_hsla(colors.bg),
        fg: to_hsla(colors.fg),
        link: to_hsla(colors.link),
    })
}

/// 内置主题名按表序。选择器用它摆卡，与纸色由浅入深同一个顺序。
pub fn built_in_names() -> impl Iterator<Item = &'static str> {
    BUILT_IN.iter().map(|theme| theme.name)
}

#[cfg(test)]
mod tests {
    use super::{
        MUTED_ALPHA, ReadingPalette, built_in_names, effective_mode, pick, resolve, set_page_bg,
        swatch, to_hsla,
    };
    use folio_core::infra::config::{Appearance, Mode};
    use gpui_kit::TestAppContext;
    use gpui_kit::component::{Theme, ThemeMode};

    use crate::ui::foundation::theme::{register_builtin_themes, repaint};

    /// 一份指着羊皮纸的外观样件。
    fn sepia(mode: Mode) -> Appearance {
        Appearance {
            mode,
            reading_theme: Some("sepia".into()),
            ..Appearance::default()
        }
    }

    /// 跟随软件 = 壳主题那四格逐位：这是回落路径的契约，改了它就是改了老行为。
    #[gpui_kit::test]
    fn none_falls_back_to_the_shell_theme(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(register_builtin_themes);
        cx.update(|app| {
            let theme = Theme::global(app);
            let palette = pick(None, &Appearance::default(), app);
            assert_eq!(palette.bg, theme.colors.background);
            assert_eq!(palette.fg, theme.colors.foreground);
            assert_eq!(palette.link, theme.colors.blue);
            assert_eq!(palette.muted, theme.colors.muted_foreground);
        });
    }

    /// 书自己声明了整页底色时，纸这一格**首帧**就以书为准。
    ///
    /// 这就是「先变成读者设置的背景色、再变成书籍自带的背景色」那一段跳变的门：
    /// 设一次书色，`resolve` 当场就得返回它，而不是等异步排版把装饰块盖上来。
    /// 只换 `bg`，墨 / 链接 / 淡墨仍归读者那套；换到没声明的书立刻回落。
    #[gpui_kit::test]
    fn a_book_page_background_wins_from_the_first_frame(cx: &mut TestAppContext) {
        use folio_core::infra::reading_theme::Rgba8;

        cx.update(gpui_kit::init);
        cx.update(register_builtin_themes);
        let appearance = sepia(Mode::Light);
        cx.update(|app| {
            set_page_bg(None);
            let base = resolve(&appearance, app);
            let book = to_hsla(Rgba8 {
                r: 0xc4,
                g: 0xaa,
                b: 0x87,
                a: 255,
            });
            set_page_bg(Some(book));
            let got = resolve(&appearance, app);
            assert_eq!(got.bg, book, "首帧就得以书色铺纸");
            assert_ne!(got.bg, base.bg, "不该先铺读者选的那张纸");
            assert_eq!(got.fg, base.fg, "书只声明了底色，墨色不跟着换");
            // 换到没声明整页底色的书：立刻回落，旧书色不许串过来。
            set_page_bg(None);
            assert_eq!(resolve(&appearance, app).bg, base.bg);
        });
    }

    /// 跟随软件的侧：外观说深、壳停在浅时，回落的是壳那一对的**深侧**（现解一份），
    /// 不是壳此刻正涂着的浅四格 —— 离场转场里正文要靠这一条保持夜里。
    #[gpui_kit::test]
    fn the_fallback_takes_the_shell_pairs_dictated_side(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(register_builtin_themes);
        cx.update(|app| {
            // 壳涂到浅档（Default Light）。
            repaint(
                &Appearance {
                    mode: Mode::Light,
                    ..Appearance::default()
                },
                None,
                app,
            );
            let dark_appearance = Appearance {
                mode: Mode::Dark,
                ..Appearance::default()
            };
            let palette = resolve(&dark_appearance, app);
            assert_ne!(
                palette.bg,
                Theme::global(app).colors.background,
                "取到的不是壳此刻正涂着的浅纸"
            );
            // 且确实是壳那一对里的深侧：壳翻到深档后，现成的四格与它逐位相同。
            repaint(
                &Appearance {
                    mode: Mode::Dark,
                    ..Appearance::default()
                },
                None,
                app,
            );
            assert_eq!(palette.bg, Theme::global(app).colors.background);
            assert_eq!(palette.fg, Theme::global(app).colors.foreground);
            assert_eq!(palette.link, Theme::global(app).colors.blue);
        });
    }

    /// 选了正文主题：纸与墨换成表里的浅侧，壳主题是什么不再相干 —— 这就是分离。
    #[gpui_kit::test]
    fn a_reading_theme_replaces_the_paper_not_the_shell(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(register_builtin_themes);
        cx.update(|app| {
            let appearance = sepia(Mode::Light);
            let palette = resolve(&appearance, app);
            assert_eq!(palette.bg, swatch("sepia", false).unwrap().bg);
            // 壳的纸是白的（Default Light），正文纸不是：两者不再同源。
            assert_ne!(palette.bg, Theme::global(app).colors.background);
        });
    }

    /// 日夜配对：外观翻到深档，同一套主题翻深侧，而不是换一套主题。
    ///
    /// 侧由外观自己折（[`side_of`]）：壳停在浅档也照样取深侧 —— 取色不再读壳的档，
    /// 这是离场改造的契约（壳在离场起步那帧翻回软件外观，正文得按本书的夜里画完）。
    #[gpui_kit::test]
    fn night_flips_the_same_theme_to_its_dark_side(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(register_builtin_themes);
        cx.update(|app| {
            let day = resolve(&sepia(Mode::Light), app);
            let night = resolve(&sepia(Mode::Dark), app);
            assert_ne!(day.bg, night.bg, "翻面换了纸色");
            assert_eq!(
                night.bg,
                swatch("sepia", true).unwrap().bg,
                "深侧就是同一套主题的深侧"
            );
            // 壳换肤走它自己那条链：这里特意把壳涂回浅档，深档外观取侧不受它影响。
            repaint(
                &Appearance {
                    mode: Mode::Light,
                    ..Appearance::default()
                },
                None,
                app,
            );
            assert_eq!(
                Theme::global(app).mode,
                ThemeMode::Light,
                "脚手架就位：壳此刻确实停在浅档"
            );
            assert_eq!(
                resolve(&sepia(Mode::Dark), app).bg,
                swatch("sepia", true).unwrap().bg,
                "壳停在浅档，深档的外观照样取到深侧"
            );
            // 全局默认那一档同理：阅读浅深盖软件浅深（这一份带着正文主题，走内置表）。
            let pinned = Appearance {
                mode: Mode::Light,
                reading_mode: Some(Mode::Dark),
                reading_theme: Some("sepia".into()),
                ..Appearance::default()
            };
            assert_eq!(
                resolve(&pinned, app).fg,
                swatch("sepia", true).unwrap().fg,
                "阅读浅深钉了夜里，软件停在白天也翻深侧"
            );
        });
    }

    /// 水墨：取浅侧再逐格去饱和 —— 带色相的羊皮纸过尺后不再有颜色。
    /// 亮度用同一把尺（Rec. 709 luma）比：去饱和保的是它，不是 HSL 的 l。
    #[gpui_kit::test]
    fn ink_desaturates_the_light_side(cx: &mut TestAppContext) {
        fn luma(color: gpui_kit::Hsla) -> f32 {
            let rgba = gpui_kit::Rgba::from(color);
            0.2126 * rgba.r + 0.7152 * rgba.g + 0.0722 * rgba.b
        }
        cx.update(gpui_kit::init);
        cx.update(register_builtin_themes);
        cx.update(|app| {
            let appearance = sepia(Mode::Ink);
            let palette = resolve(&appearance, app);
            let day = swatch("sepia", false).unwrap();
            assert!((palette.bg.s - 0.0).abs() < 1e-4, "羊皮纸的色相该被拿掉");
            assert!(
                (luma(palette.bg) - luma(day.bg)).abs() < 1e-4,
                "亮度不动，只拿色相"
            );
        });
    }

    /// 手改坏的名字回落「跟随软件」，不是白屏也不是 panic。
    #[gpui_kit::test]
    fn an_unknown_name_falls_back_to_the_shell(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(register_builtin_themes);
        cx.update(|app| {
            let appearance = Appearance {
                reading_theme: Some("没有这套".into()),
                ..Appearance::default()
            };
            let palette = resolve(&appearance, app);
            assert_eq!(palette.bg, Theme::global(app).colors.background);
        });
    }

    /// 折叠档这一份是唯一的：阅读浅深盖软件浅深，水墨从两种来路都认得出。
    #[test]
    fn the_effective_mode_folds_reading_mode_over_mode() {
        let mut appearance = Appearance::default();
        assert_eq!(effective_mode(&appearance), Mode::System);
        appearance.mode = Mode::Dark;
        assert_eq!(effective_mode(&appearance), Mode::Dark);
        appearance.reading_mode = Some(Mode::Light);
        assert_eq!(effective_mode(&appearance), Mode::Light, "书里的选择说了算");
        appearance.reading_mode = Some(Mode::Ink);
        assert_eq!(effective_mode(&appearance), Mode::Ink);
    }

    /// 淡墨是正文墨压薄：同色相、同一份 alpha。
    #[gpui_kit::test]
    fn muted_is_the_reading_ink_thinned(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(register_builtin_themes);
        cx.update(|app| {
            let appearance = sepia(Mode::Light);
            let palette = resolve(&appearance, app);
            let ink_rgba = gpui_kit::Rgba::from(palette.fg);
            let muted_rgba = gpui_kit::Rgba::from(palette.muted);
            assert!((muted_rgba.a - MUTED_ALPHA).abs() < 1e-4);
            assert_eq!(ink_rgba.r, muted_rgba.r, "淡墨与正文同色相，只压透明度");
            assert_eq!(ink_rgba.g, muted_rgba.g);
        });
    }

    /// 选择器的样色查得到每一套内置主题，两侧各一份且纸色不同。
    #[test]
    fn swatches_cover_every_built_in_theme() {
        for name in built_in_names() {
            let day = swatch(name, false).expect("内置主题必须有样色");
            let night = swatch(name, true).expect("内置主题必须有样色");
            assert_ne!(day.bg, night.bg, "主题 {name} 的日夜样色不该一样");
        }
        assert!(swatch("没有这套", false).is_none());
        // ReadingPalette 字段完整性用一次构造钉住：加格子时这里先红。
        let _ = ReadingPalette {
            bg: gpui_kit::Hsla::default(),
            fg: gpui_kit::Hsla::default(),
            link: gpui_kit::Hsla::default(),
            muted: gpui_kit::Hsla::default(),
        };
    }
}
