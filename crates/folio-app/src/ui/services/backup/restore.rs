//! 备份恢复的预览、凭据选择与后台准备，状态跟随设置页已有的 BackupState。

use std::path::{Path, PathBuf};

use folio_core::backup::{
    RestoreActivation, RestoreLimits, RestorePreview, Restored, preview_restore,
};
use folio_core::{Priority, TaskEvent};
use gpui_kit::{App, AppContext as _, Context, SharedString};

use super::{BackupState, entity};

#[derive(Default)]
pub struct RestoreState {
    archive: Option<PathBuf>,
    preview: Option<RestorePreview>,
    include_credentials: bool,
    busy: bool,
    prepared: Option<Restored>,
    failure: Option<SharedString>,
}

impl RestoreState {
    pub fn is_busy(&self) -> bool {
        self.busy
    }
    pub fn preview(&self) -> Option<&RestorePreview> {
        self.preview.as_ref()
    }
    pub fn archive(&self) -> Option<&Path> {
        self.archive.as_deref()
    }
    pub fn includes_credentials(&self) -> bool {
        self.include_credentials
    }
    pub fn prepared(&self) -> Option<&Restored> {
        self.prepared.as_ref()
    }
    pub fn failure(&self) -> Option<&SharedString> {
        self.failure.as_ref()
    }
}

enum Outcome {
    Preview(RestorePreview),
    Prepared(Restored),
}

impl BackupState {
    pub fn restore(&self) -> &RestoreState {
        &self.restore
    }

    fn inspect_restore(&mut self, archive: PathBuf, cx: &mut Context<Self>) {
        if self.busy() || self.restore.prepared.is_some() {
            return;
        }
        self.restore.archive = Some(archive.clone());
        self.restore.preview = None;
        self.restore.include_credentials = false;
        self.pending_delete = None;
        self.run_restore(
            move || {
                Ok(Outcome::Preview(preview_restore(
                    &archive,
                    RestoreLimits::default(),
                )?))
            },
            cx,
        );
    }

    fn run_restore(
        &mut self,
        work: impl FnOnce() -> folio_core::Result<Outcome> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.restore.busy = true;
        self.restore.failure = None;
        let handle = super::resident::pool()
            .submit_with(Priority::Interactive, move |_| work().map_err(Into::into));
        let (tx, events) = async_channel::unbounded();
        cx.background_spawn(async move {
            for event in handle.events.iter() {
                if tx.send_blocking(event).is_err() {
                    break;
                }
            }
        })
        .detach();
        self._restore_pump = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |this, cx| match event {
                        TaskEvent::Finished(result) => {
                            this.restore.busy = false;
                            match result {
                                Ok(Outcome::Preview(preview)) => {
                                    this.restore.preview = Some(preview)
                                }
                                Ok(Outcome::Prepared(restored)) => {
                                    this.restore.prepared = Some(restored)
                                }
                                Err(error) => this.restore.failure = Some(error.to_string().into()),
                            }
                            cx.notify();
                        }
                        TaskEvent::Cancelled => {
                            this.restore.busy = false;
                            cx.notify();
                        }
                        _ => {}
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
        cx.notify();
    }
}

pub fn inspect(cx: &mut App, path: PathBuf) {
    entity(cx).update(cx, |state, cx| state.inspect_restore(path, cx));
}

pub fn pick(cx: &mut App) {
    let state = entity(cx);
    if state.read(cx).busy() || state.read(cx).restore.prepared.is_some() {
        return;
    }
    state.update(cx, |state, cx| {
        state.restore.busy = true;
        state.restore.failure = None;
        cx.notify();
    });
    let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: None,
    });
    cx.spawn(async move |cx| {
        let result = receiver.await;
        state.update(cx, |state, cx| {
            state.restore.busy = false;
            match result {
                Ok(Ok(Some(mut paths))) => {
                    if let Some(path) = paths.pop() {
                        state.inspect_restore(path, cx);
                    }
                }
                Ok(Err(error)) => state.restore.failure = Some(error.to_string().into()),
                _ => {}
            }
            cx.notify();
        });
    })
    .detach();
}

pub fn include_credentials(cx: &mut App, include: bool) {
    entity(cx).update(cx, |state, cx| {
        if !state.busy() && state.restore.prepared.is_none() {
            state.restore.include_credentials = include;
            cx.notify();
        }
    });
}

pub fn dismiss(cx: &mut App) {
    entity(cx).update(cx, |state, cx| {
        if !state.busy() && state.restore.prepared.is_none() {
            state.restore = RestoreState::default();
            cx.notify();
        }
    });
}

pub fn prepare(cx: &mut App) {
    prepare_at(cx, RestoreActivation::from_startup());
}

fn prepare_at(cx: &mut App, activation: RestoreActivation) {
    entity(cx).update(cx, |state, cx| {
        if state.busy() || state.restore.preview.is_none() || state.restore.prepared.is_some() {
            return;
        }
        let Some(archive) = state.restore.archive.clone() else {
            return;
        };
        let credentials = state.restore.include_credentials;
        state.run_restore(
            move || {
                Ok(Outcome::Prepared(
                    activation.prepare(&archive, credentials)?,
                ))
            },
            cx,
        );
    });
}

#[cfg(test)]
mod tests {
    use crate::ui::services::backup::entity;
    use crate::ui::services::backup::restore::{dismiss, include_credentials, inspect, prepare_at};
    use folio_core::backup::{Options, RestoreActivation, Roots, create};
    use folio_core::{Config, Store};
    use gpui_kit::TestAppContext;

    fn wait(cx: &mut TestAppContext) {
        for _ in 0..400 {
            cx.background_executor.run_until_parked();
            if cx.update(|cx| !entity(cx).read(cx).busy()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("restore task did not complete");
    }

    #[gpui_kit::test]
    fn restore_preview_choice_preparation_and_failure_preserve_the_active_store(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let temp = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let mut config = Config::default();
        config.sync.password = "secret".into();
        let roots = Roots {
            backups: temp.path().join("backups"),
            data: temp.path().into(),
            library: None,
            dicts: None,
            fonts: None,
            themes: None,
        };
        let archive = create(&store, &config, &roots, Options::default(), &mut |_, _| {}).unwrap();
        cx.update(|cx| inspect(cx, archive.path.clone()));
        wait(cx);
        cx.update(|cx| {
            let state = entity(cx);
            assert!(
                state
                    .read(cx)
                    .restore()
                    .preview()
                    .unwrap()
                    .contains_credentials
            );
            assert!(!state.read(cx).restore().includes_credentials());
            include_credentials(cx, true);
            assert!(state.read(cx).restore().includes_credentials());
            dismiss(cx);
            assert!(state.read(cx).restore().preview().is_none());
            inspect(cx, temp.path().join("missing.zip"));
        });
        wait(cx);
        cx.update(|cx| {
            assert!(entity(cx).read(cx).restore().failure().is_some());
            inspect(cx, archive.path);
        });
        wait(cx);
        let activation =
            RestoreActivation::new(temp.path().join("config"), temp.path().join("data"));
        cx.update(|cx| prepare_at(cx, activation.clone()));
        wait(cx);
        cx.update(|cx| {
            assert!(entity(cx).read(cx).restore().prepared().is_some());
            assert!(activation.selected_root().unwrap().is_some());
        });
        assert!(store.list_books().unwrap().is_empty());
    }
}
