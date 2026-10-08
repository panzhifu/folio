use std::path::PathBuf;

use folio_app::app::FolioApp;

use gpui_kit::component::*;
use gpui_kit::*;

fn main() {
    gpui_kit::application()
        // 默认图标包不含 library/book/circle-user 等，换 AllAssets 才拿得到全套 Lucide 图标。
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);

            // 主题集要在 `init` 之后装：那一步才把 `ThemeRegistry` 这个 Global 建出来。
            // 先装内置的（构建时从依赖源码嵌进来），再装读者自己放进 `themes/` 的，
            // 后者的同名项按 registry 的规则不覆盖已有名字，所以顺序决定谁优先。
            folio_app::theme::register_builtin_themes(cx);
            folio_app::theme::register_user_themes(cx);

            // 配置先读出来，并立刻把界面语言推给全局：下面任何一条要给人看的消息
            // （包括书库打不开）都得用读者自己选的语言写。
            let config = folio_core::Config::load();
            config.language.locale.apply();

            let store = match folio_core::Store::open_default() {
                Ok(store) => store,
                Err(e) => {
                    eprintln!(
                        "{}",
                        folio_core::i18n::t_with("error.database_unopenable", &[("reason", &e)])
                    );
                    std::process::exit(1);
                }
            };
            // 配置存成按系统标准目录的 JSON 文件；这里把落盘目标交给配置持有者。
            cx.set_global(folio_app::config_model::ConfigPath(Some(
                folio_core::config_path(),
            )));

            // 命令行传入的路径直接导入并打开：既是「用 Folio 打开这本书」的正常用法，
            // 也让端到端验证可以不依赖 GUI 自动化。
            let startup: Vec<PathBuf> = std::env::args()
                .skip(1)
                .filter(|arg| !arg.starts_with('-'))
                .map(PathBuf::from)
                .collect();

            // 阅读字体是否已由 fontconfig 解析到，决定「字体随包」的优先级。
            let probe = Font {
                family: config.reader.font_family.clone().into(),
                ..Default::default()
            };
            println!(
                "字体探测：{} -> {:?}",
                config.reader.font_family,
                cx.text_system().resolve_font(&probe)
            );

            // 自定义标题栏：以 `TitleBar::window_options()` 为基底（透明标题栏 +
            // 由应用自己负责拖拽），再补上 Wayland 窗口分组需要的 app_id。
            // `title` 只用于任务栏/切换器显示，标题栏本身由 FolioApp 绘制。
            let mut options = TitleBar::window_options();
            options.titlebar = Some(TitlebarOptions {
                title: Some("Folio".into()),
                ..TitleBar::title_bar_options()
            });
            options.app_id = Some("dev.folio.Reader".into());
            options.window_min_size = Some(size(px(520.), px(360.)));

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| FolioApp::new(store, config, startup, window, cx))
            })
            .expect("failed to open Folio window");
        });
}
