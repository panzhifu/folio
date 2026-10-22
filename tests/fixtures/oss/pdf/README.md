# PDF 样本

`three-pages.pdf` —— 1857 字节、三页的合成书，形状是**故意摆成会出错的那种**：

| 页 | 框 | `/Rotate` | 这一页在测什么 |
| --- | --- | --- | --- |
| 1 | `MediaBox [0 0 595 842]`（A4） | 0 | 最常见的一页：竖、无裁切 |
| 2 | `MediaBox 400×600` 套 `CropBox [50 50 350 550]` | 0 | 被裁过的页。`render_page_fit` 按 MediaBox 定比例、按可见框出图，不反推目标框就会短一截（实测要 1024 只出 853） |
| 3 | 不写自己的 `/MediaBox`，从 `/Pages` 继承 `612×792` | 90 | 继承（规范 §7.7.3.3 的 inheritable 属性）与旋转叠加：转完之后**宽比高长**，页表里存反了屏幕上就是字压字 |

书目那一格也摆了三个不好办的形状：`/Title` 是 `<FEFF…>` 的 UTF-16BE 中文（不是
`String::from_utf8` 那条路能解的），`/Author` 写成 `Chen, Xiaoming`（按逗号切就变成两个
作者），`/Keywords` 混用逗号与分号还带一个括号包着的词组。`/CreationDate` 是规范的
`D:YYYYMMDDHHmmSS+08'00'`。书签三条，第三条**只有标题没有 `/Dest`**（规范允许，界面上
点不动，所以 provider 该把它丢掉而不是摆一条空落点的目录）。

内容流只画一个灰矩形，**不吃任何字体**：标准 14 号字体（Helvetica）在 PDF 里是「名字，
不带字库」，能不能渲出来归系统的字体解析，不该让这一格门去赌本机装了什么字体。

## 怎么重做

`crates/folio-core/src/content/format/pdf.rs` 的测试模块里有一份同构的合成器（`Fixture`
与 `write_fixture`），它生成的是**同一个形状**；两边各有一条门互相盯（
`the_shipped_fixture_matches_the_synthetic_one`）。要单独重做也可以，用任何能写对象表与
xref 的工具，只要保持上面那张表的形状。

**产物必须由独立实现验过再入库** —— 这一步不能省，否则就成了「只有我们自己读得懂的 PDF」，
它证明不了我们读得懂 PDF。这里用的裁判是 poppler：

```sh
pdfinfo three-pages.pdf              # Pages: 3 / Title: 测试合成本 / CreationDate: 2009-11-10
pdfinfo -f 1 -l 3 -box three-pages.pdf   # 三页的 MediaBox 与 CropBox，逐页对照上面那张表
pdftoppm -r 72 -png three-pages.pdf /tmp/out  # 三页都画得出来（第 2 页是裁过的那一页）
```

写这份夹具的脚本（Python，与 Rust 合成器同构）留在版本历史里；它只用来生成字节，
**不进测试路径** —— 测试读的是这个文件本身。

## `wps-paper.pdf` —— 带文字层的**别人生产的**那一份

前一份的形状是故意摆的，但它**一个字符都没有**：内容流只画灰矩形。于是「文字层」那一整条
路（逐字盒、翻 y、词与词之间补缝）此前只被我们自己的合成器量过 —— 而合成器与解析器同时
写错时，两边是互相圆场的。这一份补的就是那一格。

| 格 | 值 |
| --- | --- |
| 生产者 | Kingsoft Office Professional（WPS）—— 与 Word、LaTeX、预览器各不相同的一条写盘路 |
| 页 | 2 页，`612 × 792`（Letter），无 `/Rotate`、无 `CropBox` |
| 字体 | `ABCDEF+TeXGyreTermes-{Regular,Bold,Italic,BoldItalic}`，`/Subtype /Type0` + `/Encoding /Identity-H` + 各带一份 `ToUnicode` CMap |
| 大小 | 90 191 字节 |
| 来源 | `reference/readest/apps/readest-app/src/__tests__/fixtures/data/sample-paper.pdf`（别家阅读器的测试夹具，正文是一份空白论文模板：`TITLE OF THE PAPER` / `Author(s) Name(s)` / `ABSTRACT`，没有创作内容） |

字体那一格才是这份样本的价值：内容流里摆的不是 ASCII 而是**两位数码**，靠 `ToUnicode`
折回 Unicode。一份书的字能不能选出来，全看这一折对不对，而这与我们自己写的合成器
（`/Helvetica` 标准十四、不带字库、直接写 ASCII）是两条完全不同的路。

裁判仍是 poppler：

```sh
pdfinfo wps-paper.pdf                       # Pages: 2 / 612x792 pts (letter) / Producer: Kingsoft Office Professional
pdftotext -f 1 -l 1 wps-paper.pdf -         # TITLE OF THE PAPER / Author(s) Name(s) / ABSTRACT …
pdftotext -f 1 -l 1 -bbox wps-paper.pdf -   # 逐词框：TITLE = xMin 249.000 yMin 99.230 xMax 280.211
```

门是 `crates/folio-core/src/content/format/pdf.rs` 的
`a_real_producer_pdf_agrees_with_poppler_on_its_boxes`：x 轴对到 0.01 pt，y 轴对到 1 pt 以内
（poppler 那个盒高是**行**的 16.03，我们的是**字号**的 12.0，差的就是那条 leading ——
把它当「y 翻反了」去修就真的修错了）。

**同一扇门里还钉着一条已知限制**：WPS 把词尾的空格算进了前一个字的前进量（`F` 宽 9.60 =
字形 6.67 + 空格 2.93），于是几何上没有缝、poppler 分得开的两个词在这位引擎那里合成一个，
摊平出来是 `TITLE OFTHE PAPER`。这条不是认命，是让它有名有姓：哪天引擎补回那一个空格，
断言先红，改期望的那一手才轮得到人来决定。
