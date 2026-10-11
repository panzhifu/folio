//! 库文件的形状：迁移清单。
//!
//! 单独一个文件是因为它是**只增不改的日志**（已经发布过的迁移不能回头改形状），
//! 混在 `Store` 的操作里会让人分不清「哪条是历史、哪条是现在」。
//!
//! 需要读进来再写回去的迁移（v8 的正文压缩，SQLite 没有压缩函数）只有 SQL 这半，
//! 那一半在 [`super::Store::finish_v8`]。

/// 迁移清单。索引即版本号：`MIGRATIONS[2]` 把库带到 v3。
///
/// 每条各自一句：`execute_batch` 中间失败时，日志里最后一条「完成」的就是该看的那条 ——
/// 库停在 v3 还是 v4，决定的是后面怎么修。
pub(super) const MIGRATIONS: &[&str] = &[
    // v1：书库、章节、进度、偏好、标注、集合。
    // 标注与集合的表在 v1 一次建好 —— 已经发布过的迁移不能回头改形状。
    r#"
CREATE TABLE books (
    id             INTEGER PRIMARY KEY,
    path           TEXT    NOT NULL UNIQUE,
    title          TEXT    NOT NULL,
    authors        TEXT    NOT NULL DEFAULT '',
    format         TEXT    NOT NULL,
    size           INTEGER NOT NULL DEFAULT 0,
    cover          BLOB,
    added_at       INTEGER NOT NULL,
    last_opened_at INTEGER
);

CREATE TABLE chapters (
    id         INTEGER PRIMARY KEY,
    book_id    INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    idx        INTEGER NOT NULL,
    title      TEXT    NOT NULL DEFAULT '',
    spine_href TEXT    NOT NULL DEFAULT '',
    char_len   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_chapters_book ON chapters(book_id, idx);

CREATE TABLE chapter_text (
    chapter_id INTEGER PRIMARY KEY REFERENCES chapters(id) ON DELETE CASCADE,
    text       TEXT NOT NULL
);

CREATE TABLE positions (
    book_id      INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
    mode         TEXT    NOT NULL,
    locator_json TEXT    NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE TABLE prefs (
    key        TEXT PRIMARY KEY,
    value_json TEXT NOT NULL
);

CREATE TABLE annotations (
    id          INTEGER PRIMARY KEY,
    book_id     INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    chapter_id  INTEGER REFERENCES chapters(id) ON DELETE CASCADE,
    anchor_json TEXT    NOT NULL,
    note        TEXT    NOT NULL DEFAULT '',
    color       TEXT    NOT NULL DEFAULT 'amber',
    stale       INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX idx_annotations_book ON annotations(book_id, chapter_id);

CREATE TABLE collections (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE collection_books (
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    book_id       INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    added_at      INTEGER NOT NULL,
    PRIMARY KEY (collection_id, book_id)
);
"#,
    // v2：全文检索。中文必须用 trigram —— FTS5 默认的 unicode61 会把一整段
    // 没有空格的中文当成单个 token，中文查询永远命中不了。
    r#"
CREATE VIRTUAL TABLE chapter_fts USING fts5(
    text,
    content='chapter_text',
    content_rowid='chapter_id',
    tokenize='trigram case_sensitive 0'
);

CREATE TRIGGER chapter_fts_ai AFTER INSERT ON chapter_text BEGIN
    INSERT INTO chapter_fts(rowid, text) VALUES (new.chapter_id, new.text);
END;

CREATE TRIGGER chapter_fts_ad AFTER DELETE ON chapter_text BEGIN
    INSERT INTO chapter_fts(chapter_fts, rowid, text) VALUES ('delete', old.chapter_id, old.text);
END;

CREATE TRIGGER chapter_fts_au AFTER UPDATE ON chapter_text BEGIN
    INSERT INTO chapter_fts(chapter_fts, rowid, text) VALUES ('delete', old.chapter_id, old.text);
    INSERT INTO chapter_fts(rowid, text) VALUES (new.chapter_id, new.text);
END;
"#,
    // v3：EPUB 的文档级数据。CSS 与目录属于整本书，不属于任何一章，
    // 所以它们跟着 books 走而不是被塞进每份章节源文本里重复存一遍。
    r#"
ALTER TABLE books ADD COLUMN css TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN toc_json TEXT NOT NULL DEFAULT '[]';
"#,
    // v4：整本书的图片资源。key 是解析后的容器绝对路径（与 chapter.spine_href 同形），
    // 排版与绘制都按它查表；固有尺寸在读取时算好，省得每次打开都重新解 header。
    r#"
CREATE TABLE images (
    id         INTEGER PRIMARY KEY,
    book_id    INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    href       TEXT    NOT NULL,
    media_type TEXT    NOT NULL DEFAULT '',
    width      INTEGER NOT NULL DEFAULT 0,
    height     INTEGER NOT NULL DEFAULT 0,
    bytes      BLOB    NOT NULL
);
CREATE INDEX idx_images_book ON images(book_id, href);
"#,
    // v5：图片字节不再入库，只留元数据（href / media_type / 宽高）。学 KOReader：
    // 原图随时能从容器里按 href 再拿一次，就不该在库里留第二份。
    //
    // 旧库的 `bytes` 在这里被丢掉。SQLite 的 `DROP COLUMN` 只改 schema、不还磁盘
    // （腾出的页给后续写入复用），所以库文件不会立即变小；真要在意体积，得手动
    // `VACUUM` —— 那是一次写全套，不在开书路径上做。
    r#"
ALTER TABLE images DROP COLUMN bytes;
"#,
    // v6：阅读统计。存**原始会话**而不是按天汇总 —— 「今天读了多久」依赖读者所在
    // 时区，而时区只有界面侧知道。core 只留事实（何时开始、多久、前进多少字），
    // 汇总时把偏移当参数收进来（见 [`crate::time`]），不把某个时区的日界线写死进表。
    r#"
CREATE TABLE reading_sessions (
    id         INTEGER PRIMARY KEY,
    book_id    INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    started_at INTEGER NOT NULL,
    ended_at   INTEGER NOT NULL,
    seconds    INTEGER NOT NULL,
    chars      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_sessions_book ON reading_sessions(book_id, started_at);
CREATE INDEX idx_sessions_time ON reading_sessions(started_at);
"#,
    // v7：定版式文档（漫画 / 将来的 PDF）。页表存成 JSON 一列 —— 页是一条线性的
    // 「一页接一页」，没有要按列查的东西，为它单建一张表只会多一层 join。
    // 图片字节照旧不入库（见 v5）：页表里的 href 只是去容器里现取的钥匙。
    r#"
ALTER TABLE books ADD COLUMN fixed_json TEXT NOT NULL DEFAULT '';
"#,
    // v8：**正文出库**。库里只留它在容器里的位置（`chapters.locator`）。
    //
    // 两件事：
    // 1. 摘掉 v2 建的 trigram 索引与三个触发器。实测它是库里最大的一项：一本
    //    二十万字的中文书，正文 0.44 MB，索引 1.3 MB（搜索推后，见
    //    `plans/text-out-of-db.md`）；搜索真要做时，索引是**可重建的派生数据**。
    // 2. 给章表加位置列。老库的行要按 `books.format` + `spine_href` + `idx` 算，
    //    SQL 里做不干净（JSON 转义），所以回填在 Rust 里，见 [`super::Store::finish_v8`]。
    r#"
DROP TRIGGER IF EXISTS chapter_fts_ai;
DROP TRIGGER IF EXISTS chapter_fts_ad;
DROP TRIGGER IF EXISTS chapter_fts_au;
DROP TABLE IF EXISTS chapter_fts;

ALTER TABLE chapters ADD COLUMN locator TEXT NOT NULL DEFAULT '';
"#,
    // v9：正文出库之后剩下的冗余与**文件指纹**。
    //
    // - `chapters.spine_href` 与位置列里的 `Entry` 是同一个字符串，两处存就是两处会
    //   不一致；图片的基准目录从位置推得出来（见 `Chapter::spine_href`）。
    // - `chapters.char_len` 只写不读，而出库之后它连写都写不出真值（要读正文才数得清）。
    // - `books.mtime`：正文现在**只在文件里**，文件被换了（重新下载 / 同步 / 改名）
    //   必须能被发现 —— 位置还在，指的却是别的内容。`size` 一样的概率不低，所以两样都存。
    // - `books.index_version`：分章规则或位置方案一改（比如 Markdown 的标题层级），
    //   老库里那些 `Section(n)` 就指到别的章去了，得能认出「这份索引是旧算法算的」。
    r#"
ALTER TABLE chapters DROP COLUMN spine_href;
ALTER TABLE chapters DROP COLUMN char_len;
ALTER TABLE books ADD COLUMN mtime INTEGER NOT NULL DEFAULT 0;
ALTER TABLE books ADD COLUMN index_version INTEGER NOT NULL DEFAULT 0;
"#,
];
