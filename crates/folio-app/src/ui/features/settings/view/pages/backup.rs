//! 备份：把库、设置与（可选的）书文件打成一个 zip，以及管理已经打好的那几份。
//!
//! 这一页回答两个问题，分组也跟着这两个问题走：
//!
//! 1. **现在打一份** —— 一个「要不要带书文件」的开关加一颗按钮。开关决定包的大小，
//!    而读者在慢网络 / 空间紧的那台机器上会一直选小档，所以它是这一页唯一记住的东西
//!    （`config::Backup`）。
//! 2. **打好的在哪儿** —— 一份按时间倒序的清单，每行带大小、是否完整与一颗删除。
//!    删除是**两份动作**：先摊开确认面板，再真删；那一格在确认时整行换成面板，与
//!    「存储」那一屏同一形状。
//!
//! 清单与打包都走 [`crate::ui::services::backup`] 的作业实体。这一页自己**不碰盘**：
//! 每格现读实体（静态设置页每帧重建，捕获进来就是一份旧账），后台跑完 `notify`，
//! 两端的设置屏重画。
//!
//! 包里还有一份 `config.json`，里面有同步密码与 API 密钥。这一页把那句警告摆在按钮
//! 下面 —— 备份是拿去换机器的东西，读者至少该知道它装了什么。

use super::super::*;

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{AnyElement, FontWeight};

use crate::ui::components::muted_label;
use crate::ui::services::backup::{self, human_bytes};
use folio_core::backup::Entry;
use folio_core::infra::time::{DAY, civil_from_days, local_day};

/// 这一页。
pub(in crate::ui::features::settings) fn backup_page(cx: &App) -> Page {
    page(
        tr(cx, "page.backup"),
        IconName::Archive,
        vec![
            group(tr(cx, "group.backup_create"), create_rows(cx)),
            group(tr(cx, "group.backup_list"), list_rows(cx)),
            group(tr(cx, "group.location"), location_rows(cx)),
        ],
    )
}

/// 「现在打一份」这一组的搜索词。
const CREATE_FIND: &[&str] = &[
    "backup", "archive", "zip", "export", "备份", "打包", "导出", "存档",
];

/// 「打好的」这一组的搜索词。
const LIST_FIND: &[&str] = &["backup", "archive", "restore", "备份", "存档", "恢复"];

/// 开关 + 那颗按钮。
fn create_rows(cx: &App) -> Vec<Row> {
    vec![
        Row::toggle(
            tr(cx, "field.backup_include_files"),
            |cx: &App| ConfigHolder::backup(cx).include_files,
            |on, cx| ConfigHolder::edit_backup(cx, |backup| backup.include_files = on),
        )
        .description(tr(cx, "field.backup_include_files.desc"))
        .keywords(CREATE_FIND),
        Row::custom(move |_, cx| create_block(cx)).keywords(CREATE_FIND),
    ]
}

/// 那颗「立即备份」与它的三句反馈：说明、密钥警告、上一次的结果 / 失败原因。
fn create_block(cx: &App) -> AnyElement {
    let view = backup::handle(cx);
    let busy_job = view.as_ref().and_then(|view| view.read(cx).busy_job());
    let busy = busy_job.is_some();
    let creating = busy_job == Some(backup::Job::Create);
    let last = view.as_ref().and_then(|view| {
        view.read(cx)
            .last()
            .map(|created| (created.manifest.created_at, created.bytes))
    });
    let reason = view.as_ref().and_then(|view| view.read(cx).reason());

    let mut block = v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_3()
                .child(
                    Button::new("backup-create")
                        .primary()
                        .icon(IconName::Archive)
                        .label(if creating {
                            tr(cx, "backup.creating").to_string()
                        } else {
                            tr(cx, "action.backup_now").to_string()
                        })
                        .disabled(busy)
                        .on_click(|_, _, cx| backup::request(cx, backup::Job::Create)),
                )
                .child(muted_label(tr(cx, "backup.create.desc"), cx)),
        )
        // 这一句是**安全警告**，不是说明文字：包里那份 config.json 带着账号。
        .child(muted_label(tr(cx, "backup.secrets"), cx));

    if let Some((at, bytes)) = last {
        block = block.child(muted_label(
            tf(
                cx,
                "backup.last",
                &[("when", &when(cx, at)), ("size", &human_bytes(cx, bytes))],
            ),
            cx,
        ));
    }
    if let Some(reason) = reason {
        block = block.child(div().text_sm().text_color(cx.theme().danger).child(tf(
            cx,
            "backup.failed",
            &[("reason", &reason)],
        )));
    }
    block.into_any_element()
}

/// 已经打好的那几份。还没读过目录时是一句「正在读取」，空目录是一句「还没有备份」。
fn list_rows(cx: &App) -> Vec<Row> {
    let Some(view) = backup::handle(cx) else {
        return vec![note_row(cx, "backup.loading")];
    };
    let state = view.read(cx);
    let Some(entries) = state.entries() else {
        return vec![note_row(cx, "backup.loading")];
    };
    if entries.is_empty() {
        return vec![note_row(cx, "backup.empty")];
    }
    // 先拷一份再放锁：每行都是一个会在别的帧里重跑的闭包，握不住这一帧的读守卫。
    let entries = entries.to_vec();
    let pending = state.pending_delete().cloned();
    entries
        .into_iter()
        .map(|entry| entry_row(entry, pending.clone()))
        .collect()
}

fn note_row(cx: &App, key: &'static str) -> Row {
    let text = tr(cx, key);
    Row::custom(move |_, cx| muted_label(text.clone(), cx).into_any_element()).keywords(LIST_FIND)
}

/// 一行备份：时间 + 大小/范围 + 删除。要删的那一行整行换成确认面板。
fn entry_row(entry: Entry, pending: Option<PathBuf>) -> Row {
    let path = entry.path.clone();
    let id = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "backup".to_string());
    Row::custom(move |_, cx| {
        let asked = pending.as_deref() == Some(path.as_path());
        if asked {
            return delete_panel(entry.clone(), cx);
        }
        let busy = backup::handle(cx).is_some_and(|view| view.read(cx).busy());
        let target = path.clone();
        let meta = if entry.include_files() {
            tf(
                cx,
                "backup.entry.full",
                &[
                    ("size", &human_bytes(cx, entry.bytes)),
                    ("files", &entry.files().to_string()),
                ],
            )
        } else {
            tf(
                cx,
                "backup.entry.data",
                &[("size", &human_bytes(cx, entry.bytes))],
            )
        };
        let scope = if entry.include_files() {
            tr(cx, "backup.scope.files")
        } else {
            tr(cx, "backup.scope.data")
        };
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_0p5()
                    .child(
                        div()
                            .text_base()
                            .text_ellipsis()
                            .child(when(cx, entry.created_at())),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .text_ellipsis()
                            .child(meta),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(scope),
            )
            .child(
                Button::new(format!("backup-delete-{id}"))
                    .ghost()
                    .small()
                    .icon(IconName::Trash)
                    .label(tr(cx, "action.delete").to_string())
                    .accessibility_label(when(cx, entry.created_at()))
                    .disabled(busy)
                    .on_click(move |_, _, cx| backup::ask_delete(cx, target.clone())),
            )
            .into_any_element()
    })
    .keywords(LIST_FIND)
}

/// 删除确认面板。标题 / 正文都是**一档一条整句**，别拿「通用问句 + 对象名」拼。
fn delete_panel(entry: Entry, cx: &App) -> AnyElement {
    let at = when(cx, entry.created_at());
    let body = tf(
        cx,
        "backup.delete.body",
        &[("when", &at), ("size", &human_bytes(cx, entry.bytes))],
    );
    v_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(tr(cx, "backup.delete.title")),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(body),
        )
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .child(
                    Button::new("backup-delete-cancel")
                        .ghost()
                        .label(tr(cx, "shelf.cancel").to_string())
                        .on_click(|_, _, cx| backup::answer(cx, false)),
                )
                .child(
                    Button::new("backup-delete-go")
                        .danger()
                        .label(tr(cx, "backup.delete.action").to_string())
                        .on_click(|_, _, cx| backup::answer(cx, true)),
                ),
        )
        .into_any_element()
}

/// 「位置」那一组：备份目录与一颗「打开所在文件夹」。
fn location_rows(cx: &App) -> Vec<Row> {
    let path = folio_core::infra::paths::backups_dir();
    let title = tr(cx, "backup.folder");
    vec![
        Row::custom_titled(title, move |_, cx| {
            let path = path.clone();
            h_flex()
                .w_full()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(muted_label(path.display().to_string(), cx)),
                )
                .child(open_folder_button(ElementId::from("backup-open"), path, cx))
                .into_any_element()
        })
        .keywords(LIST_FIND),
    ]
}

/// Unix 秒 → 本地 `YYYY-MM-DD HH:MM`。时区偏移由宿主给，core 不读环境。
fn when(_cx: &App, at: i64) -> String {
    when_at(at, crate::ui::host::local_offset_secs())
}

/// 同 [`when`]，偏移由调用方给 —— 纯函数，测试按 UTC 钉死。
fn when_at(at: i64, tz_offset_secs: i64) -> String {
    let day = local_day(at, tz_offset_secs);
    let date = civil_from_days(day);
    let secs = (at + tz_offset_secs).rem_euclid(DAY);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        date.year,
        date.month,
        date.day,
        secs / 3_600,
        secs % 3_600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 时间那格按本地偏移写，**不是 UTC**：东八区的 1970-01-01 08:00 是当天早上，
    /// 不是前一天半夜。
    #[test]
    fn the_stamp_follows_the_offset_it_is_given() {
        assert_eq!(when_at(0, 0), "1970-01-01 00:00");
        assert_eq!(when_at(0, 8 * 3_600), "1970-01-01 08:00");
        assert_eq!(when_at(1_800, -3_600), "1969-12-31 23:30");
    }
}
