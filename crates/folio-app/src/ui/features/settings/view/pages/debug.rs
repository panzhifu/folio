//! 调试：库文件在哪儿、多大、几本书，以及日志最后写了什么。
//!
//! **这一页只在 debug 构建或 `debug-ui` 特性下存在**（登记在 [`SettingsPage::ALL`] 的末尾，
//! 见那一格；手机上要带它就用那个特性打包，理由写在 `Cargo.toml` 的特性注释里）。它的
//! 读者是改代码的人，不是读者，所以它不进任何一档正式导航解释，也不为它写「这一页能干什么」
//! 的引导句 —— 要读的人是带着问题来的。
//!
//! 两组各答一件事：
//!
//! - **库**：`folio.db` 与两条附档（`-wal` / `-shm`）的字节、书目行数、书库位置。报错只报
//!   一条原因，读库失败不把日志那一段一起判死。
//! - **日志**：这一份二进制现在往哪写、写了多厚、最后二十行长什么样。日志关着时这里说的是
//!   「关着」，不是「空着」—— 那两件事在排查时结论完全不同。
//!
//! 每一格都是**现读实体**（[`debug_panel`]）：读数在后台线程上跑，这一页只摆已经取回来的
//! 快照。手机上钻进这一页时会自己起一趟（见 `compact.rs` 的 `open`，与「存储」「备份」同一处），
//! 桌面没有那个钩子（页的选择在 `Settings` 组件内部），所以第一份读数靠那一颗「重新读取」——
//! 两端各按自己能拿到的时机起活，读数本身是同一份。

use super::super::*;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{ParentElement as _, Styled as _};

use crate::ui::components::muted_label;
use crate::ui::services::debug_panel;
use crate::ui::services::l10n::integer;
use crate::ui::services::storage::human_bytes;

/// 这一页。组序与 [`debug_page`] 的模块说明一致：先库、再日志。
pub(in crate::ui::features::settings) fn debug_page(cx: &App) -> Page {
    page(
        tr(cx, "page.debug"),
        IconName::Bug,
        vec![library_group(cx), log_group(cx)],
    )
}

/// 库那一组：三处字节、书目行数、书库位置，末尾一颗重新读取。
fn library_group(cx: &App) -> Group {
    let snapshot = snapshot_now(cx);
    let db = folio_core::store::default_db_path();
    let dir = ConfigHolder::snapshot(cx).library.dir.clone();

    // 路径不摆进 `Row::info` 的值那一栏：绝对路径比控件那一栏宽得多，手机上会把标题挤成
    // 一格一个字（「库／文／件」竖着排），桌面上则把路径截成两行看不清。整条铺开、标题在上，
    // 与「存储」页那几行路径同一个形状。
    let mut rows = vec![
        Row::custom_block(tr(cx, "field.debug_db"), move |_, cx| {
            muted_label(db.display().to_string(), cx).into_any_element()
        })
        .keywords(&["db", "sqlite", "库文件"]),
    ];
    rows.push(match snapshot.as_ref() {
        Some(snapshot) => Row::info(
            tr(cx, "field.debug_db_size"),
            human_bytes(
                cx,
                snapshot.db_bytes + snapshot.wal_bytes + snapshot.shm_bytes,
            ),
        )
        .description(tr(cx, "field.debug_db_size.desc"))
        .keywords(&["size", "大小", "wal", "shm"]),
        None => not_read(cx),
    });
    // 查库失败那一句原样带出来：这一页的价值就在「为什么是零本」这种问题上，把它糊成
    // 「还没读取」等于把现场擦了。`Row::description` 那一行只有桌面摆，而这一页的读者
    // 多半在桌面上 —— 手机上那半句也进得去（见 `compact.rs` 对 Info 行的处理）。
    let books_error = snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.books_error.clone());
    rows.push(
        match snapshot.as_ref().and_then(|snapshot| snapshot.books) {
            Some(books) => {
                let row = Row::info(tr(cx, "field.debug_books"), integer(cx, books as u64))
                    .keywords(&["books", "书目", "数量"]);
                match books_error {
                    Some(error) => row.description(error),
                    None => row,
                }
            }
            None => match books_error {
                Some(error) => Row::info(tr(cx, "field.debug_books"), "—").description(error),
                None => not_read(cx),
            },
        },
    );
    rows.push(if dir.trim().is_empty() {
        Row::info(
            tr(cx, "field.library_dir"),
            tr(cx, "usage.library_unconfirmed"),
        )
    } else {
        Row::info(tr(cx, "field.library_dir"), dir).keywords(&["library", "书库", "目录"])
    });
    rows.push(reload_row());
    group(tr(cx, "group.debug_library"), rows)
}

/// 日志那一组：写在哪、多厚、最后二十行。
fn log_group(cx: &App) -> Group {
    let snapshot = snapshot_now(cx);
    let mut rows = Vec::new();
    rows.push(match snapshot.as_ref().map(|snapshot| &snapshot.log_path) {
        // 路径与库文件那一行同一个形状，理由见 `library_group` 顶上那条注释。
        Some(Some(path)) => {
            let shown = path.display().to_string();
            Row::custom_block(tr(cx, "field.debug_log_file"), move |_, cx| {
                muted_label(shown.clone(), cx).into_any_element()
            })
            .keywords(&["log", "日志", "路径"])
        }
        // 读过、且知道日志关着：这是配置里的结论，不是「还没读」。
        Some(None) => Row::info(tr(cx, "field.debug_log_file"), tr(cx, "usage.logging_off")),
        None => not_read(cx),
    });
    rows.push(match snapshot.as_ref() {
        Some(snapshot) if snapshot.log_path.is_some() => {
            let row = Row::info(
                tr(cx, "field.debug_log_size"),
                human_bytes(cx, snapshot.log_bytes),
            )
            .keywords(&["size", "大小", "日志"]);
            match snapshot.log_error.clone() {
                Some(error) => row.description(error),
                None => row,
            }
        }
        _ => not_read(cx),
    });
    rows.push(log_block());
    group(tr(cx, "group.debug_logs"), rows)
}

/// 一趟读数还没回来时，每一格说的同一句话。**必须与「零」分得开**：零本书是真的零本，
/// 而「还没读」只是这一屏刚建出来。值那一格给一道短横，不另起一句话。
fn not_read(cx: &App) -> Row {
    Row::info(tr(cx, "field.debug_not_read"), "—")
}

/// 当前快照；实体还没建、或一趟都还没跑过时是 `None`。
fn snapshot_now(cx: &App) -> Option<debug_panel::Snapshot> {
    debug_panel::handle(cx).and_then(|panel| panel.read(cx).snapshot().cloned())
}

/// 那一颗「重新读取」。按下去起一趟后台读数，回来时实体 `notify` → 这一屏重画。
fn reload_row() -> Row {
    Row::custom(|_, cx| {
        let busy = debug_panel::handle(cx)
            .as_ref()
            .is_some_and(|panel| panel.read(cx).busy());
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                Button::new("debug-reload")
                    .label(tr(cx, "action.debug_reload").to_string())
                    .ghost()
                    .small()
                    .disabled(busy)
                    .on_click(|_, _, cx| debug_panel::reload(cx)),
            )
            .into_any_element()
    })
    .keywords(&["debug", "reload", "调试", "重新读取"])
}

/// 日志尾巴那一块。三种状态各有一句话，理由见 [`debug_panel`] 模块头。
fn log_block() -> Row {
    Row::custom(|_, cx| {
        let Some(panel) = debug_panel::handle(cx) else {
            return muted_label(tr(cx, "field.debug_not_read"), cx).into_any_element();
        };
        let panel = panel.read(cx);
        let Some(snapshot) = panel.snapshot() else {
            return muted_label(tr(cx, "field.debug_not_read"), cx).into_any_element();
        };
        match (&snapshot.log, &snapshot.log_error) {
            // 不设最大高度：那一行贴死在半截上比多滚一段更难读（日志每行都长，2–3 行折一次）。
            // 行数由 [`debug_panel`] 那头收（二十行），这一屏本来就能滚。
            (Some(text), _) => v_flex()
                .w_full()
                .rounded_md()
                .bg(cx.theme().muted)
                .p_3()
                .child(
                    div()
                        .w_full()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(text.clone())),
                )
                .into_any_element(),
            // 读过、但读失败：说失败，不说「空」。
            (None, Some(error)) => muted_label(error.clone(), cx).into_any_element(),
            (None, None) if snapshot.log_path.is_none() => {
                muted_label(tr(cx, "usage.logging_off"), cx).into_any_element()
            }
            (None, None) => muted_label(tr(cx, "field.debug_log_empty"), cx).into_any_element(),
        }
    })
    .keywords(&["log", "日志", "tail", "尾巴"])
}

#[cfg(test)]
mod tests {
    use folio_core::Config;
    use gpui_kit::TestAppContext;

    use super::{debug_page, snapshot_now};
    use crate::ui::services::config::ConfigHolder;
    use crate::ui::services::l10n::tr;

    /// 一页两组，且没装读数实体时每一格说的是「还没读取」而不是零 —— 后者会让人以为
    /// 库里真的一本书都没有。
    #[gpui_kit::test]
    fn the_page_has_two_groups_and_reads_as_not_read_before_any_load(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| ConfigHolder::install(Config::default(), cx));
        cx.read(|cx| {
            assert!(snapshot_now(cx).is_none());
            let page = debug_page(cx);
            assert_eq!(page.title, tr(cx, "page.debug"));
            assert_eq!(page.groups.len(), 2, "库与日志两组");
            assert_eq!(page.groups[0].title, tr(cx, "group.debug_library"));
            assert_eq!(page.groups[1].title, tr(cx, "group.debug_logs"));
        });
    }
}
