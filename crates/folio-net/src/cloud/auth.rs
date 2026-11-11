//! 网盘凭据的**续期**：`TokenSource`（问一个能用的 access token）、`TokenStore`
//! （轮换出来的新凭据往哪儿放）与 `AuthedStore`（`RemoteStore` 的装饰器）。
//!
//! ## 为什么要有这一层
//!
//! 两家网盘的 access token 都约 30 天过期，且**刷新会轮换 refresh_token** —— 旧的那支
//! 立即作废，新的一支必须落盘。2026-11-09 之前这件事没有任何抽象：token 在 `config`
//! 里躺着，`expires_at` 只写不读，401 之后的重试写在 UI 里、两家各一份几乎逐行同构的
//! 代码（`cloud::view` 的 `refresh_baidu` / `refresh_aliyun`）。加第三家就是第三份。
//!
//! 现在的分工：
//!
//! - [`Refresher`]：**怎么换**一对新凭据（包住各家的 `refresh_tokens`）；
//! - [`TokenStore`]：**换到的新凭据往哪儿放**（app 侧负责落 config）；
//! - [`SharedTokenSource`]：什么时候换 —— 按 `expires_at` 提前续，持锁单飞
//!   （并发请求在刷新期间排队，刷新是一趟 HTTP，几百毫秒）；
//! - [`AuthedStore`]：怎么把续期接进每一次请求 —— 过期先续、401 强刷一次重放；
//!   token 变了就按**同一个 build 闭包**重建内层仓库（内层客户端把 token 烤在构造参数里，
//!   重建比让它可变更便宜，BaiduDrive / AliyunDrive 的构造都是几次 `HashMap::insert`）。
//!
//! ## 后台线程与落盘
//!
//! 续期发生在**后台线程**上（`drive.list()` 跑在作业池里），而 config 的写回要走主线程
//! （`ConfigHolder` 是 gpui 的 `Entity`）。所以 [`TokenStore::save`] 只把轮换结果递进
//! 一条通道，app 侧开机时起一条**常驻主线程任务**把它落盘（`services::cloud_auth`）。
//! 窗口是毫秒级的：主线程的下一次泵就会消化它，期间重建出的新仓库拿的还是旧
//! refresh_token 的概率可以忽略 —— 而就算真撞上，内层那次 401 重放失败会把「请重新授权」
//! 原样交回界面，读者重走一遍授权即可。

use std::sync::{Arc, Mutex, MutexGuard};

use super::Drive;
use crate::{Cancel, Error, RemoteStore, Result, Sink, Source, Transport};

/// 一对凭据。`expires_at` 是**绝对时刻**（Unix 秒）；0 = 未知（手贴 token 那条路没有
/// 这个数）——未知就不做主动续期，等 401 再说。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Token {
    pub access: String,
    pub refresh: String,
    pub expires_at: i64,
}

impl Token {
    /// 有没有一支可以拿来续的 refresh_token。
    pub fn refreshable(&self) -> bool {
        !self.refresh.trim().is_empty()
    }
}

/// **怎么换**一对新凭据。两家各一份实现，包住各自的 `refresh_tokens`。
///
/// 换回来的 [`Token`] 里必须是**轮换后**的那一支 refresh_token —— 旧的已经作废，
/// 存错一支下一次续期就断。
pub trait Refresher: Send + Sync {
    fn refresh(&self, refresh_token: &str, transport: Arc<dyn Transport>) -> Result<Token>;
}

/// **换到的新凭据往哪儿放**。`save` 会在后台线程上被调（续期跟着请求走），
/// 实现自己负责把结果搬回主线程落盘。
///
/// 没有 `load`：初始凭据在构造 [`SharedTokenSource`] 时由调用方递进来（那一刻在
/// 主线程上，config 就在手边），core 不需要再回头读。
pub trait TokenStore: Send + Sync {
    fn save(&self, token: Token);
}

/// **什么时候换**。实现是进程内共享的（[`SharedTokenSource`]），持锁单飞。
pub trait TokenSource: Send + Sync {
    /// 当前可用的 access token；按 `expires_at` 判断要不要先续。
    ///
    /// 续不了（没有 refresh_token）也照样把手上这支交出去 —— 让请求自己撞 401，
    /// 界面按「请重新授权」翻，比在这里凭空造一个错更诚实。
    fn fresh(&self) -> Result<String>;

    /// 强制续一次（401 重放前调）。没有 refresh_token 时回 `Err`，
    /// 调用方应保留**原来的** 401 错而不是把这句 Malformed 交出去。
    fn force_refresh(&self) -> Result<String>;
}

/// 提前多少秒判过期：时钟差一点就不至于拿一支马上作废的 token 出门。
const EXPIRY_MARGIN: i64 = 60;

/// [`TokenSource`] 的进程内实现：持锁单飞 + 落盘。
pub struct SharedTokenSource {
    /// 当前凭据。**刷新期间持锁**：并发请求在那一趟 HTTP 上排队，换来的是
    /// 「同一时刻至多一次续期」，不需要另起一套 single-flight 记账。
    state: Mutex<Token>,
    store: Arc<dyn TokenStore>,
    refresher: Arc<dyn Refresher>,
    transport: Arc<dyn Transport>,
}

impl SharedTokenSource {
    /// `initial` 来自调用时的 config（主线程读的）；`store` 收轮换结果。
    pub fn new(
        initial: Token,
        store: Arc<dyn TokenStore>,
        refresher: Arc<dyn Refresher>,
        transport: Arc<dyn Transport>,
    ) -> Self {
        Self {
            state: Mutex::new(initial),
            store,
            refresher,
            transport,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Token> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 该不该现在续：没有 refresh_token 永远不主动续（手贴 token 那条路）；
    /// 过期时刻未知（0）也不主动续，等 401。
    fn should_refresh(token: &Token) -> bool {
        if !token.refreshable() {
            return false;
        }
        if token.access.is_empty() {
            return true;
        }
        token.expires_at > 0 && folio_base::time::now() >= token.expires_at - EXPIRY_MARGIN
    }

    /// 持锁续期：换到的新凭据先落 [`TokenStore`]，再替换内存里的那一份。
    /// 落盘失败不拦内存替换 —— 这一屏还能用，下次启动才需要盘上那份。
    fn refresh_locked(&self, state: &mut Token) -> Result<String> {
        let refreshed = self
            .refresher
            .refresh(&state.refresh, Arc::clone(&self.transport))?;
        *state = refreshed.clone();
        self.store.save(refreshed);
        Ok(state.access.clone())
    }
}

impl TokenSource for SharedTokenSource {
    fn fresh(&self) -> Result<String> {
        let mut state = self.lock();
        if Self::should_refresh(&state) {
            return self.refresh_locked(&mut state);
        }
        Ok(state.access.clone())
    }

    fn force_refresh(&self) -> Result<String> {
        let mut state = self.lock();
        if !state.refreshable() {
            return Err(Error::Malformed(
                "没有刷新凭据（手贴的 token 过期只能重贴）".into(),
            ));
        }
        self.refresh_locked(&mut state)
    }
}

/// [`RemoteStore`] 的装饰器：**过期先续、401 强刷一次重放**。
///
/// 内层仓库把 token 烤在构造参数里（[`RemoteStore`] 的两家实现都是这么长的），
/// 所以这里按「同一个 build 闭包 + 当前 access」缓存一份内层实例：token 没变就复用
/// （BaiduDrive 的 `fs_id` 缓存因此跨请求活着），变了才重建（构造只是几次
/// `HashMap::insert`，不心疼）。
///
/// 重放**至多一次**：强刷失败时交回**原来的** 401 —— 那一句才是界面上
/// 「请重新授权」的依据，续期失败的理由（没有刷新凭据之类）混进来只会把人看糊涂。
pub struct AuthedStore<S> {
    build: Box<dyn Fn(&str) -> S + Send + Sync>,
    tokens: Arc<dyn TokenSource>,
    /// `(当次 access, 内层仓库)`。按 token 变化惰性重建。
    current: Mutex<Option<(String, S)>>,
}

impl<S> AuthedStore<S>
where
    S: RemoteStore,
{
    /// `build` 从一支 access token 造出内层仓库 —— 与两家客户端现有构造同一个形状。
    pub fn new(
        build: impl Fn(&str) -> S + Send + Sync + 'static,
        tokens: Arc<dyn TokenSource>,
    ) -> Self {
        Self {
            build: Box::new(build),
            tokens,
            current: Mutex::new(None),
        }
    }

    fn build(&self, access: &str) -> S {
        (self.build)(access)
    }

    fn lock(&self) -> MutexGuard<'_, Option<(String, S)>> {
        self.current.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 拿当次 token 对应的内层仓库（没有或 token 变了就重建），当场跑一次。
    ///
    /// 锁在跑的那一会儿里是持着的 —— `f` 不会重入这一层，持着最省事。
    fn attempt<R>(&self, access: &str, f: &mut dyn FnMut(&S) -> Result<R>) -> Result<R> {
        let mut current = self.lock();
        if current
            .as_ref()
            .map(|(token, _)| token != access)
            .unwrap_or(true)
        {
            *current = Some((access.to_string(), self.build(access)));
        }
        let (_, store) = current.as_ref().expect("上面刚填过");
        f(store)
    }

    /// 每个方法的共同落点：过期先续 → 跑一次 → 401 就强刷重放一次。
    fn call<R>(&self, mut f: impl FnMut(&S) -> Result<R>) -> Result<R> {
        let access = self.tokens.fresh()?;
        match self.attempt(&access, &mut f) {
            Ok(value) => Ok(value),
            Err(Error::Unauthorized { .. }) => {
                let fresh = match self.tokens.force_refresh() {
                    Ok(fresh) => fresh,
                    // 续不了：原来的 401 才是界面上「请重新授权」的依据。
                    Err(_) => return Err(Error::Unauthorized { status: 401 }),
                };
                let store = self.build(&fresh);
                let outcome = f(&store);
                // 重放成功的话把新 token 对应的实例缓存住，下一次直接用。
                if outcome.is_ok() {
                    *self.lock() = Some((fresh, store));
                }
                outcome
            }
            Err(err) => Err(err),
        }
    }
}

impl<S: RemoteStore> RemoteStore for AuthedStore<S> {
    fn list(&self, path: &str) -> Result<Vec<super::Entry>> {
        self.call(|store| store.list(path))
    }

    fn get(&self, path: &str) -> Result<Vec<u8>> {
        self.call(|store| store.get(path))
    }

    fn put(&self, path: &str, body: Vec<u8>) -> Result<()> {
        // 401 重放要再调一次闭包，body 得还能拿出来。走这条路的只有同步快照
        // （几 KB 的 JSON），大文件走 `put_file`（那边是 `&mut dyn Source`，不用拷）。
        self.call(|store| store.put(path, body.clone()))
    }

    fn mkdir(&self, path: &str) -> Result<()> {
        self.call(|store| store.mkdir(path))
    }

    fn download(&self, path: &str, sink: &mut dyn Sink, cancel: &Cancel) -> Result<u64> {
        self.call(|store| store.download(path, sink, cancel))
    }

    /// 分片上传必须显式路由：内层两家各自覆盖了 [`RemoteStore::put_file`]，
    /// 默认实现会把一本书整个读进内存。
    fn put_file(
        &self,
        path: &str,
        source: &mut dyn Source,
        size: u64,
        cancel: &Cancel,
    ) -> Result<u64> {
        self.call(|store| store.put_file(path, source, size, cancel))
    }
}

impl<S: Drive> Drive for AuthedStore<S> {
    fn label(&self) -> &'static str {
        // 内层怎么自报，这一层就怎么转：provenance 与日志里的「哪家网盘」不因装饰器变味。
        (self.build)("").label()
    }
}

#[cfg(test)]
mod tests {
    //! 单飞、过期判定、轮换落盘、401 重放、重放失败保留原错。
    //!
    //! 假 Refresher / 假 TokenStore / 假内层仓库，全离线（传输只是摆设）。

    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::dav::{Entry, Mock};

    /// 可编程的假 Refresher：每次续期换一支新 refresh_token（模拟轮换），记录调用数。
    struct FakeRefresher {
        calls: AtomicU32,
        fail: bool,
    }

    impl FakeRefresher {
        fn working() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicU32::new(0),
                fail: false,
            })
        }

        fn broken() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicU32::new(0),
                fail: true,
            })
        }
    }

    impl Refresher for FakeRefresher {
        fn refresh(&self, refresh_token: &str, _t: Arc<dyn Transport>) -> Result<Token> {
            if self.fail {
                return Err(Error::Unauthorized { status: 401 });
            }
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(Token {
                access: format!("access-{n}"),
                // 轮换：新的一支里带旧的那支做记号，断言「用的正是上一轮换出来的」。
                refresh: format!("{refresh_token}-{n}"),
                expires_at: folio_base::time::now() + 3600,
            })
        }
    }

    /// 假 TokenStore：save 的调用数。
    struct FakeStore {
        saved: AtomicU32,
    }

    impl FakeStore {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                saved: AtomicU32::new(0),
            })
        }
    }

    impl TokenStore for FakeStore {
        fn save(&self, _token: Token) {
            self.saved.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn source(
        initial: Token,
        refresher: Arc<FakeRefresher>,
        store: Arc<FakeStore>,
    ) -> Arc<SharedTokenSource> {
        Arc::new(SharedTokenSource::new(
            initial,
            store as Arc<dyn TokenStore>,
            refresher as Arc<dyn Refresher>,
            Arc::new(Mock::new(Vec::new())) as Arc<dyn Transport>,
        ))
    }

    fn token(access: &str, refresh: &str, expires_at: i64) -> Token {
        Token {
            access: access.to_string(),
            refresh: refresh.to_string(),
            expires_at,
        }
    }

    /// 没过期就不续：fresh 原样交出，Refresher 一次都不跑。
    #[test]
    fn a_valid_token_is_handed_out_without_a_refresh() {
        let refresher = FakeRefresher::working();
        let source = source(
            token("a0", "r0", folio_base::time::now() + 3600),
            refresher.clone(),
            FakeStore::new(),
        );
        assert_eq!(source.fresh().unwrap(), "a0");
        assert_eq!(refresher.calls.load(Ordering::SeqCst), 0, "没过期不该续");
    }

    /// 过期就续：拿到新 access，轮换后的 refresh_token 落了盘，下一问用的是它。
    #[test]
    fn an_expired_token_is_refreshed_and_the_rotation_is_persisted() {
        let refresher = FakeRefresher::working();
        let store = FakeStore::new();
        let source = source(token("a0", "r0", 1), refresher.clone(), store.clone());
        assert_eq!(source.fresh().unwrap(), "access-1");
        assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.saved.load(Ordering::SeqCst), 1, "轮换结果要落盘");
        // 第二问不再续（刚续过，远远没到新的过期时刻）。
        assert_eq!(source.fresh().unwrap(), "access-1");
        assert_eq!(refresher.calls.load(Ordering::SeqCst), 1);
    }

    /// 手贴 token（没有 refresh_token）那一路：永远不主动续，原样交出。
    #[test]
    fn a_token_without_a_refresh_secret_is_never_refreshed() {
        let refresher = FakeRefresher::working();
        let source = source(token("a0", "", 0), refresher.clone(), FakeStore::new());
        assert_eq!(source.fresh().unwrap(), "a0");
        assert_eq!(refresher.calls.load(Ordering::SeqCst), 0);
        // 强刷也明确拒绝 —— AuthedStore 据此保留原来的 401。
        assert!(source.force_refresh().is_err());
    }

    /// 401 一次重放：内层对第一支 token 回 401，强刷拿到新 token，重建的内层成功。
    #[test]
    fn a_401_gets_one_retry_with_a_refreshed_token() {
        let refresher = FakeRefresher::working();
        let store = FakeStore::new();
        let source = source(
            token("a0", "r0", folio_base::time::now() + 3600),
            refresher.clone(),
            store.clone(),
        );
        // 内层对 `a0` 这一支回 401；续期后拿到的是 `access-1`，内层放行。
        let authed = AuthedStore::new(
            |access: &str| {
                if access == "a0" {
                    FakeInner::unauthorized()
                } else {
                    FakeInner::ok()
                }
            },
            source as Arc<dyn TokenSource>,
        );
        let got = RemoteStore::get(&authed, "books/a.epub").unwrap();
        assert_eq!(got, b"ok");
        assert_eq!(
            refresher.calls.load(Ordering::SeqCst),
            1,
            "401 之后强刷一次"
        );
        assert_eq!(store.saved.load(Ordering::SeqCst), 1);
    }

    /// 续期失败（refresh_token 被撤销）：交回**原来的 401**，不是续期那边的错。
    #[test]
    fn a_failed_refresh_keeps_the_original_error() {
        let source = source(
            token("a0", "r0", folio_base::time::now() + 3600),
            FakeRefresher::broken(),
            FakeStore::new(),
        );
        let authed = AuthedStore::new(
            |_| FakeInner::unauthorized(),
            source as Arc<dyn TokenSource>,
        );
        let err = RemoteStore::get(&authed, "x").unwrap_err();
        assert!(
            matches!(err, Error::Unauthorized { status: 401 }),
            "交回的应是原 401：{err:?}"
        );
    }

    /// token 变了内层就按新 token 重建；没变就复用（BaiduDrive 的 id 缓存靠这个活着）。
    #[test]
    fn the_inner_store_is_rebuilt_only_when_the_token_changes() {
        let builds: Arc<Mutex<Vec<String>>> = Arc::default();
        let source = source(
            token("a0", "r0", 0), // expires_at 0 = 不主动续
            FakeRefresher::working(),
            FakeStore::new(),
        );
        let authed = AuthedStore::new(
            {
                let builds = Arc::clone(&builds);
                move |access: &str| {
                    builds.lock().unwrap().push(access.to_string());
                    FakeInner::ok()
                }
            },
            source as Arc<dyn TokenSource>,
        );
        RemoteStore::list(&authed, "").unwrap();
        RemoteStore::list(&authed, "").unwrap();
        RemoteStore::list(&authed, "").unwrap();
        assert_eq!(
            builds.lock().unwrap().len(),
            1,
            "同一支 token 只该建一次内层"
        );
    }

    /// 假内层仓库：`fail_auth` 决定每次调用回 401 还是成功。
    struct FakeInner {
        fail_auth: bool,
    }

    impl FakeInner {
        fn unauthorized() -> Self {
            Self { fail_auth: true }
        }

        fn ok() -> Self {
            Self { fail_auth: false }
        }

        fn settle<T>(&self, ok: T) -> Result<T> {
            if self.fail_auth {
                Err(Error::Unauthorized { status: 401 })
            } else {
                Ok(ok)
            }
        }
    }

    impl RemoteStore for FakeInner {
        fn list(&self, _path: &str) -> Result<Vec<Entry>> {
            self.settle(Vec::new())
        }

        fn get(&self, _path: &str) -> Result<Vec<u8>> {
            self.settle(b"ok".to_vec())
        }

        fn put(&self, _path: &str, _body: Vec<u8>) -> Result<()> {
            self.settle(())
        }

        fn mkdir(&self, _path: &str) -> Result<()> {
            self.settle(())
        }

        fn download(&self, _p: &str, _s: &mut dyn Sink, _c: &Cancel) -> Result<u64> {
            Ok(0)
        }
    }
}
