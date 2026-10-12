//! 存储层：迁移、书库、章节/图片/进度/偏好的往返与级联。只依赖公开 API。
//!
//! 级联那条测试要看「牌面之下的表」，但绝不碰私有字段：用公开的 `Store::open_at`
//! 建库，再用一条裸 `rusqlite` 连接按**库文件契约**查计数。

use std::path::{Path, PathBuf};

use crate::Store;
use crate::format::{
    Chapter, ChapterLocator, Content, DocMeta, FixedDoc, FixedPage, FlowDoc, Format, ImageRes,
    PageSource, TocEntry, guess_media_type,
};
use crate::position::Locator;
use crate::store::{MIGRATIONS, StaleReason};

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
            .map(|(i, (title, source))| {
                Chapter::inline(*title, *source).with_spine_href(format!("#{i}"))
            })
            .collect(),
        // 文档级三项一起进往返测试：EPUB 的 CSS / 目录 / 封面走的是 books 表的列。
        css: "p { text-indent: 2em }".into(),
        css_hrefs: Vec::new(),
        toc: vec![TocEntry {
            title: parts.first().map(|t| t.0.to_string()).unwrap_or_default(),
            href: "#0".into(),
            fragment: Some("sec1".into()),
            children: Vec::new(),
        }],
        cover: Some(vec![0x89, b'P', b'N', b'G', 13, 10]),
        // 一张图片一起进往返：EPUB 的内嵌图走 images 表。
        images: vec![ImageRes {
            href: "OEBPS/Images/fig1.png".into(),
            media_type: "image/png".into(),
            width: 640,
            height: 480,
        }],
    }
}

#[test]
fn migrations_are_idempotent_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");

    let store = Store::open_at(&db).unwrap();
    assert_eq!(store.user_version().unwrap(), MIGRATIONS.len() as i64);
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    store
        .save_flow(id, &flow(&[("第一章", "正文一")]), Format::Markdown)
        .unwrap();
    drop(store);

    let again = Store::open_at(&db).unwrap();
    assert_eq!(
        again.user_version().unwrap(),
        MIGRATIONS.len() as i64,
        "重开不应重复跑迁移"
    );
    assert_eq!(again.list_books().unwrap().len(), 1);
    assert_eq!(
        again.load_flow(id).unwrap().unwrap().chapters[0].title,
        "第一章"
    );
}

/// v5 迁移：旧库（v4，`images.bytes` 还在）升上来时，字节要被丢掉、元数据留下。
///
/// 这条不是形式主义 —— 已经装过的 `folio.db` 就是 v4 形状，
/// `ALTER TABLE ... DROP COLUMN` 在它上面成功与否，决定老用户开不开得了书。
#[test]
fn the_v5_migration_drops_image_bytes_from_an_old_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    // 手工造一个 v4 库：跑前四条迁移（到 v4 为止），塞一张带字节的图，
    // 然后把版本号停在 4。
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        for sql in &MIGRATIONS[..4] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 4).unwrap();
        conn.execute(
            "INSERT INTO books (path, title, authors, format, size, added_at)
             VALUES ('a.epub', 'A', '', 'epub', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO images (book_id, href, media_type, width, height, bytes)
             VALUES (1, 'OEBPS/i.png', 'image/png', 3, 2, x'89504E47')",
            [],
        )
        .unwrap();
    }

    let store = Store::open_at(&db).unwrap();
    assert_eq!(store.user_version().unwrap(), MIGRATIONS.len() as i64);
    drop(store);

    let conn = rusqlite::Connection::open(&db).unwrap();
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(images)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert!(
        !cols.iter().any(|c| c == "bytes"),
        "v5 之后 images 表不该还有 bytes 列：{cols:?}"
    );
    let (href, w, h): (String, i64, i64) = conn
        .query_row(
            "SELECT href, width, height FROM images WHERE book_id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((href.as_str(), w, h), ("OEBPS/i.png", 3, 2), "元数据要留下");
}

/// v8 迁移：旧库（v7，正文是 `TEXT` + trigram 索引）升上来时，**正文要被丢掉**、
/// 索引与触发器都要没，而章表要拿到一个能回容器取回它的位置。
///
/// 正文丢得理直气壮：它在 `books.path` 那个文件里，重开一次容器就回来了（见
/// `plans/text-out-of-db.md`）。这条钉的是「丢得干净、且位置算对了」。
#[test]
fn the_v8_migration_drops_the_text_and_keeps_a_locator() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    // 手工造一个 v7 库：跑前七条迁移，塞一本带正文的书，版本停在 7。
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        for sql in &MIGRATIONS[..7] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 7).unwrap();
        conn.execute(
            "INSERT INTO books (path, title, authors, format, size, added_at)
             VALUES ('a.epub', 'A', '', 'epub', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chapters (book_id, idx, title, spine_href, char_len)
             VALUES (1, 0, '第一章', 'OEBPS/c1.xhtml', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chapter_text (chapter_id, text) VALUES (1, '正文')",
            [],
        )
        .unwrap();
        // v2 的触发器应该已经把索引填上了 —— 先确认旧形状真是这样，
        // 否则下面的断言就只是在验一个空库。
        let indexed: i64 = conn
            .query_row("SELECT count(*) FROM chapter_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(indexed, 1, "v2 的触发器该把正文索引进 FTS");
    }

    let store = Store::open_at(&db).unwrap();
    assert_eq!(store.user_version().unwrap(), MIGRATIONS.len() as i64);
    let flow = store.load_flow(1).unwrap().expect("章表要还在");
    assert_eq!(flow.chapters.len(), 1);
    assert_eq!(flow.chapters[0].title, "第一章");
    assert_eq!(flow.chapters[0].source(), "", "正文不该跟着回来");
    assert_eq!(
        flow.chapters[0].locator(),
        Some(&ChapterLocator::Entry("OEBPS/c1.xhtml".into())),
        "EPUB 那一章该拿到它自己的容器条目"
    );
    drop(store);

    let conn = rusqlite::Connection::open(&db).unwrap();
    let leftovers: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master
             WHERE name LIKE 'chapter_fts%' OR name IN ('chapter_text', 'chapter_text_new')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftovers, 0, "索引表与正文表都要没");

    // 再来一次不该出事（迁移可重入）。
    Store::open_at(&db).unwrap();
}

/// v11 迁移：旧库（v10）升上来时要有 `cover_spec` 列，缺省 `0` = 迁移之前生成的封面，
/// 一律当旧规格 —— 开书时由 `stale_reason` 认出来重扫重生成（`plans/基础设施.md` §5.1）。
#[test]
fn the_v11_migration_adds_the_cover_spec_column() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        for sql in &MIGRATIONS[..10] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 10).unwrap();
        conn.execute(
            "INSERT INTO books (path, title, authors, format, size, added_at, cover)
             VALUES ('a.epub', 'A', '', 'epub', 0, 0, x'89504E47')",
            [],
        )
        .unwrap();
    }

    let store = Store::open_at(&db).unwrap();
    assert_eq!(store.user_version().unwrap(), MIGRATIONS.len() as i64);
    drop(store);

    let conn = rusqlite::Connection::open(&db).unwrap();
    let spec: i64 = conn
        .query_row("SELECT cover_spec FROM books WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(spec, 0, "迁移之前生成的封面一律当旧规格");
}

/// §5.1：有封面、但记的是旧规格 → 认成 `OldCover`，好让开书路径重扫重生成；
/// 没有封面的书不受这条影响。
#[test]
fn an_old_cover_spec_is_reported_as_stale() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    let path = dir.path().join("a.md");
    std::fs::write(&path, "# 一\n\n正文\n").unwrap();
    let store = Store::open_at(&db).unwrap();
    let id = store.add_book(&path, &meta("A")).unwrap();
    store
        .save_flow(id, &flow(&[("一", "正文")]), Format::Markdown)
        .unwrap();
    assert_eq!(store.stale_reason(id).unwrap(), None, "刚存完规格是当前的");

    // 手工把规格退回迁移前的 0（模拟老库升上来）。
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("UPDATE books SET cover_spec = 0 WHERE id = ?1", [id])
            .unwrap();
    }
    assert_eq!(
        store.stale_reason(id).unwrap(),
        Some(StaleReason::OldCover),
        "有封面且规格落后，该认出来"
    );

    // 没有封面就没什么可重生成 —— 同一条规则不该再触发。
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("UPDATE books SET cover = NULL WHERE id = ?1", [id])
            .unwrap();
    }
    assert_eq!(store.stale_reason(id).unwrap(), None, "没有封面就不管规格");
}

/// 定版式文档（CBZ 等）落库时也要盖 `index_version` —— 不然 `stale_reason` 永远说它旧，
/// 每次开书都白重扫一遍。这是那个既存 bug 的回归门。
#[test]
fn a_fixed_document_is_not_perpetually_stale() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    let path = dir.path().join("m.cbz");
    std::fs::write(&path, b"fake zip bytes").unwrap();
    let store = Store::open_at(&db).unwrap();
    let id = store.add_book(&path, &meta("M")).unwrap();

    let fixed = FixedDoc {
        pages: vec![FixedPage {
            source: PageSource::Image {
                href: "1.png".into(),
            },
            width: 800,
            height: 1200,
        }],
        toc: Vec::new(),
        cover: Some(vec![0x89, b'P', b'N', b'G']),
        images: Vec::new(),
        width: 800,
        height: 1200,
    };
    store.save_fixed(id, &fixed).unwrap();

    // `index_version` 要盖上当前版本（修之前它是 0，于是下面那条断言永远报 OldIndex）。
    let version: i64 = {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.query_row("SELECT index_version FROM books WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!(version, crate::format::INDEX_VERSION);
    assert_eq!(
        store.stale_reason(id).unwrap(),
        None,
        "定版式书不该每次开都被判重扫"
    );
}

/// §5.2 护栏：库文件坏了，`open_at` 要**报错**而不是把它删掉重建 —— 损坏的库被自动
/// 重建等价于丢数据（进度、批注、集合都在里面）。
#[test]
fn a_corrupt_database_is_reported_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    let garbage = b"this is not a sqlite database at all, not even close".as_slice();
    std::fs::write(&db, garbage).unwrap();

    // `Store` 不是 `Debug`，所以不用 `expect_err`。
    let err = match Store::open_at(&db) {
        Ok(_) => panic!("坏库该报错，实际打开了"),
        Err(err) => err,
    };
    assert!(
        matches!(err, crate::Error::Sql(_)),
        "该是 sqlite 那一层的错，实际 {err:?}"
    );
    assert_eq!(
        std::fs::read(&db).unwrap(),
        garbage,
        "坏库一个字节都不许被动过"
    );
}

/// 删掉一本书之后，文件要**缩回去**，而不是把空间留在 freelist 里。
///
/// `auto_vacuum=INCREMENTAL` 单独不会吐页 —— 得有人叫一次 `incremental_vacuum`，
/// 这条钉的就是那个调用没被删掉。注意只能保证「峰位回得去」而不是「回到基线」：
/// 增量真空只能截掉文件尾部的空页，夹在中间的要全量 `VACUUM` 才动得了。
/// 量的是**主文件**（WAL 里还没 checkpoint 的内容不算），所以先 checkpoint 再读。
#[test]
fn deleting_a_book_gives_its_pages_back() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    let pages = |db: &Path| -> i64 {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        conn.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap()
    };

    let store = Store::open_at(&db).unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    // 正文出库之后，能撑大文件的只剩封面与图片元数据 —— 拿一张 1 MB 的封面来撑。
    let mut doc = flow(&[("一", "正文")]);
    doc.cover = Some(vec![0u8; 1 << 20]);
    store.save_flow(id, &doc, Format::Markdown).unwrap();
    let peak = pages(&db);
    store.remove_book(id).unwrap();
    drop(store);

    let after = pages(&db);
    assert!(after < peak, "删完之后文件该缩回去：{after} vs 峰位 {peak}");
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

/// 落库的**索引**原样往返：章表（标题 + 条目名）、CSS、目录、封面、图片元数据，
/// 以及每章在容器里的位置。
///
/// 注意这里**没有正文**：正文不在库里（`the_text_never_reaches_the_database` 钉这一条），
/// 所以「往返」这个词覆盖的只有索引 —— 正文的往返在
/// `crate::document::book::tests` 里对容器测。
#[test]
fn the_index_roundtrips_through_the_store() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    let doc = flow(&[
        ("前言", "# A\n中文前言，带标点。\n"),
        ("第一章", "## 第一章\n「引号」和 emoji 😀 也要原样回来。\n"),
    ]);
    store.save_flow(id, &doc, Format::Markdown).unwrap();
    let back = store.load_flow(id).unwrap().unwrap();

    assert_eq!(back.chapters.len(), 2);
    assert_eq!(back.chapters[0].title, "前言");
    assert_eq!(back.chapters[1].title, "第一章");
    assert_eq!(back.css, doc.css);
    assert_eq!(back.toc, doc.toc);
    assert_eq!(back.cover, doc.cover);
    assert_eq!(back.images, doc.images);
    // 正文没跟着回来，位置回来了 —— 单文件格式按序号认。
    assert!(back.chapters.iter().all(|c| c.source().is_empty()));
    assert_eq!(
        back.chapters
            .iter()
            .map(|c| c.locator().cloned())
            .collect::<Vec<_>>(),
        vec![
            Some(ChapterLocator::Section(0)),
            Some(ChapterLocator::Section(1))
        ]
    );
}

/// 图片的元数据必须原样回来（字节不入库，见 v5 迁移），且重存时是替换而非追加。
#[test]
fn images_roundtrip_and_replace() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    let mut doc = flow(&[("一", "正文")]);
    store.save_flow(id, &doc, Format::Markdown).unwrap();

    let got = store.load_flow(id).unwrap().unwrap();
    assert_eq!(got.images.len(), 1);
    assert_eq!(got.images[0].href, "OEBPS/Images/fig1.png");
    assert_eq!(got.images[0].media_type, "image/png");
    assert_eq!((got.images[0].width, got.images[0].height), (640, 480));

    doc.images.clear();
    store.save_flow(id, &doc, Format::Markdown).unwrap();
    assert!(
        store.load_flow(id).unwrap().unwrap().images.is_empty(),
        "重存应替换旧图片，而不是留下孤行"
    );
}

#[test]
fn saving_a_flow_twice_replaces_rather_than_appends() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    store
        .save_flow(id, &flow(&[("一", "a"), ("二", "b")]), Format::Markdown)
        .unwrap();
    store.save_flow(id, &flow(&[("新", "c")]), Format::Markdown).unwrap();
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

/// 删书要把章节、图片、进度一起带走。表之间是外键级联，所以按**库文件里的表**验，
/// 不通过私有连接字段。
#[test]
fn removing_a_book_cascades_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    {
        let store = Store::open_at(&db).unwrap();
        let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
        store
            .save_flow(id, &flow(&[("一", "内容一")]), Format::Markdown)
            .unwrap();
        store
            .set_position(id, "continuous", &Locator::start())
            .unwrap();

        store.remove_book(id).unwrap();
        assert_eq!(store.book(id).unwrap(), None);
        assert!(store.load_flow(id).unwrap().is_none(), "章节随书一起消失");
        assert!(store.position(id).unwrap().is_none(), "进度随书一起消失");
    }

    let conn = rusqlite::Connection::open(&db).unwrap();
    let count = |table: &str| -> i64 {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count("chapters"), 0, "章节必须随书一起消失");
    assert_eq!(count("images"), 0, "图片必须随书一起消失");
}

/// 位置表与章表**一一对应**：空章、多字节的章、短章都不该错位。
///
/// 位置是打开一本书唯一的钥匙，错一格就是「翻到第二章看到第三章的字」。
#[test]
fn the_locator_table_lines_up_with_the_chapters() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    let long = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。".repeat(200);
    let doc = flow(&[("第一章", &long), ("第二章", ""), ("第三章", "短")]);
    store
        .save_content(id, &Content::Reflow(doc), Format::Markdown)
        .unwrap();

    let back = store.load_flow(id).unwrap().expect("该读回来");
    assert_eq!(back.chapters.len(), 3);
    for (ix, chapter) in back.chapters.iter().enumerate() {
        assert_eq!(
            chapter.locator(),
            Some(&ChapterLocator::Section(ix)),
            "单文件格式按序号认章"
        );
    }
    assert_eq!(back.chapters[1].source(), "", "正文不该跟着回来");
}

/// **正文根本不在库里**：落库的是位置，不是那几万个字。
///
/// 这条是 `plans/text-out-of-db.md` 的核心断言。它比「压缩后小一半」强：
/// 不管怎么压，库里都不应该有读者要读的那段文字。
#[test]
fn the_text_never_reaches_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    let store = Store::open_at(&db).unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    let long = "于是我们奋力向前，逆水行舟，被不断地向后推，推入过去。".repeat(2000);
    // 走**生产那条路**：带上格式，`save_flow_in` 于是把位置算成 `Section`。
    let doc = flow(&[("一", &long)]);
    store
        .save_content(id, &Content::Reflow(doc), Format::Markdown)
        .unwrap();
    drop(store);

    let conn = rusqlite::Connection::open(&db).unwrap();
    let locator: String = conn
        .query_row("SELECT locator FROM chapters", [], |r| r.get(0))
        .unwrap();
    assert_eq!(locator, r#"{"Section":0}"#);
    // 库里任何一列的字节里都不该出现正文的片段。
    let probe = "逆水行舟，被不断地向后推";
    let hits: i64 = conn
        .query_row(
            "SELECT count(*) FROM chapters WHERE locator LIKE ?1 OR title LIKE ?1",
            rusqlite::params![format!("%{probe}%")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hits, 0, "正文那几万个字不该出现在章表里");
}

/// **源文件不在了**：连数据一起删掉，而不是留一本书点开是空白。
///
/// 正文只在文件里（`plans/text-out-of-db.md`），所以文件没了这本书就是死的。
/// 但「看不见」有两种：文件真删了，还是盘/授权没挂上 —— 后者把整库删空是灾难，
/// 所以判据里加了一条「上一层目录读得到」。
#[test]
fn a_book_whose_source_is_gone_is_pruned_with_its_data() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    let store = Store::open_at(&db).unwrap();

    // 一本文件还在的、一本文件被删的。
    let kept = dir.path().join("kept.md");
    std::fs::write(&kept, "# 一\n\n正文\n").unwrap();
    let gone = dir.path().join("gone.md");
    std::fs::write(&gone, "# 二\n\n正文\n").unwrap();
    let kept_id = store.add_book(&kept, &meta("留着")).unwrap();
    let gone_id = store.add_book(&gone, &meta("没了")).unwrap();
    store
        .save_flow(kept_id, &flow(&[("一", "正文")]), Format::Markdown)
        .unwrap();
    store
        .save_flow(gone_id, &flow(&[("二", "正文")]), Format::Markdown)
        .unwrap();
    store.set_position(gone_id, "page", &Locator::start()).unwrap();
    std::fs::remove_file(&gone).unwrap();

    let pruned = store.prune_missing().unwrap();
    assert_eq!(pruned, vec!["没了".to_string()]);
    assert!(store.book(gone_id).unwrap().is_none(), "书要没");
    assert!(store.load_flow(gone_id).unwrap().is_none(), "章节要跟着没");
    assert!(store.position(gone_id).unwrap().is_none(), "进度要跟着没");
    assert!(store.book(kept_id).unwrap().is_some(), "文件还在的那本不能动");

    // 盘点不出来（把上一层目录也删了）时**不删**：那是「现在看不见」，不是「没了」。
    let detached = dir.path().join("外接盘/book.md");
    std::fs::create_dir_all(detached.parent().unwrap()).unwrap();
    std::fs::write(&detached, "# 三\n\n正文\n").unwrap();
    let detached_id = store.add_book(&detached, &meta("外接盘")).unwrap();
    std::fs::remove_dir_all(dir.path().join("外接盘")).unwrap();
    assert!(store.prune_missing().unwrap().is_empty(), "目录都读不到就别动");
    assert!(store.book(detached_id).unwrap().is_some());
}

/// 文件换了 / 索引是旧算法算的，都要认得出来。
#[test]
fn a_changed_file_or_an_old_index_is_reported_as_stale() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    let path = dir.path().join("a.md");
    std::fs::write(&path, "# 一\n\n正文\n").unwrap();
    let store = Store::open_at(&db).unwrap();
    let id = store.add_book(&path, &meta("A")).unwrap();
    store
        .save_flow(id, &flow(&[("一", "正文")]), Format::Markdown)
        .unwrap();
    assert_eq!(store.stale_reason(id).unwrap(), None, "刚导完该是可信的");

    // 文件被换掉（写新内容，mtime 与大小都变）。
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&path, "# 一\n\n换了内容\n\n# 二\n\n又一章\n").unwrap();
    assert_eq!(
        store.stale_reason(id).unwrap(),
        Some(StaleReason::Changed),
        "文件换过要认出来"
    );

    // 重扫之后又可信了，而且章表跟着新的文件走。
    folio_core_reindex(&store, id);
    assert_eq!(store.stale_reason(id).unwrap(), None);
    assert_eq!(store.load_flow(id).unwrap().unwrap().chapters.len(), 2);

    // 索引是旧算法算的。
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("UPDATE books SET index_version = 0 WHERE id = ?1", [id])
            .unwrap();
    }
    assert_eq!(
        store.stale_reason(id).unwrap(),
        Some(StaleReason::OldIndex)
    );

    // 文件删了：这是「读不了」，比另外两种严重。
    std::fs::remove_file(&path).unwrap();
    assert_eq!(store.stale_reason(id).unwrap(), Some(StaleReason::Missing));
}

fn folio_core_reindex(store: &Store, id: i64) {
    crate::reindex(store, id).unwrap();
}

/// 打开过的书排在从没打开过的前面。
#[test]
fn opened_books_rank_before_never_opened_ones() {
    let store = Store::in_memory().unwrap();
    let a = store.add_book(&path("a.md"), &meta("A")).unwrap();
    let _b = store.add_book(&path("b.md"), &meta("B")).unwrap();
    store.touch_opened(a).unwrap();
    let books = store.list_books().unwrap();
    assert_eq!(books.len(), 2);
    assert_eq!(books[0].title, "A", "打开过的排最前，没打开过的垫后");
}

/// 书架的一条：封面字节跟着书目一起回来，排序与 `list_books` 同一条规则。
///
/// 封面存在 `books.cover`，但只有 `save_flow` 会写它，所以这条测试必须走
/// `add_book` + `save_flow` 两步，而不是只看 `add_book`。
#[test]
fn shelf_returns_cover_bytes_and_ranks_like_list_books() {
    let store = Store::in_memory().unwrap();
    let a = store.add_book(&path("a.epub"), &meta("A")).unwrap();
    let mut flow_a = flow(&[("一", "正文")]);
    flow_a.cover = Some(png_header());
    store.save_flow(a, &flow_a, Format::Markdown).unwrap();
    let b = store.add_book(&path("b.md"), &meta("B")).unwrap();
    store
        .save_flow(b, &flow(&[("一", "正文")]), Format::Markdown)
        .unwrap();

    store.touch_opened(b).unwrap();
    let shelf = store.shelf("").unwrap();
    assert_eq!(shelf.len(), 2, "空查询就是整架");
    assert_eq!(shelf[0].title, "B", "打开过的排最前");
    assert_eq!(shelf[1].title, "A");
}

/// `BookRow` 那些项之外的两列都得对：`cover` 是卡片的主视觉，`last_opened_at`
/// 决定「继续读」那格的措辞。
#[test]
fn shelf_carries_the_cover_and_the_opened_stamp() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.epub"), &meta("蛇结")).unwrap();
    let mut flow_a = flow(&[("一", "正文")]);
    flow_a.cover = Some(png_header());
    store.save_flow(id, &flow_a, Format::Markdown).unwrap();

    let item = &store.shelf("蛇结").unwrap()[0];
    assert_eq!(item.id, id);
    assert_eq!(
        item.authors,
        vec!["作者甲".to_string(), "作者乙".to_string()]
    );
    assert_eq!(item.format, "markdown");
    assert_eq!(item.cover.as_deref(), Some(png_header().as_slice()));
    assert_eq!(item.last_opened_at, None, "没打开过就是 NULL，不是 0");
}

/// 没封面的书也得进架（Markdown / TXT 恒如此），否则「导入了一本但看不到」
/// 就成了没法和「导入失败」区分开的问题。
#[test]
fn shelf_includes_books_without_a_cover() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.md"), &meta("无封面")).unwrap();
    // `flow()` 那个公共夹具带着一份假封面，这里要的是真·没封面的一本。
    let mut no_cover = flow(&[("一", "正文")]);
    no_cover.cover = None;
    store.save_flow(id, &no_cover, Format::Markdown).unwrap();
    let shelf = store.shelf("").unwrap();
    assert_eq!(shelf.len(), 1);
    assert_eq!(shelf[0].cover, None);
}

#[test]
fn shelf_filters_by_title_or_author() {
    let store = Store::in_memory().unwrap();
    for (name, title, author) in [
        ("a.epub", "蛇结", "弗朗索瓦·莫里亚克"),
        ("b.epub", "窄门", "安德烈·纪德"),
        ("c.md", "纪德的日记", "安德烈·纪德"),
    ] {
        let id = store
            .add_book(&path(name), &meta_by(title, &[author]))
            .unwrap();
        store
            .save_flow(id, &flow(&[("一", "正文")]), Format::Markdown)
            .unwrap();
    }
    let by_title = store.shelf("蛇结").unwrap();
    assert_eq!(by_title.len(), 1);
    assert_eq!(by_title[0].title, "蛇结");

    // 作者那一列存的是换行拼接的多个人名，LIKE 是子串匹配，因此照样命中。
    let by_author = store.shelf("纪德").unwrap();
    assert_eq!(by_author.len(), 2, "作者命中要带上两本");

    assert!(store.shelf("加缪").unwrap().is_empty());
    assert_eq!(store.shelf("  蛇结  ").unwrap().len(), 1, "首尾空白先去掉");
}

/// 读者改掉自己认的书名与作者：写回的只有那两列，而且**立刻**参与书架搜索。
///
/// 钉四件编译看不出来的事：多个人名按 `\n` 存、读回还是原来那几个人（界面那侧串成
/// 一行摆，写回再拆开，两侧必须认同一个编码，否则「A · B」会变成一个人 `A · B`）；
/// 改完按新书名搜得到、按旧书名搜不到（架上那根框查的就是这两列）；`format` / `size`
/// 这些文件的事实不被顺手改掉；作者清空后读回是空 `Vec`（界面上那一行随之不摆）。
#[test]
fn editing_the_title_and_authors_roundtrips_and_refilters() {
    let store = Store::in_memory().unwrap();
    let id = store
        .add_book(&path("a.epub"), &meta_by("未命名", &["出版社"]))
        .unwrap();
    store
        .save_flow(id, &flow(&[("一", "正文")]), Format::Markdown)
        .unwrap();

    store
        .set_book_meta(id, "窄门", &["安德烈·纪德".into(), "译者甲".into()])
        .unwrap();
    let row = store.book(id).unwrap().expect("改完还在库里");
    assert_eq!(row.title, "窄门");
    assert_eq!(
        row.authors,
        vec!["安德烈·纪德", "译者甲"],
        "多位作者要按原来的人数读回来"
    );
    assert_eq!(row.format, "markdown", "文件的事实不该被碰到");
    assert_eq!(row.size, 1234, "同上");
    // 那一屏的封面走单独一条取字节：`BookRow` 不带图（整库握在手里时不需要）。
    assert!(
        store.book_cover(id).unwrap().is_some(),
        "点开信息那一屏要能单独拿到这一本的封面字节"
    );
    assert!(
        store.book_cover(4242).unwrap().is_none(),
        "没有那一行给 None，不是报错"
    );

    assert_eq!(store.shelf("窄门").unwrap().len(), 1, "新书名立刻可搜");
    assert_eq!(store.shelf("纪德").unwrap().len(), 1, "新作者立刻可搜");
    assert!(store.shelf("未命名").unwrap().is_empty(), "旧书名不再命中");

    store.set_book_meta(id, "窄门", &[]).unwrap();
    assert!(
        store.book(id).unwrap().unwrap().authors.is_empty(),
        "清空作者读回应是空 `Vec`"
    );

    // 没有那一行：只留一条日志，不报错（与 `retarget_book_path` 同一语义）。
    store.set_book_meta(4242, "别的书", &[]).unwrap();
}

/// 书名里的 `%` 和 `_` 是字面字符，不是通配符。
///
/// 不转义的话搜 `100%` 会把整个书架列出来（`%` 匹配任意串），
/// 搜 `_门` 会命中「窄门」以外的任何两字书名（`_` 匹配任意单字）。
#[test]
fn shelf_treats_like_wildcards_in_query_as_text() {
    let store = Store::in_memory().unwrap();
    for title in ["100% 数学", "窄门", "X门"] {
        let id = store.add_book(&path(title), &meta(title)).unwrap();
        store
            .save_flow(id, &flow(&[("一", "正文")]), Format::Markdown)
            .unwrap();
    }
    assert_eq!(store.shelf("100%").unwrap().len(), 1);
    assert_eq!(store.shelf("_门").unwrap().len(), 0, "下划线不该当通配符用");
    assert_eq!(
        store.shelf(r"a\b").unwrap().len(),
        0,
        "转义符本身也要当字面量"
    );
}

/// 封面的媒体类型：`books.cover` 那一列只有字节，所以格式得按字节头反推。
/// 每种都写满探测窗口（前 16 字节），因为 `with_guessed_format` 只读那段。
#[test]
fn guess_media_type_reads_the_header_of_a_cover_with_no_media_type() {
    assert_eq!(guess_media_type(&png_header()), Some("image/png"));
    assert_eq!(
        guess_media_type(&[
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, b'J', b'F', b'I', b'F', 0, 0, 0, 0, 0, 0, 0
        ]),
        Some("image/jpeg")
    );
    assert_eq!(
        guess_media_type(b"GIF89a\0\0\0\0\0\0\0\0\0\0"),
        Some("image/gif")
    );
    assert_eq!(
        guess_media_type(b"RIFF\x04\x00\x00\x00WEBPVP8 "),
        Some("image/webp")
    );
    assert_eq!(
        guess_media_type(b"BM\0\0\0\0\0\0\0\0\0\0\0\0\0\0"),
        Some("image/bmp")
    );
    // 认不出就是 None，让调用方去画占位 —— 猜成 PNG 去解一次只会静默画空。
    assert_eq!(guess_media_type(b"<svg xmlns='x'></svg>"), None);
    assert_eq!(guess_media_type(b""), None);
}

/// 一段够探测用的 PNG 头（magic + IHDR 的前几字节）。
fn png_header() -> Vec<u8> {
    vec![
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, b'I', b'H', b'D', b'R',
    ]
}

fn meta_by(title: &str, authors: &[&str]) -> DocMeta {
    DocMeta {
        title: title.into(),
        authors: authors.iter().map(|a| (*a).to_string()).collect(),
        format: Format::Markdown,
        size: 1234,
    }
}

/// v12 迁移：老库升上来要拿到 `content_hash` / `hash_algo` 两列，而**老行留空**（懒补）。
///
/// 这一条钉三件事，每件都是选「懒补」而不是「迁移里一次算完」要买的东西：
/// 1. 开库**不读磁盘**：几百本的一次性顺序读不该压在开库那一下
///    （`plans/基础设施.md` §1.2 第 1 条），所以迁移里没有回填循环；
/// 2. 「还没算过」是一个能表达的状态（`NULL`），不是空串，也不是某个假的哈希；
/// 3. 旧数据照旧读得出来 —— 加两列不许动到书目、章节、进度任何一行。
#[test]
fn the_v12_migration_adds_identity_columns_without_backfilling_them() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    {
        // 手工造一个 v11 库：跑前 11 条迁移，塞一本带章带进度的书，版本停在 11。
        let conn = rusqlite::Connection::open(&db).unwrap();
        for sql in &MIGRATIONS[..11] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 11).unwrap();
        conn.execute(
            "INSERT INTO books (path, title, authors, format, size, added_at)
             VALUES ('/books/老名字.md', '老名字', '作者甲', 'markdown', 10, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chapters (book_id, idx, title, locator)
             VALUES (1, 0, '第一章', '{\"v\":1,\"chapter\":0,\"offset\":0}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO positions (book_id, mode, locator_json, updated_at)
             VALUES (1, 'scroll', '{\"v\":1,\"chapter\":0,\"offset\":12}', 0)",
            [],
        )
        .unwrap();
    }

    let store = Store::open_at(&db).unwrap();
    assert_eq!(
        store.user_version().unwrap(),
        MIGRATIONS.len() as i64,
        "v12 要跑到位"
    );

    // 旧数据照旧：书目、章节位置、进度都读得出来。
    let rows = store.list_books().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "老名字");
    let flow = store.load_flow(rows[0].id).unwrap().expect("章表还在");
    assert_eq!(flow.chapters[0].title, "第一章");
    let (mode, locator) = store.position(rows[0].id).unwrap().expect("进度还在");
    assert_eq!(mode, "scroll");
    assert_eq!(locator.offset, 12);

    // 身份列在，而老行是空的 —— 这就是「懒补」的形状。
    let missing = store.rows_missing_hash().unwrap();
    assert_eq!(
        missing.len(),
        1,
        "老行一条都没补，等后台那一趟（或它自己被碰到）"
    );
    assert_eq!(missing[0].id, rows[0].id);
    assert!(
        store
            .book_by_hash("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
            .unwrap()
            .is_none(),
        "没算过的行不许被任何哈希命中"
    );

    // 补上之后：从「缺身份」那张清单里消失，而按哈希能找回来。
    let hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    store
        .set_content_hash(rows[0].id, hash, crate::identity::HASH_ALGO)
        .unwrap();
    assert!(store.rows_missing_hash().unwrap().is_empty());
    assert_eq!(
        store.book_by_hash(hash).unwrap().unwrap().id,
        rows[0].id,
        "判重的入口要认得补过的那一行"
    );

    // 记下的是算法名，换算法时这是唯一线索。
    let algo: String = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row("SELECT hash_algo FROM books WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(algo, crate::identity::HASH_ALGO);

    // 索引只建在非空那一段上：老库此刻绝大多数行是 NULL，全表索引是给空值建 B 树。
    let indexed: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'idx_books_content_hash'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(indexed, 1, "那条 partial index 要在");

    drop(store);
    // 重开不重复跑迁移，也不把补好的哈希冲掉。
    let again = Store::open_at(&db).unwrap();
    assert_eq!(again.user_version().unwrap(), MIGRATIONS.len() as i64);
    assert!(again.rows_missing_hash().unwrap().is_empty());
}
