# 已钉死的事实

从实测与真机踩坑提炼，写代码前先读。改动任何一条 = 先在真机/宿主上重新验证。

## 框架（gpui-pre 0.3.9，锁版 `=0.3.9`）

- **2026-10-17 已升到 gpui-kit main `f8cf0bd9`（0.7.3）+ `gpui-pre =0.3.9`**。fork
  `panzhifu/gpui-mobile` 同步推到 `78384e0`：补了 0.3.9 新加的
  `PlatformDisplay::refresh_interval`（Android 返回 `None`，NDK surface 不报刷新率）、
  `Platform::on_displays_changed` 与 `PlatformWindow::on_display_changed`（Android 窗口
  绑一个 surface，暂为 no-op）。**两边都是精确 pin**：动一边必须同时动另一边，否则
  图里会出现两棵互不兼容的 `gpui`（0.x 的 minor 就是 breaking，`Window` / `Element`
  类型对不上）。升级顺序：先推 fork → 改 `crates/folio-app/Cargo.toml` 的 rev →
  `cargo update -p gpui-kit -p gpui-pre-mobile`。

- `TextSystem::resolve_font` 是 **panic 不是回退**（text_system.rs:370）：请求家族连同整串
  桌面回退名都不存在时直接 SIGABRT，Android 上这是常态。唯一公开判据是 `all_font_names()`
  （`font_id` 私有）。任何新加的文字（包括导航栏标签）都要显式指定设备真能解析的字族。
- `ShapedLine::paint()` / `WrappedLine::paint()` 是公开 API；`Window` 上没有
  `paint_line`/`paint_text`。
- `force_width` **不会**压缩行宽，不是 justify 钩子。
- gpui 有 `PlatformInput::LongPress`；`gpui-kit/examples/touch_selection` 是选中交互参考。
- **绘制顺序不是树的先序，而是“相交即上抬”**：`Scene::insert_primitive` 用
  `BoundsTree::insert` 算 order —— 与已有图元相交的那个拿到“相交集里最大 order + 1”。
  所以一个不透明的页面底色如果画在字形 canvas 里、而图片先入树，底色就会盖住图。
  阅读页把图片放在正文 canvas **之后**入树（`view.rs` 的 `image_layers`）。
- **`img` 走 sprite atlas，单张图超过设备的 `max_texture_dimension_2d` 就只写日志、
  静默不画**（`gpui-pre-wgpu` 的 `WgpuAtlasTextures::allocate` → `paint_image` 的
  `.expect`）。tile 用的是**图像原始像素**，而 GLES 上这个上限常见 4096、部分只有 2048；
  一张 1200×1600 的插图在 2.8 倍屏手机上就是 3360×4480 超过上限 —— 手机「书里的图看不见」
  多半是这一条。所以 `folio_core::content::format::shrink_image` 在第一次要画时把长边缩到 2048 以内
  （KOReader 的 `ImageWidget` 也是先缩再缓存）。
- 文字 run 的 `Font` 解析（`gpui_font`）要拿候选链去 `all_font_names()` 里线性扫；
  `all_font_names()` 每次调用都把整张字体表枚举+排序+去重。两者都**不能每帧做** ——
  绘制路径的 `FontCache` 缓存 `FontKey → Font`，设备字体表只在换书时读一次。
- `IconName` 画 `svg().path("icons/x.svg")`，字节由应用的 `AssetSource` 给：Android 入口链了
  `AllAssets`（代价：整套 Lucide 嵌进 .so）；iOS/HarmonyOS 的 `Application` 在 gpui-mobile 内部
  构造、没有注册口，**图标静默空白不报错**。移动端控件必须保留文字，图标只作前缀 ——
  只用图标的按钮在那两端会整颗隐形。
- release profile（opt-level z + fat LTO + strip + panic=abort）为 Android 体积调的，桌面共用；
  移动端与桌面共用同一张依赖图，workspace 根是唯一能声明 profile/patch 的地方。

## Android（Meizu 20 实测）

- 文档必须放应用专属外部目录 `/sdcard/Android/data/<包名>/files/`；`/sdcard/Download/` 属主是
  媒体提供者（另一 uid），scoped storage 下本进程读到 EACCES，文件名放对也不会有画面。
- 触摸只上报 `PlatformInput::Touch`，轻点由手势识别器合成 Tap 再派发 MouseDown/MouseUp，
  `on_click` 在真机成立。**惯量由平台层完成**（VelocityTracker + MomentumScroller → 合成
  `ScrollWheel` 事件），视图只累加 dy；再套一个惯量器会翻倍。
- **长按也不是平台事件**：`gpui-mobile` 只发 `PlatformInput::Touch`，`LongPressEvent` 由
  gpui 的 `TouchGestureRecognizer`（500ms 定时器 → `offer_long_press`）合成。2026-10-10
  在 Meizu 20 实测这条链通：选中的长按吸字、两端把手、菜单条都上屏，不必自己写计时器兜底。
- **浮层（Popover）在 Android 上可以点住**（2026-10-11 读者更正：早前「点不住」的记录已过时）——
  需要交互的面板可以走浮层；存量代码里那几处内联面板（书库确认、阅读页底栏模式）照旧工作，
  迁移到浮层是可选的后续优化，不是必须。
- 屏 1080×2400，scale 2.8；可用中文正文字族 `Noto Sans CJK SC`。
- **窗口属性只能在 UI 线程上写**（Android 16 / API 36 实测）：GPUI 的原生线程直接调
  `Window.setAttributes()` 抛
  `CalledFromWrongThreadException: Only the original thread that created a view hierarchy can
  touch its views. Expected: main Calling: android_main`（`Window.setAttributes` →
  `Activity.onWindowAttributesChanged` → `decorView.setLayoutParams` → `requestLayout`）。
  背光滑条就是踩在这上面：fork 的 `gpui_mobile::set_screen_brightness` 在原生线程直写，
  每次都只吞掉那个 Java 异常，所以背光从未真正变过 —— 改成走 `GpuiSystemUi
  .setWindowBrightness`（自己在 Java 里 `runOnUiThread`）后 `dumpsys display` 的
  `mScreenBrightness` 实测随 0.35 / -1 两档变化。凡是写窗口 / View 的活都照这条路走。
- 验证卫生：`screencap` 整帧 md5 必变（状态栏时钟），比对要裁状态栏数内容区差异像素；
  判断输入行为前先 `am force-stop` + `am start` 取基线（出现过幽灵触摸）。

### debug 编出的 `.so` 里**没有图标**，而且不报错

`cargo ndk … build --lib`（不带 `--release`）装到机器上，症状是**文字全正常、图标一个都不见**：
底栏四个、搜索镜、菜单三点、设置页每一格的图标，全是空白。没有 panic，没有 logcat 报错，
`folio.log` 里也一个字都没有。

原因不在 folio：`gpui-kit` 的 `crates/assets/Cargo.toml:28` 用 `rust-embed = "8.7.2"`，
**没开 `debug-embed`**。rust-embed 的默认行为是「debug 构建下不嵌字节，运行时按编译期记下的
**绝对路径**去读盘」，那个路径是构建机上的 `~/.cargo/git/checkouts/gpui-kit-…/assets/icons`。
桌面上 debug 跑没事（那条路径就在这台机器上，实测存在），手机上它不存在，于是每一次
`Assets::get()` 都落空 —— 而组件对「取不到图标」的处理是**什么都不画**，所以连一句错都没有。

两条实测的分辨方法，比看症状快：

```bash
strings -a android/app/src/main/jniLibs/arm64-v8a/libfolio_app.so | grep -c '<svg'
#   release：1830        debug：0
```

所以：**上机验证一律 `--release` 编 `.so`**（README 那条配方就是这么来的，`gradle assembleDebug`
只决定 APK 的签名，不影响图标）。想在 debug 下也嵌，得给 `rust-embed` 开 `debug-embed`，
那是上游的 feature，不在本仓库能改的范围里。

顺带一条同源的坑：debug 的 `libfolio_app.so` 带全量 DWARF，实测 **1.4 GB**；
`llvm-strip` 之后 139 MB。release 是 36 MB，不用剥。


## 语料与格式

- Markdown 适配器按 `#`/`##` 分章：以标题开头的语料 chapter 0 只有标题，看起来像
  「正文没渲染」，不是 bug。测试语料把正文段落放在第一个标题之前。
- CSS 子集是合同：`crates/folio-core/src/content/format/css/computed/apply.rs` 里那批
  match 臂（加上 `computed.rs` 的 `KNOWN_UNIMPLEMENTED`）之外的一律忽略并记录，
  不许临时加特例进引擎核心；要扩清单，先改清单（代码里那份）再写实现。
- `css-gap` example 的落差表口径：按元素排名的声明覆盖率，**不含**未实现属性的几何影响；
  结论只用于排优先级，不是渲染保证。「排版影响」一节另量**平均行墨迹宽**（`ink_width`）：
  `text-align: justify` 不改行数/高/左缘，只在行内摊开，只有墨迹宽量得到它（并把它的
  before 臂折回 `start`）。
- 2026-10-20 把这张表读完的结论：**声明级覆盖率已经见底，剩下的「不认识」大多不是落差**。
  《哈利·波特全集》那 10.9% 里 5149/5647 是 `duokan-text-indent` —— 多看转换器把标准
  `text-indent` 连同厂商前缀那一条一起写在同一条规则里，17 条规则逐条对过，16 条两侧同值；
  《蛇结》那 102 条 `list-style-image: none` / `list-style-position: outside` 要的正是
  CSS 初值，也就是引擎现在的行为；`padding: 0 auto` 那 240 条在 CSS 里是非法值
  （`padding` 不吃 `auto`），拒掉才与浏览器逐字一致。所以看到这张表的百分比要先拆属性，
  别把「不认识」直接读成「屏上还差多少」。
- `vertical-align` 在《哈利·波特全集》那 241 条**不在 `<td>` 上**（旧记录错过一次），
  实测选择器是 `sup img`，落点 `[p>sup>a>img]` —— 上标里的注号图。它仍然几乎不改屏上
  的东西，但理由不是「没有表格布局」，而是 `layout::collect` 摆图片时**逐级累加祖先的
  `baseline_shift`**，UA 给 `<sup>` 的那一档上标位移早就落在那张图上。要重测这类结论，
  用层叠自己的选择器匹配（`RuleSet::matching_declarations`），别用正则扫 CSS。
- 块级 `width` / `max-width` / `height` / `max-height` 与 `margin: auto` 均分是 2026-10-20
  这一刀接的，判据在 `layout::collect::solve_block_width`。**这一族从前就报 `Applied`，
  所以 `css-gap` 那张表上一个痕迹都没有** —— 声明解出来了不等于排版层读了它，
  这两件事之间的缝只能靠「同一份排版、把声明整条丢掉」这种对照去量。
- **两端对齐（`text-align: justify`）与字距（`letter-spacing`）的 RTL 一期不做**：
  余量的摊开方向与空隙计数、字形右推的方向在 RTL 下都反转。`layout::Justify` 对 RTL 行
  直接给 `None`（该行不摊）；字距在绘制（`paint::GlyphRun`）的 RTL 段跳过右推，命中/选区
  在 RTL 段则不精确（两条 shaping 路径的口径不同）。LTR 下三处逐字同源、逐像素对得上；
  RTL 下宁可不动也不半接，记在这里而不是假装成了。本机四本书的 css/xhtml 里
  `letter-spacing` 出现 0 次、带 justify 的 RTL 书为 0。

- **CB7 / CBR / CBT 的 7z、RAR、tar 只有顺序解压**：要第 100 页得从第 1 页一路解过来，
  滚动时每页重解是 O(n²)。所以第一次打开把图片条目解到
  `<数据根>/cache/comics/<sha256(路径+长度+mtime)>/`，之后按相对路径现取。缓存是派生数据，
  删掉只会触发重解；目录用 `.folio-done` 标记完成，**内容记着「解出来多少字节 + 是哪本书」**
  （算总量不必再走一遍目录，而源文件没了一本才算得出那是孤儿），没标记的半成品会被整个删掉重来。
  解压回调自己拒绝绝对路径、`..` 与盘符，且只写图片。三件事必须一起记着：
  **键里带着路径，而导入的最后一步要把副本改名成哈希名**，所以那一格得跟着搬
  （`comic_cache::relocate`，不搬就是导入那一趟白解、开书时整本再解一遍）；删书要连着删它
  （`drop_for`，`folio-app` 的 `library::delete_book` 那一步）；每次解压之后把整棵压回
  `MAX_CACHED_BYTES`（那是**政策不是实测**：源文件已不在的先删，剩下的按标记 mtime 从最旧的丢）。
  这一族唯一的量具是 `Metric::ComicExtracts`：重复解压给读者的字节一模一样，屏幕上看不出来。
  **已知没管的一件事**：正在读的那一本在 `prune` 里没有豁免（只有开书那一次盖 mtime，逐页取字节
  不写盘），所以「开着旧漫画 + 解压一本新的 + 总量正好超上限」这一种组合会让被丢掉那一本的
  页变空，重开才恢复。上限那一档就是为了让它尽量不发生。
- **CBR 用纯 Rust `rars`，不绑 unrar C++**：RAR4/RAR5 的正文是 LZSS + PPMd + 滤波器，
  还有 solid 字典、跨卷与 CRC/BLAKE2sp；手写等于重做一遍 unRAR。`rars` 0.11 是独立实现
  （Apache-2.0，MSRV 1.89），支持 RAR 1.4–7、solid/多卷、store/LZ/PPMd，`cargo ndk`
  交叉检查过。关掉默认 feature（write/recovery/encryption/parallel），只带读路径；
  **加密 RAR 这一版没有密码入口**，会明确报错而不是读出一本乱码书。目录缓存与 CB7
  共用（`ensure_rar`）。
- **AZW4 不是新 PDF 引擎**：它是 PalmDB 里一条以 `%PDF-` 开头的记录，取出来交给
  `PdfDocument::from_bytes`，页表/书签/封面复用 `format::pdf::read_pdf`。判定时先看
  记录里有没有 PDF，再看 `BOOKMOBI`，所以 `BOOK` 系的其他 creator 也能认出来。
- **PalmDOC 的记录 0 头偏移 12 是加密方式，不是「当前位置」**：非 0 就是加过密，那一本的
  正文永远解不开。MOBI 那一路早就读这一格（`mobi::header`），PDB 这一路以前没读，
  于是 libmobi 的真样本 `sample-drm-v1` 被「解压」成一屏乱码交出去，而屏幕上看着有正文。
  同一张头还有两处别再抄错：偏移 0..32 是**数据库名**（PalmDOC 没有 EXTH 时书名就只有它，
  mobitool 报的 Title 就是那一格），偏移 4 是正文总长（头里的记录数会说谎，按实际记录夹住）。
- **`TEXt`/`REAd` 只说得出「文本记录」，说不出那文本是纯字还是标记**：早期 PalmDOC
  （TextReader / OEB1 那一类）记录里直接写着 `<HTML><HEAD>…`，libmobi 两本真样本里就有一本。
  按纯文本收会把整份标记转义成屏上一堆尖括号。判据锚在**开头**（`<!doctype` / `<html` /
  `<?xml`），不能「整本里含 `<html` 就算」—— 一本讲 HTML 的小说会含那几个字符。
  同一批样本还写着 `<dc:*>` 那一块（早期 Mobipocket 的书目就在这里，没有 EXTH）：
  认**带 `dc:` 前缀**的标签名，裸 `<title>` 是页面标题、不是书名；`<dc:Identifier>` 要按
  scheme 筛，真样本第一条是 `id="uid"`，不是 ISBN。
- **Word 的表格里每个格子是一整段**（`<w:tc><w:p>…`，四份真 Word 文件都这形状）：所以
  「一行拼成一个 `<p>`」的那一步必须先把格子里的段落包拆掉，否则交出去的是
  `<p><p>…</p></p>`，而 html5ever 见到 `<p>` 里开 `<p>` 会当场关掉外层 —— 那一行被劈成两段。
- **DjVu 用纯 Rust 的 `djvu-rs`（0.43，MIT，无 GPL），不引进 DjVuLibre**。三条按上游源码
  注释与本机实测钉住：
  ① `Document::open` **只吃 bundled**（自包含的 DJVM），indirect（索引 + 一页一个文件）要
  `Document::open_dir`；实测 `open_dir` 对 bundled 同样给得出正确页数（两份真样本各开一次），
  所以入口走 `open_dir` 一条路 covers 两种形状。**没验到的那半边如实记着**：本机没有一份
  真 indirect DjVu（`djvu encode` 与 CLI 的 `split` 都只产 bundled），所以 indirect 那一支
  是按上游文档 + `open_dir` 在 bundled 上的实测推的，不是撞到的。
  ② 书签（NAVM）与书目（`(metadata …)`，也就是 ANTT / AGINT 那一块）都解得出来，也都接了。
  书签那份 71 页的公版 spec 样本实测 100 条、最深五层，url 三种形态：`#64`（**1 起**的页号）、
  `#p0064.djvu`（按 DIRM 里 `P` 条目的顺序换页号）、空串（根那一条，只管分组）。
  **绝不猜组件名里的数字**：真样本里第一页可以叫 `p0001_1.djvu`，另一本 Internet Archive 的
  页组件从 `_0000` 起 —— 按数字减一在后者上整整差一页，而那正是「点目录跳错地方」那一类最难查的错。
  书目那一格的门是**合成**的：手边四份真 DjVu（含两本 IA 公版扫描）`metadata()` 全回 `None`
  —— IA 那一族的元数据写在旁边的 `.meta` 文件里，不在 DjVu 内。所以那条门用上游编码器
  （`djvu_mut::set_metadata`）往真样本上加一块再读回来：钉的是「那一块怎么落到书目的格子」，
  不是「那一块的字节长什么样」。
- **quick-xml 0.42 把实体引用报成**独立事件**，只吃 `Event::Text` 的解析会静默丢字符**：
  `Text("一个城市的") + GeneralRef("#38") + Text("与它的灯。")`（实测）。四处各自攒文本的地方
  都中过这一枪 —— EPUB 的 OPF 书目与 NCX / nav 标签、DOCX 的 `w:t` 与 `docProps/core.xml`、
  ODT 的 `text:p` 与 `meta.xml`、漫画那份 `ComicInfo.xml`。症状是**书名与正文少一个 `&`**
  （`Harry Potter & the ...`、`R&D`、`Tom & Jerry` 满地都是），而屏幕上看着像原书就这么印的。
  收口在 `format::xml_piece`：`Text` 与 `GeneralRef` 两支都走它，实体还原成 `&名;` 再统一 unescape。
  门四条（`entity_references_*` 那几条），**把 `is_reference` 那一支掏空，四条一起红**。
- **ComicInfo.xml 里 `<Chapter Page="…">` 的基数没找到可信权威**（Komga 那份生产级 DTO
  干脆没建模它、calibre 的漫画入口不读这份便条、手边也没有带 `<Chapters>` 的真包），
  所以**故意不读**那一族：猜 0 起还是 1 起的后果是「每一条目录都跳到隔壁那一页」，
  比目录少一条糟得多。书目那一半（Title / Writer / Series / Number / Genre / Tags /
  Year + Month / LanguageISO / GTIN）没有这个歧义，按 Komga 的口径接了（角色全进
  `dc:contributor`，只有 `Writer` 进 `dc:creator`；多值按**逗号**切）。

- **`folder.jpg` / `cover.jpg` 是封面缓存，不是页** —— CDisplayEx / Kavita / YACReader 往包里写
  `folder.jpg`，Calibre 导出写 `cover.jpg`。从前没人认，于是它们被自然序排进页表：同一张封面
  在书架上一次、在正文里多出来一页（数字命名的页排在字母之前，所以多半落在**书末**，症状是
  翻到最后凭空多一页封面）。现在 `folder.*` 优先于 `cover.*`（前者是阅读器写的缓存，与封面
  同义得更彻底），同类按自然序取第一张，子目录里的 `images/cover.jpg` 也算，大小写都算。
  **一个兜底**：包里的图只有那一张时留着当页 —— 摘掉它就是「一页都没有」，而那一格当场判
  打不开，比书架与正文共用同一张图糟得多。门四条 + 一条 CB7 的（那一族走磁盘缓存，四个
  provider 各接一次，漏一个就有一族多一页）。
- **DOCX 的列表分两半，形状与记数各在一处**：段落的 `w:pPr/w:numPr` 只交 `w:ilvl`（层号）+
  `w:numId`（实例号），**怎么记数在 `word/numbering.xml`**（`w:num` → `w:abstractNumId` →
  那一层的 `w:numFmt`），所以跳一层就认不出号。真样本 `numbering.docx` 量出三件事：
  ①两个生产者（Word 与 LibreOffice 导出的 docx）都把 `numPr` **写在段落自己身上**，所以没去
  `word/styles.xml` 找样式间接那一层（那几份里的 `List*` 样式只是出厂定义，正文一条没引用）；
  ②`numId` 1 与 2 **都是 `decimal` 但是两份列表**，Word 的纸面是「1 / 1、2」，合成一条就成了
  「1 / 2、3」—— 所以归约时换了实例就关掉重开，而不问记数；③`w:lvlText` 那种模板
  （`Section %1.`）画不出来（标记由 `layout::marker` 按记数折），取同一条 `w:lvl` 里的 `w:numFmt`，
  代价是「Section 1.」读成「1.」。认不出的那几档（实例没有、层超出定义、整份文件缺失）一律
  **只留缩进不画号**：编一个圆点或一个「1.」是造出一个书里没有的东西。
- **DOCX 脚注那三种 `w:type` 格子不能按 id 判**：`separator` / `continuationSeparator` /
  `continuationNotice` 的内容是「画页底那条线」的东西，真样本 `without_numid.docx` 里它们占着
  id `-1` / `0` / **`1`**，而 `footnotes.docx` 里那条**真注文的 id 也是 `1`**。所以判据只能是
  `w:type` 在不在。另一条：注体的段间用空格接、`w:footnoteRef`（注里那个标记自己）不计入注体的字。
  引用落成的形状必须是弹层那条链认的（`<sup><a epub:type="noteref" href="#fnN">` +
  `<aside epub:type="footnote" id="fnN">`，注体必须在**引用它的那一章**里 —— 弹层只在本章找 id）。
  同一条注被引用第二次（Word 的交叉引用）：号不变、注体只摆一份，否则一章里两个同 id 的元素，
  解到的是后那一个而屏上看不出异常。
- **ODT 的列表同样是两半，但形状来自元素本身**：`text:list` 是**嵌套元素**（层号取嵌套深度），
  记数在 `text:list-style` 里 —— 而且定义**两处都可能出现**（`styles.xml` 的命名样式与
  `content.xml` 的 automatic-styles），扫的时候不看它的父容器。真样本量到的四种记数：
  `text:bullet-char` 的 `•` `◦` `▪`（→ disc / circle / square 三档，`liste-courses.odt` 一份里
  就前三层）与 `➢`（表外的自定义符号 → 圆点）、`style:num-format="1"` 与 `"A"`（十进制与
  上写字母，`welcome.odt`）。**`text:style-name=""` 是 LibreOffice 真写出来的**（那份 welcome
  里两个注解列表就是这样），认不出的一律圆点。`text:continue-numbering`（接上一串的号）不读。
- **ODT 的 `text:note` 是内联的**：引用点在句子中间，注体（`text:note-body` 里那些 `text:p`）
  也写在这里 —— 与 DOCX 的「注体在另一份部件里」正相反。所以进 `text:note` 必须先把外层那段
  寄存起来，否则注体里那个 `text:p` 会把外层段落提前 flush，症状是「正文」与「尾巴」被劈成两段。
  号取 `text:note-citation` 自己写的那串（Writer 已算好，含 `a.` 这种写法），那一格空着才按出现序数。
  **手边五份真 .odt 里没有任何一份带 `text:note`**：这一路的形状按 ODF 的三格写、由合成夹具钉，
  「真生产者到底怎么摆」没被量过。
- **DOCX / ODT 的依赖候选实测**（在仓库外的空项目里跑真样本，`/home/noke/tmp/docxdep`、
  `/tmp/probe-*`；代价按「`cargo tree -e normal` 的闭包减去 folio-core 已有的」算）：
  `docx-rs` 0.4.22 净新增 **1 个包**（闭包 57 个全在 folio-core 那 339 个里），列表那一半它
  确实替得掉（每段的 `numbering_property` 与 `numberings.abstract_nums[].levels[].format` 都是
  `pub`，实测 8 个 numPr 段落交出来），内嵌图片走 `doc.images`（`having-images.docx` 交 4 条，
  而 `doc.media` 恒空 —— 别按那个判它读不到图）；**脚注交不出**（七份样本全 `footnotes=0`，
  包括真有 `<w:footnote w:id="1">` 那一份：`src/reader/` 里 footnote 出现 0 次，而
  `Footnotes.footnotes` 是 `pub(crate)`）；**书目交不出**（`CoreProps.config` 与
  `CorePropsConfig` 九格全私有、只有 builder 式 setter，reader 根本不打开 `docProps/core.xml`
  —— 注意不能用样本读出的 null 当证据，那几份的 core.xml 本来就是空的）。
  `docx` 1.1.2 要 +8 且带 `bzip2-sys`（C）；`ferrodoc-docx` +2、`ferrodoc-odt` +3（pandoc 那棵
  AST，`ferrodoc` 整包 +19 且会把 `xml5ever` / `comrak` 这套**与 html5ever / pulldown-cmark 重复**
  的栈搬进来）；`docx-to-md` 0.1.0 只要 **+1**，实测两种格式都读得动（`Hello[^fn-1]` +
  `[^fn-1]: World`、`1. / -` 按实例分开、docProps 的作者与时间都拿到了）。**但它把图片摊成
  base64 内联**（`having-images.docx` 一份 50 KB）、书目写成 Markdown 顶上的一行 HTML 注释，
  而产出是 MD 不是 XHTML —— 与「字节不入库」和「provider 交引擎就绪的 XHTML」两条都顶，
  所以结论是**不换**：它适合当对照实现与夹具发生器，不适合当我们的 reader。
  顺带一条与格式表有关的：`rwml` 0.1.5（MIT，纯 Rust 的**旧 .doc / Word 97-2003** 读写）
  在仓库外量过一次是「净新增 3 个包」，**接进来以后按这一份锁重量的数是 2 条包名** —— 见下面那一条。

- **`.doc`（Word 97-2003）接进来之时量过的五条**（依赖账按 `Cargo.lock` 的包名相减，
  时间账按同一份 9 KB 的最小书交替跑 7 轮）：
  - **`default-features = false` 是这一条依赖能进图的前提**：那条默认 feature 叫 `docx`
    （OOXML 那一半，我们已经有自研的 `.docx`），开着它会把**第二个** `quick-xml`（它要 0.41，
    图上是 0.42）与第二个 `zip`（9.0）搬进来。关掉之后 `Cargo.lock` 只多两条包名：`rwml` 自身
    与它的容器 `cfb`；`cfb` 带的 `fnv` / `byteorder` / `uuid` 与 `rwml` 带的 `encoding_rs` /
    `thiserror` **一条都没新增**（`fnv` 此前只在锁里、从没被编过，所以 `cargo check` 那一趟
    看着是三个 crate 在编，账要按锁的包名算）。`wasm-bindgen` 那一支只在
    `cfg(target_arch = "wasm32")`，桌面与 Android 都不吃。
  - **FIB 的 `wIdent` 是 `0xA5EC`，不是 `0x0170`**（Word 6/95 那一代写后者）。那三份
    LibreOffice 按 `MS Word 97` 导出的真样本开头就是 `EC A5`，而 `rwml` 的 `fib::parse`
    只认 `0xA5EC` —— 所以判定层与 provider 用同一条判据（`format::doc::ole2_shape`），
    Word 6/95 那一代两边都不认，症状是一次说得清的拒绝。
  - **判定层不能拿 `Document::open` 的成败当判据**（CHM 那一支可以，那一问只读头）：
    `rwml` 的 `open` 只接受**整份字节**，判一次等于先把正文解一遍。实测 9 KB 的最小书
    `open` 283 µs、`model()` 再 83 µs，而一份「OLE2 但不是 Word」的容器 329 µs 就报
    `MissingStream("WordDocument")` —— 快慢都不是问题，问题是带图的 `.doc` 是几十 MB 量级，
    而判定排在哈希那一趟之前。开目录读流名 + 那两个字节是 O(1)。
  - **`rwml` 不读 `SummaryInformation`**（全 crate grep 一处都没有）：`.doc` 的 Title /
    Author / Keywords 就存在那一格，所以它的 `core_properties()` 交回全空（那三份真样本实测）。
    书名只能回文件名、作者那一行不显示。要补就得自己读那一格属性集（[MS-PROPSET]），
    而那三份样本里没有一条填好的作者可钉 —— 记在「后置」，不是「已知能读」。
  - **`.doc` 的正文不止一块**：`DocModel::regions` 按子文档分区（`Main` / `Footnote` /
    `HeaderFooter` / `Annotation` / `Endnote` / `TextBox`），页眉页脚被 promote 进 `setup`，
    **其余还在 `blocks` 里**。整份照读的症状不是少字而是**顺序错**：注体与文本框的字插在
    正文中间，而 `chapters.char_len` 与 `Locator::offset` 数的都是这一串。注的**引用位置**
    它交不出（它自己的注释写着 `.doc` 那一支的 note id 是合成的），所以锚接不上。

## 支撑「自研排版」的三条实测结论

- **排版可以离开窗口在后台线程完成** —— `Arc<WindowTextSystem>` 可 move 进
  `background_spawn`，缩放窗口时布局不卡主线程（后台化仍欠着，见 roadmap）。
- **gpui 自带 shaping**（cosmic-text + swash + harfrust），kerning / 连字 / bidi 现成；
  `ShapedLine::paint` 内建文本对齐。
- **竖排不等于旋转** —— 横排 shaping 里 `glyph.position.x` 是推进量、`.y` 是基线偏移，
  两者对调即得 CJK 竖排，不需要 transform（拉丁侧转除外，显式降级）。
