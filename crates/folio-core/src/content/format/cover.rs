//! 封面落盘：**按封面自己的字节哈希**存一份文件，库里只留描述。
//!
//! `plans/导入管线.md` §八 第 4 条。三条规矩各有理由：
//!
//! **一、封面用它自己的哈希命名，不用书的哈希。** 两本书共用一张封面（同一套系列
//! 设计）时自然去重；重新解析或换 provider 不该让封面文件改名；封面目录因此可以
//! 独立做垃圾回收，不必和书的记录耦合。
//!
//! **二、重导同一本书不重写封面。** 字节相同 → 哈希相同 → 文件已存在就不写。
//! 这条让「重扫整库」成为安全操作：不会把几十本书的封面重写一遍。
//!
//! **三、写失败不阻断导入。** 封面是装饰，不是内容的入口。磁盘满、目录没权限、
//! 路径太长 —— 都只是「这次没写成」，库里照旧留那一行，只是没有文件名可指，
//! 于是退回去用 `books.cover` 那个 BLOB 兜底（老库与新失败都走这一条）。
//!
//! 与 `plans/基础设施.md` §5.1 那条「一切持久化格式带版本戳」的关系：
//! `books.cover_spec` 记的是**规格**（缩到多长边、什么编码），换规格要重扫重生成；
//! 这里记的是**这份字节是谁**。两件事不同，所以是两列。

use std::path::{Path, PathBuf};

use crate::Result;
use crate::ingest::identity::ContentHash;

/// 封面文件名的主名：哈希十六进制的前 20 字（= 10 字节 = 80 位）。
///
/// 与书目副本那边的 `ContentHash::file_stem` 同一条口径（前 10 字节），只是不用
/// base32 —— 封面名字不需要可读性，只要够短且够不撞。80 位对「一个书库里的封面」
/// 这个量级是远远过剩的，而短名字让 `covers/` 目录列起来清爽。
///
/// **写与读必须用同一个函数**：写的时候截断、读的时候拿完整哈希去拼路径，
/// 就会得到「文件明明在、却读不出来」—— 而那种 bug 在界面上只表现为
/// 「封面没了」，没有任何报错。
fn stem(hex: &str) -> String {
    hex.chars().take(20).collect()
}

/// 一份封面在磁盘上的位置：`<书库>/covers/<哈希>.<扩展名>`。
///
/// `root` 是书库根（`paths::covers_dir` 要的那一个），调用方从**这本书自己的副本
/// 路径**推出来 —— 库层不持有书库位置，那是读者确认过的目录，只有界面/管线知道。
pub fn cover_path(root: &Path, hash_hex: &str, mime: &str) -> PathBuf {
    crate::infra::paths::covers_dir(root).join(format!("{}.{}", stem(hash_hex), extension(mime)))
}

/// 从 MIME 推文件扩展名。认不出的回 `bin` 而不是空串 —— 空串会让文件名以点结尾，
/// 那种名字在某些文件系统上直接创建失败。
///
/// 真书里 `image/jpg`、`IMAGE/PNG`、`image/octet-stream` 这类写错很常见，
/// 所以这里只做小写归一化与前缀忽略，不试图猜内容（猜错了会影响文件管理器双击打开）。
pub fn extension(mime: &str) -> &'static str {
    let m = mime.trim().to_ascii_lowercase();
    match m.as_str() {
        "image/jpeg" | "image/jpg" | "image/pjpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" | "image/x-ms-bmp" => "bmp",
        "image/svg+xml" => "svg",
        "image/avif" => "avif",
        _ => "bin",
    }
}

/// 把一份封面写进 `covers/`，交回它的哈希。
///
/// **已存在就不写**（同一份字节第二次导入是常见事：系列书共用封面、重扫整库）。
/// 写入走「先写临时名再 rename」：半写完的封面文件比没有封面糟得多 ——
/// 它会被当成一份完好的封面画出来，而内容是截断的。
///
/// 失败一律回 `Err`，由调用方决定降级（`save_flow_in` 那边只记一句日志）。
pub fn write(root: &Path, bytes: &[u8], mime: &str) -> Result<ContentHash> {
    let hash = crate::ingest::identity::sha256_of_bytes(bytes);
    let dest = cover_path(root, &hash.to_hex(), mime);
    if dest.exists() {
        // 哈希相同 = 字节相同，重写一遍只是白付一次 IO（还可能把 mtime 搅动，
        // 让「封面有没有变」这类判断失灵）。
        return Ok(hash);
    }
    let dir = crate::infra::paths::covers_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| crate::Error::Io {
        path: dir.clone(),
        source: Box::new(e),
    })?;
    let tmp = dir.join(format!(".{}.part", stem(&hash.to_hex())));
    std::fs::write(&tmp, bytes).map_err(|e| crate::Error::Io {
        path: tmp.clone(),
        source: Box::new(e),
    })?;
    std::fs::rename(&tmp, &dest).map_err(|e| crate::Error::Io {
        path: dest.clone(),
        source: Box::new(e),
    })?;
    Ok(hash)
}

/// 一份封面文件还在不在。读那一侧的第一步：在就交回路径，不在就退回 BLOB。
pub fn resolve(root: &Path, hash: &str, mime: &str) -> Option<PathBuf> {
    // 与写入侧同一个 `stem`：截断口径必须一致，否则「文件明明在却读不出来」。
    let path =
        crate::infra::paths::covers_dir(root).join(format!("{}.{}", stem(hash), extension(mime)));
    path.is_file().then_some(path)
}

/// 一条封面记录的描述（库里那几列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverRef {
    /// 十六进制哈希。是**这一份字节**的身份，不是书的。
    pub hash: String,
    pub mime: String,
}

#[cfg(test)]
mod tests {
    use super::{cover_path, extension, resolve, write};
    use crate::infra::metrics::Metric;

    /// MIME 认不出时回 `bin` 而不是空串：空串会让文件名以点结尾。
    #[test]
    fn unknown_mime_still_yields_a_usable_extension() {
        assert_eq!(extension("image/jpeg"), "jpg");
        assert_eq!(extension("IMAGE/PNG"), "png", "大小写不该影响");
        assert_eq!(extension("image/jpg"), "jpg", "写错的那种也认");
        assert_eq!(extension("application/octet-stream"), "bin");
        assert_eq!(extension(""), "bin");
    }

    /// **已存在就不写**：同一份封面第二次导入不该重写文件（系列书共用封面、
    /// 重扫整库都会走到这条）。数的是写入次数而不是时间，所以在哪台机器上都一样。
    #[test]
    fn the_same_bytes_are_never_written_twice() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let bytes = b"pretend-jpeg-bytes";

        let first = write(root, bytes, "image/jpeg").unwrap();
        let path = cover_path(root, &first.to_hex(), "image/jpeg");
        assert!(path.is_file(), "第一次该真的写下去");

        // 把 mtime 记下来，再写一次同一份字节：文件不该被碰。
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        let second = write(root, bytes, "image/jpeg").unwrap();
        assert_eq!(first, second, "同一份字节同一个哈希");
        let after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after, "第二次不该重写文件");

        // 同一个哈希但扩展名不同（MIME 声明变了）：那是另一个文件名，
        // 所以会各写一份 —— 这正是「按 MIME 定扩展名」的代价，换来的是
        // 文件管理器能按真实类型打开它。
        let as_png = cover_path(root, &first.to_hex(), "image/png");
        assert_ne!(as_png, path);
        write(root, bytes, "image/png").unwrap();
        assert!(as_png.is_file());
    }

    /// 一份封面文件不见了（被读者手工清过 `covers/`、或目录整块删了）时
    /// `resolve` 回 `None`，让读那一侧退回 BLOB —— 不许假装还在。
    #[test]
    fn resolve_reports_a_missing_file_instead_of_pretending() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let hash = write(root, b"bytes", "image/png").unwrap();
        let hex = hash.to_hex();
        assert!(resolve(root, &hex, "image/png").is_some());

        std::fs::remove_file(cover_path(root, &hash.to_hex(), "image/png")).unwrap();
        assert!(
            resolve(root, &hex, "image/png").is_none(),
            "文件不在了就该回 None"
        );
    }

    /// 封面文件不带任何「这本书」的信息：两本书共用一份封面时只有**一个**文件。
    /// 这是「用它自己的哈希命名」那条规矩的全部收益。
    #[test]
    fn two_books_sharing_a_cover_share_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let bytes = b"series-cover";
        let a = write(root, bytes, "image/jpeg").unwrap();
        // 第二本走的是完全同一条调用（它不知道前一本存在）。
        let b = write(root, bytes, "image/jpeg").unwrap();
        assert_eq!(a, b);
        let covers = crate::infra::paths::covers_dir(root);
        let count = std::fs::read_dir(&covers).unwrap().count();
        assert_eq!(count, 1, "同一个哈希只该有一个文件，实际 {count} 个");
        // 顺带确认没有留下 `.part` 半成品。
        let leftovers: Vec<_> = std::fs::read_dir(&covers)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "不该留 .part");
        // 量具：写这一步不该开容器（它是纯文件系统的事）。
        assert_eq!(crate::infra::metrics::take_local(Metric::ContainerOpens), 0);
    }
}
