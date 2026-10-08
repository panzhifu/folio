use std::path::{Path, PathBuf};

use rusqlite::params;

use super::Store;
use crate::format::{Chapter, DocMeta, FlowDoc, Format, TocEntry};
use crate::position::Locator;
use crate::prefs::{Prefs, ThemeName};

fn meta(title: &str) -> DocMeta {
    DocMeta {
        title: title.into(),
        authors: vec!["作者甲".into(), "作者乙".into()],
        format: Format::Markdown,
        size: 1234,
    }
}

fn path(name: &str) -> PathBuf {
    Path::new("/books").join(name)
}

fn flow(parts: &[(&str, &str)]) -> FlowDoc {
    FlowDoc {
        chapters: parts
            .iter()
            .enumerate()
            .map(|(i, (title, source))| Chapter {
                title: (*title).into(),
                source: (*source).into(),
                spine_href: format!("#{i}"),
            })
            .collect(),
        // 文档级三项一起进往返测试：EPUB 的 CSS / 目录 / 封面走的是 books 表的列。
        css: "p { text-indent: 2em }".into(),
        toc: vec![TocEntry {
            title: parts.first().map(|t| t.0.to_string()).unwrap_or_default(),
            href: "#0".into(),
            children: Vec::new(),
        }],
        cover: Some(vec![0x89, b'P', b'N', b'G', 13, 10]),
    }
}

#[test]
fn migrations_are_idempotent_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");

    let store = Store::open_at(&db).unwrap();
    assert_eq!(store.user_version().unwrap(), 3);
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    store.save_flow(id, &flow(&[("第一章", "正文一")])).unwrap();
    drop(store);

    let again = Store::open_at(&db).unwrap();
    assert_eq!(again.user_version().unwrap(), 3, "重开不应重复跑迁移");
    assert_eq!(again.list_books().unwrap().len(), 1);
    assert_eq!(
        again.load_flow(id).unwrap().unwrap().chapters[0].title,
        "第一章"
    );
}

#[test]
fn adding_the_same_path_twice_updates_in_place() {
    let store = Store::in_memory().unwrap();
    let first = store.add_book(&path("a.md"), &meta("旧标题")).unwrap();
    let second = store.add_book(&path("a.md"), &meta("新标题")).unwrap();
    assert_eq!(first, second, "同一路径不该变成两本书");
    let books = store.list_books().unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].title, "新标题");
    assert_eq!(books[0].authors, vec!["作者甲", "作者乙"]);
}

#[test]
fn flow_roundtrips_byte_for_byte() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    let doc = flow(&[
        ("前言", "# A\n中文前言，带标点。\n"),
        ("第一章", "## 第一章\n「引号」和 emoji 😀 也要原样回来。\n"),
    ]);
    store.save_flow(id, &doc).unwrap();
    assert_eq!(store.load_flow(id).unwrap().unwrap(), doc);
}

#[test]
fn saving_a_flow_twice_replaces_rather_than_appends() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    store
        .save_flow(id, &flow(&[("一", "a"), ("二", "b")]))
        .unwrap();
    store.save_flow(id, &flow(&[("新", "c")])).unwrap();
    let got = store.load_flow(id).unwrap().unwrap();
    assert_eq!(got.chapters.len(), 1);
    assert_eq!(got.chapters[0].title, "新");
}

#[test]
fn unknown_book_has_no_flow() {
    let store = Store::in_memory().unwrap();
    assert_eq!(store.load_flow(4242).unwrap(), None);
}

#[test]
fn position_roundtrips_and_overwrites() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    assert_eq!(store.position(id).unwrap(), None);

    let loc = Locator {
        chapter: 3,
        offset: 1024,
    };
    store.set_position(id, "continuous", &loc).unwrap();
    let (mode, got) = store.position(id).unwrap().unwrap();
    assert_eq!(mode, "continuous");
    assert_eq!(got, loc);

    let moved = Locator {
        chapter: 4,
        offset: 8,
    };
    store.set_position(id, "paged", &moved).unwrap();
    let (mode, got) = store.position(id).unwrap().unwrap();
    assert_eq!(mode, "paged");
    assert_eq!(got, moved, "同一本书只应有一份进度");
}

#[test]
fn prefs_default_then_roundtrip() {
    let store = Store::in_memory().unwrap();
    assert_eq!(store.prefs(), Prefs::default());
    let p = Prefs {
        theme: ThemeName::Sepia,
        font_size: 22.0,
        ..Default::default()
    };
    store.set_prefs(&p).unwrap();
    assert_eq!(store.prefs(), p);
}

#[test]
fn removing_a_book_cascades_its_chapters_and_position() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    store.save_flow(id, &flow(&[("一", "内容一")])).unwrap();
    store
        .set_position(id, "continuous", &Locator::start())
        .unwrap();

    store.remove_book(id).unwrap();
    assert_eq!(store.book(id).unwrap(), None);
    let chapter_rows: i64 = store
        .conn
        .query_row("SELECT count(*) FROM chapters", [], |r| r.get(0))
        .unwrap();
    assert_eq!(chapter_rows, 0, "章节必须随书一起消失");
    assert!(store.position(id).unwrap().is_none());
}

#[test]
fn chinese_fulltext_search_works_via_trigram() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    store
        .save_flow(
            id,
            &flow(&[
                ("第一章", "于是我们奋力向前，逆水行舟。"),
                ("第二章", "另一段毫无关系的文字。"),
            ]),
        )
        .unwrap();

    let hits = store.search("逆水行舟", 10).unwrap();
    assert_eq!(hits.len(), 1, "只应命中第一章");
    assert_eq!(hits[0].chapter_title, "第一章");
    assert!(
        hits[0].snippet.contains("逆水行舟"),
        "snippet={}",
        hits[0].snippet
    );

    assert!(
        store.search("逆水", 10).unwrap().is_empty(),
        "trigram 需要 3 个字符"
    );
    assert!(store.search("不存在的词", 10).unwrap().is_empty());
}

#[test]
fn search_index_follows_chapter_replacement() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    store.save_flow(id, &flow(&[("一", "逆水行舟")])).unwrap();
    assert_eq!(store.search("逆水行舟", 10).unwrap().len(), 1);

    store
        .save_flow(id, &flow(&[("一", "完全不同的内容")]))
        .unwrap();
    assert!(
        store.search("逆水行舟", 10).unwrap().is_empty(),
        "重写章节后旧命中不该还在"
    );
    assert_eq!(store.search("完全不同的", 10).unwrap().len(), 1);
}

#[test]
fn fts_query_syntax_in_user_input_is_treated_as_text() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    store
        .save_flow(id, &flow(&[("一", "这里有个 AND 关键字和引号 \"x\"")]))
        .unwrap();
    let hits = store.search("AND", 10).unwrap();
    assert_eq!(hits.len(), 1, "裸 AND 不该被当成布尔运算符");
    assert!(store.search("NEAR/a", 10).unwrap().is_empty());
}

#[test]
fn list_orders_most_recently_opened_first() {
    let store = Store::in_memory().unwrap();
    let a = store.add_book(&path("a.md"), &meta("A")).unwrap();
    let _b = store.add_book(&path("b.md"), &meta("B")).unwrap();
    store
        .conn
        .execute(
            "UPDATE books SET last_opened_at = 100 WHERE id = ?1",
            params![a],
        )
        .unwrap();
    let books = store.list_books().unwrap();
    assert_eq!(books[0].title, "A", "最近打开的排最前，没打开过的垫后");
}
