//! 书架多选 → 网盘：批量上传的界面接线。
//!
//! 状态挂在实体上而不是 `Global` 平铺：后台跑完要 `notify`，否则架面那张浮层会
//! 永远停在「正在上传」。一次只跑一批，第二批在第一批完成前不叠上去。
//! 上传本身（建目录、按内容哈希逐本传、单本失败不拦后面）住在
//! `folio_core::net::cloud::upload`，这里只留状态与调度。

pub use folio_core::net::cloud::upload::{Failure, Progress};

use folio_core::UploadBook;
use gpui_kit::{App, AppContext as _, Entity, Global};

use crate::ui::services::{app_store, sync};
use folio_core::net::cloud::upload::upload_batch;

#[derive(Default)]
pub struct UploadState {
    progress: Option<Progress>,
}

impl UploadState {
    pub fn progress(&self) -> Option<&Progress> {
        self.progress.as_ref()
    }

    pub fn busy(&self) -> bool {
        matches!(self.progress, Some(Progress::Running { .. }))
    }
}

pub struct UploadHolder(pub Entity<UploadState>);

impl Global for UploadHolder {}

pub fn handle(cx: &App) -> Option<Entity<UploadState>> {
    cx.try_global::<UploadHolder>()
        .map(|holder| holder.0.clone())
}

fn entity(cx: &mut App) -> Entity<UploadState> {
    if let Some(entity) = handle(cx) {
        return entity;
    }
    let entity = cx.new(|_| UploadState::default());
    cx.set_global(UploadHolder(entity.clone()));
    entity
}

/// 收起那张浮层。上传本身不受影响 —— 读者关掉的是「这一批的报表」。
pub fn dismiss(cx: &mut App) {
    entity(cx).update(cx, |state, cx| {
        state.progress = None;
        cx.notify();
    });
}

fn settle(cx: &mut App, progress: Progress) {
    entity(cx).update(cx, |state, cx| {
        state.progress = Some(progress);
        cx.notify();
    });
}

/// 把这一批书传上去。调用方给的是书架上的 id；没有哈希的会被数进 `skipped`。
pub fn start(cx: &mut App, ids: Vec<i64>) {
    let state = entity(cx);
    if state.read(cx).busy() {
        return;
    }
    let Some(store) = app_store::handle(cx) else {
        settle(cx, Progress::Failed(Failure::Store));
        return;
    };
    let books: Vec<UploadBook> = match store.upload_books(&ids) {
        Ok(books) => books,
        Err(err) => {
            tracing::warn!("upload: 读书目失败：{err}");
            settle(cx, Progress::Failed(Failure::Store));
            return;
        }
    };
    let skipped = ids.len().saturating_sub(books.len());
    if books.is_empty() {
        settle(
            cx,
            Progress::Done {
                uploaded: 0,
                failed: 0,
                skipped,
            },
        );
        return;
    }
    let Some((remote, dir)) = sync::remote_store(cx) else {
        settle(cx, Progress::Failed(Failure::Config));
        return;
    };
    let total = books.len();
    state.update(cx, |state, cx| {
        state.progress = Some(Progress::Running { total });
        cx.notify();
    });
    let work =
        cx.background_spawn(async move { upload_batch(remote.as_ref(), &dir, books, skipped) });
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let progress = work.await;
        let _ = weak.update(cx, |state, cx| {
            state.progress = Some(progress);
            cx.notify();
        });
    })
    .detach();
}
