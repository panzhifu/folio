use rusqlite::{Connection, OptionalExtension as _, params};

use crate::{BookSource, Error, Result, Store};

const COLUMNS: &str = "id,book_id,locator,kind,is_primary,generation,fingerprint,state";
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<BookSource> {
    Ok(BookSource {
        id: r.get(0)?,
        book_id: r.get(1)?,
        locator: r.get(2)?,
        kind: r.get(3)?,
        is_primary: r.get(4)?,
        generation: r.get(5)?,
        fingerprint: r.get(6)?,
        state: r.get(7)?,
    })
}

pub(crate) fn source_in(conn: &Connection, id: i64) -> Result<BookSource> {
    Ok(conn.query_row(
        &format!("SELECT {COLUMNS} FROM book_sources WHERE id=?1"),
        [id],
        row,
    )?)
}

pub(crate) fn bind_in(
    conn: &Connection,
    book_id: i64,
    locator: &str,
    kind: &str,
    primary: bool,
) -> Result<BookSource> {
    let existing = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM book_sources WHERE locator=?1"),
            [locator],
            row,
        )
        .optional()?;
    if let Some(source) = existing {
        if source.book_id != book_id {
            return Err(Error::Integrity("来源已经关联另一书目".into()));
        }
        if primary {
            conn.execute(
                "UPDATE book_sources SET is_primary=0,generation=generation+1 WHERE book_id=?1 AND id<>?2 AND is_primary=1",
                params![book_id,source.id],
            )?;
        }
        conn.execute(
            "UPDATE book_sources SET generation=generation+CASE WHEN kind<>?2 OR is_primary<>?3 OR state<>'available' THEN 1 ELSE 0 END,kind=?2,is_primary=?3,state='available' WHERE id=?1",
            params![source.id, kind, primary],
        )?;
        conn.execute("DELETE FROM suppressed_sources WHERE locator=?1", [locator])?;
        return source_in(conn, source.id);
    }
    if primary {
        conn.execute(
            "UPDATE book_sources SET is_primary=0,generation=generation+1 WHERE book_id=?1 AND is_primary=1",
            [book_id],
        )?;
    }
    conn.execute(
        "INSERT INTO book_sources(book_id,locator,kind,is_primary) VALUES(?1,?2,?3,?4)",
        params![book_id, locator, kind, primary],
    )?;
    conn.execute("DELETE FROM suppressed_sources WHERE locator=?1", [locator])?;
    source_in(conn, conn.last_insert_rowid())
}

impl Store {
    /// 新原件已捕获并成为Ready后才切换主来源，与版本来源token一笔提交。
    pub fn link_ready_source(
        &self,
        book_id: i64,
        target: crate::RevisionFence,
        expected_active: crate::RevisionFence,
        locator: &str,
        kind: &str,
        fingerprint: &str,
    ) -> Result<BookSource> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        crate::revisions::check_revision_in(&tx, book_id, expected_active)?;
        let ready:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM book_revisions WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3 AND state='ready' AND source_id IS NULL)",params![book_id,target.rev_no,target.index_generation],|r|r.get(0))?;
        if !ready {
            return Err(Error::Integrity("新原件尚未成为可关联的就绪版本".into()));
        }
        let source = bind_in(&tx, book_id, locator, kind, true)?;
        tx.execute("UPDATE book_sources SET generation=generation+CASE WHEN fingerprint<>?2 THEN 1 ELSE 0 END,fingerprint=?2,state='available' WHERE id=?1",params![source.id,fingerprint])?;
        let source = source_in(&tx, source.id)?;
        tx.execute("UPDATE book_revisions SET source_id=?4,source_generation=?5 WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",params![book_id,target.rev_no,target.index_generation,source.id,source.generation])?;
        tx.commit()?;
        Ok(source)
    }

    pub fn set_source_state_at(&self, id: i64, generation: i64, state: &str) -> Result<bool> {
        if !matches!(
            state,
            "available" | "missing" | "unreadable" | "preparing" | "failed" | "relink-required"
        ) {
            return Err(Error::Integrity("未知来源状态".into()));
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(conn.execute("UPDATE book_sources SET state=?3 WHERE id=?1 AND generation=?2 AND state<>'suppressed'",params![id,generation,state])?!=0)
    }
    pub fn observe_source_at(
        &self,
        id: i64,
        expected_generation: i64,
        fingerprint: &str,
    ) -> Result<Option<BookSource>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let changed=conn.execute("UPDATE book_sources SET generation=generation+CASE WHEN fingerprint<>?3 THEN 1 ELSE 0 END,fingerprint=?3,state='available' WHERE id=?1 AND generation=?2 AND state<>'suppressed'",params![id,expected_generation,fingerprint])?;
        if changed == 0 {
            return Ok(None);
        }
        Ok(Some(source_in(&conn, id)?))
    }
    pub fn lookup_source(&self, locator: &str) -> Result<Option<BookSource>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM book_sources WHERE locator=?1"),
                [locator],
                row,
            )
            .optional()?)
    }
    pub fn bind_source(
        &self,
        book_id: i64,
        locator: &str,
        kind: &str,
        primary: bool,
    ) -> Result<BookSource> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        let source = bind_in(&tx, book_id, locator, kind, primary)?;
        tx.commit()?;
        Ok(source)
    }
    pub fn observe_source(&self, id: i64, fingerprint: &str) -> Result<BookSource> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute("UPDATE book_sources SET generation=generation+CASE WHEN fingerprint<>?2 THEN 1 ELSE 0 END,fingerprint=?2,state='available' WHERE id=?1 AND state<>'suppressed'",params![id,fingerprint])?;
        source_in(&conn, id)
    }
    pub fn mark_source_dirty(&self, id: i64) -> Result<BookSource> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE book_sources SET generation=generation+1 WHERE id=?1 AND state<>'suppressed'",
            [id],
        )?;
        source_in(&conn, id)
    }
    pub fn set_source_state(&self, id: i64, state: &str) -> Result<()> {
        if !matches!(
            state,
            "available"
                | "missing"
                | "unreadable"
                | "preparing"
                | "failed"
                | "suppressed"
                | "relink-required"
        ) {
            return Err(Error::Integrity("未知来源状态".into()));
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE book_sources SET state=?2 WHERE id=?1",
            params![id, state],
        )?;
        Ok(())
    }
    pub fn sources_for_reconcile(&self) -> Result<Vec<BookSource>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt=conn.prepare(&format!("SELECT {COLUMNS} FROM book_sources WHERE is_primary=1 AND state NOT IN ('suppressed','relink-required') ORDER BY id"))?;
        Ok(stmt
            .query_map([], row)?
            .collect::<std::result::Result<_, _>>()?)
    }
    pub fn sources_for_book(&self, book_id: i64) -> Result<Vec<BookSource>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM book_sources WHERE book_id=?1 ORDER BY is_primary DESC,id"
        ))?;
        Ok(stmt
            .query_map([book_id], row)?
            .collect::<std::result::Result<_, _>>()?)
    }
    pub fn source_is_suppressed(&self, locator: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM suppressed_sources WHERE locator=?1)",
            [locator],
            |r| r.get(0),
        )?)
    }
    pub fn suppress_book_sources(&self, book_id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction()?;
        suppress_in(&tx, book_id)?;
        tx.commit()?;
        Ok(())
    }
}

pub(crate) fn suppress_in(conn: &Connection, book_id: i64) -> Result<()> {
    conn.execute("INSERT OR REPLACE INTO suppressed_sources SELECT locator,?2 FROM book_sources WHERE book_id=?1",params![book_id,crate::now()])?;
    conn.execute(
        "UPDATE book_sources SET state='suppressed',generation=generation+1 WHERE book_id=?1",
        [book_id],
    )?;
    Ok(())
}
