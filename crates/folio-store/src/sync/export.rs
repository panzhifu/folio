use std::collections::{BTreeMap, HashMap};

use folio_model::TextAnchor;
use folio_model::revision::RevisionFence;
use folio_model::sync::{
    AnnotationRecord, BookSnapshot, BookmarkRecord, LocalSnapshot, Snapshot, Tombstone,
};
use rusqlite::{Connection, params};

use crate::{Result, Store};

impl Store {
    /// 保留每条记录的原内容桶；当前书目换版不重标旧锚点。
    pub fn collect_sync(&self, device_id: &str, exported_at: i64) -> Result<LocalSnapshot> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut owners = HashMap::<String, i64>::new();
        let mut books = BTreeMap::<String, BookSnapshot>::new();
        let mut skipped_books = 0;
        let mut statement=conn.prepare("SELECT id,coalesce(content_hash,''),active_rev_no,active_index_generation FROM books ORDER BY id")?;
        let current = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    RevisionFence {
                        rev_no: r.get(2)?,
                        index_generation: r.get(3)?,
                    },
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (book_id, current_hash, current_fence) in current {
            let mut stmt=conn.prepare("SELECT content_hash,rev_no,index_generation,resource_hash FROM book_revisions WHERE book_id=?1 AND content_hash<>'' AND state IN ('active','retained','legacy-unverified') ORDER BY rev_no,index_generation")?;
            let mut versions = stmt
                .query_map([book_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        RevisionFence {
                            rev_no: r.get(1)?,
                            index_generation: r.get(2)?,
                        },
                        r.get::<_, String>(3)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if !current_hash.is_empty() && !versions.iter().any(|(_, f, _)| *f == current_fence) {
                versions.push((current_hash.clone(), current_fence, String::new()));
            }
            let mut supported = false;
            let mut deferred = false;
            for (hash, fence, resource_hash) in versions {
                if !resource_hash.is_empty() {
                    continue;
                }
                supported = true;
                if owners
                    .insert(hash.clone(), book_id)
                    .is_some_and(|owner| owner != book_id)
                {
                    return Err(crate::Error::Integrity(
                        "同内容多个书目不能合成一个同步桶".into(),
                    ));
                }
                let canonical = super::revisions::owner(&conn, &hash)?
                    .map(|(_, f)| f)
                    .unwrap_or(fence);
                if fence == current_fence
                    && !super::revisions::same_coordinates(&conn, book_id, fence, canonical)?
                {
                    deferred = true;
                }
                let book = books.entry(hash).or_default();
                let position = if fence == current_fence {
                    crate::sync_order::position(&conn, book_id)?
                } else {
                    crate::record_revisions::position_in(&conn, book_id, fence)?
                };
                if fence == canonical {
                    book.position = position;
                }
                book.settings =
                    crate::sync_order::settings(&conn, book_id)?.filter(|r| r.updated_at > 0);
            }
            deferred |= annotations(&conn, book_id, &current_hash, &owners, &mut books)?;
            deferred |= bookmarks(&conn, book_id, &current_hash, &owners, &mut books)?;
            if !supported || deferred {
                skipped_books += 1;
            }
        }
        let mut stmt = conn.prepare("SELECT kind,book_hash,uid,deleted_at FROM sync_tombstones")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (kind, hash, uid, deleted_at) = row?;
            if let (Some(kind), Some(book)) = (super::tombstone_kind(&kind), books.get_mut(&hash)) {
                book.tombstones.push(Tombstone {
                    kind,
                    uid,
                    deleted_at,
                });
            }
        }
        for book in books.values_mut() {
            book.annotations.sort_by(|a, b| a.uid.cmp(&b.uid));
            book.bookmarks.sort_by(|a, b| a.uid.cmp(&b.uid));
            book.tombstones.sort_by(super::sort_tombstones);
        }
        Ok(LocalSnapshot {
            snapshot: Snapshot {
                version: folio_model::sync::SNAPSHOT_VERSION,
                device_id: device_id.into(),
                exported_at,
                books,
            },
            skipped_books,
        })
    }
}

fn annotations(
    conn: &Connection,
    book_id: i64,
    current_hash: &str,
    owners: &HashMap<String, i64>,
    books: &mut BTreeMap<String, BookSnapshot>,
) -> Result<bool> {
    let mut deferred = false;
    let mut stmt=conn.prepare("SELECT a.uid,a.anchor_json,a.note,a.color,a.style,a.created_at,a.updated_at,CASE WHEN o.uid IS NULL THEN ?2 ELSE o.content_hash END FROM annotations a LEFT JOIN record_origins o ON o.kind='annotation' AND o.uid=a.uid WHERE a.book_id=?1")?;
    let rows = stmt.query_map(params![book_id, current_hash], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, String>(7)?,
        ))
    })?;
    for row in rows {
        let (uid, json, note, color, style, created_at, updated_at, hash) = row?;
        let (Some(uid), Some(anchor)) =
            (uid.filter(|s| !s.is_empty()), TextAnchor::from_json(&json))
        else {
            deferred = true;
            continue;
        };
        if owners.get(&hash) == Some(&book_id)
            && super::revisions::record_exportable(conn, "annotation", &uid, book_id, &hash)?
        {
            books
                .entry(hash)
                .or_default()
                .annotations
                .push(AnnotationRecord {
                    uid,
                    anchor,
                    note,
                    color,
                    style,
                    created_at,
                    updated_at,
                });
        } else {
            deferred = true;
        }
    }
    Ok(deferred)
}

fn bookmarks(
    conn: &Connection,
    book_id: i64,
    current_hash: &str,
    owners: &HashMap<String, i64>,
    books: &mut BTreeMap<String, BookSnapshot>,
) -> Result<bool> {
    let mut deferred = false;
    let mut stmt=conn.prepare("SELECT a.uid,a.anchor_json,a.created_at,a.updated_at,CASE WHEN o.uid IS NULL THEN ?2 ELSE o.content_hash END FROM bookmarks a LEFT JOIN record_origins o ON o.kind='bookmark' AND o.uid=a.uid WHERE a.book_id=?1")?;
    let rows = stmt.query_map(params![book_id, current_hash], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (uid, json, created_at, updated_at, hash) = row?;
        let (Some(uid), Some(anchor)) =
            (uid.filter(|s| !s.is_empty()), TextAnchor::from_json(&json))
        else {
            deferred = true;
            continue;
        };
        if owners.get(&hash) == Some(&book_id)
            && super::revisions::record_exportable(conn, "bookmark", &uid, book_id, &hash)?
        {
            books
                .entry(hash)
                .or_default()
                .bookmarks
                .push(BookmarkRecord {
                    uid,
                    anchor,
                    created_at,
                    updated_at,
                });
        } else {
            deferred = true;
        }
    }
    Ok(deferred)
}
