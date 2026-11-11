//! 读者**自己持有**的那份口令，存在本机、按书的 `content_hash` 认。
//!
//! 只有 PDF 那一族用得上（`content::format::pdf` 与 `azw4`：那边的引擎本来就收口令）。
//! MOBI / EPUB 的 DRM 不需要这一条，也永远不会走到这一条 —— 那一族的密钥要从读者的
//! 注册身份派生，不是他手上的一份口令，本仓库不解。
//!
//! 为什么是一张独立的表（而不是 `books` 上多一列）：同步那条链的取数面是逐条 SELECT
//! 写死的（[`store::sync`](super::sync) 只碰 `books` / `positions` / `annotations` /
//! `bookmarks` / `sync_tombstones`），而快照是明文 JSON 传到 WebDAV。加在 `books` 上
//! 今天不出事，靠的是那份清单的纪律；独立一张表把「不上同步」变成结构。这条由
//! `store::sync` 底部的 `remembered_passphrase_never_enters_the_sync_payload` 钉着。
//!
//! 明文存：与 `infra::config` 里那几份 WebDAV 口令 / API key 同一信任域（同一个数据目录、
//! 同一台设备、本机没有任何钥匙串可用）。

use rusqlite::{OptionalExtension, params};

use crate::Result;
use folio_format::Passphrase;

use super::Store;

impl Store {
    /// 记住这一本的口令（按 `content_hash`）。重复写就是覆盖：读者改了手上那份口令。
    ///
    /// 空口令不当成「记住了」—— 那是「这本没锁」，存它只会多一行没人读的明文。
    pub fn set_book_passphrase(&self, book_hash: &str, passphrase: &str) -> Result<()> {
        if book_hash.is_empty() || passphrase.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO book_passphrases (book_hash, passphrase, set_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(book_hash) DO UPDATE SET passphrase = ?2, set_at = ?3",
            params![book_hash, passphrase, now()],
        )?;
        Ok(())
    }

    /// 这一本（按 `content_hash`）记住的口令。没有就 `None` = 这本没锁，或者没记住过。
    pub fn passphrase_for_hash(&self, book_hash: &str) -> Result<Option<Passphrase>> {
        if book_hash.is_empty() {
            return Ok(None);
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let text: Option<String> = conn
            .query_row(
                "SELECT passphrase FROM book_passphrases WHERE book_hash = ?1",
                params![book_hash],
                |row| row.get(0),
            )
            .optional()?;
        Ok(text.map(Passphrase::new))
    }

    /// 同 [`passphrase_for_hash`](Self::passphrase_for_hash)，但按书架上那行的 id 问。
    ///
    /// 单独给一条是因为「打开一本书」「搜一本书」手上只有 `book_id`，而口令按哈希存 ——
    /// 这一句把那次换算收在这里，界面与搜索各自不必再去背「先去 books 拿 hash」那一步。
    pub fn passphrase_for_book(&self, book_id: i64) -> Result<Option<Passphrase>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let text: Option<String> = conn
            .query_row(
                "SELECT p.passphrase FROM books b
                   JOIN book_passphrases p ON p.book_hash = b.content_hash
                  WHERE b.id = ?1",
                params![book_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(text.map(Passphrase::new))
    }

    /// 忘掉这一本的口令（读者在设置里取消「记住这本」，或换了那份口令又想重输）。
    pub fn forget_book_passphrase(&self, book_hash: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "DELETE FROM book_passphrases WHERE book_hash = ?1",
            params![book_hash],
        )?;
        Ok(())
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

/// 这一本记住过口令，**下次开书必须带着它**：`with_container` 少了那一格，
/// 取正文与取页都会回空，屏上的症状是「架上看得见、点开是空白、搜也搜不到」——
/// 所以按 id 问这一条 (`passphrase_for_book`) 是打开那一路的必经一步，不是可选优化。
#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::Store;
    use folio_format::{DocMeta, Format};

    /// 一本带 `content_hash` 的书 —— 口令那一格认的就是这个哈希，没有它哪儿都对不上。
    fn store_with_book(hash: &str) -> (Store, i64) {
        let store = Store::in_memory().unwrap();
        let book = store
            .add_book(
                Path::new(&format!("/books/{hash}.pdf")),
                &DocMeta {
                    title: hash.into(),
                    format: Format::Pdf,
                    size: 4096,
                    ..DocMeta::default()
                },
            )
            .unwrap();
        store.set_content_hash(book, hash, "sha256").unwrap();
        (store, book)
    }

    /// 记住 → 按哈希与按 id 两种问法都取回同一份。后一条是「从书架点开」那一路的形状。
    #[test]
    fn a_remembered_passphrase_comes_back_by_hash_and_by_book_id() {
        let (store, book) = store_with_book("hash-a");
        store.set_book_passphrase("hash-a", "samplefiles").unwrap();
        let by_hash = store.passphrase_for_hash("hash-a").unwrap();
        let by_id = store.passphrase_for_book(book).unwrap();
        assert_eq!(by_hash.as_ref().map(|p| p.as_str()), Some("samplefiles"));
        assert_eq!(by_id.as_ref().map(|p| p.as_str()), Some("samplefiles"));
    }

    /// 同一本改口令是覆盖，不是多出一行 —— 否则旧那条会一直被 `passphrase_for_book` 的
    /// JOIN 撞见，而它已经不开了。
    #[test]
    fn setting_a_passphrase_twice_overwrites_instead_of_stacking() {
        let (store, book) = store_with_book("hash-d");
        store.set_book_passphrase("hash-d", "旧的").unwrap();
        store.set_book_passphrase("hash-d", "新的").unwrap();
        assert_eq!(
            store
                .passphrase_for_book(book)
                .unwrap()
                .as_ref()
                .map(|p| p.as_str()),
            Some("新的")
        );
    }

    /// 空串两头的两种用法都不该留一行明文：没给哈希 = 没处存，空口令 = 这本根本没锁。
    #[test]
    fn an_empty_passphrase_or_hash_stores_nothing() {
        let store = Store::in_memory().unwrap();
        store.set_book_passphrase("", "hunter2").unwrap();
        store.set_book_passphrase("hash-b", "").unwrap();
        assert!(store.passphrase_for_hash("").unwrap().is_none());
        assert!(store.passphrase_for_hash("hash-b").unwrap().is_none());
    }

    /// 忘掉之后两种问法都回空。
    #[test]
    fn forgetting_a_passphrase_clears_both_lookups() {
        let (store, book) = store_with_book("hash-c");
        store.set_book_passphrase("hash-c", "hunter2").unwrap();
        store.forget_book_passphrase("hash-c").unwrap();
        assert!(store.passphrase_for_hash("hash-c").unwrap().is_none());
        assert!(store.passphrase_for_book(book).unwrap().is_none());
    }

    /// 没记住过的书（含哈希不存在于口令表）回 `None`，而不是报错：那是「这本没锁」。
    #[test]
    fn a_book_without_a_remembered_passphrase_reads_as_none() {
        let (store, book) = store_with_book("hash-e");
        assert!(store.passphrase_for_book(book).unwrap().is_none());
        assert!(store.passphrase_for_hash("hash-e").unwrap().is_none());
    }

    /// `Debug` 不泄露原文：那是口令进日志的唯一路径。长度留着 —— 它是「有没有输进东西」
    /// 的唯一线索，而那个数看不出内容。
    #[test]
    fn a_passphrase_never_prints_itself() {
        let passphrase = folio_format::Passphrase::new("samplefiles");
        let printed = format!("{passphrase:?}");
        assert!(
            !printed.contains("samplefiles"),
            "调试输出漏了原文：{printed}"
        );
        assert!(printed.contains("11"), "调试输出该带上长度：{printed}");
    }
}
