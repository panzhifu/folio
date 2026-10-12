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
///
/// 池给 **2 条** worker：导入（落库档，封顶 1）与整机扫描（预热档）能各占一条并行，
/// 而导入之间仍靠「落库封顶 1」保持串行 —— 见 `TaskManager` 的分档与封顶（§1.1）。
pub fn services() -> &'static Arc<Services> {
    SERVICES.get_or_init(|| Arc::new(Services::new(Arc::new(TaskManager::new(2)))))
}

/// 共用的作业池。导入 / 扫描 / watch / transfer 都取这一份（§1.1「一条队列」）。
pub fn pool() -> Arc<TaskManager> {
    Arc::clone(services().pool())
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
