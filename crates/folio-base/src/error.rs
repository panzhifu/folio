//! 地层的错误定义。
//!
//! 只有两格：读盘与日志装配。业务词汇（书、章节、批注、导入、网络）在 `folio-core`
//! 的 `Error` 里，上层在边界处用 `From` 1:1 映射回去，调用方 match 的分支不变 ——
//! 与 `folio-format::Error` → `folio_core::Error` 那条同一条口径。
//!
//! 约定：每个变体都带上下文（哪个路径），底层原因留在 `source` 链上。

use std::path::PathBuf;

/// 一条被套上上下文的底层错误。
///
/// 用 `Box<dyn Error + Send + Sync>` 而不是 `to_string()`：文字信息一点没少，
/// 但因果链保住了。
pub type Source = Box<dyn std::error::Error + Send + Sync + 'static>;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("读 {path} 失败：{source}")]
    Io {
        path: PathBuf,
        #[source]
        source: Source,
    },

    /// 日志装不起来。只有一个位置：`tracing` 的全局 subscriber 全程只能装一次。
    ///
    /// 单独立一条而不是混进 [`Error::Io`]：它的后果是「这次没有诊断输出」而不是
    /// 「某个文件读不到」—— 调用方对此只需要做一件事：照常启动，把原因说给还能看见
    /// 的地方。原文留在链上。
    #[error("日志装不上：这个进程里只有一个 subscriber 的位置（{0}）")]
    Logging(#[from] tracing::subscriber::SetGlobalDefaultError),
}

#[cfg(test)]
mod tests {
    use super::{Error, Source};

    /// 路径要出现在文案里，底层错误要留在链上：压成字符串之后，
    /// 「是权限问题还是磁盘满」就只能靠匹配文案了，而文案是会改的。
    #[test]
    fn an_io_error_keeps_its_path_and_its_source_on_the_chain() {
        let source: Source = Box::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let err = Error::Io {
            path: "/x/y".into(),
            source,
        };
        let shown = format!("{err}");
        assert!(shown.contains("/x/y"), "路径要出现在文案里：{shown}");
        let source = std::error::Error::source(&err).expect("链上要留着底层那一条");
        assert_eq!(
            source
                .downcast_ref::<std::io::Error>()
                .expect("就是那条 io::Error")
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
}
