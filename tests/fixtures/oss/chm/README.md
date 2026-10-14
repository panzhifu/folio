# CHM 样本

两本同一内容、不同码页的最小书，加它们的**源工程**（`.hhp` / `.hhc` / `.htm` / 一张 PNG）。

| 文件 | 正文怎么存 | 声明 |
| --- | --- | --- |
| `minimal-zh-utf8.chm` | UTF-8 | `charset=utf-8` |
| `minimal-zh-gbk.chm` | GBK | `charset=gb2312` |

两本的内容逐字相同，所以测试能直接比「解出来是不是同一串汉字」—— 这是 CHM 最大的一坑：
真实中文书九成按 GBK 存，而 `String::from_utf8` 那一族的失败方式是一排问号**且不报错**。

## 怎么重做

编译器是 [`rustchm`](https://crates.io/crates/rustchm)（FastChm 的 Rust 移植，自带 LZX 编码器），
下面用的是 `0.1.2`。它不在本仓库的依赖图里 —— **所以源工程要留在仓里**，否则将来重做只能凭记忆。

```sh
cargo install rustchm --version 0.1.2   # 或 cargo build --release 那份 crate
(cd utf8 && rustchm minimal.hhp)        # 产出 minimal-zh-utf8.chm
(cd gbk  && rustchm minimal.hhp)        # 产出 minimal-zh-gbk.chm
```

`gbk/` 那份不是「另一本书」：它是把 `utf8/` 的 `.htm`/`.hhc` 按 GBK 重打一遍字节、
把声明改回 `charset=gb2312` 之后的工程。改内容时两份都要改。

产物验证走 7-Zip（`7z e`）—— 它是独立实现，解出的字节与源工程逐字节相同才算样本没坏。
