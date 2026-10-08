# 测试样本约定

## 两个目录，规则完全不同

| 目录 | 入库 | 内容 |
|---|---|---|
| `oss/` | ✅ 提交 | 可自由再分发的样本：由测试代码**自己合成**的 EPUB/TXT/MD，或公版且来源明确的书 |
| `local/` | ❌ 已在 `.gitignore` 里 | 你自己拥有的受版权保护的书籍，只用于本机验证真实排版效果 |

`local/` 不入库不是"防止意外"，而是**性质问题**：把一本仍在版权期内的书提交进仓库，就是公开再分发。仓库设成 private 不改变这一点；而且 git 历史里的文件在推送后会被 fork、缓存和 reflog 留住，删不干净。

## 怎么用 local/ 里的书

排版观感靠人眼，不靠断言，所以这类样本走 `#[ignore]` 的显式冒烟测试，CI 上永远跳过：

```bash
cargo test -p folio-core -- --ignored --nocapture
```

被 `#[ignore]` 的测试**不能因为文件不在就通过**——那等于一盏永远绿的假灯。
它们在没有本地样本时直接 panic，所以只会在你显式要求时跑，且跑了一定给结论。

## 现有样本

- `oss/minimal-zh.epub` —— 合成中文书，4.7 KB。故意布成会出错的形状：
  manifest 顺序与 spine 顺序不同、文件名里有空格（href 写成 `%20`）、
  nav 条目带 `#fragment`、两份 CSS 需要按声明顺序拼接、第三章没有 `<h1>`（测标题回退）。
- `local/shejie-mauriac.epub` —— calibre 导出的 EPUB 2.0（无 nav，只有 `toc.ncx`），
  28 章 / 25 条目录 / 一份 2.4 KB 的 CSS。它抓到了合成样本抓不到的两个真 bug：
  **NCX 的元素名是大小写敏感的 `navMap` / `navPoint`**，以及出版书 CSS 里
  开头的 `@charset` 语句会把后一条规则吃掉。

## 合成样本才是自动化测试的主力

EPUB 解析的正确性其实**不需要**真书。要验的是具体结构，而这些都能程序化生成：

- **容器与导航**：`mimetype` 位置不合规、`META-INF/container.xml` 指向非常规 OPF 路径、NCX 与 nav.xhtml 并存、spine 的 linear/non-linear、加密资源（EPUB 4 DRM 声明）
- **中文排版**：`<ruby>`/`<rt>` 注音、`writing-mode: vertical-rl`、`text-indent: 2em`、`text-align: justify`、`line-height` 无单位与带单位
- **结构边界**：章内嵌图（含 SVG）、`epub:type="footnote"` 脚注、`columns` 多栏、`float: left` 绕图、超长无空格英文词、表格 `colspan`/`rowspan`
- **畸形与恶意**：未闭合标签、BOM、CRLF、UTF-16 XHTML、zip 路径穿越（`../../etc/passwd`）、高压缩比 zip 炸弹、非 UTF-8 字节序列
- **锚点稳定性**：同一本书改字号 / 改窗口宽度后重定位；改章节标题后 `NodeIdentity` 是否仍可解析

这套覆盖度远高于"一本真书跑通了"，并且可提交、可在 CI 重复。
