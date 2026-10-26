# 已钉死的事实

从实测与真机踩坑提炼，写代码前先读。改动任何一条 = 先在真机/宿主上重新验证。

## 框架（gpui-pre 0.3.8，锁版 `=0.3.8`，升级是独立任务）

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
  多半是这一条。所以 `folio_core::format::shrink_image` 在第一次要画时把长边缩到 2048 以内
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
- 验证卫生：`screencap` 整帧 md5 必变（状态栏时钟），比对要裁状态栏数内容区差异像素；
  判断输入行为前先 `am force-stop` + `am start` 取基线（出现过幽灵触摸）。

## 语料与格式

- Markdown 适配器按 `#`/`##` 分章：以标题开头的语料 chapter 0 只有标题，看起来像
  「正文没渲染」，不是 bug。测试语料把正文段落放在第一个标题之前。
- CSS 子集是合同：`crates/folio-core/src/css/computed.rs` 顶部清单之外的一律忽略并记录，
  不许临时加特例进引擎核心；要扩清单，先改清单（代码里那份）再写实现。
- `css-gap` example 的落差表口径：按元素排名的声明覆盖率，不含未实现属性的**几何**影响；
  结论只用于排优先级，不是渲染保证。

## 支撑「自研排版」的三条实测结论

- **排版可以离开窗口在后台线程完成** —— `Arc<WindowTextSystem>` 可 move 进
  `background_spawn`，缩放窗口时布局不卡主线程（后台化仍欠着，见 roadmap）。
- **gpui 自带 shaping**（cosmic-text + swash + harfrust），kerning / 连字 / bidi 现成；
  `ShapedLine::paint` 内建文本对齐。
- **竖排不等于旋转** —— 横排 shaping 里 `glyph.position.x` 是推进量、`.y` 是基线偏移，
  两者对调即得 CJK 竖排，不需要 transform（拉丁侧转除外，显式降级）。
