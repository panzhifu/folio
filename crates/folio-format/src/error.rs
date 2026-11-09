//! 解析层的错误。
//!
//! 与 `folio_core::Error` 分开的一份：这一层不认识数据库、网络与界面，所以只保留
//! 「这份文件读不动」「这一章取不到」这一类。core 在边界处把它 1:1 映射回自己的
//! 变体（`impl From<folio_format::Error> for folio_core::Error`），调用方的 match
//! 不用改。
//!
//! 约定与 core 那边一致：每个变体都带上下文，底层原因留在 `source` 链上。

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

/// 一条被套上上下文的底层错误。
pub type Source = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("读 {path} 失败：{source}")]
    Io {
        path: PathBuf,
        #[source]
        source: Source,
    },

    #[error("不支持的书籍格式：{0}")]
    UnsupportedFormat(String),

    /// 库里只有正文的**位置**，而按它回容器取时取不到。
    #[error("读不出第 {index} 章的正文：{path} 取不到 {at}")]
    ChapterUnavailable {
        path: PathBuf,
        index: usize,
        at: String,
    },

    /// 容器完好、判定也放行了，但 `META-INF/encryption.xml` 里写的是 DRM。
    #[error("《{name}》带 DRM 加密，读不了：换一本没有加密的文件")]
    Encrypted { name: String, method: String },

    /// 文件用读者手里的口令锁着：输对了就是一本正常的书。
    #[error("《{name}》要口令才能打开")]
    NeedsPassword { name: String },
}

/// 把底层错误套上「哪个路径」再往外传，原始错误留在链上。
pub trait IoResultExt<T> {
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

/// 导入拒绝的类别。第四类 `Cancelled` 不在这里：取消由作业层
/// （`crate::infra::task::TaskEvent::Cancelled`）报，那是一次中断而不是一句判决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportReject {
    /// 三层判定认得出但这一版读不动，或者根本认不出来。
    UnsupportedFormat,
    /// 头四字节说它是个 ZIP，中央目录却读不出来 —— 文件可能是坏的，值得留档查。
    CorruptContainer,
    /// 收得下、也认得出，但打开 / 解析失败。副本是那份刚拷进来的字节，问题在字节里。
    Unreadable,
    /// 容器读得动，内容被 DRM 锁着。与上面三档的区别是**重扫一万次也不会变好**：
    /// 「这一版读不动」是我们将来的事，「有 DRM」是这本书永远的事。
    Encrypted,
    /// 要读者手里那份口令。从 `Encrypted` 分出来只为一件事：**这一档可以再交一次**
    /// （界面问一句口令、重跑同一批），而 `Encrypted` 永远不可以。报表、文案、
    /// 界面上那颗按钮的语义全都挂在这一条分界上。
    NeedsPassword,
}

impl Error {
    /// 这条错误是不是「导入拒了这本书」，属于哪一类。批量报表按它分组。
    pub fn import_reject(&self) -> Option<ImportReject> {
        match self {
            Error::UnsupportedFormat(_) => Some(ImportReject::UnsupportedFormat),
            Error::Encrypted { .. } => Some(ImportReject::Encrypted),
            Error::NeedsPassword { .. } => Some(ImportReject::NeedsPassword),
            _ => None,
        }
    }
}
