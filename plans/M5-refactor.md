# M5 — 架构重构 + 绘制简化

状态：**已完成（2026-10-10）**。`cargo test --workspace --lib` 323 条全绿，
clippy `-D warnings` 干净，Android 交叉编译通过。

**实现时的两处偏差**（有意为之）：

- 模块化没有引入独立的 `session` Entity：KOReader 的模块是「拿到 `ui` 指针的功能对象」，
  Rust 里对应的形状就是**同一个类型在子文件里的 `impl` 块** —— 状态只有一份，`&mut self`
  即命令入口。gpui 拆成多个 Entity 会给每次命令加一层 `update` 间接与通知转发，
  单窗口阅读器得不偿失。最终结构：`session.rs`（状态+装配）+ `session/{navigation,position,toc,selection,view}.rs`。
- 「下划线绘制的 quad 计数断言」做不了：这份 gpui 快照的 `TestWindow` 不暴露绘制场景，
  headless renderer 需要 `render_to_image` 配置。下划线修复改由**结构性保证**
  （委托 `ShapedLine::paint`，装饰线由 gpui 内部画），观感待真机冒烟确认。

## M5.1 引擎查询接口（folio-core/src/hit.rs）

对照 KOReader 的引擎契约（`getWordFromPosition` / `getScreenBoxesFromPositions`），
全部基于现有 `Layout` + `Measure` trait 实现，不引新依赖：

- `hit_test(layout, dom, map, measure, x, y) -> Option<usize>`
  `line_at_y`（anchor.rs 已有）→ 行内逐 run 累加 x → `measure.char_width` 定字内偏移 →
  TextMap 字偏移。行内图（U+FFFC）按一个字符位处理。
- `rects_for_range(layout, dom, map, measure, Range<usize>) -> Vec<(x, y, w, h)>`
  按行求交、行内按 run 切片；选中跨行时每行一条矩形，天然正确。
- `snap_word(map, offset, direction) -> usize`
  词边界吸附。CJK 单字自为界；拉丁词字符合集照抄 gpui-kit `text/selection.rs` 的
  `is_word_char`（拉丁扩展 / 西里尔 / 越南文）。

`folio-app` 的 `Document` 加三个薄包装（`hit_test` / `selection_rects` / `snap_word`）。

同源性防线：hit-test 与绘制必须走同一个 `Measure` 实例（GpuiMeasure），
与现有「按 A 算宽、按 B 画字」防线同一套路。

## M5.2 绘制层简化（reader/paint.rs）

- `GlyphsElement` 的全套 `Element` impl（≈110 行）换成 `gpui::canvas(prepaint, paint)`，
  闭包按帧捕获 runs/fills/background，布局样式沿用 `relative(1.)` 铺满。
- `paint_run` 的逐字形循环（照抄内部 paint_line 的那 35 行）替换为公开 API
  `ShapedLine::paint(origin, line_height, align, None, window, cx)`。
- 基线 → 行框原点换算：origin.y = baseline − (line_height + ascent − descent)/2；
  line_height 取自然行高（ascent+descent）时即 baseline − ascent。
  ascent/descent 来自 `Measure::run_metrics`。
- **顺带修 bug**：现代码构造了 `UnderlineStyle`/`StrikethroughStyle` 却从未画装饰线；
  `ShapedLine::paint` 走 decoration_runs，会画。用测试钉住。

## M5.3 ReaderView 模块化拆分

`view.rs`（2400+ 行）拆为：

| 新文件 | 职责 | KOReader 对应 |
|---|---|---|
| `session.rs` | ReaderSession：模块组装、命令入口 | ReaderUI |
| `modules/navigation.rs` | 滚动/翻页/跨章/接章、进度条 seek | readerpaging + readerrolling |
| `modules/position.rs` | locator 落库/恢复、anchor、按行去重 | docsettings |
| `modules/toc.rs` | 目录摊平/折叠/当前行 | readertoc |
| `view.rs`（瘦身） | 纯渲染 + 输入翻译 | readerview |

`ui/shared/document.rs` 挪入 `ui/reader/document.rs`（只属于阅读器）。

## M5.4 文档重组

旧 `docs/` 全部删除；新 `plans/` 按里程碑拆文件（见 README.md 索引）。

## 验收

- `cargo test` 全绿（folio-core + folio-app，含 11 条既有 UI 测试原样通过）。
- 新增：下划线绘制断言；hit.rs 的几何单元测试。
- 真机冒烟：滚动 / 翻页 / 换章 / 换字号与重构前观感一致。
