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
    /// 场景构造**物化**出来的 run 数（`SharedString` 从 `BlockBox::text` 拷出来那几个）。
    /// 一次 `Scene::build` 记一笔、值 = `runs.len()`（见模块头那条纪律）。
    ///
    /// 目标值：**滚下去再滚回同一屏，第二趟这一格增量 0**。单元表按行**懒物化**，某一行
    /// 第一次进带必须物化它，所以「往前滚为 0」是个错的口径（2026-10-13 立门时就这么错过一次，
    /// 见 `plans/阅读管线.md` §九 第四条纪律）；回到走过的地方才是那个确定的零。
    ///
    /// 根子在 `GlyphRun.x/baseline` 存的是**相对元素原点的屏幕值**：滚动一动每条 run 都变，
    /// 场景只能整份重造。修法是「按行号索引的行单元表（章内局部量）+ 每帧一个平移量」。
    SceneRuns,
    /// `TextSystem::shape_line` 被调了几次（`paint_run` 每 run 一次）。
    ///
    /// gpui 自己有 `LineLayoutCache`，命中时不重 shaping，但**键含整段文本**——命中也要
    /// 把文本哈希一遍，而那个哈希是 O(文本长度)。所以这一格测的不是「有没有重新 shaping」，
    /// 是「绘制回调每帧替读者算了多少趟查表」。**它不进上面那条往返门**：把 `ShapedLine`
    /// 存进行单元表是已记为「可省」的下一步（省的是那趟哈希，不是那 62 次拷贝），
    /// 拿它当验收会把一件正确的事钉成错的。
    ShapeLineCalls,
}

/// 计数器条目的存储顺序。新增 [`Metric`] 变体时必须同步扩这两个数组，否则下面的
/// `ALL` 数组长度对不上，编译期就会报错——这是刻意的：让"漏加一个计数器"成为
/// 编译错误而不是运行时的零。
const ALL: [Metric; 6] = [
    Metric::ContainerOpens,
    Metric::EntryNameTablePasses,
    Metric::DomParses,
    Metric::SceneRebuilds,
    Metric::SceneRuns,
    Metric::ShapeLineCalls,
];

thread_local! {
    static LOCAL: [Cell<u64>; 6] = const { [const { Cell::new(0) }; 6] };
}

static GLOBAL: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

impl Metric {
    fn slot(self) -> usize {
        match self {
            Metric::ContainerOpens => 0,
            Metric::EntryNameTablePasses => 1,
            Metric::DomParses => 2,
            Metric::SceneRebuilds => 3,
            Metric::SceneRuns => 4,
            Metric::ShapeLineCalls => 5,
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
