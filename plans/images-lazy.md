# 图片不落字节：学 KOReader 的容器按需读

状态：**已实现**。`cargo test --workspace --lib` 全绿。

## 问题

`images` 表把整本书的图片以 BLOB 存下来（`store.rs` v4 迁移），封面还在
`books.cover` 与 `images` 各存一份。实测本机 `folio.db`：19 张图 492 KB、
封面 404 KB，而其一本书的 EPUB 原件才 672 KB —— 图就是书的全部重量，
数据库等于把书抄了一遍。漫画/画册一本 100–500 MB，库与书同量级。

## KOReader 怎么做的（`reference/koreader`）

正文插图**不入库、不解码常驻**：

- 要画时现从容器开流：`ldomDocument::getObjectImageStream()` → `container->OpenStream()`
  （`crengine/src/lvtinydom.cpp:22503`、`:22537`），包成 `LVCreateStreamImageSource`；
- 缓存的只是 `NodeImageProxy`（`lvtinydom.cpp:22178`），带 `refName` + 宽高，
  `Decode()` 时**重新开一次流**（`:22215`）；
- 落笔 `LVColorDrawBuf::Draw()` 逐行解码进目标帧缓冲（`lvdrawbuf.cpp:1637`），
  内存里不留解码后的位图；
- `cr3cache`（`cre.cpp:436` `initCache`）存 DOM/style/rect/text/字体，**不含图**。

只有一个例外：封面。`coverbrowser` 插件把它放进 `bookinfo_cache.sqlite3`，
但存的是**按显示尺寸缩过的 blitbuffer**，再 zstd 压缩
（`plugins/coverbrowser.koplugin/bookinfomanager.lua:535-548`）。

判据是：**库里只放「算出来贵、且原文件给不了」的东西**。原图随时能从容器再拿，
就不复制；封面要显示尺寸的缩略图，就存缩略图而不是原图。

## Folio 的改法

1. **`ImageRes` 去掉 `bytes`**：只留 `href / media_type / width / height`。
   尺寸在导入时解码一次 header 算好，之后布局只查表 —— 与 KOReader 的
   `NodeImageProxy` 同形。
2. **`images` 表 v5 迁移 `DROP COLUMN bytes`**。旧库的字节随迁移清掉
   （不 `VACUUM`，空间由后续写入复用）。
3. **字节按需从容器读**：`format::image_bytes(path, format, href)`，对应
   KOReader 的 `getObjectImageStream`。每次调用重开容器 —— KOReader 的
   `Decode` 也是重开；调用方用 `OnceLock` 保证一张图只读一次。
4. **`BookImages` 拿一个 `ByteSource`**（`Arc<dyn Fn(&str) -> Option<Vec<u8>>>`）：
   生产环境指向 `books.path` + 格式，测试用内存表。`ImageEntry::rendered`
   改成返回 `Option<Arc<Image>>` —— 文件不在时**静默不画**，正文照排，
   与 KOReader 容器丢失时的降级一致。
5. **封面存缩略图**：导入时 `shrink_image(cover, 1024)`，只留一份在
   `books.cover`（书架用），不再进 `images`。对应 KOReader 的 `bookinfo` 表。

## 影响面

| 文件 | 改动 |
|---|---|
| `folio-core/src/format/mod.rs` | `ImageRes` 去 `bytes`；`image_bytes()` + `FormatAdapter::image_bytes` |
| `folio-core/src/format/epub.rs` | 读 manifest 图不再留字节；封面缩到 1024；实现 `image_bytes` |
| `folio-core/src/store.rs` | v5 迁移；`save_flow` / `load_flow` 去 bytes 列 |
| `folio-app/src/ui/shared/images.rs` | `BookImages` 持 `ByteSource`；`rendered` 回 `Option` |
| `folio-app/src/ui/features/reader/book.rs` | 建 `BookImages` 时传容器来源 |
| `folio-app/src/ui/features/reader/session.rs` | `with_book` / `open_book` 把 `path` 接到图片上 |

## 不做的事

- 不把图片读取挪到后台线程：现在也是首帧同步解码，行为不回归；要挪就整体挪
  （连同 `shrink_image`），单为图片开一条异步路得不偿失。
- 不做图片的 LRU 淘汰：`Arc<Image>` 仍按 `OnceLock` 常驻到关书，与改动前一致。
  真要做，入口在 `BookImages`，与本次解耦。
