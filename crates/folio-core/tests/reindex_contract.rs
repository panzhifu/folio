//! 重扫（reindex）与书库的**集成契约**：编排住在 core 的 ingest，store 在 folio-store
//! ——两边的配合在这里验（原 store/tests.rs 的两条，2026-11-12 阶段四随拆分搬来；
//! 单侧留半份会削弱门）。五问：重读来源、不改文件名、不新增记录、章表跟着新内容走、
//! 读者的批注还在。
//!
//! 背景：reindex 的 SQL 曾经写成「删光这本书的章行再重插」—— 于是**每重扫一次就
//! 清空一次批注**，界面上那句「位置与批注都还在」是一句谎话。触发它的路是日常的：
//! 文件被换过、`INDEX_VERSION` 被 bump 过一次（动了那台分章引擎时必须 bump），或者
//! `.txt` 那一本的规则表换了。

use folio_core::StaleReason;
use folio_core::{DocMeta, Format, Store};
use folio_format::{Chapter, ChapterExt as _, FlowDoc};
use folio_model::types::AnnotationStyle;

fn meta(title: &str) -> DocMeta {
    DocMeta {
        title: title.into(),
        authors: vec!["作者甲".into()],
        format: Format::Markdown,
        size: 1234,
        ..DocMeta::default()
    }
}

fn flow(parts: &[(&str, &str)]) -> FlowDoc {
    FlowDoc {
        chapters: parts
            .iter()
            .enumerate()
            .map(|(i, (title, source))| {
                Chapter::inline(*title, *source).with_spine_href(format!("#{i}"))
            })
            .collect(),
        ..FlowDoc::default()
    }
}

/// 文件被换过之后重扫：又可信、章表跟着新文件走、批注原样。
#[test]
fn a_reindex_recovers_trust_and_keeps_reader_data() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    let path = dir.path().join("a.md");
    std::fs::write(&path, "# 一\n\n正文\n").unwrap();
    let store = Store::open_at(&db).unwrap();
    let id = store.add_book(&path, &meta("A")).unwrap();
    store
        .save_flow(id, &flow(&[("一", "正文")]), Format::Markdown)
        .unwrap();
    assert_eq!(store.stale_reason(id).unwrap(), None);

    // 读者在这一章划了一条批注：章 0 的第 1 个字之后（插入点，避开长度边界）。
    let anchor = folio_model::TextAnchor {
        chapter: 0,
        start: 1,
        end: 1,
        exact: String::new(),
        prefix: String::new(),
        suffix: String::new(),
        path: String::new(),
    };
    store
        .add_annotation(
            id,
            &anchor,
            "这一段写得真好",
            "amber",
            AnnotationStyle::default(),
        )
        .unwrap();

    // 文件被换掉（多了一章），然后重扫 —— 走的就是 `stale_reason` 之后那条路。
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&path, "# 一\n\n正文\n\n# 二\n\n又一章\n").unwrap();
    assert_eq!(store.stale_reason(id).unwrap(), Some(StaleReason::Changed));
    folio_core::reindex(&store, id).unwrap();

    assert_eq!(store.stale_reason(id).unwrap(), None, "重扫之后要恢复信任");
    assert_eq!(
        store.load_flow(id).unwrap().unwrap().chapters.len(),
        2,
        "章表要跟着新文件走，否则这条测试没在验真的重扫"
    );
    assert_eq!(
        store.annotations(id).unwrap().len(),
        1,
        "重扫不该清掉读者的批注（历史批注一条都不漂）"
    );
    assert_eq!(
        store.book(id).unwrap().unwrap().path,
        path.to_string_lossy(),
        "文件名不许改"
    );
    assert_eq!(store.list_books().unwrap().len(), 1, "重扫不许新增记录");
}

/// 重扫把索引版本顶回当前值（旧算法算的索引在重扫后不再报旧）。
#[test]
fn a_reindex_bumps_the_index_version_back() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    let path = dir.path().join("a.md");
    std::fs::write(&path, "# 一\n\n正文\n").unwrap();
    let store = Store::open_at(&db).unwrap();
    let id = store.add_book(&path, &meta("A")).unwrap();
    store
        .save_flow(id, &flow(&[("一", "正文")]), Format::Markdown)
        .unwrap();

    // 把索引版本按回 0，等价于「这一本是旧算法扫的」。
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("UPDATE books SET index_version = 0 WHERE id = ?1", [id])
            .unwrap();
    }
    assert_eq!(store.stale_reason(id).unwrap(), Some(StaleReason::OldIndex));

    folio_core::reindex(&store, id).unwrap();
    assert_eq!(
        store.stale_reason(id).unwrap(),
        None,
        "重扫之后该按当前算法记回索引版本"
    );
}
