//! 存储层：迁移、书库、章节/图片/进度/偏好的往返与级联。只依赖公开 API。
//!
//! 级联那条测试要看「牌面之下的表」，但绝不碰私有字段：用公开的 `Store::open_at`
//! 建库，再用一条裸 `rusqlite` 连接按**库文件契约**查计数。

use std::path::{Path, PathBuf};

use crate::Store;
use crate::content::format::{
    Chapter, ChapterLocator, Content, DocMeta, FixedDoc, FixedPage, FlowDoc, Format, ImageRes,
    PageSource, Series, TocEntry, guess_media_type,
};
use crate::position::Locator;
use crate::store::{MIGRATIONS, StaleReason};

fn meta(title: &str) -> DocMeta {
    DocMeta {
        title: title.into(),
        authors: vec!["作者甲".into(), "作者乙".into()],
        format: Format::Markdown,
        size: 1234,
        ..DocMeta::default()
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
        // 文档级四项一起进往返测试：EPUB 的 CSS / 目录 / 封面 / 翻页方向走的是 books 表的列。
        css: "p { text-indent: 2em }".into(),
        css_hrefs: Vec::new(),
        // 故意取 `true`：这一格默认是 `false`，填 false 的往返测试什么都守不住
        // （漏写 INSERT 或 SELECT 都是绿）。
        progression_rtl: true,
        toc: vec![TocEntry {
            title: parts.first().map(|t| t.0.to_string()).unwrap_or_default(),
            href: "#0".into(),
            fragment: Some("sec1".into()),
            children: Vec::new(),
        }],
        cover: Some(vec![0x89, b'P', b'N', b'G', 13, 10]),
        // 两张图一起进往返：EPUB 的内嵌图走 images 表。第二张是 EPUB3 挂降级路那种
        // （正文的 `src` 指 SVG，字节在 PNG），它钉的是 `source` 这一列也得原样回来 ——
        // 键与去处只要有一格落错，那张图就既画不出也查不到尺寸。
        images: vec![
            ImageRes {
                href: "OEBPS/Images/fig1.png".into(),
                source: "OEBPS/Images/fig1.png".into(),
                media_type: "image/png".into(),
                width: 640,
                height: 480,
            },
            ImageRes {
                href: "OEBPS/Images/chart.svg".into(),
                source: "OEBPS/Images/chart.png".into(),
                media_type: "image/png".into(),
                width: 40,
                height: 20,
            },
        ],
        // 封面**落盘之后**才有描述，而这一份是直接构造的（没有文件）——
        // 所以留 `None`，读那一侧退回 `cover` 那个 BLOB。这正是老库的形状。
        cover_ref: None,
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
        // 一章：`load_flow` 对没有章节行的库回 `None`（那是「还没扫过」而不是「扫出个空的
        // 库」），而下面要从 `load_flow` 那一边读这一张图。
        conn.execute(
            "INSERT INTO chapters (book_id, idx, title, spine_href, char_len)
             VALUES (1, 0, '第一章', 'OEBPS/c1.xhtml', 0)",
            [],
        )
        .unwrap();
    }

    let store = Store::open_at(&db).unwrap();
    assert_eq!(store.user_version().unwrap(), MIGRATIONS.len() as i64);
    // v17 给 `images` 加的那一列在老行上是空的，而空串按「与 `href` 同一条」读回 ——
    // 老库本来就没有降级路可谈。这一句同时是那条读法的反证：把它改成原样交回空串，
    // 取字节那一路就拿着一个空条目名去问容器，症状是老书里的图全都不见了。
    let old = store.load_flow(1).unwrap().expect("图片表要跟着迁过来");
    assert_eq!(old.images.len(), 1);
    assert_eq!(
        old.images[0].source, "OEBPS/i.png",
        "老行的空 source 该读成它自己：{:?}",
        old.images[0]
    );
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
            extra_images: Vec::new(),
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
    assert_eq!(version, crate::content::format::INDEX_VERSION);
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
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        conn.query_row("PRAGMA page_count", [], |r| r.get(0))
            .unwrap()
    };

    let store = Store::open_at(&db).unwrap();
    let id = store.add_book(&path("a.md"), &meta("A")).unwrap();
    // 正文与封面字节都不进库（v16）之后，能撑大文件的只剩章表、目录、图片元数据 ——
    // 量小得多，所以这里拿**很多章**来撑：`DELETE FROM chapters` 是整批删，
    // 正是 `reclaim` 要还页的那种。
    let chapters: Vec<(String, String)> = (0..400)
        .map(|i| (format!("第{i}章"), format!("<p>正文{i}</p>")))
        .collect();
    let parts: Vec<(&str, &str)> = chapters
        .iter()
        .map(|(t, b)| (t.as_str(), b.as_str()))
        .collect();
    store
        .save_flow(id, &flow(&parts), Format::Markdown)
        .unwrap();
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
/// `crate::content::document::book::tests` 里对容器测。
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
    assert_eq!(
        back.progression_rtl, doc.progression_rtl,
        "v18 那一列得跟着回来，否则 RTL 书重开就变成左翻"
    );
    // 封面字节**不回来**（v16）：库里只有 `cover_ref` 那两格描述，
    // 字节在 `covers/` 那个文件里。这一格是 `flow()` 造的假封面，它没有对应的
    // 文件，所以读回来是 `None` —— 这正是「字节不能进库」的形状。
    assert_eq!(back.cover, None, "封面字节不该跟着库回来");
    assert_eq!(back.cover_ref, None, "没有文件就没有描述");
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

/// 每一章的**图片引用清单**落库、读回，逐字不变。
///
/// 这一列是图片查看器「第 n 张 / 共 m 张」和「定位原文」的唯一来源，而它只在导入那一趟
/// 算得出来（正文之后就不在库里了）。写漏一句 INSERT 或读漏一句 SELECT，症状都是那一列
/// 静默少几张 —— 正文里那些图照样画得出来，没有第二个界面会喊这件事。
#[test]
fn the_image_uses_of_each_chapter_roundtrip_through_the_store() {
    let store = Store::in_memory().unwrap();
    let id = store.add_book(&path("a.epub"), &meta("A")).unwrap();
    let doc = flow(&[
        (
            "有图",
            "<body><p>甲乙</p><img src=\"a.png\"/><p>丙</p><img src=\"b.png\"></body>",
        ),
        ("没图", "<body>只有字</body>"),
    ]);
    store.save_flow(id, &doc, Format::Epub).unwrap();
    let back = store.load_flow(id).unwrap().unwrap();

    assert_eq!(
        back.chapters[0].images, doc.chapters[0].images,
        "两张都要回来，顺序与位置一样"
    );
    assert!(
        back.chapters[1].images.is_empty(),
        "没有图的章读回来是空表，不是「还没算」：{:#?}",
        back.chapters[1].images
    );
    // 数字本身再钉一次：只存 `src` 不存 `offset` 的那种往返，两边都缺同一格，
    // 上面那句相等断言照样绿，而「定位原文」会跳到章首。
    let listed: Vec<(&str, usize)> = back.chapters[0]
        .images
        .iter()
        .map(|u| (u.src.as_str(), u.offset))
        .collect();
    assert_eq!(
        listed,
        [("a.png", 2), ("b.png", 3)],
        "「甲乙」两字、图、「丙」一字、图 —— 位置是它前面已经数到的可见字符"
    );
}

/// v20 迁移：旧库升上来多出 `chapters.images_json`，**老行全部取默认值（空表）**。
///
/// 不回填是有意的一条：回填要把每本的容器再开一次读一遍正文，而这个库的规矩是
/// 「清单是导入那一趟顺手写的」。老行读出来是空表，界面按「这本书没有图」处理；
/// 把值补回来靠 `INDEX_VERSION` 这一版的 bump 在开书时触发重扫。
#[test]
fn the_v20_migration_adds_image_uses_empty() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    {
        // 手工造一个 v19 库：跑前 19 条迁移，塞一本带一章的书。
        let conn = rusqlite::Connection::open(&db).unwrap();
        for sql in &MIGRATIONS[..19] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 19).unwrap();
        conn.execute(
            "INSERT INTO books (path, title, authors, format, size, added_at)
             VALUES ('a.epub', 'A', '', 'epub', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chapters (book_id, idx, title, locator, char_len)
             VALUES (1, 0, '第一章', '{\"Entry\":\"OEBPS/c1.xhtml\"}', 10)",
            [],
        )
        .unwrap();
    }

    let store = Store::open_at(&db).unwrap();
    assert_eq!(
        store.user_version().unwrap(),
        MIGRATIONS.len() as i64,
        "v20 要跑到位"
    );
    let flow = store.load_flow(1).unwrap().expect("旧行也读得到章表");
    assert_eq!(flow.chapters.len(), 1);
    assert_eq!(
        flow.chapters[0].char_len(),
        Some(10),
        "同一行里字数那格不该被这一版带歪"
    );
    assert!(
        flow.chapters[0].images.is_empty(),
        "老行的清单是「这一版还没扫过」，读成空表：{:#?}",
        flow.chapters[0].images
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
    assert_eq!(got.images, doc.images, "两张图的每一格都得原样回来");
    // 第二张是挂降级路那一种：正文的 `src` 与字节所在不是同一条目。这一格落错的话
    // 症状是「图上画的是解不动的那份字节」，而清单里看着一切正常。
    assert_eq!(got.images[1].href, "OEBPS/Images/chart.svg", "键不许被换掉");
    assert_eq!(got.images[1].source, "OEBPS/Images/chart.png");

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
    store
        .save_flow(id, &flow(&[("新", "c")]), Format::Markdown)
        .unwrap();
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
    store
        .set_position(gone_id, "page", &Locator::start())
        .unwrap();
    std::fs::remove_file(&gone).unwrap();

    let pruned = store.prune_missing().unwrap();
    assert_eq!(pruned, vec!["没了".to_string()]);
    assert!(store.book(gone_id).unwrap().is_none(), "书要没");
    assert!(store.load_flow(gone_id).unwrap().is_none(), "章节要跟着没");
    assert!(store.position(gone_id).unwrap().is_none(), "进度要跟着没");
    assert!(
        store.book(kept_id).unwrap().is_some(),
        "文件还在的那本不能动"
    );

    // 盘点不出来（把上一层目录也删了）时**不删**：那是「现在看不见」，不是「没了」。
    let detached = dir.path().join("外接盘/book.md");
    std::fs::create_dir_all(detached.parent().unwrap()).unwrap();
    std::fs::write(&detached, "# 三\n\n正文\n").unwrap();
    let detached_id = store.add_book(&detached, &meta("外接盘")).unwrap();
    std::fs::remove_dir_all(dir.path().join("外接盘")).unwrap();
    assert!(
        store.prune_missing().unwrap().is_empty(),
        "目录都读不到就别动"
    );
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
    assert_eq!(store.stale_reason(id).unwrap(), Some(StaleReason::OldIndex));

    // 文件删了：这是「读不了」，比另外两种严重。
    std::fs::remove_file(&path).unwrap();
    assert_eq!(store.stale_reason(id).unwrap(), Some(StaleReason::Missing));
}

fn folio_core_reindex(store: &Store, id: i64) {
    crate::reindex(store, id).unwrap();
}

/// **重扫一本书不许动读者的数据**（`plans/导入管线.md` §九 那条门）。
///
/// 旧形状下这条会红，而且红在最要命的那一格：`annotations.chapter_id` 带着
/// `ON DELETE CASCADE` 挂在 `chapters(id)` 上，而 `save_flow_in` 落库的第一句是
/// 「删光这本书的章行再重插」—— 于是**每重扫一次就清空一次批注**，界面上那句
/// 「位置与批注都还在」是一句谎话。触发它的路是日常的：文件被换过，或者
/// `INDEX_VERSION` 被 bump 过一次（改分章规则时必须 bump）。
///
/// 五问合一条，因为「重扫」这个动作的正确性本来就是一个整体：
/// 不重读来源、不改文件名、不新增记录、章表真的跟着新内容走了、读者的批注还在。
/// 分开写会漏掉组合出来的状态（比如「章表更新了但批注没了」）。
#[test]
fn reindexing_keeps_the_reader_data_and_touches_no_new_record() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    // 副本躺在书库里，与真实导入之后的位置同形（重扫读的是它，不是来源）。
    let copy = dir.path().join("copy.md");
    std::fs::write(&copy, "# 一\n\n正文\n").unwrap();

    let store = Store::open_at(&db).unwrap();
    let id = store.add_book(&copy, &meta("A")).unwrap();
    store
        .save_flow(id, &flow(&[("一", "正文")]), Format::Markdown)
        .unwrap();
    let before_rows = store.list_books().unwrap().len();

    // 读者在这一章划了一条批注：章 0 的第 1 个字之后（插入点，避开长度边界）。
    let anchor = crate::content::anchor::TextAnchor {
        chapter: 0,
        start: 1,
        end: 1,
        exact: String::new(),
        prefix: String::new(),
        suffix: String::new(),
        path: "/html[1]/body[1]/p[1]#0".into(),
    };
    store
        .add_annotation(id, &anchor, "这一段写得真好", "amber")
        .unwrap();
    assert_eq!(store.annotations(id).unwrap().len(), 1);

    // 文件被换掉（多了一章），然后重扫 —— 走的就是 `stale_reason` 之后那条路。
    std::fs::write(&copy, "# 一\n\n正文\n\n# 二\n\n又一章\n").unwrap();
    crate::reindex(&store, id).unwrap();

    assert_eq!(
        store.load_flow(id).unwrap().unwrap().chapters.len(),
        2,
        "章表要跟着新文件走，否则这条测试没在验真的重扫"
    );
    assert_eq!(
        store.annotations(id).unwrap().len(),
        1,
        "重扫不该清掉读者的批注（§九：历史批注一条都不漂）"
    );
    assert_eq!(
        store.book(id).unwrap().unwrap().path,
        copy.to_string_lossy(),
        "重扫不重读来源、也不改副本文件名"
    );
    assert_eq!(
        store.list_books().unwrap().len(),
        before_rows,
        "重扫不该新增记录"
    );
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
    // 书架项**不带封面字节**（v16）：字节在 `covers/` 那个文件里，
    // 由 `book_card` 按 `cover_ref` 去取。所以 `cover` 恒为 `None`，
    // 而「这一本有没有封面」看的是 `cover_ref`。
    assert_eq!(item.cover, None, "封面字节不再跟着书架项走");
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

/// 分组筛选那两头（某个组 / 未分组）都认同一个搜索词，且与整架那条是同一套 LIKE 语义。
///
/// 三条查询共用一段 `WHERE`（见 `query_clause`）：少带一处 `ESCAPE '\\'` 或漏掉作者那一列
/// 都不会报错，只会让读者在组里搜「100%」时把整组都列出来，而搜「纪德」时又少一本。
#[test]
fn collection_filters_honor_the_search_term() {
    let store = Store::in_memory().unwrap();
    let mut ids = Vec::new();
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
        ids.push(id);
    }
    let group = store.create_collection("想读").unwrap();
    store.add_to_collection(group, ids[0]).unwrap();
    store.add_to_collection(group, ids[1]).unwrap();

    assert_eq!(
        store.shelf_in_collection(group, "").unwrap().len(),
        2,
        "空词就是整组"
    );
    assert_eq!(store.shelf_in_collection(group, "蛇结").unwrap().len(), 1);
    assert_eq!(
        store.shelf_in_collection(group, "纪德").unwrap().len(),
        1,
        "作者那一列在组里同样命中（组里只有「窄门」是他）"
    );
    assert!(
        store.shelf_in_collection(group, "日记").unwrap().is_empty(),
        "组外那本不该混进来"
    );

    let loose = store.shelf_ungrouped("").unwrap();
    assert_eq!(loose.len(), 1, "没进组的只剩一本");
    assert_eq!(loose[0].title, "纪德的日记");
    assert!(
        store.shelf_ungrouped("蛇结").unwrap().is_empty(),
        "未分组那一头也认这个词"
    );
    assert_eq!(store.shelf_ungrouped("纪德").unwrap().len(), 1);
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
    // 这一本没有封面（`flow()` 那份假封面没有对应文件，v16 起字节不进库），所以回 `None`。
    assert!(
        store.book_cover(id).unwrap().is_none(),
        "没有封面文件就回 None，不退回任何库里的副本"
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
        ..DocMeta::default()
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
        .set_content_hash(rows[0].id, hash, crate::ingest::identity::HASH_ALGO)
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
    assert_eq!(algo, crate::ingest::identity::HASH_ALGO);

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

/// v14 迁移：请回 `chapters.char_len`，**老行留 NULL**（懒补），新行才带真数。
///
/// 这一条钉的是「`NULL` 与 `0` 必须分得开」这件事，因为它是这一列唯一容易做错的地方：
/// 折成一个数，进度条就会把「还没算过」的章当成「这一章是空的」，于是分母偏小、
/// 百分比偏大 —— 读者读到一半进度条已经满了，而且没有任何报错。
///
/// 三个来源都要能表达「知道」：`Chapter::inline`（正文在手，建的时候就数）、
/// provider 的 `index()`（导入那一趟数的）、以及老库读回来的 `NULL`（不知道）。
#[test]
fn the_v14_migration_brings_back_char_len_as_nullable() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    {
        // 手工造一个 v13 库：跑前 13 条迁移，塞一本带两章的书，版本停在 13。
        let conn = rusqlite::Connection::open(&db).unwrap();
        for sql in &MIGRATIONS[..13] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 13).unwrap();
        conn.execute(
            "INSERT INTO books (path, title, authors, format, size, added_at)
             VALUES ('/books/老.md', '老书', '作者甲', 'markdown', 10, 0)",
            [],
        )
        .unwrap();
        for idx in 0..2 {
            conn.execute(
                "INSERT INTO chapters (book_id, idx, title, locator)
                 VALUES (1, ?1, '旧章', '{\"v\":1,\"chapter\":0,\"offset\":0}')",
                [idx],
            )
            .unwrap();
        }
    }

    let store = Store::open_at(&db).unwrap();
    assert_eq!(
        store.user_version().unwrap(),
        MIGRATIONS.len() as i64,
        "v14 要跑到位"
    );

    // 老行读回来是「不知道」而不是「0 字」。
    let flow = store.load_flow(1).unwrap().expect("章表还在");
    assert_eq!(flow.chapters.len(), 2, "加一列不许动到已有的章");
    assert_eq!(
        flow.chapters[0].char_len(),
        None,
        "老行没算过字数，必须是 NULL 而不是 0"
    );
    assert_eq!(
        flow.char_len(),
        None,
        "有一章不知道，全书总字数就不能报一个数"
    );
    let (known, unknown) = flow.known_char_len();
    assert_eq!((known, unknown), (0, 2), "降级读数要能说清几章不知道");

    // 新写的一份：正文在手，字数建的时候就数出来，落库读回一致。
    let fresh = crate::content::format::FlowDoc {
        chapters: vec![
            crate::content::format::Chapter::inline("一", "<p>正文四个字</p>"),
            crate::content::format::Chapter::inline("二", "<p>短</p>"),
        ],
        ..Default::default()
    };
    assert_eq!(
        fresh.chapters[0].char_len(),
        Some(5),
        "可见文本是「正文四个字」五个字，标签不算"
    );
    let book = store.add_book(&path("新.md"), &meta("新书")).unwrap();
    store.save_flow(book, &fresh, Format::Markdown).unwrap();
    let back = store.load_flow(book).unwrap().unwrap();
    assert_eq!(back.chapters[0].char_len(), Some(5), "字数要能原样落库读回");
    assert_eq!(
        back.char_len(),
        Some(6),
        "两章一共 5 + 1 = 6 字，这就是全局字符进度的分母"
    );
}

/// **书架上的已读比例**：`(前面各章字数之和 + 章内偏移) ÷ 全书总字数`。
///
/// 这条门护的是三件事，每一件错了都只会在界面上表现为「进度条数字不对」——
/// 没有报错、没有崩溃，所以必须有断言：
///
/// 1. **数字要对**：它是**数字数**算的，不是排版几何。所以这一条完全不需要窗口、
///    不需要排版，也不需要打开书 —— 这正是书架能显示「读了 37%」的原因，也正是
///    两台屏宽不同的设备会显示同一个数的原因（`plans/Locator.md` 第二节）。
/// 2. **没打开过就没有进度**，而不是 0%。合成 0 会让「从没打开」与「刚翻开第一页」
///    在架上长得一样。
/// 3. **有一章字数不知道就不报进度**，而不是报一个偏大的数。`SUM` 会跳过 NULL，
///    分母因此偏小、百分比偏大 —— 读者读到一半进度条已经满了，而且无人报警。
///    这是 v14 那一列可空带来的唯一风险，所以单独钉一条。
#[test]
fn shelf_reports_progress_from_character_counts() {
    let store = Store::in_memory().unwrap();
    let book = store.add_book(&path("a.md"), &meta("A")).unwrap();

    // 三章，字数由 `visible_chars` 数出来：各 10 / 20 / 30 字。
    let chapters: Vec<(String, String)> = [10, 20, 30]
        .iter()
        .map(|n| ("章".to_string(), format!("<p>{}</p>", "字".repeat(*n))))
        .collect();
    let parts: Vec<(&str, &str)> = chapters
        .iter()
        .map(|(t, s)| (t.as_str(), s.as_str()))
        .collect();
    store
        .save_flow(book, &flow(&parts), Format::Markdown)
        .unwrap();
    // 三章一共 60 字，是下面每个分母。

    // 从没打开过：没有进度，**不是** 0%。
    let item = |store: &Store| store.shelf("").unwrap().remove(0);
    assert_eq!(
        item(&store).progress,
        None,
        "没打开过的书不该报进度（0% 会让它和「刚翻开」长得一样）"
    );

    // 正中间：第 1 章（0 起）第 10 字 → (10 + 10) / 60。
    store
        .set_position(
            book,
            "scroll",
            &Locator {
                chapter: 1,
                offset: 10,
            },
        )
        .unwrap();
    let got = item(&store).progress.expect("位置在、字数全，该有进度");
    assert!(
        (got - 20.0 / 60.0).abs() < 1e-6,
        "第 1 章第 10 字该是 20/60，实际 {got}"
    );

    // 章首是 10/60，章尾是 30/60 —— 「章内偏移」那一格真的进了式子。
    store
        .set_position(
            book,
            "scroll",
            &Locator {
                chapter: 1,
                offset: 0,
            },
        )
        .unwrap();
    let at_head = item(&store).progress.unwrap();
    assert!(
        (at_head - 10.0 / 60.0).abs() < 1e-6,
        "第 1 章章首该是 10/60，实际 {at_head}"
    );

    // 越界偏移夹到 100%，不冲出进度条。
    store
        .set_position(
            book,
            "scroll",
            &Locator {
                chapter: 2,
                offset: 99_999,
            },
        )
        .unwrap();
    assert_eq!(item(&store).progress, Some(1.0), "越界偏移夹在 100%");

    // ── 现在把「有一章字数不知道」这一档单独验一遍 ──
    // 直接改库造出 v14 之前那种行：字数留 NULL。
    // 用文件库才能另开一条连接（与别的迁移测试同一条路）。
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    let store = Store::open_at(&db).unwrap();
    let book = store.add_book(&path("b.md"), &meta("B")).unwrap();
    store
        .save_flow(book, &flow(&parts), Format::Markdown)
        .unwrap();
    store
        .set_position(
            book,
            "scroll",
            &Locator {
                chapter: 1,
                offset: 10,
            },
        )
        .unwrap();
    assert!(
        store.shelf("").unwrap()[0].progress.is_some(),
        "前提：字数齐的时候是有进度的"
    );
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("UPDATE chapters SET char_len = NULL WHERE idx = 2", [])
            .unwrap();
    }
    assert_eq!(
        store.shelf("").unwrap()[0].progress,
        None,
        "有一章字数不知道就不报进度 —— 报出来的百分比会偏大（SUM 跳过 NULL）"
    );
}

/// 书架的**三条查询**（整架 / 集合内 / 未分组）都要能算出已读比例。
///
/// 这三条各写一句 SQL、共用一个 `SHELF_COLUMNS` 与 `shelf_from_row`，所以它们是
/// 「一处改动三处生效」的典型：加投影列时漏掉任何一条的 JOIN，那一处就会在运行期
/// 报「no such column: positions.locator_json」—— 而不是编译期。所以三条都过一遍。
///
/// 顺带钉住 JOIN 的**方向**：`LEFT JOIN` 而不是 `JOIN`。用内连接的话，
/// 「一本都没打开过的新书库」会查出**零行** —— 一整架书凭空消失，而这比没有
/// 进度条严重得多。
#[test]
fn every_shelf_query_can_report_progress() {
    let store = Store::in_memory().unwrap();
    let a = store.add_book(&path("a.md"), &meta("A")).unwrap();
    let b = store.add_book(&path("b.md"), &meta("B")).unwrap();
    let parts: Vec<(&str, &str)> = vec![("一", "<p>正文</p>")];
    store.save_flow(a, &flow(&parts), Format::Markdown).unwrap();
    store.save_flow(b, &flow(&parts), Format::Markdown).unwrap();

    // 一本都没打开过：三条路都必须把书查出来（LEFT JOIN 的意义）。
    assert_eq!(
        store.shelf("").unwrap().len(),
        2,
        "整架不该因为没进度就少书"
    );
    assert_eq!(
        store.shelf("").unwrap()[0].progress,
        None,
        "没打开过就没有进度"
    );

    let group = store.create_collection("合集").unwrap();
    store.add_to_collection(group, a).unwrap();
    assert_eq!(
        store.shelf_in_collection(group, "").unwrap().len(),
        1,
        "集合内那条要查得出来"
    );
    // 未分组那一组由 `collection_groups` 带出来（它是 `shelf_in_no_collection_in`
    // 唯一的调用方），所以从那里取。
    let groups = store.collection_groups().unwrap();
    let loose = groups
        .iter()
        .find(|g| g.name == crate::group::UNGROUPED)
        .expect("末尾该有「未分组」那一组");
    assert_eq!(loose.items.len(), 1, "未分组那条要查得出来");
    assert_eq!(loose.items[0].id, b);

    // 打开其中一本，三条路都要给出同一个比例。
    store
        .set_position(
            a,
            "scroll",
            &Locator {
                chapter: 0,
                offset: 1,
            },
        )
        .unwrap();
    let want = store
        .shelf("")
        .unwrap()
        .into_iter()
        .find(|i| i.id == a)
        .unwrap()
        .progress;
    assert!(want.is_some(), "开了就该有进度");
    let in_collection = store.shelf_in_collection(group, "").unwrap()[0].progress;
    assert_eq!(in_collection, want, "集合内那条给同一个数");
}

/// **封面落盘**：磁盘优先、BLOB 兜底，而且删一本书不许动别人共用的那张。
///
/// 这条门护的是三件事，每一件错了都只表现为「封面对了/没了」，没有报错：
///
/// 1. **磁盘优先**：新导入的书封面在 `covers/` 里，库里那两列只是文件名。
/// 2. **BLOB 兜底**：老库（v15 之前入库、还没被重扫过）与落盘失败的行只有 BLOB；
///    文件被清掉时也要能退回它 —— 否则读者的封面会凭空消失。
/// 3. **共用的那张不能删**：封面是**它自己字节**的身份，不是这本书的。两本书共用
///    一张封面时磁盘上只有一个文件，删掉一本不能把它带走。
#[test]
fn a_cover_lives_in_a_file_and_never_in_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    let root = dir.path(); // 书库根：副本直接躺在它上面
    let store = Store::open_at(&db).unwrap();

    // 两本共用同一份封面字节。
    let cover = b"pretend-jpeg-cover-bytes";
    let hash = crate::content::format::cover::write(root, cover, "image/jpeg").unwrap();
    let hash_hex = hash.to_hex();

    let mut with_cover = flow(&[("一", "正文")]);
    with_cover.cover = Some(cover.to_vec());
    with_cover.cover_ref = Some(crate::content::format::cover::CoverRef {
        hash: hash_hex.clone(),
        mime: "image/jpeg".into(),
    });

    let a = store.add_book(&root.join("a.md"), &meta("甲")).unwrap();
    let b = store.add_book(&root.join("b.md"), &meta("乙")).unwrap();
    store.save_flow(a, &with_cover, Format::Markdown).unwrap();
    store.save_flow(b, &with_cover, Format::Markdown).unwrap();

    // 1) 字节从文件来：两本都读得到，而且拿到的是同一份。
    assert_eq!(store.book_cover(a).unwrap().as_deref(), Some(&cover[..]));
    assert_eq!(store.book_cover(b).unwrap().as_deref(), Some(&cover[..]));

    // 两本指向**同一个**文件 —— 这是「按封面自己的字节哈希命名」的全部收益。
    let file_a = crate::content::format::cover::cover_path(root, &hash_hex, "image/jpeg");
    assert!(file_a.is_file(), "封面该真的落盘了");

    // 2) 两本共用一张时删掉一本不能把文件带走（封面是**它自己字节**的身份，
    //    不是这本书的）。
    assert!(
        store.orphan_cover_file(a).unwrap().is_none(),
        "b 还指着同一张，所以它不是孤儿"
    );
    store.remove_book(a).unwrap();
    assert_eq!(
        store.book_cover(b).unwrap().as_deref(),
        Some(&cover[..]),
        "删一本不该让另一本的封面消失"
    );

    // 3) **库里不存字节**：把文件删掉，封面就是没有了 —— 回 `None`，画占位。
    //
    // 这是 v16 有意选的一档，与 v15 正好相反。v15 靠 `books.cover` 那个 BLOB 兜底，
    // 代价是库随书数线性膨胀（封面是 MB 级的东西，而 `SHELF_COLUMNS` 每次列书目
    // 都要过一遍）。现在**不要这个兜底**：封面是装饰，不是内容的入口，
    // 宁可画占位也不让库背几百 MB 的字节。
    std::fs::remove_file(&file_a).unwrap();
    assert_eq!(
        store.book_cover(b).unwrap(),
        None,
        "文件不在就是没有封面，不退回任何库里的副本"
    );

    // 4) 没有 `cover_ref` 的行（Markdown / TXT，或落盘失败）就是没有封面。
    let plain = store.add_book(&root.join("c.md"), &meta("丙")).unwrap();
    store
        .save_flow(plain, &flow(&[("一", "正文")]), Format::Markdown)
        .unwrap();
    assert_eq!(store.book_cover(plain).unwrap(), None);
    assert!(store.orphan_cover_file(plain).unwrap().is_none());
}

/// 都柏林那几项写进去再读出来，一格都不该变样。
///
/// 单开一条而不是并进 `the_index_roundtrips_through_the_store`：多值的两格走换行拼接，
/// 系列的两格必须成对（空名字带一个孤立序号是脏数据），这两条规则各要一个断言才说清。
#[test]
fn the_dublin_core_fields_roundtrip_through_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_at(&dir.path().join("folio.db")).unwrap();
    let want = DocMeta {
        title: "车刀集".into(),
        authors: vec!["果戈理".into()],
        contributors: vec!["译者甲".into(), "绘者乙".into()],
        subjects: vec!["俄国小说".into(), "讽刺".into()],
        description: "一段长简介\n第二行也在".into(),
        publisher: "某某社".into(),
        published: "2023-05-01T00:00:00Z".into(),
        language: "zh-CN".into(),
        isbn: "978-7-02-000000-0".into(),
        dctype: "text".into(),
        rights: "版权由出版社持有".into(),
        series: Some(Series {
            name: "世界文学".into(),
            position: "12".into(),
        }),
        format: Format::Epub,
        size: 4096,
    };
    let id = store.add_book(&path("a.epub"), &want).unwrap();
    let got = store.book_meta(id).unwrap().expect("刚入库该行还在");

    assert_eq!(got.id, id);
    assert_eq!(got.title, "车刀集");
    assert_eq!(got.authors, want.authors);
    assert_eq!(
        got.contributors, want.contributors,
        "多值的贡献者要拆回两条"
    );
    assert_eq!(got.subjects, want.subjects);
    assert_eq!(
        got.description, want.description,
        "简介里的换行是内容，不是分隔符"
    );
    assert_eq!(got.publisher, "某某社");
    assert_eq!(got.published, want.published, "日期原样存，不折成日期类型");
    assert_eq!(got.language, "zh-CN");
    assert_eq!(got.isbn, "978-7-02-000000-0");
    assert_eq!(got.dctype, "text");
    assert_eq!(got.rights, "版权由出版社持有");
    assert_eq!(got.series, want.series);
    assert_eq!(got.format, "epub");
    assert_eq!(got.size, 4096);

    // 没写的那些不许凭空长出值来。
    let bare = store
        .book_meta(store.add_book(&path("b.epub"), &meta("只有名字")).unwrap())
        .unwrap()
        .expect("入库了就读得到");
    assert_eq!(bare.description, "");
    assert_eq!(bare.series, None, "没有系列名字时不造一个系列");
    assert!(bare.contributors.is_empty() && bare.subjects.is_empty());
    assert_eq!(bare.authors, meta("只有名字").authors, "作者那一格照旧");

    // 同一本书重扫一次（`add_book` 走 upsert），书目要跟着更新而不是留下一半旧的。
    let mut again = want.clone();
    again.description = "改过的简介".into();
    again.series = None;
    store.add_book(&path("a.epub"), &again).unwrap();
    let got = store.book_meta(id).unwrap().unwrap();
    assert_eq!(got.description, "改过的简介");
    assert_eq!(got.series, None, "系列被作者删掉之后不能留在库里那半对");
}

/// v19 迁移：旧库升上来多出都柏林那 11 列，**老行全部取默认值（空）**。
///
/// 不回填是有意的一条：回填要把每本的容器再开一次，而这个库的规矩是「书目是导入
/// 那一趟顺手写的」。老行读出来是空的，界面按「这本书没说」处理；`INDEX_VERSION`
/// 那一版的 bump 让重扫在开书时发生，才是把值补回来的那一步。
#[test]
fn the_v19_migration_adds_the_dublin_core_columns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("folio.db");
    {
        // 手工造一个 v18 库：跑前 18 条迁移，塞一行只带四格书目的老书。
        let conn = rusqlite::Connection::open(&db).unwrap();
        for sql in &MIGRATIONS[..18] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 18).unwrap();
        conn.execute(
            "INSERT INTO books (path, title, authors, format, size, added_at)
             VALUES ('/books/老库.epub', '老库', '作者甲', 'epub', 10, 0)",
            [],
        )
        .unwrap();
    }

    let store = Store::open_at(&db).unwrap();
    assert_eq!(
        store.user_version().unwrap(),
        MIGRATIONS.len() as i64,
        "v19 要跑到位"
    );
    let rows = store.list_books().unwrap();
    assert_eq!(rows.len(), 1, "旧行照旧读得出来");
    assert_eq!(rows[0].title, "老库");

    let got = store.book_meta(rows[0].id).unwrap().expect("旧行也读得到");
    assert_eq!(got.contributors, Vec::<String>::new());
    assert_eq!(
        got.description, "",
        "老行没有简介可读，是「没说」不是「读错了」"
    );
    assert_eq!(got.series, None);
    assert_eq!(got.language, "");
}
