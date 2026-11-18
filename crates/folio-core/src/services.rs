//! 常驻服务的有序关停：先停监听/传书，再排空作业池，最后放掉所持书库句柄。
//! 保留 Arc<Store> 保证后台作业结束前连接仍有效；UI 决定启动与退出时机。

use std::sync::Arc;
use std::sync::{Mutex, PoisonError};

use crate::infra::task::TaskManager;
use crate::ingest::watch::WatchService;
use crate::net::transfer::TransferServer;
use crate::store::Store;

/// 一个进程共用的作业池、监听服务、传书服务与书库句柄。
pub struct Services {
    pool: Arc<TaskManager>,
    watch: Mutex<Option<WatchService>>,
    transfer: Mutex<Option<TransferServer>>,
    store: Mutex<Option<Arc<Store>>>,
}

impl Services {
    /// 使用调用方提供的共享作业池。
    pub fn new(pool: Arc<TaskManager>) -> Self {
        // 格式层只使用宿主注入的缓存根目录。
        folio_format::comic_cache::set_root_resolver(crate::infra::paths::comics_cache_dir);
        Self {
            pool,
            watch: Mutex::new(None),
            transfer: Mutex::new(None),
            store: Mutex::new(None),
        }
    }

    /// 手工导入、监听与传书共用此池。
    pub fn pool(&self) -> &Arc<TaskManager> {
        &self.pool
    }

    /// 持有书库句柄，直到关停完成。
    pub fn attach_store(&self, store: Arc<Store>) {
        *self.store.lock().unwrap_or_else(PoisonError::into_inner) = Some(store);
    }

    /// 先停止旧服务，再保存新服务；None 表示停止。
    pub fn set_watch(&self, service: Option<WatchService>) {
        let mut slot = self.watch.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = slot.take() {
            old.stop();
        }
        *slot = service;
    }

    /// 先停止旧服务，再保存新服务；None 表示停止。
    pub fn set_transfer(&self, service: Option<TransferServer>) {
        let mut slot = self.transfer.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = slot.take() {
            old.stop();
        }
        *slot = service;
    }

    /// 持锁读取传书状态；未启动时返回 None。
    pub fn with_transfer<R>(&self, f: impl FnOnce(&TransferServer) -> R) -> Option<R> {
        let slot = self.transfer.lock().unwrap_or_else(PoisonError::into_inner);
        slot.as_ref().map(f)
    }

    /// 停止投递新作业，不取消池内已排队的工作。
    pub fn stop_drivers(&self) {
        self.set_watch(None);
        self.set_transfer(None);
    }

    /// 幂等地排空作业后关停。此调用会阻塞，不能在被回收的工作线程内调用。
    pub fn shutdown(&self) {
        self.stop_drivers();
        self.pool.shutdown();
        let _ = self
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::Services;
    use crate::infra::task::TaskManager;
    use crate::ingest::import::ImportOutcome;

    fn pool(workers: usize) -> Arc<TaskManager> {
        Arc::new(TaskManager::new(workers))
    }

    #[test]
    fn shutdown_joins_the_pool() {
        let pool = pool(3);
        assert_eq!(pool.workers(), 3);
        let services = Services::new(Arc::clone(&pool));
        services.shutdown();
        assert_eq!(pool.workers(), 0, "关停后池里不该还留着工作线程");
    }

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

    #[test]
    fn shutdown_is_idempotent() {
        let services = Services::new(pool(2));
        services.shutdown();
        services.shutdown();
    }

    #[test]
    fn stopping_drivers_without_any_is_a_noop() {
        let services = Services::new(pool(1));
        services.stop_drivers();
        assert!(services.with_transfer(|_| ()).is_none());
    }
}
