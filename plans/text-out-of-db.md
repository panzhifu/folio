# 正文出库：库里只存「怎么加载」

状态：**已实现**。`cargo test --workspace` 全绿；本机那份 5.47 MB 的 `folio.db`
升级后 491 KB（**-91%**）。

## 问题

库把**正文**当权威数据存了。实测一台机器上 3 本书 5.47 MB：

| 项 | 字节 | 占比 |
|---|---|---|
| `chapter_fts`（trigram 全文索引） | 2,510,848 | 46% |
| freelist（已释放、没归还的空页） | 1,957,888 | 36% |
| `chapter_text`（正文，未压缩） | 471,040 | 9% |
| `books.cover`（封面缩略图） | 404,858 | 7% |

三本书的正文只有 21.7 万个字符。合成基准（真实中文书 10.8 万字）：原样 XHTML
282 KB、同内容 EPUB 140 KB、入库后 DB **1.31 MB = 9.3× EPUB**。

而 `Store::search` 当时**只有测试在用** —— 搜索推后（见 `plans/README.md` 的 M10），
索引那份钱白付。

## 判据

与 `plans/images-lazy.md` 同一条：

> **库里只放「算出来贵、且原文件给不了」的东西。**

正文原文件给得了 —— 重开一次容器就回来了。所以：

- **库里存位置**（`chapters.locator`），不存正文；
- 打开时按位置回容器取，见 `Document::chapter_source`；
- 索引是**可重建的派生数据**，搜索真要做时按需建，不跟正文一起当权威数据。

## 位置是什么

`ChapterLocator` 三种，对应两类容器：

| 变体 | 谁用 | 取一章的成本 |
|---|---|---|
| `Entry(href)` | EPUB、CHM（那份 hhc 的 topic 条目）、HTMLZ（包里每份 HTML 一章） | 解一个条目 |
| `Section(n)` | TXT / MD / HTML / FB2 / RTF / MOBI / DOCX / ODT / PDB（单文件） | 重读整本 + 重分一次章 |
| `Inline(text)` | 测试、`from_content`、旧库兜底 | 0（正文就在手上） |

**为什么不给单文件格式存字节区间**：分章规则（Markdown 的 `#`、FB2 的
`<section>`）一旦调整，字节区间就悄悄指到别处；序号只会让「第几章」这个说法本身
失效 —— 两者都要重扫，但后者坏得看得见。

`Section` 的重扫代价由阅读会话兜住：`reader::Book` 按章缓存正文，来回翻同一章只付
一次；缓存的上界就是这一本书（关书即走），与 KOReader 把整本留在 crengine 内存里
同一个量级。

## 落点

| 位置 | 改动 |
|---|---|
| `format/mod.rs` | `ChapterLocator` + `locator_for(format, chapter, index)` |
| `format/content.rs` | `FlowDoc::chapter_locators`（与 `chapters` 一一对应）+ `locator()` / `resolve_locators()` |
| `format/provider.rs` | `entry_text(path, entry)`（默认 `None`：单文件格式走重读那条降级路） |
| `format/epub.rs` | 实现 `entry_text`（与 `image_bytes` 同一条 zip 路） |
| `document/mod.rs` | `Document::chapter_source(index)`：`Inline` 直取 / `Entry` 解条目 / `Section` 重读 |
| `document/book.rs` | `BookDocument` 实现它；取不到时是 `Error::ChapterUnavailable`，不是空串 |
| `store/schema.rs` | v8：摘掉 FTS 与三个触发器，`chapters` 加 `locator` |
| `store/mod.rs` | `finish_v8`：老库按 `books.format` + `spine_href` + `idx` 回填位置，然后 `DROP TABLE chapter_text` |
| `store/content.rs` | 存位置不存正文；`save_document` 带上格式（只有它知道格式，才算得出位置） |
| `reader/book.rs` | 持有 `Arc<dyn Document>`，按章缓存正文 |

## 连带的两件事

1. **`auto_vacuum=INCREMENTAL` + `incremental_vacuum`**。原来那 1.96 MB 的空页是
   「删了但没还」 —— 重导入与删书都是整本一起删，不主动收一次，文件就停在历史
   最高水位。开库时一次性 `VACUUM` 把老库转过来（就是上面那 -36%），之后
   `Store::reclaim` 在删书/重导入后收尾。
2. **搜推后**。`Store::search` 与 `SearchHit` 一起删掉了。真要接的时候按「可重建的
   索引」做：`books.fts_version` 标记 + 后台按需构建，而不是跟着入库一起写。

## 代价与已知取舍

- **开书多一次容器解析**（读 OPF/nav + 第一章正文）。换来的是一条索引级的开书路径；
  真嫌慢时把 `provider.read` 拆成 `index()` / `chapter_source()` 两相（索引只读
  OPF，不碰正文），接口已经在那儿了。
- **源文件必须还在**。不在时的表现是「这一章取不到」（`ChapterUnavailable`），
  不是「书没了」——条目、进度、作者、封面都还在。界面该给重新导入/重新定位的入口，
  这一条**还没接**。
- **`chapters.char_len` 从此多半是 0**：位置算得出来，字数要读正文才算得出。
  这一列目前只写不读，真要用时再在索引那一相里数。
- **封面现在是库里最大的一项**（491 KB 里占 405 KB）。下一步若要继续压，动的是
  它（缩略图再小一档 / 换编码），不是正文。
