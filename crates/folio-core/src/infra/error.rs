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

    /// 容器完好、判定也放行了，但 `META-INF/encryption.xml` 里写的是 DRM 而不是字体
    /// 去混淆 —— 正文那些字节**永远**解不开。
    ///
    /// 单独立一条而不是混进 [`Error::UnsupportedFormat`]：后者的意思是「这一版还没写
    /// 这个格式的 provider」，写完了就能读；DRM 不会因为我们多写代码就变得能读，
    /// 读者的动作也不是等版本，是换一本无加密的文件。`method` 是那个算法 URI，
    /// 它决定的是开发者能不能一眼认出是哪一家 DRM。
    #[error("这本书带 DRM 加密（{method}），读不了：{name}")]
    Encrypted { name: String, method: String },

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
    Remote(#[from] crate::net::remote::Error),

    /// 日志装不起来。只有一个位置：`tracing` 的全局 subscriber 全程只能装一次。
    ///
    /// 单独立一条而不是混进 `Io`，是因为它的后果是「这次没有诊断输出」而不是「某个文件
    /// 读不到」—— 调用方（入口）对此只需要做一件事：照常启动，把原因说给还能看见的地方。
    /// 装的是 `set_global_default` 那个错误（subscriber 已被别处占住），原文留在链上。
    #[error("日志装不上：这个进程里只有一个 subscriber 的位置（{0}）")]
    Logging(#[source] Source),

    /// 导入把一本候选**拒了**。`why` 是给人看的那一句，底层原因留在 `source` 上。
    ///
    /// 单独立一条而不是复用 [`Error::UnsupportedFormat`]：报表要按类别分组
    /// （「跳过 3（格式不支持）」），而「不是书」「容器坏了」「读不动」是三件不同的事，
    /// 界面上读者该做的动作也不同（换一个文件、去修那个文件、重新下载一次）。
    /// 判据装在类型上而不是靠匹配文案 —— `plans/基础设施.md` §4.1 第 3 条说的就是这个。
    #[error("收不下《{name}》：{why}")]
    Rejected {
        name: String,
        why: String,
        reject: ImportReject,
        #[source]
        source: Option<Source>,
    },
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
}

impl Error {
    /// 这条错误是不是「导入拒了这本书」，属于哪一类。批量报表按它分组。
    ///
    /// 认 [`Error::UnsupportedFormat`] 那一条是因为 provider 那一路仍在用它；
    /// 两条迟早合成一条，先都认，免得报表少算。
    pub fn import_reject(&self) -> Option<ImportReject> {
        match self {
            Error::Rejected { reject, .. } => Some(*reject),
            Error::UnsupportedFormat(_) => Some(ImportReject::UnsupportedFormat),
            Error::Encrypted { .. } => Some(ImportReject::Encrypted),
            _ => None,
        }
    }
}

impl From<tracing::subscriber::SetGlobalDefaultError> for Error {
    fn from(err: tracing::subscriber::SetGlobalDefaultError) -> Self {
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
