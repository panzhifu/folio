//! 读者自装字体的服务：下载、导入、删除。
//!
//! 已装清单的真相源是**配置**（`Config::fonts.installed`），不是磁盘扫描 —— 与词典
//! 同一条：设置页、boot 重注册（`platform::user_fonts`）与字体选择器读的是同一份，
//! 测试也不会因为开发机上恰好放了什么而多出一行。
//!
//! 文件活在常驻作业池上（下载是几十 MB 的网络活、导入是整份拷贝，都不该占界面线程），
//! 下载进度经 `async_channel` 推回界面；字体字节进 `TextSystem` 的注册也在作业线程上
//! 做 —— `system_fonts` 开机那一趟就是这么走的，`add_fonts_with_families` 本来就是
//! `&self`。注册成功才有配置那条：注册失败的份连目录一起撤掉，盘上不留下认得一半的
//! 一份，列表里也不出现一行排不出来的字。
//!
//! 一趟一件事：同一时刻只有一份字体在装（下载或导入互斥，也和删除互斥）。状态是
//! 单槽的，两趟并行会让结束事件写坏对方的状态；清单页上「装着的那份在动」时别的
//! 下载钮禁用，正好是同一句话的两面。

use std::path::PathBuf;
use std::sync::Arc;

use folio_core::fonts::FontsError;
use folio_core::net::remote::Cancel;
use folio_core::{FontEntry, Priority};
use gpui_kit::{App, AppContext as _, Context, Entity, Global, WeakEntity};

use crate::ui::features::reader::note_external_registration;
use crate::ui::platform::user_fonts;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::resident;
use crate::ui::services::update;
use folio_core::text as font_primary;

/// 一趟装 / 删的状态。设置页 observe 它；`Downloading` 带着 id，清单页好知道
/// 哪张卡在动（同一时刻只有一趟，见模块注释）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    /// 下载中：`received / total` 是字节进度，total 来自清单里实测过的那份。
    Downloading {
        id: String,
        received: u64,
        total: u64,
    },
    /// 正在导入本地文件。
    Importing,
    /// 上一趟失败了，带着给人看的那一句。下一趟开始时清掉。
    Failed(String),
}

pub struct UserFontsState {
    pub status: Status,
    /// 正在跑的那一趟的取消旗标；不在下载时是 `None`。界面上的「取消」触发它，
    /// 传输在块与块之间看见（`net::remote` 的契约）。
    pub cancel: Option<Cancel>,
}

pub struct UserFontsHolder(pub Entity<UserFontsState>);

impl Global for UserFontsHolder {}

/// 取状态实体，没有就建。与 `dictionary::entity` 同一形状：设置页在构造时手里就是
/// `&mut App`，不必等外壳先装。
pub fn entity(cx: &mut App) -> Entity<UserFontsState> {
    if let Some(holder) = cx.try_global::<UserFontsHolder>() {
        return holder.0.clone();
    }
    let entity = cx.new(|_| UserFontsState {
        status: Status::default(),
        cancel: None,
    });
    cx.set_global(UserFontsHolder(entity.clone()));
    entity
}

/// 当前状态；没装实体时是 `Idle`（纯视图测试不建作业）。
pub fn status(cx: &App) -> Status {
    cx.try_global::<UserFontsHolder>()
        .map(|holder| holder.0.read(cx).status.clone())
        .unwrap_or_default()
}

/// 已装的字体，按安装顺序。
pub fn entries(cx: &App) -> Vec<FontEntry> {
    ConfigHolder::fonts(cx).installed
}

/// 清单里的一款（按 id）。清单页从它取卡片的全部内容。
pub fn manifest_item(id: &str) -> Option<folio_core::fonts::FontItem> {
    folio_core::fonts::manifest().item(id).cloned()
}

/// 下载清单里的一款字体。装完（下载 + 注册进文本系统）才写配置。
pub fn download(cx: &mut App, item_id: &str) {
    let Some(item) = manifest_item(item_id) else {
        return;
    };
    if ConfigHolder::fonts(cx).has(item_id) {
        return;
    }
    let state = entity(cx);
    if busy(&state.read(cx).status) {
        return;
    }
    let cancel = Cancel::new();
    let texts: Arc<gpui_kit::TextSystem> = cx.text_system().clone();
    state.update(cx, |state, cx| {
        state.status = Status::Downloading {
            id: item.id.clone(),
            received: 0,
            total: item.size,
        };
        state.cancel = Some(cancel.clone());
        cx.notify();
        let (tx, rx) = async_channel::unbounded::<Message>();
        let root = folio_core::fonts_dir();
        let cancel = cancel.clone();
        resident::pool().submit_with(Priority::Prewarm, move |_| {
            let transport = update::transport();
            let progress_tx = tx.clone();
            let outcome = match folio_core::fonts::download_item(
                &item,
                &*transport,
                &root,
                &cancel,
                move |received| {
                    let _ = progress_tx.send_blocking(Message::Progress(received));
                },
            ) {
                Ok(path) => install_outcome(&path, &texts),
                Err(err) => Err(err),
            };
            let _ = tx.send_blocking(Message::Done(outcome));
            Ok(())
        });
        cx.spawn(async move |this: WeakEntity<UserFontsState>, cx| {
            loop {
                let Ok(message) = rx.recv().await else {
                    return;
                };
                match message {
                    Message::Progress(received) => {
                        let _ = this.update(cx, |state, cx| {
                            if let Status::Downloading { received: slot, .. } = &mut state.status {
                                *slot = received;
                            }
                            cx.notify();
                        });
                    }
                    Message::Done(outcome) => {
                        let _ = this.update(cx, |state, cx| state.finish(outcome, cx));
                        return;
                    }
                }
            }
        })
        .detach();
    });
}

/// 开文件选择器挑字体文件，挑中了就导入。多选可以；一个都没选就安静回来。
///
/// Android 上「没给所有文件访问」那一问由 `library` 的选择器统一摆（与词典同一条）。
pub fn import_from_picker(cx: &mut App) {
    let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
        files: true,
        directories: false,
        multiple: true,
        prompt: None,
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = receiver.await else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        cx.update(|cx| import(cx, paths));
    })
    .detach();
}

/// 导入本地字体文件（ttf / otf / ttc / woff / woff2）。全部拷完、注册完才写配置；
/// 有注册失败的份，那一份撤掉并进失败句，其余照常入库。
pub fn import(cx: &mut App, paths: Vec<PathBuf>) {
    let state = entity(cx);
    if busy(&state.read(cx).status) {
        return;
    }
    let taken: std::collections::BTreeSet<String> = ConfigHolder::fonts(cx)
        .installed
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    let texts: Arc<gpui_kit::TextSystem> = cx.text_system().clone();
    state.update(cx, |state, cx| {
        state.status = Status::Importing;
        state.cancel = None;
        cx.notify();
        let (tx, rx) = async_channel::bounded(1);
        let root = folio_core::fonts_dir();
        resident::pool().submit_with(Priority::Cleanup, move |_| {
            let mut taken = taken;
            let mut installed = Vec::new();
            let mut failures = Vec::new();
            for path in &paths {
                let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    failures.push(format!("{}：文件名不是合法 UTF-8", path.display()));
                    continue;
                };
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    failures.push(format!("{}：文件名不是合法 UTF-8", path.display()));
                    continue;
                };
                let id = folio_core::fonts::unique_id(
                    &folio_core::fonts::dir_id(stem),
                    &mut taken,
                    &root,
                );
                let landed = match folio_core::fonts::install_copy(
                    path,
                    &root,
                    &id,
                    &folio_core::fonts::wash_file_name(name),
                ) {
                    Ok(landed) => landed,
                    Err(err) => {
                        failures.push(err.to_string());
                        continue;
                    }
                };
                match install_outcome(&landed, &texts) {
                    Ok(entry) => installed.push(entry),
                    Err(err) => failures.push(err.to_string()),
                }
            }
            let _ = tx.send_blocking(ImportOutcome {
                installed,
                failures,
            });
            Ok(())
        });
        cx.spawn(async move |this, cx| {
            let Ok(outcome) = rx.recv().await else {
                return;
            };
            let _ = this.update(cx, |state, cx| state.finish_import(outcome, cx));
        })
        .detach();
    });
}

/// 删掉一份：磁盘目录与配置里那一条一起。删过的家族进退休表 —— gpui 的注册撤不
/// 回来（字节驻留到重启），但选择器从这一刻起就看不见它。
pub fn remove(cx: &mut App, id: String) {
    let state = entity(cx);
    if busy(&state.read(cx).status) {
        return;
    }
    let family = ConfigHolder::fonts(cx)
        .installed
        .iter()
        .find(|entry| entry.id == id)
        .map(|entry| entry.family.clone())
        .unwrap_or_default();
    state.update(cx, |state, cx| {
        state.status = Status::Importing;
        cx.notify();
        let (tx, rx) = async_channel::bounded(1);
        let root = folio_core::fonts_dir();
        let job_id = id.clone();
        resident::pool().submit_with(Priority::Cleanup, move |_| {
            let result = folio_core::fonts::remove(&root, &job_id);
            let _ = tx.send_blocking(result);
            Ok(())
        });
        cx.spawn(async move |this, cx| {
            let Ok(result) = rx.recv().await else {
                return;
            };
            let _ = this.update(cx, |state, cx| state.finish_remove(&id, family, result, cx));
        })
        .detach();
    });
}

/// 取消正在跑的那一趟下载。取消不是失败：状态回 `Idle`，半份由 core 清掉。
pub fn cancel(cx: &mut App) {
    let state = entity(cx);
    state.update(cx, |state, _| {
        if let Some(cancel) = &state.cancel {
            cancel.trigger();
        }
    });
}

fn busy(status: &Status) -> bool {
    matches!(status, Status::Downloading { .. } | Status::Importing)
}

/// 一份落好盘的字体 → 配置条目。注册（字节进文本系统）也在这半程里：
/// 报不出家族名的份按「装失败」处理，目录由调用方收走。
fn install_outcome(
    path: &std::path::Path,
    texts: &gpui_kit::TextSystem,
) -> Result<FontEntry, FontsError> {
    let families = user_fonts::register_file(texts, path).ok_or_else(|| {
        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(path));
        folio_core::fonts::FontsError::Font {
            path: path.to_path_buf(),
            why: "这份字体解不出来（文件可能是坏的）".to_string(),
            source: None,
        }
    })?;
    let id = path
        .parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let file = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    Ok(FontEntry {
        id,
        family: families.first().cloned().unwrap_or_default(),
        file,
        size_bytes: size,
    })
}

/// 作业线程递回界面的消息：下载走多档进度，结束那档带着结果。
enum Message {
    Progress(u64),
    Done(Result<FontEntry, FontsError>),
}

struct ImportOutcome {
    installed: Vec<FontEntry>,
    failures: Vec<String>,
}

impl UserFontsState {
    fn finish(&mut self, outcome: Result<FontEntry, FontsError>, cx: &mut Context<Self>) {
        self.cancel = None;
        match outcome {
            Ok(entry) => {
                ConfigHolder::edit_fonts(cx, |fonts| {
                    if !fonts.has(&entry.id) {
                        fonts.installed.push(entry);
                    }
                });
                self.status = Status::Idle;
                cx.refresh_windows();
            }
            // 取消不是失败：读者的话就是「不装了」，状态回 Idle，失败句不给。
            // 按类型分档（FontsError::Cancelled），别匹配文案。
            Err(FontsError::Cancelled) => {
                self.status = Status::Idle;
            }
            Err(err) => self.status = Status::Failed(err.to_string()),
        }
        cx.notify();
    }

    fn finish_import(&mut self, outcome: ImportOutcome, cx: &mut Context<Self>) {
        ConfigHolder::edit_fonts(cx, |fonts| {
            for entry in outcome.installed {
                if !fonts.has(&entry.id) {
                    fonts.installed.push(entry);
                }
            }
        });
        self.status = match outcome.failures.first() {
            Some(first) => Status::Failed(first.clone()),
            None => Status::Idle,
        };
        cx.refresh_windows();
        cx.notify();
    }

    fn finish_remove(
        &mut self,
        id: &str,
        family: String,
        result: Result<(), FontsError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(()) => {
                ConfigHolder::edit_fonts(cx, |fonts| {
                    fonts.installed.retain(|entry| entry.id != id);
                });
                if !family.is_empty() {
                    font_primary::retire_family(&family);
                }
                // 选择器按版本号对账才重取名单；退休不注册新脸，版本要自己推。
                font_primary::bump_families_revision();
                note_external_registration();
                self.status = Status::Idle;
                cx.refresh_windows();
            }
            Err(err) => self.status = Status::Failed(err.to_string()),
        }
        cx.notify();
    }
}
