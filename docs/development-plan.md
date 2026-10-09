# Folio 开发方案 — 基于 gpui-kit 的桌面阅读器

> 状态：v2（排版路线已改）· 2026-10-08 · 目标框架：gpui-kit 0.7.1 / GPUI (gpui-pre 0.3.8)
> 开发环境：Arch Linux，niri（Wayland-only），rustc 1.99
>
> **v2 变更**：正文排版从「EPUB → 规范化 Markdown → TextView 渲染」改为「**自研 DOM + CSS 子集 + 排版引擎**」。
> 触发原因是样式需求确定为：中文排版基础（缩进/居中/扉页）、图片尺寸与对齐、ruby / 脚注弹层 / 竖排、多栏 / 绕图 / 局部颜色字重。
> §3 记录了支持这一决定的实测 API，并纠正 v1 中两处过于乐观的判断。

## 1. 背景与目标

Folio 是一款原生 GPU 渲染的桌面阅读器，不使用 Electron / Tauri / WebView。

- **一期**：Linux 桌面（Wayland 优先），后续扩展 macOS / Windows。
- **MVP 范围**：书库 + 标注 + 主题的完整闭环。
- **架构约束**：新增一种书籍格式的成本必须控制在一个模块 + 一行注册以内；与 UI 无关的逻辑必须能搬到移动端复用。

---

## 2. 三个已钉死的技术结论

### 2.1 渲染路线：gpui 原生绘制，不用 `gpui-webview`

`gpui-webview` 底层是 Wry + **WebKitGTK**，而 GTK 只能嵌入 **X11 window**。Wayland 协议中不存在「把一个客户端的 surface 放进另一个客户端的 surface」的机制，因此：

- 使用 WebView 就必须让整个应用跑在 X11 / XWayland（GPUI 一个进程只连接一个显示服务，该选择对所有窗口生效）。`WebView::build()` 检测到 Wayland 窗口时直接返回错误。
- niri 是 Wayland-only 合成器。强行 XWayland 会失去原生路径，且**分数缩放下窗口比原生更模糊**（GTK 仅支持整数 `GDK_SCALE`）。
- WebView 会遮挡同区域的 GPUI 内容，**popover / dialog / tooltip 盖不上去**（无合成器的 X11 桌面还会退化为「从网页中挖空」、不绘制阴影）。阅读器恰恰重度依赖选中浮动菜单、目录浮层、批注弹层。

> v2 下这条结论的含义更明确：正文**由我们自己的引擎排版，交给 gpui 绘制**，框架与 WebView 都不参与正文渲染。

### 2.2 移动端：架构必须留门（Android 已决定先行，见下方补充）

gpui-kit 官方立场：首要目标仍是桌面，移动支持只是让**部分组件**能在 iOS / Android 原生界面里复用，**没有计划**做成 Flutter 那样的完整移动框架。现状基于社区 `gpui-mobile`，gpui-kit 通过兼容性 fork 的 `gpui-pre-mobile` 接入，**只能 git 依赖、未发布 crates.io**。仅 Swift 托管的 iOS 模拟器示例经过验证；已验证的是一个 AI Chat 区域（TextView / Button / Menu / Popover / Scrollbar / Input / Textarea / 文本选择），其他组件与完整布局未验证。

> **架构约束**：`folio-core` 不得依赖 gpui。v2 之后这条约束变得更关键 —— **DOM、CSS、布局算法全部是无 UI 的纯计算**，可以原样跑在移动端宿主里，只有 `Measure`（字体度量）和 `Paint`（绘制）两处需要平台实现。将来移动端只需替换这两个实现。

#### 2.2b Android 先行（2026-10-08 决定；现状与构建配方见 [`docs/android.md`](android.md)）

一期移动端**只做 Android**。平台层来自 `longbridge/gpui-mobile`（crates.io 包名 `gpui-pre-mobile`，lib 名仍是 `gpui_mobile`）的 `gpui-pre` 那条线：`src/android/{platform,window,dispatcher,display,keyboard,jni}.rs` 与 Gradle + native-activity 宿主已成型。许可证是三选一 `GPL-3.0 OR AGPL-3.0 OR Apache-2.0`，我们取 **Apache-2.0**，不把 MIT 的 Folio 拖进 copyleft。

上面那条架构约束已兑现：**`folio-core` 零 gpui 依赖，Android 原样复用**。移动端只需要替换 `Measure`（字体度量）与绘制两处，两份实现都在 `crates/folio-app/src/mobile/`（`measure.rs` / `paint.rs`）。

当初判为「唯一未知数」的 trait 漂移，实测只有**一处**：`gpui-pre` 0.3.7 → 0.3.8 给 `RequestFrameOptions` 加了 `signal_at` 与 `signal_source` 两个字段，在 AChoreographer 回调处补齐即可。所以既不必把桌面降到旧快照，也不必接受「移动端只做正文 + 两套 UI 并存」。

**（后记 2026-10-09）移动端现在与桌面端在同一个 crate**：`crates/folio-app` 下移动代码在 `src/mobile/`，由 `cfg(mobile)` 与桌面模块互斥编译。原先分家的理由「两端链的不是同一个 `gpui` 包」在 fork 换到 `gpui-pre =0.3.8` 之后不成立；`[patch.crates-io]`（`async-task` 那条）与 `[profile.release]` 随之挪到仓库根 `Cargo.toml`。见 `docs/android.md` §1 与 §6。

已达成（2026-10-08 在 Meizu 20 真机验证）：正文渲染、平台滚动、底部导航栏四格切换。缺口清单见 `docs/known-limitations.md` 的「移动端」一节 —— 其中一条是这份文档当初就写下的要求至今没落实：这条依赖**仍带着 default features**（30 多个平台插件全开），`default-features = false` 还没改。

### 2.3 gpui-kit skill（已安装）

`skills/gpui-kit`（组件目录、用法、GPUI 机制、测试）与 `skills/gpui-kit-design-guides`（布局、间距、层级、交互态、浮层、文案）已装到 `.qoder/skills/`，两份 SKILL.md 均含 references/ 深读文件（coding-guides.md、recipes.md、gpui/*.md）。

其中两条规范本方案全程遵守：**Never invent an API**（每个 API 必须在当前源码里核实签名）与 **framework owns behavior, application owns presentation**。

---

## 3. 引擎可行性依据（实测 API，v2 新增）

自研排版引擎的成败取决于「gpui 是否允许把文本绘制权交给我们」。核实结论是**允许**，且度量、断行、绘制三级都有公开接口：

### 3.1 绘制：逐字形

```rust
// gpui-pre-0.3.8/src/window.rs:4687
pub fn paint_glyph(&mut self, origin: Point<Pixels>, font_id: FontId,
                   glyph_id: GlyphId, font_size: Pixels, color: Hsla) -> Result<()>
```
同族还有 `paint_emoji`(:4792)、`paint_image`(:4919)、`paint_quad`(:4550)、`paint_path`(:4621)、`paint_underline`(:4638)，以及自定义绘制容器 `canvas()`（`elements/canvas.rs:10`）。

- **含义**：行盒、列、浮动避让区、ruby 双层定位全部可由我们自行决定坐标。
- **竖排 ≠ 旋转，竖排 = 交换轴向**：横排 shaping 结果里 `ShapedGlyph.position.x` 是沿行推进量、`.y` 是相对基线偏移，竖排只需把两者对调后交给 `paint_glyph`。汉字本就不旋转，因此 **CJK 竖排不需要 transform**。真正需要旋转的只有两类：竖排中的拉丁词/数字、竖排标点（见 §4.8「竖排的四档实现」）。

### 3.2 度量：单字级，公开

```rust
// gpui-pre-0.3.8/src/text_system.rs
advance(font_id, font_size, ch) -> Result<Size<Pixels>>      // :433
layout_width(font_id, font_size, ch) -> Pixels                // :446
typographic_bounds(font_id, font_size, ch) -> Result<Bounds>  // :414
ascent / descent / bounding_box / em_width / ch_width
/ x_height / cap_height / units_per_em / baseline_offset      // :409-518
resolve_font(&Font) -> FontId                                 // :370
add_fonts(Vec<Cow<'static,[u8]>>) -> Result<()>               // :295
```

- **含义**：单字级度量公开，用于 CSS 单位换算（`em`/`ex`/`ch`）与字体回退链。
### 3.2b **shaping 全部公开**（v2.1 更正 —— v2 初稿在此判断错误）

gpui 自带完整 shaping：依赖树里是 `cosmic-text 0.19` + `swash` + `harfrust` + `skrifa`，渲染走 `gpui-pre-wgpu`。**这推翻了 v2 初稿 R1「gpui 无 shaping、复杂文字不可行」的结论。**

```rust
// text_system.rs —— 三个入口都接受 runs: &[TextRun]，即一行内混合字族/字重/字号
pub fn shape_line(&self, text: SharedString, font_size: Pixels,
                  runs: &[TextRun], force_width: Option<Pixels>) -> ShapedLine        // :638
pub fn shape_text(&self, text, font_size, runs: &[TextRun],
                  wrap_width: Option<Pixels>, line_clamp: Option<usize>)
    -> Result<SmallVec<[WrappedLine; 1]>>                                             // :750
pub fn layout_line(&self, text:&str, font_size, runs:&[TextRun],
                  force_width: Option<Pixels>) -> Arc<LineLayout>                      // :886

// text_system/line_layout.rs —— shaping 结果结构体公开可遍历
pub struct LineLayout { pub font_size, pub width, pub ascent, pub descent,
                        pub runs: Vec<ShapedRun>, pub len }
pub struct ShapedRun  { pub font_id: FontId, pub glyphs: Vec<ShapedGlyph> }
pub struct ShapedGlyph{ pub id: GlyphId, pub position: Point<Pixels>,
                        pub index: usize, pub is_emoji: bool }
impl LineLayout { pub fn index_for_x(&self, x: Pixels) -> Option<usize> }               // :61
pub struct GlyphId(pub u32);                                                           // :1255

// text_system/line.rs —— ShapedLine 可直接绘制，且内建对齐
impl ShapedLine {   // Deref → Arc<LineLayout>
    pub fn paint(&self, origin: Point<Pixels>, line_height: Pixels,
                 align: TextAlign, align_width: Option<Pixels>,
                 window: &mut Window, cx: &mut App) -> Result<()>                        // :108
    pub fn paint_with_underline_handler(..)                                              // :129
    pub fn paint_background(..)                                                          // :160
    pub fn split_at(&self, byte_index: usize) -> (ShapedLine, ShapedLine)                 // :193
    pub fn take_until(&mut self, byte_index: usize) -> ShapedLine                          // :282
    pub fn width(&self) -> Pixels;  pub fn x_offset(&self) -> Pixels
}
```

对引擎成本的四条直接修正：

1. **同一字号下的混合字族/字重/颜色由框架 shaping 一次完成**（`runs: &[TextRun]`）—— §4.5-1「需要自己按 run 分段测量」的估计作废。混合**字号**仍需分段（见下方「新的真实限制」）。
2. **`text-align` 与两端对齐不必自己实现**：`ShapedLine::paint(.., align, align_width)`，`force_width` 就是 justify 的钩子。
3. **行盒与 baseline 公开**：`LineLayout.{ascent,descent,width}`。
4. **命中测试横排白拿**：`index_for_x`；竖排自己写 `index_for_y`（遍历 `glyph.position`）。

`rasterize_glyph` 是 `pub(crate)`（:574）⇒ 拿不到 gpui 的字形位图；只有竖排「真侧转」那一档才需直接依赖 `swash`/`skrifa`（已在树内，版本天然对齐）。
**OpenType feature 可直达（v2.2 更正）**：`pub struct FontFeatures(pub Arc<Vec<(String, u32)>>)`（`text_system/font_features.rs:8`）是公开的 tag→value 列表，经 `Font.features` 生效 ⇒ **`vert`/`vrt2`（竖排专用字形）可以显式请求**，竖排标点优先靠字体而非码点替换。
**新的真实限制**：`TextRun { len, font: Font, color, background_color, underline, strikethrough }` **没有 size 字段**（字号是 `shape_line` 的入参）⇒ 一次 shape 只能一个字号；**混合字号**（上标、ruby 注文、首字下沉）需分别 shape 再手工对齐 baseline。混合字族/字重/颜色则同一字号内自由。

### 3.3 断行：框架已有，且支持行内对象

```rust
// text_system.rs:545
fn line_wrapper(self: &Arc<Self>, font: Font, font_size: Pixels) -> LineWrapperHandle
// text_system/line_wrapper.rs:52
pub fn wrap_line<'a>(&'a mut self, fragments: &'a [LineFragment],
                     wrap_width: Pixels, indent_adjustment: IndentAdjustment)
    -> impl Iterator<Item = Boundary> + 'a
// :665  LineFragment::{ Text{ text: &str }, Element{ width: Pixels, len_utf8: usize } }
// :725  Boundary { ix: usize /* 行末字符索引 */, next_indent: u32 }
// 另有 should_truncate_line / truncate_line / truncate_wrapped_line，MAX_INDENT = 256
```

- **含义**：UAX 风格的断行 + 悬挂缩进（`IndentAdjustment::{NoIndent, SameIndent, ExtraColumns}`）现成可用；`LineFragment::Element` 表示**图片等行内对象能参与折行** —— 这是图文混排的地基，不必自己实现。
- **两条路线并存，按层选用**：`LineWrapper` 绑定单一 `font + font_size`，适合纯文本段落与编辑器式悬挂缩进；**混合样式段落应改用 §3.2b 的 `shape_text(.., runs, wrap_width)`**，它一次完成折行 + shaping + 多样式。引擎的 inline 层因此不必自己分段测量，只需处理「断行候选与 float 避让可用宽度」的交互。

### 3.4 半自绘备选（若某层想省事）

`elements/text.rs:614 pub struct TextLayout` 暴露了 `paint_background()` / `paint_foreground()`（:176/:181）以及命中测试 `index_for_position()` / `position_for_index()` / `line_layouts()`（:231/:265/:319）。
即：可以「一行一个不折行的 Text 元素 + absolute 定位 + `set_highlights` 指定 run 样式」，用我们的断行结果、把绘制交回框架，还白拿位置↔索引映射。逐字自绘与这条路可按层混用。

### 3.5 v1 中两处判断的纠正（保留记录，避免重犯）

1. **gpui-kit 明确不做 CSS**。`crates/base/src/text/text_view.rs:139` 的 Not Goals 原文：「Customization of the complex style」「**As a HTML viewer, we not support CSS**, we only support basic HTML tags…」。**执行 CSS 无论走哪条路线都是我们的活。**
2. **`TextView` 的 HTML 视图不支持源区间映射**，他们自己的测试为证：`state.rs` 中 `html_text_is_unsupported` 断言 `set_range_highlights(...) == Err(RangeHighlightError::Unsupported)`；`html_views_convert_no_source` 断言 `text.range_for_source(..) == None`。
3. **`RangeHighlight` 不是样式通道**：`pub struct RangeHighlight { range: Range<usize>, background: Hsla }` —— 只有背景色，没有字号/字重/颜色。v1 里「样式可以挂在区间高亮上」的隐含假设不成立。

> 结论：v1 选 Markdown 中间层是为了白拿 `selected_source_range()`。既然样式需求已确定为必须自研排版，**这个白拿的价值就消失了**，而它的代价（丢失 DOM 结构与 class/style 信息）反而成为天花板。

### 3.6 引擎侧可直接复用的 gpui-kit 能力（正文之外）

排版引擎接管正文后，gpui-kit 的角色变成**应用壳 + 交互原语**，这部分依然全部复用：

| 需求 | 复用 | 位置 |
|---|---|---|
| 应用入口 | `gpui_kit::application().with_assets(assets::Assets).run(..)` + `gpui_kit::init(cx)` + `open_window(options, cx, build)` | `kit/src/lib.rs:176` |
| 书库侧栏 / 工具条 / 标签 / 状态栏 | `Sidebar`+`SidebarGroup`+`SidebarMenu`；`Toolbar`+`ToolbarGroup`；`TabBar::new(id).pill()`；`StatusBar::new().left().right()` | `sidebar/mod.rs:222`、`toolbar.rs:168`、`tab/tab_bar.rs:59`、`status_bar.rs:41` |
| 面板分栏（对照 / 笔记） | `DockArea`、`add_panel`、`dump()/load()` 可序列化 | `base/src/dock/dock_area.rs:126` |
| 主题与运行期切换 | `cx.theme()`、`Theme::change(ThemeMode, None, cx)`、`ThemeRegistry::global_mut(cx).load_themes_from_str(&json)`、theme JSON 支持 `font.family` | `theme/mod.rs:389`、`theme/registry.rs:151` |
| 设置面板 / 对话框 / 弹层 | `Settings::new(id).page(SettingPage)`、`window.open_dialog/open_sheet/push_notification`（`WindowExt`）、`Popover`、`PopupMenu` | `setting/settings.rs:48`、`window_ext.rs:15` |
| **正文选中原语（引擎要用它接选择）** | `base::TextSelectionHandle`（`new/register/update_runs -> TextSelectionProjection`、`snapshot`、`copy_with/focus_with/clear_with`）、`SelectableText::new(id,text)/with_handle`、`TextSelectionRun`、`TextSelection::selected_text(window,cx)` | `base/src/selectable_text.rs:31/37`、`base/src/text_selection.rs:789/2329` |
| 滚动与虚拟化 | `overflow_y_scrollbar()`、`Scrollbar`/`ScrollbarMode`/`AutoScroll`；`v_virtual_list(view,id,item_sizes,closure)` + `scroll_to_item(ix,ScrollStrategy)` | `base/src/virtual_list.rs:139` |
| 图片解码后显示（封面、PDF 页） | `img(ImageSource)`、`ImageSource::{Resource,Image,Render,Custom}`、`Image::from_bytes(fmt,bytes)`（按字节 hash 缓存）、`.image_cache(&Entity<I>)` | `gpui-pre/src/elements/img.rs:201/42` |
| 后台解析 / 布局 | `cx.spawn(..)` 持 `Task` 取消 + `cx.background_spawn(..)`，`this.update(cx,..)` 回投 | 范式 `examples/tig/src/app.rs:205` |
| 无头 UI 集成测试 | `#[gpui_kit::test]` + feature `test-support`，`render_frame()/find()/click()/press()` | `crates/kit/src/test.rs:25` |

⚠️ `WindowOptions` **没有** `title_bar`/`appearances`/`center`/`size`/`font_priority`；真实字段是 `titlebar: Option<TitlebarOptions>`、`window_bounds`（`WindowBounds::centered(size,cx)`）、`window_decorations`、`window_min_size`、`app_id`、`icon` 等（`platform.rs:2438`）。Linux 自绘标题栏用 `TitleBar::title_bar_options()`。
⚠️ **实测缺陷（M0-3）**：我们的 `folio` 窗口在 niri 里 `App ID` 显示为 `dev.zed.Zed`（gpui-pre 的默认值），必须显式设置 `app_id = "dev.folio.Reader"`，否则 Wayland 下的窗口分组、任务栏图标、.desktop 关联全部错位。

---

## 4. 架构设计（v2）

### 4.1 workspace 分层（2026-10-09 更正为实际结构）

```
folio/
├── Cargo.toml                    workspace：members = crates/folio-core, crates/folio-app
│                                 （default-members 只有 folio-app，所以裸 `cargo run` 起桌面）
├── crates/folio-core/            ★ 零 gpui 依赖，全部可 `cargo test`
│   ├── dom/                      html5ever → NodeId arena DOM（可序列化、身份稳定）
│   ├── css/                      规则集 + 选择器匹配 + specificity → ComputedStyle（含 UA 表）
│   ├── layout/                   纯计算排版：块级 + 多 run 行内 + UAX#14 断行 + 图片尺寸
│   ├── format/                   epub / markdown / txt（pdf、cbz 属 M7/M8）
│   ├── store.rs                  rusqlite：schema、迁移、书库 CRUD、位置
│   ├── position.rs               Locator：章 + 章内字符偏移
│   ├── config.rs                 读者偏好、外观、语言、账号（落盘为 config.json）
│   ├── i18n.rs / paths.rs        文案与落点（paths 有 `set_app_dirs`，给没有标准目录的移动平台）
│   └── locales/                  rust-i18n 表，一门语言一份（`en.toml` / `zh-CN.toml` / `ja.toml`）
└── crates/folio-app/             gpui-kit 侧（`[lib] crate-type = rlib + cdylib + staticlib`）
    ├── build.rs                  嵌上游主题集 + 设 `cfg(mobile)`
    ├── ui/shared/                ★ 两端共用：config_model / l10n / theme / account / images /
    │                             measure（`Measure` 的 gpui 实现）/ document（一章 → Layout）/ chrome
    ├── ui/desktop/               app.rs 外壳、reader.rs、engine.rs（行盒进虚拟列表）、
    │   └── settings/             一页一个文件 + window.rs（开-or-聚焦的独立窗口）
    └── ui/mobile/                mod.rs 三平台入口、shell.rs（顶部章导航 + 底部栏）、
                                  reader_view.rs（逐行绘制 + 触摸滚动 + 换章落盘）、paint.rs
```

`folio` 二进制名由 `crates/folio-app` 产出（`[[bin]] name = "folio"`）。

这一层分界的判据是**「这段代码在两个平台上必须不能长得不一样」**：断行口径、层叠、版心宽度、
主题解析、颜色换算都在 `ui/shared`，只有一份实现；`desktop` 与 `mobile` 只负责把自己那侧的
滚动与摆法接上去。旧结构里移动端另写了一份 `measure.rs` 与 `document.rs`（与桌面端逐行同义、
只是 import 不同），那两份已经并进来 —— 并进来之后 `ui/shared/document.rs` 的 tests 同时就是移动端的
排版测试，因为移动 UI 只在 `cfg(mobile)` 下编译，宿主测试本来碰不到它。

### 4.2 数据流：五段，每段都可单测

```
文件字节
  └─ FormatAdapter ─→ 章节 (XHTML/TXT/PDF page)
       └─ Parser    ─→ DOM(arena)                 ← 纯数据，落盘缓存
            └─ Style    ─→ ComputedStyle 表        ← CSS 子集，纯函数
                 └─ Layout ─→ LayoutBox 树 + LineBox[]  ← 注入 Measure，纯计算
                      └─ Paint ─→ paint_glyph / Text 元素 + Selection runs
```

**锚点定义在 Layout 之前的坐标系上**（`NodeId` + 节点内 char 偏移），不是 Layout 之后。这是 v2 的关键性质：字号、行距、窗口宽度、分页模式全变，锚点不变；也正因如此，标注不需要 v1 设计里的「上下文指纹兜底」那么重的机制（指纹仍保留，作为 DOM 结构变化时的兜底）。

```rust
pub struct NodeId(pub u32);                        // arena 索引，仅在单章内稳定
pub struct TextAnchor {
    pub chapter: u32,
    pub node: NodeIdentity,   // 稳定身份，见 §4.3
    pub start: u32,           // 节点内 char 偏移
    pub end: u32,
    pub exact: String,        // 兜底指纹
    pub prefix: String, pub suffix: String,
}
```

### 4.3 节点身份稳定性（必须正面解决）

重新解析同一章会得到新的 arena 索引。`NodeIdentity` 取「可复现的结构签名」：

```
(spine_href, 从最近带 id 的祖先向下的 (tag, child_index) 路径, text_hash)
```
解析时给每个元素算一次并入库；带 `id`/`ref` 属性的节点直接用该 id。重定位顺序：① id 命中 → ② 路径命中且 `text_hash` 匹配 → ③ 路径命中但内容变了 → 用 `prefix+exact` 在该子树内模糊定位 → ④ 全文定位 → ⑤ 失败则标 `stale`（仍出现在侧栏与导出，正文不高亮，**绝不静默丢用户数据**）。

### 4.4 CSS 子集范围（显式清单，避免无限扩张）

不承诺 CSS 兼容，承诺**这份清单**：

- **选择器**：`*`、type、`.class`、`#id`、`[attr]`/`[attr=v]`、后代/子代/相邻兄弟、`,` 分组、`:first-child`/`:last-child`/`:nth-child(n)`/`:lang()`。伪元素 `::first-letter`/`::first-line` 仅做首字下沉（drop cap）。
- **继承**：`font-*`、`color`、`text-align`、`line-height`、`letter-spacing`、`word-spacing`、`writing-mode`、`direction`。非继承按 CSS 规范处理。
- **层叠**：origin 顺序 `用户 agent < reader.css（用户偏好） < author < author !important < reader !important`；specificity 用 `selectors` crate 的实现；`@media`（阅读宽度/方向）与 `@font-face`（走 `add_fonts`）。
- **盒模型**：`margin`（块级外边距折叠）、`padding`、`border`、`width/height` 的 `px/em/rem/%/auto`、`max-width`、`display: block|inline|none|inline-block|list-item`、`box-sizing`。
- **文本**：`font-family/size/weight/style/variant`、`color`、`text-indent`、`text-align: left|right|center|justify`、`line-height`、`white-space: normal|pre|nowrap`、`text-decoration`、`letter-spacing`、`word-spacing`、`hyphens`（中英分开处理，中文不连字）。
- **分页/分栏相关**：`page-break-before/after`、`break-inside: avoid`、`orphans`/`widows`、`columns`/`column-gap`（多栏与分页共用 fragmentation 引擎，见 §4.5）。
- **浮动与图片**：`float: left|right|none`、`clear`、图片 `width/height/max-width`、`object-fit`（按 gpui 能力实现）、`vertical-align`。
- **明确不支持并降级**：`position: fixed/sticky`、`transform`、`transition/animation`、`flex`/`grid`（除非接 `taffy`，属独立决策）、`background-image`、`opacity`、`z-index`、`writing-mode: vertical-*` 中的拉丁侧转。
降级策略统一为「忽略该声明、保留内容」，并写入 `docs/known-limitations.md`。

**2026-10-09 的落地情况**（不在上面清单里的东西一律按「接受不生效 + 计 `ignored`」处理）：
`margin` / `padding` 的 1–4 值简写、`font` 简写（有意不重置未写到的子属性）、`word-spacing`、
`vertical-align: super|sub|<length>|<percentage>`、`text-decoration` / `text-decoration-line`
都已经真生效；`text-transform` 仍留在不生效名单里，理由不是工作量而是坐标系 —— 它会改变字符本身，
而锚点与进度建立在源文本字节偏移上，要接得连 §4.6 的锚点映射一起定。

这份清单准不准可以用书来量：`cargo run -p folio-core --example css-gap`（无界面）按**元素**排名，
判定与层叠共用 `css::computed::declaration_status`。仓库那本《蛇结》在这一轮从
「41.8% 的元素身上有胜出却没接住的声明」降到 2.1%，「不认识」归零；同一轮正文总高只动了
+0.04%。两个数都要看，只有前者会诱导去补没有视觉收益的属性，只有后者会漏掉正在骗人的统计。

### 4.5 排版引擎的四个硬骨头（按依赖顺序）

1. **Inline formatting context（E2）**：把 DOM 内联子树展平成 `Run` 流（每 run 携带字体/字号/颜色/区间），用 §3.2 的 `advance` 累加宽度、§3.3 的 `wrap_line` 定断点（跨 run 时按 run 分段喂或自建 wrapper 池），求每行的 max-ascent/descent 定行盒高，`justify`/`text-align` 在此层生效。
2. **Block layout（E1）**：块级盒顺序布局、外边距折叠、`text-indent`、`::first-letter` 下沉占位。
3. **Float 避让（E3）**：行盒维护「活动浮动区间表」，断行时按行询问可用宽度 `left/right offset`。`LineFragment::Element` 让图片本身可作为行内对象参与折行（图注、行内小图标走这条路；绕图走浮动）。
4. **Fragmentation（E4）**：`BreakToken` 驱动的按高度切分，分页与多栏共用一套实现（多栏 = **一栏一片**、按顺序填满一栏再进下一栏；分页 = 固定视口分片 + 允许 `break-inside: avoid` 提升整块）。初稿写的「多栏 = 平衡高度的分栏分片」在 2026-10-09 实现时改成顺序填满：平衡高度会让同一页里各栏的内容长度不同，阅读顺序要在页内跳栏，对连续翻页的读者是负担；代价是末页各栏可能长短不一，已记进 `docs/known-limitations.md`。**这也是引擎同时产出「连续滚动 / 分页 / 双页对开」三种模式的原因** —— v1 里这三者是不同代码，v2 里是同一套 fragment 引擎的不同参数。

`Measure` 由 app 用 `TextSystem` 实现。注意：断行候选与 shaping 都在 CPU 上，DOM/Style/Layout 三段应能在 `background_spawn` 里跑（`LayoutBox` 必须是 `Send` 纯数据，绘制阶段只读），这是「1000 页书翻页 < 30ms」的前提。**M0 spike① 因此改为验证：不持有 `Window` 时能否用 `TextSystem` 度量与断行**（`line_wrapper`/`advance` 都不要求 `&mut Window`，理论上可，但要实测）。

### 4.6 选择与标注（E 系列之外的独立工程）

引擎自持 run 表后，选择必须我们接线：用 `base::TextSelectionHandle`（`register/update_runs -> TextSelectionProjection`）把每个 LineBox 的文本注册成 run，`index_for_position` 级别的命中由我们的几何（行盒 + run 宽度）负责，`selection.snapshot()` 的 `Range<usize>` 经 run 表反解回 `TextAnchor`。
gpui-kit 已把「拖拽选择、双句柄、触摸长按选择、复制到剪贴板、全选」做在 `TextSelectionLayer` 里（`base/src/text_selection.rs`），我们只补 run ↔ 锚点的映射，不必重造选择交互。

PDF 仍是独立分支：pdfium 栅格化页 + 文字层 glyph box → 锚点为 `{page, rects}`，与 reflow 分支共享标注存储与 UI。

### 4.7 存储

`~/.local/share/folio/folio.db`，SQLite（`rusqlite` bundled，FTS5 免费带来「全书 + 跨书批注搜索」）：

```sql
books(id, path UNIQUE, title, authors, format, cover BLOB, size, added_at, last_opened_at)
positions(book_id PK, mode, locator_json)              -- 进度：chapter + char offset（非页码）
chapters(id, book_id, idx, title, spine_href, char_len)
dom_cache(chapter_id, blob)                             -- 序列化 DOM，避免重复解析
style_cache(chapter_id, blob)                            -- 章节 CSSOM
annotations(id, book_id, chapter_id, anchor_json, note, color, stale, created_at, updated_at)
collections(id, name) / collection_books(collection_id, book_id, added_at)
CREATE VIRTUAL TABLE chapter_fts USING fts5(text, content=…);
CREATE VIRTUAL TABLE annotation_fts USING fts5(note, exact);
```

外键 + `PRAGMA journal_mode=WAL`；迁移用 `user_version` 手写递增。

**阅读偏好与账号另存为** `<系统配置目录>/folio/config.json`（`dirs::config_dir()`：Linux `~/.config`、
macOS `~/Library/Application Support`、Windows `%APPDATA%`），不放进书库数据库 —— 它是应用级、跨书的数据，
一个小 JSON 文件更方便用户查看/备份/手改。v1 迁移里建的 `prefs` 表已不再使用（迁移只增不改，故保留）。

**手改坏一处的爆炸半径是一节，不是整份**：`Config::load_from` 先整份解析，失败时按节重试
（`config.rs::salvage`），所以 `"font_size":"big"` 只会让 `reader` 一节退回默认，主题名、书库目录、
读者名都还在。粒度止于节 —— 同一节里写对的键跟着一起退，serde 的字段级默认只有整节解成功才有机会生效。
枚举一律手写序列化、不用 derive（`Mode`、`Locale`）：derive 只认变体名，而界面上显示的是
`system` / `zh-CN` 这类标签，读者照看到的写法手改就会读失败。`Locale` 存出去是 BCP-47 的 `code()`，
读回来宽松到认 `en` / `en-US` / `English` / 变体名 `En`（老文件里的值），见 `i18n::Locale::from_label`。

---

### 4.8 竖排的四档实现（M6 的执行清单）

竖排不等于旋转。横排 shaping 结果里 `ShapedGlyph.position.x` 是**沿行推进量**、`.y` 是**相对基线偏移**，把两者对调就是竖排：

```rust
// 全部为已核实的公开 API
let shaped = cx.text_system().shape_line(text.into(), font_size, &[run], None); // text_system.rs:638
let mut y = origin_y;                                    // 纵向推进
for r in &shaped.runs {                                  // LineLayout.runs
    for g in &r.glyphs {                                 // ShapedGlyph{ id, position, index }
        let ox = origin_x + g.position.y;                // 基线偏移 → 横向
        let oy = y        + g.position.x;                // 推进量   → 纵向
        window.paint_glyph(point(px(ox), px(oy)), r.font_id, g.id, shaped.font_size, color)?;
    }
    y += shaped.width;                                   // 整行宽度 = 本列占位
}
```

| 档 | 内容 | 做法 | 成本 |
|---|---|---|---|
| **1** | CJK 正文竖排、竖排中的数字/拉丁（**縦中横**：字母直立逐字堆叠，这是日文排版的正规处理方式，不是降级） | 上述轴向对调 | 公开 API，纯几何 |
| **2** | 竖排标点 | 首选：给 `Font.features` 传 `FontFeatures(Arc::vec![("vert".into(),1),("vrt2".into(),1)])`，用字体自带的竖排形式（`。、《》「」` 本身居中，对调即正确；`！？`、`——`、引号方向靠 `vert`）。兜底：排版前做一张 U+FE10–FE19 码点替换表，供缺 `vert` 的字体使用 | feature 传入近乎零成本；替换表是纯数据 |
| **3** | 竖排中拉丁**词**整体侧转 90° | `rasterize_glyph` 是 `pub(crate)` 拿不到位图 ⇒ 自己栅格：直接依赖已在树内的 `swash`/`skrifa` 取 outline → 画进 Q8 紧凑位图 → **90° 旋转 = 转置 + 一次翻转（整数操作，无滤波损失）** → `image` 编 PNG → `Image::from_bytes(Png, bytes)` → `Window::paint_image`。缓存 key `(font_id, size, text)`，一个词栅格一次 | 独立模块，可后置；中文书低频 |
| **4** | ruby 注音 | 基文与注文各跑一次 `shape_line`（字号不同），注文列更高、行盒按 `max(基文高, 注文高)` 扩展；CJK 的 1 基字 : 1 注音是简单情形 | 中等 |

命中测试：横排 `LineLayout::index_for_x` 现成；竖排自己写 `index_for_y`（遍历 `glyph.position`），几行代码。

---

## 5. 依赖选型（v2）

```toml
# crates/folio-core —— 全部纯 Rust，无 gpui
html5ever        = "0.40"   # 浏览器级 HTML5 解析（正确处理 malformed XHTML）
cssparser        = "0.38"   # CSS Syntax Level 3 词法/解析
selectors        = "0.41"   # Servo 的选择器匹配 + specificity，实现其 Element trait 即可
unicode-linebreak = "0.1.5" # UAX#14 断行候选（喂给 inline 层）
zip       = { version = "8.6", default-features = false, features = ["deflate"] }
quick-xml = "0.42"          # container.xml / OPF / NCX / navMap
rusqlite= { version = "0.40", features = ["bundled"] }   # FTS5
image   = { version = "0.25", default-features = false, features = ["jpeg","png","webp"] }
serde = { version = "1", features = ["derive"] } / serde_json = "1" / thiserror = "2" / chrono = "0.4"

# crates/folio-app
gpui-kit  = { version = "0.7", features = ["assets"] }   # 壳、组件、主题、测试
pdfium-render = "0.9"      # M6 才引入：PDF 栅格 + 文字层
```

v1 的 `htmd` / `pulldown-cmark` 随 Markdown 中间层一并去掉。
需要 shaping（连字 / complex script）时再评估 `cosmic-text 0.19`；需要 flex 时再评估 `taffy 0.14` —— 两者都不提前引入。

**避坑记录**：`rustpub` 是 ActivityPub 协议库不是 EPUB；`kuchiki` 自 2020 年归档；`epub` crate（2.1.5）偏写入、容器/导航覆盖不全，我们自研解析层。

### 实现偏离记录：`selectors` / `cssparser` 未接入（2026-10-08）

上面原计划用 `selectors` 做选择器匹配、`cssparser` 做 CSS 词法。**实现时改成了自写**，理由与代价都记在这里：

- `selectors::SelectorImpl` 需要 10 个关联类型，外加 `NonTSPseudoClass` 与 `PseudoElement` 两套 trait 实现，再配 `cssparser` 的 `QualifiedRuleParser` + `DeclarationParser` 双 visitor —— 那是为 Servo 的全量 CSS Selectors 4 准备的骨架，而 §4.4 承诺的子集（`*` / type / `.class` / `#id` / `[attr]` / 四种组合器 / `,` / `:first-child` / `:last-child` / `:lang()`）用不上它。
- 改成自写后：选择器与规则装配约 500 行、25 个测试覆盖（含 specificity 序、引号内逗号/花括号、注释、`@media` 递归、畸形输入不 panic）。
- **换来的真实收益**：`cssparser` 只被当作工具箱而非现成 stylesheet 解析器，用它仍要自己写规则装配；自写版顺带把「清单外的构造让整条选择器作废」这条策略做实了（`selectors` 会尽力解析出可支持的部分，反而造成部分生效）。
- **代价**：`:nth-child(An+B of S)`、属性选择器的 `i`/`s` 大小写标志、`:is()`/`:where()` 等语义要自己实现。当前策略是**一律拒绝并计入 `RuleSet::ignored`**，不猜语义。若将来某本书因此明显掉样式，再单独评估接 `selectors`（接口已收在 `Selector::parse` / `RuleSet::matching_declarations` 两处，替换成本可控）。

**层叠模型的一条纠正**：`!important` 与 origin 是**交错**的，不是「先比来源再比重要」。CSS Cascade 从低到高是
`UA normal < user normal < author normal < author !important < user !important`。
第一版把 origin 放在重要位之上，导致「读者 !important 盖不过作者普通声明」，被测试抓出。
同时 `!important` 是**逐声明**的属性，所以层叠以声明流（`matching_declarations`）而不是规则为单位输出。

---

## 6. 里程碑（v2）

引擎与产品是两条并行的时间线，分开编号，避免「引擎没做完 = 产品不能用」。

| # | 交付 | 内容 | 验收 |
|---|---|---|---|
| **M0** | 环境与地基验证 | ✅ skill 已装；✅ workspace 分层 + `cargo check` 通过（gpui-kit 0.7.1）；窗口跑通 niri/Wayland。**spike①** 无 `Window` 能否 `line_wrapper`/`advance`；**spike②** `paint_glyph` 自绘一行中英混排并验证 baseline/行高；**spike③** `add_fonts` 加载自带中文 serif + `ThemeRegistry` 注册 light/dark/sepia 并 `Theme::change`；**spike④** CJK 断行表现（`wrap_line` 在中文语料上的断点、标点是否越线） | 窗口不崩、可拖拽缩放、分数缩放不发虚；四份结论写入 `docs/spikes.md` |
| **M1** | 产品骨架（不含引擎） | `FormatAdapter` + Markdown/TXT 适配器（走临时 `TextView` 渲染，仅作为占位可读实现）；`Sidebar` 书库 + `TabBar` 多书 + `Toolbar` + `StatusBar`；设置面板与偏好持久化；主题切换；SQLite schema + 迁移；进度记忆 | ✅ **已完成**（34 测试全绿、零警告）。验收断言在 `ui/features/reader/desktop.rs` 的 tests（原本单在 `tests/ui.rs`）：滚轮真产生位移、字符偏移↔滚动比例一致、进度无损穿过 SQLite、设置面板可开可关、越界偏好被夹住。**遗留**：像素级恢复（TextView 的 `reveal_range` 是黑盒，M2 自研引擎后由我们保证）；sepia 主题注册 |
| **M2** | **引擎 E1：DOM + CSS + 块级排版** | html5ever DOM（arena + NodeIdentity）；`selectors`/`cssparser` 接入与 cascade；Block layout + 外边距折叠 + `text-indent`；单字体 inline（`wrap_line`）；`Measure` 走 TextSystem；`text-align: center` | 一本地化中文 EPUB 的章节能显示出**首行缩进、居中标题、正确行距** —— 这是中文排版基础达成点。**✅ 已完成（2026-10-08）**：DOM（13 测试）、CSS 子集 + 层叠 + UA 表（61 测试）、E1 块级/行内排版（16 几何测试）、EPUB 容器（11 测试 + 真实书冒烟）、app 侧 `Measure` + 逐行虚拟列表。验收断言在 `folio_app::ui::features::reader::desktop::tests::the_engine_indents_centers_and_spaces_lines`（首行缩进 2em、居中标题、行盒高按各自字号、真的折行、行互不重叠），并在 niri 下开真实窗口截图确认观感。遗留：`selectors`/`cssparser` 仍未接入（§5 偏离记录）；像素级位置恢复属 M5。限制清单见 `docs/known-limitations.md` |
| **M3** | **引擎 E2 + E3：局部样式与图片** | 多 run inline（混合字重/字号/颜色）；`<span>/<em>/<strong>` 行内样式；图片按 `width/max-width` 缩放、块级图片对齐；`LineFragment::Element` 行内图片；封面与内嵌图 resolver | 局部颜色/字重正确；插图不溢出、可居中、可控宽度。**✅ 已完成（2026-10-08）**：E2 —— 断行改由引擎自持（UAX #14 禁则），每个 `LineRun` 带自己的字体/字号/颜色与绘制宽度，行盒高取最高 run、基线取最大 ascent；验收见 `inline_styles_survive_into_runs`。E3 —— EPUB `image/*` 读出并入库（`images` 表 + v4 迁移），排版期按本章目录解析 `src`；`LineFragment::{Text,Image}` 枚举取代 `LineBox.runs`，行内图以哨兵字符 U+FFFC 参与折行、宽度计入断行，块级图（`display:block`）独立成行按容器 `text-align` 对齐；`layout::image_size` 实现 `width/height/max-width`（px/em/rem/%/auto）并按纵横比缩放、绝不溢出；app 侧 `ChapterImages` 提供内在尺寸与 `gpui::Image`。验收见 `layout::tests::*image*`（6 条）与 `ui.rs::images_flow_through_the_engine_as_fragments`。遗留：`float` 绕图属 M4 |
| **M4** | **引擎 E4：Fragmentation = 连续/分页/双页** | `BreakToken` 分片引擎；`page-break-*`、`break-inside: avoid`、`orphans/widows`；连续滚动 = 单一无限分片；分页/双页 = 视口分片；`columns`/`column-gap` 多栏；布局结果后台 `background_spawn` 缓存 | 三种模式同源；缩放窗口后重排在后台完成，主线程不卡；**多栏达成**。**进行中（2026-10-09）—— 分片核心已成立**：`layout::fragment` 是布局之后的纯后处理，读 `Layout` + 计算样式表，不重新折行；`Fragmentainer` 实现了 `Iterator`（`.count()` = 总页数、`.nth(k)` = 第 k 页），`BreakToken` 可存可续（`resumed`），所以翻到第 500 页不必先分完前 499 页。规则齐的：按高度切、`break-before/after: page\|always\|left\|right`（含左右页奇偶补空片）、`break-inside: avoid`（块比页高时让位，保证推进）、`orphans`/`widows`（两难时保 orphans，与排版传统一致）。CSS 侧 `orphans`/`widows`/`break-inside`/`page-break-*` 从「接受不生效」的名单移进了真生效。**多栏（视口级）已完成（2026-10-09）**：一片 = 一栏，一页 = 连续 `columns` 片，`Fragment{page, column}` 直接给位置；强制断点把本页剩下的栏补成空片，而这份补位状态存在 `BreakToken::pad_until` 里（所以从中间某一页续算仍然逐片相等，`resuming_in_the_middle_of_padding_still_pads` 就是专门钉这一条的）。四条切点规则一行没改。**还没做**：作者写的块级 `column-count`（栏宽决定折行，属排版期）、后台 `background_spawn` 排版缓存、以及 app 侧的分页/双页/分栏上屏接线（含把 `column_width` 算出的栏宽交给排版）—— 三者是 M4 剩下的全部 |
| **M5** | 选择、标注、书库收尾 | `TextSelectionHandle` ↔ run 表；选中浮动菜单（4 色 / 笔记 / 复制 / 搜索）；`TextAnchor` 回显与四级降级重定位；批注侧栏；Markdown 导出；DOM/style 缓存使二次打开 < 100ms | 改 3 档字号 + 缩窗口 → 全部锚点命中或显式 stale；导出 diff 符合预期。**地基已完成（2026-10-09）**：`folio-core::anchor` 给了可见文本坐标系（`TextMap`）、带四个指纹的锚点（`TextAnchor`）、四级降级（`relocate`：原偏移 → 同 DOM 节点 → ±400 字窗口 → 全章 → 显式 stale）、以及坐标 ↔ 行盒 ↔ 滚动位置的双向映射；`Store` 补上 `annotations` 表的全套 CRUD（那张表 v1 就建好了，但整个仓库没有一行代码碰过它）。进度已从「章」落到「字」：两端存的都是章内字符偏移，验收那条「改字号 + 缩窗口 → 命中」由 `shared_layout.rs::the_progress_coordinate_survives_a_relayout` 与 `ui.rs::the_engine_progress_is_a_character_offset_that_comes_back` 钉住。**还没做**：选择交互（`TextSelectionHandle` ↔ run 表）、浮动菜单与批注侧栏、Markdown 导出、二次打开提速 |
| **M6** | 脚注弹层 + ruby + 竖排 | 脚注：链接点击 → `Popover`/`Sheet`（相对 gpui-kit，成本低）；`<ruby>/<rt>` 双层定位与行盒扩展；`writing-mode: vertical-rl` 逐字堆叠（**拉丁侧转不支持，明确降级**） | 日文可读书；带注音的儿童读物可读书；竖排书籍可读书 |
| **M7** | PDF | `pdfium-render` 栅格 + 文字层 → `{page,rects}` 锚点；缩放/适应宽度；LRU 页缓存与预取 | 1000 页 PDF 首次翻页 < 30ms，连续翻页不掉帧 |
| **M8** | 打磨与发布 | FTS5 全书/跨书搜索；集合；CBZ 漫画；快捷键表 + Vim 式导航；`DockArea` 双栏对照；打包（`.desktop` + icon + AppImage，Wayland `app_id`）；`#[gpui_kit::test]` UI 回归 | `cargo test` 全绿 + 手工验收清单 |

**成本要诚实（v2.1 下修）**：M2–M4 是「写一个电子书专用的小型排版引擎」，这三块是整个项目的主要风险与工时所在。v2 初稿估 3–6 人月；核实 §3.2b 之后，**E2（混合样式行内）与 justify/对齐这两块由框架承担**，估计下修到 **2–4 人月**（不含 UI/书库/标注/PDF）。剩余的真正自研量集中在：CSS 层叠与盒模型（E1 的 style 部分）、float 避让（E3）、fragmentation（E4）。建议 M1 的产品骨架与 M2 的引擎地基**并行推进**，先用 M1 的占位渲染保持产品可运行、可演示，避免长时间没有可用版本。

---

## 7. 风险与对策

- **R1~~gpui 无 shaping~~（已作废）**：v2.1 核实 `shape_line`/`shape_text`/`layout_line` 与 `LineLayout.runs[].glyphs[]` 全部公开，gpui 内部是 cosmic-text + swash + harfrust，kerning / 连字 / bidi 可用。剩余的真实限制只有两条：① 无公开 OpenType feature 开关 ⇒ `vert`/`vrt2` 拿不到（竖排标点靠 U+FE10–FE19 码点替换解决，见 §4.8）；② `rasterize_glyph` 是 `pub(crate)` ⇒ 旋转位图需自己走 swash 栅格。
- **R2 竖排的拉丁与标点**：`paint_glyph` 无旋转 ⇒ 竖排中的拉丁字母与部分标点无法侧转。对策：M6 单独交付，先做纯汉字竖排 + 标点替换表，拉丁降级为堆叠，并在 UI 里提示。
- **R3 自研选择的正确性**：跨行、跨节点、反向拖拽、长按触摸选择都要接 `TextSelectionHandle` 的 run 注册契约，比 v1 的白拿难得多。对策：M5 前先在 gpui-kit 的 `selectable_text` 测试与 `examples/text_selection`、`examples/touch_selection` 上吃透契约，再动我们的 run 表。
- **R4 性能**：DOM→Layout 全链在缩放窗口时重算。对策：`LayoutBox` 为 `Send` 纯数据，布局在 `background_spawn` 跑，绘制只读；分片缓存（已排版页不重排）；spike① 必须先证明离窗可行，否则布局只能留在主线程，性能目标要下调。
- **R5 CSS 子集无限扩张的引力**：每本奇怪的书都会提出「再加一条声明」。对策：§4.4 的清单是合同；清单外的**一律降级 + 记录**，不允许临时加特例进引擎核心。问题逐本打补丁的方式是加规则数据，不是加代码路径。
- **R6 Wayland 成熟度**：GPUI Linux 侧 X11 用 `x11rb`、Wayland 用 `wayland-client`，Vulkan 渲染。niri 下需验分数缩放、IME（中文搜索与笔记输入）、剪贴板、窗口装饰。对策：M0 spike 优先；`gpui_kit::platform::linux(WindowingModes::X11)` 是最后逃生阀（不依赖 WebView，退化到 XWayland 不损失功能）。
- **R7 pdfium 非纯 Rust 依赖**：需 `libpdfium`（约 10MB），Linux 无现成包。对策：M7 之前完全不碰。
- **R8 gpui-kit 0.7 迭代快**：本机已缓存 `gpui-pre-0.3.5/0.3.7/0.3.8`。对策：提交 `Cargo.lock`；升级 kit 版本作为独立任务，不与功能开发混合。
- **R9 引擎未完成时没有可用产品**：对策即 §6 的并行策略 + M1 的占位渲染，`Reader` 抽象成 trait，占位实现与引擎实现可切换。

---

## 8. 验证方式

1. **引擎单元测试（主力，`folio-core` 不依赖 gpui）**：DOM 解析（喂 malformed XHTML 语料）、`NodeIdentity` 稳定性（重解析后签名一致）、选择器匹配与 cascade 优先级、外边距折叠、断行结果、fragmentation 分片点、锚点重定位。布局层用**假 `Measure`**（固定字宽）⇒ 几何可完全确定、断言可精确、不依赖字体环境。
2. **快照/语料测试**：一组 EPUB 语料（中文出版书、多栏杂志页、带 ruby 的日文书、图较多的书）跑 Layout 后 dump LineBox 树做快照比对，作为回归网。
3. **UI 集成测试**：`#[gpui_kit::test]` + `test-support`，`render_frame()` 后用 `find()/click()/press()` 断言：M1 打开书、M5 选中→浮动菜单→批注进侧栏→重启仍在、M4 翻页后 `StatusBar` 页码变化。范式抄 `crates/kit/tests/ui.rs:58` 与 `tests/common/mod.rs:9`。
4. **手工验收**：niri 下 golden path —— 导入 → 打开 → 改字号 → 缩窗口 → 高亮 → 写笔记 → 重启 → 回到原位置 → 导出 Markdown；并检查分数缩放清晰度、IME 中文输入、窗口拖拽。
5. **性能**：`gpui-fps` 盯连续滚动与翻页；基准语料固定 1000 页 PDF 与 800KB 单章 EPUB。
