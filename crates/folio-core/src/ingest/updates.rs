//! 原件更新的服务入口；捕获与导入共用实现。
#[path = "updates/reconciler.rs"]
mod reconciler;
pub use reconciler::SourceReconciler;
pub fn open_revision_document(
    snapshot: crate::store::RevisionSnapshot,
    passphrase: Option<folio_format::Passphrase>,
) -> crate::Result<Box<dyn folio_format::document::Document>> {
    crate::content::version_document::open_revision_document(
        snapshot.revision,
        snapshot.meta,
        snapshot.content,
        passphrase,
    )
}
pub use super::import::prune_revision_history;
pub use super::import::refresh_anchor_projections;

pub use super::import::{
    SourceUpdate, SourceUpdateState, prepare_legacy_revision, reconcile_sources_once,
    relink_source, update_from_path,
};
pub use super::import::{reconcile_source_ids_with_access, update_from_link};
