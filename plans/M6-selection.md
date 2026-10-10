# M6 — 选中一期（session-only）

状态：**已完成（2026-10-10，宿主 + 真机）**。长按/拖动/复制/就地菜单条/翻页清除全部落地。
真机验证（Meizu 20，2026-10-10）：gpui-mobile 的 `on_touch` 直发 `PlatformInput::Touch`，
gpui 的 `TouchGestureRecognizer` 在 500ms 后 `offer_long_press` —— 长按吸字、两端把手、
菜单条（复制 / 全选本段 / 取消）在屏幕上均成立，R1 风险排除，不需要计时器兜底。

**PC（指针）选中（同日追加）**：桌面不再只能靠长按 —— `MouseSelect` 在按下时只记锚点，
**拖过 4px 阈值才成立选中**（否则一下普通点击会留下高亮）。拖动松手的那一下
`ClickEvent` 被 `suppress_tap` 吃掉（否则 `page::on_body_tap` 会把刚选的清掉）；浮层开着时
指针拖选让位（那一击的语义是收浮层）。**不做双击吸词**：gpui 的双击会先送一个
`click_count=1` 的 click，那一下已经翻页 / 开浮层了；要不冲突就得把单击延迟到双击窗口
结束，代价是每次翻页都慢一拍。落点在 `session/selection.rs` 的
`begin/drag/end_mouse_selection` + `register_handlers` 里那三层指针监听；
宿主用例包括一条真实指针事件（down → 8 步 move → up）的端到端断言。
前置：M5.1（hit.rs）与 M5.3（模块化拆分）已完成。

**把手调整（同日追加，gpui-kit `touch_selection` 的移植）**：松手后两端各留一个把手
（杆 + 圆钮，44px 命中区），按住拖动单独改那一端 —— 长按之后**唯一**能改选中的路
（二次长按是重选）。规矩照搬 gpui-kit：拖动期间记「手指→插入符」偏移（圆钮长在行的
上/下侧，手指不压在文字上）；拖到另一端正上停住不塌缩；拖过另一端两端互换、手指改握
新的那头；拖动期间菜单条让位、松手即回；按在把手上不算「点别处清除」也不触发重选
（`covers_handle`，含上一帧命中区残影）。落点：`session/selection.rs`
（`EdgeDrag` + `begin/update/end_edge_drag` + `prepare_handle_hitboxes` /
`register_handle_handlers`），触摸走 `TouchDragEvent`、指针走 `MouseDown/Move/Up`
两套同构监听。

KOReader onHold 流程的映射，临时态与持久态分离（持久态归 M7）。

## 交互流

1. **长按**：body 层接 `PlatformInput::LongPress`（gpui 平台事件已存在；
   `gpui-kit/examples/touch_selection` 为交互参考）。
   第一步是真机 spike：验证 gpui-mobile 是否透传该事件；不透传则在本层用
   计时器手势兜底（touch-down 后 N ms 无移动即视为长按）。
2. **选词**：`document.hit_test(pos)` → `snap_word` 两侧吸附 →
   `Selection { start, end }` 存入 `modules/selection.rs`。
3. **拖动扩展**：pan 事件持续更新活动端（反向拖支持，start/end 互换），跟手重画。
4. **绘制**：`selection_rects(range)` → `FillRect`（fills 层在字之下，机制现成）；
   颜色取主题 accent 半透明。
5. **浮出菜单**：复制 / 全选本段 / 取消。**Android 浮层点不住是钉死事实**，
   沿用阅读页底栏的就地展开模式（压在正文上方的一条小条），不做 Popover。
6. **取文本**：`TextMap::slice(range)` → `cx.write_to_clipboard`。
7. **清除**：点正文其他处、翻页、换章、切模式时清空。

## 边界

- 选中范围限本章；跨章拖动夹到章界。
- 选中态下滚轮/翻页照常工作，翻走即清。
- 连续滚动拖到视口边缘：暂不做自动滚动，列入 M7 前的打磨。

## 验收（宿主测试，合成 EPUB）

- 长按命中单字（CJK）；拉丁词吸附到词边界。
- 拖动扩展跨行后 `rects_for_range` 返回多行矩形，首尾矩形与首末字对齐。
- 反向拖不崩，start/end 正确互换。
- 复制内容与可视文本一致（`TextMap::slice`）。
- 翻页 / 换章 / 点空白清除选中。
