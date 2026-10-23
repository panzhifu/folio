# `.doc` 样本（Word 97-2003）

三份 LibreOffice 按 **`MS Word 97`** 过滤器导出的最小书，来自 [`rwml`](https://crates.io/crates/rwml)
0.1.5 自带的那份公开语料（`corpus/public/benchmark/sample/`）。它们的源是同一个仓库里的合成
`.docx`，**不含任何第三方文本**，许可随那份仓库的 MIT（`LICENSE.rwml` 是许可证的副本）。

| 文件 | 里面有什么 | 在这一版量到了什么 |
| --- | --- | --- |
| `nested_tables.doc` | 一张表、两行，每行一格：`Outer cell text` / `Inner cell text` | 格子里是**完整的一段**，摊平成两行读；`wIdent` = `0xA5EC` |
| `floating_wrap_policy.doc` | 一段 25 个字（`Wrap before.  Wrap after.`），外带一个浮动形状 | 浮动形状的字不在正文那一区（`regions` 只交 `Main`） |
| `floating_text_bearing.doc` | 一段 49 个字，浮动的形状里还带字 | 同上：带字的浮动形状**不摆进正文** |

三份共同量到的一条：Word 97-2003 的 FIB 开头写的 `wIdent` 是 `0xA5EC`（不是老 Word 6/95 那一代
的 `0x0170`）。判定层（`content::format::doc::ole2_shape`）与 `rwml` 自己的 FIB 检查因此是同一条
判据 —— 不会漂成「判定收下了、provider 说这不是 Word」。

## 这三份**没**覆盖的

标题层级、列表、脚注、图片，以及**中文字节**（三份都是 ASCII 正文，FIB 报的 `codepage` 是 1252）。
归约那一层的这些形状由 `content/format/doc.rs` 里手搭的 `DocModel` 钉（`rwml` 的类型全是 `pub`，
所以那一层不必吃文件），但**带图 / 带列表 / 带标题的真 `.doc` 还没撞到过** —— 撞到之前别当成
已验过（记在 `plans/README.md`「真样本覆盖」那一行的「只有合成夹具」那一批里）。

`cp936` / `cp950` 那两条码页支路同样没撞到过：GBK 的字节只在 piece 标了「压缩」的那一段里出现，
而压缩那一段是**整篇全是单字节字符**时才会用到 —— 一本中文书真撞上它时，正文会存成 UTF-16，
所以这一条大概率不会咬人，但没量过就是没量过。

## 怎么重做

手边有 LibreOffice 时（这三份就是这么来的）：

```sh
soffice --headless --convert-to "doc:MS Word 97" 那份.docx
```

或者直接从 crates.io 拉 `rwml` 0.1.5，取它 `corpus/public/benchmark/sample/` 那三份 ——
`cargo` 用过的本地缓存里就在 `~/.cargo/registry/src/*/rwml-0.1.5/`。

跑门：`cargo test -p folio-core --lib content::format::doc`（那两条真样本门**不带 `--ignored`**：
样本在仓库里）。
