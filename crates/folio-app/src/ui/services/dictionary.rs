//! 本地 StarDict 词典服务：导入、删除、查词的界面接线。
//!
//! **列表的真相源是配置**（`Config::dictionary.installed`），不是磁盘扫描：导入完成时
//! 写一条，删除时摘一条。这样设置页和阅读器的动作条读的是同一份数，测试也不会因为
//! 开发机上恰好放了几本词典而多出一行。
//!
//! 文件活（复制 / 删除）走常驻作业池的 `Cleanup` 档，不在界面线程上搬几十 MB；
//! 查词走 `Interactive` 档，结果原样交给会话。盘上那一半（导入的原子拷贝、id 消毒、
//! 查词与进程级缓存）住在 `folio_core::dict::library`，这里只留状态与调度。

use std::collections::BTreeSet;
use std::path::PathBuf;

pub use folio_core::dict::library::{Hit, Source, lookup_in};
use folio_core::dict::library::{ImportedDict, clear_cache, import_folder, remove_folder};
use folio_core::{DictionaryEntry, Priority};
use gpui_kit::{App, AppContext as _, Context, Entity, Global};

use crate::ui::services::config::ConfigHolder;
use crate::ui::services::library::{self, Pick};
use crate::ui::services::resident;

/// 导入 / 删除那一趟的状态。设置页 observe 它。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Importing,
    ImportFailed(String),
    DeleteFailed(String),
}

pub struct DictionaryState {
    pub status: Status,
}

pub struct DictionaryHolder(pub Entity<DictionaryState>);

impl Global for DictionaryHolder {}

/// 取状态实体，没有就建。与 `library::access` 同一形状：设置页在构造时手里就是
/// `&mut App`，不必等外壳先装。
pub fn entity(cx: &mut App) -> Entity<DictionaryState> {
    if let Some(holder) = cx.try_global::<DictionaryHolder>() {
        return holder.0.clone();
    }
    let entity = cx.new(|_| DictionaryState {
        status: Status::default(),
    });
    cx.set_global(DictionaryHolder(entity.clone()));
    entity
}

/// 当前状态；没装实体时是 `Idle`（纯视图测试不建作业）。
pub fn status(cx: &App) -> Status {
    cx.try_global::<DictionaryHolder>()
        .map(|holder| holder.0.read(cx).status.clone())
        .unwrap_or_default()
}

/// 已导入的词典，按导入顺序。
pub fn entries(cx: &App) -> Vec<DictionaryEntry> {
    ConfigHolder::dictionary(cx).installed
}

/// 这一台机器现在有没有能查的词典。动作条每帧问一次，只读配置。
pub fn available(cx: &App) -> bool {
    let config = ConfigHolder::dictionary(cx);
    config
        .installed
        .iter()
        .any(|entry| config.is_enabled(&entry.id))
}

/// 拼出这一趟要查的目录列表。
pub fn enabled_sources(cx: &App) -> Vec<Source> {
    let config = ConfigHolder::dictionary(cx);
    let root = config.root();
    config
        .installed
        .iter()
        .filter(|entry| config.is_enabled(&entry.id))
        .map(|entry| Source {
            id: entry.id.clone(),
            name: entry.name.clone(),
            dir: root.join(&entry.id),
        })
        .collect()
}

/// 开系统文件夹选择器，挑中了就导入。
///
/// Android 上「没给所有文件访问」那一问由 `choose_dir` 统一摆；设置页按
/// [`Pick::Dictionary`] 认出自己，并把 `access_question` 摆回那一格。
pub fn import_from_picker(cx: &mut App) {
    library::choose_dir(cx, Pick::Dictionary, |cx, dir| {
        import(cx, dir.to_path_buf());
    })
    .detach();
}

/// 导入一个文件夹（里面可以有一本或多本 StarDict 词典）。
pub fn import(cx: &mut App, src: PathBuf) {
    let config = ConfigHolder::dictionary(cx);
    let root = config.root();
    let taken: BTreeSet<String> = config
        .installed
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    let state = entity(cx);
    state.update(cx, |state, cx| {
        state.status = Status::Importing;
        cx.notify();
        let (tx, rx) = async_channel::bounded(1);
        resident::pool().submit_with(Priority::Cleanup, move |_| {
            let result = import_folder(&src, &root, &taken).map(|imported| (root, imported));
            let _ = tx.send_blocking(result);
            Ok(())
        });
        cx.spawn(async move |this, cx| {
            let Ok(result) = rx.recv().await else {
                return;
            };
            let _ = this.update(cx, |state, cx| state.finish_import(result, cx));
        })
        .detach();
    });
}

/// 删掉一本（磁盘目录与配置里的那一条一起）。
pub fn remove(cx: &mut App, id: String) {
    let config = ConfigHolder::dictionary(cx);
    let root = config.root();
    let state = entity(cx);
    state.update(cx, |state, cx| {
        state.status = Status::Importing;
        cx.notify();
        let (tx, rx) = async_channel::bounded(1);
        let job_id = id.clone();
        resident::pool().submit_with(Priority::Cleanup, move |_| {
            let result = remove_folder(&root, &job_id);
            let _ = tx.send_blocking(result);
            Ok(())
        });
        cx.spawn(async move |this, cx| {
            let Ok(result) = rx.recv().await else {
                return;
            };
            let _ = this.update(cx, |state, cx| state.finish_remove(id, result, cx));
        })
        .detach();
    });
}

/// 打开 / 关掉一本。写配置，不动磁盘。
pub fn set_enabled(cx: &mut App, id: &str, enabled: bool) {
    let id = id.to_string();
    ConfigHolder::edit_dictionary(cx, |dictionary| {
        dictionary.disabled.retain(|disabled| disabled != &id);
        if !enabled {
            dictionary.disabled.push(id.clone());
        }
    });
    if let Some(holder) = cx.try_global::<DictionaryHolder>() {
        let state = holder.0.clone();
        state.update(cx, |_, cx| cx.notify());
    }
}

impl DictionaryState {
    fn finish_import(
        &mut self,
        result: folio_core::Result<(PathBuf, Vec<ImportedDict>)>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok((root, imported)) => {
                if imported.is_empty() {
                    self.status = Status::ImportFailed("这个文件夹里没有 .ifo".to_string());
                } else {
                    ConfigHolder::edit_dictionary(cx, |dictionary| {
                        if dictionary.dir.is_empty() {
                            dictionary.dir = root.display().to_string();
                        }
                        dictionary.installed.extend(imported.into_iter().map(|one| {
                            DictionaryEntry {
                                id: one.id,
                                name: one.name,
                                words: one.words,
                            }
                        }));
                    });
                    self.status = Status::Idle;
                }
            }
            Err(err) => self.status = Status::ImportFailed(err.to_string()),
        }
        cx.notify();
    }

    fn finish_remove(
        &mut self,
        id: String,
        result: folio_core::Result<()>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(()) => {
                ConfigHolder::edit_dictionary(cx, |dictionary| {
                    dictionary.installed.retain(|entry| entry.id != id);
                    dictionary.disabled.retain(|disabled| disabled != &id);
                });
                clear_cache();
                self.status = Status::Idle;
            }
            Err(err) => self.status = Status::DeleteFailed(err.to_string()),
        }
        cx.notify();
    }
}
