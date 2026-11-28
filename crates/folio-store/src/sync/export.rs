use std::collections::{BTreeMap, HashMap};

use folio_model::TextAnchor;
use folio_model::revision::RevisionFence;
use folio_model::sync::{
    AnnotationRecord, BookSnapshot, BookmarkRecord, LocalSnapshot, Snapshot, Tombstone,
};
use rusqlite::{Connection, params};

use crate::{Result, Store};

/// 一页扫多少本书。页间放锁不是为了省内存，而是让「读者撞上同步导出」的最坏等待
/// 从一整趟压到一页（doc/计划/2026-11-23-性能基准记录.md：3000 本整趟一锁时读者
/// 等了 290 ms）。每页 64 本 × 每本几条查询，锁持有时间有界；页间新进的书写不进
/// 这一份快照，下一轮全量重扫必带上。
const SYNC_PAGE: usize = 64;

impl Store {
    /// 保留每条记录的原内容桶；当前书目换版不重标旧锚点。
    ///
    /// 长趟导出**按页多次上锁**（`doc/储存.md` 锁纪律的登记例外）：书目清单按
    /// `books.id` 游标走，id 由持久高水位单条分配、不复用，页间新进的书 id 更大
    /// 不会漏。跨页的 `owners` / `books` 状态由这里持有、按 `&mut` 递进页调用，
    /// 锁只在每次页调用内存在；删的那半（墓碑）单独一趟，必须在所有页扫完之后。
    pub fn collect_sync(&self, device_id: &str, exported_at: i64) -> Result<LocalSnapshot> {
        let mut owners = HashMap::<String, i64>::new();
        let mut books = BTreeMap::<String, BookSnapshot>::new();
        let mut skipped_books = 0;
        let mut after = 0;
        while let Some(next) =
            self.collect_sync_page(after, SYNC_PAGE, &mut owners, &mut books, &mut skipped_books)?
        {
            after = next;
            // 页间把锁**真放掉一拍**：紧接着的页调用立刻重取这把非公平 Mutex，热线程
            // 几乎每轮都赢过已经阻塞的读者——只让一步（yield）读者仍等到整趟量级
            // （sync-probe 实测 3000 本 max 311ms），睡 1ms 才保证阻塞方先拿。代价是
            // 导出时长每页 +1ms（3000 本约 +13%），换读者最坏等待从整趟压到一页量级。
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        self.collect_sync_tombstones(&mut books)?;
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

    /// 扫一页书（书号严格大于 `after`，按书号升序），交回下一页的游标；
    /// 本页没满就是到了头，交回 `None`。**每次调用只上锁一次**：页内每本书走
    /// [`collect_book_in`] 助手，整批同一把锁。
    ///
    /// `owners` 的跨书完整性断言（同内容多个书目不能合成一个同步桶）随页检查——
    /// 冲突当页即报 `Error::Integrity`，与单趟一锁只差报错时机，语义等价。
    pub(super) fn collect_sync_page(
        &self,
        after: i64,
        limit: usize,
        owners: &mut HashMap<String, i64>,
        books: &mut BTreeMap<String, BookSnapshot>,
        skipped_books: &mut usize,
    ) -> Result<Option<i64>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = conn.prepare(
            "SELECT id,coalesce(content_hash,''),active_rev_no,active_index_generation FROM books
             WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        let page = statement
            .query_map(params![after, limit as i64], |r| {
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
        let next = (page.len() == limit)
            .then(|| page.last().map(|(id, _, _)| *id))
            .flatten();
        for (book_id, current_hash, current_fence) in page {
            if collect_book_in(
                &conn,
                book_id,
                &current_hash,
                current_fence,
                owners,
                books,
            )? {
                *skipped_books += 1;
            }
        }
        Ok(next)
    }

    /// 墓碑按 book_hash 归桶。桶的存在由页扫决定，所以这一趟只在全部页之后跑；
    /// 每次调用只上锁一次。
    fn collect_sync_tombstones(&self, books: &mut BTreeMap<String, BookSnapshot>) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
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
        Ok(())
    }
}

/// 一本书的收集（原 `collect_sync` 的按书循环体），接连接。
/// 交回这本书是否计入 skipped（版本桶全不支持或有 deferred 记录）。
fn collect_book_in(
    conn: &Connection,
    book_id: i64,
    current_hash: &str,
    current_fence: RevisionFence,
    owners: &mut HashMap<String, i64>,
    books: &mut BTreeMap<String, BookSnapshot>,
) -> Result<bool> {
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
        versions.push((current_hash.to_owned(), current_fence, String::new()));
    }
    // 差量在行上、与版本桶无关：每个桶写的是同一份。原先在桶循环里逐桶重查同一行，
    // 每本书多付 (桶数 − 1) 次查询；提到循环外，桶循环里只克隆。
    let settings = crate::sync_order::settings(conn, book_id)?.filter(|r| r.updated_at > 0);
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
        let canonical = super::revisions::owner(conn, &hash)?
            .map(|(_, f)| f)
            .unwrap_or(fence);
        if fence == current_fence
            && !super::revisions::same_coordinates(conn, book_id, fence, canonical)?
        {
            deferred = true;
        }
        let book = books.entry(hash).or_default();
        let position = if fence == current_fence {
            crate::sync_order::position(conn, book_id)?
        } else {
            crate::record_revisions::position_in(conn, book_id, fence)?
        };
        if fence == canonical {
            book.position = position;
        }
        book.settings = settings.clone();
    }
    deferred |= annotations(conn, book_id, current_hash, owners, books)?;
    deferred |= bookmarks(conn, book_id, current_hash, owners, books)?;
    Ok(!supported || deferred)
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
