//! SAF 的本机持久来源能力。Java 只授予文件描述符，分块捕获归 core。

#[cfg(any(target_os = "android", test))]
use std::io;
use std::path::Path;
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
