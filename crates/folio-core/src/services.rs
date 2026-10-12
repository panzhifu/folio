//! 常驻服务与作业池的**收口**：一个进程里就这一处，管住「谁先谁后关」。
//!
//! 三样东西此前各挂各的：导入作业池（[`crate::task::TaskManager`]）、文件夹监听
//! （[`crate::watch::WatchService`]）、传书服务（[`crate::transfer::TransferServer`]）。
//! 散着放时关停次序没人负责，而次序是有后果的 —— 见 `plans/基础设施.md` §1.3 / §5.6：
//!
//! 1. **常驻服务不是作业的一部分**，它是**往池里塞活的驱动者**。所以先停服务，
//!    就没有新的作业再进队列；
//! 2. **关停的语义是「跑完已排的」**，不是「立刻停」：`TaskManager::shutdown` 幂等、
//!    会把手上与队里的作业全部跑完再回收线程（`task.rs`）；
//! 3. **作业池必须先于数据库连接销毁**，否则后台线程会拿着已失效的连接写。
//!
//! 因此 [`Services::shutdown`] 的顺序是死的：**停驱动者 → 关作业池 → 放掉本结构持有的
//! 最后一个书库句柄**。第三样是这里持有 [`Store`] 的唯一理由 —— 别的界面各自拿的是
//! `Arc<Store>` 的 clone，那些随它们自己的作用域 drop；本结构活得比它们久，所以最后
//! 一个句柄落在它手上，库的关闭就排在作业池之后。
//!
//! 本模块不认识 gpui（与 core 其余部分一致）：它只提供「注册句柄 + 有序关停」，
//! 「何时起、何时停」由界面侧决定。

use std::sync::Arc;
use std::sync::{Mutex, PoisonError};

use crate::store::Store;
use crate::task::TaskManager;
use crate::transfer::TransferServer;
use crate::watch::WatchService;

/// 一个进程的常驻资源：一条导入作业池、两个长驻服务、一个书库句柄。
pub struct Services {
    pool: Arc<TaskManager>,
    watch: Mutex<Option<WatchService>>,
    transfer: Mutex<Option<TransferServer>>,
    /// 只为「最后关库」而持有（见模块文档第 3 条）。
    store: Mutex<Option<Arc<Store>>>,
}

impl Services {
    /// 用一条作业池建起来。池是别的服务的共用去处，所以由调用方建好后交进来。
    pub fn new(pool: Arc<TaskManager>) -> Self {
        Self {
            pool,
            watch: Mutex::new(None),
            transfer: Mutex::new(None),
            store: Mutex::new(None),
        }
    }

    /// 共用的导入作业池。watch / transfer / 手选导入都从这里取，**只有这一份**。
    pub fn pool(&self) -> &Arc<TaskManager> {
        &self.pool
    }

    /// 记一份书库句柄，把「关库」排到 [`Self::shutdown`] 的最后。
    pub fn attach_store(&self, store: Arc<Store>) {
        *self.store.lock().unwrap_or_else(PoisonError::into_inner) = Some(store);
    }

    /// 换一只监听服务：旧的先停再存新的；`None` 就是停掉不再起。
    pub fn set_watch(&self, service: Option<WatchService>) {
        let mut slot = self.watch.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = slot.take() {
            old.stop();
        }
        *slot = service;
    }

    /// 换一只传书服务：旧的先停再存新的；`None` 就是停掉。
    pub fn set_transfer(&self, service: Option<TransferServer>) {
        let mut slot = self.transfer.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = slot.take() {
            old.stop();
        }
        *slot = service;
    }

    /// 借当前这只传书服务读一下对外信息（地址、配对码）。没起就是 `None`。
    pub fn with_transfer<R>(&self, f: impl FnOnce(&TransferServer) -> R) -> Option<R> {
        let slot = self.transfer.lock().unwrap_or_else(PoisonError::into_inner);
        slot.as_ref().map(f)
    }

    /// §1.3 第一步：停掉两个「驱动者」，不再往池里塞新活。
    ///
    /// 已经排进池里的作业不受影响 —— 那是下一步 `shutdown` 的事（「跑完已排的」）。
    pub fn stop_drivers(&self) {
        self.set_watch(None);
        self.set_transfer(None);
    }

    /// 完整关停序列，幂等：**先停投递 → 关作业池 → 最后放掉书库句柄**。
    ///
    /// 会阻塞到两个服务线程与池里已排的作业全部收尾（见模块文档）。所以别在正被
    /// 这些线程占用的路径上调它 —— 只能在「不再需要它们」的退出路径上。
    pub fn shutdown(&self) {
        self.stop_drivers();
        self.pool.shutdown();
        // 放掉最后一份书库句柄：到这里后台线程已经全 join 完，不会再有别人拿着它写。
        let _ = self
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}

#[cfg(test)]
mod tests {
    //! 只验关停顺序里能脱离 gpui 观察到的两件事：池被 join、已排的作业跑完。

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::Services;
    use crate::import::ImportOutcome;
    use crate::task::TaskManager;

    fn pool(workers: usize) -> Arc<TaskManager> {
        Arc::new(TaskManager::new(workers))
    }

    /// 关停后工作线程被回收（`shutdown` 真的 join 了池）。
    #[test]
    fn shutdown_joins_the_pool() {
        let pool = pool(3);
        assert_eq!(pool.workers(), 3);
        let services = Services::new(Arc::clone(&pool));
        services.shutdown();
        assert_eq!(pool.workers(), 0, "关停后池里不该还留着工作线程");
    }

    /// 「跑完已排的」：队列里已经排着的作业在 `shutdown` 返回前必须执行完。
    #[test]
    fn queued_jobs_finish_before_shutdown_returns() {
        let ran = Arc::new(AtomicUsize::new(0));
        let services = Services::new(pool(1));
        for _ in 0..4 {
            let counter = Arc::clone(&ran);
            services.pool().submit(move |_stage| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::<ImportOutcome>::new())
            });
        }
        services.shutdown();
        assert_eq!(ran.load(Ordering::SeqCst), 4, "已排的作业要全部跑完");
    }

    /// 幂等：重复关停不出错、也不 panic。
    #[test]
    fn shutdown_is_idempotent() {
        let services = Services::new(pool(2));
        services.shutdown();
        services.shutdown();
    }

    /// 没有起任何服务时，停驱动者只是空操作。
    #[test]
    fn stopping_drivers_without_any_is_a_noop() {
        let services = Services::new(pool(1));
        services.stop_drivers();
        assert!(services.with_transfer(|_| ()).is_none());
    }
}
