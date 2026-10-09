//! 桌面入口：装主题集、装平台服务、开书库、开窗。

use folio_core::{Config, Store};
use gpui_kit::component::TitleBar;
use gpui_kit::{App, AppContext as _, Font, TitlebarOptions, px, size};

use crate::ui::host;
use crate::ui::shared::config_model::{ConfigHolder, ConfigPath};
use crate::ui::shared::theme;
use crate::ui::shell::AppShell;

/// 桌面入口：`main.rs` 在非移动 target 上调它。
pub fn run_desktop() {
    gpui_kit::application()
        // 默认图标包不含 library/book/circle-user 等，换 AllAssets 才拿得到全套 Lucide 图标。
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);

            // 主题集要在 `init` 之后装：那一步才把 `ThemeRegistry` 这个 Global 建出来。
            // 先装内置的（构建时从依赖源码嵌进来），再装读者自己放进 `themes/` 的。
            theme::register_builtin_themes(cx);
            theme::register_user_themes(cx);
            // 平台服务边界（安全区、输入法）。桌面两者都是 0。
            host::install(cx);

            // 配置先读出来，并立刻把界面语言推给全局。
            let config = Config::load();
            config.language.locale.apply();

            let store = match Store::open_default() {
                Ok(store) => store,
                Err(e) => {
                    eprintln!(
                        "{}",
                        folio_core::i18n::t_with("error.database_unopenable", &[("reason", &e)])
                    );
                    std::process::exit(1);
                }
            };
            // 书库挂成全局：书架、阅读、批注都从这里读同一个库。
            crate::ui::shared::app_store::install(store, cx);
            // 配置落盘到系统标准目录，再把配置挂成可观察的 Global。
            // `install` 必须在开窗前：`AppShell::boot` 一上来就查读者偏好定字体，
            // 而外壳的订阅要拿到那个实体 —— 晚一步就是「no state of type ConfigHolder」。
            cx.set_global(ConfigPath(Some(folio_core::config_path())));
            ConfigHolder::install(config, cx);

            // 阅读字体是否已由 fontconfig 解析到，决定「字体随包」的优先级。
            let probe = Font {
                family: ConfigHolder::reader(cx).font_family.into(),
                ..Default::default()
            };
            println!(
                "字体探测：{} -> {:?}",
                probe.family,
                cx.text_system().resolve_font(&probe)
            );

            // 自定义标题栏：以 `TitleBar::window_options()` 为基底（透明标题栏 +
            // 应用自己负责拖拽），再补上 Wayland 窗口分组需要的 app_id。
            let mut options = TitleBar::window_options();
            options.titlebar = Some(TitlebarOptions {
                title: Some("Folio".into()),
                ..TitleBar::title_bar_options()
            });
            options.app_id = Some("dev.folio.Reader".into());
            options.window_min_size = Some(size(px(520.), px(360.)));

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| AppShell::boot(window, cx))
            })
            .expect("failed to open Folio window");
        });
}
