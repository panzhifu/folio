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
    // 汇总时把偏移当参数收进来（见 [`crate::infra::time`]），不把某个时区的日界线写死进表。
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
    // v7：定版式文档（漫画四种 / PDF / AZW4 / DjVu）。页表存成 JSON 一列 —— 页是一条线性的
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
    // v10：读过的章。目录那一屏要把「来过的章」和「没来的章」分开画（浅色 / 正常），
    // 而 `positions` 每本书只有一行 —— 它记的是「现在在哪儿」，答不了「到过哪些地方」。
    //
    // 键是 `chapters.idx` 而不是位置串：章号是界面上唯一用来寻址的东西，且
    // `books.index_version` 已经负责在分章规则改了的时候认出一份旧索引。
    // 只插不改（`INSERT OR IGNORE`）：这条记录说的是「来过」，第二次来不该把
    // 第一次的时间冲掉，也不该多出一行。
    r#"
CREATE TABLE chapter_read (
    book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    idx     INTEGER NOT NULL,
    read_at INTEGER NOT NULL,
    PRIMARY KEY (book_id, idx)
);
"#,
    // v11：封面缩略图的**规格版本**。`books.cover` 只是字节，没有「按什么规格生成的」，
    // 换了尺寸 / 编码之后老封面认不出来，只能一直用下去（`plans/基础设施.md` §5.1）。
    // 这一列记下生成它的 `format::COVER_SPEC_VERSION`；缺省的 0 = 迁移之前生成的封面，
    // 一律当旧规格，开书时由 `stale_reason` 认出来、走重扫重生成。
    r#"
ALTER TABLE books ADD COLUMN cover_spec INTEGER NOT NULL DEFAULT 0;
"#,
    // v12：**书的身份从「它在哪儿」换成「它是什么」**（`plans/导入管线.md` §六）。
    //
    // `content_hash` 可空而不是 `NOT NULL DEFAULT ''`：这一列是**懒补**的。迁移本身不许
    // 读全库——几百本的一次性顺序读与哈希不该压在开库那一下（`plans/基础设施.md` §1.2 第 1
    // 条），所以「还没算过」必须是一个能表达的状态，而空串会和它混起来。NULL 的行由
    // [`Store::rows_missing_hash`] 数出来，逐本填上（导入命中时、`reindex` 时，以及
    // `Cleanup` 档那个补齐副本改名的后台作业）。
    //
    // `hash_algo` 记下「用哪个算的」：哈希函数被写进了身份本身，将来换算法是重算这一对列，
    // 不是把老数据作废。
    //
    // 索引只建在非空那一段（partial index）：老库此刻绝大多数行是 NULL，全表索引等于给一堆
    // 空值建一棵用不上的 B 树。
    //
    // `books.path` 的 UNIQUE 留着不动，但它的身份退休了：从现在起它是「副本在哪儿」，
    // 判重走那一列完整哈希（§六「同哈希 = 同一本书」）。
    r#"
ALTER TABLE books ADD COLUMN content_hash TEXT;
ALTER TABLE books ADD COLUMN hash_algo TEXT NOT NULL DEFAULT '';
CREATE INDEX idx_books_content_hash ON books(content_hash) WHERE content_hash IS NOT NULL;
"#,
    // v13：**摘掉 `annotations.chapter_id`** —— 它是「重扫一次书就清空批注」那条
    // 丢数据路的根，见 `plans/导入管线.md` §九 那条门（「历史批注一条都不漂」）。
    //
    // 这一列从 v1 起就带着 `REFERENCES chapters(id) ON DELETE CASCADE`，而
    // `save_flow_in` 落库的第一句是 `DELETE FROM chapters WHERE book_id = ?1` 再重插。
    // `foreign_keys` 是开着的，于是**每次 reindex（文件换过、或 `index_version` 被
    // bump 过）都把这本书的批注整批级联删掉**，而界面上那句注释还写着「批注都还在」。
    // 实测复现：两条批注 → `save_flow` 之后 0 条。
    //
    // 修法不是「重指一遍」而是**把这一列去掉**，因为它是设计里明确不要的东西：
    //
    // - 它是**纯写入列，零读取**：`annotations()` 的 SELECT 里没有它，按章分组走的是
    //   `anchor_json` 里的 `chapter`（类型文档与那两处注释都写明了「坐标里的章下标才是
    //   权威」）。没人读的列只会在读的人出现那天开始漂。
    // - 它是一份**第二真相**：章的真实身份是锚点里的 `chapter` 下标，而这一列记的是
    //   一个行号。行的 `id` 在每次重扫时都会换（删了重插），所以这一列从落库那刻起
    //   就注定了要么漂、要么把主人一起拖走 —— 两种结局都已经在代码里发生过一次。
    // - 它带来的「按章筛批注」并不需要它：批注数是每本几条到几百条，读回来按锚点分组的
    //   成本可以忽略，而正确性归一处。
    //
    // 连带删掉 `idx_annotations_book(book_id, chapter_id)`：这一列一走它只剩 `book_id`，
    // 而 `annotations()` 就是按 `book_id` 查，所以换成一条只含 `book_id` 的索引，
    // 顺带把「书→批注」这条路的索引长度收回去。
    //
    // 只删列、不改形状：不动 `books` 与 `chapters` 任何一列，所以 v13 在旧库上是
    // 「删一列 + 换一条索引」，在新建库上是空操作（v1 建表之后随即被这一条修掉）。
    r#"
DROP INDEX IF EXISTS idx_annotations_book;
ALTER TABLE annotations DROP COLUMN chapter_id;
CREATE INDEX idx_annotations_book ON annotations(book_id);
"#,
    // v14：**请回 `chapters.char_len`** —— 它在 v9 被删掉，理由是「只写不读，
    // 而出库之后连写都写不出真值」。两条现在都反过来（`plans/导入管线.md` §八 第 3 条）：
    //
    // - **有读者了**：它是「全局字符进度」的分母。`plans/Locator.md` 第二节把
    //   「前面各章字数之和 + 章内偏移」定成可以相减的全局进度，而百分比 = 全局进度 ÷
    //   全书总字数 —— 那条式子不打开书、不排版就成立，所以书架能显示「读了 37%」，
    //   而且换一台屏宽不同的设备显示的是同一个数。
    // - **写得出来了**：正文出库之后，字数是 `index()` 那一趟**真的读一遍正文**数出来的
    //   （已决第 11 条：「`index()` 允许读正文，在导入时算准」）。代价认下来：
    //   导入的 IO 从「读清单」变成「读全书」，`reindex` 必然全量重读。
    //
    // **可空**是这一版的关键，也是与 v9 删掉的那一列最要紧的差别：`NULL` = 还没算过，
    // `0` = 这一章真的是空的。折成一个数就会让进度条把没算过的章当成空章 ——
    // 而「算不出来就记 NULL，绝不用字节数估」是硬要求：字节↔字符的比例在各章之间
    // 不均匀（中英混排、标签密度），估出来的百分比会**不单调**，读者往下读而进度条后退。
    //
    // 老库的行留 NULL，由下一次 `reindex`（`stale_reason` 会把它们认成旧索引）
    // 或补扫作业填上 —— 与 v12 的哈希列同一条懒补口径，迁移本身不读全库。
    r#"
ALTER TABLE chapters ADD COLUMN char_len INTEGER;
"#,
    // v15：**封面落盘**。封面按它自己的字节哈希写一份文件到 `<书库>/covers/`，
    // 库里只留这两格描述（`plans/导入管线.md` §八 第 4 条）。
    //
    // 为什么封面要单独落文件而不是继续塞 BLOB：`books.cover` 是 MB 级的字节，
    // 而书架每次列书目都要读它（`SHELF_COLUMNS` 里就有 `cover`）。落到文件之后
    // 那一列可以只读描述，图片本身由 fp 在绘制前解码一次、按 book_id 缓存
    // （`ui/shared/images.rs` 已经是这个形状）。
    //
    // **按封面自己的哈希命名，不是书的哈希**，三个理由：两本书共用一张封面
    // （同一套系列设计）时自然去重；重新解析或换 provider 不该让封面改名；
    // 封面目录因此可以独立做垃圾回收。
    //
    // `cover_mime` 决定扩展名（`covers/<hash>.<ext>`），所以它与 `cover_hash`
    // 是一对 —— 少一个就拼不出路径，于是退回 BLOB。
    //
    // **不动 `books.cover` 与 `cover_spec`**：读的时候磁盘优先、BLOB 兜底，
    // 老库（v15 之前入库的行）与新写入失败的行都靠那条兜底。删列要等老行都被
    // reindex 过 —— 那不可能是这个迁移的事（`plans/基础设施.md` §5.2：
    // 删数据不是迁移该顺手做的）。
    r#"
ALTER TABLE books ADD COLUMN cover_hash TEXT;
ALTER TABLE books ADD COLUMN cover_mime TEXT;
"#,
    // v16：**封面字节彻底出库**。`books.cover` 这个 BLOB 连同 `cover_spec` 一起删掉，
    // 封面只活在 `<书库>/covers/<哈希>.<扩展名>` 那个文件里，缩略图只活在
    // `<书库>/.thumbs/<尺寸>/` 那个缓存里。
    //
    // 为什么现在能删（v15 时还不能）：那时读那一侧靠 BLOB 兜底，而**封面字节不可再生**
    // —— 它是从容器里抽出来的（EPUB 依次试三条规则、FB2 从 `<binary>` 解 base64），
    // 而 `covers/` 被设计成「可整目录删掉」。两件事凑在一起就矛盾：没有输入，
    // 重跑生成无从谈起。所以当时留着 BLOB 当唯一能重建的源。
    //
    // 这一版换了取舍：**文件不在就当没有封面**，界面画占位。理由是这一列是 MB 级的
    // 字节，而 `SHELF_COLUMNS` 每次列书目都要读它（`length(cover)` 那种只问长度的
    // 写法只能省一半）—— 几百本书的封面就是几百 MB 的库，而读者要的只是「架上那一格
    // 画什么」。丢掉能重建的装饰，换回一个不再随书数线性膨胀的库，是划算的。
    //
    // 代价写明白：`covers/` 被清掉之后封面**不会自己回来**，要重新导入那一本。
    // 这是有意的 —— 封面是装饰，不是内容的入口。
    //
    // 只删列，不动任何读者数据（进度、批注、集合、统计都不在 books 这两列上）。
    r#"
ALTER TABLE books DROP COLUMN cover;
ALTER TABLE books DROP COLUMN cover_spec;
"#,
    // v17：`images` 多一列 `source` —— **字节在容器里真实的那一条**。
    //
    // 为什么 `href` 一列不够用了：EPUB3 的 `<item fallback="…">` 让作者把「这一版解不动的
    // 那一份」（逐页 SVG 与 `image/svg+xml` 的封面是主流那两个）和同一张图的位图版本挂在
    // 一起。这时**键与去处是两件事**：`href` 必须仍是正文 `<img src>` 解出来的那一条，
    // 因为排版与绘制都拿它查表，而正文一个字都不许改（锚点坐标建在源文本上，
    // `plans/Locator.md`）；要读的字节却在另一条目里。以前只有后一件事，于是这类书画不出来。
    //
    // 空串读回时算「与 `href` 同一条」（`load_flow` 那一趟），所以 EPUB 以外的格式写空、
    // 这一版之前入库的老行也不必回填 —— 它们本来就没有降级路可谈。
    r#"
ALTER TABLE images ADD COLUMN source TEXT NOT NULL DEFAULT '';
"#,
    // v18：**整本的翻页方向入库**。`books.progression_rtl` 存 EPUB 的
    // `<spine page-progression-direction="rtl">` 那一格，读者要的是「点屏幕哪一侧算下一页」
    // 和「箭头按钮按下去往哪走」—— 那是**手**的事，与正文往哪边流（CSS 的 `direction`，
    // 来自元素的 `dir`）是两件事，两格各存各的，不许从一个推另一个。
    //
    // 为什么不在开书时现读 OPF：开书只读索引（`plans/text-out-of-db.md` 那条立场），
    // 而这一格是导入那一趟本来就把 OPF 读完之后顺手有的数，带过来比再开一次容器便宜。
    // SQLite 没有布尔列，0 / 1 存 INTEGER，与 `books` 其余那几格同一口径；默认 0 = 按左翻，
    // 也就是「这一版还没读过这本书的 spine」的旧行照旧能用。
    r#"
ALTER TABLE books ADD COLUMN progression_rtl INTEGER NOT NULL DEFAULT 0;
"#,
    // v19：**书目按 Dublin Core 补齐**。`DocMeta` 以前只有书名 / 作者 / 格式 / 大小四格，
    // 而 OPF、EXTH、FB2 的 `<book-info>` 里写的就是都柏林那套术语 —— 白拿的简介、出版社、
    // 出版日期、ISBN、语言、主题词、系列都没有地方存。
    //
    // 三条口径跟着这次一起定死：
    // - **多值的两格（贡献者、主题词）用换行拼接**，与 `authors` 同一套编码，
    //   读侧同一个 `split('\n')`；不再引一种 JSON 列，免得同一件事两种写法。
    // - **日期与系列序号原样存字符串**：书里给的精度不一（「1962」与
    //   「2023-05-01T00:00:00Z」都是合法值），折成日期类型就是替作者决定了他没说的位数。
    // - **空串就是「这本书没说」**，与「这一版还没读过」分开：后者由
    //   [`crate::content::format::INDEX_VERSION`] 的 bump 触发重扫来消除，
    //   所以那一版号这一轮跟着动了。
    //
    // 老库里的旧行全部取默认值（空），不回填 —— 回填要开容器，而这条库的规矩是
    // 「书目是导入那一趟顺手写的」。
    r#"
ALTER TABLE books ADD COLUMN contributors TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN subjects TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN description TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN publisher TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN published TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN language TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN isbn TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN dctype TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN rights TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN series TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN series_position TEXT NOT NULL DEFAULT '';
"#,
    // v20：**每章的图片引用清单入库**（`chapters.images_json`）。图片查看器要摆的是
    // 「整本书一共有几张图、现在这一张排第几」，还要能一句话回到它出现的那个字上去 ——
    // 那两样排版都给不出来：排版只认识**已经排好的那一章**。
    //
    // 存的是扫描那一趟的产物（作者写下的原样 `src` + 它前面的可见字符数，
    // 见 `content::scan::ImageUse`），**不是**解析后的容器路径：解析要按本章目录做、
    // 还要对得上那本书的图片表，那是界面侧 `ChapterImages::resolve` 一条路的事。
    // 存原样才不会出现「列里说有三张、正文里只画得出两张」那种两本账。
    //
    // 默认 `'[]'` = 「这一章没有图」。老行不回填（回填要开容器，而这条库的规矩是
    // 「清单是导入那一趟顺手写的」），所以跟着这一版 bump `INDEX_VERSION`，
    // 开书时按本重扫（`stale_reason` 认 `OldIndex` 那一条路）。
    r#"
ALTER TABLE chapters ADD COLUMN images_json TEXT NOT NULL DEFAULT '[]';
"#,
    // v21：**划线怎么画入库**（`annotations.style`）。此前这一件事没有地方存 ——
    // 表里只有 `color`，而界面那一层把「划线的样子」写死成一条 2 像素实线
    // （`folio-app` 的 `annotation_underline_rects`），于是读者只能选颜色、不能选画法。
    //
    // 存的是**名字**（`highlight` / `line` / `wavy`）而不是一个小整数：与同一张表里
    // `color` 那一列同一条口径（枚举的顺序是界面的摆法，会动；名字是身份，不该动）。
    // 把它折成序号等于把「菜单里第几格」写进历史数据，将来往中间插一档就把老行读错。
    //
    // **默认 `'line'` 而偏好那一带的默认是 `highlight`，两个默认值不是一件事**，
    // 各自答的是各自的问题：
    //
    // - 这一列的默认答的是「v21 之前落的那些条长什么样」。答案确定是实线 —— 那时界面
    //   只会画实线。老行因此一行都不改观感，也不需要回填（回填要猜，而这里本不用猜）。
    // - 配置里那一格答的是「下一条款该长什么样」，那是读者的选择，与历史无关。
    //
    // 新建一条永远显式写这一列（`Store::add_annotation` 多带一格），所以 `'line'` 这个
    // 默认只对**迁移之前就在的那些行**生效，不会变成「新划线悄悄实线」。
    r#"
ALTER TABLE annotations ADD COLUMN style TEXT NOT NULL DEFAULT 'line';
"#,
    // v22：**这份索引算自哪一份分章规则**（`books.rules_hash`）。
    //
    // 为什么要有这一格：库里存的不是字节区间而是**章的序号**
    // （`ChapterLocator::Section(n)`），而那个序号只在「按今天这套规则切」的前提下指到
    // 第三章。`.txt` 的分章规则是一份可换的数据（`content::format::novel::rules`），
    // 换一套 = 每一本书的序号重新排一遍 —— 不记账的话，进度与批注会**安静地**指到隔壁
    // 那一章，而这是这一层最坏的一种错：屏上看着是对的。
    //
    // 有了这一格，调一个阈值不再需要 bump `INDEX_VERSION` 再发一版：数据自己会说话
    // （判据见 [`crate::content::format::rules_hash`]，`stale_reason` 认成
    // `RulesChanged` 那一条路）。
    //
    // **可空而不是 `NOT NULL DEFAULT ''`**：空串会变成「有一本书的哈希恰好是空串」，
    // 而它答不出任何问题；NULL 才敢说「这一行没记」。三种行都读回 NULL ——
    // 除 `.txt` 以外的格式（它们的索引不碰那张表，所以判据根本不看这一格）、
    // 定版式那一路（`save_fixed` 不写它），以及 v22 之前入库的老行。
    // 老行不回填：回填要把整本重读一遍，而那正是重扫本身，交给开书时那一趟顺手做完。
    r#"
ALTER TABLE books ADD COLUMN rules_hash TEXT;
"#,
];
