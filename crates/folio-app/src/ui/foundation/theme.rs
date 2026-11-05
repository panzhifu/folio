//! 主题：用 UI 框架自己的主题系统选配色，本应用**不定义任何颜色值**。两处例外：
//! 滚动条那一条的**浓度**（`SCROLLBAR_TINT` 那三个 alpha：色相照样从当下生效那本主题的
//! `foreground` 现取，这里只答「墨压到纸上是几成」，所以浅深两套与读者自己 `themes/` 里
//! 那几本 JSON 都跟着走），以及水墨那一档的配色（`INK_THEME_JSON`，一份主题文件，
//! 装载与查找仍走 `ThemeRegistry`，见 [`INK_LIGHT`]）。
//!
//! 模型和 gpui-kit 一致，分两层：
//!
//! - **主题集**是别人的东西：框架自带的那几套（构建时从依赖源码嵌进来，见 `build.rs`）
//!   加上用户自己放进 `~/.config/folio/themes/*.json` 的，全部由 `ThemeRegistry` 按名字管；
//! - **外观**是我们自己的偏好：浅深怎么选、解析到浅和深时各用哪一套主题名，存在
//!   [`folio_core::infra::config::Appearance`] 里。
//!
//! 所以换肤就是把 `Theme` 这个 Global 的 `light_theme` / `dark_theme` 两个槽换成选中的
//! 配置，再切 `mode`；框架的每个组件都读 `cx.theme()`，不需要谁去通知它们。
//!
//! 主题名认不出时回落到框架自带的 `Default Light` / `Default Dark`：主题可能被改名、
//! 被从目录里删掉，那时下拉框选不中但不该整界面变白。

use folio_core::infra::config::{Appearance, Mode};
use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{App, SharedString, Window, WindowAppearance};

/// 框架内置的默认主题名，存在 `ThemeRegistry` 里，不由我们嵌入。
/// 没设过主题、或设过的名字已经不存在时用它们兜底。
pub const DEFAULT_LIGHT: &str = "Default Light";
pub const DEFAULT_DARK: &str = "Default Dark";

/// 水墨那一档在 registry 里的名字。`folio_core::infra::config::Mode::Ink` 认的就是这一个。
pub const INK_LIGHT: &str = "Ink Wash";

/// 本仓库自己写的那一本宣纸 + 墨的配色。
///
/// 这是 `theme.rs` 头部那句「本应用不定义任何颜色值」**唯一**的一处数据例外：水墨是一档
/// 固定的样子，不是「浅侧多一本可选的主题」，所以在依赖源码里没有它的位置。写色值的仍然
/// 是主题文件而不是 Rust —— 装载、按名字取、回落全都走 `ThemeRegistry` 那一条路。
const INK_THEME_JSON: &str = include_str!("../../../assets/ink-wash.json");

// `BUILTIN_THEME_JSON` 由 build.rs 从依赖源码里的 `themes/` 生成。
include!(concat!(env!("OUT_DIR"), "/folio_themes.rs"));

/// 把嵌入的主题集灌进 registry。要在 `gpui_kit::init` 之后调用 —— 那一步才把
/// registry 这个 Global 建出来。
pub fn register_builtin_themes(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    // 上游那几本由 build.rs 从依赖源码嵌进来，水墨那一本由本仓库写着（见 [`INK_THEME_JSON`]）。
    for json in BUILTIN_THEME_JSON.iter().chain(&[INK_THEME_JSON]) {
        if let Err(error) = registry.load_themes_from_str(json) {
            tracing::warn!("theme: 加载内置主题失败：{error}");
        }
    }
}

/// 读读者自己放进 `themes/` 的 JSON，返回装进去的份数。
///
/// 目录不存在返回 0，不是错误：里面的东西全是可选的。顺序按文件名排过，所以
/// 一个文件重定义已有名字时结果稳定，不依赖目录遍历顺序。
pub fn register_user_themes(cx: &mut App) -> usize {
    let dir = folio_core::infra::paths::themes_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();

    let mut loaded = 0;
    for path in paths {
        match std::fs::read_to_string(&path) {
            Ok(json) => {
                let registry = ThemeRegistry::global_mut(cx);
                match registry.load_themes_from_str(&json) {
                    Ok(()) => loaded += 1,
                    Err(error) => tracing::warn!("theme: 加载 {} 失败：{error}", path.display()),
                }
            }
            Err(error) => tracing::warn!("theme: 读取 {} 失败：{error}", path.display()),
        }
    }
    loaded
}

/// `mode` 那一侧有哪些主题名可选，按 registry 的显示顺序。
pub fn theme_names(cx: &App, mode: ThemeMode) -> Vec<SharedString> {
    ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .filter(|theme| theme.mode == mode)
        .map(|theme| theme.name.clone())
        .collect()
}

/// 那一侧没选过主题、或选过的名字已经不存在时用的主题名。
/// 界面要把「当前用的是哪一套」显示出来时，得和 [`repaint`] 的回落规则同一个数。
pub fn default_name(mode: ThemeMode) -> &'static str {
    match mode {
        ThemeMode::Light => DEFAULT_LIGHT,
        ThemeMode::Dark => DEFAULT_DARK,
    }
}

/// 把主题的**应用字体**换成这台设备上真的存在的那个家族。
///
/// `Theme::font_family` 是每一个框架组件取文字字体的地方（`semantic_tokens()` 直接从
/// 这些公开字段算出 `typography.sans`），默认值 `.SystemUIFont` 在 Android 上可能连着
/// 整串回退名都解析不出来，而 `TextSystem::resolve_font` 解析不到是 **panic**。
/// 所以移动入口必须在建第一帧视图之前调它，之后再建视图才不会碰到那个 panic。
///
/// 桌面端不调：那里的字体由 fontconfig 解析，读者偏好里那个名字本来就能用，
/// 而且桌面端换字体是偏好里的事（见 `settings::fonts_page`），不是兜底。
pub fn set_application_font(family: &str, cx: &mut App) {
    Theme::global_mut(cx).font_family = family.into();
}

/// 滚动条那一条淡到什么程度 —— 静止 / 指针压上去与正在拖，两档各是当前主题墨色的几成。
///
/// 框架那一格默认拿的是主题的 `accent`，而且是**满强度**（`component/theme/schema.rs` 里那句
/// `apply_background_color!(scrollbar_thumb, fallback = tokens.accent)`）：一条品牌色竖条压在
/// 正文边上，比正文还抢眼。轨道那一条本来就铺 `background`，看不出来，所以显眼的只有这一根。
/// 这里换成「当下生效那本主题的 `foreground` 压薄」：静止 0.12 与纸面已经差不到一档灰度，
/// 伸手时回到 0.28 ——「看得见自己在哪儿」交还给那一下动作。
///
/// 只给两档不是省事：框架把 hover 与 active 投影成同一个色（`base_theme()` 里两处都读
/// `scrollbar_thumb_hover`），第三格没有地方落。
///
/// **写的是 alpha，不是颜色**：色相每次换肤从 `colors.foreground` 现取（见
/// [`apply_scrollbar_tint`]），浅深两套与读者自己 `themes/` 里那几本 JSON 都跟着走。
const SCROLLBAR_TINT: (f32, f32) = (0.12, 0.28);

/// 把 [`SCROLLBAR_TINT`] 按当前主题的墨色装进主题自己那两格。
///
/// 挂在换肤那一步**之后**，不在启动时装一次就走：`Theme::change` 里会 `apply_config` 把主题
/// 文件那一套色重新铺一遍（它自己的注释就写着「closure 之后编辑的东西会被文件里的值盖回去」），
/// 只在启动时改一次，下一次翻面就没了。
///
/// 走 [`Theme::update`] 而不是直接摸 `global_mut`：那条写路径把 `colors` 与 `tokens` 两侧对齐
/// （投影给底层滚动条的是 `tokens.scrollbar_thumb`，只改 `colors` 那一侧对不上），再 `sync_base`
/// 推下去。调用方 [`repaint`] 只在「配置或解析出的浅深真的变了」那一帧走（`shell` 那侧比过一对
/// 值才调），所以这里多出来的那一次 `sync_base` 不是每帧的开销，也不会自己触发重画。
fn apply_scrollbar_tint(cx: &mut App) {
    let (rest, touch) = SCROLLBAR_TINT;
    Theme::update(cx, |theme| {
        let ink = theme.colors.foreground;
        theme.colors.scrollbar_thumb = ink.alpha(rest);
        theme.colors.scrollbar_thumb_hover = ink.alpha(touch);
    });
}

/// 存下来的浅深选择，配上操作系统的当前外观，实际解析成哪一档。
///
/// 水墨解析到**浅**那一侧：它是宣纸底，跟着系统翻面就没有「水墨」可言了。真正的颜色
/// 由 [`repaint`] 那一头换成 [`INK_LIGHT`] 那本，这一句只管「哪一侧」。
pub fn resolve_mode(appearance: Mode, os: WindowAppearance) -> ThemeMode {
    match appearance {
        Mode::System => ThemeMode::from(os),
        Mode::Light | Mode::Ink => ThemeMode::Light,
        Mode::Dark => ThemeMode::Dark,
    }
}

/// 按外观配置换肤，并刷所有窗口。
///
/// `window` 是给「跟随系统」用的：Linux 上 `cx.window_appearance()` 走一个
/// `RefRefCell`，外观观察者正持有它的借用，再从回调里查会 panic，所以开着窗口时
/// 直接读那个窗口的外观。没有窗口（在设置界面里改）时查平台是安全的，
/// 此时改成刷所有窗口 —— 主题 token 是应用级的，每个窗口都要重画。
pub fn apply(appearance: &Appearance, window: Option<&mut Window>, cx: &mut App) {
    repaint(appearance, window, cx);
    // 主题 token 是应用级的：不在渲染那一帧里改的话，每个窗口都得刷一遍。
    cx.refresh_windows();
}

/// 换肤的公共部分：解析浅深、挑两侧的主题、切 `Theme::mode`。
/// **不刷任何窗口**，所以可以在渲染那一帧里安全调用。
///
/// 移动端与桌面都走这一条：偏好一改、或系统翻了面，外壳重画那一帧时读**这一窗口**的浅深。
/// 一是上面那条借用规矩（`cx.window_appearance()` 在外观观察者的回调里会 panic），
/// 二是窗口的这一份由平台在系统翻面时主动写进来（Android 读 `Configuration.uiMode`，
/// 开窗时先取一次），跟着眼前这一屏走。
/// 返回解析出来的那一档，调用方要用它做别的判断时不必再算一遍。
pub fn repaint(appearance: &Appearance, window: Option<&mut Window>, cx: &mut App) -> ThemeMode {
    let os = window
        .as_ref()
        .map_or_else(|| cx.window_appearance(), |window| window.appearance());
    let mode = resolve_mode(appearance.mode, os);

    // 两个配置先从 registry 里取出来再动 `Theme`：它们都是 Global，
    // 一边借着 registry 一边可变地借 Theme 过不去借用检查。
    let (light, dark) = {
        let registry = ThemeRegistry::global(cx);
        let pick = |name: Option<&str>, fallback: &str| {
            name.and_then(|name| registry.themes().get(name).cloned())
                .or_else(|| registry.themes().get(fallback).cloned())
        };
        (
            // 水墨那一档把浅侧那一格**换成** [`INK_LIGHT`] 那本。`appearance.light` 里存的
            // 还是读者自己选过的那本：两侧的主题每次 repaint 都从 registry 重取，所以出了
            // 这一档下一帧就照旧生效，不必把名字存回去、也没有第二份状态要维护。
            // 认不出那一本时（嵌进来的文件缺了一格 = 构建问题）回落到读者原本选的那本，
            // 而不是回落到框架默认 —— 让偏好继续管用比让界面整齐要紧。
            if appearance.mode == Mode::Ink {
                registry
                    .themes()
                    .get(INK_LIGHT)
                    .cloned()
                    .or_else(|| pick(appearance.light.as_deref(), DEFAULT_LIGHT))
            } else {
                pick(appearance.light.as_deref(), DEFAULT_LIGHT)
            },
            pick(appearance.dark.as_deref(), DEFAULT_DARK),
        )
    };
    let theme = Theme::global_mut(cx);
    // 查不到就不动那一侧：把槽位清空比留着上一本读者的选择更糟。
    if let Some(light) = light {
        theme.light_theme = light;
    }
    if let Some(dark) = dark {
        theme.dark_theme = dark;
    }

    Theme::change(mode, window, cx);
    // 滚动条那一格跟着同一档现取：`Theme::change` 之后 `cx.theme()` 才是这一屏真正生效的那本。
    apply_scrollbar_tint(cx);
    // 状态栏图标跟着刚定下的这一档翻。挂在换肤这里而不是某个屏幕上：浅深是应用级的，
    // 而图标色只有**一处**真相，跟着 `mode` 走就不会出现「界面切了、图标没切」。
    crate::ui::platform::set_status_bar_icons(matches!(mode, ThemeMode::Light));
    mode
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_DARK, DEFAULT_LIGHT, INK_LIGHT, SCROLLBAR_TINT, register_builtin_themes, repaint,
        resolve_mode, theme_names,
    };
    use folio_core::infra::config::Mode;
    use gpui_kit::component::{Theme, ThemeMode};
    use gpui_kit::{Background, TestAppContext, WindowAppearance};

    /// registry 是用这两个名字播种的；上游改了名字，这里必须跟着改，
    /// 否则每一次查找回落都静默命中「查不到」，读者选的主题就不再生效。
    #[test]
    fn the_default_names_match_the_framework() {
        assert_eq!(DEFAULT_LIGHT, "Default Light");
        assert_eq!(DEFAULT_DARK, "Default Dark");
    }

    /// 滚动条那一根淡到静止不太看得见、伸手才回来，而且**色相跟着当下那一本主题**。
    ///
    /// 钉四件事，每一件都是这一格真会犯错的：① 装完之后静止档确实比框架那份默认
    /// （主题的 `accent`，满强度）淡；② 浅深两套各取自己那本的 `foreground`，不是把某一家的
    /// 墨写死 —— 只在浅档验一遍，深档那条品牌色还留在屏上也测不出来；③ 投影给底层滚动条读的
    /// 是 `tokens` 那一侧（`base_theme()` 里写 `self.tokens.scrollbar_thumb`），只改 `colors`
    /// 而 `reconcile` 没跟上，屏上一点变化都没有；④ 伸手那一档必须比重，否则「淡」连自己也
    /// 找不到了。
    #[gpui_kit::test]
    fn the_scrollbar_bar_is_quiet_and_follows_the_active_theme(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut inks = vec![];
        for mode in [Mode::Light, Mode::Dark] {
            cx.update(|app| {
                let mut appearance = folio_core::Config::default().appearance;
                appearance.mode = mode;
                // 走生产那一条钩子（`repaint`）而不是直接调那一格：它的**落点**是这一格的
                // 一半 —— 少了换肤之后的那一句，屏上还是主题那根品牌色。
                repaint(&appearance, None, app);
                let theme = Theme::global(app);
                let ink = theme.colors.foreground;
                inks.push(ink);
                assert_eq!(
                    theme.colors.scrollbar_thumb,
                    ink.alpha(SCROLLBAR_TINT.0),
                    "{mode:?} 静止档不是当下那本墨色压薄的那一档"
                );
                assert_eq!(
                    theme.colors.scrollbar_thumb_hover,
                    ink.alpha(SCROLLBAR_TINT.1),
                    "{mode:?} 伸手档"
                );
                assert!(
                    SCROLLBAR_TINT.0 < SCROLLBAR_TINT.1,
                    "伸手比静止还淡，那一根就永远摸不着了"
                );
                // 这一格默认拿的是 accent（主题文件没写它），满强度。
                assert!(
                    theme.colors.scrollbar_thumb.a < theme.colors.accent.a,
                    "装完不比这一本主题原本的色淡"
                );
                // 底层的滚动条拿的是这一侧，两个数都得跟上。
                assert_eq!(
                    *theme.tokens.scrollbar_thumb,
                    ink.alpha(SCROLLBAR_TINT.0),
                    "{mode:?} 只改了 colors，tokens 那一侧没对齐 —— 屏上不会有变化"
                );
                assert_eq!(
                    theme.tokens.scrollbar_thumb.background,
                    Background::from(ink.alpha(SCROLLBAR_TINT.0)),
                    "{mode:?} tokens 的 background 那一格没跟上"
                );
            });
        }
        // 两套的墨本来就不同；相同的话上面那几条断言就只是一句空话。
        assert_ne!(inks[0], inks[1], "夹具里浅深两本的 foreground 是同一个色");
    }

    #[test]
    fn system_follows_the_os_and_the_other_two_ignore_it() {
        assert_eq!(
            resolve_mode(Mode::System, WindowAppearance::Light),
            ThemeMode::Light
        );
        assert_eq!(
            resolve_mode(Mode::System, WindowAppearance::Dark),
            ThemeMode::Dark
        );
        assert_eq!(
            resolve_mode(Mode::Light, WindowAppearance::Dark),
            ThemeMode::Light
        );
        assert_eq!(
            resolve_mode(Mode::Dark, WindowAppearance::Light),
            ThemeMode::Dark
        );
    }

    /// 嵌入清单不该是空的：空了说明 build.rs 没找到依赖源码里的 `themes/`，
    /// 而那种情况下选择器会安静地只剩两套。
    #[test]
    fn the_bundled_catalogue_is_not_empty() {
        assert!(
            super::BUILTIN_THEME_JSON
                .iter()
                .any(|json| json.contains("catppuccin")),
            "嵌入的主题集里该有上游的那些主题"
        );
    }

    /// 水墨那一档**真的换到宣纸那本**，而且出了这一档能回到读者自己选过的那本。
    ///
    /// 这一条钉的是第四档唯一的可见后果。三种会安静坏掉的样子：`INK_LIGHT` 那个名字与
    /// 主题文件里的对不上（`repaint` 的回落把它变成「什么都没换」）；`resolve_mode` 把
    /// 它接到深侧（宣纸底配一套深色 token，屏上半浅半深）；`repaint` 里那一支写成了
    /// 改 `appearance.light`（出档时回不来，读者的偏好也被改掉）。
    #[gpui_kit::test]
    fn ink_mode_swaps_the_light_side_and_back(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(register_builtin_themes);
        cx.update(|app| {
            assert!(
                theme_names(app, ThemeMode::Light)
                    .iter()
                    .any(|name| name.as_ref() == INK_LIGHT),
                "水墨那一本没进 registry：`INK_LIGHT` 与文件里的名字对不上，或它没被装载"
            );
            // 这一档不跟着系统翻面：系统正深色时它仍旧走浅侧。
            assert_eq!(
                resolve_mode(Mode::Ink, WindowAppearance::Dark),
                ThemeMode::Light
            );

            let mut appearance = folio_core::Config::default().appearance;
            appearance.mode = Mode::Light;
            appearance.light = Some(DEFAULT_LIGHT.into());
            let before = {
                repaint(&appearance, None, app);
                Theme::global(app).colors.background
            };

            appearance.mode = Mode::Ink;
            repaint(&appearance, None, app);
            let theme = Theme::global(app);
            let (paper, ink) = (theme.colors.background, theme.colors.foreground);
            assert_ne!(paper, before, "选了水墨，纸面却还是原来那一本的颜色");
            // 墨要比纸暗、纸要真是浅色那一头：写反了就是亮字压在一面白纸上，
            // 而 `resolve_mode` 把这一档接到深侧时 `paper.l` 会先掉下来。
            assert!(ink.l < paper.l, "水墨这本的底色比字还亮");
            assert!(paper.l > 0.85, "宣纸底不是一张浅纸：l = {paper:?}");

            // 出档：`appearance.light` 那本照旧，而不是停在宣纸上、也不是落到框架默认。
            appearance.mode = Mode::Light;
            repaint(&appearance, None, app);
            assert_eq!(
                Theme::global(app).colors.background,
                before,
                "退出水墨之后浅侧没回到读者自己选过的那本"
            );
        });
    }

    /// 每个 mode 都至少能选到两套，且默认名一定在里面 —— 「选不中」在界面上
    /// 表现为下拉框空白，没有任何报错，所以只能在这里钉住。
    #[gpui_kit::test]
    async fn registered_themes_are_pickable_per_mode(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(register_builtin_themes);
        let (light, dark) = cx.update(|cx| {
            (
                theme_names(cx, ThemeMode::Light),
                theme_names(cx, ThemeMode::Dark),
            )
        });
        assert!(
            light.iter().any(|name| name == DEFAULT_LIGHT),
            "浅色侧该有框架默认那套"
        );
        assert!(
            dark.iter().any(|name| name == DEFAULT_DARK),
            "深色侧该有框架默认那套"
        );
        assert!(
            light.len() > 2,
            "浅色侧只剩默认那两套，说明嵌入的主题没进 registry"
        );
        assert!(
            dark.len() > 2,
            "深色侧只剩默认那两套，说明嵌入的主题没进 registry"
        );
    }
}
