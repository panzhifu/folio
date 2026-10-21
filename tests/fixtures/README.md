# 测试样本约定

## 两个目录，规则完全不同

| 目录 | 入库 | 内容 |
|---|---|---|
| `oss/` | ✅ 提交 | 可自由再分发的样本：由测试代码**自己合成**的 EPUB/TXT/MD，或公版且来源明确的书 |
| `local/` | ❌ 已在 `.gitignore` 里 | 你自己拥有的受版权保护的书籍，只用于本机验证真实排版效果 |

`local/` 不入库不是"防止意外"，而是**性质问题**：把一本仍在版权期内的书提交进仓库，就是公开再分发。仓库设成 private 不改变这一点；而且 git 历史里的文件在推送后会被 fork、缓存和 reflog 留住，删不干净。

## 怎么用 local/ 里的书

排版观感靠人眼，不靠断言，所以这类样本走**手动工具**，不进测试套件：

```bash
cargo run -p folio-core --example css-gap
```

需要「整本书规模」那道闸门时，**先把那个形状合成进测试**（`content/format/epub.rs` 的
`published_shape_epub`：28 章 / 25 条目录 / `@charset` 打头的 CSS），而不是让测试去读一个
只有某台机器上才存在的文件。**默认套件里一条 `#[ignore]` 都没有** —— 这是刻意的，
合成样本才是自动化测试的主力（见下面那一节）。

**默认套件（不带 `--ignored`）里一条 `#[ignore]` 都不跑** —— 这是刻意的，
合成样本才是自动化测试的主力（见下面那一节）。标了 `#[ignore]` 的只有两种，各自都在
那句里写清跑法：**真样本门**（输入是「别人生产的文件」，这种形状合成不出来）与
**墙钟 / 基准门**（量的是时间，同一台机器两次就不一样，不适合当门）。
新增真样本门时的三条规矩：① 样本目录不存在或为空要**当场红**，不许「不在就算通过」
（那是永远绿的假灯）；② 先把量出来的东西记在下面那一节，再决定钉哪几条 ——
门是量完之后写的，不是先写门再去找样本；③ 合成夹具能照出来的形状一律合成，
真样本门只补它照不出来的那部分。

```bash
cargo test -p folio-core --lib -- --ignored          # 全跑（要下面那些本机样本）
cargo test -p folio-core --lib -- --ignored the_two_real_palmdoc
cargo test -p folio-core --lib -- --ignored the_real_word_documents
```

## 现有样本

- `oss/minimal-zh.epub` —— 合成中文书，4.7 KB。故意布成会出错的形状：
  manifest 顺序与 spine 顺序不同、文件名里有空格（href 写成 `%20`）、
  nav 条目带 `#fragment`、两份 CSS 需要按声明顺序拼接、第三章没有 `<h1>`（测标题回退）。
- `local/shejie-mauriac.epub` —— calibre 导出的 EPUB 2.0（无 nav，只有 `toc.ncx`），
  28 章 / 25 条目录 / 一份 2.4 KB 的 CSS。它抓到了合成样本抓不到的两个真 bug：
  **NCX 的元素名是大小写敏感的 `navMap` / `navPoint`**（目标写在 `<content src="…"/>` 里，
  不是 `<src>`），以及出版书 CSS 里开头的 `@charset` 语句会把后一条规则吃掉。
  它这些形状现在由 `published_shape_epub` 合成进默认套件，本机这一份只给 `css-gap` 看观感。

## 真样本量出来的东西

这一节记的是「合成夹具照不出来的形状」，以及它们各自逼出来的一条实现。样本本身不入库，
取法写在每一节里。

- **DOCX —— 四份真 Word 文件**（python-docx 的 `tests/test_files`，MIT；放
  `tests/fixtures/local/docx/`）：
  ```bash
  mkdir -p tests/fixtures/local/docx && cd tests/fixtures/local/docx
  for n in test.docx having-images.docx blk-inner-content.docx sct-inner-content.docx; do
    curl -sLO "https://raw.githubusercontent.com/python-openxml/python-docx/master/tests/test_files/$n"
  done
  ```
  量出来一条：**Word 的表格里每个格子是一整段**（`<w:tc><w:p>…`）。我们那一行拼成一个
  `<p>`，格子里那个 `<p>` 没拆，交出去就是 `<p><p>…</p></p>`；html5ever 见到 `<p>` 里开
  `<p>` 会当场关掉外层，屏上那一行被劈成两段。门叫
  `docx::tests::table_cells_do_not_nest_paragraphs_or_start_chapters`（合成那一份也摆了这个形状，
  所以这条在默认套件里就跑），本机这四份由
  `docx::tests::the_real_word_documents_keep_their_shapes` 再过一遍普遍不变量。
  顺带量到：`docProps/core.xml` 的 `dc:creator` / `dc:description` 真能落到书目的那两格。
- **DOCX 的列表与脚注 —— 再加五份**（docx-rs 的 `fixtures/`，MIT，作者 bokuweb；那份仓库
  是「真 Word 写出来的文件」最集中的合法来源之一）：
  ```bash
  cd tests/fixtures/local/docx
  for n in footnotes/footnotes numbering/numbering without_numid/without_numid \
           lvl_override/override hello_libre_office/hello; do
    f=$(basename "$n")
    gh api -H "Accept: application/vnd.github.raw" "repos/bokuweb/docx-rs/contents/fixtures/$n.docx" > "$f.docx"
  done
  ```
  量出来四条：① 编号的形状是 `w:pPr/w:numPr` 里两个空元素（`w:ilvl` + `w:numId`），**怎么记数
  在 `word/numbering.xml`**（`w:num` → `w:abstractNumId` → 那一层的 `w:numFmt`）；② `numId` 1
  与 2 都是 `decimal` 却是两份列表，纸面上是「1 / 1、2」，合成一条就成了「1 / 2、3」；
  ③ `override.docx` 证明 `w:lvlOverride` 真的在用（同一个抽象定义被第三个实例改写第一层）；
  ④ `footnotes.docx` 有一条真注文（`w:id="1"`，注体是 `<w:p>` 里一个 run），而
  `without_numid.docx` 的 `footnotes.xml` 里**只有** separator / continuationSeparator /
  continuationNotice 三种格子、第三条占的 id 正是 `1` —— 所以「按 `w:type` 判、不按 id 判」
  不是讲究。门：`docx::tests::numbered_lists_come_out_nested_and_renumbered`、
  `…::an_unresolvable_numbering_is_an_indent_not_an_invented_number`、
  `…::footnotes_are_anchored_and_their_bodies_land_in_the_referencing_chapter`、
  `…::the_three_typed_entries_never_become_notes`、`…::a_repeated_reference_keeps_one_number_and_one_body`，
  真样本那一条叫 `…::the_real_word_documents_keep_their_lists_and_notes`。
  `hello.docx` 是 LibreOffice 写的 docx（第二个生产者），它证明 `numPr` 同样写在段落上。
- **ODT —— 五份真 OpenDocument 文件**（odfjs 的 `tests/fixtures`，那份仓库是 ODT 模板引擎，
  样本都是 LibreOffice 存的；加 webodf 的 `programs/editor/welcome.odt`）：
  ```bash
  mkdir -p tests/fixtures/local/odt && cd tests/fixtures/local/odt
  for n in liste-courses liste-nombres enum-courses tableau-simple \
           formatting-liste-nombres-plusieurs-couches; do
    gh api -H "Accept: application/vnd.github.raw" "repos/Odfjs/odfjs/contents/tests/fixtures/$n.odt" > "$n.odt"
  done
  gh api -H "Accept: application/vnd.github.raw" "repos/webodf/webodf/contents/programs/editor/welcome.odt" > welcome.odt
  ```
  量出来三条：① 列表的形状在 markup（嵌套的 `text:list` / `text:list-item`）、记数在样式
  （`text:list-style` 的 `text:list-level-style-bullet/@text:bullet-char` 与
  `…-number/@style:num-format`），而且**样式定义两处都可能出现**（`styles.xml` 的命名样式与
  `content.xml` 的 automatic-styles）；② `•` `◦` `▪` 三层符号真的存在（`liste-courses.odt`），
  对上引擎的 disc / circle / square 三档；`welcome.odt` 另有 `➢`（表外字符 → 圆点）与
  `num-format="A"` / `"1"`；③ `text:style-name=""` 是 LibreOffice **真写出来的**（welcome 里
  那两个注解列表），所以「认不出就圆点」不是想象。门：
  `odt::tests::list_levels_come_from_the_style_and_the_nesting`、
  `…::an_unstyled_list_is_a_bullet_not_an_invented_number`，真样本那一条叫
  `…::the_real_odf_documents_keep_their_lists`。
  **这批样本里没有任何一份带 `text:note`**：注那一路由合成夹具钉（门
  `…::notes_are_anchored_in_place_and_their_bodies_land_at_the_chapter_tail`），
  「真生产者怎么摆」在这一条上没被量过。
- **ComicInfo.xml —— 只有合成**：字段口径抄 Komga 的那份生产级 DTO
  （`komga/…/infrastructure/metadata/comicrack/dto/ComicInfo.kt`，MPL-2.0），门在
  `cbz::tests::the_comicinfo_sidecar_fills_the_shelf_fields` 与
  `cb7::tests::the_sidecar_survives_the_disk_cache`。真包不好合法取得（漫画基本都在版权期内），
  所以 `<Chapter Page="…">` 那一族的基数**没有权威**、代码里故意不读 —— 猜 0 起还是 1 起的
  后果是整条目录跳到隔壁那一页，比目录少一条糟得多（理由写在该模块的头）。
  同一条理由也适用于那张 `folder.jpg` / `cover.jpg`：约定是公开的，但带它的真包没合法取得到，
  所以封面的门（`cbz::tests::the_cover_sidecar_is_the_cover_not_an_extra_page` 那四条 +
  `cb7::tests::the_cover_sidecar_is_picked_out_of_the_cache`）目前是合成的。
- **还只有合成夹具的几族**：HTMLZ、AZW4、CB7 / CBT、ComicInfo.xml、漫画的封面缓存、
  ODT 的 `text:note`。它们的门目前是「自己写的夹具刚好符合自己的解析」—— 撞到真文件形状之前
  别当成已验过。（公网上找一个**可自由再分发**的 .odt / .htmlz / .azw4 比想象困难：HTMLZ 是
  Calibre 私有产物，AZW4 是 Kindle 印刷版，天然带 DRM。）
- **PalmDOC —— 两本 `TEXt`/`REAd`**（libmobi 的 `tests/samples`，随 `reference/` 那一份克隆在
  本机，不入库）：文件名后缀是 `.mobi`，但**容器类型字段说的才是它是什么**，判定层按那一格
  走，所以 `sample-textread.mobi` 判成 PDB。量出来三条：① 书名在 PalmDB 头偏移 0..32
  那一格（mobitool 报的 Title 就是它，而我们从前只用文件名主名）；② 它的正文其实是
  `<HTML>…` 标记，还带一块 `<dc:*>` 的早期 Mobipocket 都柏林核心（Title / Creator /
  Publisher / Language 四条）；③ `sample-drm-v1` 记录 0 头偏移 12 那个数是 1 = 加过密，
  从前没人读它，于是被「解压」成一屏乱码交出去。三条的门分别叫
  `pdb::tests::the_title_is_the_palmdb_name_not_the_file_name`、
  `…::a_markup_payload_is_handed_to_the_engine_unescaped` +
  `…::the_oeb1_dublin_core_block_fills_the_shelf_fields`、
  `…::an_encrypted_palmdoc_is_refused_as_encrypted`，真样本那一条是
  `pdb::tests::the_two_real_palmdoc_books_keep_their_three_facts`。
- **DjVu —— 两份**（`oss/djvu/`，来自 djvu-rs 的测试集，取法与许可见那里那份
  `ATTRIBUTION.md`）：量出来三条 —— ① `Document::open` 只吃 bundled，所以入口走
  `open_dir`（实测它对 bundled 给得出同样的页数；真 indirect 样本本机没有，那一支还没撞到过）；
  ② 那份 71 页的 spec 真样本带 100 条 NAVM 书签、三种 url 形态（`#64` 是 1 起的页号、
  `#p0064.djvu` 要按 DIRM 的 `P` 顺序换、根那一条是空串），所以「书签 → 目录」这一格
  是**拿真文件钉的**（门：`a_real_navm_outline_lands_on_pages`）；③ 手边四份真 DjVu
  （含两本 Internet Archive 公版扫描）的 `(metadata …)` 全为空 —— IA 把元数据写在旁边的
  `.meta` 文件里，所以书目那一格的门改用**上游编码器**（`djvu_mut::set_metadata`）往真样本上
  加一块再读回来（`the_shared_metadata_block_lands_on_the_shelf_fields`）：钉的是映射，
  不是那一块的字节形状。

## 合成样本才是自动化测试的主力

EPUB 解析的正确性其实**不需要**真书。要验的是具体结构，而这些都能程序化生成：

- **容器与导航**：`mimetype` 位置不合规、`META-INF/container.xml` 指向非常规 OPF 路径、NCX 与 nav.xhtml 并存、spine 的 linear/non-linear、加密资源（EPUB 4 DRM 声明）
- **中文排版**：`<ruby>`/`<rt>` 注音、`writing-mode: vertical-rl`、`text-indent: 2em`、`text-align: justify`、`line-height` 无单位与带单位
- **结构边界**：章内嵌图（含 SVG）、`epub:type="footnote"` 脚注、`columns` 多栏、`float: left` 绕图、超长无空格英文词、表格 `colspan`/`rowspan`
- **畸形与恶意**：未闭合标签、BOM、CRLF、UTF-16 XHTML、zip 路径穿越（`../../etc/passwd`）、高压缩比 zip 炸弹、非 UTF-8 字节序列
- **锚点稳定性**：同一本书改字号 / 改窗口宽度后重定位；改章节标题后 `NodeIdentity` 是否仍可解析

这套覆盖度远高于"一本真书跑通了"，并且可提交、可在 CI 重复。
