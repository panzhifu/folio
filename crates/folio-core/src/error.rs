use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("读取 {path} 失败：{reason}")]
    Io { path: PathBuf, reason: String },

    #[error("不支持的书籍格式：{0}")]
    UnsupportedFormat(String),

    #[error("没有这本书：{0}")]
    NoSuchBook(i64),

    #[error("没有这一章：{0}")]
    NoSuchChapter(i64),
}

pub trait IoResultExt<T> {
    fn io_path(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T, E: std::fmt::Display> IoResultExt<T> for std::result::Result<T, E> {
    fn io_path(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| Error::Io {
            path: path.into(),
            reason: e.to_string(),
        })
    }
}
