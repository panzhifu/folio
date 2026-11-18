use rusqlite::{Connection, OptionalExtension, params};

use crate::{Error, Result};
use folio_model::TextAnchor;
use folio_model::position::Locator;
use folio_model::reading::BookSettings;
use folio_model::sync::{AnnotationRecord, BookmarkRecord, PositionRecord, SettingsRecord};

pub(crate) fn next_version(observed: i64, now: i64) -> Result<i64> {
    Ok(now.max(
        observed
            .checked_add(1)
            .ok_or_else(|| Error::Integrity("同步版本已达上限".into()))?,
    ))
}

pub(crate) fn tombstone_at(conn: &Connection, kind: &str, uid: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT deleted_at FROM sync_tombstones WHERE kind=?1 AND uid=?2",
            params![kind, uid],
            |r| r.get(0),
        )
        .optional()?)
}

pub(crate) fn next_mark_version(
    conn: &Connection,
    kind: &str,
    uid: &str,
    observed: i64,
    now: i64,
) -> Result<i64> {
    let active = if kind == "annotation" {
        annotation(conn, uid)?.map(|(_, r)| r.updated_at)
    } else {
        bookmark(conn, uid)?.map(|(_, r)| r.updated_at)
    };
    next_version(
        observed
            .max(active.unwrap_or(0))
            .max(tombstone_at(conn, kind, uid)?.unwrap_or(0)),
        now,
    )
}

pub(crate) fn position(conn: &Connection, id: i64) -> Result<Option<PositionRecord>> {
    Ok(conn
        .query_row(
            "SELECT mode, locator_json, updated_at FROM positions WHERE book_id=?1",
            [id],
            |r| {
                Ok(PositionRecord {
                    mode: r.get(0)?,
                    locator: Locator::from_json(&r.get::<_, String>(1)?),
                    updated_at: r.get(2)?,
                })
            },
        )
        .optional()?)
}

pub(crate) fn settings(conn: &Connection, id: i64) -> Result<Option<SettingsRecord>> {
    Ok(conn
        .query_row(
            "SELECT settings_json, settings_updated_at FROM books WHERE id=?1",
            [id],
            |r| {
                Ok(SettingsRecord {
                    settings: BookSettings::from_column(&r.get::<_, String>(0)?),
                    updated_at: r.get(1)?,
                })
            },
        )
        .optional()?)
}

pub(crate) fn annotation(conn: &Connection, uid: &str) -> Result<Option<(i64, AnnotationRecord)>> {
    let row: Option<(i64,String,String,String,String,i64,i64)> = conn.query_row("SELECT book_id,anchor_json,note,color,style,created_at,updated_at FROM annotations WHERE uid=?1", [uid], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    row.map(|(id, json, note, color, style, created_at, updated_at)| {
        Ok((
            id,
            AnnotationRecord {
                uid: uid.into(),
                anchor: TextAnchor::from_json(&json)
                    .ok_or_else(|| Error::Integrity("同步批注锚点损坏".into()))?,
                note,
                color,
                style,
                created_at,
                updated_at,
            },
        ))
    })
    .transpose()
}

pub(crate) fn bookmark(conn: &Connection, uid: &str) -> Result<Option<(i64, BookmarkRecord)>> {
    let row: Option<(i64, String, i64, i64)> = conn
        .query_row(
            "SELECT book_id,anchor_json,created_at,updated_at FROM bookmarks WHERE uid=?1",
            [uid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    row.map(|(id, json, created_at, updated_at)| {
        Ok((
            id,
            BookmarkRecord {
                uid: uid.into(),
                anchor: TextAnchor::from_json(&json)
                    .ok_or_else(|| Error::Integrity("同步书签锚点损坏".into()))?,
                created_at,
                updated_at,
            },
        ))
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use crate::sync_order::next_version;
    #[test]
    fn logical_version_survives_equal_seconds_and_clock_rollback() {
        assert_eq!(next_version(20, 20).unwrap(), 21);
        assert_eq!(next_version(20, 10).unwrap(), 21);
        assert_eq!(next_version(20, 40).unwrap(), 40);
        assert!(next_version(i64::MAX, 40).is_err());
    }
}
