# 已知限制

这份文件是**承诺的边界**，不是待办清单。引擎遇到清单外的写法时的行为统一是
「忽略该声明、保留内容」——一本书因为作者用了 `float` 就排不出版，
是最不能接受的失败方式。

对应实现位置都在括号里。

## CSS（`crates/folio-core/src/css/`）

| 写法 | 现在的行为 | 为什么 |
| --- | --- | --- |
| `:nth-child()`、`:hover`、伪元素、属性选择器的 `~=^$|= ~` 变体 | 整条**选择器作废**，规则体一起丢，计入 `ignored` | 部分生效会让作者样式以不可预期的方式命中内容，宁可整条不要 |
| `width` / `max-width` / `height` / `border` / `background` / `text-decoration` / `text-shadow` | 接受、不生效、**不计入** `ignored` | 出版书里满地都是；计入会把真正需要关注的「没预料到的写法」淹掉（`KNOWN_UNIMPLEMENTED`） |
| `font: 1em serif` 简写 | 同上，接受不生效 | 一期只认长写法 |
| `margin` / `padding` 多值（`0 auto`、`1em 2em`） | 判为不支持，值保持默认 | 四值简写的展开规则还没接，`auto` 更是需要包含块宽度 |
| `margin-top: 10%`、`text-indent: 5%` | 判为不支持，**不猜** | 百分比按包含块宽度折算，那是排版期才知道的数，层叠期算不出来 |
| `line-height: 1.5px` 之类带单位行高 | 折算成倍数（除以**自己的**字号） | 与 CSS 一致；代价是行高倍数被冻在计算结果上，改字号时不会重算 |
| `@media` / `@supports` | 无条件收进内容，不做条件判断 | 阅读器没有「屏幕宽度切换样式」这种语义 |
| `@font-face` / `@page` / `@keyframes` | 丢弃，计入 `ignored` | `@font-face` 真要接是 `add_fonts` + 字体清单的事，属 M6 |
| `position` / `flex` / `grid` / `transform` / `transition` | 完全不在清单内 | 自研排版引擎不承担版式级布局，见开发方案 §2.1 |
| 外部字体、`rem` 的根字号跟随 `html { font-size }` | `rem` 一律按**读者偏好字号**折算 | 让读者的字号说了算，比让书自己改全局字号更符合阅读器的立场 |

## 排版（`crates/folio-core/src/layout/`）

- **一个段落内只有一种度量**。`<strong>` 的粗体、`<small>` 的小字在 E1 层被拉平成
  块元素自己的样式；多 run 度量是 M3 的 E2（开发方案 §4.5）。
- **不做父子外边距折叠**。嵌套 `<div><h1>` 时子元素的 top margin 会计入父高度，
  结果是比浏览器多一档空白。兄弟之间的折叠是做了的——那才是段间距看着对不对的关键。
  选择代价是「多一档」而不是「重叠」，因为重叠会把文字压在一起读不了。
- **负外边距按 0**。书里用负边距做视觉效果的不多，但它能让行盒重叠，代价不成比例。
- **行内图片还没有片段**（M3）。块级 `<img>` 目前不显示，这是 EPUB 里最明显的缺口。
- **ruby / 竖排 / 多栏 / 绕图** 都还没接，分别是 M6 与 M4。
- `text-indent` 最多允许吃掉半个版心，`width` 再窄也不会让首行变成零宽。
- 空白折叠跨节点是一次性的（`Collector::pending_space`），但**嵌套元素自己的
  `white-space` 不单独判断**，整段按外层块的设置走。`<p>` 里藏一个 `<pre>` 会按 `p` 折叠。

## 格式（`crates/folio-core/src/format/`）

- EPUB 的 `<img>` src **还不改写**、图片**还不入库**（M3）。封面已能读出并落库。
- NCX 目录**拍平成一层**，`nav` 目录保留层级。阅读器用它跳章，层级只是好看。
- `@import` 引 external CSS 的书会少一份样式，计入 `ignored` 以便被发现。
- Markdown / TXT 仍然走 M1 的 `TextView` 占位渲染；等 E1 接上屏幕后，
  Markdown 要转成 XHTML 才能进新引擎（M5 一起做）。

## 平台

- **CJK 禁则（kinsoku）未量化**。gpui 的 `LineWrapper` 走 unicode-linebreak，
  中文标点是否越线要在 M2 用定向语料测（开发方案 R3、任务 #10）。
- **justify 还没验**。`ShapedLine::paint(align, align_width)` 是唯一可用路径；
  spike① 实测 `force_width` **不会**压缩行宽，所以它不是对齐钩子（见 `docs/spikes.md`）。
- Android 侧只有架构留门，工具链未装（任务 #11）。
