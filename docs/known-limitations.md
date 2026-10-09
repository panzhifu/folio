# 已知限制

这份文件是**承诺的边界**，不是待办清单。引擎遇到清单外的写法时的行为统一是
「忽略该声明、保留内容」——一本书因为作者用了 `float` 就排不出版，
是最不能接受的失败方式。

对应实现位置都在括号里。

## CSS（`crates/folio-core/src/css/`）

| 写法 | 现在的行为 | 为什么 |
| --- | --- | --- |
| `:nth-child()`、`:hover`、伪元素、属性选择器的 `~=^$|= ~` 变体 | 整条**选择器作废**，规则体一起丢，计入 `ignored` | 部分生效会让作者样式以不可预期的方式命中内容，宁可整条不要 |
| `border` / `background` / `background-color` / `text-shadow` | 接受、不生效、**不计入** `ignored` | 出版书里满地都是；计入会把真正需要关注的「没预料到的写法」淹掉（`KNOWN_UNIMPLEMENTED`）。块级装饰要的是「块盒矩形」，而排版层现在只产出行盒，这条和分页/双页的上屏接线一起定 |
| `text-decoration` / `text-decoration-line` | **已生效**：`underline`、`line-through` 进 `ComputedStyle` → `RunStyle`，桌面走 gpui 的 `.underline()` / `.line_through()`，移动端走 `TextRun` 的 `underline` / `strikethrough`（都是 1px、按字色）。线型与颜色 token 认但不改画法；`overline` / `blink` 整条不接并计 `ignored` | 装饰线不计入行盒高（CSS 也是），所以它只是绘制信息。`<u>` / `<ins>` / `<s>` / `<del>` / `<strike>` 由 UA 表给默认值，作者不写也有 |
| `width` / `height` / `max-width` / `max-height` | 只在**替换元素（图片）**上生效；块级元素的这些声明接受但不生效 | 一期只做图片的尺寸，块级盒宽跟随版心。图片尺寸见 `layout::image_size` |
| `font` 简写 | **已生效**：`[style] [weight] size [/ line-height] family`，字族取第一候选并去引号。**有意不重置没写到的子属性** —— CSS 规定简写要把其余拉回初值（含 `line-height: normal`），而我们的行距来自读者偏好，一句 `font: 1em serif` 把读者的 1.75 改回 1.2 是产品级的错。`small-caps`、系统字体关键字（`caption` 等）整条作废并计 `ignored` | 老 EPUB 里 `font:` 简写满地都是，只认长写法等于把这些书的字号与字族丢掉 |
| `margin` / `padding` 简写 | **已生效**：1 / 2 / 3 / 4 值按 CSS 的规则展开成上右下左；值数超过 4 或任一个值不认识则**整条作废**（不做半套）。`margin` 的 `auto` 折成 **0**：当前盒模型里正文块使用宽度等于可用宽度，CSS 自己也算出 0；等排版层按 `max-width` 收窄块级宽度时才需要改成均分（那一条改动的位置就在 `computed::edge_px`）。`padding: auto` 仍非法 | 这本《蛇结》917 个元素带胜出的 `margin` 简写，旧实现只认单值形式，于是整条作废 |
| `word-spacing` | **已生效**：可继承，折行与绘制宽度都算它（每个空格之后加一次，行尾被裁掉的那个空格不算） | 见 `RunStyle::advance` 与 `RunStyle::extra_for_spaces` |
| `vertical-align` | **部分生效**：`super` / `sub` / `<length>` / `<percentage>`（百分比按 CSS 相对自己的 line-height）折成 `RunStyle::baseline_shift`，绘制层减一次；`baseline` 是 0。**`top` / `bottom` / `middle` / `text-top` / `text-bottom` 不接并计 `ignored`** —— 它们要的是行盒对齐机制。上标位移是**我们自己定的常数**（super 1/3 em、sub 1/4 em，按各自字号），没有浏览器可量，所以不假装那是实现值 | `<sup>` / `<sub>` 由 UA 表用同一条式子给，带 class 的上标与裸 `<sup>` 同字号时一样高 |
| `list-style` / `-type` / `-position` / `-image` | 接受、不生效。列表标记一期**根本不画**，所以作者写 `none` 时恰好与我们的输出一致，写 `disc` / `decimal` 时是真落差 | 后处理标记要的是「合成一个 marker 盒」，属排版层；《蛇结》那 51 个元素写的正是 `none` |
| `margin-top: 10%`、`text-indent: 5%` | 判为不支持，**不猜** | 百分比按包含块宽度折算，那是排版期才知道的数，层叠期算不出来 |
| `line-height: 1.5px` 之类带单位行高 | 折算成倍数（除以**自己的**字号） | 与 CSS 一致；代价是行高倍数被冻在计算结果上，改字号时不会重算 |
| `@media` / `@supports` | 无条件收进内容，不做条件判断 | 阅读器没有「屏幕宽度切换样式」这种语义 |
| `@font-face` / `@page` / `@keyframes` | 丢弃，计入 `ignored` | `@font-face` 真要接是 `add_fonts` + 字体清单的事，属 M6 |
| `position` / `flex` / `grid` / `transform` / `transition` | 完全不在清单内 | 自研排版引擎不承担版式级布局，见开发方案 §2.1 |
| 外部字体、`rem` 的根字号跟随 `html { font-size }` | `rem` 一律按**读者偏好字号**折算 | 让读者的字号说了算，比让书自己改全局字号更符合阅读器的立场 |

### 这一张表有多准：用仓库里的书量

上面每一行是「定性」的，落差到底多大要数出来。工具在
`crates/folio-core/examples/css-gap.rs`，无界面、纯引擎，跑法：

```sh
cargo run -p folio-core --example css-gap                 # 仓库那两本 fixture
cargo run -p folio-core --example css-gap -- 某本书.epub   # 也可以指别的书
```

它按**元素**而不是按声明排名（`border-bottom` 写三十次只落在三个小元素上，和
`text-align` 落在全书两千个段落上不是同一种落差），判定走层叠用的同一份
`css::computed::declaration_status`，所以这张表和实际渲染行为不会分叉。

仓库里那本《蛇结》（28 章、2440 个元素、4960 条胜出声明）在 2026-10-09 这一轮前后的数：

| | 改前 | 改后 |
| --- | --- | --- |
| 至少有一条「胜出但没接住」声明的元素 | 1019（41.8%） | 52（2.1%） |
| 值/属性完全不认识的声明 | 917 | **0** |
| 认得但不生效 | 205 | 154（只剩 list-style 三件 + 一处 `background-color`） |

**但几何上的差很小**：全书每章最后一行的行底累加，正文高从 102179px 到 102224px（+45px，+0.04%），
行盒数 3120 一条没变，平均行左缘也没动。原因是这本书的 `margin: 1em 0` / `0.67em 0`
与 UA 表给 `<p>` 的 1em 上下边距本来就基本重合，被丢掉的常常是「同一件事的第二次声明」；
而那 917 条里的左右边距多数写在 `<body>` 的 class 上，排版从 body 起算，不吃它自己的外边距。
+45px 就是 51 个注号被抬起来的那部分（每个约 0.9px）。结论：**声明落差表说明「我们在骗谁」，
几何差说明「读者看得见多少」，两个都要看，只看一个都会做错决定。**

## 排版（`crates/folio-core/src/layout/`）

- 断行按**单字前进量**累加，行宽按**实际 shaping** 报告并用于对齐。两者在拉丁文上
  有亚像素差异（gpui 自己的 LineWrapper 也是这样），所以极端情况下某行可能比
  可用宽度窄一点点或宽一点点，不会溢出成两行。
- **不做父子外边距折叠**。嵌套 `<div><h1>` 时子元素的 top margin 会计入父高度，
  结果是比浏览器多一档空白。兄弟之间的折叠是做了的——那才是段间距看着对不对的关键。
  选择代价是「多一档」而不是「重叠」，因为重叠会把文字压在一起读不了。
- **负外边距按 0**。书里用负边距做视觉效果的不多，但它能让行盒重叠，代价不成比例。
- **图片已接上**（M3-E3）：行内图是 `LineFragment::Image`，宽度进入折行累加，底贴基线；
  `display:block` 的图独立成行、按容器 `text-align` 对齐。按 `width`/`max-width`/`height`
  缩放（px/em/rem/%，`auto` 用固有尺寸并按纵横比）；未写 `max-width` 的超宽图也会被
  夹到版心，绝不溢出。命中不到资源的 `<img>` 整张跳过（不塌空盒、不留占位）。
- **`float` 绕图仍未接**（M4）：`float: left/right` 接受但不生效，图会照常占一行。
- **SVG 没有固有尺寸**：`image` crate 不解 SVG，`image/svg+xml` 的固有尺寸算 0，
  降级成「用指定宽、否则用容器宽、比例按 1:1」。Raster（png/jpeg/webp/gif/bmp）正常。
- **百分比高度按 `auto`**：`height: %` 需要包含块高度，一期拿不到，按纵横比处理。
- **图片字节常驻内存**：`FlowDoc.images` 在打开一本书时一次性载入内存，大图多的书
  内存占用随之上升；惰性加载留待后续。
- **行内图占位用一个哨兵字符**（U+FFFC）：可见文本里已过滤，但源码若真含该字符会被
  当成图片位（现实里几乎不会）。
- **ruby / 竖排 / 多栏** 都还没接，分别是 M6 与 M4。
- `text-indent` 最多允许吃掉半个版心，`width` 再窄也不会让首行变成零宽。
- 空白折叠跨节点是一次性的（`Collector::pending_space`），但**嵌套元素自己的
  `white-space` 不单独判断**，整段按外层块的设置走。`<p>` 里藏一个 `<pre>` 会按 `p` 折叠。

## 分片（`crates/folio-core/src/layout/fragment.rs`）

行为统一是「尽量照作者的意图，实在不行先保证能读」：

- **断点处的上下 margin 被丢掉**：第 N 页末行下面那段空白不会出现在第 N+1 页开头。
  CSS 规定两侧都保留，但那会让每页顶部多出一段随书而异的空白，看起来像「排版不稳」。
  这里选稳定，并且是有意选的。
- **`orphans` 与 `widows` 冲突时保 `orphans`**：一页只剩三行的段落里切一刀，两侧不可能都满足
  初值 2。这时宁可让下一页顶着一行，也不让本页底部剩一行 —— 页底孤行比页顶寡行更刺眼是
  排版传统的共识。整段能挪到下一页时优先挪（`orphans_and_widows_conflict_moves_the_whole_paragraph`）。
- **`break-inside: avoid` 在块比一页高时让位**：照样强切。一本书因为一张长表格或一幅插图
  排不出版，是比违反作者意图严重得多的失败。
- **多栏做到「一页几栏」为止**（2026-10-09）：`FragmentOptions::columns` 让**一片代表一栏**，
  一页是连续 `columns` 片；作者的强制断点会把本页剩下的栏补成空片（`BreakToken::pad_until`），
  所以文档中间每页恰好 `columns` 片，末页可能不满（补满会让读者翻到一页空白）。
  切点本身一条都没改 —— 四条规则只关心「这一栏装到哪儿」。
- **栏是顺序填满的，不做高度平衡**：开发方案 §4.5 初稿设想的是「平衡高度的分栏分片」，
  实现时改成填满一栏再进下一栏。理由是阅读器是连续翻页：各栏长度不一时读者要在页内跳栏，
  而顺序填满保证「读完这栏才到下一栏」。代价是末页各栏可能长短不一（尤其是内容刚好
  断在页中间时），这是选边的结果，不是漏掉的 bug。
- **作者写的 `column-count` / `column-gap` 仍然接受不生效**（留在 `KNOWN_UNIMPLEMENTED` 名单里，
  不计入 `ignored`）：真正的 CSS 多栏是**块级**的，同一本书里只有某几个块分栏，而栏宽决定折行，
  要在排版期按块切宽度 —— 那是布局层的事，不是给分片器加个参数就能有的。这里的 `columns` 是
  **视口**参数（界面或读者决定一页几栏），栏宽由 `column_width(总宽, 栏数, 间距)` 算出来交给排版，
  两边由调用方对齐。
- **`recto` / `verso` / `avoid-column` / `avoid-page` 这些取值不认**，计入 `ignored`。
  把它们悄悄当成 `auto` 或 `avoid` 会让作者的意图静默消失，比不认识更难查。
- **分片只切行，不切块盒**：一片拿到的是 `Layout::lines` 的一个下标区间。块自己的背景、
  边框要按片裁剪的话得由绘制侧处理（现在两端的绘制都只画行）。
- **分片器还没有任何界面消费者**：两端都只画连续滚动，`ReaderView` 直接读 `Layout` 的行，
  `mode_name()` 固定返回 `"continuous"`。分页 / 双页 / 分栏的上屏接线（含把栏宽传给排版）
  属 M4 的最后一块。

## 格式（`crates/folio-core/src/format/`）

- EPUB 的图片按 manifest 里 `image/*` 的条目读出并入库（`images` 表），附带解码前的
  固有尺寸；`<img src>` 在**排版期**按本章目录解析成容器路径，不改写章节源文本。
  SVG 的固有尺寸取不到（见「排版」一节）。
- NCX 目录**拍平成一层**，`nav` 目录保留层级。阅读器用它跳章，层级只是好看。
- `@import` 引 external CSS 的书会少一份样式，计入 `ignored` 以便被发现。
- Markdown / TXT 仍然走 M1 的 `TextView` 占位渲染；等 E1 接上屏幕后，
  Markdown 要转成 XHTML 才能进新引擎（M5 一起做）。

## 配置与账号

- 阅读偏好与账号存在 `<系统配置目录>/folio/config.json`（`dirs::config_dir()`）。改了设置**即时落盘**
  （写临时文件再 rename）；文件缺失或坏掉都回落默认值，绝不让程序起不来。
- **坏一处的代价是一节，不是整份**：整份解析失败时按节重试（`config.rs::salvage`），
  `"font_size":"big"` 只归零 `reader` 一节，`appearance` / `library` / `account` 照旧读回。
  补救的粒度**止于节** —— 同一节里写对的键（如那节的 `line_height`）跟着一起退回本节默认。
  连 JSON 本身都不合法（截断、括号不配对）时没有可逐节尝试的载体，仍是整份回落。
  两条行为分别由 `folio_core::config::tests::one_broken_section_does_not_take_the_others_down` 与
  `unparseable_json_still_falls_back_as_a_whole` 钉住。
- **移动平台没有「系统配置目录」**：Android / iOS 上 `$HOME` 与 XDG 都不存在，可写路径只有宿主
  在启动时给的那一个应用专属目录，所以由入口调 `folio_core::paths::set_app_dirs` 报上来
  （Android 取 `internal_data_path()/files/folio/{config,data}`）。环境变量 `FOLIO_CONFIG_DIR` /
  `FOLIO_DATA_DIR` 仍然压在上面，调试时可以把整份状态搬走。只有第一次调用生效 ——
  路径一旦有文件写进去就不能再换，否则第二次启动读不到第一次的偏好。
- **设置是一个独立的开-or-聚焦窗口**，不是主窗口里的一个页面，也不是对话框：`Settings` 组件内部
  是一个 `h_resizable`（自己的页面导航 + 正文两栏），它和正文都要一个确定的盒子，而组件默认的
  对话框只有 448px，扣掉侧栏后正文不到 480px 的堆叠阈值，每个字段会竖着堆成一列没法看的窄条。
- **账号是本地占位**：只有一个可改的读者名，没有登录/同步。头像用名字首字画圆，不依赖图片资产。
- SQLite 里的 `prefs` 表已不再读写（迁移只增不改，故保留），是历史遗留。

## 进度与锚点（`crates/folio-core/src/anchor.rs`、`crates/folio-core/src/store.rs`）

- **坐标是「可见文本的第几个字符」，不是「读者数出来的第几个字」**。`TextMap` 按文档序
  拼所有文本节点，所以标签之间的换行与缩进也算（这本《蛇结》第一段正文的坐标是 1 而不是 0），
  `display:none` 块里的文字也在内。这个数只用于「回到原处」，界面上显示的是百分比。
  它必须与解析结果绑定：同一份 XHTML 换个缩进风格重排源码，坐标就平移，所以别用格式化工具改书。
- **同一个 `Locator.offset` 字段在两条渲染路径下是两套坐标**：自研引擎是可见文本字符，
  M1 留下的 `TextView` 路径（Markdown / TXT）是源文本字符。同一本书只会走一条，所以不会互相踩；
  但「把一本 .md 换成 .epub 再打开」会让位置漂。等 Markdown 接进引擎（M5）时这一条自动消失。
- **批注的读列表按行降级**：`anchor_json` 解不出来的那一行**跳过**，其余照常返回。
  这一列可能来自旧版本或别的机器，为一块坏 JSON 让读者看不到他所有的批注是最糟的失败方式。
- **`annotations` 表的 `chapter_id` 只是外键挂钩**，权威是锚点里的章下标；章节行还没入库时
  那一列留 NULL。`set_annotation_stale` 有意不碰 `updated_at`（stale 是排版结果，不是编辑记录）。
- **桌面端列表的项高不含块间空隙**（实测：三段各一行时，最后一行的行底在 132 之上，
  而逐项高度之和只有 89.25）。两个后果：① 进度换算必须在**列表空间**里做
  （`engine::item_top` / `line_at_item_top`），拿排版的绝对 y 会每段差一档 margin；
  ② 段与段之间在桌面上少一档空白 —— 这是 M2 就存在的绘制问题，不是这一轮引入的，
  修它要连 `line_element` 的 `top` 一起改，`shared_layout.rs::the_engine_list_counts_line_boxes_not_the_gaps_between_them`
  是它的门（改对了那条会红，红才是提示你去改另一边）。
- **还没接的**：选择交互（`TextSelectionHandle` ↔ run 表）、选中浮动菜单、批注侧栏。
  锚点与存储这两层已经能用，缺的是把它们接到屏幕上。

## 平台

- **CJK 禁则（kinsoku）未量化**。gpui 的 `LineWrapper` 走 unicode-linebreak，
  中文标点是否越线要在 M2 用定向语料测（开发方案 R3、任务 #10）。
- **justify 还没验**。`ShapedLine::paint(align, align_width)` 是唯一可用路径；
  spike① 实测 `force_width` **不会**压缩行宽，所以它不是对齐钩子（见 `docs/spikes.md`）。
- Android 侧**已经跑通到屏幕上**（2026-10-08，Meizu 20）：正文渲染、平台滚动、底部导航栏都在真机验证过。
  构建配方与依赖路线见 `docs/android.md`，缺口列在下面。
- **2026-10-09 这一轮的移动端改动没有在真机上验过**：换章按钮、`cx.theme()` 取色、
  「跟随系统」读窗口级外观、插图上屏、进度落库，五项都只过了 `cargo check --target
  aarch64-linux-android` 与宿主侧的排版测试。交叉编译能过 ≠ 那些行为是对的，
  下一次连真机时按这五条逐个看。

## 移动端（`crates/folio-app/src/ui/mobile/`）

一期移动端缺过的东西里，**这一轮已经补上的**记在这儿，免得下次又当成缺口去查：

- **章节不再写死第一章**。顶部那条给「上一章 / 下一章」两个按钮，到头的那一侧置灰，
  而不是让人点一下什么也没发生。
- **图片会上屏了**。排版走 `ui/shared/document`，图片索引与桌面共用同一份 `ChapterImages`，
  绘制用 gpui 自己的 `img` 元素（纹理上传归它），不再只留一个空位。
- **主题、偏好、文案都接进来了**。`ui/shared/theme.rs` 两端共用：`config.json` 里的 `Appearance`
  决定浅深与主题名，颜色一律从 `cx.theme()` 取，界面上不再有写死的纸墨值。
- **进度落盘了**。`Store::open_at(paths::db_path())` + `add_book` / `save_flow` / `set_position`，
  杀掉进程回来还在原来那一章。
- **移动端的排版有测试了**。移动 UI 本身只在 `cfg(mobile)` 下编译、宿主碰不到，但断行、层叠、
  版心宽度、缓存失效这些并进了 `ui/shared`，`ui/shared/document.rs` 的 tests 因此同时是那一侧的排版测试。

还没做的：

- **章内偏移已经落到字级**（2026-10-09）。两端存的都是「本章可见文本的第几个字符」，
  换字号、换行距、缩窗口都不动它。移动端的落库按行去重：偏移只在跨过一行时才变，所以
  「变了才写」等于每行至多写一次，不需要定时器。
- **没有书架、没有导入**。文档仍然靠 `adb push` 进应用目录，冷启动按固定文件名
  （`folio-sample.md` / `.txt` / `.epub`）依次试。`/sdcard/Download/` 那一档在 scoped storage 下
  读到 EACCES，所以只能排最后。登记进书库是为了有个能回写进度的键，不是为了列一本书。
- **没有设置界面**。字号、行距、主题、语言在手机上改不了，只能由桌面端改同一份 `config.json`，
  或者手动编辑那个文件。
- **图标只在 Android 上有**。`IconName` 画的是 `svg().path("icons/x.svg")`，字节要由应用的
  `AssetSource` 给，而那个 source 只能在 `Application` 构造期注册：Android 入口链了
  `.with_assets(gpui_kit::assets::AllAssets)`（代价是整套 Lucide —— 1830 个 SVG、源码 0.71 MB
  嵌进 `.so`），而 iOS / HarmonyOS 的 `Application` 在 gpui-mobile 内部构造、没有注册口，
  那两端的默认 source 对任何路径都给 `Ok(None)` —— **静默空白，不报错**。
  所以移动端的每个控件都保留文字、图标只加在文字前面：只用图标的按钮在没有 source 的那两端
  会整颗隐形，「点不到的导航」比「少一个装饰」严重得多。
- **底部导航栏不消费安全区**。fork 在窗口打开时报 `safe_area_insets top=116 bottom=70`（物理像素，
  scale 2.8 ⇒ 底部 25 逻辑像素），栏的底边落进系统手势区。标签是垂直居中的，视觉上没被压住，
  但点击热区下沿确实延伸进手势条。`safe_area_insets_logical()` 挂在 `AndroidWindow` 上，
  视图手上的 `&mut Window` 拿不到它，要接得先开一条平台层的路。
  （这一条数值现在只反映系统栏/手势/挖孔，弹键盘不再把它顶大 —— fork `96afdf2b` 起把输入法
  inset 单独走 `ime_insets_logical()`，见 `docs/android.md`。）
- **排版在主线程**。`Document::ensure_layout` 在渲染帧里做（宽度与偏好没变才跳过），
  桌面端 M4 的后台排版在移动端同样欠着。
- **依赖仍带 default features**：`gpui-pre-mobile` 的 30 多个平台插件 feature 全开
  （开发方案 §2.2b 要求的 `default-features = false` 还没落实）。
- **移动 UI 的交互仍只有真机算验证**。换章按钮的点击、主题在 Android 上解析到浅还是深、
  插图是否真画出来 —— 这几条宿主测试覆盖不到，`ui/shared/document.rs` 的 tests 顶部写清了边界在哪。
