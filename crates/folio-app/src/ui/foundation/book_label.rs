//! 多处书头共用的作者与文件大小表示。

pub const AUTHOR_SEP: &str = " · ";

pub fn fmt_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024 * 1024) as f64)
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
