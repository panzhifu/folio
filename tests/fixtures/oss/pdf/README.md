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
