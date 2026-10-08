//! 主题：用 UI 框架自己的主题系统选配色，本应用**不定义任何颜色值**。
//!
//! 模型和 gpui-kit 一致，分两层：
//!
//! - **主题集**是别人的东西：框架自带的那几套（构建时从依赖源码嵌进来，见 `build.rs`）
//!   加上用户自己放进 `~/.config/folio/themes/*.json` 的，全部由 `ThemeRegistry` 按名字管；
//! - **外观**是我们自己的偏好：浅深怎么选、解析到浅和深时各用哪一套主题名，存在
//!   [`folio_core::config::Appearance`] 里。
//!
//! 所以换肤就是把 `Theme` 这个 Global 的 `light_theme` / `dark_theme` 两个槽换成选中的
//! 配置，再切 `mode`；框架的每个组件都读 `cx.theme()`，不需要谁去通知它们。
//!
//! 主题名认不出时回落到框架自带的 `Default Light` / `Default Dark`：主题可能被改名、
//! 被从目录里删掉，那时下拉框选不中但不该整界面变白。

use folio_core::config::{Appearance, Mode};
use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{App, SharedString, Window, WindowAppearance};

/// 框架内置的默认主题名，存在 `ThemeRegistry` 里，不由我们嵌入。
/// 没设过主题、或设过的名字已经不存在时用它们兜底。
pub const DEFAULT_LIGHT: &str = "Default Light";
pub const DEFAULT_DARK: &str = "Default Dark";

// `BUILTIN_THEME_JSON` 由 build.rs 从依赖源码里的 `themes/` 生成。
include!(concat!(env!("OUT_DIR"), "/folio_themes.rs"));

/// 把嵌入的主题集灌进 registry。要在 `gpui_kit::init` 之后调用 —— 那一步才把
/// registry 这个 Global 建出来。
pub fn register_builtin_themes(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    for json in BUILTIN_THEME_JSON {
        if let Err(error) = registry.load_themes_from_str(json) {
            eprintln!("加载内置主题失败：{error}");
        }
    }
}

/// 读读者自己放进 `themes/` 的 JSON，返回装进去的份数。
///
/// 目录不存在返回 0，不是错误：里面的东西全是可选的。顺序按文件名排过，所以
/// 一个文件重定义已有名字时结果稳定，不依赖目录遍历顺序。
pub fn register_user_themes(cx: &mut App) -> usize {
    let dir = folio_core::paths::themes_dir();
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
                    Err(error) => eprintln!("加载主题 {} 失败：{error}", path.display()),
                }
            }
            Err(error) => eprintln!("读取主题 {} 失败：{error}", path.display()),
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

/// 存下来的浅深选择，配上操作系统的当前外观，实际解析成哪一档。
pub fn resolve_mode(appearance: Mode, os: WindowAppearance) -> ThemeMode {
    match appearance {
        Mode::System => ThemeMode::from(os),
        Mode::Light => ThemeMode::Light,
        Mode::Dark => ThemeMode::Dark,
    }
}

/// 按外观配置换肤。
///
/// `window` 是给「跟随系统」用的：Linux 上 `cx.window_appearance()` 走一个
/// `RefRefCell`，外观观察者正持有它的借用，再从回调里查会 panic，所以开着窗口时
/// 直接读那个窗口的外观。没有窗口（在设置界面里改）时查平台是安全的，
/// 此时改成刷所有窗口 —— 主题 token 是应用级的，每个窗口都要重画。
pub fn apply(appearance: &Appearance, window: Option<&mut Window>, cx: &mut App) {
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
            pick(appearance.light.as_deref(), DEFAULT_LIGHT),
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
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_DARK, DEFAULT_LIGHT, register_builtin_themes, resolve_mode, theme_names};
    use folio_core::config::Mode;
    use gpui_kit::component::ThemeMode;
    use gpui_kit::{TestAppContext, WindowAppearance};

    /// registry 是用这两个名字播种的；上游改了名字，这里必须跟着改，
    /// 否则每一次查找回落都静默命中「查不到」，读者选的主题就不再生效。
    #[test]
    fn the_default_names_match_the_framework() {
        assert_eq!(DEFAULT_LIGHT, "Default Light");
        assert_eq!(DEFAULT_DARK, "Default Dark");
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
