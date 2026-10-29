# Folio 没接住的 CSS

书里写了、而我们的引擎没兑现的 CSS，一条一份理由。这份文档只做两件事：**列清单**与**报实测数**。

真相源是代码里的两处，不是这份文档：

- `crates/folio-core/src/content/format/css/computed/apply.rs` 里那条属性分发的 match —— 收哪些
  `(属性, 值)` 组合，以它为准；
- `crates/folio-core/src/content/format/css/computed.rs` 里的 `KNOWN_UNIMPLEMENTED` —— 认得、
  一期不生效的那一份名单，理由写在它自己的注释上。

文档与代码冲突时改文档。要扩支持面，先改代码里那两处，再来改这张表。

## 三档状态

一条声明在我们引擎里只有三种下场。判据函数是 `css::computed::declaration_status`，它内部就是层叠
用的同一份 `apply`，所以统计与实际渲染行为不会分叉。

| 状态 | 含义 | 进不进落差表 |
|---|---|---|
| `Applied` | 值解出来了，写进 `ComputedStyle` | 不算落差 |
| `Inert` | 属性名认得，一期不生效 | 单独一档「认得，不生效」 |
| `Unparsed` | 属性名或值不认识 | 算「不认识」，计入 `StyledDom::ignored` |

### `Applied` 不等于排版层读了

`Applied` 只说明「层叠期把这个值存进了 `ComputedStyle`」，**不**说明排版层或绘制层读过它。
2026-10-29 这一轮在值这一层抓到两条假接住，两条都已改判：

| 写法 | 从前 | 现在 |
|---|---|---|
| `break-before: avoid`、`break-after: avoid`（含 `page-break-*` 同名前缀版） | 存成 `BreakWhen::Avoid`，报 `Applied`；而 `BreakWhen::is_forced` 明写它不是强制断，`layout::fragment` 除了 `is_forced` 与 `parity` 没有第三个消费者 ⇒ 静默 no-op | 判 `Unparsed`，`BreakWhen` 里那个变体删掉 |
| `display: inline-block` | 存成 `Display::InlineBlock`，报 `Applied`；而 `layout::collect` 只判 `None` / `Inline` / `ListItem`（`is_inline_flow` 与 `is_item` 两个判据），屏上与 `display: block` 一字不差 | 判 `Unparsed`，`Display` 里那个变体删掉 |

别和 **`break-inside: avoid`** 搞混：那一条落在 `ComputedStyle::break_inside_avoid` 这个布尔上，
分片器真读，是真生效的。

以后加一条值的判据：**在 `crates/folio-core/src/layout/` 里搜得到那个枚举变体被读**，才算接住；
搜不到就判 `Unparsed`。把「没做」记成「做了」比报成不认识更坏 —— 落差表会说谎，而屏幕上看不出
任何异常。这两条各有守卫测试钉住（`inline_block_is_not_accepted_as_a_display_value`、
`avoid_is_not_a_break_before_or_after_value_but_break_inside_avoid_is`）。

## 本机语料的实测落差

四本：哈利·波特全集（多看系转换本，301 章）、《斗战狂潮》校对版全本（转换本，1225 章）、
《蛇结》（原版排版，28 章），加一本合成的中文测试书。

| 书 | 声明 | 元素 | 有落差 | 接住 / 认得不生效 / 不认识 | 拼不出规则的块 |
|---|---|---|---|---|---|
| 哈利·波特全集 | 294,607 | 51,590 | **481（0.9%）** | 294,126 / 0 / 481 | 3 处 |
| 《斗战狂潮》 | 708,826 | 80,199 | **1（0.0%）** | 708,825 / 1 / 0 | 0 处 |
| 《蛇结》 | 4,963 | 2,440 | **0（0.0%）** | 4,963 / 0 / 0 | 1 处 |
| 合成测试书 | 31 | 28 | 0 | 31 / 0 / 0 | 1 处 |

哈利·波特剩下的 481 个元素正好是下面那两条按数否决的写法：
`vertical-align: text-top` 241 + `padding: … auto` 240。

### 2026-10-29 接住的十四条与各自的落地程度

数字的变化来自这十四条，装进引擎的是它们的**部分**语义，剩下的边界都写在这里：

| 写法 | 落地程度 |
|---|---|
| `text-shadow` | 偏移、颜色、颜色缺省取 `currentColor`、多层逗号列表都真画。**模糊半径不是高斯**：gpui 只有纯色四边形与字形两条绘制原语，按半径摊成「中心 + 四份对角」的副本近似，副本封顶 5 份、半径封顶 3px。真书里这个值普遍是 1px，《哈利·波特》13 条规则全是 1，屏上看不出与高斯的差别。`transparent` 影子暂不收（`Color` 没有 alpha 通道），整条判不认识 |
| `border-radius` | 简写 1–4 值与百分比都收，四个角按盒尺寸折成 px（百分比按**盒宽**、每角封顶 `min(宽,高)/2`）。**只有底色那一块四边形带圆角**：边框仍画四条轴对齐的直条，所以圆角盒配粗边框时四角是直的。四个单角长手（`border-top-left-radius` 等）与椭圆斜杠写法（`10px / 20px`）不收，判不认识 |
| `duokan-text-indent` | 多看转换器的私有前缀，收成与标准 `text-indent` 同一个数。它与同一条规则里的标准声明是副本（21 个规则块全部成对、20 块同值），所以屏上不变 —— 变的是这张表不再把 5,149 个元素报成落差 |
| `border-style: dotted` / `dashed` | 带子沿长边切段画：`dotted` 方点边长 = 线宽、隙 = 线宽；`dashed` 段 = 3×线宽、隙 = 3×线宽（CSS 不规定比例，取浏览器那一套）。从一端铺到另一端、**尾巴按剩余长度截短**，不分平。点是方的：绘制原语只有轴对齐四边形，1px 的点分不出圆方。`groove` / `ridge` / `inset` / `outset` 仍判不认识 —— 那四档要的是斜角明暗，纯色带画出来一定是错的 |
| `border-collapse: collapse` | 相邻两格的公共边只画**一条**：逐边与邻格的对面那一侧比一次，赢的留、输的抹。优先级按 CSS 2.1 §17.6.2 取前三条 —— 宽 > 画法（`double` > `solid` > `dashed` > `dotted` > `none`）> 位置（左右相邻取左格、上下相邻取上格）。`separate` 是默认。**没做的**：几何上相邻两格没有互相让出半个边宽（那条线因此落在赢的那格边内侧，半个像素的偏移看不出来）；表格自己的边框与最外圈格子的边框不合并（CSS 里会）；`border-style: hidden` 折进 `none`，于是「`hidden` 压掉整条线」给不出来 |
| 表格布局（`display: table` 一族） | UA 表给 `<table>` / `<tr>` / `<td>` / `<th>` / `<thead>` / `<tbody>` / `<tfoot>` 四档 `display`，排版走 `layout::table`：**auto** 布局，每列按「最长不可断段的宽(min) / 整行不折的宽(max)」两条固有宽分配，行高取这一行最高的格子，格内顶对齐、盒子拉到行高（边框与底色因此铺满一行）。`width: auto` 的表**收缩到内容宽**（上限是版心），`margin: auto` 在收缩之后重算中缝。**不做的**：`colspan` / `rowspan`（跨格按 1 格算）、`border-collapse: collapse`（边框按 separate 画，相邻两格各画一条）、`table-layout: fixed`、`display: inline-table`、格内只做顶对齐、`<caption>` 不进网格 |
| `position: absolute` / `fixed` + `left` / `right` / `top` / `bottom` | 脱流，按四个偏移相对**这一章的盒**摆（`%` 的基准：左右按宽、上下按高），`margin` 加在偏移上 —— 封面那套 `left:50%; margin-left:-9.5em; width:19em` 的居中写法靠的就是它。**只认章级包含块**：CSS 的「最近的定位祖先」没做；没写 `width` 时铺满包含块（没有 shrink-to-fit）；绝对定位不撑高这一章（与 CSS 一致，分页因此不被封面装饰改写）。`relative` / `sticky` 判不认识 |
| `currentColor` 在背景色与 `color` 上 | `background-color: currentcolor` 折成本元素自己的 `color`（没写就取继承来的），`color: currentcolor` 就是继承。`color` 在层叠里**单独一批**先落地，所以背景色那句写在 `color` 前面也折得对。边框色里的 `currentColor` 上一轮已经接了 |
| `word-break: break-all` | 断行机会放开到**任意两个字符之间**（不再只认 UAX #14 的词边界）：拉丁词填不满剩余宽度时也能断，于是同样版心少一行。表格里的格子跟着把「最小宽」从整词降到**一个字符**。`keep-all` 不收（它要的是中文也不许逐字断，与本引擎的断行模型相反） |
| `border-style` 的 3D 四档（`inset` / `outset` / `groove` / `ridge`） | 由作者的边框色派生亮/暗两档（`three_d_tone`：暗 = 明度 ×0.7、亮 = 朝 1 补 35%，纯黑纯白因此也能各推出一档）：`inset` 上/左暗下/右亮、`outset` 相反、`groove` 每条边外暗内亮、`ridge` 相反。角上仍是四条带子互相压边，与浏览器的 45° 拼角差在角上 |
| 表格格里的 `vertical-align: top` / `middle` / `bottom` | 格内内容在**行高**里贴哪一边：`top` 是初值也是以前的行为，`middle` 挪一半、`bottom` 挪到底；盒子始终拉满行高，所以边框与底色仍铺满一行。`vertical-align` 不继承，所以整行写 `middle` 不会漏到格子上 —— 浏览器同样如此（Calibre 的书要另写 `inherit` 才生效）。行内盒上的 `top`/`middle`/`bottom`（相对行盒）**还没做**，收下这个值但在行内不产生位移 |
| `border-spacing` | `separate` 模式下格与格之间的缝（横/纵各一个数），缝算进表盒、列宽在扣掉缝之后分配。塌陷模式里忽略（CSS 2.1 §17.6.1） |
| 「接受初值」一族：`font-variant: normal` / `font-variant-caps: normal` / `oeb-column-number: 1` / `background: initial` | 这几个值的语义**就是我们做的事**（不启用任何字体特性、不分栏、清空背景图层），所以收下；同属性的其它值判不认识。《乌合之众》157 个 `font-variant: normal`、The Riddle 那本书的 `background: initial` 都落在这里 |
| `list-style-image: none` / `list-style-position: outside` | 这两个**初值就是我们本来在做的事**（标记只有文字、挂在流动之外），所以只收初值：其余值（`url(…)`、`inside`）判不认识。《蛇结》51 个 `li` 写的正是这两个初值，从前被记成 102 条「认得不生效」；现在这本书的落差归零，屏上一个像素没动 |

两个口径要分清，下面的表都用这两列：

- **元素数** = `css-gap` 报的「层叠胜出且没接住的声明落在几个元素上」，用来排优先级。
  规则命中 0 个元素就不进 `css-gap` 的表。
- **声明点** = 解包 epub 后对 `.css` / `.xhtml` / `.html` 直接 grep 的书写处数，用来看作者写了多少遍。
  一条规则可以命中几百个元素，所以这两个数经常差两三个数量级。复跑：

```bash
cargo run -p folio-core --example css-gap                       # 仓库 fixture
cargo run -p folio-core --example css-gap -- <书1> <书2> …       # 也可以指别的书
# 声明点：unzip -o <书>.epub -d <dir>，再对 *.css *.xhtml *.html 数 `属性\s*:`
```

## 一、认得、不生效（`Inert`，名单里 13 条）

「为什么给不了」那一列照 `computed.rs` 里 `KNOWN_UNIMPLEMENTED` 自己的注释写；代码没附理由的地方
明写「未附理由」，不替它编一个。

| 属性 | 元素数 | 声明点 | 为什么给不了 |
|---|---|---|---|
| `background-attachment` | 0 | 0 | 流式排版里没有对应物：这里的「视口」是一栏纸，滚动时背景跟着纸走才是读者要的；真按 `fixed` 摆会让扉页底图钉在屏幕上、正文从它底下漂过去。`local` 要的是「按这一块的内容盒滚动」，而流式块没有自己的滚动区。两条都按初值 `scroll` 摆 |
| `background-clip` | 0 | 0 | 一律按**边框盒**摆，正是出版书「底色盖到边框下面」那一大片的写法；`padding-box` / `content-box` 要给的是内缩矩形，本书语料里一次没出现 |
| `background-origin` | 0 | 0 | 同 `background-clip`：基准一律取边框盒，内缩那一档语料里没有 |
| `columns` | 0 | 0 | 分片器认的是**视口**栏数（`FragmentOptions::columns`，由界面或读者给）。作者写的块级 `columns` 要改排版期的折行宽度，还没接 |
| `column-gap` | 0 | 0 | `columns` 的另一半，跟着一起欠着 |
| `text-transform` | 0 | 0 | 它会改变字符本身，而锚点与进度都建立在**源文本字节偏移**上（`Locator` 的坐标系）：`capitalize`、`ß → SS` 这类一改就让偏移对不上。要接得连锚点映射一起定，不是加一条 `to_uppercase()` 的事 |
| `font-stretch` | 0 | 0 | OpenType 层共用一条理由：要的是 gpui 的 `FontFeatures` 通道，那是接字体文件那一层的事，不是层叠期能折出来的数。（`font-variant` 与 `font-variant-caps` 2026-10-29 移了出去：初值 `normal` 就是我们做的事，改成收初值） |
| `font-kerning` | 0 | 0 | 与 `font-stretch` 同一条通道，同一个理由 |
| `font-feature-settings` | 0 | 0 | 与 `font-stretch` 同一条通道，同一个理由 |
| `cursor` | 5,718（《毛泽东选集》） | 1 | 界面提示，不是排版属性：它说「指针移到这儿该长什么样」。阅读器里能兑现的地方只有链接（桌面端本来就给手形），而书里把它写在几千个元素上只是转换器把网页样式原样带进来。记「认得、不生效」——落差表不再报成「听都没听过」，它也确实不该动版面 |
| `ruby-position` / `-epub-ruby-position` | 157（《乌合之众》） | 1 | **ruby 布局没做**：`<rt>` 现在按普通行内串在字后。真正要的是「注音排在字上方、不推进基线、行盒按注音长高」，那要动行内收集与行盒高度。而这本属**全书 20 个 epub 里 `<ruby>` / `<rt>` / `<rp>` 元素各 0 个** —— 没有一本书能验证，所以「`over` 这一档接住了」这句话现在说不得，记「认得、不生效」 |
| `-webkit-text-combine` | 3（《乌合之众》） | 1 | 竖排里把两个数字并成一格（`horizontal` 是不并）。竖排那一套还没细到这一层 |
| `z-index` | 0 | 0 | 绘制顺序是「相交即上抬」（`Scene::insert_primitive` 走 `BoundsTree`，见 `doc/facts.md`），不是 z 树；绝对定位盒的层序按文档序。`position` / `left` / `right` / `top` / `bottom` 2026-10-29 已移出这份名单（见下面「接住的十四条」） |

## 二、属性名认得、这个值故意不收（`Unparsed` 的一部分）

这一族最容易被误读成「整条属性没做」，所以单列：下面每一条都只收**一部分值**，同一条属性的别的值
是真生效的。

| 写法 | 元素数 | 声明点 | 处置 |
|---|---|---|---|
| `vertical-align: text-top`（同族还有 `top` / `bottom` / `middle` / `text-bottom`） | 241（哈利·波特） | 1（另有 1 处 `super`，那条是收的） | 只收长度、百分比与 `super` / `sub`。2026-10-20 用引擎自己的选择器匹配复测过：那 241 个元素全落在 `sup img` 上（`[p>sup>a>img]` 236 条，注号图），而 `layout::collect` 摆图片时逐级累加祖先的 `baseline_shift`，UA 给 `<sup>` 的上标位移早就作用在那张图上 —— 接了几乎不动屏。**这条被数否决过，别顺手加回来** |
| `padding: … auto` | 240（哈利·波特） | 1 | 判不认识是对的：CSS 里 `padding: auto` 本来就非法，拒掉才与浏览器逐字一致。只有 `margin` 那一族吃 `auto`（`solve_block_width` 做均分） |
| `text-decoration: overline`（与 `blink`） | 0 | 0 | 画不出来就整条判不认识，不许在统计里显示成「接住了」。线型那几个 token（`solid` / `double` / `dashed` / `wavy` / `thick` / `thin`）认得、画法不变（一律按当前文字色画 1px 实线），由 `the_decoration_line_alias_and_the_line_style_tokens` 钉住 |
| `white-space: nowrap` / `pre-line` | 0 | 0（`white-space` 整条属性在本机语料里 0 处） | `pre` 与 `pre-wrap` 共用一个「保留换行」布尔；`nowrap` / `pre-line` 一期不区分，按不支持计数而不是假装成功。**为 0 次出现把布尔换成五态枚举不值** |
| `list-style-type: hebrew` / `arabic-indic` / `armenian` / `georgian` / `devanagari` | 0 | 0 | EPUB 3 CSS Profile 点名收录 `hebrew`，但本机没有一本希伯来或阿拉伯书可验它那套数值记数（1–10 单字、20/100 另字、组合规则）。猜错是把整本目录的编号排错 —— 判「不认识」比画出一个看着像的号诚实 |
| `border` 线型还剩四档不收 | 0 | 0 | `BorderStyle` 有 `none`（`hidden` 折进来）/ `solid` / `double` / `dotted` / `dashed` 五类。`groove` / `ridge` / `inset` / `outset` 整条判不认识：那四档要的是斜角明暗关系，纯色带画出来一定是错的，不是悄悄当 `solid` |
| `list-style-image: url(…)`（非初值） | 0 | 0 | 标记只有文字那一条路，没有图源可画。初值 `none` 收（见上面「接住的十四条」），其余判不认识 |
| `list-style-position: inside`（非初值） | 0 | 0 | 要把标记塞进首行、让出首行的位置；现在是挂在流动之外。初值 `outside` 收，其余判不认识 |
| `border-radius` 的椭圆写法与单角长手 | 0 | 0 | 简写 1–4 值已收（见「刚接住的三条」）。`border-radius: 10px / 20px` 要给每个角存横纵两套半径，而字段只有一套；`border-top-left-radius` 这类四个长手会落进 `apply` 的 `border*` 前缀分支被判不认识 —— 半接不接比不接更难查 |
| `box-sizing: padding-box` | 0 | 0（`box-sizing` 本机语料 0 处） | 只收 `content-box` / `border-box`。`padding-box` 那一档要把边框留着只扣内边距，扣错的表现是「盒子宽了边框那几像素」，比一条明确的「不认识」难查 |
| `float: inline-start` / `inline-end` / `float-inherit` | 0 | 0（语料里 `float` 只有 `left` 4 处、`right` 1 处） | CSS 3 方向敏感那一族不接；出版书里真正出现的写法（插图、章首下沉）都写死 `left` / `right` |
| `writing-mode: tb-rl` / `tb-lr` / `lr` / `lr-tb` / `rl` | 0 | 0 | SVG 遗留那一族是「先转整页再转字」的另一套语义，猜错的症状比不猜难查。`sideways-rl` / `sideways-lr` 反而**收**，折进 `vertical-*`：这台渲染器没有字形旋转通道，竖排里的拉丁本来就显式降级成直立，降级后两档在屏上是同一个画面 |
| `text-align` 的厂商值（`-webkit-center` 之类） | 0 | 0（语料里只有 `center` 37 / `left` 17 / `right` 7 / `justify` 5） | 只收 `start` / `end` / `left` / `right` / `center` / `justify` 六个 |
| `display: inline-block` | 0 | 0 | 排版层没有那一档：`layout::collect` 的 `is_inline_flow` 与 `is_item` 只认 `None` / `Inline` / `ListItem`，所以存进去的 `InlineBlock` 没人读、屏上与 `display: block` 一字不差。行内块要的 shrink-to-fit 宽与「占一行内位置」都还没做 |
| `break-before: avoid` / `break-after: avoid` | 0 | 0 | 排版层兑现不了：`BreakWhen::is_forced` 明写 `avoid` 不是强制断，而分片器除了 `is_forced` 与 `parity` 没有第三个消费者。`break-inside: avoid` 走 `break_inside_avoid` 那个布尔，是真生效的另一条路 |

## 三、属性名从没进过清单

这些名字在 `apply` 的 match 与 `KNOWN_UNIMPLEMENTED` 里都不存在，所以一律 `Unparsed`。本机语料的
声明点**全部为 0** —— 列出来是为了下次有语料时能被看见，不是候选清单。

`overflow`、`opacity`、`transform`、`filter`、`box-shadow`、`outline`、`transition`、`animation`、
`unicode-bidi`、`table-layout`、`caption-side`、`object-fit`、`flex`、`grid`。

表边框那两条现在只剩 `border-spacing`（格与格之间的缝）：`border-collapse` 已接住
（见「接住的十四条」），而 `border-spacing: 2px` 要给相邻两列之间留 2px —— 本机语料 7 个元素。

另有两件结构性的欠账：

- **`@page` 只兑现页边距**：`margin` 四边读进阅读器的页边距默认值，`padding` 与其余声明不兑现
  （边界由 `page_margins_are_parsed_for_the_reader` 与 `a_later_page_rule_only_overrides_the_sides_it_writes` 钉住）。
- **收缩宽度只有定宽那一档**：`float` 的 shrink-to-fit 只认「作者写了 `width`」或「整块是一张图」，
  max-content 那一档没接 —— 所以文本浮动盒仍需作者给宽。

## 四、明确不做（已按数否决）

- **绝对定位整族**（`position: absolute|fixed`、四个偏移、`z-index`）：代价是重排整条流动，
  本机语料带非 static 定位的元素实测**只有 1 处**。
- **`vertical-align` 的五个关键字值**：241 个元素落在 `sup img` 上，UA 的上标位移已经作用在那张图，
  接了几乎不动屏。
- **`white-space` 的五态**：整条属性在本机四本语料里 0 处。
- **`hebrew` / `arabic-indic` / `armenian` / `georgian` / `devanagari` 记数**：0 处，且无样本可验。

## 五、全量语料的排名

`tests/fixtures` 里所有能带 CSS 的 epub 都跑了一遍（20 个文件；`phone/ev3b5ihfkyrsb3us.epub`
是哈利·波特、`phone/bajfbqrzqsu3tb72.epub` 是斗战狂潮的副本，去重后 18 本）。
**20 个文件里 11 个落差为 0**；最大的一个（《毛泽东选集》）剩下的全是 `cursor`，
而它已经判「认得、不生效」。

| 书 | 章 | 落差 | 现在剩下的是什么 |
|---|---|---|---|
| 《毛泽东选集》 | 416 | 5,718（23.5%），全在 Inert 档 | `cursor: pointer` —— 界面提示，不动版面 |
| 《基督山伯爵》 | 135 | 843（3.8%） | `letter-spacing: auto` —— **书写错了**：CSS 只有 `normal` 与长度，浏览器同样整条丢 |
| 哈利·波特 | 301 | 481（0.9%） | `vertical-align: text-top` 241（UA 的上标位移已经生效）+ `padding: … auto` 240（非法值） |
| 《乌合之众》 | 38 | 157（6.8%），全在 Inert 档 | `-epub-ruby-position: over` —— ruby 布局没做，而全书 `<ruby>` 元素 0 个 |
| 人类简史 / 未来简史 / 世界简史 / 西游记 / 蛇结 / 罗生门 / 别想太多啦 / 我胆小如鼠 / 上瘾式学习 / The Riddle / 合成书 | — | **0** | — |
| 其余（夏天烟火等读客本） | — | 1 | `font-weight: blod` 这类书写错误 |

剩下的都能一眼看出「该不该做」：

- **ruby**：全书 **0 个 `<ruby>` / `<rt>`**，那 157 条 `-epub-ruby-position` 是转换器带进来的
  死声明。真做 ruby 布局（注音排字上方、不推进基线、行盒按注音长高）没有一本书能验证，
  所以先记 Inert。要开这一刀得先自己做样本。
- **`all: initial`**（5）：要把**每一个**属性拉回初值（含继承来的），值得单独一刀；
  `background: initial` 那一档已经收了。
- **`100vh`**（1）：分页阅读器里没有对应物（我们的「视口」是一栏纸），判不认识是对的。
- **`cursor`**（5,718）：阅读器里能兑现它的地方只有链接，而正文里的链接**点不动**
  （没有 `href` 跳转），现在给手形光标等于广告一个不存在的动作。要做先把链接跳转做出来。

两件**不是我们缺口**的，别再当新发现报：《我胆小如鼠》那 12 处「拼不出规则的块」是它自己
少了一个分号（`text-align:centertext-indent:0`，浏览器同样整条丢）；同书 `font-weight: blod`、
`padding: auto` 也都是坏的写法。

## 六、语言层：还没进的构造（不是「属性」那一层）

`css-gap` 只数**属性声明**，下面这些根本进不了它那张表，单列在这里。括号里是 20 个
epub fixture 里量到的处数。

| 构造 | 现在的行为 | 语料 |
|---|---|---|
| `@media` / `@supports` | **`print` 独有的块整块丢掉**（纸质的页眉页脚与分页收进来就是错）；含 `screen` / `all`、以及判不了的条件（`(min-width: 800px)` 这类，版心归读者设）照收。`@supports` 判不了特性支持，一律照收 | `@media` 1 处（The Riddle 的 `@media screen`，照收） |
| `@import` | 丢掉（那条外链样式表不加载） | 2 处（哈利·波特那条指向的 `fonts.css` **同时也在 manifest 里**，所以没丢样式；只靠 `@import` 的书会丢） |
| `@charset` / `@namespace` | 丢掉，无害 | 6 / 1 |
| `::before` / `::after` | 不生成（`content` 属性也不认）。要「凭空造一个盒」，不是解析器的事 | 各 0 |
| `::first-line` | 不认（只有 `::first-letter` 做了首字下沉） | 0 |
| `var()` / 自定义属性 / `calc()` | 都不认 | 各 0 |
| 长度单位 `vh` / `vw` / `ch` / `q` | 不认（只收 px / pt / pc / in / cm / mm / em / rem / ex / %）。`ex` 已按 `0.5em` 近似收下 | `vh` 1，其余 0 |
| ~~`all: initial`~~ | **已做**：把每一个属性拉回初值（含继承来的），`link` 记号留着 | 5（The Riddle，现在是 0 落差） |
| ~~`<table>` 的 `colspan` / `rowspan`~~ | **已做**：跨格只排一次、宽取所跨列之和、行高缺口均摊 | 7（人类简史 5、HP 1） |
| `table-layout: fixed` | 判不认识（只做 auto） | 1（人类简史） |
| `<caption>` | 不进网格、也不单独排 | — |
| `@page` 里 `margin` 以外的声明 | 不兑现（只读四边页边距） | — |
| `float` 的 max-content 收缩宽度 | 没接（文本浮动盒仍需作者给宽） | — |

## 加一条支持时的顺序

1. **先量语料**：`css-gap` 的元素排名排优先级，`grep` 数声明点看作者写了几遍。量到 0 就把「0 处」
   写进文档再决定做不做，别靠猜加属性。
2. 在 `apply` 的 match 里收值；整条属性认得但一期给不了效果，加进 `KNOWN_UNIMPLEMENTED` 而不是让
   `apply` 返回 `true`（返回 `true` 就是报「接住了」）。
3. **验一遍「谁读这个值」**：在 `crates/folio-core/src/layout/` 与 `folio-app` 的绘制路径里搜那个
   枚举变体或那个字段。搜不到，就说明这条还只存在 `ComputedStyle` 里。
4. 测试两条都要：`css/computed/tests.rs` 里钉计算值与 `ignored` 计数（值恰好等于默认时，只有
   `ignored` 计数能让断言不空过），`layout/tests.rs` 或 `layout/fragment/tests.rs` 里钉排版几何。
5. 改完重跑 `css-gap` 对照四本书的落差数，并说明动了哪一格。
