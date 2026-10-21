//! 计数型量具：给「回归断言必须能在关掉修复时失败」提供可断言的东西。
//!
//! 为什么是计数而不是计时：计时在 CI 里被调度噪声牵着走（本项目实测过 ±20% 的摆动，
//! 见 `plans/layout-optimization.md` §5.3），而「取 100 张图，容器被重新打开几次」这个问题
//! 有一个确定的整数答案，不依赖机器、不需要真机、也不会被系统活动骗。
//!
//! ## 两套计数各自的用途
//!
//! [`count`] 同时累加**线程局部**值与**进程全局**值，因为这两份要回答的问题不同：
//!
//! - 线程局部（[`local`] / [`take_local`]）给测试用。cargo 默认并行跑测试，若测试比对全局
//!   增量，另一个线程正好也在开容器，断言就会随机失败。测试与被测代码在同一个线程上跑，
//!   所以读局部值天然不受别的测试干扰。
//! - 进程全局（[`global`]）给诊断输出用：读者报障时要看的是一整轮运行的总量。
//!
//! ## 一条纪律：别给热路径逐项加计数
//!
//! 这里只允许统计**本身就贵**的事件（打开容器、建条目名索引、重排一个块）。每行、每字形
//! 那种量级一旦**逐项**插进计数器，量具本身就成了被测量的开销——与「每张图每帧一条
//! `info!`」是同一种错误，只是换了个出口。
//!
//! 要数那种量级的东西，走 [`Metric::add`]：**一段事做完记一次、把数量当值加进去**。
//! 一次 Cell + 一次原子加，与被它记下的那次堆拷贝相比可以忽略，而断言照样是确定的整数。
//! `Scene::build` 就这么记它物化了几个 run。
//!
//! [`count`]: Metric::count
//! [`local`]: local
//! [`take_local`]: take_local
//! [`global`]: global

// `clippy::missing_const_for_thread_local` 在这一格是空转：下面那个 `thread_local!` 写的
// `const { [const { Cell::new(0) }; 14] }` 已经是它要的那个形态，而 `cargo clippy --fix` 交回的
// 是把 `const` 加回来的同一份代码（2026-10-16 实测，改前改后 `diff -q` 一致）。只有
// `--target aarch64-linux-android --lib` 报它（CI 的移动端那道门），宿主三条 target 都不报，
// 诊断的 span 出自宏展开内部 —— 挂在 `thread_local!` 语句上的 allow 也压不住它（同样实测）。
// 所以这是那条 lint 在这个 target 上的假阳性，不是这里少写了一层；没有可改的形态，allow 收在文件级。
#![allow(clippy::missing_const_for_thread_local)]

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

/// 被数的事件。**固定集合**，不做成字符串键：字符串键会让计数器名随写随起，
/// 拼错一个字母就得到一个永远为零的计数器，而"为零"看起来像是好消息。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Metric {
    /// 打开一个 zip 容器（= 读一遍中央目录）。滚动时每次取图、每次取章文本都算一次。
    ///
    /// 目标值：一本书整个阅读过程应当是 **1**（容器索引留成共享的不可变结果，
    /// 取字节只按条目偏移定位）。见 `plans/阅读管线.md` 第十三节第 2 步。
    ContainerOpens,
    /// 扫一遍容器的**条目名全表**。一次就算一次，因为它的代价是 O(条目数) 且每个名字
    /// 都要分配一个 String。
    ///
    /// 这条是「一次建表、之后查表」的守门员：建表正确时，一个容器开一次只扫一遍；
    /// 谁把查名退回成逐次扫全表，这条立刻翻红。
    EntryNameTablePasses,
    /// 解析一份 XHTML → DOM（`Dom::parse`）。一章一次，代价与章长成正比。
    ///
    /// 目标值：**改一次读者偏好，这一格增量 0**（`plans/阅读管线.md` §十二）。解析产物
    /// 与读者偏好无关，所以它应当按章缓存着复用；每次改字号都重解析，就是「全书重解析」
    /// 那个已知瓶颈的直接体现。
    ///
    /// 注意这一格在**读库/取章那一路也会被加**（`format::` 里校验 XHTML 的那几处），
    /// 所以断言要用 `take_local` 圈出自己的那段，而不是读进程总量。
    DomParses,
    /// 场景被构造了几次（`Scene::build` 一次算一次；连续滚动一帧里每个进链的章各一次）。
    ///
    /// 这一格问的是**呈现层有没有跨帧复用**：现在每帧整份重建，所以每一帧都在涨。
    SceneRebuilds,
    /// 行单元**新建**了几条（`reader::document::Document::row_units` 未命中时记一笔，
    /// 值 = 那一行的单元条数）。一条单元的主体是那段从 `BlockBox::text` 切出来的
    /// `SharedString` —— 那才是被缓存的这笔钱。
    ///
    /// **口径 2026-10-15 从「每帧摆了几条 run」改成「这一帧新建了几条单元」**，计数点也跟着
    /// 从 `Scene::build` 挪进 `row_units`：前者数的是这一帧搬运了多少条（缓存生效时照样涨），
    /// 后者数的才是真正付过的物化次数。不改这一格口径，那条往返目标门永远绿不了。
    ///
    /// 目标值：**滚下去再滚回同一屏，第二趟这一格增量 0**。单元表按行**懒物化**，某一行
    /// 第一次进带必须物化它，所以「往前滚为 0」是个错的口径（2026-10-13 立门时就这么错过一次，
    /// 见 `plans/阅读管线.md` §九 第四条纪律）；回到走过的地方才是那个确定的零。
    ///
    /// 根子在 `GlyphRun.x/baseline` 存的是**相对元素原点的屏幕值**：滚动一动每条 run 都变，
    /// 场景只能整份重造。修法是「按行号索引的行单元表（章内局部量）+ 每帧一个平移量」——
    /// 已接（2026-10-15）。
    SceneRuns,
    /// 落到 gpui 那层 shaping 的次数（`reader::paint::shape` 一趟记一次）。
    ///
    /// **口径是「章的 shaping 表未命中一次」**，不是「每条 run 一次」：表命中时绘制只抬
    /// 一次引用计数，一趟 gpui 都不走（`reader::document::Document::shaped_line`）。
    ///
    /// gpui 自己有 `LineLayoutCache`，命中时不重 shaping，但它只保两帧、而且**键含整段
    /// 文本**——命中也要把文本哈希一遍，那是 O(文本长度)。所以这一格测的是「有多少趟
    /// 掉到了 gpui 那层」，两帧之外的那些（邻章排完到进屏、滚回走过的地方）从前全算钱。
    ///
    /// 门是 `reader::view::tests::scrolling_back_to_the_same_band_does_not_reshape`
    /// （往返增量 0），探测器是它上面那条 `..._currently_rematerializes_the_visible_runs`
    /// （往前滚增量 > 0，把计数删掉它当场红）。**它仍然不进 `SceneRuns` 那条往返门**：
    /// run 物化是行单元表（`plans/阅读管线.md` §九）的事，两笔分开钉。
    ShapeLineCalls,
    /// 为「取一章」而**重解整本**那一趟，一次算一次（`BookDocument::full_sections`）。
    ///
    /// 单文件容器（MOBI / FB2 / RTF / Markdown）的 `Section` 位置就是这个形状：provider 不知道
    /// 怎么只解第 N 章，只能整本重来。实测（合成 600 章 KF7，release）：整本 109 ms，
    /// **翻一章 107 ms** —— 顺着读完整本就是几百个 107 ms。
    ///
    /// 目标值：**一本书整个阅读过程是 1**（第一次翻章解完就 memo 住，后面是下标取值）。
    /// 它涨到「章数」那一档，就是 `section_texts` 那条 memo 断了。
    WholeBookRereads,
    /// `format::shrink_image` 被调了几次 —— 那是**解码 + 重编码**，真机实测一张
    /// 1000×1386 的插图 8–38 ms，而且跑在 UI 线程上（绘制侧按需取图那一步）。
    ///
    /// 目标值：**阅读一本插图尺寸正常的书，这一格是 0**。索引里记着固有尺寸
    /// （`chapters`/`images` 那两张表），没超 atlas 上限的图根本不必解码
    /// （见 `ui::shared::images::ImageEntry::fits_the_device_limits`）；
    /// 它一旦涨起来，就是「每滚进一张图就顿一下」那条老路又回来了。
    ImageShrinks,
    /// 排版键的两格输入哈希算了几次（`layout::LayoutInputs::compute`）。
    ///
    /// 那两格是「整本书的 CSS 走一遍 SipHash」与「整份 `Reader` 用 `serde_json`
    /// 序列化一遍再哈希」。它们**只该在输入真的变了时算**：样式表开书时读一次，
    /// 偏好只在 `set_prefs` 变。
    ///
    /// 目标值：**滚动与重排过程中这一格完全不涨**。它的反面就是这次改动之前的形状 ——
    /// `Document::layout_key()` 每帧为链上每个章各调一次，而每次都把这两趟重走一遍。
    /// 门是 `layout::tests::cache::the_layout_inputs_hash_once`。
    LayoutInputHashes,
    /// 「哪些块要画装饰」那张表被**重折**了几次（`Layout::decor_seeds`）。
    ///
    /// 那张表只随排版结果变（底色与边框来自层叠结果），所以合法的重折时机只有
    /// 「排完一章拿到新一代结果」那一次。折一次要走**整章的块表**，而从前它是
    /// 每帧、链上每个章各走一次 —— `plans/阅读管线.md` §十一 第 12 条。
    ///
    /// 目标值：**滚动过程中这一格完全不涨**；改一次字号 / 换一次章才 +1。
    /// 门是 `reader::document::tests::scrolling_never_rebuilds_the_decor_table`。
    DecorRebuilds,
    /// 选中/批注矩形那一趟**走了几行**（`content::hit::rects_for_range_in` 的外层循环）。
    ///
    /// 为什么这一格要数：批注回显是「每条批注 × 每帧」各问一次，而每一条从前都把
    /// **整章的行**走一遍再让调用方按 y 丢掉 —— 一章 25 条批注实测每帧 2.0 ms
    /// （`plans/阅读管线.md` §十一 第 9 条）。窄化生效时这一格应该是「与选区相交的
    /// 那几行 ∩ 可见带那几行」，量级是个位数；它涨到「章行数 × 批注条数」那一档，
    /// 就是索引或可见带回退到了整章扫。
    ///
    /// 门是 `reader::document::tests::a_banded_query_walks_only_the_visible_lines`。
    RectLines,
    /// 书的**作者样式表**被解析了几次（`RuleSet::parse`，`reader::document::author_rules`）。
    ///
    /// 那一整本 CSS 与每一章都配同一份规则，所以它只该在开书时解析一次。从前
    /// `Document::with_source` 每建一章就重解析一遍：2026-10-15 真机拆开量到
    /// **每进一章 1.5–1.9 ms**（《未来简史》23.6 KB / 323 条规则，`RuleSet::parse`
    /// 单独 1.16 ms），跳章一次铺 4 章就是 7 ms 落在 UI 线程的一帧里。
    ///
    /// 目标值：**打开一本书是 1**，滚动、跳章、改偏好都不涨。
    /// 门是 `reader::document::tests::the_author_stylesheet_is_parsed_once_per_book`。
    RuleSetParses,
    /// 正文插图在后台起了几趟「现取字节 → 必要时缩小 → 交 gpui 解码」。
    ///
    /// 为什么要数：一张图进图集要占的是**它自己的原始像素**（`width × height × 4`），
    /// 实测《未来简史》55 张全部进过一次就是 **126.8 MB**，而 gpui 那条
    /// `ImageSource::Image` 的路没有任何回收口 —— 于是那一百多兆永久压在显存里
    /// （`plans/阅读管线.md` §十四）。现在改成界面自己持有解好的 `RenderImage`，
    /// 图离开可见带就把 atlas 那一格放掉，代价是**回屏要重解一次**。
    /// 这一格数的就是那笔代价：它等于「进过可见带的次数」而不是「书的图数」，
    /// 反复读同一屏应当一次都不涨。
    ///
    /// 门是 `reader::image_atlas::tests`：在场期间不涨（每帧重解就是红的），
    /// 回收后回屏只 +1。
    AtlasDecodes,
    /// 回收掉的图集字节累计量（按每格的真像素数记）。
    ///
    /// 这一格是「回收这件事到底有没有发生」的唯一直接证据：它为零就是**一件都没放掉**，
    /// 而屏幕上看不出任何区别 —— 图照样画得出来，只是显存一路涨。
    /// 关掉 `ImageAtlas::end_frame` 那一步，`a_tile_leaves_the_atlas_...` 当场红。
    ///
    /// 口径与 `AtlasDecodes` 一对：一个记放掉了多少，一个记为此重解了几趟。
    AtlasFreed,
    /// 7z / RAR / tar 漫画包**真解了一遍**的次数（`format::comic_cache::ensure_*` 里
    /// 缓存未命中那一支）。
    ///
    /// 为什么数它：这三种容器只能顺序解压，第一次打开要把整本图片落到磁盘 —— 那是
    /// 「读一本漫画」里最贵的一笔，比解一张图贵两个量级，所以它必须**每本只付一次**。
    /// 导入那一步已经解过一遍而开书时再解一遍，屏幕上看不出任何区别，只有第一次翻页
    /// 会卡在那里；这一格就是那条重复解压唯一能被抓到的地方。
    ///
    /// 目标值：**导入一本漫画并打开它，这一格是 1**。它涨到 2 就是缓存键在改名那一趟
    /// 换了来源（`ingest::import` 的 `relocate` 断了）。
    ComicExtracts,
}

/// 计数器条目的存储顺序。新增 [`Metric`] 变体时必须同步扩这两个数组，否则下面的
/// `ALL` 数组长度对不上，编译期就会报错——这是刻意的：让"漏加一个计数器"成为
/// 编译错误而不是运行时的零。
const ALL: [Metric; 15] = [
    Metric::ContainerOpens,
    Metric::EntryNameTablePasses,
    Metric::DomParses,
    Metric::SceneRebuilds,
    Metric::SceneRuns,
    Metric::ShapeLineCalls,
    Metric::WholeBookRereads,
    Metric::ImageShrinks,
    Metric::LayoutInputHashes,
    Metric::DecorRebuilds,
    Metric::RectLines,
    Metric::RuleSetParses,
    Metric::AtlasDecodes,
    Metric::AtlasFreed,
    Metric::ComicExtracts,
];

thread_local! {
    static LOCAL: [Cell<u64>; 15] = const { [const { Cell::new(0) }; 15] };
}

static GLOBAL: [AtomicU64; 15] = [const { AtomicU64::new(0) }; 15];

impl Metric {
    fn slot(self) -> usize {
        match self {
            Metric::ContainerOpens => 0,
            Metric::EntryNameTablePasses => 1,
            Metric::DomParses => 2,
            Metric::SceneRebuilds => 3,
            Metric::SceneRuns => 4,
            Metric::ShapeLineCalls => 5,
            Metric::WholeBookRereads => 6,
            Metric::ImageShrinks => 7,
            Metric::LayoutInputHashes => 8,
            Metric::DecorRebuilds => 9,
            Metric::RectLines => 10,
            Metric::RuleSetParses => 11,
            Metric::AtlasDecodes => 12,
            Metric::AtlasFreed => 13,
            Metric::ComicExtracts => 14,
        }
    }

    /// 记一次。用 [`Default`] 之外的显式函数名，是为了让调用点在 diff 里一眼可辨。
    pub fn count(self) {
        let i = self.slot();
        LOCAL.with(|c| c[i].set(c[i].get() + 1));
        GLOBAL[i].fetch_add(1, Ordering::Relaxed);
    }

    /// 一次记 `n` 个。给**量级大过自身开销**的那类计数用（模块头那条纪律的另一半）：
    /// 一段事做完再记一笔，而不是逐项各记一次。
    ///
    /// 用在「这一趟场景构造一共物化了几个 run」这种**结果本身是个数**的地方——逐项计数
    /// 会让量具变成被测量的开销，而 `add(runs.len())` 只多两条加法。
    pub fn add(self, n: u64) {
        if n == 0 {
            return;
        }
        let i = self.slot();
        LOCAL.with(|c| c[i].set(c[i].get() + n));
        GLOBAL[i].fetch_add(n, Ordering::Relaxed);
    }
}

/// 本线程从上一清零至今的计数。测试用这个，不要用 [`global`]。
pub fn local(metric: Metric) -> u64 {
    LOCAL.with(|c| c[metric.slot()].get())
}

/// 读并清零，返回读到的值。用在「做完一段事，数一下这段事里发生了几次」的断言里，
/// 免得测试之间因为同线程复用而互相累计。
pub fn take_local(metric: Metric) -> u64 {
    LOCAL.with(|c| {
        let slot = &c[metric.slot()];
        slot.replace(0)
    })
}

/// 整个进程的累计量，给诊断输出用。
pub fn global(metric: Metric) -> u64 {
    GLOBAL[metric.slot()].load(Ordering::Relaxed)
}

/// 全部计数器的当前值，按声明顺序。诊断 dump 直接把它打出来，不需要调用方知道有几个。
pub fn snapshot() -> Vec<(&'static str, u64)> {
    ALL.iter().map(|m| (label(*m), global(*m))).collect()
}

/// 给人看的计数器名（英文、下划线，因为它会进日志与 dump，不进文案表）。
pub fn label(metric: Metric) -> &'static str {
    match metric {
        Metric::ContainerOpens => "container_opens",
        Metric::EntryNameTablePasses => "entry_name_table_passes",
        Metric::DomParses => "dom_parses",
        Metric::SceneRebuilds => "scene_rebuilds",
        Metric::SceneRuns => "scene_runs",
        Metric::ShapeLineCalls => "shape_line_calls",
        Metric::WholeBookRereads => "whole_book_rereads",
        Metric::ImageShrinks => "image_shrinks",
        Metric::LayoutInputHashes => "layout_input_hashes",
        Metric::DecorRebuilds => "decor_rebuilds",
        Metric::RectLines => "rect_lines",
        Metric::RuleSetParses => "rule_set_parses",
        Metric::AtlasDecodes => "atlas_decodes",
        Metric::AtlasFreed => "atlas_freed",
        Metric::ComicExtracts => "comic_extracts",
    }
}

#[cfg(test)]
mod tests {
    use super::{ALL, Metric, label};

    /// 计数按线程隔离：这正是测试能断增量的前提。
    ///
    /// 这条断言同时也是给下一个读这段代码的人的解释：若在别的线程上也数得到，
    /// 那测试里读局部值就会看到别的测试的活动，断言从此不可信。
    #[test]
    fn counts_are_per_thread_so_parallel_tests_do_not_pollute_each_other() {
        let metric = Metric::ContainerOpens;
        metric.count();
        let mine = crate::infra::metrics::take_local(metric);
        assert_eq!(mine, 1, "本线程记了一次就该读到一次");

        let from_other_thread = std::thread::spawn(move || {
            Metric::ContainerOpens.count();
            crate::infra::metrics::local(Metric::ContainerOpens)
        })
        .join()
        .unwrap();
        assert_eq!(
            from_other_thread, 1,
            "新线程从自己的零开始，不继承别人的计数"
        );
    }

    #[test]
    fn take_local_reads_and_resets_in_one_step() {
        let metric = Metric::EntryNameTablePasses;
        metric.count();
        metric.count();
        assert_eq!(crate::infra::metrics::take_local(metric), 2);
        assert_eq!(crate::infra::metrics::local(metric), 0, "读过就该归零");
    }

    /// [`Metric::add`] 与逐项 [`Metric::count`] 必须落进**同一个格子**：量具读的是那一个
    /// 槽位，走岔了就是一条永远为零的假绿。零不许占一次（`Scene::build` 交出空场景时
    /// 不该在 `scene_runs` 上留一笔），也不许动全局量。
    #[test]
    fn add_accumulates_into_the_same_slot_as_count() {
        let metric = Metric::SceneRuns;
        let before = crate::infra::metrics::global(metric);
        metric.add(0);
        assert_eq!(
            crate::infra::metrics::take_local(metric),
            0,
            "记零次不该留下任何东西"
        );
        assert_eq!(
            crate::infra::metrics::global(metric),
            before,
            "记零次也不该动全局量"
        );
        metric.add(3);
        metric.count();
        assert_eq!(
            crate::infra::metrics::take_local(metric),
            4,
            "一次记 3 加一次记 1，读出来是 4"
        );
    }

    #[test]
    fn global_totals_accumulate_across_threads() {
        let before = crate::infra::metrics::global(Metric::ContainerOpens);
        std::thread::spawn(|| Metric::ContainerOpens.count())
            .join()
            .unwrap();
        assert!(
            crate::infra::metrics::global(Metric::ContainerOpens) > before,
            "全局量应当把别的线程也算进来"
        );
    }

    /// 快照不许漏计数器：新增变体却忘了登记数组时，这条会红。
    #[test]
    fn snapshot_lists_every_metric() {
        let names = crate::infra::metrics::snapshot();
        // 与 `ALL` 比长度而不是比一个写死的数：写死的数会在加计数器时变成一条假绿。
        assert_eq!(
            names.len(),
            ALL.len(),
            "每个 Metric 变体都该在 snapshot 里有一行"
        );
        for metric in ALL {
            assert!(
                names.iter().any(|(n, _)| *n == label(metric)),
                "{:?} 没登记标签",
                metric
            );
        }
    }
}
