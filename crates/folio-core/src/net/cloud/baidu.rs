//! 百度网盘作为的一种网盘：xpan OpenAPI 的最小客户端。
//!
//! 一期只认**读者手里已有的 token**（从开放平台的简化授权、或第三方工具的存储
//! 配置里整段粘贴进来），不做刷新 —— 换新 token 要 `client_id` / `client_secret`，
//! 那是开发者侧的事，等云函数中转那条路落地再接。token 过期（`errno 111`）报成
//! [`Error::Unauthorized`]，界面上说的就是「重新粘贴」，不含糊。
//!
//! 两条硬规矩来自官方文档：请求要带 `User-Agent: pan.baidu.com`（下载直链
//! `dlink` 明确要求，list 一并带上没坏处）；`dlink` 自带时效（8 小时），所以每次
//! 下载都先问一次 `filemetas` 拿新直链，不缓存它。
//!
//! [`Drive::list`] 的路径语义在这里的换算是：网盘绝对路径以 `/` 开头，[`Entry::path`]
//! 收成**相对根**（掐掉开头的 `/`）；[`Drive::download`] 收到相对路径，先补回 `/`。
//!
//! 走的都是 [`Transport`]，所以全部离线可测（假传输 + fixture JSON）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{Drive, Entry};
use crate::net::remote::{Cancel, Error, Method, Request, Response, Result, Sink, Transport};

const API: &str = "https://pan.baidu.com/rest/2.0";
/// 官方要求下载请求带这个 UA；list / filemetas 一并带上。
const USER_AGENT: &str = "pan.baidu.com";
/// 一层目录的条目上限。书目录撞到 1000 个文件再谈翻页。
const LIST_LIMIT: u32 = 1000;

/// 一个百度网盘入口。token 是读者粘贴进来的那一整段；`Transport` 注入进来，
/// core 不需要任何 runtime 就能测完。
pub struct BaiduDrive {
    access_token: String,
    transport: Arc<dyn Transport>,
    /// 上一次 list 见过的 `相对路径 → fs_id`。`filemetas` 只认 fs_id，缓存让
    /// 「浏览完就下载」不用再解析一遍路径；没见过的路径现场去它的父目录找。
    ids: Mutex<HashMap<String, u64>>,
}

impl BaiduDrive {
    pub fn new(access_token: impl Into<String>, transport: Arc<dyn Transport>) -> Self {
        Self {
            access_token: access_token.into(),
            transport,
            ids: Mutex::new(HashMap::new()),
        }
    }

    /// 列一层。`path` 为空就是根；返回的条目记进 id 缓存。
    fn list_inner(&self, path: &str) -> Result<Vec<Entry>> {
        let dir = absolute(path);
        let url = format!(
            "{API}/xpan/file?method=list&order=name&limit={LIST_LIMIT}&dir={}&access_token={}",
            query_encode(&dir),
            query_encode(&self.access_token),
        );
        tracing::debug!("baidu: list {dir}");
        let req = Request::new(Method::Get, url).header("User-Agent", USER_AGENT);
        let resp = self.transport.request(req)?;
        check(&resp)?;
        let parsed: ListResp = parse_json(&resp.body)?;
        if parsed.errno != 0 {
            return Err(errno_error(parsed.errno));
        }
        let mut ids = self.ids.lock().expect("baidu id 表不该中毒");
        let entries = parsed
            .list
            .into_iter()
            .map(|item| {
                // 网盘路径是绝对的；收成 Drive 语义里的相对路径。
                let relative = item.path.trim_start_matches('/').to_string();
                let name = item
                    .server_filename
                    .filter(|name| !name.trim().is_empty())
                    .map(|name| name.trim().to_string())
                    .unwrap_or_else(|| {
                        relative.rsplit('/').next().unwrap_or(&relative).to_string()
                    });
                if item.fs_id > 0 {
                    ids.insert(relative.clone(), item.fs_id);
                }
                Entry {
                    name,
                    path: relative,
                    is_dir: item.isdir != 0,
                    size: (item.size > 0).then_some(item.size),
                    modified: None,
                    etag: None,
                }
            })
            .collect();
        Ok(entries)
    }

    /// 找到路径的 `fs_id`：先查缓存，没有就去它的父目录里现找一遍。
    fn resolve_id(&self, path: &str) -> Result<u64> {
        let relative = path.trim_matches('/');
        if let Some(id) = self
            .ids
            .lock()
            .expect("baidu id 表不该中毒")
            .get(relative)
            .copied()
        {
            return Ok(id);
        }
        // 缓存里没有：父目录列一遍。这条路径多半是别的会话记下的，或者下载是
        // 不经浏览直接发起的。以路径为准找 —— 名字在不同目录里可能重。
        let parent = relative.rsplit_once('/').map_or("", |(head, _)| head);
        let found = self
            .list_inner(parent)?
            .iter()
            .any(|entry| entry.path == relative);
        if !found {
            return Err(Error::NotFound);
        }
        self.ids
            .lock()
            .expect("baidu id 表不该中毒")
            .get(relative)
            .copied()
            .ok_or(Error::NotFound)
    }

    /// 问一次 `filemetas` 拿下载直链。
    fn fetch_dlink(&self, fs_id: u64) -> Result<String> {
        let url = format!(
            "{API}/xpan/multimedia?method=filemetas&access_token={}",
            query_encode(&self.access_token),
        );
        let body = format!("fsids=[{fs_id}]&dlink=1");
        tracing::debug!("baidu: filemetas fs_id={fs_id}");
        let req = Request::new(Method::Post, url)
            .header("User-Agent", USER_AGENT)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body.into_bytes());
        let resp = self.transport.request(req)?;
        check(&resp)?;
        let parsed: FileMetas = parse_json(&resp.body)?;
        if parsed.errno != 0 {
            return Err(errno_error(parsed.errno));
        }
        parsed
            .list
            .into_iter()
            .find_map(|item| (!item.dlink.is_empty()).then_some(item.dlink))
            .ok_or(Error::Malformed("filemetas 没回 dlink".into()))
    }
}

impl Drive for BaiduDrive {
    fn label(&self) -> &'static str {
        "baidu"
    }

    fn list(&self, path: &str) -> Result<Vec<Entry>> {
        self.list_inner(path)
    }

    fn download(&self, path: &str, sink: &mut dyn Sink, cancel: &Cancel) -> Result<u64> {
        let fs_id = self.resolve_id(path)?;
        let mut dlink = self.fetch_dlink(fs_id)?;
        // dlink 自带一串查询参数；access_token 以追加参数的方式带上。
        dlink.push_str("&access_token=");
        dlink.push_str(&query_encode(&self.access_token));
        let req = Request::new(Method::Get, dlink.clone()).header("User-Agent", USER_AGENT);
        tracing::debug!("baidu: download {}（fs_id {fs_id}）", path.trim_matches('/'));
        let got = self.transport.download(req, sink, cancel);
        match &got {
            Ok(bytes) => tracing::debug!("baidu: 下载 {path:?} → {bytes} 字节"),
            Err(Error::Cancelled) => tracing::debug!("baidu: 下载 {path:?} 被取消"),
            Err(err) => tracing::warn!("baidu: 下载 {path:?} 失败：{err}",),
        }
        got
    }
}

/// 相对路径 → 网盘绝对路径：掐掉首尾斜杠再补一个开头。根就是 `/`。
fn absolute(path: &str) -> String {
    format!("/{}", path.trim_matches('/'))
}

/// HTTP 层的状态码 → 错。百度偶尔把鉴权失败直接写在 HTTP 状态上，挡在 JSON 之前。
fn check(resp: &Response) -> Result<()> {
    if (200..300).contains(&resp.status) {
        return Ok(());
    }
    match resp.status {
        401 => Err(Error::Unauthorized { status: 401 }),
        403 => Err(Error::Forbidden { status: 403 }),
        404 => Err(Error::NotFound),
        other => Err(Error::Unexpected { status: other }),
    }
}

/// 业务层的 `errno` → 错。111 是 token 过期、-6 是身份验证失败 —— 界面上都是
/// 「重新粘贴 token」这一个动作，报成 [`Error::Unauthorized`] 归到同一档。
fn errno_error(errno: i64) -> Error {
    match errno {
        111 | -6 => Error::Unauthorized { status: 401 },
        other => Error::Malformed(format!("errno {other}")),
    }
}

fn parse_json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T> {
    serde_json::from_slice(body).map_err(|err| Error::Malformed(err.to_string()))
}

/// 查询串参数的百分号编码：白名单外一律转义，`/` 也不例外（它在路径里是分隔符，
/// 在参数值里只是普通字符）。
fn query_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~') {
            out.push(ch);
        } else {
            for byte in ch.encode_utf8(&mut [0u8; 4]).as_bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

#[derive(Debug, serde::Deserialize)]
struct ListResp {
    errno: i64,
    #[serde(default)]
    list: Vec<ListItem>,
}

#[derive(Debug, serde::Deserialize)]
struct ListItem {
    fs_id: u64,
    path: String,
    #[serde(default)]
    server_filename: Option<String>,
    #[serde(default)]
    isdir: u8,
    #[serde(default)]
    size: u64,
}

#[derive(Debug, serde::Deserialize)]
struct FileMetas {
    errno: i64,
    #[serde(default)]
    list: Vec<FileMeta>,
}

#[derive(Debug, serde::Deserialize)]
struct FileMeta {
    #[serde(default)]
    dlink: String,
}

#[cfg(test)]
mod tests {
    //! 假传输 + fixture：list 的形状、下载的两段请求、token 过期的分档、缓存脱靶时的补列。

    use std::sync::Arc;

    use super::*;
    use crate::net::remote::dav::Mock;

    const LIST_JSON: &str = r#"{"errno":0,"list":[
        {"fs_id":1111,"path":"/我的书/斗战狂潮.epub","server_filename":"斗战狂潮.epub","isdir":0,"size":15144675},
        {"fs_id":2222,"path":"/我的书/漫画","server_filename":"漫画","isdir":1,"size":0}
    ]}"#;

    const METAS_JSON: &str =
        r#"{"errno":0,"list":[{"fs_id":1111,"dlink":"https://d.pcs.baidu.com/file/x?fid=1"}]}"#;

    fn drive(responses: Vec<Response>) -> (BaiduDrive, Arc<Mock>) {
        let mock = Arc::new(Mock::new(responses));
        (
            BaiduDrive::new("121.abc", Arc::clone(&mock) as Arc<dyn Transport>),
            mock,
        )
    }

    #[test]
    fn list_returns_one_level_with_relative_paths() {
        let (drive, mock) = drive(vec![Response::new(200, LIST_JSON.as_bytes().to_vec())]);
        let entries = drive.list("我的书").expect("应当解析出列表");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "我的书/斗战狂潮.epub");
        assert_eq!(entries[0].name, "斗战狂潮.epub");
        assert_eq!(entries[0].size, Some(15144675));
        assert!(!entries[0].is_dir);
        assert!(entries[1].is_dir);

        let req = mock.last_request();
        assert!(
            req.url.contains("dir=%2F%E6%88%91%E7%9A%84%E4%B9%A6"),
            "目录要整体编码进查询串：{}",
            req.url
        );
        assert!(req.url.contains("access_token=121.abc"));
        assert!(
            req.headers.iter().any(|(k, v)| k == "User-Agent" && v == USER_AGENT),
            "请求要带官方要求的 UA"
        );
        // 列过一遍，id 表就该记住这两条 —— 下面的下载测试靠它免掉补列那一趟。
        assert!(drive.ids.lock().unwrap().contains_key("我的书/斗战狂潮.epub"));
    }

    #[test]
    fn download_goes_through_filemetas_with_the_token() {
        let (drive, mock) = drive(vec![
            Response::new(200, METAS_JSON.as_bytes().to_vec()),
            Response::new(200, Vec::new()),
        ]);
        drive.ids.lock().unwrap().insert("我的书/书.epub".into(), 1111);
        let mut sink = CountSink::default();
        let got = drive
            .download("我的书/书.epub", &mut sink, &Cancel::new())
            .expect("下载应当成功");
        // Mock 的块是固定的 "abc" + "defg"。
        assert_eq!(got, 7);

        let metas = mock.seen.lock().unwrap()[0].clone();
        assert_eq!(metas.method, Method::Post);
        assert!(metas.url.contains("method=filemetas"));
        assert!(metas.url.contains("access_token=121.abc"));
        assert_eq!(String::from_utf8_lossy(&metas.body), "fsids=[1111]&dlink=1");
        let fetch = mock.last_request();
        assert_eq!(
            fetch.url,
            "https://d.pcs.baidu.com/file/x?fid=1&access_token=121.abc",
            "dlink 要追加 access_token"
        );
        assert!(
            fetch
                .headers
                .iter()
                .any(|(k, v)| k == "User-Agent" && v == USER_AGENT)
        );
    }

    #[test]
    fn download_resolves_an_unseen_path_by_listing_its_parent() {
        // 缓存脱靶：先补列父目录，再 filemetas，再下载。
        let (drive, mock) = drive(vec![
            Response::new(200, LIST_JSON.as_bytes().to_vec()),
            Response::new(200, METAS_JSON.as_bytes().to_vec()),
            Response::new(200, Vec::new()),
        ]);
        let mut sink = CountSink::default();
        drive
            .download("我的书/斗战狂潮.epub", &mut sink, &Cancel::new())
            .expect("补列之后应当下载成功");
        let seen = mock.seen.lock().unwrap();
        assert!(seen[0].url.contains("method=list"), "第一趟要补列父目录");
        assert!(seen[1].url.contains("method=filemetas"));
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn an_expired_token_is_an_auth_failure() {
        let (drive, _mock) = drive(vec![Response::new(
            200,
            br#"{"errno":111,"list":[]}"#.to_vec(),
        )]);
        let err = drive.list("我的书").unwrap_err();
        assert!(matches!(err, Error::Unauthorized { .. }), "实际 {err:?}");
    }

    #[test]
    fn an_http_401_is_an_auth_failure_even_before_the_json() {
        let (drive, _mock) = drive(vec![Response::new(401, Vec::new())]);
        let err = drive.list("我的书").unwrap_err();
        assert!(matches!(err, Error::Unauthorized { .. }));
    }

    #[test]
    fn an_unknown_errno_does_not_pose_as_auth() {
        let (drive, _mock) = drive(vec![Response::new(200, br#"{"errno":12}"#.to_vec())]);
        let err = drive.list("我的书").unwrap_err();
        assert!(matches!(err, Error::Malformed(_)), "实际 {err:?}");
    }

    /// 只数一下字节的落点，让「下了什么」能被断言。
    #[derive(Default)]
    struct CountSink(usize);

    impl Sink for CountSink {
        fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
            self.0 += chunk.len();
            Ok(())
        }

        fn finish(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn abort(&mut self) {}
    }
}
