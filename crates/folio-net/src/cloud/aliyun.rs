//! 阿里云盘（Alipan）作为的一种网盘：官方开放平台 `adrive/v1.0` 的最小客户端。
//!
//! 与百度的 [`super::baidu`] 在**形状**上对齐（同一个 [`Drive`] / [`RemoteStore`]），
//! 但协议差别不小，照抄会错：
//!
//! - 所有接口都是 **POST + JSON 体**（百度那边多是 GET + query / 表单）；
//! - 每个请求都要 `drive_id`，且路径不是绝对路径 —— 阿里云盘**只用 file_id**。
//!   所以这一层要维护「路径 → file_id」的一张表（与百度那张 `fs_id` 表同一个用途，
//!   但百度还能按路径直接请求，阿里云盘不行）；
//! - 认证是 `Authorization: Bearer <token>`，不是 query 里的 `access_token`；
//! - 下载要先 `getDownloadUrl` 换一条带签名的直链（那条链不带 Bearer）。
//!
//! 路径语义与 [`Drive`] 一致：相对这一台仓库的根、`/` 分隔、不带首尾斜杠。
//! 根由 `root` 决定 —— 取书浏览用 `root`（`root` 文件夹），同步与传书用
//! [`APP_ROOT`]（应用自己的目录）。
//!
//! 全部走 [`Transport`]，离线可测（假传输 + fixture JSON）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{Drive, Entry};
use crate::{
    Cancel, Error, Method, RemoteStore, Request, Response, Result, Sink, Source, Transport,
};

const API: &str = "https://open.aliyundrive.com";
/// 应用自己的目录。与百度那边同一条取舍：第三方应用不该往别人的目录里写快照。
pub const APP_ROOT: &str = "Folio";
/// 阿里云盘根目录的固定 file_id（不是路径，是一个常量）。
pub const ROOT_ID: &str = "root";
/// 一层目录取多少条。阿里云盘单页上限 200，写死这个数不会撞墙。
const LIST_LIMIT: u32 = 200;
/// 上传的分片大小。阿里云盘要每个分片先报 `part_number` 与 `part_size`，
/// 服务端回一组 `upload_url`，再逐片 PUT。64 MB 是官方示例用的数。
const CHUNK: usize = 64 * 1024 * 1024;

/// 一个阿里云盘入口。
pub struct AliyunDrive {
    access_token: String,
    /// 云盘编号。每个文件请求都要它。
    drive_id: String,
    transport: Arc<dyn Transport>,
    /// 这一台仓库的根（相对网盘根）。空 = 根目录；同步用 [`APP_ROOT`]。
    root: String,
    /// `路径 → file_id`。阿里云盘只认 file_id，所以浏览过的路径都得记住 ——
    /// 「浏览完就下载」因此不必再解析一遍。
    ids: Mutex<HashMap<String, String>>,
}

impl AliyunDrive {
    pub fn new(
        access_token: impl Into<String>,
        drive_id: impl Into<String>,
        transport: Arc<dyn Transport>,
    ) -> Self {
        Self::with_root(access_token, drive_id, transport, "")
    }

    /// 把这一台仓库钉在某个子目录下（同步与传书用 [`APP_ROOT`]）。
    pub fn with_root(
        access_token: impl Into<String>,
        drive_id: impl Into<String>,
        transport: Arc<dyn Transport>,
        root: impl Into<String>,
    ) -> Self {
        let root = root.into().trim_matches('/').to_string();
        let mut ids = HashMap::new();
        // 根自己先记上：`list("")` 要问 root 的 id，免得多一次解析。
        ids.insert(String::new(), ROOT_ID.to_string());
        Self {
            access_token: access_token.into(),
            drive_id: drive_id.into(),
            transport,
            root,
            ids: Mutex::new(ids),
        }
    }

    /// 仓库相对路径 → 网盘相对路径（补上根）。
    fn rooted(&self, path: &str) -> String {
        let path = path.trim_matches('/');
        if self.root.is_empty() {
            path.to_string()
        } else if path.is_empty() {
            self.root.clone()
        } else {
            format!("{}/{}", self.root, path)
        }
    }

    /// 发一条 JSON POST。所有接口共用这一个形状。
    fn post(&self, endpoint: &str, body: serde_json::Value) -> Result<Response> {
        let req = Request::new(Method::Post, format!("{API}/adrive/v1.0/{endpoint}"))
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .body(body.to_string().into_bytes());
        tracing::debug!("aliyun: POST {endpoint}");
        let resp = self.transport.request(req)?;
        check(&resp)?;
        Ok(resp)
    }

    /// 列一层。`path` 为空就是根；返回的条目记进 id 缓存。
    fn list_inner(&self, path: &str) -> Result<Vec<Entry>> {
        let dir = self.rooted(path);
        let parent = self.resolve_id(&dir)?;
        // 分页：`marker` 为空就是第一页。一本书的目录通常一页就完，但画册那种
        // 一目录几百上千张的必须翻完 —— 少翻一页的症状是「后面几张图不见」。
        let mut out = Vec::new();
        let mut marker = String::new();
        loop {
            let body = serde_json::json!({
                "drive_id": self.drive_id,
                "parent_file_id": parent,
                "limit": LIST_LIMIT,
                "marker": marker,
                "order_by": "name",
                "order_direction": "ASC",
            });
            let resp = self.post("openFile/list", body)?;
            let parsed: ListResp = parse_json(&resp.body)?;
            let mut ids = self.ids.lock().expect("aliyun id 表不该中毒");
            for item in parsed.items {
                let relative = if dir.is_empty() {
                    item.name.clone()
                } else {
                    format!("{dir}/{}", item.name)
                };
                ids.insert(relative.clone(), item.file_id.clone());
                out.push(Entry {
                    name: item.name,
                    path: relative,
                    is_dir: item.kind == "folder",
                    size: item
                        .size
                        .and_then(|size| u64::try_from(size).ok())
                        .filter(|size| *size > 0),
                    modified: None,
                    etag: None,
                });
            }
            if parsed.next_marker.is_empty() {
                break;
            }
            marker = parsed.next_marker;
        }
        Ok(out)
    }

    /// 找到路径对应的 `file_id`：先查缓存，没有就去它的父目录列一遍。
    ///
    /// 路径里的每一段都要有 id 才解得出来 —— 缓存是 `list` 顺手记的，所以「浏览过
    /// 再打开」这一条路一直命中；直接下载（不走浏览）时逐段现查一次。
    fn resolve_id(&self, path: &str) -> Result<String> {
        let relative = path.trim_matches('/');
        if let Some(id) = self
            .ids
            .lock()
            .expect("aliyun id 表不该中毒")
            .get(relative)
            .cloned()
        {
            return Ok(id);
        }
        let parent = relative.rsplit_once('/').map_or("", |(head, _)| head);
        // 先把父目录列一遍（这一步会把它自己的子项都记进表里）。
        self.list_inner(parent)?;
        self.ids
            .lock()
            .expect("aliyun id 表不该中毒")
            .get(relative)
            .cloned()
            .ok_or(Error::NotFound)
    }

    /// 问一条下载直链。`expire_sec` 用 900（15 分钟）：够下完一本大书，又不至于
    /// 在读者挂机时一直有效。
    fn download_url(&self, file_id: &str) -> Result<String> {
        let body = serde_json::json!({
            "drive_id": self.drive_id,
            "file_id": file_id,
            "expire_sec": 900,
        });
        let resp = self.post("openFile/getDownloadUrl", body)?;
        let parsed: DownloadResp = parse_json(&resp.body)?;
        if parsed.url.is_empty() {
            return Err(Error::Malformed("getDownloadUrl 没回 url".into()));
        }
        Ok(parsed.url)
    }

    /// 在 `parent` 下建一个目录。已经存在算成功。
    fn mkdir_in(&self, parent: &str, name: &str) -> Result<String> {
        let parent_id = self.resolve_id(parent)?;
        let body = serde_json::json!({
            "drive_id": self.drive_id,
            "parent_file_id": parent_id,
            "name": name,
            "type": "folder",
            "check_name_mode": "refuse",
        });
        match self.post("openFile/create", body) {
            Ok(resp) => {
                let parsed: CreateResp = parse_json(&resp.body)?;
                let path = if parent.is_empty() {
                    name.to_string()
                } else {
                    format!("{parent}/{name}")
                };
                self.ids
                    .lock()
                    .expect("aliyun id 表不该中毒")
                    .insert(path, parsed.file_id.clone());
                Ok(parsed.file_id)
            }
            // 同名已存在（`refuse` 档）：去父目录里找出它，算成功。
            Err(Error::Malformed(message)) if message.contains("Exist") => {
                let path = if parent.is_empty() {
                    name.to_string()
                } else {
                    format!("{parent}/{name}")
                };
                self.list_inner(parent)?;
                self.ids
                    .lock()
                    .expect("aliyun id 表不该中毒")
                    .get(&path)
                    .cloned()
                    .ok_or(Error::NotFound)
            }
            Err(err) => Err(err),
        }
    }

    /// 上传一个文件。三步：`create` 要分片槽位 → 逐片 PUT → `complete`。
    fn upload_from(
        &self,
        path: &str,
        source: &mut dyn Source,
        size: u64,
        cancel: &Cancel,
    ) -> Result<u64> {
        let relative = path.trim_matches('/');
        let (parent, name) = match relative.rsplit_once('/') {
            Some((head, name)) => (head, name),
            None => ("", relative),
        };
        if name.is_empty() {
            return Err(Error::Malformed("上传路径没有文件名".into()));
        }
        let parent_id = self.resolve_id(parent)?;
        let parts = size.div_ceil(CHUNK as u64).max(1) as usize;
        let part_info: Vec<serde_json::Value> = (1..=parts)
            .map(|part_number| serde_json::json!({ "part_number": part_number }))
            .collect();
        let body = serde_json::json!({
            "drive_id": self.drive_id,
            "parent_file_id": parent_id,
            "name": name,
            "type": "file",
            "size": size,
            "check_name_mode": "overwrite",
            "part_info_list": part_info,
        });
        let resp = self.post("openFile/create", body)?;
        let created: CreateResp = parse_json(&resp.body)?;
        if created.upload_id.is_empty() {
            return Err(Error::Malformed("openFile/create 没回 upload_id".into()));
        }

        // 逐片 PUT。`upload_url` 是带签名的直链，**不带** Bearer。
        let mut total = 0u64;
        for (index, part) in created.part_info_list.iter().enumerate() {
            if cancel.triggered() {
                return Err(Error::Cancelled);
            }
            let Some(url) = part.upload_url.as_deref().filter(|url| !url.is_empty()) else {
                return Err(Error::Malformed(format!(
                    "第 {} 片没有 upload_url",
                    index + 1
                )));
            };
            let mut buf = vec![0u8; CHUNK];
            let want = (size as usize - total as usize).min(CHUNK);
            let got = read_exact(source, &mut buf[..want])?;
            let req = Request::new(Method::Put, url.to_string())
                .header("Content-Type", "application/octet-stream")
                .body(buf[..got].to_vec());
            let resp = self.transport.request(req)?;
            if !(200..300).contains(&resp.status) {
                return Err(Error::Unexpected {
                    status: resp.status,
                });
            }
            total += got as u64;
        }

        let body = serde_json::json!({
            "drive_id": self.drive_id,
            "file_id": created.file_id,
            "upload_id": created.upload_id,
        });
        self.post("openFile/complete", body)?;
        let full = if self.root.is_empty() {
            relative.to_string()
        } else {
            format!("{}/{}", self.root, relative)
        };
        self.ids
            .lock()
            .expect("aliyun id 表不该中毒")
            .insert(full, created.file_id);
        Ok(total)
    }
}

/// 读满 `buf`；源提前结束是 `UnexpectedEof`（分片大小对不上，交上去也会被拒）。
fn read_exact(source: &mut dyn Source, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = source.read(&mut buf[filled..])?;
        if n == 0 {
            if filled == 0 {
                return Ok(0);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "分片没读满",
            ));
        }
        filled += n;
    }
    Ok(filled)
}

impl RemoteStore for AliyunDrive {
    fn list(&self, path: &str) -> Result<Vec<Entry>> {
        self.list_inner(path)
    }

    fn get(&self, path: &str) -> Result<Vec<u8>> {
        let mut sink = VecSink::default();
        RemoteStore::download(self, path, &mut sink, &Cancel::default())?;
        Ok(sink.bytes)
    }

    fn put(&self, path: &str, body: Vec<u8>) -> Result<()> {
        let size = body.len() as u64;
        let mut source = std::io::Cursor::new(body);
        self.upload_from(path, &mut source, size, &Cancel::default())
            .map(|_| ())
    }

    fn mkdir(&self, path: &str) -> Result<()> {
        let relative = path.trim_matches('/');
        if relative.is_empty() {
            return Ok(());
        }
        let (parent, name) = match relative.rsplit_once('/') {
            Some((head, name)) => (head, name),
            None => ("", relative),
        };
        self.mkdir_in(parent, name).map(|_| ())
    }

    /// 先补根（同步用 [`APP_ROOT`]，第一次跑时它可能还没被建出来），再逐层。
    fn mkdir_all(&self, dir: &str) -> Result<()> {
        if !self.root.is_empty() {
            self.mkdir_in("", &self.root)?;
        }
        let mut prefix = String::new();
        for part in dir.split('/').filter(|part| !part.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            self.mkdir(&prefix)?;
        }
        Ok(())
    }

    fn download(&self, path: &str, sink: &mut dyn Sink, cancel: &Cancel) -> Result<u64> {
        let file_id = self.resolve_id(path)?;
        let url = self.download_url(&file_id)?;
        let req = Request::new(Method::Get, url);
        tracing::debug!("aliyun: download {}", path.trim_matches('/'));
        let got = self.transport.download(req, sink, cancel);
        match &got {
            Ok(bytes) => tracing::debug!("aliyun: 下载 {path:?} → {bytes} 字节"),
            Err(Error::Cancelled) => tracing::debug!("aliyun: 下载 {path:?} 被取消"),
            Err(err) => tracing::warn!("aliyun: 下载 {path:?} 失败：{err}"),
        }
        got
    }

    fn put_file(
        &self,
        path: &str,
        source: &mut dyn Source,
        size: u64,
        cancel: &Cancel,
    ) -> Result<u64> {
        self.upload_from(path, source, size, cancel)
    }
}

impl Drive for AliyunDrive {
    fn label(&self) -> &'static str {
        "aliyun"
    }
}

/// 阿里云盘的状态挡板：401 / 403 各自的语义与 [`crate::Error`] 对齐。
///
/// 错误体里带 `code` 的（`AccessTokenInvalid` / `AccessTokenExpired` / `I400JD`）
/// 一律归认证 —— 界面上的动作只有「重新授权」，不该按 HTTP 状态再分一次。
fn check(resp: &Response) -> Result<()> {
    if (200..300).contains(&resp.status) {
        // 2xx 也可能是错误体（阿里云盘偶发 200 + code）。
        if let Ok(parsed) = serde_json::from_slice::<ErrorBody>(&resp.body)
            && let Some(code) = parsed.code.as_deref()
            && !code.trim().is_empty()
        {
            return Err(body_error(code, parsed.message.as_deref()));
        }
        return Ok(());
    }
    // 非 2xx：先看体里有没有更具体的 code。
    if let Ok(parsed) = serde_json::from_slice::<ErrorBody>(&resp.body)
        && let Some(code) = parsed.code.as_deref()
        && !code.trim().is_empty()
    {
        return Err(body_error(code, parsed.message.as_deref()));
    }
    match resp.status {
        401 => Err(Error::Unauthorized { status: 401 }),
        403 => Err(Error::Forbidden { status: 403 }),
        404 => Err(Error::NotFound),
        other => Err(Error::Unexpected { status: other }),
    }
}

/// 错误码 → 我们那一套。
fn body_error(code: &str, message: Option<&str>) -> Error {
    match code {
        "AccessTokenInvalid" | "AccessTokenExpired" | "I400JD" => {
            Error::Unauthorized { status: 401 }
        }
        "ForbiddenAccess" | "Forbidden" => Error::Forbidden { status: 403 },
        "NotFound.File" | "NotFound" => Error::NotFound,
        other => Error::Malformed(match message {
            Some(message) => format!("{other}: {message}"),
            None => other.to_string(),
        }),
    }
}

fn parse_json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T> {
    serde_json::from_slice(body).map_err(|err| Error::Malformed(err.to_string()))
}

/// 攒字节的落点：`get` 与小文件的 `put` 用它。
#[derive(Default)]
struct VecSink {
    bytes: Vec<u8>,
}

impl Sink for VecSink {
    fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    fn finish(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn abort(&mut self) {
        self.bytes.clear();
    }
}

#[derive(Debug, serde::Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ListResp {
    #[serde(default)]
    items: Vec<ListItem>,
    #[serde(default)]
    next_marker: String,
}

#[derive(Debug, serde::Deserialize)]
struct ListItem {
    #[serde(default)]
    file_id: String,
    #[serde(default)]
    name: String,
    /// `"file"` / `"folder"`
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    size: Option<i64>,
}

#[derive(Debug, serde::Deserialize)]
struct DownloadResp {
    #[serde(default)]
    url: String,
}

#[derive(Debug, serde::Deserialize)]
struct CreateResp {
    #[serde(default)]
    file_id: String,
    #[serde(default)]
    upload_id: String,
    #[serde(default)]
    part_info_list: Vec<PartInfo>,
}

#[derive(Debug, serde::Deserialize)]
struct PartInfo {
    #[serde(default)]
    upload_url: Option<String>,
}

#[cfg(test)]
#[path = "aliyun_tests.rs"]
mod tests;
