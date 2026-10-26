# M7 — 批注二期

状态：**后端已完成**，界面未接。前置：M6 选中一期完成。

已落地的后端面（`folio-core`）：`store` 的批注全套 CRUD（v1 建表）、
`anchor::relocate` 四级降级、`annotation::resolve_annotations`（按章批量重定位，含
stale 判定）、`annotation::annotations_by_chapter`（按章分组）、
`annotation::annotations_markdown`（Markdown 导出）。剩下的都是界面：回显、浮条、侧栏。

## 数据流

**保存**：选中确认 → `TextAnchor::new(map, chapter, start, end)` →
`store.add_annotation(book_id, ...)`（annotations 表与全套 CRUD 已在
`folio-core/src/store.rs`，v1 建表，尚无任何调用方）。

**回显**：进章排版完成后 → `store.annotations(book_id)` 过滤本章 →
`relocate(anchor, map)`（四级降级：原偏移 → 同 DOM 节点 → ±400 字窗口 → 全章；
Confidence 不足以命中时按 stale 显式标记，不静默错位）→
`rects_for_range` → fills 层画底色。

## 接口落点

- `modules/annotations.rs` 定义 `trait AnnotationStore`，实现包 `Store`；
  selection 模块的确认入口留 `confirm_selection()` 钩子，M7 接入。
- 批注颜色沿用 KOReader 四色（lighten / underscore / strikeout / invert 中先做前三种的
  底色版），存 `color` 字符串。
- 笔记（note）：`update_annotation` 已支持 note 字段，入口挂在高亮浮条上。

## 内部里程碑（启动时展开为独立文件）

1. 保存与回显闭环（含 stale 降级显示）
2. 批注侧栏（列表 / 跳转 / 删除）—— 就地面板，不用浮层
3. Markdown 导出
4. 视口边缘拖动自动滚动、跨章选中（与 M6 边界项一起收）
