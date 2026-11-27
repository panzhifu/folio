//! SAF 的本机持久来源能力。Java 只授予文件描述符，分块捕获归 core。

#[cfg(any(target_os = "android", test))]
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use folio_core::ingest::external_source::ExternalSourceAccess;
#[cfg(not(target_os = "android"))]
use folio_core::ingest::external_source::NoExternalSources;
#[derive(Debug, Clone)]
pub(crate) struct PickedSource {
    pub locator: String,
    pub display_name: String,
}

#[cfg(any(target_os = "android", test))]
#[derive(Debug, Clone)]
pub(crate) struct UriMetadata {
    pub size: Option<u64>,
    /// Unix 毫秒；提供方未报告时为 None。
    pub modified: Option<i64>,
    pub display_name: String,
}

#[cfg(any(target_os = "android", test))]
fn validate_uri(locator: &str) -> io::Result<()> {
    let valid = locator.strip_prefix("content://").is_some_and(|rest| {
        let authority = rest.split('/').next().unwrap_or_default();
        !authority.is_empty()
            && !authority.contains(['@', '?', '#'])
            && !locator.chars().any(char::is_control)
    });
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "来源不是可持久读取的文档 URI",
        ))
    }
}

#[cfg(any(target_os = "android", test))]
fn parse_metadata(json: &str) -> io::Result<Option<UriMetadata>> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "系统文档元数据不可读"))?;
    if value["error"] == "permission_denied" {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "文档持久读取权限已失效",
        ));
    }
    if value.get("error").is_some() {
        return Err(io::Error::other("文档来源暂不可用"));
    }
    if value["missing"] == true {
        return Ok(None);
    }
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "系统文档元数据不完整");
    let size = match value.get("size") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(value.as_u64().ok_or_else(invalid)?),
    };
    let modified = match value.get("modified") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(value.as_i64().ok_or_else(invalid)?),
    };
    let display_name = value["display_name"]
        .as_str()
        .ok_or_else(invalid)?
        .to_owned();
    Ok(Some(UriMetadata {
        size,
        modified,
        display_name,
    }))
}

#[cfg(target_os = "android")]
fn java_json(pick: bool, argument: &str) -> io::Result<Option<String>> {
    use ::jni::objects::JValue;
    use gpui_mobile::android::jni::{activity, find_app_class, get_string, with_env};
    with_env(|env| {
        let activity = activity(env)?;
        let class = find_app_class(env, "dev.gpui.mobile.FolioSourceAccess")?;
        let argument = env
            .new_string(argument)
            .map_err(|_| "文档参数无法交给系统".to_string())?;
        let method = if pick {
            ::jni::jni_str!("sourceForPick")
        } else {
            ::jni::jni_str!("statUri")
        };
        let result = env
            .call_static_method(
                &class,
                method,
                ::jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;)Ljava/lang/String;"),
                &[JValue::Object(&activity), JValue::Object(&argument)],
            )
            .and_then(|value| value.l())
            .map_err(|_| {
                env.exception_clear();
                "系统文档桥调用失败".to_string()
            })?;
        Ok((!result.is_null()).then(|| get_string(env, &result)))
    })
    .map_err(io::Error::other)
}

#[cfg(target_os = "android")]
pub(crate) fn source_for_pick(path: &Path) -> Option<PickedSource> {
    let json = java_json(true, path.to_str()?).ok()??;
    let value: serde_json::Value = serde_json::from_str(&json).ok()?;
    let source = PickedSource {
        locator: value["locator"].as_str()?.to_owned(),
        display_name: value["display_name"].as_str()?.to_owned(),
    };
    validate_uri(&source.locator).ok()?;
    Some(source)
}

#[cfg(not(target_os = "android"))]
pub(crate) fn source_for_pick(_path: &Path) -> Option<PickedSource> {
    None
}

#[cfg(target_os = "android")]
pub(crate) fn stat_uri(locator: &str) -> io::Result<Option<UriMetadata>> {
    validate_uri(locator)?;
    let json =
        java_json(false, locator)?.ok_or_else(|| io::Error::other("系统文档元数据未返回"))?;
    parse_metadata(&json)
}

#[cfg(target_os = "android")]
pub(crate) fn open_uri_fd(locator: &str) -> io::Result<std::fs::File> {
    use ::jni::objects::JValue;
    use gpui_mobile::android::jni::{activity, find_app_class, with_env};
    use std::os::fd::FromRawFd as _;
    validate_uri(locator)?;
    let fd = with_env(|env| {
        let activity = activity(env)?;
        let class = find_app_class(env, "dev.gpui.mobile.FolioSourceAccess")?;
        let argument = env
            .new_string(locator)
            .map_err(|_| "文档参数无法交给系统".to_string())?;
        env.call_static_method(
            &class,
            ::jni::jni_str!("openUriFd"),
            ::jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;)I"),
            &[JValue::Object(&activity), JValue::Object(&argument)],
        )
        .and_then(|value| value.i())
        .map_err(|_| {
            env.exception_clear();
            "系统文档桥调用失败".to_string()
        })
    })
    .map_err(io::Error::other)?;
    match fd {
        // SAFETY: Java detachFd 转交一个新打开的描述符，之后由 File 唯一持有并关闭。
        fd if fd >= 0 => Ok(unsafe { std::fs::File::from_raw_fd(fd) }),
        -2 => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "文档持久读取权限已失效",
        )),
        -3 => Err(io::Error::new(io::ErrorKind::NotFound, "原文档不存在")),
        _ => Err(io::Error::other("无法读取原文档")),
    }
}

pub(crate) fn source_access() -> Arc<dyn ExternalSourceAccess> {
    #[cfg(target_os = "android")]
    {
        Arc::new(AndroidSources)
    }
    #[cfg(not(target_os = "android"))]
    {
        Arc::new(NoExternalSources)
    }
}

/// 系统送进来的 intent 文档 URI（`ACTION_VIEW` / `ACTION_SEND`）落成 cache 里的临时
/// 文件，交导入管线吃路径。授权只随进程存活，所以**一次读干净**最稳：`stat` 取
/// display_name、`open` 开 fd、流式拷到盘上，不攒在内存里。
///
/// 目录 `intent-import-<序号>/<原名>`：`PathsSource::picked_temporary` 读完连目录
/// 一起清，与选择器副本同一条口径。返回 `None` = 文档不存在（提供方明确说了）。
#[cfg(target_os = "android")]
pub(crate) fn import_uri_to_temp(uri: &str) -> io::Result<Option<PathBuf>> {
    use std::sync::atomic::{AtomicU64, Ordering};

    let Some(metadata) = stat_uri(uri)? else {
        return Ok(None);
    };
    let mut source = open_uri_fd(uri)?;
    // display_name 可能带路径分隔符（恶意或奇葩提供方）：只取末段，取不到就给个中性名。
    let name = Path::new(&metadata.display_name)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".to_owned());
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = cache_dir()?.join(format!("intent-import-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let temp = dir.join(name);
    let mut out = std::fs::File::create(&temp)?;
    std::io::copy(&mut source, &mut out)?;
    Ok(Some(temp))
}

#[cfg(not(target_os = "android"))]
pub(crate) fn import_uri_to_temp(_uri: &str) -> std::io::Result<Option<PathBuf>> {
    Ok(None)
}

/// 应用 cache 目录（临时副本落在这里，系统清 cache 时一并带走）。
#[cfg(target_os = "android")]
fn cache_dir() -> io::Result<PathBuf> {
    use gpui_mobile::android::jni::{activity, get_string, with_env};
    with_env(|env| {
        let activity = activity(env)?;
        let file = env
            .call_method(
                &activity,
                ::jni::jni_str!("getCacheDir"),
                ::jni::jni_sig!("()Ljava/io/File;"),
                &[],
            )
            .and_then(|value| value.l())
            .map_err(|_| "getCacheDir 无法递给系统".to_string())?;
        let path = env
            .call_method(
                &file,
                ::jni::jni_str!("getAbsolutePath"),
                ::jni::jni_sig!("()Ljava/lang/String;"),
                &[],
            )
            .and_then(|value| value.l())
            .map_err(|_| "cache path 无法递给系统".to_string())?;
        Ok(PathBuf::from(get_string(env, &path)))
    })
    .map_err(io::Error::other)
}

#[cfg(target_os = "android")]
struct AndroidSources;
#[cfg(target_os = "android")]
impl ExternalSourceAccess for AndroidSources {
    fn metadata(
        &self,
        locator: &str,
    ) -> io::Result<folio_core::ingest::external_source::SourceMetadata> {
        let metadata = stat_uri(locator)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "原文档不存在"))?;
        Ok(folio_core::ingest::external_source::SourceMetadata {
            size: metadata.size,
            modified: metadata.modified,
            display_name: metadata.display_name,
        })
    }
    fn open(&self, locator: &str) -> io::Result<Box<dyn std::io::Read + Send>> {
        Ok(Box::new(open_uri_fd(locator)?))
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::platform::android_sources::{parse_metadata, validate_uri};
    #[test]
    fn document_access_rejects_non_content_locators_and_preserves_unknown_metadata() {
        for bad in [
            "file:///private/book.epub",
            "https://server/book",
            "content:///missing",
            "content://user@authority/path",
            "content://provider/\nsecret",
        ] {
            assert!(validate_uri(bad).is_err());
        }
        assert!(validate_uri("content://provider/document/a%3Ab").is_ok());
        let metadata =
            parse_metadata(r#"{"size":null,"modified":null,"display_name":"我的书.epub"}"#)
                .unwrap()
                .unwrap();
        assert_eq!(metadata.size, None);
        assert_eq!(metadata.modified, None);
        assert_eq!(metadata.display_name, "我的书.epub");
        assert!(parse_metadata(r#"{"missing":true}"#).unwrap().is_none());
        assert_eq!(
            parse_metadata(r#"{"error":"permission_denied"}"#)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
}
