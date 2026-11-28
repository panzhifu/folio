//! 桌面入口：装主题集、装平台服务、开书库、开窗。

use folio_core::Store;
use gpui_kit::component::TitleBar;
use gpui_kit::{App, AppContext as _, Font, TitlebarOptions, WindowBackgroundAppearance, px, size};

use crate::ui::foundation::theme;
use crate::ui::host;
use crate::ui::services::config::{ConfigHolder, ConfigPath};
use crate::ui::shell::AppShell;

/// 桌面入口：`main.rs` 在非移动 target 上调它。
pub fn run_desktop() {
    if !super::restore::activate() {
        return;
    }
    // 第一件事是**读配置并装日志**（§5.6：配置 → 日志）：`logging` 那一节就在配置里，
    // 而它得在第一条日志之前生效。`Config::load` 只要文件系统，不需要 App 上下文，所以放在
    // 这里而不是 `run` 里。装不上（比如别处已经占了这个槽）只影响诊断输出，程序照起。
    let (resolved, log_err) = folio_core::infra::logging::init_from_config(None);
    if let Err(err) = log_err {
        eprintln!("日志装不上，这一轮只有 stderr 上的这一句：{err}");
    }

    gpui_kit::application()
        // 图标子集（folio 用到的 + gpui-kit 组件默认名单兜底），不再嵌全部 1830 个
        // lucide svg —— 名单见 platform::icons。
        .with_assets(super::icons::FolioAssets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            // 替换gpui 默认的「不发请求」的 client，方便后续进行同步

            // 主题集要在 `init` 之后装：那一步才把 `ThemeRegistry` 这个 Global 建出来。
            // 先装内置的（构建时从依赖源码嵌进来），再装读者自己放进 `themes/` 的。
            theme::register_builtin_themes(cx);
            theme::register_user_themes(cx);
            // 平台服务边界（安全区、输入法）。桌面两者都是 0。
            host::install(cx);

            // 配置在外面读好了带进来，这里只把界面语言推给全局。
            resolved.effective.language.locale.apply();

            let store = match Store::open_default() {
                Ok(store) => store,
                Err(e) => {
                    // 两条不同的话，给两个不同的读者：日志里要的是整条链（哪一步、
                    // 哪个 errno），屏幕上只要那句能看懂的。`{:?}` 是 thiserror 那个枚举
                    // 自己的 Debug，`#[source]` 上那条底层错误也在里面。
                    tracing::error!("desktop: 书库打不开：{e:?}");
                    eprintln!(
                        "{}",
                        folio_core::infra::i18n::t_with(
                            "error.database_unopenable",
                            &[("reason", &e)]
                        )
                    );
                    std::process::exit(1);
                }
            };
            // 启动现场（§4.2）：库是哪个 schema 版本 —— 报障时这一行能把「迁移没跑」
            // 与「跑过了」分开。留一份给下面的诊断 dump（§5.8）。
            let db_version = match store.user_version() {
                Ok(version) => {
                    tracing::info!("库 schema v{version}");
                    Some(version)
                }
                Err(err) => {
                    tracing::warn!("库 schema 版本读不出：{err:#}");
                    None
                }
            };
            // 缺失或暂不可读的书源只报告，进度与批注仍属于读者。
            match store.unavailable_sources() {
                Ok(unavailable) if !unavailable.is_empty() => {
                    tracing::warn!(
                        "desktop: {} 本书的源文件不可用，已保留数据：{unavailable:?}",
                        unavailable.len()
                    );
                }
                Ok(_) => {}
                Err(err) => tracing::warn!("desktop: 检查书源失败：{err:#}"),
            }
            // 书库挂成全局：书架、阅读、批注都从这里读同一个库。
            crate::ui::services::app_store::install(store, cx);
            // 配置落盘到系统标准目录，再把配置挂成可观察的 Global。
            // `install` 必须在开窗前：`AppShell::boot` 一上来就查读者偏好定字体，
            // 而外壳的订阅要拿到那个实体 —— 晚一步就是「no state of type ConfigHolder」。
            cx.set_global(ConfigPath(Some(folio_core::config_path())));
            ConfigHolder::install_resolved(resolved, cx);
            // 常驻资源（导入作业池 + 监听 + 传书）收拢到一个进程级句柄里；书库句柄也交过去，
            // 好让关停时「作业池先于库连接销毁」。
            crate::ui::services::resident::install(cx);
            // 网盘凭据的轮换落盘（后台线程续期 → 通道 → 主线程写回 config）。
            crate::ui::services::cloud_auth::install(cx);
            // 诊断 dump（§5.8）：`FOLIO_DUMP=1` 时把生效配置 + 库版本 + 队列长度打一份。
            if folio_core::infra::diagnostics::enabled() {
                let provenance = ConfigHolder::provenance(cx);
                folio_core::infra::diagnostics::dump(
                    &ConfigHolder::snapshot(cx),
                    Some(&provenance),
                    db_version,
                    Some(crate::ui::services::resident::pool().queued()),
                    // 预算执行端已接线：物理内存由 host 读，读不到时 `for_device`
                    // 自己回退设备档。
                    Some(folio_core::MemoryBudget::for_device(
                        crate::ui::host::physical_memory_bytes(),
                    )),
                );
            }
            // 清掉上次崩溃留在书库目录里的 `.part` 半成品（§1.3 末条）。
            crate::ui::services::library::sweep_partials(cx);
            // 老副本改名的那一趟：投进 Cleanup 档，不等它、也不在界面线程上算哈希。
            // 封面补齐：升级把 books.cover 那个 BLOB 删了之后，老行的封面得从书文件里重抽一次。
            crate::ui::services::library::schedule_cover_backfill(cx);
            // 监听是后台能力：配置开着就随启动起，活过任何一屏。
            crate::ui::services::watch::sync_from_config(cx);
            crate::ui::services::source_updates::start(cx);

            // 阅读字体是否已由 fontconfig 解析到，决定「字体随包」的优先级。
            let probe = Font {
                family: ConfigHolder::reader(cx).font_family.into(),
                ..Default::default()
            };
            tracing::info!(
                "desktop: 字体探测 {} -> {:?}",
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
            // 低干扰偏好只对窗口背景做启动期设置；模式切换由服务在运行期改。
            let stealth = ConfigHolder::stealth(cx);
            if stealth.enabled && stealth.mode != folio_core::StealthMode::Off {
                options.window_background = WindowBackgroundAppearance::Transparent;
            }

            gpui_kit::open_window(options, cx, |window, cx| {
                host::install_stealth(cx);
                crate::ui::services::stealth::install(window.window_handle(), cx);
                cx.new(|cx| AppShell::boot(window, cx))
            })
            .expect("failed to open Folio window");
        });

    // `run` 返回 = 窗口全关、`App` 已 drop（它的 `AppStore` 句柄也放了）。
    // 这时按 §1.3/§5.6 的有序收尾：停监听/传书 → 关作业池 → 放掉我们这份最后一个书库句柄。
    crate::ui::services::resident::shutdown();
}
