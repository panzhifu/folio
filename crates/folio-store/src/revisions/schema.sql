ALTER TABLE books ADD COLUMN book_uuid TEXT NOT NULL DEFAULT '';
ALTER TABLE books ADD COLUMN active_rev_no INTEGER NOT NULL DEFAULT 0;
ALTER TABLE books ADD COLUMN active_index_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE books ADD COLUMN managed_root TEXT NOT NULL DEFAULT '';
UPDATE books SET book_uuid=lower(hex(randomblob(16))) WHERE book_uuid='';
CREATE UNIQUE INDEX idx_books_uuid ON books(book_uuid) WHERE book_uuid<>'';
CREATE TABLE book_id_clock (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 high_water INTEGER NOT NULL CHECK(high_water>=0)
);
INSERT INTO book_id_clock SELECT 1,max(0,coalesce(max(id),0)) FROM books;
CREATE TRIGGER book_id_clock_insert AFTER INSERT ON books BEGIN
 UPDATE book_id_clock SET high_water=max(high_water,NEW.id) WHERE singleton=1;
END;
CREATE TABLE record_id_clocks (
 kind TEXT PRIMARY KEY,
 high_water INTEGER NOT NULL CHECK(high_water>=0)
);
INSERT INTO record_id_clocks SELECT 'annotation',max(0,coalesce(max(id),0)) FROM annotations;
INSERT INTO record_id_clocks SELECT 'bookmark',max(0,coalesce(max(id),0)) FROM bookmarks;
CREATE TRIGGER annotation_id_clock_insert AFTER INSERT ON annotations BEGIN
 UPDATE record_id_clocks SET high_water=max(high_water,NEW.id) WHERE kind='annotation';
END;
CREATE TRIGGER bookmark_id_clock_insert AFTER INSERT ON bookmarks BEGIN
 UPDATE record_id_clocks SET high_water=max(high_water,NEW.id) WHERE kind='bookmark';
END;

CREATE TABLE book_sources (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 locator TEXT NOT NULL UNIQUE,
 kind TEXT NOT NULL,
 is_primary INTEGER NOT NULL DEFAULT 0,
 generation INTEGER NOT NULL DEFAULT 0,
 fingerprint TEXT NOT NULL DEFAULT '',
 state TEXT NOT NULL DEFAULT 'available'
);
CREATE UNIQUE INDEX idx_primary_source ON book_sources(book_id) WHERE is_primary=1;
CREATE TABLE suppressed_sources(locator TEXT PRIMARY KEY, suppressed_at INTEGER NOT NULL);

CREATE TABLE book_revisions (
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 rev_no INTEGER NOT NULL,
 index_generation INTEGER NOT NULL,
 content_hash TEXT NOT NULL,
 resource_hash TEXT NOT NULL DEFAULT '',
 managed_path TEXT NOT NULL,
 library_root TEXT NOT NULL DEFAULT '',
 file_size INTEGER NOT NULL DEFAULT 0,
 file_mtime INTEGER NOT NULL DEFAULT 0,
 resource_manifest TEXT NOT NULL DEFAULT '[]',
 metadata_json TEXT NOT NULL DEFAULT '{}',
 index_json TEXT NOT NULL DEFAULT '',
 state TEXT NOT NULL,
 source_id INTEGER REFERENCES book_sources(id) ON DELETE SET NULL,
 source_generation INTEGER NOT NULL DEFAULT 0,
 operation_id TEXT NOT NULL DEFAULT '',
 created_at INTEGER NOT NULL,
 PRIMARY KEY(book_id,rev_no,index_generation)
);
INSERT INTO book_revisions(book_id,rev_no,index_generation,content_hash,managed_path,file_size,file_mtime,state,created_at)
 SELECT id,0,0,coalesce(content_hash,''),path,size,mtime,'legacy-unverified',added_at FROM books;

CREATE TABLE revision_operations (
 id TEXT PRIMARY KEY,
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 rev_no INTEGER NOT NULL,
 index_generation INTEGER NOT NULL,
 stage TEXT NOT NULL,
 owned_path TEXT NOT NULL DEFAULT '',
 error TEXT NOT NULL DEFAULT '',
 updated_at INTEGER NOT NULL
);
CREATE TABLE record_origins (
 kind TEXT NOT NULL,
 uid TEXT NOT NULL,
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 rev_no INTEGER NOT NULL,
 index_generation INTEGER NOT NULL,
 content_hash TEXT NOT NULL,
 PRIMARY KEY(kind,uid)
);
INSERT INTO record_origins SELECT 'annotation',a.uid,a.book_id,0,0,coalesce(b.content_hash,'')
 FROM annotations a JOIN books b ON b.id=a.book_id WHERE a.uid IS NOT NULL AND a.uid<>'';
INSERT INTO record_origins SELECT 'bookmark',a.uid,a.book_id,0,0,coalesce(b.content_hash,'')
 FROM bookmarks a JOIN books b ON b.id=a.book_id WHERE a.uid IS NOT NULL AND a.uid<>'';
CREATE TABLE revision_positions (
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 rev_no INTEGER NOT NULL,
 index_generation INTEGER NOT NULL,
 mode TEXT NOT NULL,
 locator_json TEXT NOT NULL,
 updated_at INTEGER NOT NULL,
 PRIMARY KEY(book_id,rev_no,index_generation)
);
INSERT INTO revision_positions SELECT book_id,0,0,mode,locator_json,updated_at FROM positions;
CREATE TABLE revision_chapter_read (
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 rev_no INTEGER NOT NULL,
 index_generation INTEGER NOT NULL,
 idx INTEGER NOT NULL,
 read_at INTEGER NOT NULL,
 PRIMARY KEY(book_id,rev_no,index_generation,idx)
);
INSERT INTO revision_chapter_read SELECT book_id,0,0,idx,read_at FROM chapter_read;
CREATE TABLE anchor_projections (
 kind TEXT NOT NULL,
 uid TEXT NOT NULL,
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 rev_no INTEGER NOT NULL,
 index_generation INTEGER NOT NULL,
 anchor_json TEXT,
 status TEXT NOT NULL,
 PRIMARY KEY(kind,uid,rev_no,index_generation)
);
