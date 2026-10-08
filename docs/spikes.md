# M0 Spike 结论

> 日期：2026-10-08 · 环境：Arch / niri（Wayland）/ 缩放 100% / rustc 1.99 / gpui-kit 0.7.1（gpui-pre 0.3.8）
> 复现：`cargo run --example spike`（`crates/folio-app/examples/spike.rs`）
> 视觉证据：`docs/assets-spike-m0.png`（本机文件，未入库 —— 那是整张桌面抓取，含其他窗口内容）

## 汇总

| # | 问题 | 结论 |
|---|---|---|
| ① | 排版能否离开 Window 在后台线程跑 | **能**（编译与运行双证） |
| ② | 自绘文本（横排混合字重 + 竖排轴向对调）能否成立 | **能**（截图为证） |
| ③ | 自带中文字体能否加载并生效 | **能**（主题注册未测，另做） |
| ④ | `line_wrapper` 的 CJK 断行质量 / 禁则 | **未定论**，需定向语料重测 |
| — | 窗口在 niri/Wayland 下能否正常出现 | **能**，但 `app_id` 泄漏为 `dev.zed.Zed`，需修 |

## spike① 离窗排版 —— 通过（R4 前提解除）

API 归属是这次最大的发现，之前所有估计都建立在误解上：

- **`TextSystem`（App 级）**：`add_fonts` / `all_font_names` / `resolve_font` / **`line_wrapper`** / `advance` / `layout_width` / `typographic_bounds` / `ascent` / `descent` / `bounding_box` / `em_width` / `ch_width` / `x_height` / `cap_height` / `baseline_offset`
- **`WindowTextSystem`（窗口级）**：**`shape_line` / `shape_text` / `layout_line`** —— shaping 全在这里

于是「能否后台排版」收敛为一个问题：`Arc<WindowTextSystem>` 可否 move 进后台线程。答案是可以——`window.text_system().clone()` 得到的 `Arc<WindowTextSystem>` 是 `Send + Sync`，在 worker 线程调用 `shape_line` 正常返回：

```
=== spike① 离窗排版（后台线程 ThreadId(20)，name=Some("Worker-18)）===
WindowTextSystem::shape_line OK：width=504.764px ascent=15.775999px descent=4.352px runs=1 字形数=46
TextSystem::advance(排) -> Ok(Size { 17px × 17px })
```

数值合理性交叉验证：17px 字号下 46 个字形宽 504.76px；`advance('排')` = 17×17px，正是 CJK 全角 em 方块；`ascent+descent ≈ 20.13px` 符合 17px 字体的常规行高。

**对架构的影响**：`folio-core` 的 `Measure` trait 必须**分两级**——App 级（度量/断行，天然可后台）与 Window 级（shaping，靠捕获 `Arc<WindowTextSystem>`）。整条 DOM→Style→Layout 链可以放进 `cx.background_spawn`，R4「缩放窗口时布局卡主线程」的风险解除。
**新增待办**：窗口跨屏移动会改变 scale factor，需确认 `WindowTextSystem` 是否随之失效（M2 验）。

### ⚠️ 一条被推翻的假设

`shape_line(.., force_width: Some(px(400.0)))` **没有把行宽压下来**：

```
force_width 两端对齐钩子：自然宽 578px → 强制 400 后 width=578px
```

所以 `force_width` 不是「压缩对齐」的开关（更可能只做拉伸分配，或仅影响 glyph 定位而不改 `width` 字段）。**justify 必须走 `ShapedLine::paint(origin, line_height, align, align_width)`**，其正确性留到 M2 用定向语料验证。开发方案 §3.2b 里把 `force_width` 称作 justify 钩子这句话，按「未证实」处理。

## spike② 自绘文本 —— 通过

`canvas(prepaint, paint)` 的 paint 闭包直接拿到 `&mut Window`，因此无需自定义 Element 就能绘制。三个样本全部正确（见截图）：

1. **横排混合字重**：`shape_line` 一次传入两个 `TextRun`（`FontWeight(400.0)` / `FontWeight(700.0)`，同字号），`ShapedLine::paint(origin, line_height, TextAlign::Left, None, window, cx)` 绘制 —— 截图后半句明显更粗。**证实「同字号混合字族/字重/颜色由框架一次 shaping 完成」。**
2. **中英混排**：`Folio 把 position.x 当纵向…` 正常成形。
3. **竖排**：按开发方案 §4.8 档 1 的做法，遍历 `ShapedRun.glyphs`，把 `glyph.position.x`（推进量）当纵轴、`position.y`（基线偏移）当横轴，逐字 `window.paint_glyph(origin, run.font_id, glyph.id, size, color)` —— 截图右起一列汉字纵向正确堆叠，无需任何 transform。

**结论**：`paint_glyph` 无旋转参数不构成竖排障碍；引擎的绘制层可以完全自持。

## spike③ 字体加载 —— 部分通过

```
add_fonts -> Ok("Ok")
TextSystem 可见 WenKai 家族：["LXGW WenKai"]
resolve_font(LXGW WenKai) -> FontId(FontId(2))
```

`add_fonts(Cow::Owned(bytes))` 加载 `~/.local/share/fonts/design/LXGWWenKai-Regular.ttf` 成功，且 `resolve_font` 能按 family 名拿到 `FontId`，spike② 的绘制即用它。**注意**：本次读的是系统里已装的字体文件，正式实现要改成随包 `Assets` 内嵌（OFL 授权允许），否则换机器就退化。

**未测**：`ThemeRegistry::load_themes_from_str` 注册 light/dark/sepia + `Theme::change` 运行期切换。列为 M1 的前置项。

## spike④ CJK 断行质量 —— 未定论

340px 宽度下 `LineWrapper::wrap_line` 对中文语料的输出：

```
[  ok] 于是我们奋力向前，逆水行舟——被许多人当
[  ok] 作标点排版的检验样本：「了不起的盖茨比」
[  ok] 中译本里那句，！？；：『』《》
```

观察：

- 断点位置符合 CJK 惯例（任意两汉字之间可断，`当/作` 被拆开是允许的，中文不需要分词断行）。
- 行首未出现句读点，行末未出现开引号 —— **但这是样本巧合，不是禁则生效的证据**。本次语料没有构造出「边界正好落在 `「` 之后」的情形。
- `IndentAdjustment::{NoIndent, SameIndent, ExtraColumns}` 与 `MAX_INDENT=256` 可用于悬挂缩进，`首行缩进 2em` 需要另测（`Boundary.next_indent` 是 u32 列数，em 需换算）。

**下一步（M2 前置）**：构造定向语料矩阵——把 `「『《（` 与 `。，、！？」』` 精确放在宽度边界两侧各测一次，统计违规率；同时验证 `text-indent` 与 `line-height` 对断点的影响。在此之前，禁则处理按「可能需要自己做字符级替换」估工。

## 窗口与 Wayland —— 通过，有一处必须修

窗口在 niri 下正常出现、可交互、100% 缩放下文字清晰（截图右侧）。

**缺陷**：`niri msg windows` 显示我们的窗口 `App ID: "dev.zed.Zed"`（gpui-pre 默认值）。必须在 `WindowOptions` 显式设 `app_id`，否则 Wayland 下的窗口分组、任务栏图标、`.desktop` 关联全部错位。已建任务 M0-3b。

## 顺带确认的框架事实

- gpui-pre 0.3.8 的渲染后端是 **wgpu**（`gpui-pre-wgpu`），依赖树含 `cosmic-text 0.19` + `swash` + `harfrust` + `skrifa` ⇒ **shaping 是真的**（kerning / 连字 / bidi），开发方案 R1 已据此作废。
- `FontFeatures(pub Arc<Vec<(String,u32)>>)` 公开 ⇒ `vert`/`vrt2` 可显式请求。
- `TextRun` **无 size 字段** ⇒ 一次 `shape_line` 一个字号；混合字号（上标 / ruby 注文 / 首字下沉）需分段 shape 再手工对 baseline。
- `cargo check` 冷启动全量编译约 1 分 08 秒（USTC 镜像）。

## M1 接入 TextView 时踩到的八条（2026-10-08，直接决定 M2 引擎设计）

这些不是风格问题，每一条都对应一次「看起来对、实际不工作」：

1. **`TextViewState` 的 markdown 解析是异步的。** 不先 `cx.run_until_parked()` 就渲染，`list_state().item_count()` 是 0，整篇文档量不出高度。任何依赖块数的逻辑（进度、恢复、目录定位）都必须等解析落地。
2. **TextView 自带 list 虚拟化，`scrollable(true)` 时它自己滚。** 外面再套一层 `overflow_y_scroll` 是错的：内层 list 吃掉视口高度，外层 `max_offset` 恒为 0 —— 表现就是「滚不动」。滚动状态从 `state.read(cx).list_state()` 读，不要自己再持一个 `ScrollHandle`。
3. **gpui 的滚动偏移向下为负。** `scroll_px_offset_for_scrollbar().y` 是 `-900px` 这种值，直接 `offset / max` 会得到负比例并被 clamp 成 0，看起来就像"进度永远是 0%"。
4. **`RenderedText` 只有 `range_for_source`（source→rendered）单向映射。** 反解渲染位置需要二分；但**单字节探针 `mid..mid+1` 在中文上必然失败**（落在一字三字节的中间 → 返回 None），会让二分一路把答案推向章首。探针必须先吸附到字符边界。
5. **`reveal_range` 在 headless 测试里"请求被接受"但像素落点不可断言。** 其文档自己写明：请求会在「文本变化 / 行数被 clamp / 一秒内显示不出来」时被**静默丢弃**，`Ok(())` 只代表"被收下"。所以 M1 的验收断言只覆盖我们能拥有的一半（偏移↔比例一致、进度无损穿过 SQLite、恢复请求被消费），像素级恢复转成 M2 自研引擎的待办（任务已建）。
6. **`TextSelectionLayer` 在 `gpui-base`，`gpui-component` 不转发。** 要用 `gpui_kit::base::TextSelectionLayer` 挂进选择容器；缺了它，TextView 的选择行为不成立（M3 标注的地基）。
7. **`cx.observe(&entity, …)` 返回 `Subscription`，不是 `Observation<T>`。** 而 `TextViewState` 不实现 `EventEmitter`，所以"解析完成"只能靠 observe 该实体的一般变化来重试，没有专门事件。
8. **UI 集成测试要求 crate 有 lib target。** 集成测试不能 import 二进制 crate，所以 `folio-app` 拆成 `src/lib.rs`（`pub mod app/reader`）+ `src/main.rs` 薄壳；测试因此能用**生产视图**而不是测试里另搭一份近似视图。

另外两条测试写法上的教训，记下来防止重犯：
- 测试辅助函数里的 `settle()` 需要**两轮**「跑空事件 → 渲染一帧」，一轮不够（reveal 请求在首帧才挂上，而 `run_until_parked` 只跑事件不画帧）。
- 绝不要写 `assert!(x || true)` 这类永真断言占位。它比没有测试更糟，因为绿色会骗人。
