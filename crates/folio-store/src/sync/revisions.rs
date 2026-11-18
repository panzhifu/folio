//! v1按真实内容哈希收发；历史版本保持自己的坐标和墓碑。

use folio_model::revision::RevisionFence;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Error, Result};

pub(super) fn same_coordinates(
    conn: &Connection,
    book_id: i64,
    left: RevisionFence,
    right: RevisionFence,
) -> Result<bool> {
    if left == right {
        return Ok(true);
    }
    Ok(conn.query_row(
        "SELECT l.content_hash=r.content_hash AND l.resource_hash='' AND r.resource_hash='' AND l.index_json<>'' AND l.index_json=r.index_json
         AND coalesce(json_extract(l.metadata_json,'$.format'),'') NOT IN ('Text','Markdown')
         FROM book_revisions l JOIN book_revisions r ON r.book_id=l.book_id
         WHERE l.book_id=?1 AND l.rev_no=?2 AND l.index_generation=?3 AND r.rev_no=?4 AND r.index_generation=?5",
        params![book_id,left.rev_no,left.index_generation,right.rev_no,right.index_generation],|r|r.get(0),
    ).optional()?.unwrap_or(false))
}

pub(super) fn record_exportable(
    conn: &Connection,
    kind: &str,
    uid: &str,
    book_id: i64,
    hash: &str,
) -> Result<bool> {
    let Some((owner_id, canonical)) = owner(conn, hash)? else {
        return Ok(false);
    };
    if owner_id != book_id {
        return Ok(false);
    }
    let origin: Option<RevisionFence> = conn
        .query_row(
            "SELECT rev_no,index_generation FROM record_origins WHERE kind=?1 AND uid=?2",
            params![kind, uid],
            |r| {
                Ok(RevisionFence {
                    rev_no: r.get(0)?,
                    index_generation: r.get(1)?,
                })
            },
        )
        .optional()?;
    match origin {
        Some(origin) => same_coordinates(conn, book_id, origin, canonical),
        None => Ok(true),
    }
}

pub(super) fn owner(conn: &Connection, hash: &str) -> Result<Option<(i64, RevisionFence)>> {
    let mut stmt=conn.prepare(
        "SELECT id,active_rev_no,active_index_generation FROM books b WHERE content_hash=?1
         AND NOT EXISTS(SELECT 1 FROM book_revisions r WHERE r.book_id=b.id AND r.rev_no=b.active_rev_no AND r.index_generation=b.active_index_generation AND r.resource_hash<>'')
         UNION SELECT book_id,rev_no,index_generation FROM book_revisions WHERE content_hash=?1 AND resource_hash='' AND state IN ('active','retained','legacy-unverified') ORDER BY 1,2,3"
    )?;
    let rows = stmt
        .query_map([hash], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                RevisionFence {
                    rev_no: r.get(1)?,
                    index_generation: r.get(2)?,
                },
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let Some(first) = rows.first().copied() else {
        return Ok(None);
    };
    if rows.iter().any(|row| row.0 != first.0) {
        return Err(Error::Integrity(
            "同内容对应多个书目，无法安全应用同步".into(),
        ));
    }
    Ok(Some(first))
}

pub(super) fn validate_origin(
    conn: &Connection,
    kind: &str,
    uid: &str,
    book_id: i64,
    hash: &str,
) -> Result<()> {
    let old: Option<(i64, String)> = conn
        .query_row(
            "SELECT book_id,content_hash FROM record_origins WHERE kind=?1 AND uid=?2",
            params![kind, uid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if old.is_some_and(|(id, origin)| id != book_id || (!origin.is_empty() && origin != hash)) {
        return Err(Error::Integrity("同步记录跨书或跨建锚内容冲突".into()));
    }
    let tombstone: Option<String> = conn
        .query_row(
            "SELECT book_hash FROM sync_tombstones WHERE kind=?1 AND uid=?2",
            params![kind, uid],
            |r| r.get(0),
        )
        .optional()?;
    if tombstone.is_some_and(|old| old != hash) {
        return Err(Error::Integrity("同步墓碑的内容归属冲突".into()));
    }
    Ok(())
}
