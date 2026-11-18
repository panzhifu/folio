//! 关联原件的后台对账。服务只发布数据，页面决定何时切换阅读版本。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use folio_core::ingest::external_source::{ExternalSourceAccess, SourceLink};
use folio_core::ingest::updates::{
    SourceReconciler, SourceUpdateState, prepare_legacy_revision, update_from_link,
};
use folio_core::store::{BookSource, RevisionFence};
use folio_core::{Cancel, Priority, Store};
use gpui_kit::{
    App, AppContext as _, AppLifecyclePhase, Context, Entity, Global, Subscription, Task,
};

use super::{app_store, config::ConfigHolder, library, resident};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(15);
mod jobs;
mod notifications;

#[derive(Clone, Copy, Default)]
pub struct UpdateProgress {
    pub percent: Option<u8>,
    pub cancelling: bool,
}

impl UpdateProgress {
    pub fn label(self, cx: &App) -> String {
        if self.cancelling {
            super::l10n::tr(cx, "source_update.cancelling").to_string()
        } else if let Some(percent) = self.percent {
            super::l10n::tf(cx, "source_update.progress", &[("percent", &percent)])
        } else {
            super::l10n::tr(cx, "source_update.working").to_string()
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct BookUpdate {
    pub sources: Vec<BookSource>,
    pub ready: Option<RevisionFence>,
    pub state: Option<SourceUpdateState>,
    pub message: Option<String>,
}

pub struct SourceUpdates {
    store: Arc<Store>,
    reconciler: Arc<Mutex<SourceReconciler>>,
    _access: Arc<dyn ExternalSourceAccess>,
    root: Option<PathBuf>,
    books: HashMap<i64, BookUpdate>,
    epoch: u64,
    busy: bool,
    operation_book: Option<i64>,
    percent: Option<u8>,
    last_message: Option<String>,
    cancel: Option<Cancel>,
    timer: Option<Task<()>>,
    pump: Option<Task<()>>,
    lifecycle: Option<Subscription>,
    record_events: Option<Task<()>>,
}

struct Holder(Entity<SourceUpdates>);
impl Global for Holder {}

impl Drop for SourceUpdates {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.trigger();
        }
    }
}

struct SourceChoice {
    book_id: i64,
    path: PathBuf,
    link: Option<SourceLink>,
    _temporary_input: Option<folio_core::ingest::import::PathsSource>,
}

impl SourceUpdates {
    pub fn book(&self, book_id: i64) -> BookUpdate {
        self.books.get(&book_id).cloned().unwrap_or_default()
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn progress(&self, book_id: i64) -> Option<UpdateProgress> {
        (self.busy && self.operation_book.is_none_or(|id| id == book_id)).then(|| UpdateProgress {
            percent: self.percent,
            cancelling: self.cancel.as_ref().is_some_and(Cancel::triggered),
        })
    }

    pub fn reader_progress(&self, book_id: i64) -> Option<UpdateProgress> {
        (self.operation_book == Some(book_id))
            .then(|| self.progress(book_id))
            .flatten()
    }

    pub fn last_message(&self) -> Option<&str> {
        self.last_message.as_deref()
    }

    fn run(&mut self, operation: Option<SourceChoice>, force: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(root) = self.root.clone() else {
            return;
        };
        let operation_book = operation.as_ref().map(|operation| operation.book_id);
        let store = self.store.clone();
        let reconciler = self.reconciler.clone();
        let priority = if operation.is_some() {
            Priority::Interactive
        } else {
            Priority::Cleanup
        };
        let handle = resident::pool().submit_with(priority, move |ctx| {
            let outcomes = match operation {
                Some(choice) => {
                    vec![update_from_link(
                        &store,
                        choice.book_id,
                        &choice.path,
                        choice.link,
                        &root,
                        ctx,
                    )?]
                }
                None => reconciler
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .reconcile(&store, &root, ctx, force),
            };
            let sources = store.sources_for_reconcile()?;
            Ok((outcomes, sources))
        });
        jobs::follow(
            self,
            handle,
            operation_book,
            move |this, result, cx| {
                let before = this.books.clone();
                match result {
                    Ok((outcomes, sources)) => {
                        for status in this.books.values_mut() {
                            status.sources.clear();
                        }
                        for source in sources {
                            this.books
                                .entry(source.book_id)
                                .or_default()
                                .sources
                                .push(source);
                        }
                        for outcome in outcomes {
                            let status = this.books.entry(outcome.book_id).or_default();
                            status.ready = if matches!(outcome.state, SourceUpdateState::Ready) {
                                outcome.revision
                            } else {
                                None
                            };
                            status.state = Some(outcome.state);
                            status.message = outcome.message;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "来源对账失败，保留当前阅读版本");
                        if let Some(book_id) = operation_book {
                            let state = this.books.entry(book_id).or_default();
                            state.state = Some(SourceUpdateState::Failed);
                            state.message = Some(error.to_string());
                        }
                    }
                }
                if before != this.books {
                    this.epoch = this.epoch.wrapping_add(1);
                }
                cx.notify();
            },
            cx,
        );
    }
}

/// 每次切换书库都替换实体；旧库任务不能向新库界面发布结果。
pub fn reset(store: Arc<Store>, cx: &mut App) {
    if let Some(previous) = handle(cx) {
        previous.update(cx, |this, _| {
            if let Some(cancel) = this.cancel.take() {
                cancel.trigger();
            }
            this.root = None;
            this.timer = None;
            this.pump = None;
            this.lifecycle = None;
            this.record_events = None;
            this.busy = false;
            this.percent = None;
            this.operation_book = None;
        });
    }
    let events = notifications::subscribe(&store);
    let access = crate::ui::platform::source_access();
    let entity = cx.new(|cx: &mut Context<SourceUpdates>| SourceUpdates {
        store,
        reconciler: Arc::new(Mutex::new(SourceReconciler::with_access(access.clone()))),
        _access: access,
        root: None,
        books: HashMap::new(),
        epoch: 0,
        busy: false,
        operation_book: None,
        percent: None,
        last_message: None,
        cancel: None,
        timer: None,
        pump: None,
        lifecycle: None,
        record_events: Some(cx.spawn(async move |this, cx| {
            while events.recv().await.is_ok() {
                if this
                    .update(cx, |this, cx| {
                        this.epoch = this.epoch.wrapping_add(1);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })),
    });
    cx.set_global(Holder(entity));
}

pub fn handle(cx: &App) -> Option<Entity<SourceUpdates>> {
    cx.try_global::<Holder>().map(|holder| holder.0.clone())
}

/// 平台装好配置、书库和作业池后启动；不在 render 中读取原件。
pub fn start(cx: &mut App) {
    let Some(entity) = handle(cx) else { return };
    let root = library::target_dir(&ConfigHolder::library(cx));
    entity.update(cx, |this, cx| {
        this.root = root;
        if this.lifecycle.is_none() {
            let weak = cx.weak_entity();
            this.lifecycle = Some(cx.on_app_lifecycle(move |phase, cx| {
                if phase == AppLifecyclePhase::Active {
                    let _ = weak.update(cx, |this, cx| this.run(None, false, cx));
                }
            }));
        }
        if this.timer.is_none() {
            this.timer = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(RECONCILE_INTERVAL).await;
                    if this
                        .update(cx, |this, cx| this.run(None, false, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
        this.run(None, false, cx);
    });
}

pub fn poll_now(cx: &mut App) {
    if let Some(entity) = handle(cx) {
        entity.update(cx, |this, cx| this.run(None, false, cx));
    }
}

pub fn check_now(cx: &mut App) {
    if let Some(entity) = handle(cx) {
        entity.update(cx, |this, cx| this.run(None, true, cx));
    }
}

pub fn cancel(cx: &mut App) {
    if let Some(entity) = handle(cx) {
        entity.update(cx, |this, cx| {
            if let Some(cancel) = &this.cancel {
                cancel.trigger();
                cx.notify();
            }
        });
    }
}

#[cfg(test)]
pub(crate) fn track_job_for_test<R: Send + 'static>(
    handle: folio_core::TaskHandle<R>,
    book: i64,
    cx: &mut App,
) {
    if let Some(entity) = self::handle(cx) {
        entity.update(cx, |this, cx| {
            jobs::follow(this, handle, Some(book), |_, _, _| {}, cx)
        });
    }
}

pub fn adopted(book_id: i64, cx: &mut App) {
    if let Some(entity) = handle(cx) {
        entity.update(cx, |this, cx| {
            let state = this.books.entry(book_id).or_default();
            state.ready = None;
            state.state = Some(SourceUpdateState::Adopted);
            state.message = None;
            this.epoch = this.epoch.wrapping_add(1);
            cx.notify();
        });
    }
}

pub fn signal_records_changed(store: &Arc<Store>) {
    notifications::signal(store);
}

pub fn records_changed(cx: &mut App) {
    if let Some(store) = app_store::handle(cx) {
        signal_records_changed(&store);
    }
}

pub fn pick_replacement(book_id: i64, cx: &mut App) {
    let Some(entity) = handle(cx) else { return };
    if entity.read(cx).is_busy() {
        return;
    }
    let temporary = crate::ui::host::picked_files_are_temporary(cx);
    let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: None,
    });
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(paths))) = receiver.await
            && let Some(path) = paths.into_iter().next()
        {
            entity.update(cx, |this, cx| {
                let link = crate::ui::platform::source_for_pick(&path)
                    .filter(|source| !source.display_name.trim().is_empty())
                    .map(|source| SourceLink {
                        kind: "android-document".into(),
                        locator: source.locator,
                    })
                    .or_else(|| {
                        (!temporary).then(|| SourceLink {
                            kind: "local".into(),
                            locator: path.to_string_lossy().into_owned(),
                        })
                    });
                // 选择器等待期间可能已有检查开始；即使run拒绝提交也回收本次临时输入。
                let temporary_input = temporary.then(|| {
                    folio_core::ingest::import::PathsSource::picked_temporary(vec![path.clone()])
                });
                this.run(
                    Some(SourceChoice {
                        book_id,
                        path,
                        link,
                        _temporary_input: temporary_input,
                    }),
                    true,
                    cx,
                )
            });
        }
    })
    .detach();
}

pub fn belongs_to_current_store(entity: &Entity<SourceUpdates>, cx: &App) -> bool {
    app_store::handle(cx).is_some_and(|store| Arc::ptr_eq(&store, &entity.read(cx).store))
}

pub fn verify_legacy(book_id: i64, cx: &mut App) {
    let Some(entity) = handle(cx) else { return };
    entity.update(cx, |this, cx| {
        let Some(root) = this
            .root
            .clone()
            .or_else(|| library::target_dir(&ConfigHolder::library(cx)))
        else {
            return;
        };
        if this.busy {
            return;
        }
        let store = this.store.clone();
        let job = resident::pool().submit_with(Priority::Interactive, move |ctx| {
            prepare_legacy_revision(&store, book_id, &root, ctx).map_err(Into::into)
        });
        jobs::follow(
            this,
            job,
            Some(book_id),
            move |this, result, cx| {
                let before = this.books.clone();
                match result {
                    Ok(outcome) => {
                        let state = this.books.entry(book_id).or_default();
                        state.ready = if outcome.state == SourceUpdateState::Ready {
                            outcome.revision
                        } else {
                            None
                        };
                        state.state = Some(outcome.state);
                        state.message = outcome.message;
                    }
                    Err(error) => {
                        let state = this.books.entry(book_id).or_default();
                        state.state = Some(SourceUpdateState::Failed);
                        state.message = Some(error.to_string());
                    }
                }
                if before != this.books {
                    this.epoch = this.epoch.wrapping_add(1);
                }
                cx.notify();
            },
            cx,
        );
    });
}

#[cfg(test)]
mod tests {
    use crate::ui::services::source_updates::{belongs_to_current_store, check_now, handle, start};
    use crate::ui::services::{app_store, config::ConfigHolder};
    use folio_core::infra::task::StageCtx;
    use folio_core::ingest::import::{PathsSource, import_from};
    use folio_core::{Config, Store};
    use gpui_kit::TestAppContext;

    fn wait(cx: &mut TestAppContext) {
        for _ in 0..600 {
            cx.background_executor.run_until_parked();
            if cx.update(|cx| !handle(cx).unwrap().read(cx).is_busy()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("source update did not finish");
    }

    #[gpui_kit::test]
    fn source_revision_background_adopts_unopened_books_and_waits_for_a_reader(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        cx.background_executor.allow_parking();
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original.md");
        let library = temp.path().join("library");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(&original, "# First\n\nOriginal text").unwrap();
        let store = cx.update(|cx| {
            let mut config = Config::default();
            config.library.dir = library.to_string_lossy().into_owned();
            ConfigHolder::install(config, cx);
            app_store::install(Store::in_memory().unwrap(), cx);
            app_store::handle(cx).unwrap()
        });
        let outcomes = import_from(
            &PathsSource::picked(vec![original.clone()]),
            &library,
            &store,
            &StageCtx::detached(),
        );
        assert!(outcomes[0].result.is_ok());
        let book_id = store.list_books().unwrap()[0].id;
        let initial = store.active_revision(book_id).unwrap().unwrap();
        cx.update(start);
        wait(cx);
        std::fs::write(&original, "# Second\n\nChanged text").unwrap();
        cx.update(check_now);
        wait(cx);
        let second = store.active_revision(book_id).unwrap().unwrap();
        assert_ne!(second.fence, initial.fence);
        let reader = store.acquire_revision(book_id).unwrap();
        std::fs::write(&original, "# Third\n\nMore changed text").unwrap();
        cx.update(check_now);
        wait(cx);
        assert_eq!(
            store.active_revision(book_id).unwrap().unwrap().fence,
            second.fence
        );
        let prepared = cx
            .update(|cx| handle(cx).unwrap().read(cx).book(book_id).ready)
            .unwrap();
        drop(reader);
        cx.update(check_now);
        wait(cx);
        assert_eq!(
            store.active_revision(book_id).unwrap().unwrap().fence,
            prepared
        );
        assert_eq!(store.list_books().unwrap().len(), 1);
        assert_eq!(
            std::fs::read_to_string(original).unwrap(),
            "# Third\n\nMore changed text"
        );
    }

    #[gpui_kit::test]
    fn source_revision_store_replacement_cancels_the_old_service_and_isolates_new_state(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            app_store::install(Store::in_memory().unwrap(), cx);
            let old = handle(cx).unwrap();
            old.update(cx, |state, _| {
                state.cancel = Some(folio_core::Cancel::new());
                state.busy = true;
            });
            let cancel = old.read(cx).cancel.clone().unwrap();
            app_store::install(Store::in_memory().unwrap(), cx);
            assert!(cancel.triggered());
            assert!(!belongs_to_current_store(&old, cx));
            assert!(belongs_to_current_store(&handle(cx).unwrap(), cx));
            assert!(handle(cx).unwrap().read(cx).books.is_empty());
            assert!(
                app_store::handle(cx)
                    .unwrap()
                    .list_books()
                    .unwrap()
                    .is_empty()
            );
        });
    }
}
