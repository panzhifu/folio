//! 正文主题背景图的落盘：**按图自己的字节哈希**存一份文件，配置里只留文件名。
//!
//! 与封面（`content/format/cover.rs`）同三条规矩、同一条口径：
//!
//! **一、按内容哈希命名。** 读者给两套主题配了同一张纸纹时自然去重；换主题名不换图。
//!
//! **二、重选同一张图不重写。** 字节相同 → 哈希相同 → 文件已存在就不写。
//!
//! **三、这里只管落盘，不管清理。** 换图、清图之后旧文件可能已经没人指 ——
//! 留着它（一张纸纹几百 KB，量级与封面相当），孤儿回收是以后整目录 GC 的事，
//! 不在「读者清一张图」这条路径上付目录扫描的账。
//!
//! 图放在**数据根**（[`crate::infra::paths::backgrounds_dir`]）而不是书库：它是
//! 应用级的读者偏好，跟着 `config.json` 走，不跟着哪一间书库走。

use std::path::PathBuf;

use crate::Result;

/// 文件名的主名：哈希十六进制的前 20 字（= 80 位）。
///
/// 与封面那边的 `stem` 同一条口径（前 10 字节）。**写与读必须用同一个函数**：
/// 截断口径错了，bug 只表现为「背景图没了」，没有任何报错。
fn stem(hex: &str) -> String {
    hex.chars().take(20).collect()
}

/// 选择器交回来的扩展名换成落盘用的那一个；认不出回 `None`。
///
/// 只收白名单：文件名要拼进配置与路径，随手放行的扩展名既能让名字带进 `/` 之类的
/// 字符，也会让「图能不能解出来」等到画那一帧才炸。大写与小写等价（`JPG` = `jpg`）。
fn extension(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("jpg"),
        "png" => Some("png"),
        "webp" => Some("webp"),
        "gif" => Some("gif"),
        "bmp" => Some("bmp"),
        "avif" => Some("avif"),
        "svg" => Some("svg"),
        _ => None,
    }
}

/// 一份背景图在磁盘上的位置。只在文件名**来自 [`store`]** 时有意义 ——
/// 名字里的扩展名是白名单挑过的，这里不再消毒。
pub fn path_of(name: &str) -> PathBuf {
    crate::infra::paths::backgrounds_dir().join(name)
}

/// 把一张图收进 `backgrounds/`，交回它在配置里存的文件名（`<哈希前 20 字>.<扩展名>`）。
///
/// **已存在就不写**（理由见模块注释第二条）。写入走「先写临时名再 rename」：
/// 半写完的图比没有图糟得多 —— 它会被当成一份完好的纸纹铺到正文底下。
///
/// `raw_ext` 是系统选择器给的那个文件扩展名；认不出（`None`）是调用方的输入错，
/// 返回 `Err` 由界面提示，不猜内容 —— 猜错了会把一张不是图的文件收成纸纹。
pub fn store(bytes: &[u8], raw_ext: &str) -> Result<String> {
    store_in(&crate::infra::paths::backgrounds_dir(), bytes, raw_ext)
}

/// [`store`] 的指定目录版。拆出来给测试用：两根目录是**进程级**的全局
/// （[`crate::infra::paths`]），并发测试里为一张图改它，会泼到旁边正按默认两根
/// 做断言的测试（`usage` 那条就是这么红的）。真入口只有 [`store`] 一个。
pub fn store_in(dir: &std::path::Path, bytes: &[u8], raw_ext: &str) -> Result<String> {
    let Some(ext) = extension(raw_ext) else {
        return Err(crate::Error::UnsupportedImageFormat(raw_ext.to_string()));
    };
    let hash = crate::infra::identity::sha256_of_bytes(bytes);
    let name = format!("{}.{}", stem(&hash.to_hex()), ext);
    let dest = dir.join(&name);
    if dest.exists() {
        return Ok(name);
    }
    std::fs::create_dir_all(dir).map_err(|e| crate::Error::Io {
        path: dir.to_path_buf(),
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
    Ok(name)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::infra::backgrounds::{extension, store_in};

    /// 白名单里的每一条都收，大小写等价；认不出的与带路径的那类一律拒收。
    #[test]
    fn the_extension_whitelist_accepts_images_only() {
        assert_eq!(extension("png"), Some("png"));
        assert_eq!(extension("JPEG"), Some("jpg"));
        assert_eq!(extension(" webp "), Some("webp"));
        assert_eq!(extension("exe"), None);
        assert_eq!(extension(""), None);
    }

    /// 同一份字节存两遍是同一个名字、落同一份文件；换扩展名是另一份文件
    /// （哈希一样但名字不一样 —— 名字里带扩展名，配置指向哪份就是哪份）。
    #[test]
    fn storing_twice_is_idempotent_and_names_are_stable() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"a paper texture";

        let first = store_in(dir.path(), bytes, "png").unwrap();
        let again = store_in(dir.path(), bytes, "png").unwrap();
        assert_eq!(first, again);
        assert!(dir.path().join(&first).exists());

        let other = store_in(dir.path(), bytes, "JPEG").unwrap();
        assert_ne!(first, other, "扩展名变了就是另一份文件名");
        assert!(dir.path().join(&other).exists());
    }

    /// 拒收不该把临时文件或半份目录留下：报错早于任何 IO。
    #[test]
    fn a_rejected_extension_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(store_in(dir.path(), b"not an image", "exe").is_err());
        assert!(
            !dir.path().join("backgrounds").exists(),
            "拒收的那一趟连目录都不该建"
        );
        assert!(dir.path().read_dir().unwrap().next().is_none());
    }

    /// 临时名写完就 rename 成正名：半写完的图不能顶着正名躺在盘上
    /// （它会被当成一份完好的纸纹铺到正文底下）。
    #[test]
    fn the_temp_file_never_shares_the_final_name() {
        let dir = tempfile::tempdir().unwrap();
        let name = store_in(dir.path(), b"bytes", "png").unwrap();
        let entries: Vec<_> = dir
            .path()
            .read_dir()
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec![name], "盘上只有正名那一份");
        assert!(Path::new(&entries[0]).extension().is_some());
    }
}
