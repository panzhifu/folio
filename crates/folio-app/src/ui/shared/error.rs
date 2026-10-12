//! 界面侧的错误：一律收成 `anyhow`。
//!
//! 分工是这样的 —— **后端**（`folio-core`）用 `thiserror` 定义一个能按分支处理的枚举，
//! 它不知道自己会被谁调，「没有这本书」和「sqlite 打不开」得能被分开数；**界面**对错误
//! 只有两个动作：把一条因果链说清楚，以及决定这一次要不要拦住那个操作。这正是 `anyhow`
//! 的位置：`E` 是 erased 的，所以同一个 `Result` 里 `store.add_book(..)?` 与
//! `std::fs::copy(..)?` 直接共存，不必先互相写 `From`。
//!
//! 于是这一层的约定只有两条：
//!
//! 1. **干活的函数返回 [`Result`]**，并且在每个 `?` 上挂一句「当时在做哪件事」。
//!    链因此是「打开 X.epub：复制到 Y：读 Z 失败：权限不够」这种能直接读出卡在哪一层的句子。
//! 2. **回调那一层 [`report`] 一次**。gpui 的点击回调与异步任务收尾没有调用方可 `?`，
//!    所以在那儿写日志、必要时提示读者 —— 不 panic，也不静默。
//!
//! 「后端那一条错误」不会被弄丢：`folio_core::Error` 是标准的 `std::error::Error`，
//! 而它的每个变体都把自己底层那条留在 `#[source]` 上，所以 `{:#}` 打出来的是整条链。

pub use anyhow::Context;
pub use anyhow::Result;
pub use anyhow::anyhow;
pub use anyhow::bail;

/// 一次界面侧的失败：把整条因果链写进日志。
///
/// 用 `{:#}` 而不是 `{}`：anyhow 的 `:#` 是「外层那句：中层那句：最底层那句」，
/// 而 `{}` 只有最外面一句。界面出错时缺的从来不是「失败了」，是「卡在哪一层」——
/// 日志文件（见 `folio_core::infra::logging`）就是拿来看这一行的。
pub fn report(what: &str, err: &anyhow::Error) {
    tracing::error!("{what}：{err:#}");
}

/// 给读者看的那一句。
///
/// 链是给开发者看的，界面不能把「读 /data/… 失败：Permission denied」原样端上桌：
/// 所以这里取**最外层**那句上下文（`err.to_string()`，即最后一次 `.context()` 写的话），
/// 它按约定就是一句人话说得通的「书库位置没确认」这种。要更稳的文案就在调用点自己给，
/// 别把 i18n 键藏进 `context()` 的字符串里。
pub fn shown(err: &anyhow::Error) -> String {
    err.to_string()
}

/// 一条「尽力而为」的步骤：失败就写日志并交出 `None`，调用方照原样往下走。
///
/// 这一层有一类操作不该拦住屏幕上正在做的事 —— 登记书目、落章节、记「最近打开」、
/// 读回进度，它们失败的代价是「书架少一本 / 下次回来不在同一页」，不是「书打不开」。
/// 但静默失败等于没发生：所以每一条都必须留下声音，而 [`report`] 就是留声音的地方。
///
/// 用它而不是 `result.ok()`：后者把「为什么失败」这条唯一查得到的信息也一并丢了。
pub fn or_log<T>(result: Result<T>, what: &str) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(err) => {
            report(what, &err);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Result, anyhow, report, shown};
    use anyhow::Context as _;

    /// 一条错误从后端穿到界面的样子：`thiserror` 的枚举进 `anyhow` 的链，
    /// 链上每一层的上下文都在 —— `{:#}` 正是日志与调试要的那一行。
    fn load(store_dir: &str) -> Result<String> {
        inner(store_dir).with_context(|| format!("书库目录 {store_dir} 里读不出东西"))
    }

    fn inner(store_dir: &str) -> std::result::Result<String, folio_core::Error> {
        let _ = store_dir;
        Err(folio_core::Error::UnsupportedFormat("book.epub".into()))
    }

    #[test]
    fn the_whole_causal_chain_survives_into_the_app_layer() {
        let err = load("/books").unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("书库目录 /books 里读不出东西"), "{chain}");
        assert!(chain.contains("不支持的书籍格式"), "{chain}");
        // 界面拿到的那一句只是最外层，不是整条链。
        assert_eq!(shown(&err), "书库目录 /books 里读不出东西");
    }

    /// `report` 不许把链压掉，也不许因为日志层没装而 panic。
    #[test]
    fn reporting_never_panics_and_never_eats_the_chain() {
        let err = anyhow!("装上书库").context("导入失败");
        report("ShelfView: 导入", &err);
        assert!(format!("{err:#}").contains("导入失败: 装上书库"));
    }

    /// 一个 `?` 让两种互不相干的错误进同一个 `Result` —— 后端那层的 `From` 表因此
    /// 一次都不必写。这正是选 anyhow 的理由。
    #[test]
    fn two_unrelated_error_types_share_one_result() {
        fn core_step() -> std::result::Result<u32, folio_core::Error> {
            Ok(3)
        }

        fn mixed(read_config: bool) -> Result<u32> {
            let rows = core_step()?;
            if read_config {
                std::fs::read_to_string("/proc/no-such-file-folio").context("读偏好")?;
            }
            Ok(rows)
        }

        assert_eq!(mixed(false).unwrap(), 3);
        let err = mixed(true).unwrap_err();
        assert!(format!("{err:#}").contains("读偏好"), "{err:#}");
    }
}
