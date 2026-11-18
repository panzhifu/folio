use std::path::Path;

use rusqlite::{Connection, OpenFlags, params};

use crate::{Error, Result, Store};

impl Store {
    /// 只读校验备份数据库，拒绝未来 schema 与损坏/悬空外键；不运行迁移。
    pub fn inspect_backup(path: &Path) -> Result<i64> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version <= 0 || version > crate::schema::MIGRATIONS.len() as i64 {
            return Err(Error::Integrity("不支持的备份数据库版本".into()));
        }
        let integrity: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return Err(Error::Integrity("备份数据库完整性检查失败".into()));
        }
        let mut statement = conn.prepare("PRAGMA foreign_key_check")?;
        if statement.query([])?.next()?.is_some() {
            return Err(Error::Integrity("备份数据库含悬空外键".into()));
        }
        Ok(version)
    }

    /// 只对恢复副本调用：把旧书库下的相对位置投影到新根，保留书目身份。
    pub fn relocate_backup_library(&self, old_root: &str, new_root: &Path) -> Result<usize> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let rows: Vec<(i64, String)> = {
            let mut statement = conn.prepare("SELECT id, path FROM books ORDER BY id")?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?
        };
        let old_root = old_root.replace('\\', "/");
        let prefix = format!("{}/", old_root.trim_end_matches('/'));
        let mut translated = Vec::new();
        for (id, path) in rows {
            let path = path.replace('\\', "/");
            let relative = path
                .strip_prefix(&prefix)
                .ok_or_else(|| Error::Integrity("书目路径不属于备份书库根".into()))?;
            if relative.is_empty()
                || relative
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == ".." || p.contains(':'))
            {
                return Err(Error::Integrity("备份书目路径非法".into()));
            }
            translated.push((id, new_root.join(relative).to_string_lossy().into_owned()));
        }
        let tx = conn.unchecked_transaction()?;
        for (id, path) in &translated {
            tx.execute(
                "UPDATE books SET path = ?2 WHERE id = ?1",
                params![id, path],
            )?;
        }
        let revisions: Vec<(i64, i64, i64, String)> = {
            let mut stmt=tx.prepare("SELECT book_id,rev_no,index_generation,managed_path FROM book_revisions WHERE managed_path<>''")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<std::result::Result<_, _>>()?
        };
        for (book, rev, generation, path) in revisions {
            let normalized = path.replace('\\', "/");
            let relative = normalized
                .strip_prefix(&prefix)
                .ok_or_else(|| Error::Integrity("版本路径不属于备份书库根".into()))?;
            if relative.is_empty()
                || relative
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == ".." || p.contains(':'))
            {
                return Err(Error::Integrity("备份版本路径非法".into()));
            }
            tx.execute("UPDATE book_revisions SET managed_path=?4,library_root=?5 WHERE book_id=?1 AND rev_no=?2 AND index_generation=?3",params![book,rev,generation,new_root.join(relative).to_string_lossy(),new_root.to_string_lossy()])?;
        }
        tx.execute(
            "UPDATE books SET managed_root=?1",
            [new_root.to_string_lossy().as_ref()],
        )?;
        // 原机器的路径/URI描述可以保留，但恢复不自动重用其访问能力。
        tx.execute(
            "UPDATE book_sources SET state='relink-required',generation=generation+1",
            [],
        )?;
        tx.execute("DELETE FROM revision_operations", [])?;
        tx.execute(
            "UPDATE book_revisions SET state='failed' WHERE state='preparing'",
            [],
        )?;
        tx.commit()?;
        Ok(translated.len())
    }

    /// 恢复副本关闭前把 WAL 合并回主文件，供下一进程打开。
    pub fn checkpoint_restored(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let (busy, _, _): (i64, i64, i64) =
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
        if busy != 0 {
            return Err(Error::Integrity("恢复副本仍被占用".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;
    use folio_format::{DocMeta, Format};

    #[test]
    fn restore_relocation_preserves_identity_and_rejects_outside_paths() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open_at(&root.path().join("folio.db")).unwrap();
        let id = store
            .add_book(
                std::path::Path::new("C:/old/books/a.md"),
                &DocMeta {
                    title: "A".into(),
                    format: Format::Markdown,
                    ..Default::default()
                },
            )
            .unwrap();
        let dest = root.path().join("books");
        assert_eq!(
            store
                .relocate_backup_library("C:\\old\\books", &dest)
                .unwrap(),
            1
        );
        assert_eq!(
            store.book(id).unwrap().unwrap().path,
            dest.join("a.md").to_string_lossy()
        );
        assert!(store.relocate_backup_library("unrelated", &dest).is_err());
        store.checkpoint_restored().unwrap();
        drop(store);
        assert!(Store::inspect_backup(&root.path().join("folio.db")).is_ok());
    }
}
