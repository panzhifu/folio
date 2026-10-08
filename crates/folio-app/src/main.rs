mod app;
mod reader;

use std::path::PathBuf;

use gpui_kit::component::*;
use gpui_kit::*;

use app::FolioApp;

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);

            let store = match folio_core::Store::open_default() {
                Ok(store) => store,
                Err(e) => {
                    eprintln!("打不开书库数据库：{e}");
                    std::process::exit(1);
                }
            };
            let prefs = store.prefs();

            // 命令行传入的路径直接导入并打开：既是「用 Folio 打开这本书」的正常用法，
            // 也让端到端验证可以不依赖 GUI 自动化。
            let startup: Vec<PathBuf> = std::env::args()
                .skip(1)
                .filter(|arg| !arg.starts_with('-'))
                .map(PathBuf::from)
                .collect();

            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Folio".into()),
                    ..TitleBar::title_bar_options()
                }),
                app_id: Some("dev.folio.Reader".into()),
                ..Default::default()
            };

            // 阅读字体是否已由 fontconfig 解析到，决定任务「字体随包」要不要立刻做。
            let probe = Font {
                family: prefs.font_family.clone().into(),
                ..Default::default()
            };
            println!(
                "字体探测：{} -> {:?}",
                prefs.font_family,
                cx.text_system().resolve_font(&probe)
            );

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| FolioApp::new(store, prefs, startup, window, cx))
            })
            .expect("failed to open Folio window");
        });
}
