//! 后端的错误定义。用 `thiserror` 而不是 `anyhow`，是因为这一层不知道自己会被谁调：
//! 界面要能把「没有这本书」和「sqlite 打不开」分开处理（一个提示读者，一个退出程序），
//! 而 `anyhow` 那种被抹平的错误类型把这件事变成字符串比对。
//!
//! 约定两条：
//! - **每个变体都带上下文**（哪个路径、哪本书），错误文案自己就说得出发生了什么；
//! - **底层错误留在链上**（`#[source]` / `#[from]`），不压成字符串 —— 压扁之后
//!   `.source()` 走不下去，上层想按类型判断（是权限问题还是磁盘满）就只剩匹配文案了。
//!   界面侧收口成 `anyhow` 时（见 `folio_app::ui::shared::error`），这条链会整条打出来。

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

/// 一条被套上上下文的底层错误。
///
/// 用 `Box<dyn Error + Send + Sync>` 而不是 `to_string()`：文字信息一点没少（Display
/// 照样打得出），但因果链保住了。`Send + Sync` 是界面侧收进 `anyhow` 的前提。
pub type Source = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("读 {path} 失败：{source}")]
    Io {
        path: PathBuf,
        #[source]
        source: Source,
    },

    #[error("不支持的书籍格式：{0}")]
    UnsupportedFormat(String),

    #[error("没有这本书：{0}")]
    NoSuchBook(i64),

    #[error("没有这一章：{0}")]
    NoSuchChapter(i64),

    /// 库里只有正文的**位置**（v8 起），而按它回容器取时取不到。
    ///
    /// 单独立一条而不是回空串：它的后果与「这一章本来就是空的」完全不同 ——
    /// 前者要告诉读者「源文件不在了」（该重新导入或重新定位），而不是画一页空白。
    #[error("读不出第 {index} 章的正文：{path} 取不到 {at}")]
    ChapterUnavailable {
        path: PathBuf,
        index: usize,
        at: String,
    },

    /// 集合名是用户直接敲进来的东西。空名不只是「难看」：空串在 `UNIQUE` 上只容得下
    /// 一条，第二个人再建一个空名集合会收到「名字已存在」这种看不懂的报错。
    /// 所以在入口挡掉，而不是让它变成一条数据库错误。
    #[error("集合名不能为空")]
    EmptyCollectionName,

    /// 网络传输那一层的错（`remote`）。整条链保留：URL 下载、将来的 WebDAV
    /// 同步失败时，界面拿到的是「连不上 / 认证被拒 / 已取消」的原文。
    #[error(transparent)]
    Remote(#[from] crate::remote::Error),

    /// 日志装不起来。只有一个位置：`tracing` 的全局 subscriber 全程只能装一次。
    ///
    /// 单独立一条而不是混进 `Io`，是因为它的后果是「这次没有诊断输出」而不是「某个文件
    /// 读不到」—— 调用方（入口）对此只需要做一件事：照常启动，把原因说给还能看见的地方。
    /// 装的是 `try_init` 那个错误（它除了 subscriber 冲突，还可能带上 `log` facade 那一条），
    /// 原文留在链上，别压掉。
    #[error("日志装不上：这个进程里只有一个 subscriber 的位置（{0}）")]
    Logging(#[source] Source),
}

impl From<tracing_subscriber::util::TryInitError> for Error {
    fn from(err: tracing_subscriber::util::TryInitError) -> Self {
        Self::Logging(Box::new(err))
    }
}

pub trait IoResultExt<T> {
    /// 把底层错误套上「哪个路径」再往外传，原始错误留在链上。
    fn io_path(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T, E: std::error::Error + Send + Sync + 'static> IoResultExt<T> for std::result::Result<T, E> {
    fn io_path(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| Error::Io {
            path: path.into(),
            source: Box::new(e),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{IoResultExt as _, Result};
    use std::io::ErrorKind;

    /// `io_path` 必须把原始错误留在链上：压成字符串之后，「是权限问题还是磁盘满」
    /// 就只能靠匹配文案了，而文案是会改的。
    #[test]
    fn a_wrapped_io_error_keeps_its_source_on_the_chain() {
        let err: Result<()> =
            Err(std::io::Error::from(ErrorKind::PermissionDenied)).io_path("/x/y");
        let err = err.unwrap_err();
        assert!(
            format!("{err}").contains("/x/y"),
            "路径要出现在文案里：{err}"
        );
        let source = std::error::Error::source(&err).expect("链上要留着底层那一条");
        assert_eq!(
            source
                .downcast_ref::<std::io::Error>()
                .expect("就是那条 io::Error")
                .kind(),
            ErrorKind::PermissionDenied
        );
    }
}
