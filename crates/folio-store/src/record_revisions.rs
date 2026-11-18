//! 阅读记录保留建锚版本；显示投影不改变同步原桶。

use std::collections::HashSet;

use folio_model::revision::RevisionFence;
use folio_model::sync::PositionRecord;
use folio_model::types::{Annotation, AnnotationStyle, Bookmark};
use folio_model::{Locator, TextAnchor};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Error, Result, Store};

pub(crate) fn bind_origin_in(
    conn: &Connection,
    kind: &str,
    uid: &str,
    book_id: i64,
    fence: RevisionFence,
    hash: &str,
) -> Result<()> {
    let old: Option<(i64, String)> = conn
        .query_row(
            "SELECT book_id,content_hash FROM record_origins WHERE kind=?1 AND uid=?2",
            params![kind, uid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if old.as_ref().is_some_and(|(owner, original)| {
        *owner != book_id || (!original.is_empty() && !hash.is_empty() && original != hash)
    }) {
        return Err(Error::Integrity(
            "记录的建锚版本不能跨书或跨内容改写".into(),
        ));
    }
    conn.execute(
        "INSERT INTO record_origins(kind,uid,book_id,rev_no,index_generation,content_hash)
         VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(kind,uid) DO UPDATE SET
         content_hash=CASE WHEN record_origins.content_hash='' THEN excluded.content_hash ELSE record_origins.content_hash END",
        params![kind,uid,book_id,fence.rev_no,fence.index_generation,hash],
    )?;
    Ok(())
}

pub(crate) fn active_hash_in(conn: &Connection, book_id: i64) -> Result<String> {
    Ok(conn.query_row(
        "SELECT coalesce(content_hash,'') FROM books WHERE id=?1",
        [book_id],
        |r| r.get(0),
    )?)
}

pub(crate) fn remember_origin_in(
    conn: &Connection,
    kind: &str,
    uid: &str,
    book_id: i64,
) -> Result<()> {
    let fence = crate::revisions::current_revision_in(conn, book_id)?;
    bind_origin_in(
        conn,
        kind,
        uid,
        book_id,
        fence,
        &active_hash_in(conn, book_id)?,
    )
}

pub(crate) fn restore_origin_in(
    conn: &Connection,
    kind: &str,
    uid: &str,
    book_id: i64,
) -> Result<()> {
    let owner: Option<i64> = conn
        .query_row(
            "SELECT book_id FROM record_origins WHERE kind=?1 AND uid=?2",
            params![kind, uid],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(owner) = owner {
        return if owner == book_id {
            Ok(())
        } else {
            Err(Error::Integrity("撤销记录的原书目不一致".into()))
        };
    }
    let hash: Option<String> = conn
        .query_row(
            "SELECT book_hash FROM sync_tombstones WHERE kind=?1 AND uid=?2",
            params![kind, uid],
            |r| r.get(0),
        )
        .optional()?;
    let fence = crate::revisions::current_revision_in(conn, book_id)?;
    bind_origin_in(
        conn,
        kind,
        uid,
        book_id,
        fence,
        &hash.unwrap_or(active_hash_in(conn, book_id)?),
    )
}

/// 撤销只能恢复同一逻辑记录；不能以整数行号冲突为由覆盖另一条笔记。
pub(crate) fn check_restore_identity_in(
    conn: &Connection,
    kind: &str,
    id: i64,
    uid: &str,
    book_id: i64,
) -> Result<()> {
    let table = match kind {
        "annotation" => "annotations",
        "bookmark" => "bookmarks",
        _ => return Err(Error::Integrity("未知撤销记录种类".into())),
    };
    let conflict: bool = conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE (id=?1 AND (coalesce(uid,'')<>?2 OR book_id<>?3)) OR (uid=?2 AND book_id<>?3))"),
        params![id, uid, book_id],
        |row| row.get(0),
    )?;
    if conflict {
        return Err(Error::Integrity("撤销记录与已有记录身份冲突".into()));
    }
    Ok(())
}

/// 切换前补齐尚未经过新写入口的兼容数据，只采用原登记哈希。
pub(crate) fn adopt_position_in(
    conn: &Connection,
    book_id: i64,
    previous: RevisionFence,
    next: RevisionFence,
) -> Result<()> {
    let hash = active_hash_in(conn, book_id)?;
    for (kind, table) in [("annotation", "annotations"), ("bookmark", "bookmarks")] {
        conn.execute(&format!(
            "INSERT OR IGNORE INTO record_origins(kind,uid,book_id,rev_no,index_generation,content_hash)
             SELECT ?1,uid,book_id,?3,?4,?5 FROM {table} WHERE book_id=?2 AND uid IS NOT NULL"
        ),params![kind,book_id,previous.rev_no,previous.index_generation,hash])?;
    }
    conn.execute(
        "UPDATE record_origins SET content_hash=?4 WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3 AND content_hash=''",
        params![book_id,previous.rev_no,previous.index_generation,hash],
    )?;
    conn.execute(
        "INSERT INTO revision_positions(book_id,rev_no,index_generation,mode,locator_json,updated_at)
         SELECT book_id,?2,?3,mode,locator_json,updated_at FROM positions WHERE book_id=?1
         ON CONFLICT(book_id,rev_no,index_generation) DO UPDATE SET mode=excluded.mode,locator_json=excluded.locator_json,updated_at=excluded.updated_at",
        params![book_id,previous.rev_no,previous.index_generation],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO revision_chapter_read(book_id,rev_no,index_generation,idx,read_at)
         SELECT book_id,?2,?3,idx,read_at FROM chapter_read WHERE book_id=?1",
        params![book_id, previous.rev_no, previous.index_generation],
    )?;
    let same_coordinates: bool = conn.query_row(
        "SELECT n.content_hash=?4 AND n.resource_hash=p.resource_hash AND n.index_json=p.index_json
         AND coalesce(json_extract(n.metadata_json,'$.format'),'') NOT IN ('Text','Markdown')
         FROM book_revisions n JOIN book_revisions p ON p.book_id=n.book_id AND p.rev_no=?2 AND p.index_generation=?5
         WHERE n.book_id=?1 AND n.rev_no=?3 AND n.index_generation=?6",
        params![book_id,previous.rev_no,next.rev_no,hash,previous.index_generation,next.index_generation],
        |r| r.get(0),
    )?;
    if same_coordinates {
        conn.execute(
            "INSERT OR IGNORE INTO revision_positions(book_id,rev_no,index_generation,mode,locator_json,updated_at)
             SELECT book_id,?4,?5,mode,locator_json,updated_at FROM revision_positions WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",
            params![book_id,previous.rev_no,previous.index_generation,next.rev_no,next.index_generation],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO revision_chapter_read(book_id,rev_no,index_generation,idx,read_at)
             SELECT book_id,?4,?5,idx,read_at FROM revision_chapter_read WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",
            params![book_id,previous.rev_no,previous.index_generation,next.rev_no,next.index_generation],
        )?;
        for (kind, table) in [("annotation", "annotations"), ("bookmark", "bookmarks")] {
            conn.execute(&format!(
                "INSERT OR IGNORE INTO anchor_projections(kind,uid,book_id,rev_no,index_generation,anchor_json,status)
                 SELECT ?1,a.uid,a.book_id,?3,?4,a.anchor_json,'located' FROM {table} a JOIN record_origins o ON o.kind=?1 AND o.uid=a.uid
                 WHERE a.book_id=?2 AND o.rev_no=?5 AND o.index_generation=?6"
            ),params![kind,book_id,next.rev_no,next.index_generation,previous.rev_no,previous.index_generation])?;
        }
    }
    conn.execute("DELETE FROM positions WHERE book_id=?1", [book_id])?;
    conn.execute(
        "INSERT INTO positions(book_id,mode,locator_json,updated_at)
         SELECT book_id,mode,locator_json,updated_at FROM revision_positions WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",
        params![book_id,next.rev_no,next.index_generation],
    )?;
    conn.execute("DELETE FROM chapter_read WHERE book_id=?1", [book_id])?;
    conn.execute(
        "INSERT INTO chapter_read(book_id,idx,read_at)
         SELECT book_id,idx,read_at FROM revision_chapter_read WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",
        params![book_id,next.rev_no,next.index_generation],
    )?;
    Ok(())
}

pub(crate) fn position_in(
    conn: &Connection,
    book_id: i64,
    fence: RevisionFence,
) -> Result<Option<PositionRecord>> {
    Ok(conn.query_row(
        "SELECT mode,locator_json,updated_at FROM revision_positions WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",
        params![book_id,fence.rev_no,fence.index_generation],
        |r| Ok(PositionRecord {mode:r.get(0)?,locator:Locator::from_json(&r.get::<_,String>(1)?),updated_at:r.get(2)?}),
    ).optional()?)
}

pub(crate) fn write_position_in(
    conn: &Connection,
    book_id: i64,
    fence: RevisionFence,
    record: &PositionRecord,
) -> Result<()> {
    conn.execute(
        "INSERT INTO revision_positions(book_id,rev_no,index_generation,mode,locator_json,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(book_id,rev_no,index_generation) DO UPDATE SET
         mode=excluded.mode,locator_json=excluded.locator_json,updated_at=excluded.updated_at",
        params![book_id,fence.rev_no,fence.index_generation,record.mode,record.locator.to_json(),record.updated_at],
    )?;
    if crate::revisions::current_revision_in(conn, book_id)? == fence {
        conn.execute(
            "INSERT INTO positions(book_id,mode,locator_json,updated_at) VALUES (?1,?2,?3,?4)
             ON CONFLICT(book_id) DO UPDATE SET mode=excluded.mode,locator_json=excluded.locator_json,updated_at=excluded.updated_at",
            params![book_id,record.mode,record.locator.to_json(),record.updated_at],
        )?;
    }
    Ok(())
}

fn display_anchor(
    conn: &Connection,
    kind: &str,
    uid: &str,
    fence: Option<RevisionFence>,
    original: &str,
) -> Result<Option<TextAnchor>> {
    let Some(target) = fence else {
        return Ok(TextAnchor::from_json(original));
    };
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
    if origin.unwrap_or(RevisionFence {
        rev_no: 0,
        index_generation: 0,
    }) == target
    {
        return Ok(TextAnchor::from_json(original));
    }
    let projected: Option<Option<String>> = conn.query_row(
        "SELECT anchor_json FROM anchor_projections WHERE kind=?1 AND uid=?2 AND rev_no=?3 AND index_generation=?4 AND status='located'",
        params![kind,uid,target.rev_no,target.index_generation], |r| r.get(0),
    ).optional()?;
    Ok(projected
        .flatten()
        .as_deref()
        .and_then(TextAnchor::from_json))
}

pub(crate) fn annotations_in(
    conn: &Connection,
    book_id: i64,
    fence: Option<RevisionFence>,
) -> Result<Vec<Annotation>> {
    let mut statement = conn.prepare("SELECT id,coalesce(uid,''),anchor_json,note,color,style,stale,created_at,updated_at FROM annotations WHERE book_id=?1 ORDER BY created_at,id")?;
    let rows = statement.query_map([book_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, bool>(6)?,
            r.get::<_, i64>(7)?,
            r.get::<_, i64>(8)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, uid, json, note, color, style, stale, created_at, updated_at) = row?;
        if let Some(anchor) = display_anchor(conn, "annotation", &uid, fence, &json)? {
            out.push(Annotation {
                id,
                uid,
                book_id,
                chapter: anchor.chapter,
                anchor,
                note,
                color,
                style: AnnotationStyle::parse(&style),
                stale,
                created_at,
                updated_at,
            });
        }
    }
    Ok(out)
}

pub(crate) fn bookmarks_in(
    conn: &Connection,
    book_id: i64,
    fence: Option<RevisionFence>,
) -> Result<Vec<Bookmark>> {
    let mut statement=conn.prepare("SELECT id,coalesce(uid,''),anchor_json,created_at,updated_at FROM bookmarks WHERE book_id=?1 ORDER BY created_at,id")?;
    let rows = statement.query_map([book_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, uid, json, created_at, updated_at) = row?;
        if let Some(anchor) = display_anchor(conn, "bookmark", &uid, fence, &json)? {
            out.push(Bookmark {
                id,
                uid,
                book_id,
                chapter: anchor.chapter,
                anchor,
                created_at,
                updated_at,
            });
        }
    }
    Ok(out)
}

impl Store {
    pub fn record_origin(&self, kind: &str, uid: &str) -> Result<Option<(RevisionFence, String)>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(conn.query_row("SELECT rev_no,index_generation,content_hash FROM record_origins WHERE kind=?1 AND uid=?2",params![kind,uid],|r|Ok((RevisionFence{rev_no:r.get(0)?,index_generation:r.get(1)?},r.get(2)?))).optional()?)
    }

    /// 进度重定位只填尚无读者位置的候选版本，不覆盖已采用或已写过的位置。
    pub fn set_position_projection(
        &self,
        book_id: i64,
        target: RevisionFence,
        mode: &str,
        locator: &Locator,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if crate::revisions::current_revision_in(&conn, book_id)? == target {
            return Ok(false);
        }
        let ready:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM book_revisions WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3 AND state='ready')",params![book_id,target.rev_no,target.index_generation],|r|r.get(0))?;
        if !ready {
            return Ok(false);
        }
        let stamp = crate::sync_order::position(&conn, book_id)?.map_or(0, |p| p.updated_at);
        Ok(conn.execute("INSERT OR IGNORE INTO revision_positions(book_id,rev_no,index_generation,mode,locator_json,updated_at) VALUES(?1,?2,?3,?4,?5,?6)",params![book_id,target.rev_no,target.index_generation,mode,locator.to_json(),stamp])?>0)
    }

    /// 解析在锁外执行；只有原锚点仍一致才接收结果，删除/远端重锚不会被旧任务复活。
    pub fn set_anchor_projection_if_current(
        &self,
        book_id: i64,
        kind: &str,
        uid: &str,
        original: &TextAnchor,
        target: RevisionFence,
        anchor: Option<&TextAnchor>,
    ) -> Result<bool> {
        let table = match kind {
            "annotation" => "annotations",
            "bookmark" => "bookmarks",
            _ => return Err(Error::Integrity("未知位置投影种类".into())),
        };
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let current: Option<String> = conn
            .query_row(
                &format!("SELECT anchor_json FROM {table} WHERE book_id=?1 AND uid=?2"),
                params![book_id, uid],
                |r| r.get(0),
            )
            .optional()?;
        if current.as_deref().and_then(TextAnchor::from_json).as_ref() != Some(original) {
            return Ok(false);
        }
        let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM book_revisions WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3 AND state IN ('ready','active','retained'))",params![book_id,target.rev_no,target.index_generation],|r|r.get(0))?;
        if !exists {
            return Ok(false);
        }
        conn.execute("INSERT INTO anchor_projections(kind,uid,book_id,rev_no,index_generation,anchor_json,status) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(kind,uid,rev_no,index_generation) DO UPDATE SET anchor_json=excluded.anchor_json,status=excluded.status",params![kind,uid,book_id,target.rev_no,target.index_generation,anchor.map(TextAnchor::to_json),if anchor.is_some(){"located"}else{"unresolved"}])?;
        Ok(true)
    }

    pub fn annotations_for_revision(
        &self,
        book_id: i64,
        fence: RevisionFence,
    ) -> Result<Vec<Annotation>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        annotations_in(&conn, book_id, Some(fence))
    }

    pub fn bookmarks_for_revision(
        &self,
        book_id: i64,
        fence: RevisionFence,
    ) -> Result<Vec<Bookmark>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        bookmarks_in(&conn, book_id, Some(fence))
    }

    /// 仅保存派生位置，不修改原锚点、记录版本或同步身份。
    pub fn set_anchor_projection(
        &self,
        book_id: i64,
        kind: &str,
        uid: &str,
        target: RevisionFence,
        anchor: Option<&TextAnchor>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(kind, "annotation" | "bookmark") {
            return Err(Error::Integrity("未知位置投影种类".into()));
        }
        let owner: Option<i64> = conn
            .query_row(
                "SELECT book_id FROM record_origins WHERE kind=?1 AND uid=?2",
                params![kind, uid],
                |r| r.get(0),
            )
            .optional()?;
        if owner != Some(book_id) {
            return Err(Error::Integrity("位置投影的书目归属不符".into()));
        }
        conn.execute(
            "INSERT INTO anchor_projections(kind,uid,book_id,rev_no,index_generation,anchor_json,status) VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(kind,uid,rev_no,index_generation) DO UPDATE SET anchor_json=excluded.anchor_json,status=excluded.status",
            params![kind,uid,book_id,target.rev_no,target.index_generation,anchor.map(TextAnchor::to_json),if anchor.is_some(){"located"}else{"unresolved"}],
        )?;
        Ok(())
    }

    pub fn position_for_revision(
        &self,
        book_id: i64,
        fence: RevisionFence,
    ) -> Result<Option<(String, Locator)>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if crate::revisions::current_revision_in(&conn, book_id)? == fence {
            return Ok(crate::sync_order::position(&conn, book_id)?.map(|p| (p.mode, p.locator)));
        }
        Ok(position_in(&conn, book_id, fence)?.map(|p| (p.mode, p.locator)))
    }

    pub fn read_chapters_for_revision(
        &self,
        book_id: i64,
        fence: RevisionFence,
    ) -> Result<HashSet<usize>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement=conn.prepare("SELECT idx FROM revision_chapter_read WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3")?;
        Ok(statement
            .query_map(
                params![book_id, fence.rev_no, fence.index_generation],
                |r| r.get::<_, i64>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(|idx| usize::try_from(idx).ok())
            .collect())
    }
}
