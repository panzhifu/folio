# ZIP 压缩方法夹具

这两份是**生成**出来的，不是从哪本书里截的。`zip` crate 的读入口认 Deflate64 与
LZMA，写入口对两者都关着（`Compressing Deflate64 is not supported` /
`LZMA isn't supported for compression`），所以只能把带方法的字节摆进来当夹具。

生成命令（7-Zip 24.09；`page.xhtml` 都是可压缩的长文本，条目名都是 `page.xhtml`）：

```bash
printf '<html><body>%s</body></html>\n' "$(python3 -c 'print("<p>deflate64 fixture 压缩压缩压缩</p>" * 200, end="")')" > page.xhtml
7z a -tzip -mm=Deflate64 deflate64.zip page.xhtml

printf '<html><body>%s</body></html>\n' "$(python3 -c 'print("<p>lzma fixture 压缩压缩压缩</p>" * 200, end="")')" > page.xhtml
7z a -tzip -mm=LZMA lzma.zip page.xhtml
```

| 文件 | 方法 | 解压后 | CRC32 | sha256 |
|---|---|---|---|---|
| `deflate64.zip` | `Deflate64` | 8627 | `0F5F9BC9` | `475b0cc4f8940555330c397668201c885635fa9941bfbbac489958c10b6936e7` |
| `lzma.zip` | `LZMA:eos` | 7627 | `572F61CB` | `0a0e11470a922b8f04e414ac34e121fd145cd9ac809b241aa0f7e644dce8a843` |

其余三种方法（bzip2 / XZ / PPMd）由 `zip::ZipWriter` 在测试里现造。
zstd **不在**支持范围内：`zip` 的 zstd feature 要 `zstd-sys`（C 工具链），
与仓库的纯 Rust 交叉编译口径不符。
