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

use md5::{Digest as _, Md5};

use super::{Drive, Entry};
use crate::{
    Cancel, Error, Method, RemoteStore, Request, Response, Result, Sink, Source, Transport,
};

const API: &str = "https://pan.baidu.com/rest/2.0";
/// 分片上传的第二步在另一台主机上（官方文档的 `superfile2`）。
const PCS: &str = "https://d.pcs.baidu.com/rest/2.0/pcs";
/// 第三方应用唯一可写的目录：申请接入时填的应用名就是这里那一级。取书浏览用空根
/// （整个网盘），同步与传书用这个根 —— 别把快照写到别人的目录里。
pub const APP_ROOT: &str = "apps/Folio";
/// 官方要求下载请求带这个 UA；list / filemetas 一并带上。
pub(super) const USER_AGENT: &str = "pan.baidu.com";
/// 一层目录的条目上限。书目录撞到 1000 个文件再谈翻页。
const LIST_LIMIT: u32 = 1000;
/// 分片大小。普通用户的上限是 4 MB；会员档更大，但同一份客户端对所有账号都能用，
/// 多几个请求换一份不会在非会员账号上撞墙的代码。单文件最多 1024 片（4 GiB）。
const CHUNK: usize = 4 * 1024 * 1024;

/// 一个百度网盘入口。token 是读者粘贴进来的那一整段；`Transport` 注入进来，
/// core 不需要任何 runtime 就能测完。
pub struct BaiduDrive {
    access_token: String,
    transport: Arc<dyn Transport>,
    /// 这一台仓库的根（相对网盘根）。空 = 整个网盘（取书浏览）；同步用
    /// [`APP_ROOT`]，因为上传只能落在应用自己的目录里。
    root: String,
    /// 上一次 list 见过的 `相对路径 → fs_id`。`filemetas` 只认 fs_id，缓存让
    /// 「浏览完就下载」不用再解析一遍路径；没见过的路径现场去它的父目录找。
    ids: Mutex<HashMap<String, u64>>,
}

impl BaiduDrive {
    pub fn new(access_token: impl Into<String>, transport: Arc<dyn Transport>) -> Self {
        Self::with_root(access_token, transport, "")
    }

    /// 把这一台仓库钉在某个子目录下（同步与传书用 [`APP_ROOT`]）。传进来的路径
    /// 都相对这个根，与 [`Drive`] 的语义一致。
    pub fn with_root(
        access_token: impl Into<String>,
        transport: Arc<dyn Transport>,
        root: impl Into<String>,
    ) -> Self {
        Self {
            access_token: access_token.into(),
            transport,
            root: root.into().trim_matches('/').to_string(),
            ids: Mutex::new(HashMap::new()),
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

    /// 网盘相对路径 → 仓库相对路径（掐掉根）。根之外的东西原样留着 —— 取书浏览
    /// （根为空）每个路径都走这一支。
    fn unrooted(&self, path: &str) -> String {
        let path = path.trim_start_matches('/');
        if self.root.is_empty() {
            return path.to_string();
        }
        if path == self.root {
            return String::new();
        }
        let prefix = format!("{}/", self.root);
        path.strip_prefix(&prefix).unwrap_or(path).to_string()
    }

    /// 列一层。`path` 为空就是根；返回的条目记进 id 缓存。
    fn list_inner(&self, path: &str) -> Result<Vec<Entry>> {
        let dir = absolute(&self.rooted(path));
        let url = with_token(
            format!(
                "{API}/xpan/file?method=list&order=name&limit={LIST_LIMIT}&dir={}",
                query_encode(&dir),
            ),
            &self.access_token,
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
                // 网盘路径是绝对的；收成这一台仓库的相对路径（取书浏览时根为空，
                // 同步时根是应用目录）。
                let relative = self.unrooted(&item.path);
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
        let url = with_token(
            format!("{API}/xpan/multimedia?method=filemetas"),
            &self.access_token,
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

    /// 发一条表单请求（xpan 的写接口都是 form-urlencoded POST）。
    fn form_post(&self, url: String, body: String) -> Result<Response> {
        let req = Request::new(Method::Post, url)
            .header("User-Agent", USER_AGENT)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body.into_bytes());
        let resp = self.transport.request(req)?;
        check(&resp)?;
        Ok(resp)
    }

    /// 写接口的 URL：`{API}/xpan/file?method=<method>&access_token=<token>`。
    fn write_url(&self, method: &str) -> String {
        with_token(
            format!("{API}/xpan/file?method={method}"),
            &self.access_token,
        )
    }

    /// 第一步：预上传。先把整份源读一遍算每片 MD5，再把片清单交给服务端。
    /// `return_type == 2` 是秒传 —— 云端已有同一份，后面两步都不用跑。
    fn precreate(&self, target: &str, size: u64, source: &mut dyn Source) -> Result<Precreate> {
        let blocks = block_md5s(source)?;
        source.seek_start(0)?;
        let list = serde_json::to_string(&blocks).map_err(|e| Error::Malformed(e.to_string()))?;
        let body = format!(
            "path={}&size={size}&isdir=0&autoinit=1&rtype=3&block_list={}",
            query_encode(target),
            query_encode(&list),
        );
        let resp = self.form_post(self.write_url("precreate"), body)?;
        let parsed: Precreate = parse_json(&resp.body)?;
        if parsed.errno != 0 {
            return Err(errno_error(parsed.errno));
        }
        if parsed.return_type != 2 && parsed.uploadid.is_empty() {
            return Err(Error::Malformed("precreate 没回 uploadid".into()));
        }
        Ok(Precreate { blocks, ..parsed })
    }

    /// 第二步：传一片。multipart 的 `file` 字段就是这一片的字节；服务端回它的 MD5。
    fn superfile2(
        &self,
        target: &str,
        uploadid: &str,
        partseq: usize,
        bytes: &[u8],
    ) -> Result<String> {
        let boundary = format!("folio-{uploadid}-{partseq}");
        let mut body = Vec::with_capacity(bytes.len() + 256);
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"chunk\"\r\n",
        );
        body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let url = with_token(
            format!(
                "{PCS}/superfile2?method=upload&type=tmpfile&path={}&uploadid={}&partseq={partseq}",
                query_encode(target),
                query_encode(uploadid),
            ),
            &self.access_token,
        );
        let req = Request::new(Method::Post, url)
            .header("User-Agent", USER_AGENT)
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body);
        let resp = self.transport.request(req)?;
        check(&resp)?;
        if resp.body.is_empty() {
            return Ok(String::new());
        }
        let parsed: Superfile = match serde_json::from_slice(&resp.body) {
            Ok(parsed) => parsed,
            Err(_) => {
                return Err(Error::Malformed(format!(
                    "superfile2 回了一段读不懂的应答（{} 字节）",
                    resp.body.len()
                )));
            }
        };
        if parsed.errno != 0 {
            return Err(errno_error(parsed.errno));
        }
        Ok(parsed.md5)
    }

    /// 第三步：合并；目录也走这一条（`isdir=1`）。
    fn create(
        &self,
        target: &str,
        size: u64,
        uploadid: Option<&str>,
        blocks: &[String],
        dir: bool,
    ) -> Result<()> {
        // 文件用 3（覆盖自己那份快照/同哈希的书）；目录用 0（不重命名）—— rtype=1
        // 会在同名时造出「folio-sync(1)」，而同步要的是「这个目录可用」。
        let rtype = if dir { 0 } else { 3 };
        let mut body = format!(
            "path={}&size={size}&isdir={}&rtype={rtype}",
            query_encode(target),
            u8::from(dir),
        );
        if let Some(uploadid) = uploadid {
            body.push_str("&uploadid=");
            body.push_str(&query_encode(uploadid));
        }
        if !blocks.is_empty() {
            let list =
                serde_json::to_string(blocks).map_err(|e| Error::Malformed(e.to_string()))?;
            body.push_str("&block_list=");
            body.push_str(&query_encode(&list));
        }
        let resp = self.form_post(self.write_url("create"), body)?;
        let parsed: Simple = parse_json(&resp.body)?;
        // -8 = 已经存在。mkdir_all 要的是「这个目录可用」，不是「我亲手建了它」。
        if parsed.errno != 0 && !(dir && parsed.errno == -8) {
            return Err(errno_error(parsed.errno));
        }
        Ok(())
    }

    /// 三步上传的整条链。`size` 由调用方给（文件元数据），不靠 `read_to_end`
    /// 现数 —— 大文件不能整份进内存。
    fn upload_from(
        &self,
        path: &str,
        source: &mut dyn Source,
        size: u64,
        cancel: &Cancel,
    ) -> Result<u64> {
        let target = absolute(&self.rooted(path));
        let pre = self.precreate(&target, size, source)?;
        if pre.return_type == 2 {
            tracing::info!("baidu: {target} 秒传，跳过分片");
            return Ok(size);
        }
        // 服务端说还缺哪几片；清单为空就是「全都缺」。
        let needed: std::collections::HashSet<usize> = pre.block_list.iter().copied().collect();
        let mut uploaded = Vec::with_capacity(pre.blocks.len());
        let mut chunk = vec![0u8; CHUNK];
        for (seq, local_md5) in pre.blocks.iter().enumerate() {
            if cancel.triggered() {
                return Err(Error::Cancelled);
            }
            if !needed.is_empty() && !needed.contains(&seq) {
                uploaded.push(local_md5.clone());
                continue;
            }
            source.seek_start((seq as u64) * (CHUNK as u64))?;
            let mut filled = 0;
            while filled < chunk.len() {
                let n = source.read(&mut chunk[filled..])?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            let md5 = self.superfile2(&target, &pre.uploadid, seq, &chunk[..filled])?;
            uploaded.push(if md5.is_empty() {
                local_md5.clone()
            } else {
                md5
            });
        }
        self.create(&target, size, Some(&pre.uploadid), &uploaded, false)?;
        tracing::info!(
            "baidu: 上传 {target} → {size} 字节（{} 片）",
            uploaded.len()
        );
        Ok(size)
    }
}

impl RemoteStore for BaiduDrive {
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
        let target = absolute(&self.rooted(path));
        self.create(&target, 0, None, &[], true)
    }

    /// 百度只让第三方应用写自己的目录；根（`apps/Folio`）也是这一层的父目录，
    /// 头一次同步时它可能还没被建出来。先把根建一遍（已存在由 -8 吞掉），再逐层。
    fn mkdir_all(&self, dir: &str) -> Result<()> {
        if !self.root.is_empty() {
            self.mkdir("")?;
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
        let fs_id = self.resolve_id(path)?;
        let dlink = self.fetch_dlink(fs_id)?;
        // dlink 自带一串查询参数；access_token 以追加参数的方式带上。
        let dlink = with_token(dlink, &self.access_token);
        let req = Request::new(Method::Get, dlink.clone()).header("User-Agent", USER_AGENT);
        tracing::debug!(
            "baidu: download {}（fs_id {fs_id}）",
            path.trim_matches('/')
        );
        let got = self.transport.download(req, sink, cancel);
        match &got {
            Ok(bytes) => tracing::debug!("baidu: 下载 {path:?} → {bytes} 字节"),
            Err(Error::Cancelled) => tracing::debug!("baidu: 下载 {path:?} 被取消"),
            Err(err) => tracing::warn!("baidu: 下载 {path:?} 失败：{err}",),
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

impl Drive for BaiduDrive {
    fn label(&self) -> &'static str {
        "baidu"
    }
}

/// 每一片的 MD5（4 MB 一片，最后一片可以不满）。
fn block_md5s(source: &mut dyn Source) -> Result<Vec<String>> {
    let mut blocks = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let mut filled = 0;
        while filled < buf.len() {
            let n = source.read(&mut buf[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break;
        }
        let mut hasher = Md5::new();
        hasher.update(&buf[..filled]);
        blocks.push(format!("{:x}", hasher.finalize()));
        if filled < buf.len() {
            break;
        }
    }
    Ok(blocks)
}

/// [`RemoteStore::get`] 的落点：小对象（同步快照）一次读进内存。
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
pub(super) fn query_encode(value: &str) -> String {
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

/// 把 `access_token` 追加到一条已经拼好的 URL 上。
///
/// 百度这条 API 只认 query 里的 token（`Authorization` 头它不认），所以每一处请求都得带。
/// 拼法收在这一个函数里有两个理由：
///
/// - **少一处漏带**：从前五处各写一遍 `format!("…&access_token={}", query_encode(…))`，
///   新增一个接口时最容易忘的就是这一段；
/// - **脱敏只认一种形状**：日志脱敏（[`crate::Request`] 的 `Debug`）按
///   `access_token=` 这个键名遮值，拼法多一种就多一处遮不住的地方。
///
/// 已经带查询串的 URL 用 `&` 接，否则用 `?` —— dlink 那种自带一长串参数的正好走前一支。
pub(super) fn with_token(url: String, token: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}access_token={}", query_encode(token))
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

/// 预上传的应答。`block_list` 是服务端说还缺哪几片（序号，0 起）。
#[derive(Debug, serde::Deserialize)]
struct Precreate {
    #[serde(default)]
    errno: i64,
    #[serde(default)]
    uploadid: String,
    /// 2 = 云端已有同一份（秒传），后面两步跳过。
    #[serde(default)]
    return_type: i64,
    #[serde(default)]
    block_list: Vec<usize>,
    /// 本地算出来的每片 MD5，不参与解析。
    #[serde(skip)]
    blocks: Vec<String>,
}

/// 分片上传的应答：服务端回这一片的 MD5。
#[derive(Debug, serde::Deserialize)]
struct Superfile {
    #[serde(default)]
    errno: i64,
    #[serde(default)]
    md5: String,
}

/// 写接口（create / mkdir）只看得懂一个 `errno`。
#[derive(Debug, serde::Deserialize)]
struct Simple {
    #[serde(default)]
    errno: i64,
}

#[cfg(test)]
mod tests {
    //! 假传输 + fixture：list 的形状、下载的两段请求、token 过期的分档、缓存脱靶时的补列。

    use std::sync::Arc;

    use super::*;
    use crate::dav::Mock;

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
            req.headers
                .iter()
                .any(|(k, v)| k == "User-Agent" && v == USER_AGENT),
            "请求要带官方要求的 UA"
        );
        // 列过一遍，id 表就该记住这两条 —— 下面的下载测试靠它免掉补列那一趟。
        assert!(
            drive
                .ids
                .lock()
                .unwrap()
                .contains_key("我的书/斗战狂潮.epub")
        );
    }

    #[test]
    fn download_goes_through_filemetas_with_the_token() {
        let (drive, mock) = drive(vec![
            Response::new(200, METAS_JSON.as_bytes().to_vec()),
            Response::new(200, Vec::new()),
        ]);
        drive
            .ids
            .lock()
            .unwrap()
            .insert("我的书/书.epub".into(), 1111);
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
            fetch.url, "https://d.pcs.baidu.com/file/x?fid=1&access_token=121.abc",
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

    /// 三步上传：precreate 收片 MD5、superfile2 发片、create 合并。
    #[test]
    fn upload_runs_precreate_chunks_and_create() {
        use md5::{Digest as _, Md5};

        let body = b"hello".to_vec();
        let mut hasher = Md5::new();
        hasher.update(&body);
        let md5 = format!("{:x}", hasher.finalize());
        let (drive, mock) = drive(vec![
            Response::new(
                200,
                br#"{"errno":0,"uploadid":"up-1","return_type":1,"block_list":[]}"#.to_vec(),
            ),
            Response::new(200, format!(r#"{{"errno":0,"md5":"{md5}"}}"#).into_bytes()),
            Response::new(200, br#"{"errno":0}"#.to_vec()),
        ]);

        let mut source = std::io::Cursor::new(body);
        let got = drive
            .put_file("books/a.epub", &mut source, 5, &Cancel::default())
            .expect("上传应当成功");
        assert_eq!(got, 5);

        let reqs = mock.requests();
        assert_eq!(reqs.len(), 3, "预上传、分片、合并各一条");
        assert!(reqs[0].url.contains("method=precreate"), "{}", reqs[0].url);
        let precreate = String::from_utf8_lossy(&reqs[0].body);
        assert!(precreate.contains("path=%2Fbooks%2Fa.epub"), "{precreate}");
        assert!(precreate.contains("block_list="), "{precreate}");
        assert!(precreate.contains(&md5), "片 MD5 要进预上传：{precreate}");

        assert!(reqs[1].url.contains("superfile2"), "{}", reqs[1].url);
        assert!(reqs[1].url.contains("partseq=0"), "{}", reqs[1].url);
        assert!(
            String::from_utf8_lossy(&reqs[1].body).contains("hello"),
            "分片请求要把这一片的字节原样带上"
        );

        assert!(reqs[2].url.contains("method=create"), "{}", reqs[2].url);
        let create = String::from_utf8_lossy(&reqs[2].body);
        assert!(create.contains("uploadid=up-1"), "{create}");
    }

    /// 秒传：预上传说云端已有同一份，后两步一个请求都不发。
    #[test]
    fn a_rapid_upload_stops_after_precreate() {
        let (drive, mock) = drive(vec![Response::new(
            200,
            br#"{"errno":0,"uploadid":"","return_type":2}"#.to_vec(),
        )]);
        let mut source = std::io::Cursor::new(vec![0u8; 10]);
        let got = drive
            .put_file("a.bin", &mut source, 10, &Cancel::default())
            .expect("秒传应当成功");
        assert_eq!(got, 10);
        assert_eq!(mock.requests().len(), 1, "秒传只该发预上传这一条");
    }

    /// 根前缀：同步那一台钉在应用目录，建目录与上传都必须落在那里。
    #[test]
    fn a_rooted_drive_prefixes_every_path() {
        let mock = Arc::new(Mock::new(vec![
            Response::new(200, br#"{"errno":0}"#.to_vec()),
            Response::new(
                200,
                br#"{"errno":0,"uploadid":"up-1","return_type":1,"block_list":[]}"#.to_vec(),
            ),
            Response::new(200, br#"{"errno":0,"md5":"x"}"#.to_vec()),
            Response::new(200, br#"{"errno":0}"#.to_vec()),
        ]));
        let drive =
            BaiduDrive::with_root("121.abc", Arc::clone(&mock) as Arc<dyn Transport>, APP_ROOT);

        drive.mkdir("folio-sync").expect("建目录");
        let body = String::from_utf8_lossy(&mock.last_request().body).to_string();
        assert!(
            body.contains("path=%2Fapps%2FFolio%2Ffolio-sync"),
            "目录要落在应用目录下：{body}"
        );
        assert!(body.contains("isdir=1"), "{body}");

        let mut source = std::io::Cursor::new(b"x".to_vec());
        drive
            .put_file(
                "folio-sync/books/a.epub",
                &mut source,
                1,
                &Cancel::default(),
            )
            .expect("上传");
        let reqs = mock.requests();
        let precreate = String::from_utf8_lossy(&reqs[1].body).to_string();
        assert!(
            precreate.contains("path=%2Fapps%2FFolio%2Ffolio-sync%2Fbooks%2Fa.epub"),
            "上传路径要带应用目录：{precreate}"
        );
        let parts = &reqs[2];
        assert!(parts.url.contains("superfile2"), "{}", parts.url);
        assert!(
            parts
                .url
                .contains("path=%2Fapps%2FFolio%2Ffolio-sync%2Fbooks%2Fa.epub"),
            "分片请求也要带应用目录：{}",
            parts.url
        );
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
