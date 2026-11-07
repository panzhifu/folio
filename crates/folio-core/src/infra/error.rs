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
    ///
    /// `method` 只进 `Debug`（`{name}` 那句才是读者看的）：那一串是
    /// `http://www.w3.org/2001/04/xmlenc#rsa-oaep-mgf1p` 这种形状，摆给读者既读不懂
    /// 也换不来动作，而日志里 `{:?}` 照样取得到。
    #[error("《{name}》带 DRM 加密，读不了：换一本没有加密的文件")]
    Encrypted { name: String, method: String },

    /// 文件用**读者手里的那份口令**锁着：输对了就是一本正常的书。
    ///
    /// 与 [`Error::Encrypted`] 分成两条，理由只有一句：**这一条重做一次可能会成**。
    /// 那边的意思是「这本书永远解不开，读者的动作是换一本」，这边的意思是「界面该问
    /// 一句口令」。合成一条的后果是报表里那一栏既不能提示输入、也不能提示换文件。
    ///
    /// 只有 PDF 那一族会从这里冒出来（`format::pdf::failure`，且只认引擎的
    /// `DecryptionError::PasswordProtected` 那一档）。MOBI / EPUB 的 DRM 走
    /// [`Error::Encrypted`] 且永远不走这一条：那边的密钥要从读者的**注册身份**（PID）
    /// 派生，不是他手上的一份口令。
    #[error("《{name}》要口令才能打开")]
    NeedsPassword { name: String },

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

    /// StarDict 词典读不动：缺文件、版本不认识、索引与数据对不上。
    ///
    /// 单独立一条而不是复用 [`Error::Io`]：设置页要按「这一本词典坏了」提示并让读者
    /// 删掉重导，而不是把一份路径拼进通用的读盘错误里；导入那一趟也要能原样把它
    /// 交给弹层。
    #[error("词典 {path:?} 读不动：{why}")]
    Dictionary {
        path: PathBuf,
        why: String,
        #[source]
        source: Option<Source>,
    },

    /// 备份包这一层的语义错误：路径不在备份目录里、清单不认识。
    ///
    /// 不混进 [`Error::Io`]：这一条说的是「这一份东西不是本应用这一版认得的备份」，
    /// 读者要做的不是检查网盘权限，而是换一份包或换一个版本。
    #[error("备份包 {path:?} 用不了：{why}")]
    Backup { path: PathBuf, why: String },

    /// 字体这一层的语义错误：清单里没有这个 id、下载下来的字节大小对不上、
    /// 目录里的文件不是认得的字体。
    ///
    /// 单独立一条而不是复用 [`Error::Io`]：与 [`Error::Dictionary`] 同一条理由 ——
    /// 设置页要按「这一份字体装坏了」提示并让读者删掉重装，而不是把一条路径拼进
    /// 通用的读盘错误里。
    #[error("字体 {path:?} 装不上：{why}")]
    Font {
        path: PathBuf,
        why: String,
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
    /// 要读者手里那份口令。从 `Encrypted` 分出来只为一件事：**这一档可以再交一次**
    /// （界面问一句口令、重跑同一批），而 `Encrypted` 永远不可以。报表、文案、
    /// 界面上那颗按钮的语义全都挂在这一条分界上。
    NeedsPassword,
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
            Error::NeedsPassword { .. } => Some(ImportReject::NeedsPassword),
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

    /// DRM 那一句：**读者看到的文案里没有算法 URI**，而日志要的 `Debug` 里还得有。
    ///
    /// 从前 `Display` 直接摆 `{method}`，屏幕上就是一串
    /// `http://www.w3.org/2001/04/xmlenc#rsa-oaep-mgf1p` —— 读者读不懂，也不改变他要做的
    /// 动作（换一本没加密的）。URI 撤进 `Debug` 之后这一条还分得开是哪一家 DRM。
    #[test]
    fn the_drm_message_reads_to_a_person_while_the_log_keeps_the_algorithm() {
        let err = super::Error::Encrypted {
            name: "书甲".to_string(),
            method: "http://www.w3.org/2001/04/xmlenc#rsa-oaep-mgf1p".to_string(),
        };
        let shown = format!("{err}");
        assert!(shown.contains("书甲"), "书名要在：{shown}");
        assert!(
            !shown.contains("xmlenc"),
            "算法 URI 不该出现在读者那句话里：{shown}"
        );
        assert!(shown.contains("DRM"), "要说清楚是加密：{shown}");
        assert!(
            format!("{err:?}").contains("rsa-oaep-mgf1p"),
            "日志里要还认得出是哪一家 DRM"
        );
        // 分类不看文案，改措辞不该把批量报表的分组改散。
        assert_eq!(
            err.import_reject(),
            Some(super::ImportReject::Encrypted),
            "类别仍是「加密」"
        );
    }
}
