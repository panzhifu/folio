//! 同步用的库读写：把本机状态收成快照、把合并结果写回。
//!
//! SQL 全在这里，`crate::sync` 只做纯数据的合并 —— 于是合并规则可以在没有库、没有网络
//! 的条件下单测。两边共享的只有快照类型。

use std::collections::{BTreeMap, HashMap};

use rusqlite::{OptionalExtension, params};

use super::annotations::color_of;
use super::{AnnotationStyle, Store, UploadBook};
use crate::Result;
use crate::sync_order;
use folio_model::TextAnchor;
use folio_model::position::Locator;
use folio_model::reading::BookSettings;
use folio_model::sync::{
    AnnotationRecord, BookEntry, BookSnapshot, BookmarkRecord, LocalSnapshot, Plan, PositionRecord,
    SNAPSHOT_VERSION, SettingsRecord, Snapshot, SyncOrder, Tombstone, TombstoneKind,
};

impl Store {
    /// 本机全部可同步的书。没有 `content_hash` 的书进不来 —— 那是跨设备认书的唯一钥匙；
    /// 数量记在 [`LocalSnapshot::skipped_books`] 里，设置页要说得出「有几本没同步」。
    ///
    /// 一次持锁；坏锚点 / 坏 JSON 的行走「跳过一条」而不是让整趟同步失败，与各个列表屏
    /// 同一条口径。
    pub fn collect_sync(&self, device_id: &str, exported_at: i64) -> Result<LocalSnapshot> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let skipped_books: i64 = conn.query_row(
            "SELECT count(*) FROM books WHERE content_hash IS NULL OR content_hash = ''",
            [],
            |r| r.get(0),
        )?;

        let mut tombstones: HashMap<String, Vec<Tombstone>> = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT kind, book_hash, uid, deleted_at FROM sync_tombstones")?;
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
                let Some(kind) = tombstone_kind(&kind) else {
                    tracing::warn!("store: 认不出的同步墓碑类型 {kind}，跳过");
                    continue;
                };
                tombstones.entry(hash).or_default().push(Tombstone {
                    kind,
                    uid,
                    deleted_at,
                });
            }
        }

        let book_rows: Vec<(i64, String)> = {
            let mut stmt = conn.prepare(
                "SELECT id, content_hash FROM books
                 WHERE content_hash IS NOT NULL AND content_hash <> ''",
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };

        let mut books = BTreeMap::new();
        for (book_id, hash) in book_rows {
            let mut book = BookSnapshot::default();

            if let Some((mode, json, updated_at)) = conn
                .query_row(
                    "SELECT mode, locator_json, updated_at FROM positions WHERE book_id = ?1",
                    params![book_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get(2)?)),
                )
                .optional()?
            {
                book.position = Some(PositionRecord {
                    mode,
                    locator: Locator::from_json(&json),
                    updated_at,
                });
            }

            // 这一本的阅读设置差量。从来没写过（时间戳 0）就不带这一格 —— 它在快照里
            // 没话说；写过之后哪怕差量已经空了也要带，那说的是「这一本恢复过默认」，
            // 不带的另一台设备会拿着旧差量继续赢。
            let written: Option<(String, i64)> = conn
                .query_row(
                    "SELECT settings_json, settings_updated_at FROM books WHERE id = ?1",
                    params![book_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get(1)?)),
                )
                .optional()?
                .filter(|(_, at)| *at > 0);
            if let Some((json, updated_at)) = written {
                book.settings = Some(SettingsRecord {
                    updated_at,
                    settings: BookSettings::from_column(&json),
                });
            }

            {
                let mut stmt = conn.prepare(
                    "SELECT uid, anchor_json, note, color, style, created_at, updated_at
                     FROM annotations WHERE book_id = ?1",
                )?;
                let rows = stmt.query_map(params![book_id], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                })?;
                for row in rows {
                    let (uid, json, note, color, style, created_at, updated_at) = row?;
                    let Some(uid) = uid.filter(|uid| !uid.is_empty()) else {
                        continue;
                    };
                    let Some(anchor) = TextAnchor::from_json(&json) else {
                        continue;
                    };
                    book.annotations.push(AnnotationRecord {
                        uid,
                        anchor,
                        note,
                        color,
                        style,
                        created_at,
                        updated_at,
                    });
                }
            }

            {
                let mut stmt = conn.prepare(
                    "SELECT uid, anchor_json, created_at, updated_at
                     FROM bookmarks WHERE book_id = ?1",
                )?;
                let rows = stmt.query_map(params![book_id], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                })?;
                for row in rows {
                    let (uid, json, created_at, updated_at) = row?;
                    let Some(uid) = uid.filter(|uid| !uid.is_empty()) else {
                        continue;
                    };
                    let Some(anchor) = TextAnchor::from_json(&json) else {
                        continue;
                    };
                    book.bookmarks.push(BookmarkRecord {
                        uid,
                        anchor,
                        created_at,
                        updated_at,
                    });
                }
            }

            if let Some(mut list) = tombstones.remove(&hash) {
                list.sort_by(sort_tombstones);
                book.tombstones = list;
            }
            book.annotations.sort_by(|a, b| a.uid.cmp(&b.uid));
            book.bookmarks.sort_by(|a, b| a.uid.cmp(&b.uid));

            // 空记录仍表达“本机有这本书”，否则首次接收远端批注会被当作缺书跳过。
            books.insert(hash, book);
        }

        Ok(LocalSnapshot {
            snapshot: Snapshot {
                version: SNAPSHOT_VERSION,
                device_id: device_id.to_string(),
                exported_at,
                books,
            },
            skipped_books: skipped_books as usize,
        })
    }

    /// 书架多选传书：按 id 取「能上传的那几本」。
    ///
    /// 没有 `content_hash` 的行**不进来**：远端文件名就是它，缺了它传上去另一台设备
    /// 也认不回是哪一本。调用方那侧按 `ids.len() - 结果.len()` 数「跳过了几本」。
    pub fn upload_books(&self, ids: &[i64]) -> Result<Vec<UploadBook>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, content_hash, path, title, format, size FROM books
              WHERE id = ?1 AND content_hash IS NOT NULL AND content_hash <> ''",
        )?;
        let mut books = Vec::new();
        for id in ids {
            let row = stmt
                .query_row(params![id], |r| {
                    Ok(UploadBook {
                        id: r.get(0)?,
                        hash: r.get(1)?,
                        path: r.get(2)?,
                        title: r.get(3)?,
                        format: r.get(4)?,
                        size: r.get::<_, i64>(5)? as u64,
                    })
                })
                .optional()?;
            if let Some(book) = row {
                books.push(book);
            }
        }
        Ok(books)
    }

    /// 局域网同步的本机书架清单：本机**认得出身份**的那些书（`content_hash` 是对面
    /// 用来认书与当 `/file` 的键的，没有它的行递过去只会让对面白开一次连接）。
    ///
    /// 只查库、不看盘：副本此刻还在不在，由 `LanProvider::file` 那一次现问现答 —— 清单与
    /// 盘上的差别是秒级的，而这里为每一本 `metadata` 一次会把整趟拉慢，且挡不住「报完清单
    /// 的那一秒文件被移走」那一种。
    ///
    /// 大小原样交出去：`missing()` 那一侧不认识 0 字节的条目，那是「对面自己算错或被改过
    /// 报文」的信号，判据只写一处。
    pub fn lan_manifest(&self) -> Result<Vec<BookEntry>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT content_hash, size, title FROM books
              WHERE content_hash IS NOT NULL AND content_hash <> ''
              ORDER BY title, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(BookEntry {
                hash: r.get(0)?,
                size: r.get::<_, i64>(1)? as u64,
                title: r.get(2)?,
            })
        })?;
        let mut books = Vec::new();
        for row in rows {
            books.push(row?);
        }
        Ok(books)
    }

    /// 把合并结果写回本机。整批一个事务：写了一半就失败时不会留下「进度到了、
    /// 但批注没到」的半套状态。
    pub fn apply_sync(&self, plan: &Plan) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;

        for (hash, record) in &plan.positions {
            let Some(book_id) = book_id(&tx, hash)? else {
                tracing::warn!("store: 同步进度找不到本地书 {hash}，跳过");
                continue;
            };
            if sync_order::position(&tx, book_id)?
                .as_ref()
                .is_some_and(|old| !record.sync_cmp(old).is_gt())
            {
                continue;
            }
            tx.execute(
                "INSERT INTO positions (book_id, mode, locator_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(book_id) DO UPDATE SET
                   mode = excluded.mode, locator_json = excluded.locator_json,
                   updated_at = excluded.updated_at
                 WHERE excluded.updated_at >= positions.updated_at",
                params![
                    book_id,
                    record.mode,
                    record.locator.to_json(),
                    record.updated_at
                ],
            )?;
        }

        for (hash, record) in &plan.settings {
            let Some(book_id) = book_id(&tx, hash)? else {
                tracing::warn!("store: 同步阅读设置找不到本地书 {hash}，跳过");
                continue;
            };
            if sync_order::settings(&tx, book_id)?
                .as_ref()
                .is_some_and(|old| !record.sync_cmp(old).is_gt())
            {
                continue;
            }
            // 时间守卫与进度、批注那几条同一写法：更旧的那一份不该盖掉更新的。
            tx.execute(
                "UPDATE books
                    SET settings_json = ?2, settings_updated_at = ?3
                  WHERE id = ?1 AND ?3 >= settings_updated_at",
                params![book_id, record.settings.to_column(), record.updated_at],
            )?;
        }

        for (hash, record) in &plan.annotation_upserts {
            let Some(book_id) = book_id(&tx, hash)? else {
                tracing::warn!("store: 同步批注找不到本地书 {hash}，跳过");
                continue;
            };
            if sync_order::tombstone_at(&tx, "annotation", &record.uid)?
                .is_some_and(|at| at >= record.updated_at)
            {
                continue;
            }
            if let Some((owner, old)) = sync_order::annotation(&tx, &record.uid)? {
                if owner != book_id {
                    return Err(crate::Error::Integrity("同步批注跨书身份冲突".into()));
                }
                if !record.sync_cmp(&old).is_gt() {
                    continue;
                }
            }
            let color = color_of(&record.color);
            let style = AnnotationStyle::parse(&record.style);
            tx.execute(
                "INSERT INTO annotations
                 (book_id, uid, anchor_json, note, color, style, stale, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8)
                 ON CONFLICT(uid) DO UPDATE SET
                   book_id = excluded.book_id, anchor_json = excluded.anchor_json,
                   note = excluded.note, color = excluded.color, style = excluded.style,
                   created_at = excluded.created_at, updated_at = excluded.updated_at
                 WHERE excluded.updated_at >= annotations.updated_at",
                params![
                    book_id,
                    record.uid,
                    record.anchor.to_json(),
                    record.note,
                    color,
                    style.as_str(),
                    record.created_at,
                    record.updated_at
                ],
            )?;
            tx.execute(
                "DELETE FROM sync_tombstones WHERE kind = 'annotation' AND uid = ?1 AND deleted_at < ?2",
                params![record.uid, record.updated_at],
            )?;
        }

        for delete in &plan.annotation_deletes {
            let Some(book_id) = book_id(&tx, &delete.book_hash)? else {
                continue;
            };
            if sync_order::annotation(&tx, &delete.uid)?.is_some_and(|(owner, _)| owner != book_id)
            {
                return Err(crate::Error::Integrity("同步删除跨书批注身份冲突".into()));
            }
            // 本机在采集之后又改过这一条时，记录已经新过墓碑，不能删。
            tx.execute(
                "DELETE FROM annotations WHERE uid = ?1 AND updated_at <= ?2 AND book_id = ?3",
                params![delete.uid, delete.deleted_at, book_id],
            )?;
            write_tombstone(&tx, TombstoneKind::Annotation, delete)?;
        }

        for (hash, record) in &plan.bookmark_upserts {
            let Some(book_id) = book_id(&tx, hash)? else {
                tracing::warn!("store: 同步书签找不到本地书 {hash}，跳过");
                continue;
            };
            if sync_order::tombstone_at(&tx, "bookmark", &record.uid)?
                .is_some_and(|at| at >= record.updated_at)
            {
                continue;
            }
            if let Some((owner, old)) = sync_order::bookmark(&tx, &record.uid)? {
                if owner != book_id {
                    return Err(crate::Error::Integrity("同步书签跨书身份冲突".into()));
                }
                if !record.sync_cmp(&old).is_gt() {
                    continue;
                }
            }
            tx.execute(
                "INSERT INTO bookmarks (id, uid, book_id, anchor_json, created_at, updated_at)
                 VALUES (NULL, ?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(uid) DO UPDATE SET
                   book_id = excluded.book_id, anchor_json = excluded.anchor_json,
                   created_at = excluded.created_at, updated_at = excluded.updated_at
                 WHERE excluded.updated_at >= bookmarks.updated_at",
                params![
                    record.uid,
                    book_id,
                    record.anchor.to_json(),
                    record.created_at,
                    record.updated_at
                ],
            )?;
            tx.execute(
                "DELETE FROM sync_tombstones WHERE kind = 'bookmark' AND uid = ?1 AND deleted_at < ?2",
                params![record.uid, record.updated_at],
            )?;
        }

        for delete in &plan.bookmark_deletes {
            let Some(book_id) = book_id(&tx, &delete.book_hash)? else {
                continue;
            };
            if sync_order::bookmark(&tx, &delete.uid)?.is_some_and(|(owner, _)| owner != book_id) {
                return Err(crate::Error::Integrity("同步删除跨书书签身份冲突".into()));
            }
            tx.execute(
                "DELETE FROM bookmarks WHERE uid = ?1 AND updated_at <= ?2 AND book_id = ?3",
                params![delete.uid, delete.deleted_at, book_id],
            )?;
            write_tombstone(&tx, TombstoneKind::Bookmark, delete)?;
        }

        tx.commit()?;
        Ok(())
    }
}

fn tombstone_kind(value: &str) -> Option<TombstoneKind> {
    match value {
        "annotation" => Some(TombstoneKind::Annotation),
        "bookmark" => Some(TombstoneKind::Bookmark),
        _ => None,
    }
}

fn tombstone_kind_str(kind: TombstoneKind) -> &'static str {
    match kind {
        TombstoneKind::Annotation => "annotation",
        TombstoneKind::Bookmark => "bookmark",
    }
}

fn sort_tombstones(a: &Tombstone, b: &Tombstone) -> std::cmp::Ordering {
    let rank = |kind| match kind {
        TombstoneKind::Annotation => 0u8,
        TombstoneKind::Bookmark => 1,
    };
    (rank(a.kind), a.uid.as_str()).cmp(&(rank(b.kind), b.uid.as_str()))
}

fn book_id(conn: &rusqlite::Connection, hash: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM books WHERE content_hash = ?1",
            params![hash],
            |r| r.get(0),
        )
        .optional()?)
}

fn write_tombstone(
    conn: &rusqlite::Connection,
    kind: TombstoneKind,
    delete: &folio_model::sync::DeleteRecord,
) -> Result<()> {
    record_tombstone(
        conn,
        kind,
        &delete.uid,
        &delete.book_hash,
        delete.deleted_at,
    )
}

/// 记一条墓碑。**写它的有三处**（删批注、删书签、同步合并带进来的删除），全走这一个
/// 函数：分头写就会有一处先漏掉下面那条「迟到的墓碑不许把删除时间改早」。
pub(crate) fn record_tombstone(
    conn: &rusqlite::Connection,
    kind: TombstoneKind,
    uid: &str,
    book_hash: &str,
    deleted_at: i64,
) -> Result<()> {
    if uid.is_empty() || book_hash.is_empty() {
        // 没有 `uid` 或没有内容哈希的行**传不到任何地方**（跨设备靠这两样认身份），
        // 记一条对不上号的墓碑只会让对端把「这里删过一件不存在的事」读成删除。
        return Ok(());
    }
    // `WHERE excluded.deleted_at > 现有那条` 是 ST-3 那一刀：一条迟到的旧墓碑（另一台
    // 设备离线时带的旧时间）把 `deleted_at` 改**早**之后，合并时那条更早的活跃记录就能
    // 翻盘 —— 症状是读者删掉的东西过几天自己回来，而两处各自都还自洽。
    conn.execute(
        "INSERT INTO sync_tombstones (kind, book_hash, uid, deleted_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(kind, uid) DO UPDATE SET
           book_hash = excluded.book_hash, deleted_at = excluded.deleted_at
         WHERE excluded.deleted_at > sync_tombstones.deleted_at",
        params![tombstone_kind_str(kind), book_hash, uid, deleted_at],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
