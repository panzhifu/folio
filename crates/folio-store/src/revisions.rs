//! 阅读版本快照、单事务采用与会话租约。磁盘捕获和解析由 core 在锁外完成。

use std::path::Path;
use std::sync::{Arc, Mutex};

use folio_format::document::{ContentRef, Document};
use folio_format::{Content, DocMeta, Format};
use rusqlite::{Connection, OptionalExtension as _, params};

use crate::{BookRevision, Error, Result, RevisionFence, Store};

#[path = "revisions/index.rs"]
mod index;

pub struct RevisionSnapshot {
    pub revision: BookRevision,
    pub meta: DocMeta,
    pub content: Content,
}
pub struct LeasedRevision {
    pub snapshot: RevisionSnapshot,
    pub lease: RevisionLease,
}
pub struct RevisionLease {
    leases: Arc<Mutex<std::collections::HashMap<(i64, RevisionFence), usize>>>,
    book_id: i64,
    fence: RevisionFence,
}
impl Drop for RevisionLease {
    fn drop(&mut self) {
        let mut leases = self.leases.lock().unwrap_or_else(|e| e.into_inner());
        let key = (self.book_id, self.fence);
        if let Some(count) = leases.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                leases.remove(&key);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdoptResult {
    Adopted,
    AlreadyActive,
    ReadersActive,
    Stale,
}

pub struct ImportRegistration<'a> {
    pub book_uuid: &'a str,
    pub doc: &'a dyn Document,
    pub managed_path: &'a Path,
    pub library_root: &'a Path,
    pub content_hash: &'a str,
    pub resource_hash: &'a str,
    pub resource_manifest: &'a str,
    pub source_locator: Option<&'a str>,
    pub source_kind: &'a str,
    pub source_fingerprint: &'a str,
}
pub struct RevisionReady<'a> {
    pub book_id: i64,
    pub fence: RevisionFence,
    pub expected_active: Option<RevisionFence>,
    pub source_id: Option<i64>,
    pub source_generation: i64,
    pub content_hash: &'a str,
    pub resource_hash: &'a str,
    pub managed_path: &'a Path,
    pub library_root: &'a Path,
    pub resource_manifest: &'a str,
    pub doc: &'a dyn Document,
}

const COLUMNS: &str = "r.book_id,b.book_uuid,r.rev_no,r.index_generation,r.content_hash,r.resource_hash,r.managed_path,r.resource_manifest,r.state,r.source_id,r.source_generation,r.library_root";
fn revision_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<BookRevision> {
    Ok(BookRevision {
        book_id: r.get(0)?,
        book_uuid: r.get(1)?,
        fence: RevisionFence {
            rev_no: r.get(2)?,
            index_generation: r.get(3)?,
        },
        content_hash: r.get(4)?,
        resource_hash: r.get(5)?,
        managed_path: r.get(6)?,
        resource_manifest: r.get(7)?,
        state: r.get(8)?,
        source_id: r.get(9)?,
        source_generation: r.get(10)?,
        library_root: r.get(11)?,
    })
}

pub(crate) fn current_revision_in(conn: &Connection, book_id: i64) -> Result<RevisionFence> {
    Ok(conn.query_row(
        "SELECT active_rev_no,active_index_generation FROM books WHERE id=?1",
        [book_id],
        |r| {
            Ok(RevisionFence {
                rev_no: r.get(0)?,
                index_generation: r.get(1)?,
            })
        },
    )?)
}
pub(crate) fn check_revision_in(
    conn: &Connection,
    book_id: i64,
    fence: RevisionFence,
) -> Result<()> {
    if current_revision_in(conn, book_id)? != fence {
        return Err(Error::Integrity("阅读版本已变化，拒绝旧会话写入".into()));
    }
    Ok(())
}
fn revision_in(
    conn: &Connection,
    book_id: i64,
    fence: RevisionFence,
) -> Result<Option<BookRevision>> {
    Ok(conn.query_row(&format!("SELECT {COLUMNS} FROM book_revisions r JOIN books b ON b.id=r.book_id WHERE r.book_id=?1 AND r.rev_no=?2 AND r.index_generation=?3"),params![book_id,fence.rev_no,fence.index_generation],revision_row).optional()?)
}
fn doc_content(doc: &dyn Document) -> Content {
    match doc.content() {
        ContentRef::Reflow(flow) => Content::Reflow(flow.clone()),
        ContentRef::Fixed(fixed) => Content::Fixed(fixed.clone()),
    }
}

fn write_metadata_in(conn: &Connection, id: i64, m: &DocMeta) -> Result<()> {
    conn.execute("UPDATE books SET format=?2,size=?3,contributors=?4,subjects=?5,description=?6,publisher=?7,published=?8,language=?9,isbn=?10,dctype=?11,rights=?12,series=?13,series_position=?14 WHERE id=?1",params![id,m.format.as_str(),m.size as i64,m.contributors.join("\n"),m.subjects.join("\n"),m.description,m.publisher,m.published,m.language,m.isbn,m.dctype,m.rights,m.series.as_ref().map(|s|s.name.as_str()).unwrap_or(""),m.series.as_ref().map(|s|s.position.as_str()).unwrap_or("")])?;
    Ok(())
}

fn file_facts(path: &Path, fallback_size: u64) -> (u64, i64) {
    match std::fs::metadata(path) {
        Ok(metadata) => (
            metadata.len(),
            metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64),
        ),
        Err(_) => (fallback_size, 0),
    }
}

pub(crate) fn ensure_book_identity_in(conn: &Connection, book_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE books SET book_uuid=lower(hex(randomblob(16))) WHERE id=?1 AND book_uuid=''",
        [book_id],
    )?;
    conn.execute("INSERT OR IGNORE INTO book_revisions(book_id,rev_no,index_generation,content_hash,managed_path,file_size,file_mtime,state,created_at) SELECT id,0,0,coalesce(content_hash,''),path,size,mtime,'legacy-unverified',added_at FROM books WHERE id=?1 AND active_rev_no=0 AND active_index_generation=0",[book_id])?;
    Ok(())
}

fn other_owner_in(conn: &Connection, hash: &str, except: Option<i64>) -> Result<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM books WHERE content_hash=?1 AND (?2 IS NULL OR id<>?2) UNION ALL SELECT 1 FROM book_revisions WHERE content_hash=?1 AND (?2 IS NULL OR book_id<>?2))",params![hash,except],|r|r.get(0))?)
}

fn legacy_index_in(conn: &Connection, book_id: i64) -> Result<()> {
    ensure_book_identity_in(conn, book_id)?;
    let Some(revision) = revision_in(conn, book_id, RevisionFence::default())? else {
        return Ok(());
    };
    let pending:bool=conn.query_row("SELECT index_json='' FROM book_revisions WHERE book_id=?1 AND rev_no=0 AND index_generation=0",[book_id],|r|r.get(0))?;
    if !pending {
        return Ok(());
    }
    let Some(content) = Store::load_content_in(conn, book_id)? else {
        return Ok(());
    };
    let meta = conn.query_row(
        "SELECT title,authors,format,size FROM books WHERE id=?1",
        [book_id],
        |r| {
            let authors: String = r.get(1)?;
            let format: String = r.get(2)?;
            Ok(DocMeta {
                title: r.get(0)?,
                authors: authors
                    .split('\n')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
                format: Format::parse(&format).unwrap_or_default(),
                size: r.get::<_, i64>(3)?.max(0) as u64,
                ..Default::default()
            })
        },
    )?;
    conn.execute("UPDATE book_revisions SET index_json=?2,metadata_json=?3 WHERE book_id=?1 AND rev_no=0 AND index_generation=0",params![revision.book_id,index::encode(&content,meta.format)?,index::encode_meta(&meta)?])?;
    Ok(())
}

fn snapshot_in(conn: &Connection, book_id: i64, fence: RevisionFence) -> Result<RevisionSnapshot> {
    if fence == RevisionFence::default() {
        legacy_index_in(conn, book_id)?;
    }
    let revision = revision_in(conn, book_id, fence)?
        .ok_or_else(|| Error::Integrity("阅读版本不存在".into()))?;
    let (meta,content):(String,String)=conn.query_row("SELECT metadata_json,index_json FROM book_revisions WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",params![book_id,fence.rev_no,fence.index_generation],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if content.is_empty() {
        return Err(Error::Integrity("阅读版本缺少索引".into()));
    }
    let mut meta = index::decode_meta(&meta)?;
    if current_revision_in(conn, book_id)? == fence {
        let (title, authors): (String, String) = conn.query_row(
            "SELECT title,authors FROM books WHERE id=?1",
            [book_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        meta.title = title;
        meta.authors = authors
            .lines()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
    }
    Ok(RevisionSnapshot {
        revision,
        meta,
        content: index::decode(&content)?,
    })
}

impl Store {
    /// 数据库快照与文件租约覆盖同一连接临界区，避免备份期间被历史回收删除。
    pub fn backup_to_pinned(&self, path: &Path) -> Result<Vec<RevisionLease>> {
        if path.exists() {
            return Err(Error::Integrity("备份临时目标已存在".into()));
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let pins = self.retain_files_in(&conn)?;
        conn.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])?;
        Ok(pins)
    }
    /// 在备份数据库快照之前调用，整组字节引用固定到备份结束。
    pub fn retain_all_revision_files(&self) -> Result<Vec<RevisionLease>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        self.retain_files_in(&conn)
    }
    fn retain_files_in(&self, conn: &Connection) -> Result<Vec<RevisionLease>> {
        let mut stmt=conn.prepare("SELECT book_id,rev_no,index_generation FROM book_revisions WHERE managed_path<>'' AND state NOT IN ('preparing','failed')")?;
        let keys: Vec<(i64, RevisionFence)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    RevisionFence {
                        rev_no: r.get(1)?,
                        index_generation: r.get(2)?,
                    },
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        let mut counts = self
            .revision_leases
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Ok(keys
            .into_iter()
            .map(|(book_id, fence)| {
                *counts.entry((book_id, fence)).or_default() += 1;
                RevisionLease {
                    leases: self.revision_leases.clone(),
                    book_id,
                    fence,
                }
            })
            .collect())
    }
    /// 固定历史版本供备份或定位读取，直到句柄释放。
    pub fn retain_revision(&self, book_id: i64, fence: RevisionFence) -> Result<RevisionLease> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if revision_in(&conn, book_id, fence)?.is_none_or(|r| {
            r.managed_path.is_empty() || matches!(r.state.as_str(), "preparing" | "failed")
        }) {
            return Err(Error::Integrity("待保留版本不存在或字节已回收".into()));
        }
        *self
            .revision_leases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry((book_id, fence))
            .or_default() += 1;
        Ok(RevisionLease {
            leases: self.revision_leases.clone(),
            book_id,
            fence,
        })
    }
    pub fn note_revision_operation(
        &self,
        operation_id: &str,
        book_id: i64,
        fence: RevisionFence,
        stage: &str,
        owned_path: &str,
        error: &str,
    ) -> Result<()> {
        if !matches!(
            stage,
            "capturing" | "validating" | "published" | "ready" | "active" | "failed"
        ) {
            return Err(Error::Integrity("未知内容操作阶段".into()));
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute("INSERT INTO revision_operations(id,book_id,rev_no,index_generation,stage,owned_path,error,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET stage=excluded.stage,owned_path=excluded.owned_path,error=excluded.error,updated_at=excluded.updated_at",params![operation_id,book_id,fence.rev_no,fence.index_generation,stage,owned_path,error,crate::now()])?;
        Ok(())
    }

    /// 只返回已完成且不再需要的历史；调用方核对路径所有权后再删字节。
    pub fn prunable_revisions(&self, book_id: i64) -> Result<Vec<BookRevision>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // 只退休不可再采用的候选；同代暂时失联或待重关联仍可恢复。
        conn.execute(
            "UPDATE book_revisions SET state='failed'
             WHERE book_id=?1 AND state='ready' AND EXISTS (
               SELECT 1 FROM books b WHERE b.id=book_revisions.book_id
               AND (book_revisions.rev_no<>b.active_rev_no OR book_revisions.index_generation<>b.active_index_generation)
               AND (book_revisions.rev_no<b.active_rev_no
                 OR (book_revisions.rev_no=b.active_rev_no AND book_revisions.index_generation<b.active_index_generation)
                 OR EXISTS (SELECT 1 FROM book_sources s WHERE s.id=book_revisions.source_id AND s.generation<>book_revisions.source_generation))
             )",
            [book_id],
        )?;
        let current = current_revision_in(&conn, book_id)?;
        let mut stmt=conn.prepare(&format!("SELECT {COLUMNS} FROM book_revisions r JOIN books b ON b.id=r.book_id WHERE r.book_id=?1 AND r.state IN ('retained','failed') ORDER BY r.rev_no DESC,r.index_generation DESC"))?;
        let revisions: Vec<BookRevision> = stmt
            .query_map([book_id], revision_row)?
            .collect::<std::result::Result<_, _>>()?;
        let previous = revisions
            .iter()
            .find(|r| r.state == "retained")
            .map(|r| r.fence);
        let leases = self
            .revision_leases
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Ok(revisions
            .into_iter()
            .filter(|r| {
                r.fence != current
                    && Some(r.fence) != previous
                    && !leases.contains_key(&(book_id, r.fence))
            })
            .collect())
    }

    /// 先移除登记的字节引用再由调用方回收；身份、索引与墓碑归属保留。
    pub fn detach_revision_file(
        &self,
        book_id: i64,
        fence: RevisionFence,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        if current_revision_in(&tx, book_id)? == fence
            || self
                .revision_leases
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&(book_id, fence))
        {
            return Ok(None);
        }
        let Some(revision) = revision_in(&tx, book_id, fence)? else {
            return Ok(None);
        };
        let previous:Option<(i64,i64)>=tx.query_row("SELECT rev_no,index_generation FROM book_revisions WHERE book_id=?1 AND state='retained' ORDER BY rev_no DESC,index_generation DESC LIMIT 1",[book_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if previous == Some((fence.rev_no, fence.index_generation)) {
            return Ok(None);
        }
        if !matches!(revision.state.as_str(), "retained" | "failed") {
            return Ok(None);
        }
        let shared:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM book_revisions WHERE managed_path=?1 AND NOT(book_id=?2 AND rev_no=?3 AND index_generation=?4))",params![revision.managed_path,book_id,fence.rev_no,fence.index_generation],|r|r.get(0))?;
        if shared {
            return Ok(None);
        }
        tx.execute("UPDATE book_revisions SET managed_path='' WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",params![book_id,fence.rev_no,fence.index_generation])?;
        tx.commit()?;
        Ok(Some(revision.managed_path))
    }
    pub fn books_for_content_hash(&self, hash: &str) -> Result<Vec<i64>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt=conn.prepare("SELECT id FROM books WHERE content_hash=?1 UNION SELECT book_id FROM book_revisions WHERE content_hash=?1 ORDER BY 1")?;
        Ok(stmt
            .query_map([hash], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?)
    }
    pub fn abandon_revision(&self, book_id: i64, fence: RevisionFence) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute("UPDATE book_revisions SET state='failed' WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3 AND state IN ('preparing','ready')",params![book_id,fence.rev_no,fence.index_generation])?;
        Ok(())
    }
    pub fn active_revision(&self, book_id: i64) -> Result<Option<BookRevision>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        ensure_book_identity_in(&conn, book_id)?;
        let fence = current_revision_in(&conn, book_id)?;
        revision_in(&conn, book_id, fence)
    }
    pub fn book_uuid(&self, book_id: i64) -> Result<String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        ensure_book_identity_in(&conn, book_id)?;
        Ok(
            conn.query_row("SELECT book_uuid FROM books WHERE id=?1", [book_id], |r| {
                r.get(0)
            })?,
        )
    }
    pub fn revision_snapshot(
        &self,
        book_id: i64,
        fence: RevisionFence,
    ) -> Result<RevisionSnapshot> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        snapshot_in(&conn, book_id, fence)
    }
    pub fn acquire_revision(&self, book_id: i64) -> Result<LeasedRevision> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let fence = current_revision_in(&conn, book_id)?;
        self.acquire_revision_in(&conn, book_id, fence)
    }
    pub fn acquire_revision_at(
        &self,
        book_id: i64,
        fence: RevisionFence,
    ) -> Result<LeasedRevision> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        self.acquire_revision_in(&conn, book_id, fence)
    }
    fn acquire_revision_in(
        &self,
        conn: &Connection,
        book_id: i64,
        fence: RevisionFence,
    ) -> Result<LeasedRevision> {
        let snapshot = snapshot_in(conn, book_id, fence)?;
        if snapshot.revision.managed_path.is_empty()
            || !matches!(snapshot.revision.state.as_str(), "active" | "retained")
        {
            return Err(Error::Integrity("阅读版本尚未验证或字节已回收".into()));
        }
        *self
            .revision_leases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry((book_id, fence))
            .or_default() += 1;
        Ok(LeasedRevision {
            snapshot,
            lease: RevisionLease {
                leases: self.revision_leases.clone(),
                book_id,
                fence,
            },
        })
    }
    pub fn revision_is_leased(&self, book_id: i64, fence: RevisionFence) -> bool {
        self.revision_leases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&(book_id, fence))
    }
    pub fn revisions(&self, book_id: i64) -> Result<Vec<BookRevision>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt=conn.prepare(&format!("SELECT {COLUMNS} FROM book_revisions r JOIN books b ON b.id=r.book_id WHERE r.book_id=?1 ORDER BY r.rev_no DESC,r.index_generation DESC"))?;
        Ok(stmt
            .query_map([book_id], revision_row)?
            .collect::<std::result::Result<_, _>>()?)
    }
    pub fn ready_revisions(&self, book_id: i64) -> Result<Vec<BookRevision>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt=conn.prepare(&format!("SELECT {COLUMNS} FROM book_revisions r JOIN books b ON b.id=r.book_id LEFT JOIN book_sources s ON s.id=r.source_id WHERE r.book_id=?1 AND r.state='ready' AND (r.rev_no>b.active_rev_no OR (r.rev_no=b.active_rev_no AND r.index_generation>b.active_index_generation)) AND (r.source_id IS NULL OR (s.book_id=r.book_id AND s.is_primary=1 AND s.generation=r.source_generation AND s.state NOT IN ('suppressed','relink-required'))) ORDER BY r.rev_no DESC,r.index_generation DESC"))?;
        Ok(stmt
            .query_map([book_id], revision_row)?
            .collect::<std::result::Result<_, _>>()?)
    }
    pub fn reserve_revision(&self, book_id: i64) -> Result<RevisionFence> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        let rev: i64 = tx.query_row(
            "SELECT coalesce(max(rev_no),0)+1 FROM book_revisions WHERE book_id=?1",
            [book_id],
            |r| r.get(0),
        )?;
        tx.execute("INSERT INTO book_revisions(book_id,rev_no,index_generation,content_hash,managed_path,state,created_at) VALUES(?1,?2,?3,'','','preparing',?4)",params![book_id,rev,folio_format::INDEX_VERSION,crate::now()])?;
        tx.commit()?;
        Ok(RevisionFence {
            rev_no: rev,
            index_generation: folio_format::INDEX_VERSION,
        })
    }
    /// 同一份字节换坐标代，版本路径和内容身份不变。
    pub fn reserve_reindex(&self, book_id: i64) -> Result<RevisionFence> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        let current = current_revision_in(&tx, book_id)?;
        let generation:i64=tx.query_row("SELECT coalesce(max(index_generation),0)+1 FROM book_revisions WHERE book_id=?1 AND rev_no=?2",params![book_id,current.rev_no],|r|r.get(0))?;
        let inserted=tx.execute("INSERT INTO book_revisions(book_id,rev_no,index_generation,content_hash,resource_hash,managed_path,library_root,resource_manifest,state,created_at) SELECT book_id,rev_no,?4,content_hash,resource_hash,managed_path,library_root,resource_manifest,'preparing',?5 FROM book_revisions WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",params![book_id,current.rev_no,current.index_generation,generation,crate::now()])?;
        if inserted == 0 {
            return Err(Error::Integrity("当前版本缺失，不能重建索引".into()));
        }
        tx.commit()?;
        Ok(RevisionFence {
            rev_no: current.rev_no,
            index_generation: generation,
        })
    }
    pub fn register_import(&self, input: ImportRegistration<'_>) -> Result<BookRevision> {
        let (file_size, file_mtime) = file_facts(input.managed_path, input.doc.meta().size);
        let content = doc_content(input.doc);
        let metadata = index::encode_meta(input.doc.meta())?;
        let encoded = index::encode(&content, input.doc.format())?;
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        let prior: Option<i64> = tx
            .query_row(
                "SELECT id FROM books WHERE book_uuid=?1",
                [input.book_uuid],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(book_id) = prior {
            let fence = current_revision_in(&tx, book_id)?;
            let revision = revision_in(&tx, book_id, fence)?
                .ok_or_else(|| Error::Integrity("书目登记缺少版本".into()))?;
            if revision.content_hash == input.content_hash
                && revision.resource_hash == input.resource_hash
                && Path::new(&revision.managed_path) == input.managed_path
            {
                return Ok(revision);
            }
            return Err(Error::Integrity("重复登记请求与已有书目不一致".into()));
        }
        if other_owner_in(&tx, input.content_hash, None)? {
            return Err(Error::Integrity("该内容已属于已有书目，请重新判重".into()));
        }
        if let Some(locator) = input.source_locator {
            let occupied: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM book_sources WHERE locator=?1)",
                [locator],
                |r| r.get(0),
            )?;
            if occupied {
                return Err(Error::Integrity("来源已经登记，必须走同书更新".into()));
            }
        }
        let meta = input.doc.meta();
        let path = input.managed_path.to_string_lossy();
        tx.execute("INSERT INTO books(id,path,title,authors,format,size,added_at,book_uuid,active_rev_no,active_index_generation,content_hash,hash_algo) VALUES((SELECT max(high_water,coalesce((SELECT max(id) FROM books),0))+1 FROM book_id_clock WHERE singleton=1),?1,?2,?3,?4,?5,?6,?7,1,?8,?9,'sha256')",params![path,meta.title,meta.authors.join("\n"),meta.format.as_str(),meta.size as i64,crate::now(),input.book_uuid,folio_format::INDEX_VERSION,input.content_hash])?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE books SET managed_root=?2 WHERE id=?1",
            params![id, input.library_root.to_string_lossy()],
        )?;
        write_metadata_in(&tx, id, meta)?;
        tx.execute(
            "UPDATE books SET size=?2,mtime=?3 WHERE id=?1",
            params![id, file_size as i64, file_mtime],
        )?;
        let source = if let Some(locator) = input.source_locator {
            let source = crate::sources::bind_in(&tx, id, locator, input.source_kind, true)?;
            tx.execute(
                "UPDATE book_sources SET fingerprint=?2 WHERE id=?1",
                params![source.id, input.source_fingerprint],
            )?;
            Some(source.id)
        } else {
            None
        };
        let fence = RevisionFence {
            rev_no: 1,
            index_generation: folio_format::INDEX_VERSION,
        };
        tx.execute("INSERT INTO book_revisions(book_id,rev_no,index_generation,content_hash,resource_hash,managed_path,resource_manifest,metadata_json,index_json,state,source_id,created_at) VALUES(?1,1,?2,?3,?4,?5,?6,?7,?8,'active',?9,?10)",params![id,fence.index_generation,input.content_hash,input.resource_hash,path,input.resource_manifest,metadata,encoded,source,crate::now()])?;
        tx.execute(
            "UPDATE book_revisions SET library_root=?2 WHERE book_id=?1",
            params![id, input.library_root.to_string_lossy()],
        )?;
        tx.execute(
            "UPDATE book_revisions SET file_size=?2,file_mtime=?3 WHERE book_id=?1",
            params![id, file_size as i64, file_mtime],
        )?;
        Self::save_content_in(&tx, id, &content, meta.format)?;
        let revision = revision_in(&tx, id, fence)?.unwrap();
        tx.commit()?;
        Ok(revision)
    }
    pub fn ready_revision(&self, input: RevisionReady<'_>) -> Result<BookRevision> {
        self.ready_revision_in_transaction(input, false)
    }

    /// 用户选择一次性替代文件后，暂停旧主来源；失败不会改变其跟踪状态。
    pub fn ready_replacement_revision(&self, input: RevisionReady<'_>) -> Result<BookRevision> {
        if input.source_id.is_some() || input.expected_active.is_none() {
            return Err(Error::Integrity(
                "一次性替换必须指定预期版本且没有持久来源".into(),
            ));
        }
        self.ready_revision_in_transaction(input, true)
    }

    fn ready_revision_in_transaction(
        &self,
        input: RevisionReady<'_>,
        pause_source: bool,
    ) -> Result<BookRevision> {
        let (file_size, file_mtime) = file_facts(input.managed_path, input.doc.meta().size);
        let content = doc_content(input.doc);
        let metadata = index::encode_meta(input.doc.meta())?;
        let encoded = index::encode(&content, input.doc.format())?;
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        if other_owner_in(&tx, input.content_hash, Some(input.book_id))? {
            return Err(Error::Integrity("目标内容已属于另一书目".into()));
        }
        if let Some(expected) = input.expected_active {
            check_revision_in(&tx, input.book_id, expected)?;
        }
        if let Some(source_id) = input.source_id {
            let source = crate::sources::source_in(&tx, source_id)?;
            if source.book_id != input.book_id
                || source.generation != input.source_generation
                || matches!(source.state.as_str(), "suppressed" | "relink-required")
            {
                return Err(Error::Integrity("来源观察代已变化".into()));
            }
        }
        if let Some(existing) = revision_in(&tx, input.book_id, input.fence)?
            && existing.state == "ready"
        {
            let prior_index:String=tx.query_row("SELECT index_json FROM book_revisions WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",params![input.book_id,input.fence.rev_no,input.fence.index_generation],|r|r.get(0))?;
            if existing.content_hash == input.content_hash
                && existing.resource_hash == input.resource_hash
                && Path::new(&existing.managed_path) == input.managed_path
                && Path::new(&existing.library_root) == input.library_root
                && prior_index == encoded
            {
                return Ok(existing);
            }
            return Err(Error::Integrity("已就绪的阅读版本不可改写".into()));
        }
        let changed=tx.execute("UPDATE book_revisions SET content_hash=?4,resource_hash=?5,managed_path=?6,resource_manifest=?7,metadata_json=?8,index_json=?9,state='ready',source_id=?10,source_generation=?11 WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3 AND state='preparing'",params![input.book_id,input.fence.rev_no,input.fence.index_generation,input.content_hash,input.resource_hash,input.managed_path.to_string_lossy(),input.resource_manifest,metadata,encoded,input.source_id,input.source_generation])?;
        if changed == 0 {
            return Err(Error::Integrity("候选版本未预留或已采用".into()));
        }
        let revision = revision_in(&tx, input.book_id, input.fence)?.unwrap();
        tx.execute("UPDATE book_revisions SET library_root=?4 WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",params![input.book_id,input.fence.rev_no,input.fence.index_generation,input.library_root.to_string_lossy()])?;
        if pause_source {
            tx.execute("UPDATE book_sources SET state='relink-required',generation=generation+1 WHERE book_id=?1 AND is_primary=1 AND state<>'relink-required'",[input.book_id])?;
        }
        let mut revision = revision;
        revision.library_root = input.library_root.to_string_lossy().into_owned();
        tx.execute("UPDATE book_revisions SET file_size=?4,file_mtime=?5 WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",params![input.book_id,input.fence.rev_no,input.fence.index_generation,file_size as i64,file_mtime])?;
        tx.commit()?;
        Ok(revision)
    }
    pub fn adopt_revision(
        &self,
        book_id: i64,
        target: RevisionFence,
        expected: Option<RevisionFence>,
        allow_leased: bool,
    ) -> Result<AdoptResult> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        let current = current_revision_in(&tx, book_id)?;
        if target == current {
            return Ok(AdoptResult::AlreadyActive);
        }
        if target.rev_no < current.rev_no
            || (target.rev_no == current.rev_no
                && target.index_generation < current.index_generation)
        {
            return Ok(AdoptResult::Stale);
        }
        if expected.is_some_and(|e| e != current) {
            return Ok(AdoptResult::Stale);
        }
        if !allow_leased
            && self
                .revision_leases
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .keys()
                .any(|(id, _)| *id == book_id)
        {
            return Ok(AdoptResult::ReadersActive);
        }
        let Some(revision) = revision_in(&tx, book_id, target)? else {
            return Ok(AdoptResult::Stale);
        };
        if other_owner_in(&tx, &revision.content_hash, Some(book_id))? {
            return Ok(AdoptResult::Stale);
        }
        if revision.state != "ready" {
            return Ok(AdoptResult::Stale);
        }
        if let Some(source_id) = revision.source_id {
            let source = crate::sources::source_in(&tx, source_id)?;
            if source.generation != revision.source_generation
                || !source.is_primary
                || matches!(source.state.as_str(), "suppressed" | "relink-required")
            {
                return Ok(AdoptResult::Stale);
            }
        }
        legacy_index_in(&tx, book_id)?;
        let snapshot = snapshot_in(&tx, book_id, target)?;
        crate::record_revisions::adopt_position_in(&tx, book_id, current, target)?;
        tx.execute("UPDATE book_revisions SET state='retained' WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",params![book_id,current.rev_no,current.index_generation])?;
        tx.execute("UPDATE book_revisions SET state='active' WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",params![book_id,target.rev_no,target.index_generation])?;
        tx.execute("UPDATE books SET path=?2,content_hash=?3,hash_algo='sha256',format=?4,size=?5,active_rev_no=?6,active_index_generation=?7,cover_hash=NULL,cover_mime=NULL WHERE id=?1",params![book_id,revision.managed_path,revision.content_hash,snapshot.meta.format.as_str(),snapshot.meta.size as i64,target.rev_no,target.index_generation])?;
        Self::save_content_in(&tx, book_id, &snapshot.content, snapshot.meta.format)?;
        tx.execute(
            "UPDATE books SET managed_root=?2 WHERE id=?1",
            params![book_id, revision.library_root],
        )?;
        write_metadata_in(&tx, book_id, &snapshot.meta)?;
        tx.execute("UPDATE books SET size=(SELECT file_size FROM book_revisions WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3),mtime=(SELECT file_mtime FROM book_revisions WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3) WHERE id=?1",params![book_id,target.rev_no,target.index_generation])?;
        tx.commit()?;
        Ok(AdoptResult::Adopted)
    }
}

#[cfg(test)]
#[path = "revisions/audit_tests.rs"]
mod audit_tests;
#[cfg(test)]
#[path = "revisions/tests.rs"]
mod tests;
