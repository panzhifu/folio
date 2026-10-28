//! 常驻资源在 app 侧的**一个句柄**：导入作业池、监听、传书，全归它管。
//!
//! 为什么需要一个 `static` 而不是 gpui 的 `Global`：关停次序要**跨过 `App` 的生命周期**。
//! gpui 在 `App` 退出时才 drop 全局，而「作业池先于库连接销毁」这条要求关停排在
//! 全局 drop **之前**、且关停本身会阻塞（join 服务线程与池里已排的作业）。所以：
//!
//! - 资源活在一个**进程级 `static`** 里，活得比 `App` 久；
//! - 收尾由平台入口在 `run(...)` 返回后显式 [`shutdown()`]（见 `plans/基础设施.md` §1.3/§5.6），
//!   **不走** `cx.on_app_quit` —— 那条路只留 100ms，做不了阻塞收尾；
//! - 本结构在 core 里持有一份书库句柄，使「join 作业池 → 再关库」的顺序成立。
//!
//! 起停的**编排**留在界面侧（读配置、活过界面切换），服务的**实现**在 core；
//! 这里只是把两者的句柄收拢到一处。

use std::sync::{Arc, OnceLock};

use folio_core::{Services, TaskManager};
use gpui_kit::App;

use super::app_store;

static SERVICES: OnceLock<Arc<Services>> = OnceLock::new();

/// 进程级的常驻资源。第一次取用时惰性建好（只有作业池，没有服务/书库）——
/// 没走 boot 的测试也能照常用池，不必先装一套全局。
pub fn services() -> &'static Arc<Services> {
    SERVICES.get_or_init(|| Arc::new(Services::new(Arc::new(TaskManager::new(pool_workers())))))
}

/// 作业池给几条 worker：**手机 3 条、桌面 6 条**（计划 §二 A「池规模」那一档）。
///
/// 从前是 2 条，那笔账是给导入算的（导入落库档封顶 1、整机扫描预热档各占一条就够）。
/// 读者翻页的栅格化搬进这个池之后它就不够了：一页 PDF 是几十到几百 ms，2 条线程等于
/// 「当前页 + 一页预取」把池占满，翻过去那一下照样排队 —— 而 [`Priority::cap`] 的
/// 交互/预热各 4 那一档封顶，在只有 2 条 worker 的池里**恒为 2**，压根挡不出它想挡的形状。
///
/// - 桌面 6 = 交互封顶 4 + 导入、扫描各占 1 的量。
/// - 手机 3：并行画几页，**峰值内存按并行数涨**（一页 2048×1448 的画布是 11.8 MB，
///   三条同时在跑就是 35 MB 的瞬时量，而这台机器前台静置已经 151 MB）。三页并行已经
///   盖住「当前页 + 前后各一页预取」，再多买不到响应、只买得到内存。
///
/// 计划里那一档「平板 4」没有单列：平板的判据是窗口逻辑宽 ≥ 1000 pt，而这个池在
/// **开窗之前**就要建好（`resident::install(cx)` 排在 `open_window` 之前，见
/// `platform::desktop` 的启动次序），那时窗口还不存在。宽窗口机器与桌面同档；
/// 真正的并行度仍由 `Priority::cap` 那两张封顶卡住，多出来的 worker 只在导入/扫描
/// 同时在跑时才占得上。
fn pool_workers() -> usize {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        3
    } else {
        6
    }
}

/// 共用的作业池。导入 / 扫描 / watch / transfer 都取这一份（§1.1「一条队列」）。
pub fn pool() -> Arc<TaskManager> {
    Arc::clone(services().pool())
}

/// 作业池；没装常驻资源时给 `None`（**不**惰性建一个）。
///
/// 读者翻页时的栅格化用它：有池就排队，没池（纯视图测试）就就地跑 —— 那种场景里
/// 「起 6 条线程」比那一趟活本身还贵，而且测试结束时它们没人 join。
pub fn pool_maybe() -> Option<Arc<TaskManager>> {
    SERVICES.get().map(|services| Arc::clone(services.pool()))
}

/// 把当前书库句柄交给常驻资源：关停时由它放掉最后一份，库的关闭因此排在作业池之后。
///
/// 每个入口（`super::super::platform`）在装好书库之后、开窗前调一次。
pub fn install(cx: &App) {
    if let Some(store) = app_store::handle(cx) {
        services().attach_store(store);
    }
}

/// 有序收尾：停监听/传书 → 关作业池 → 放掉最后一份书库句柄。
///
/// 由平台入口在 `run(...)` 返回后调用（桌面 `desktop.rs`、Android `android.rs`）。
/// iOS / HarmonyOS 的入口没有这个返回点，那边只能靠进程回收 —— 见 `platform` 的模块文档。
pub fn shutdown() {
    tracing::info!("services: 开始有序收尾");
    services().shutdown();
    tracing::info!("services: 收尾完成（监听/传书已停、作业池已关、书库句柄已放）");
}
