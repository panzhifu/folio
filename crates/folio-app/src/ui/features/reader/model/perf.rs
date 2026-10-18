//! 帧耗时探针：滚动卡不卡，看这里。
//!
//! 与 [`folio_core::infra::metrics`] 的分工：那边数**次数**（几次重排、几次 shaping），
//! 这边量**时间**。次数能钉回归，时间只能看趋势 —— 所以这一层的输出**不进断言**，
//! 只在 `logging.level = "debug"` 时出声（`info` 下每帧只多几次 `Instant::now`）。
//!
//! 刻意**不逐帧写日志**：`LogFile` 是同步写、每条一次 `write_all`（见 `infra::logging`），
//! 逐帧一条会把要量的东西自己改掉（探针本身变成帧预算里的一项）。所以这里在内存里攒：
//! 超阈值的帧当场报一条（那就是卡顿现场），另外每 [`REPORT_EVERY`] 帧报一条汇总
//! （中位数 / 90 分位 / 最大 + 帧间隔分位）。
//!
//! 用法：`render` 开头 `reset()`，各段用 [`Timer`] 记一笔，`paint` 里 `log_paint`。
//!
//! 出声的闸门是 `tracing::enabled!(target: "folio_app::perf", DEBUG)`，**不是**全局级别：
//! 所以真机上只要给一条按模块的覆盖就够了，不必把整个应用降到 debug（那会被别的
//! 模块的日志刷爆，而 `LogFile` 是同步写）。Android 上配置文件在
//! `<数据根>/folio/config/config.json`（Meizu 20 实测 =
//! `/data/user/0/dev.folio.reader/files/files/folio/config/config.json`，用
//! `adb shell run-as dev.folio.reader` 读写），要加的是
//! `logging.targets = ["folio_app::perf=debug"]`；日志落在 `<数据根>/folio/data/logs/folio.log`。
//! 注意**设备时钟与宿主时钟可能差几十秒**（本机实测差 44 s），分段要用设备侧时间戳。
//!
//! 2026-10-14 真机实测（Meizu 20，release）—— 这一层量出来的三条结论：
//!
//! | 场景 | 修之前 | 修之后 |
//! |---|---|---|
//! | 稳态帧（p50 / p90） | 2.4–3.2 ms / 4.1–5.7 ms | 1.1–2.5 ms / 2.2–4.1 ms |
//! | 帧间隔 p90（掉帧） | 29–38 ms | 18–21 ms |
//! | 图片首次进屏 | 8–38 ms（解+重编码，UI 线程） | ~0 ms |
//! | 一屏新字形的光栅化 | 44–52 ms | 6–10 ms（灰度 AA） |
//!
//! 前两条是 `paint_layer` + 「没挪动就不重画」，第三条是图片快路，第四条见
//! `ui::platform::android` 里那一句渲染模式。剩下的欠账（章文本入库那一跳 9.5 ms、
//! 冷 shaping）记在 `plans/阅读管线.md` §九。
//!
//! ## 2026-10-15 重测（同一台、同一个探针、这一轮代码上屏之后）
//!
//! 脚本是钉死的一部分，不写脚本的前后对比没有意义：《未来简史》（26 章 / 55 图），
//! 开书在 31%，`adb shell input swipe` 每下 **1200 ms**（短拖会把一屏并成一帧，量出来的
//! 不是滚动帧）、800 px 前 30 下 + 800 px 后 30 下 = 4440 帧；另跑一组 400 px 前 12 下 +
//! 后 12 下（走回**还在链上**的那几章，所以去程/回程是同一批文档的冷与暖）= 1560 帧。
//!
//! | 这一轮改的那几段 | 真机 release |
//! |---|---|
//! | 稳态帧 total（p50 / p90） | **0.28–0.48 ms / 0.50–1.58 ms** |
//! | 取章一段（滚动中的 `spans_us`） | **2–3 µs**（开书那一趟是另一回事：7.0–9.3 ms / 4 章，与 10-14 的 8.05 ms 同级，那笔主要花在图片索引） |
//! | 场景一段（`scene_us` / 每帧 21–24 run） | **28–43 µs** |
//! | 装饰一段（`decor_us`） | **0–3 µs** |
//! | shaping（`shape_only_us`） | 冷一屏 **4792 µs** → 暖 **230 µs**（① 那张表） |
//! | 字形环（`glyph_only_us`） | 冷 **44165 µs / 384 字**（115 µs/字）→ 暖 **15165 µs / 306 字**（50 µs/字） |
//! | 图片首次进屏 | 4 张各自所在窗口 p90 0.63–0.88 ms、最大 1.7–2.3 ms，**没有一条 ≥8 ms 的帧** |
//!
//! 两点要更正，都是按**同一个字段**重量之后才看出来的：
//!
//! 1. 上面那张旧表最后一行写的「灰度 AA 之后 6–10 ms」，用现在这个 `glyph_only_us`
//!    量不出来 —— 今天冷屏读到 44 ms、暖屏 15 ms。要么当年那格量的不是同一个量
//!    （比如只量了光栅化、没算逐字建图元），要么这一年的别处改动把它抬回去了。
//!    **没有复现出来，所以旧那一行照抄不改，新数记在这里。**
//! 2. 「一屏新字形」不是一笔钱而是两笔：暖屏仍然要 15 ms 跑完那 306 个字形，那部分是
//!    **逐字建绘制图元**（预热也躲不过，它属于 §十一 第 6 条「场景每帧整份重建」）；
//!    冷比暖多出来的 ~29 ms 才是首次光栅化，那才是 ② 能挪走的上限。
//!
//! ## 帧间隔归谁：2026-10-15 用三个新计数结掉了
//!
//! 上一段那句「别把它当掉帧计数器」现在有答案了，而答案不在 folio 这边。为此这一层加了
//! 三个数（只在探针出声时才记账，见 `audible()`）：`renders`（会话出了几次元素树 = **帧节拍**）、
//! `painted_renders`（其中真画了正文的那几帧）、`scrolls`（收到几个滚动事件），
//! 外加 `render_mid_us` / `render_p90_us`。
//!
//! 真机 4080 帧连续滚动读到的形状：
//! - `renders == frames`、`painted_renders == renders − 1`：一帧一个正文元素，出了元素树
//!   就一定画。所以「间隔被拉长是因为正文那一层被跳过」这个解释**不成立**。
//! - `render_mid_us` **16.7–16.9 ms** = 正好一个 vsync；`render_p90_us` 32–33 ms = 两个 vsync。
//!   **中位数不差、只有尾部差** —— 这是事件流与 vsync 的相位拍频形状（注入的滚动事件
//!   ~68 下/屏，与 16.7 ms 的格子错开时约一成间隔落到第二格）。GPU 画不完不是这个形状：
//!   那样中位数会一起被抬起来，而这里 UI 线程自己只用 0.4–0.5 ms。
//! - `scrolls / renders ≈ 1.2`：约两成滚动事件没挪动东西（惯性尾巴上的零增量、滚到章尾
//!   被夹住），那是正确行为，不是丢事件。
//!
//! **滚动时的帧间隔不是 folio 这几段定的。** 4080 帧里 ≥8 ms 的只有两条：开书那一趟
//! 9.0 ms（`spans_us` 6.96 ms / 4 章，主要是图片索引），以及开书后第一屏 29.7 ms
//! （字形环 26.9 ms / 314 字，它前面那一格空了 46.8 ms）。滚动中每 120 帧窗口的最大帧：
//! p50 **2.2 ms**、p90 6.7 ms。
//!
//! 于是那笔「一屏新字形 15–44 ms」要改口径：慢滚时新行是一行一行进屏的，摊成每帧
//! 零点几毫秒；**只有整屏一次进来才一次付 15–44 ms**（开书第一屏、跳章、被合帧的快 fling）。
//! ②「预热下一屏」与 §十一 第 6 条「场景每帧整份重建」想省的都是那一笔，不是每帧那笔 ——
//! 对正常阅读来说两者都排不到前面（第 6 条的上限是 0.4–0.5 ms/帧，一帧预算的 3%）。
//!
//! ## 「整屏进来那 10 ms 在 render 里，可 render 里没有一格接住它」——2026-10-15 结掉了
//!
//! 前一轮真机读到两条 11.3 / 8.0 ms 的帧，`render_us` 占了 10.4 / 7.5 ms，而**具名部分
//! 加起来只有 60 µs**（排版 4 µs、场景 53 µs、图片 0、字形 0.9 ms）。那笔钱没人看。
//! 这一轮给 `render` 补了四格探针，并且修了一个结构性漏洞：
//!
//! - **漏洞**：`render_ns` 从前收在连续滚动那一臂的**中途**，而 `total_us` 定义为
//!   `render + paint` —— 于是「捕获点之后到返回之间」那一段（选区矩形、菜单条、overlay
//!   装配）**两头都不算**，恒等式把它藏得干干净净。现在捕获点挪到元素树全部搭完之后。
//! - **新格**：`sync_us`（`sync_prefs`，改字号那一帧的作废与重排输入从这里发起）、
//!   `spine_us`（坐标链：补偿 → 恢复落点 → 迁移 → 总高夹取）、`fills_us`（批注回显）、
//!   `shape_runs_us`（渲染帧里那趟 shaping）。
//! - **`rest_us` 这一格是这次的目的本身**：它等于 `render` 减掉全部具名部分，
//!   也就是「还没人看的部分」。加探针的唯一目标就是把这个数压到零。
//!
//! 真机（Meizu 20，release `.so`，跳章 + 开书 + 25 屏慢滚）读到的形状：
//!
//! | | `render_us` | `shape_runs_us` | `spine_us` | `sync_us` | **`rest_us`** | `paint_us` |
//! |---|---|---|---|---|---|---|
//! | 跳章一帧 | 4841 | **4448** | 264 | 2 | **22** | 43559 |
//! | 跳章另一帧 | 2870 | **2301** | 461 | 4 | **14** | 21152 |
//! | 慢滚 2040 帧 | 中位 458 | — | — | — | **中位 22 / p90 29** | — |
//!
//! 三条结论：
//!
//! 1. **那 10 ms 就是 `shape_runs`**（渲染帧里逐 run 走一遍 gpui 的 shaping）。上一轮那两条
//!    帧里 `rest` 与 `shape_only` 逐条对上（差 ≤60 µs），这一轮直接量出来 2.3–4.4 ms。
//!    补上探针之后 `rest_us` 从 ~5,000 µs 掉到 **14–22 µs**，整个 `render` 段全被接住了。
//! 2. **`sync_us` 与 `fills_us` 都是个位数微秒**，包括连改 6 档字号那几帧 ——
//!    `set_prefs_on_all` 只做作废，真正的重排在后台队列里，所以「改字号很贵」不贵在配置同步。
//! 3. **整屏进来那笔钱不在 `render`，在字形环**：那两帧 `paint` 是 21.2 / 43.6 ms，
//!    其中 `glyph_only` 21.1 / 43.5 ms，而 `render` 只有 2.9 / 4.8 ms。`render` 里最大的一格
//!    `shape_runs` 也不是白花的 —— 那是 gpui 本来就要做的 shaping，且已被
//!    `Document::shaped_line` 按 (行号, 槽位) 缓存跨帧复用。**所以「整屏进来能不能优化」的
//!    答案是：`render` 这一段已经没有可省的大头了，能省的只有 ②（下一屏的字形预热）**，
//!    而它的上限就是那 21–44 ms 里「首次光栅化」那部分（暖屏对照：同一层字形环 15 ms）。

// `clippy::missing_const_for_thread_local` 在移动端那道门上要分两件事看（宿主 target 不报它，
// 只有 CI 的 `--target aarch64-linux-android --lib` 报，且 allow 挂在 `thread_local!` 语句上压不住
// —— 诊断的 span 出自宏展开内部）。`FRAME` / `LAST_PAINT_US` / `LAST_RENDER_US` / `PAINTED` 那四条
// 写的已经是 `const { … }`，`cargo clippy --fix` 交回的是同一份代码（与 `infra/metrics.rs` 同量）：
// 假阳性，没有可改的形态。最后那条 `AGG` 是**真**的，它要的 const 初始化造不出
// `Vec::with_capacity(WINDOW)`（那不是 const fn），而换成 `Vec::new()` 等于让这六个 `Vec` 在头
// `WINDOW` 帧里边推边 realloc —— 那些分配就落进了它正在量的那几帧里。量尺不该改被测对象，
// 预分配留着，allow 收在文件级。
#![allow(clippy::missing_const_for_thread_local)]

use std::cell::{Cell, RefCell};
use std::time::Instant;

/// 一帧里各段的耗时（纳秒）与几个计数。
#[derive(Default, Clone, Copy)]
pub struct Frame {
    pub render_ns: u64,
    /// `sync_prefs` 那一趟：比对配置、必要时 `set_prefs_on_all`（它逐章作废行单元、
    /// shaping 表与排版键的输入）。2026-10-15 之前这一格不存在，而整屏进来那 10 ms
    /// 就是查不到去处才加的。
    pub sync_ns: u64,
    /// 坐标链那一段：`compensate_spine` → 恢复落点 → `migrate_spine` → 总高与夹取。
    pub spine_ns: u64,
    /// 批注回显那一趟（`annotation_fills`：每条批注各问一次矩形）。
    pub fills_ns: u64,
    /// 渲染帧里那趟 shaping（`shape_runs`：逐 run 问 `Document::shaped_line`，未命中就
    /// 现走一遍 gpui 的 shaping）。整屏进来的那一帧，`render` 的钱基本全在这一格。
    pub shape_runs_ns: u64,
    pub spans_ns: u64,
    pub spans_new: u32,
    pub layout_ns: u64,
    pub scene_ns: u64,
    pub decor_ns: u64,
    pub image_ns: u64,
    pub scene_runs: u32,
    pub scenes: u32,
    pub images: u32,
    pub paint_ns: u64,
    pub shape_ns: u64,
    pub glyphs: u32,
    /// `shape_line` 那一调自己的耗时（不含逐字形绘制）。
    pub shape_only_ns: u64,
    /// 逐字形 `paint_glyph` 的耗时（含图集光栅化）。
    pub glyph_ns: u64,
    /// 与上一帧的间隔（纳秒）。远大于一帧就是掉帧。
    pub gap_ns: u64,
}

/// 慢帧阈值：UI 线程这一段超过它就报一条（16.7ms 一帧，留一半给 gpui 自己的
/// 布局/绘制与合成）。
const SLOW_NS: u64 = 8_000_000;
/// 每多少帧报一条汇总。
const REPORT_EVERY: u32 = 120;
/// 用来算分位的滑动窗口。
const WINDOW: usize = 240;

thread_local! {
    static FRAME: Cell<Frame> = const { Cell::new(Frame {
        render_ns: 0, sync_ns: 0, spine_ns: 0, fills_ns: 0, shape_runs_ns: 0, spans_ns: 0, spans_new: 0, layout_ns: 0, scene_ns: 0, decor_ns: 0,
        image_ns: 0, scene_runs: 0, scenes: 0, images: 0, paint_ns: 0, shape_ns: 0, glyphs: 0,
        shape_only_ns: 0, glyph_ns: 0, gap_ns: 0,
    }) };
    static LAST_PAINT_US: Cell<u64> = const { Cell::new(0) };
    static LAST_RENDER_US: Cell<u64> = const { Cell::new(0) };
    /// 本帧的正文那一层画过没有。`log_paint` 置起、下一次 `note_render` 读走并清零 ——
    /// 一帧里可能有好几个正文元素（链上每章一个），所以**不能拿绘制次数当帧数**，
    /// 只能问「这一帧画过没有」。
    static PAINTED: Cell<bool> = const { Cell::new(false) };
    static AGG: RefCell<Agg> = RefCell::new(Agg::new());
}

struct Agg {
    frames: u32,
    totals: Vec<u64>,
    gaps: Vec<u64>,
    worst: Option<Frame>,
    /// 上面三个数的是**正文那一层画了几次**（`log_paint`）。下面这几个数的是两件
    /// 不同的事，分开数才能判断「帧间隔 33 ms」是谁造成的：
    /// - `renders` / `render_gaps`：阅读会话被要求重新出元素树的次数与间隔 = **帧节拍**。
    /// - `painted_renders`：其中真正画了正文的那几帧。`painted_renders / renders` 低，
    ///   说明间隔拉长是「这一帧正文没动」，不是掉帧。
    /// - `scrolls`：这一窗口里收到过几个滚动事件。它比 `renders` 大得多，说明事件到了
    ///   但没挪动（惯性尾巴上的零增量、滚到章尾被夹住），那是**正确行为**，不是卡顿。
    renders: u64,
    painted_renders: u64,
    scrolls: u64,
    render_gaps: Vec<u64>,
    /// **每次绘制**画了几个字形、字形环本身花了多久。为什么要单列：`total` 的分位数
    /// 看不出「大帧是怎么大的」—— 稳态帧与整屏一次进来的帧都算进同一个 `frames`，
    /// 而 2026-10-15 那轮量到一个说不通的形状：每帧都画了正文（`renders == frames`），
    /// 稳态帧却只花 0.4 ms，另一头 384 个字花 44 ms。这两个数摆在一起才知道
    /// 「一帧画几个字」的分布，也才知道那一笔该往哪儿省。
    glyph_counts: Vec<u64>,
    glyph_times: Vec<u64>,
    /// 每帧 `rest_ns` 的窗口。它应当贴着零：它就是「render 里还没人看的部分」。
    rests: Vec<u64>,
}

impl Agg {
    fn new() -> Self {
        Self {
            frames: 0,
            totals: Vec::with_capacity(WINDOW),
            gaps: Vec::with_capacity(WINDOW),
            worst: None,
            renders: 0,
            painted_renders: 0,
            scrolls: 0,
            render_gaps: Vec::with_capacity(WINDOW),
            glyph_counts: Vec::with_capacity(WINDOW),
            glyph_times: Vec::with_capacity(WINDOW),
            rests: Vec::with_capacity(WINDOW),
        }
    }
}

/// 探针只在该出声时才记账：这三格每次都要读一次系统时钟，`info` 级别下不该付。
fn audible() -> bool {
    tracing::enabled!(target: "folio_app::perf", tracing::Level::DEBUG)
}

fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// 会话被要求重新出元素树。挂在 `render` 开头，与 [`reset`] 同一处。
pub fn note_render() {
    if !audible() {
        return;
    }
    AGG.with(|agg| {
        let mut agg = agg.borrow_mut();
        agg.renders += 1;
        if PAINTED.replace(false) {
            agg.painted_renders += 1;
        }
        let now = now_us();
        let previous = LAST_RENDER_US.replace(now);
        if previous != 0 {
            if agg.render_gaps.len() >= WINDOW {
                agg.render_gaps.remove(0);
            }
            agg.render_gaps.push((now - previous) * 1000);
        }
    });
}

/// 收到一个滚动事件。挂在 `on_scroll` 入口。
pub fn note_scroll() {
    if !audible() {
        return;
    }
    AGG.with(|agg| agg.borrow_mut().scrolls += 1);
}

pub fn reset() {
    FRAME.with(|frame| frame.set(Frame::default()));
}

pub fn add(edit: impl FnOnce(&mut Frame)) {
    FRAME.with(|cell| {
        let mut value = cell.get();
        edit(&mut value);
        cell.set(value);
    });
}

fn take() -> Frame {
    FRAME.with(|cell| {
        let value = cell.get();
        cell.set(Frame::default());
        value
    })
}

/// 一段代码的耗时。
pub struct Timer(Instant);

impl Timer {
    pub fn start() -> Self {
        Self(Instant::now())
    }
    pub fn ns(&self) -> u64 {
        self.0.elapsed().as_nanos() as u64
    }
}

fn percentile(values: &[u64], ratio: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64 - 1.0) * ratio).round() as usize;
    sorted[index]
}

/// 绘制那一跳调用：补上绘制自己的两格，攒进窗口，慢帧当场报。
pub fn log_paint(paint_ns: u64, shape_ns: u64, glyphs: u32) {
    let mut frame = take();
    // 本帧画过正文（`note_render` 在下一次出元素树时读走）。
    PAINTED.set(true);
    frame.paint_ns = paint_ns;
    frame.shape_ns = shape_ns;
    frame.glyphs = glyphs;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    frame.gap_ns = LAST_PAINT_US.with(|last| {
        let previous = last.get();
        last.set(now);
        if previous == 0 {
            0
        } else {
            now.saturating_sub(previous) * 1000
        }
    });
    let total = frame.render_ns + frame.paint_ns;
    AGG.with(|agg| {
        let mut agg = agg.borrow_mut();
        agg.frames += 1;
        if agg.totals.len() >= WINDOW {
            agg.totals.remove(0);
            agg.gaps.remove(0);
        }
        agg.totals.push(total);
        agg.gaps.push(frame.gap_ns);
        // 每次绘制各记一格：字数与字形环耗时放在一起，才除得出「每个字多少钱」。
        if agg.glyph_counts.len() >= WINDOW {
            agg.glyph_counts.remove(0);
            agg.glyph_times.remove(0);
        }
        agg.glyph_counts.push(u64::from(frame.glyphs));
        agg.glyph_times.push(frame.glyph_ns);
        if agg.rests.len() >= WINDOW {
            agg.rests.remove(0);
        }
        agg.rests.push(rest_ns(&frame));
        let worse = agg
            .worst
            .map(|worst| total > worst.render_ns + worst.paint_ns)
            .unwrap_or(true);
        if worse {
            agg.worst = Some(frame);
        }
        let audible = tracing::enabled!(target: "folio_app::perf", tracing::Level::DEBUG);
        if audible && total >= SLOW_NS {
            report("PERF slow", &frame, total);
        }
        if audible && agg.frames % REPORT_EVERY == 0 {
            let totals = agg.totals.clone();
            let gaps = agg.gaps.clone();
            let render_gaps = agg.render_gaps.clone();
            let glyph_counts = agg.glyph_counts.clone();
            let glyph_times = agg.glyph_times.clone();
            let worst = agg.worst.unwrap_or_default();
            let mid = percentile(&totals, 0.5) / 1000;
            let p90 = percentile(&totals, 0.9) / 1000;
            let gap_mid = percentile(&gaps, 0.5) / 1000;
            let gap_p90 = percentile(&gaps, 0.9) / 1000;
            let max = totals.iter().copied().max().unwrap_or(0) / 1000;
            tracing::debug!(
                target: "folio_app::perf",
                frames = agg.frames,
                mid_us = mid,
                p90_us = p90,
                max_us = max,
                gap_mid_us = gap_mid,
                gap_p90_us = gap_p90,
                renders = agg.renders,
                painted_renders = agg.painted_renders,
                scrolls = agg.scrolls,
                render_mid_us = percentile(&render_gaps, 0.5) / 1000,
                render_p90_us = percentile(&render_gaps, 0.9) / 1000,
                glyphs_mid = percentile(&glyph_counts, 0.5),
                glyphs_p90 = percentile(&glyph_counts, 0.9),
                glyph_us_mid = percentile(&glyph_times, 0.5) / 1000,
                glyph_us_p90 = percentile(&glyph_times, 0.9) / 1000,
                rest_mid_us = percentile(&agg.rests, 0.5) / 1000,
                rest_p90_us = percentile(&agg.rests, 0.9) / 1000,
                rest_max_us = agg.rests.iter().copied().max().unwrap_or(0) / 1000,
                "PERF agg",
            );
            report("PERF worst", &worst, worst.render_ns + worst.paint_ns);
        }
    });
}

/// `render` 那一段里**还没有任何探针接住**的那一截。
///
/// 这一格存在的理由是它自己：加探针的唯一目的就是把它的数压到零。哪一天它又涨起来，
/// 说明 `render` 里长出了一段没人看过的代码，而那正是 2026-10-15 那次「整屏进来 10 ms
/// 查不到去处」的形状 —— 那时 `render_us=10404` 而具名部分加起来只有 60 µs。
///
/// `decor_us` 不减：它是 `Scene::build` 里面的一格（`scene_us` 的子项），减了会重复。
fn rest_ns(frame: &Frame) -> u64 {
    let named = frame.sync_ns
        + frame.spans_ns
        + frame.layout_ns
        + frame.spine_ns
        + frame.fills_ns
        + frame.shape_runs_ns
        + frame.scene_ns
        + frame.image_ns;
    frame.render_ns.saturating_sub(named)
}

fn report(tag: &str, frame: &Frame, total_ns: u64) {
    tracing::debug!(
        target: "folio_app::perf",
        total_us = total_ns / 1000,
        render_us = frame.render_ns / 1000,
        sync_us = frame.sync_ns / 1000,
        spine_us = frame.spine_ns / 1000,
        fills_us = frame.fills_ns / 1000,
        shape_runs_us = frame.shape_runs_ns / 1000,
        rest_us = rest_ns(frame) / 1000,
        spans_us = frame.spans_ns / 1000,
        spans_new = frame.spans_new,
        layout_us = frame.layout_ns / 1000,
        scene_us = frame.scene_ns / 1000,
        decor_us = frame.decor_ns / 1000,
        image_us = frame.image_ns / 1000,
        scenes = frame.scenes,
        scene_runs = frame.scene_runs,
        images = frame.images,
        paint_us = frame.paint_ns / 1000,
        shape_us = frame.shape_ns / 1000,
        shape_only_us = frame.shape_only_ns / 1000,
        glyph_only_us = frame.glyph_ns / 1000,
        glyphs = frame.glyphs,
        gap_us = frame.gap_ns / 1000,
        "{tag}",
    );
}

// ══ 临时探针（量完整块摘掉，连同 navigation.rs 的两处、position.rs 的一处挂点）══
//
// 要判的事：读者说「停止滑动那一下发抖」。gpui 的惯性是**关于 elapsed time 的闭式曲线**
// （`gpui-pre-0.3.9/src/gestures.rs:972-1001`，增量还被 `.max(0.)` 夹成只许前进），所以
// 一帧晚到的表现不是丢帧，而是**下一步特别大** —— 中段看不出来，减速尾巴上邻居都是
// 一两像素的碎步，那一步就是「抖」。于是要把三件事放在同一个样本里看：
// 事件隔了多久（帧节拍）、那一步挪了多少（位移）、处理器自己花了多久（我们占的）。
//
// 为什么攒 64 个样本才出一行：`LogFile` 是**同步**写的（见 `folio_core::logging`），
// 逐事件打日志会把 write() 那一趟算进被量的 `handler_us` 里，探针自己就成了被测的抖动。
// 出那一行的耗时落在样本记完之后，只推迟下一帧约几十微秒。

/// 攒多少个滚动事件出一行。30 秒连续滑动约 1800 个事件 → 28 行。
const SCROLL_WINDOW: usize = 64;

/// 一个滚动事件的样本。全部是微秒与像素。
struct ScrollSample {
    /// 与上一个滚动事件到达时刻的间隔。惯性段里这就是真正的帧节拍。
    gap_us: u64,
    /// `on_scroll` 整个处理器花掉的时间（含下面两项）。
    handler_us: u64,
    /// `persist_if_moved` 那一趟：反解位置 + 可能写库。没走到记 0。
    persist_us: u64,
    /// 其中真正落在 `store.set_position` 上的一段（含抢那条全库连接互斥锁的等待）。
    write_us: u64,
    /// 平台给的增量（像素）。
    dy: f32,
    /// 这一步实际挪了多少（夹过滚动范围之后）。与 `dy` 不等就是被夹住或被人改写过。
    step: f32,
    /// 触摸相位：0=Started 1=Moved 2=Ended 3=Cancelled 9=没有（桌面滚轮）。
    phase: u8,
    /// 这一步开始时 `scroll_y` 的**绝对值**。为什么要它：`step` 只记处理器自己那一段，
    /// 而渲染帧还会在两次事件之间再改一遍位置（补偿 / 迁移 / 按新总高重夹）。
    /// `y0_i` 与 `y0_{i-1} + step_{i-1}` 不等，中间就有人动过手，差值即挪了多少像素。
    y0: f32,
}

thread_local! {
    /// 上一个滚动事件到达的时刻（微秒）。0 = 还没有。
    static SCROLL_ARRIVAL_US: Cell<u64> = const { Cell::new(0) };
    /// 上一次 `set_position` 花掉的那一段。写库在 `persist_if_moved` 里，而它先于
    /// [`scroll_end`] 发生，所以是**同一个事件**的样本把它收走并清零。
    static SCROLL_WRITE_US: Cell<u64> = const { Cell::new(0) };
    static SCROLL_SAMPLES: RefCell<Vec<ScrollSample>> = const { RefCell::new(Vec::new()) };
}

/// 起表：记到达时刻，返回与上一个事件的间隔。挂在 `on_scroll` 最前面。
pub fn scroll_begin() -> u64 {
    if !audible() {
        return 0;
    }
    let now = now_us();
    SCROLL_ARRIVAL_US.with(|last| {
        let previous = last.replace(now);
        if previous == 0 {
            0
        } else {
            now.saturating_sub(previous)
        }
    })
}

/// 记「刚才这一趟写库花了多久」。挂在 `persist_locator` 里 `set_position` 两侧。
/// 记在线程局部而不是返回给调用方：那条路要穿过三层 `Option`/`Result`，挂点会铺开。
pub fn scroll_note_write(us: u64) {
    if audible() {
        SCROLL_WRITE_US.with(|cell| cell.set(us));
    }
}

/// 收表：一个样本进窗口，攒满就出一行。
#[allow(clippy::too_many_arguments)]
pub fn scroll_end(
    gap_us: u64,
    handler_us: u64,
    persist_us: u64,
    dy: f32,
    step: f32,
    y0: f32,
    phase: u8,
) {
    if !audible() {
        return;
    }
    let write_us = SCROLL_WRITE_US.with(|cell| cell.replace(0));
    SCROLL_SAMPLES.with(|samples| {
        let mut samples = samples.borrow_mut();
        samples.push(ScrollSample {
            gap_us,
            handler_us,
            persist_us,
            write_us,
            dy,
            step,
            y0,
            phase,
        });
        if samples.len() < SCROLL_WINDOW {
            return;
        }
        // 逐样本一段紧凑串，离线用脚本拆。字段顺序：gap;handler;persist;write;dy;step;y0;phase
        let strip = samples
            .iter()
            .map(|s| {
                format!(
                    "{};{};{};{};{:.1};{:.1};{:.1};{}",
                    s.gap_us, s.handler_us, s.persist_us, s.write_us, s.dy, s.step, s.y0, s.phase
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let max_gap = samples.iter().map(|s| s.gap_us).max().unwrap_or(0);
        let max_handler = samples.iter().map(|s| s.handler_us).max().unwrap_or(0);
        let max_write = samples.iter().map(|s| s.write_us).max().unwrap_or(0);
        samples.clear();
        tracing::debug!(
            target: "folio_app::perf",
            max_gap_us = max_gap,
            max_handler_us = max_handler,
            max_write_us = max_write,
            strip = %strip,
            "滚动事件探针",
        );
    });
}
