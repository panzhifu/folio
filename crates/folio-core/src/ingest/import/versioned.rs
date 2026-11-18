//! 首次导入与原件更新共用的捕获、解析和发布；Store 决定候选能否成为当前版。

use std::path::{Path, PathBuf};

use super::snapshot::{parse_capture, publish_capture};
use crate::content::format::ParseBudget;
use crate::error::{Error, IoResultExt as _, Result};
use crate::infra::identity;
use crate::infra::task::StageCtx;
use crate::store::{
    AdoptResult, BookRevision, BookSource, ImportRegistration, RevisionFence, RevisionReady, Store,
};

use super::capture::{Captured, capture_staged, issue};
use super::{Imported, Registered};
use crate::ingest::external_source::{ExternalSourceAccess, NoExternalSources, SourceLink};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceUpdateState {
    Unchanged,
    Adopted,
    Ready,
    Conflict,
    Unavailable,
    Failed,
}

#[derive(Debug, Clone)]
pub struct SourceUpdate {
    pub source_id: i64,
    pub book_id: i64,
    pub state: SourceUpdateState,
    pub revision: Option<RevisionFence>,
    pub message: Option<String>,
}

impl SourceUpdate {
    fn at(
        source_id: i64,
        book_id: i64,
        state: SourceUpdateState,
        revision: Option<RevisionFence>,
    ) -> Self {
        Self {
            source_id,
            book_id,
            state,
            revision,
            message: None,
        }
    }
}

/// 定时与文件事件都可触发。每轮校验主来源完整字节和资源，不以 mtime 相同跳过。
pub fn reconcile_sources_once(store: &Store, book_dir: &Path, ctx: &StageCtx) -> Vec<SourceUpdate> {
    reconcile_source_ids(store, book_dir, None, ctx)
}

/// 有界 monitor 只检查选定来源；None 用于用户显式全量检查与隔离测试。
pub fn reconcile_source_ids(
    store: &Store,
    book_dir: &Path,
    ids: Option<&[i64]>,
    ctx: &StageCtx,
) -> Vec<SourceUpdate> {
    reconcile_source_ids_with_access(store, book_dir, ids, ctx, &NoExternalSources)
}

pub fn reconcile_source_ids_with_access(
    store: &Store,
    book_dir: &Path,
    ids: Option<&[i64]>,
    ctx: &StageCtx,
    access: &dyn ExternalSourceAccess,
) -> Vec<SourceUpdate> {
    let sources = match store.sources_for_reconcile() {
        Ok(s) => s,
        Err(error) => {
            tracing::warn!("原件对账失败：{error}");
            return Vec::new();
        }
    };
    let mut results = Vec::new();
    for source in sources {
        if ids.is_some_and(|ids| !ids.contains(&source.id)) {
            continue;
        }
        if ctx.cancel().triggered() {
            break;
        }
        let result = prepare_source(store, &source, book_dir, ctx, access);
        results.push(match result {
            Ok(update) => update,
            Err(error) => {
                let missing = source.kind == "local" && !Path::new(&source.locator).is_file();
                let state = if missing {
                    SourceUpdateState::Unavailable
                } else {
                    SourceUpdateState::Failed
                };
                let _ = store.set_source_state_at(
                    source.id,
                    source.generation,
                    if missing { "missing" } else { "failed" },
                );
                let mut update = SourceUpdate::at(source.id, source.book_id, state, None);
                update.message = Some(error.to_string());
                update
            }
        });
    }
    results
}

fn source_locator(path: &Path, book_dir: &Path) -> Result<String> {
    let path = path.canonicalize().io_path(path)?;
    if let Ok(root) = book_dir.canonicalize()
        && path.starts_with(root)
    {
        return Err(issue(&path, "内部阅读版本不能登记为外部原件"));
    }
    Ok(path.to_string_lossy().into_owned())
}

fn captured_path(path: &Path, book_dir: &Path, ctx: &StageCtx) -> Result<Captured> {
    let staging = crate::infra::paths::staging_dir(book_dir);
    std::fs::create_dir_all(&staging).io_path(&staging)?;
    let name = path
        .file_name()
        .ok_or_else(|| issue(path, "原件没有文件名"))?
        .to_string_lossy();
    let part = staging.join(super::sources::staged_name(&name));
    let hash = super::capture::copy_hash(path, &part, ctx)?;
    capture_staged(&name, part, &staging, Some(path), Some(hash), ctx)
}

fn prepare_source(
    store: &Store,
    source: &BookSource,
    book_dir: &Path,
    ctx: &StageCtx,
    access: &dyn ExternalSourceAccess,
) -> Result<SourceUpdate> {
    let observed = source.clone();
    if observed.state == "suppressed" {
        return Ok(SourceUpdate::at(
            source.id,
            source.book_id,
            SourceUpdateState::Conflict,
            None,
        ));
    }
    let expected = store
        .active_revision(source.book_id)?
        .ok_or(Error::NoSuchBook(source.book_id))?;
    let captured = if source.kind == "local" {
        captured_path(Path::new(&source.locator), book_dir, ctx)?
    } else {
        let staging = crate::infra::paths::staging_dir(book_dir);
        let staged = crate::ingest::external_source::capture_external(
            access,
            &source.locator,
            &staging,
            ctx,
        )?;
        capture_staged(
            &staged.name,
            staged.path.clone(),
            &staging,
            None,
            Some(staged.hash.clone()),
            ctx,
        )?
    };
    let Some(current) =
        store.observe_source_at(source.id, observed.generation, &captured.fingerprint())?
    else {
        return Ok(SourceUpdate::at(
            source.id,
            source.book_id,
            SourceUpdateState::Conflict,
            None,
        ));
    };
    prepare_for_book(
        store,
        Preparation {
            book_id: source.book_id,
            source: Some(current),
            expected,
            captured,
            book_dir,
            auto: true,
            index_only: false,
            one_shot_replace: false,
            identity_confirmed: false,
        },
        ctx,
    )
}

/// 用户明确重新关联时才能更换主来源；验证失败不改变当前阅读版。
pub fn relink_source(
    store: &Store,
    book_id: i64,
    path: &Path,
    book_dir: &Path,
    ctx: &StageCtx,
) -> Result<SourceUpdate> {
    let locator = source_locator(path, book_dir)?;
    let expected = store
        .active_revision(book_id)?
        .ok_or(Error::NoSuchBook(book_id))?;
    let captured = captured_path(path, book_dir, ctx)?;
    // 先解析再绑定，坏文件不会抢走原主来源。
    let _ = parse_capture(store, &captured, ParseBudget::Auto)?;
    if store
        .books_for_content_hash(&captured.hash)?
        .iter()
        .any(|id| *id != book_id)
    {
        return Err(issue(path, "原件内容已经属于另一张书卡，现有来源保持不变"));
    }
    let fingerprint = captured.fingerprint();
    let expected_fence = expected.fence;
    let update = prepare_for_book(
        store,
        Preparation {
            book_id,
            source: None,
            expected,
            captured,
            book_dir,
            auto: false,
            index_only: false,
            one_shot_replace: false,
            identity_confirmed: true,
        },
        ctx,
    )?;
    link_prepared(
        store,
        update,
        expected_fence,
        SourceLink {
            kind: "local".into(),
            locator,
        },
        &fingerprint,
    )
}

/// 一次性替换不把移动 cache/下载暂存登记为可跟踪原件。
pub fn update_from_path(
    store: &Store,
    book_id: i64,
    path: &Path,
    book_dir: &Path,
    ctx: &StageCtx,
) -> Result<SourceUpdate> {
    let expected = store
        .active_revision(book_id)?
        .ok_or(Error::NoSuchBook(book_id))?;
    let captured = captured_path(path, book_dir, ctx)?;
    prepare_for_book(
        store,
        Preparation {
            book_id,
            source: None,
            expected,
            captured,
            book_dir,
            auto: true,
            index_only: false,
            one_shot_replace: true,
            identity_confirmed: true,
        },
        ctx,
    )
}

pub fn update_from_link(
    store: &Store,
    book_id: i64,
    cache_path: &Path,
    link: Option<SourceLink>,
    book_dir: &Path,
    ctx: &StageCtx,
) -> Result<SourceUpdate> {
    let Some(link) = link else {
        return update_from_path(store, book_id, cache_path, book_dir, ctx);
    };
    if link.kind == "local" {
        return relink_source(store, book_id, Path::new(&link.locator), book_dir, ctx);
    }
    let expected = store
        .active_revision(book_id)?
        .ok_or(Error::NoSuchBook(book_id))?;
    let captured = captured_path(cache_path, book_dir, ctx)?;
    let _ = parse_capture(store, &captured, ParseBudget::Auto)?;
    if store
        .books_for_content_hash(&captured.hash)?
        .iter()
        .any(|id| *id != book_id)
    {
        return Err(issue(
            cache_path,
            "原件内容已经属于另一张书卡，现有来源保持不变",
        ));
    }
    let fingerprint = captured.fingerprint();
    let expected_fence = expected.fence;
    let update = prepare_for_book(
        store,
        Preparation {
            book_id,
            source: None,
            expected,
            captured,
            book_dir,
            auto: false,
            index_only: false,
            one_shot_replace: false,
            identity_confirmed: true,
        },
        ctx,
    )?;
    link_prepared(store, update, expected_fence, link, &fingerprint)
}

fn link_prepared(
    store: &Store,
    mut update: SourceUpdate,
    expected: RevisionFence,
    link: SourceLink,
    fingerprint: &str,
) -> Result<SourceUpdate> {
    if update.state == SourceUpdateState::Unchanged {
        let source = store.bind_source(update.book_id, &link.locator, &link.kind, true)?;
        update.source_id = store.observe_source(source.id, fingerprint)?.id;
        return Ok(update);
    }
    if update.state != SourceUpdateState::Ready {
        return Ok(update);
    }
    let target = update
        .revision
        .ok_or_else(|| issue(Path::new("source"), "新候选缺少版本标记"))?;
    let source = match store.link_ready_source(
        update.book_id,
        target,
        expected,
        &link.locator,
        &link.kind,
        fingerprint,
    ) {
        Ok(source) => source,
        Err(error) => {
            let _ = store.abandon_revision(update.book_id, target);
            return Err(error.into());
        }
    };
    update.source_id = source.id;
    update.state = match store.adopt_revision(update.book_id, target, Some(expected), false)? {
        AdoptResult::Adopted | AdoptResult::AlreadyActive => SourceUpdateState::Adopted,
        AdoptResult::ReadersActive => SourceUpdateState::Ready,
        AdoptResult::Stale => SourceUpdateState::Conflict,
    };
    Ok(update)
}

/// 首次打开 Legacy 时先捕获固定副本；实际字节不同只能准备候选，不能配旧索引阅读。
pub fn prepare_legacy_revision(
    store: &Store,
    book_id: i64,
    book_dir: &Path,
    ctx: &StageCtx,
) -> Result<SourceUpdate> {
    let expected = store
        .active_revision(book_id)?
        .ok_or(Error::NoSuchBook(book_id))?;
    if expected.state != "legacy-unverified" {
        return Ok(SourceUpdate::at(
            0,
            book_id,
            SourceUpdateState::Unchanged,
            Some(expected.fence),
        ));
    }
    let captured = captured_path(Path::new(&expected.managed_path), book_dir, ctx)?;
    let same = !expected.content_hash.is_empty() && expected.content_hash == captured.hash;
    prepare_for_book(
        store,
        Preparation {
            book_id,
            source: None,
            expected,
            captured,
            book_dir,
            auto: same,
            index_only: false,
            one_shot_replace: false,
            identity_confirmed: true,
        },
        ctx,
    )
}

pub(super) fn reindex_snapshot(store: &Store, book_id: i64, ctx: &StageCtx) -> Result<()> {
    let expected = store
        .active_revision(book_id)?
        .ok_or(Error::NoSuchBook(book_id))?;
    let path = PathBuf::from(&expected.managed_path);
    let root = if expected.library_root.is_empty() {
        crate::store::book_root(&path)
    } else {
        PathBuf::from(&expected.library_root)
    };
    let captured = captured_path(&path, &root, ctx)?;
    if !expected.content_hash.is_empty() && expected.content_hash != captured.hash {
        return Err(issue(
            &path,
            "现有版本字节已改变，拒绝把旧坐标就地改为新内容",
        ));
    }
    let update = prepare_for_book(
        store,
        Preparation {
            book_id,
            source: None,
            expected,
            captured,
            book_dir: &root,
            auto: true,
            index_only: true,
            one_shot_replace: false,
            identity_confirmed: false,
        },
        ctx,
    )?;
    if matches!(
        update.state,
        SourceUpdateState::Adopted | SourceUpdateState::Ready | SourceUpdateState::Unchanged
    ) {
        Ok(())
    } else {
        Err(issue(&path, "重建索引候选已过期"))
    }
}

pub(super) struct StagedImport<'a> {
    pub name: &'a str,
    pub staged: PathBuf,
    pub book_dir: &'a Path,
    pub staging: &'a Path,
    pub per_book: usize,
    pub original: Option<&'a Path>,
    pub persistent: Option<&'a Path>,
    pub hash: Option<String>,
    pub source_link: Option<SourceLink>,
}

pub(super) fn import_staged(
    store: &Store,
    input: StagedImport<'_>,
    ctx: &StageCtx,
) -> Result<Imported> {
    let StagedImport {
        name,
        staged,
        book_dir,
        staging,
        per_book,
        original,
        persistent,
        hash,
        source_link,
    } = input;
    let captured = capture_staged(name, staged, staging, original, hash, ctx)?;
    let _claim = super::pipeline::claim_hash(&captured.hash);
    let local_locator = persistent
        .filter(|path| {
            !path
                .canonicalize()
                .ok()
                .zip(book_dir.canonicalize().ok())
                .is_some_and(|(p, root)| p.starts_with(root))
        })
        .map(|p| source_locator(p, book_dir))
        .transpose()?;
    let link = source_link.filter(|link| link.kind != "local").or_else(|| {
        local_locator.map(|locator| SourceLink {
            kind: "local".into(),
            locator,
        })
    });
    let locator = link.as_ref().map(|link| link.locator.clone());
    let source_kind = link.as_ref().map_or("one-shot", |link| link.kind.as_str());
    if let Some(locator) = &locator
        && let Some(source) = store.lookup_source(locator)?
    {
        let expected = store
            .active_revision(source.book_id)?
            .ok_or(Error::NoSuchBook(source.book_id))?;
        if !source.is_primary
            && (expected.content_hash != captured.hash
                || expected.resource_hash != captured.resource_hash)
        {
            return Err(issue(
                &captured.main,
                "备用来源已分叉，请明确重新关联主原件，当前阅读版本不变",
            ));
        }
        let Some(source) =
            store.observe_source_at(source.id, source.generation, &captured.fingerprint())?
        else {
            return Err(issue(&captured.main, "来源已变化，等待最新检查结果"));
        };
        let update = prepare_for_book(
            store,
            Preparation {
                book_id: source.book_id,
                auto: source.is_primary,
                source: Some(source),
                expected,
                captured,
                book_dir,
                index_only: false,
                one_shot_replace: false,
                identity_confirmed: false,
            },
            ctx,
        )?;
        return imported_update(store, update);
    }
    let owners = super::indexing::landing("book_by_hash", || {
        store.books_for_content_hash(&captured.hash)
    })?;
    if owners.len() > 1 {
        return Err(issue(
            &captured.main,
            "相同字节存在多个历史书目，请明确选择关联对象",
        ));
    }
    if let Some(&book_id) = owners.first() {
        let active = store
            .active_revision(book_id)?
            .ok_or(Error::NoSuchBook(book_id))?;
        if active.content_hash != captured.hash || active.resource_hash != captured.resource_hash {
            return Err(issue(
                &captured.main,
                "此内容属于已有书目的历史/资源分支，请明确选择更新",
            ));
        }
        let actual = identity::sha256_of_path(Path::new(&active.managed_path));
        if actual.as_ref().is_ok_and(|h| h.to_hex() != captured.hash) {
            return Err(issue(
                Path::new(&active.managed_path),
                "已有副本字节已改变，不能误报重复内容",
            ));
        }
        let missing = actual
            .as_ref()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
        if !missing {
            actual.io_path(&active.managed_path)?;
        }
        if let Some(locator) = &locator {
            let has_primary = store
                .sources_for_book(book_id)?
                .iter()
                .any(|s| s.is_primary);
            let source = store.bind_source(book_id, locator, source_kind, !has_primary)?;
            store.observe_source(source.id, &captured.fingerprint())?;
        }
        if missing {
            let update = prepare_for_book(
                store,
                Preparation {
                    book_id,
                    source: None,
                    expected: active,
                    captured,
                    book_dir,
                    auto: true,
                    index_only: false,
                    one_shot_replace: false,
                    identity_confirmed: false,
                },
                ctx,
            )?;
            return imported_update(store, update);
        }
        return Ok(Imported::Duplicate { book_id });
    }
    let uuid = crate::sync::new_device_id();
    let mut doc = parse_capture(store, &captured, ParseBudget::AtMost(per_book))?;
    super::indexing::land_cover(book_dir, doc.as_mut());
    let fence = RevisionFence {
        rev_no: 1,
        index_generation: folio_format::INDEX_VERSION,
    };
    let published = publish_capture(&captured, book_dir, &uuid, fence, doc.as_ref())?;
    let registration = ImportRegistration {
        library_root: book_dir,
        book_uuid: &uuid,
        doc: doc.as_ref(),
        managed_path: &published.main,
        content_hash: &captured.hash,
        resource_hash: &captured.resource_hash,
        resource_manifest: &captured.manifest,
        source_locator: locator.as_deref(),
        source_kind,
        source_fingerprint: &captured.fingerprint(),
    };
    let revision =
        super::indexing::landing("register_import", || store.register_import(registration))?;
    published.commit();
    Ok(Imported::Added(Registered {
        book_id: revision.book_id,
        dest: PathBuf::from(revision.managed_path),
        title: doc.meta().title.clone(),
    }))
}

fn imported_update(store: &Store, update: SourceUpdate) -> Result<Imported> {
    match update.state {
        SourceUpdateState::Unchanged => Ok(Imported::Duplicate {
            book_id: update.book_id,
        }),
        SourceUpdateState::Adopted | SourceUpdateState::Ready => {
            let row = store
                .book(update.book_id)?
                .ok_or(Error::NoSuchBook(update.book_id))?;
            Ok(Imported::Added(Registered {
                book_id: row.id,
                dest: PathBuf::from(row.path),
                title: row.title,
            }))
        }
        _ => Err(issue(
            Path::new("source"),
            update
                .message
                .as_deref()
                .unwrap_or("原件更新出现冲突，当前版本已保留"),
        )),
    }
}

struct Preparation<'a> {
    book_id: i64,
    source: Option<BookSource>,
    expected: BookRevision,
    captured: Captured,
    book_dir: &'a Path,
    auto: bool,
    index_only: bool,
    one_shot_replace: bool,
    identity_confirmed: bool,
}

struct Reservation<'a> {
    store: &'a Store,
    book: i64,
    fence: RevisionFence,
    finished: bool,
}
impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.store.abandon_revision(self.book, self.fence);
        }
    }
}

fn prepare_for_book(
    store: &Store,
    request: Preparation<'_>,
    ctx: &StageCtx,
) -> Result<SourceUpdate> {
    let Preparation {
        book_id,
        source,
        expected,
        captured,
        book_dir,
        auto,
        index_only,
        one_shot_replace,
        identity_confirmed,
    } = request;
    let source = source.as_ref();
    let source_id = source.map_or(0, |s| s.id);
    if !index_only
        && expected.state == "active"
        && expected.content_hash == captured.hash
        && expected.resource_hash == captured.resource_hash
    {
        match identity::sha256_of_path(Path::new(&expected.managed_path)) {
            Ok(hash) if hash.to_hex() == captured.hash => {
                return Ok(SourceUpdate::at(
                    source_id,
                    book_id,
                    SourceUpdateState::Unchanged,
                    Some(expected.fence),
                ));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(issue(
                    Path::new(&expected.managed_path),
                    "当前阅读副本已被外部改写，保留记录等待修复",
                ));
            }
        }
    }
    if let Some(ready) = store
        .ready_revisions(book_id)?
        .into_iter()
        .filter(|_| !index_only && !one_shot_replace)
        .find(|ready| {
            ready.content_hash == captured.hash
                && ready.resource_hash == captured.resource_hash
                && ready.source_id == source.map(|s| s.id)
                && ready.source_generation == source.map_or(0, |s| s.generation)
        })
    {
        if !auto {
            return Ok(SourceUpdate::at(
                source_id,
                book_id,
                SourceUpdateState::Ready,
                Some(ready.fence),
            ));
        }
        let result = store.adopt_revision(book_id, ready.fence, Some(expected.fence), false)?;
        let state = match result {
            AdoptResult::Adopted | AdoptResult::AlreadyActive => SourceUpdateState::Adopted,
            AdoptResult::ReadersActive => SourceUpdateState::Ready,
            AdoptResult::Stale => SourceUpdateState::Conflict,
        };
        return Ok(SourceUpdate::at(
            source_id,
            book_id,
            state,
            Some(ready.fence),
        ));
    }
    if store
        .books_for_content_hash(&captured.hash)?
        .iter()
        .any(|id| *id != book_id)
    {
        let mut update = SourceUpdate::at(source_id, book_id, SourceUpdateState::Conflict, None);
        update.message = Some("新版字节已经属于另一张书卡，需明确处理冲突".into());
        return Ok(update);
    }
    let mut doc = parse_capture(store, &captured, ParseBudget::Auto)?;
    if source.is_some() && !identity_confirmed {
        let old = store.revision_snapshot(book_id, expected.fence)?;
        if old.meta.format != doc.meta().format
            || (!old.meta.isbn.is_empty()
                && !doc.meta().isbn.is_empty()
                && old.meta.isbn != doc.meta().isbn)
        {
            let mut update =
                SourceUpdate::at(source_id, book_id, SourceUpdateState::Conflict, None);
            update.message = Some("原件格式或出版物标识已改变，请确认是否更新这本书".into());
            return Ok(update);
        }
    }
    if ctx.cancel().triggered() {
        return Err(issue(&captured.main, "准备新版已取消"));
    }
    let fence = if index_only {
        store.reserve_reindex(book_id)?
    } else {
        store.reserve_revision(book_id)?
    };
    let mut reservation = Reservation {
        store,
        book: book_id,
        fence,
        finished: false,
    };
    let projections =
        super::projections::prepare(store, book_id, doc.as_ref(), &captured.hash, fence, ctx)?;
    let position = super::projections::position(store, book_id, &expected, doc.as_ref(), ctx)?;
    let operation = crate::sync::new_device_id();
    store.note_revision_operation(
        &operation,
        book_id,
        fence,
        "validating",
        &captured.dir.to_string_lossy(),
        "",
    )?;
    super::indexing::land_cover(book_dir, doc.as_mut());
    let result: Result<SourceUpdate> = (|| {
        let published = publish_capture(
            &captured,
            book_dir,
            &expected.book_uuid,
            fence,
            doc.as_ref(),
        )?;
        store.note_revision_operation(
            &operation,
            book_id,
            fence,
            "published",
            &published.dir.to_string_lossy(),
            "",
        )?;
        super::indexing::landing("ready_revision", || {
            let input = RevisionReady {
                library_root: book_dir,
                book_id,
                fence,
                expected_active: Some(expected.fence),
                source_id: source.map(|s| s.id),
                source_generation: source.map_or(0, |s| s.generation),
                content_hash: &captured.hash,
                resource_hash: &captured.resource_hash,
                managed_path: &published.main,
                resource_manifest: &captured.manifest,
                doc: doc.as_ref(),
            };
            if one_shot_replace {
                store.ready_replacement_revision(input)
            } else {
                store.ready_revision(input)
            }
        })?;
        let owned_directory = published.dir.to_string_lossy().into_owned();
        published.commit();
        super::projections::apply(store, book_id, fence, projections)?;
        if let Some((mode, locator)) = position {
            store.set_position_projection(book_id, fence, &mode, &locator)?;
        }
        store.note_revision_operation(&operation, book_id, fence, "ready", &owned_directory, "")?;
        if !auto {
            return Ok(SourceUpdate::at(
                source_id,
                book_id,
                SourceUpdateState::Ready,
                Some(fence),
            ));
        }
        let adopted = store.adopt_revision(book_id, fence, Some(expected.fence), false)?;
        if matches!(adopted, AdoptResult::Adopted | AdoptResult::AlreadyActive)
            && let Err(error) = store.note_revision_operation(
                &operation,
                book_id,
                fence,
                "active",
                &owned_directory,
                "",
            )
        {
            tracing::warn!("版本已采用，操作账更新失败：{error}");
        }
        Ok(SourceUpdate::at(
            source_id,
            book_id,
            match adopted {
                AdoptResult::Adopted | AdoptResult::AlreadyActive => SourceUpdateState::Adopted,
                AdoptResult::ReadersActive => SourceUpdateState::Ready,
                AdoptResult::Stale => SourceUpdateState::Conflict,
            },
            Some(fence),
        ))
    })();
    reservation.finished = result.is_ok();
    if result.is_err() {
        let _ = store.note_revision_operation(
            &operation,
            book_id,
            fence,
            "failed",
            "",
            &result.as_ref().err().unwrap().to_string(),
        );
        let _ = store.abandon_revision(book_id, fence);
    }
    result
}
