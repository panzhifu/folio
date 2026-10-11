//! WebDAV 协议：该发什么、回来的东西怎么读。
//!
//! 这一层不含 socket，也不含 gpui，所以每条规则都能离线测。注释写的大多不是「RFC 怎么说」，
//! 而是「各家实现和 RFC 不一致时我们站哪边」—— 后者才是以后想改却不该随手改的那个东西。

use std::sync::Arc;

use base64::Engine as _;
use quick_xml::Reader;
use quick_xml::events::Event;

#[cfg(test)]
use std::collections::VecDeque;
#[cfg(test)]
use std::sync::Mutex;

use super::{Cancel, Error, Method, Request, Response, Result, Sink, Transport};

/// PROPFIND 的请求体。
///
/// 只取这五个属性，且全部在 `DAV:` 命名空间里：`resourcetype` 里的 `<collection/>` 用来判
/// 目录（比只看 `href` 尾斜杠可靠，有些服务器对目录不给尾斜杠），`getcontentlength` 给下载
/// 进度定分母，`displayname` 给中文书名用（路径末段有时带扩展名或被重写过），
/// `getetag` 与 `getlastmodified` 留给备份合并判断「远端这份变过没有」。
const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/><d:displayname/><d:getetag/></d:prop></d:propfind>"#;

/// 一个远端入口：Base URL 加凭据。
///
/// 凭据在这里只是字符串。落盘、读钥匙串都是上层的事，本层既不存也不写，所以拿到它的地方
/// 只可能是内存里那一小段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// 构造时补成以 `/` 结尾，于是拼路径只需要加、不需要判分隔符。
    pub base: String,
    pub username: String,
    pub password: String,
}

impl Endpoint {
    pub fn new(
        base: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        let mut base = base.into();
        if !base.ends_with('/') {
            base.push('/');
        }
        Self {
            base,
            username: username.into(),
            password: password.into(),
        }
    }

    /// Base URL 里「主机之后那一段」。反解 `href` 要拿它当基准：不少服务器把 WebDAV 挂在
    /// 子路径下（`https://host/dav/books/`），而它返回的 `href` 是带这段的绝对路径。
    fn path_prefix(&self) -> &str {
        let after_scheme = self
            .base
            .split_once("://")
            .map_or(self.base.as_str(), |(_, rest)| rest);
        match after_scheme.find('/') {
            Some(i) => &after_scheme[i..],
            None => "/",
        }
    }

    /// Basic 认证头。一期只发 Basic：值是 `user:pass` 的 base64，可逆，所以这条头只在
    /// https 下有意义 —— 这一点得由界面说清，本层没法替它加密。
    fn basic_header(&self) -> String {
        let pair = format!("{}:{}", self.username, self.password);
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(pair.as_bytes())
        )
    }

    /// 把相对路径拼成请求 URL；`dir` 为真时补尾斜杠（集合查询）。
    fn url(&self, path: &str, dir: bool) -> String {
        let mut url = self.base.clone();
        url.push_str(&encode_path(path));
        if dir && !url.ends_with('/') {
            url.push('/');
        }
        url
    }
}

/// 列表里的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 显示名：`displayname` 优先，没有就从路径末段取。
    pub name: String,
    /// 相对 Base 的路径：已百分号解码、用 `/` 分隔、不带尾斜杠。上层只认这个。
    pub path: String,
    pub is_dir: bool,
    /// 字节数。拿不到就是拿不到，别把它当「一定是这个大小」用。
    pub size: Option<u64>,
    /// 原样留着，不解析：`getlastmodified` 各家写法不齐（RFC 1123、ISO 8601、带不带时区的
    /// 都有），而一期只有「按名字排」这一种排序需求，为一个显示字段拖进日期库不值。
    pub modified: Option<String>,
    pub etag: Option<String>,
}

/// 对一个入口做 DAV 操作。传输是注入进来的，所以 core 这边不需要任何 runtime 就能测完。
pub struct Client {
    endpoint: Endpoint,
    transport: Arc<dyn Transport>,
}

impl Client {
    pub fn new(endpoint: Endpoint, transport: Arc<dyn Transport>) -> Self {
        Self {
            endpoint,
            transport,
        }
    }

    /// 列一层。`path` 为空就是根。
    pub fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let resp = self
            .transport
            .request(propfind_request(&self.endpoint, path))
            ?;
        check_status(&resp, &[207, 200])?;
        let raw = parse_multistatus(&String::from_utf8_lossy(&resp.body))?;
        let entries = children(&self.endpoint, &raw);
        tracing::debug!(
            "dav: 列出 {:?} → 服务器回 {} 条，收成 {} 条",
            path,
            raw.len(),
            entries.len()
        );
        Ok(entries)
    }

    /// 读一个小对象（备份那份 JSON）。整本书走 [`Client::download`]，不要读进内存。
    pub fn get(&self, path: &str) -> Result<Vec<u8>> {
        let resp = self
            .transport
            .request(self.authed(Method::Get, path, false))
            ?;
        check_status(&resp, &[200, 206])?;
        Ok(resp.body)
    }

    /// 写一个小对象。父目录必须已存在，所以流程上是 `mkdir` 在前。
    pub fn put(&self, path: &str, body: Vec<u8>) -> Result<()> {
        let mut req = self.authed(Method::Put, path, false);
        // 声明成 octet-stream 而不是 json：这里的内容是我们自己写的，不需要服务器解释，
        // 而各家对 octet-stream 的接受度是唯一一个能当常量用的东西。
        req.headers
            .push(("Content-Type".into(), "application/octet-stream".into()));
        req.body = body;
        let resp = self.transport.request(req)?;
        check_status(&resp, &[201, 204, 200])
    }

    /// 建集合（目录）。
    ///
    /// 「已经存在」在协议上没有统一说法（405 / 409 / 423 都见得到），而备份流程要的是
    /// 「这个目录可用」，不是「我亲手建了它」，所以这三种都算成功。
    pub fn mkdir(&self, path: &str) -> Result<()> {
        let resp = self
            .transport
            .request(self.authed(Method::Mkcol, path, true))
            ?;
        if matches!(resp.status, 201 | 200 | 405 | 409 | 423) {
            return Ok(());
        }
        check_status(&resp, &[207])
    }

    /// 下载到一个 [`Sink`]。字节怎么落地（临时文件、改名、清理）是 sink 的事。
    pub fn download(&self, path: &str, sink: &mut dyn Sink, cancel: &Cancel) -> Result<u64> {
        let got = self
            .transport
            .download(self.authed(Method::Get, path, false), sink, cancel)
            ;
        match &got {
            Ok(bytes) => tracing::debug!("dav: 下载 {:?} → {bytes} 字节", path),
            // 取消与真失败在界面上是两种表现（一个安静地回到原页，一个报错），
            // 日志里也要分得开。
            Err(Error::Cancelled) => tracing::debug!("dav: 下载 {:?} 被取消", path),
            Err(err) => tracing::warn!("dav: 下载 {:?} 失败：{err}", path),
        }
        got
    }

    fn authed(&self, method: Method, path: &str, dir: bool) -> Request {
        let header = self.endpoint.basic_header();
        let req =
            Request::new(method, self.endpoint.url(path, dir)).header("Authorization", header);
        tracing::debug!("dav: {} {}", method.as_str(), url_for_log(&req.url));
        req
    }
}

/// 日志里能出现的 URL：把 `https://用户:密码@host/…` 那一段凭据换成 `***`。
///
/// 有人习惯把用户名密码直接写进地址（各家 WebDAV 都接受这种写法），而日志文件是会被
/// 贴进工单、传给别人看的东西。URL 剩下的部分照原样留着 —— 排查要的正是路径。
fn url_for_log(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    // `@` 只在authority 里、下一个 `/` 之前才算凭据。
    let head = rest.split('/').next().unwrap_or(rest);
    match head.split_once('@') {
        Some((_, host)) => format!("{scheme}://***@{host}{}", &rest[head.len()..]),
        None => url.to_string(),
    }
}

/// 组装一次 `Depth: 1` 的 PROPFIND。
///
/// `Depth` 是建议而不是命令，网盘类实现尤其会无视它并递归返回整棵树 —— 所以「只取一层」
/// 最终由 [`children`] 按相对路径的段数裁，而不是指望这个头。
fn propfind_request(endpoint: &Endpoint, path: &str) -> Request {
    let url = endpoint.url(path, true);
    tracing::debug!("dav: PROPFIND {} (Depth: 1)", url_for_log(&url));
    Request::new(Method::Propfind, url)
        .header("Authorization", endpoint.basic_header())
        .header("Depth", "1")
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(PROPFIND_BODY.as_bytes().to_vec())
}

/// 状态码 → 成功，或者一条说得出下一步的错。
///
/// 207 Multi-Status 才是 PROPFIND 的成功，不是 200。401 要分「Basic 没过去」和「这台只认
/// Digest」：两者的修法完全不同，混成一条等于让用户对着密码框盲猜。
fn check_status(resp: &Response, ok: &[u16]) -> Result<()> {
    if ok.contains(&resp.status) {
        return Ok(());
    }
    match resp.status {
        401 => {
            let challenge = resp
                .header("www-authenticate")
                .unwrap_or_default()
                .to_ascii_lowercase();
            if challenge.contains("digest") && !challenge.contains("basic") {
                Err(Error::DigestRequired)
            } else {
                Err(Error::Unauthorized { status: 401 })
            }
        }
        403 => Err(Error::Forbidden { status: 403 }),
        404 => Err(Error::NotFound),
        // 对 PROPFIND 回 405 说明这个端点根本不是 WebDAV —— 最常见是把网页地址或 API 根
        // 填成了 Base。这时候报「找不到」或「未授权」会把人带到别处去查。
        405 => Err(Error::NotWebDav),
        other => Err(Error::Unexpected { status: other }),
    }
}

/// 一条 `<response>` 的原始内容，还没变成 [`Entry`]。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Raw {
    href: String,
    display: String,
    size: Option<u64>,
    modified: String,
    etag: String,
    is_dir: bool,
    /// 这个资源**自己**的状态（直接挂在 `<response>` 下的那个 `<status>`）。
    /// 服务器会把查不到的资源也塞回来，靠这个字段筛掉。
    status: Option<u16>,
}

/// 正在读的是哪个标量属性。
///
/// 这个标记**只在对应的结束标签处清掉**，不在文本事件里清：一个值会被拆成多个事件送来 ——
/// 实体引用（`&amp;`、`&#20013;`）在 quick-xml 里根本不是文本事件，而是独立的
/// `Event::GeneralRef`。一见值就清标记的结果是只留下头一段，长书名会被截断，
/// 而书名里的 `&` 会整段消失（两条都是实测，见 `entity_in_a_display_name_survives_whole`）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Href,
    Status,
    Display,
    Modified,
    Etag,
}

/// 把一个文本片段落到当前条目上。文本事件与实体引用事件共用这一条路径。
fn feed_text(cur: &mut Option<Raw>, field: Option<Field>, in_prop: bool, prop: &str, text: &str) {
    match field {
        // 值可能散在多个事件里，所以一律累加。
        Some(Field::Href) => push(cur, |r| r.href.push_str(text)),
        Some(Field::Status) => {
            let code = status_code(text);
            push(cur, |r| r.status = code.or(r.status));
        }
        Some(Field::Display) => push(cur, |r| r.display.push_str(text)),
        Some(Field::Modified) => push(cur, |r| r.modified.push_str(text)),
        Some(Field::Etag) => push(cur, |r| r.etag.push_str(text)),
        // 这三个属性没有独立的结构标签可认，只能靠 `<prop>` 里记下的元素名。
        None if in_prop && prop == "getcontentlength" => {
            let size = text.trim().parse::<u64>().ok();
            push(cur, |r| r.size = size.or(r.size));
        }
        None => {}
    }
}

/// 解 207 Multi-Status。
///
/// 命名空间前缀一律不看：`<d:response>` 与 `<response>`（默认命名空间写法）都收，
/// 真实服务器上两种输出都撞得到。
///
/// 这里**不开** `trim_text`：值里的空白有意义 —— `蜗牛 & 熊` 会被拆成三个事件，
/// 逐事件裁剪正好把 `&` 两侧的空格剪掉。空白噪声改在最后取值时一次 `trim` 处理。
fn parse_multistatus(xml: &str) -> Result<Vec<Raw>> {
    let mut reader = Reader::from_reader(xml.as_bytes());

    let mut out: Vec<Raw> = Vec::new();
    let mut cur: Option<Raw> = None;
    // 属性表内外的意义完全不同：`<prop>` 里面的元素名才是属性名，外面的是结构元素。
    let mut in_prop = false;
    let mut field: Option<Field> = None;
    let mut prop = String::new();

    loop {
        match reader
            .read_event()
            .map_err(|e| Error::Malformed(e.to_string()))?
        {
            Event::Eof => break,
            Event::Start(e) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                match local {
                    "response" => cur = Some(Raw::default()),
                    "href" if cur.is_some() && !in_prop => field = Some(Field::Href),
                    "status" if cur.is_some() && !in_prop => field = Some(Field::Status),
                    "displayname" if in_prop => field = Some(Field::Display),
                    "getlastmodified" if in_prop => field = Some(Field::Modified),
                    "getetag" if in_prop => field = Some(Field::Etag),
                    "collection" if in_prop => {
                        if let Some(r) = cur.as_mut() {
                            r.is_dir = true;
                        }
                    }
                    "prop" => in_prop = true,
                    _ if in_prop => prop = local.to_string(),
                    _ => {}
                }
            }
            // `<d:collection/>` 这种自闭合写法才是常态。
            Event::Empty(e) => {
                let name = e.name();
                if in_prop
                    && local_name(name.as_ref()) == "collection"
                    && let Some(r) = cur.as_mut()
                {
                    r.is_dir = true;
                }
            }
            Event::Text(e) => {
                let text = text_of_str(e.as_ref());
                feed_text(&mut cur, field, in_prop, &prop, &text);
            }
            // 实体引用是独立事件，`as_ref()` 给的是 `&` 与 `;` 之间那截（`amp`、`#20013`），
            // 所以拼回 `&…;` 再走同一个反转义。认不出的实体（`&nbsp;` 这类）保留原文：
            // 名字里多个看得见的 `&nbsp;` 是能看见的问题，静默少一截书名是不能发现的。
            Event::GeneralRef(e) => {
                let text = text_of_str(&format!("&{};", e.as_ref()));
                feed_text(&mut cur, field, in_prop, &prop, &text);
            }
            Event::End(e) => {
                let name = e.name();
                match local_name(name.as_ref()) {
                    "response" => {
                        if let Some(raw) = cur.take() {
                            out.push(raw);
                        }
                    }
                    "prop" => in_prop = false,
                    "href" | "status" | "displayname" | "getlastmodified" | "getetag" => {
                        field = None
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// 把原始条目收成「这一层的内容」：反解 `href`、扔掉查不到的、砍掉深层的。
fn children(endpoint: &Endpoint, raw: &[Raw]) -> Vec<Entry> {
    let prefix = endpoint.path_prefix();
    let mut out = Vec::new();
    for r in raw {
        if let Some(code) = r.status
            && !(200..300).contains(&code)
        {
            // 「远端少了一个文件」的两种成因之一：服务器把查不到的资源也塞回 207 里。
            tracing::debug!("dav: 资源 {} 自己回了 {code}，不收", r.href);
            continue;
        }
        let Some((path, had_slash)) = relative_path(prefix, &r.href) else {
            // 另一种：`href` 的基准对不上（代理前缀、虚拟目录）。这条日志是唯一的线索。
            tracing::debug!("dav: 认不出 {} 相对基准 {prefix} 的路径，不收", r.href);
            continue;
        };
        // 相对路径为空就是被查询的集合自己 —— 它出现在返回里是协议规定的，但它不属于
        // 「这一层的内容」，留着它列表里就会多出一行指向父层。
        if path.is_empty() {
            continue;
        }
        // 只留一层。`Depth: 1` 不被尊重时整棵树都会回来，按段数裁比按条数或时间猜可靠。
        if path.split('/').count() > 1 {
            continue;
        }
        let name = match (r.display.trim().is_empty(), path.split('/').next_back()) {
            (true, Some(tail)) => tail.to_string(),
            (true, None) => path.clone(),
            (false, _) => r.display.trim().to_string(),
        };
        out.push(Entry {
            name,
            path,
            is_dir: r.is_dir || had_slash,
            size: r.size,
            modified: blank_to_none(&r.modified),
            etag: blank_to_none(&r.etag),
        });
    }
    out
}

/// `href` → （相对路径, 原本是否带尾斜杠）。认不出基准时返回 `None`，由调用方丢弃这一条。
///
/// 这是整个客户端最容易错的一步：`href` 是**编码态**的，而基准各家不同 —— 绝对路径
/// (`/dav/books/a.epub`)、直接给相对 (`a.epub`)、前面多一段代理前缀 (`/proxy/dav/books/…`)
/// 三种都真实存在，所以按「先试前缀 → 再试它本来就是相对 → 最后在任何位置找一次前缀」
/// 依次退让。
fn relative_path(prefix: &str, href: &str) -> Option<(String, bool)> {
    let decoded = percent_decode(href);
    // 少数实现会把查询串或片段带回来，它们不属于路径。
    let clean = decoded
        .split(['?', '#'])
        .next()
        .unwrap_or(decoded.as_str())
        .trim();
    if clean.is_empty() {
        return None;
    }
    let had_slash = clean.ends_with('/');
    let rest = if let Some(r) = clean.strip_prefix(prefix) {
        r
    } else if !clean.starts_with('/') {
        clean
    } else {
        // 代理前缀：`prefix` 出现在 href 里但不是开头。找得到就按它裁，找不到就丢。
        let at = clean.find(prefix)?;
        &clean[at + prefix.len()..]
    };
    let rest = rest.trim_matches('/').replace('\\', "/");
    Some((rest, had_slash))
}

/// 路径逐段百分号编码，`/` 保留成分隔符。
///
/// 中文书名、空格、`#`、`%` 在真实书库里是常态而不是边角情况：`#` 不编码会被当成片段起点，
/// `?` 会被当成查询开始，`%` 不编码会让「这段到底编过没」无法判断。所以是白名单制 ——
/// 名单外一律转义，而不是只处理「看着危险」的那几个。
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if is_path_safe(ch) {
            out.push(ch);
        } else {
            for byte in ch.encode_utf8(&mut [0u8; 4]).as_bytes() {
                out.push('%');
                out.push_str(&format!("{byte:02X}"));
            }
        }
    }
    out
}

/// URL 路径里允许原样出现的字符。`/` 在名单里是因为它是分隔符而不是内容。
fn is_path_safe(ch: char) -> bool {
    matches!(
        ch,
        'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | '/'
    )
}

/// 百分号解码。非法的 `%` 序列（后面不是两个十六进制位）原样保留：书名里会出现字面的 `%`，
/// 而有些服务器偏偏不给它编码，此时把整条当错误扔掉就等于少一本书。
///
/// `+` **不**解码成空格 —— 那是表单编码的规矩，URL 路径里的空格是 `%20`。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push(hi as u8 * 16 + lo as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 从 `"HTTP/1.1 404 Not Found"` 里取那个三位数。
fn status_code(text: &str) -> Option<u16> {
    text.split_whitespace()
        .filter(|w| w.len() == 3 && w.bytes().all(|b| b.is_ascii_digit()))
        .find_map(|w| w.parse().ok())
}

fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn text_of_str(raw: &str) -> String {
    quick_xml::escape::unescape(raw)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

fn push(cur: &mut Option<Raw>, f: impl FnOnce(&mut Raw)) {
    if let Some(r) = cur {
        f(r);
    }
}

fn blank_to_none(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// 一个「第一次 poll 就给出结果」的假传输，用来把 [`Client`] 的整套动作测完而不碰网络。
#[cfg(test)]
pub(crate) struct Mock {
    /// 依次给出的响应；用尽之后还被调用就直接 panic —— 用例写错要炸在这里，而不是静默
    /// 拿到一个空响应，把断言变成永真。
    answers: Mutex<VecDeque<Response>>,
    pub(crate) seen: Mutex<Vec<Request>>,
    chunks: Vec<Vec<u8>>,
}

#[cfg(test)]
impl Mock {
    pub(crate) fn new(responses: Vec<Response>) -> Self {
        Self {
            answers: Mutex::new(responses.into()),
            seen: Mutex::new(Vec::new()),
            chunks: vec![b"abc".to_vec(), b"defg".to_vec()],
        }
    }

    pub(crate) fn last_request(&self) -> Request {
        self.seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("假传输没有收到任何请求")
    }

    fn answer(&self, req: Request) -> Result<Response> {
        self.seen.lock().unwrap().push(req);
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| Error::Malformed("假传输的响应已用尽：用例少给了一个".into()))
    }
}

#[cfg(test)]
impl Transport for Mock {
    fn request(&self, req: Request) -> super::Result<Response> {
        self.answer(req)
    }

    /// 照 [`Transport::download`] 的契约实现：块之间检查取消，结束时 `finish` 与 `abort`
    /// 恰好调一个。真实实现守着同一条，所以这条契约在这里被当协议测。
    fn download(&self, req: Request, sink: &mut dyn Sink, cancel: &Cancel) -> super::Result<u64> {
        // 先让 `download` 也能被「没给响应」的用例测到，再决定要不要写块。
        self.answer(req)?;
        let mut written = 0u64;
        for chunk in &self.chunks {
            if cancel.triggered() {
                sink.abort();
                return Err(Error::Cancelled);
            }
            match sink.write(chunk) {
                Ok(()) => written += chunk.len() as u64,
                Err(e) => {
                    sink.abort();
                    return Err(Error::Io(e));
                }
            }
        }
        match sink.finish() {
            Ok(()) => Ok(written),
            Err(e) => {
                sink.abort();
                Err(Error::Io(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> Endpoint {
        Endpoint::new("https://dav.example.com/dav/books", "noke", "app-pass")
    }

    /// 日志文件会被贴进工单，所以地址里那一段凭据不能跟着进去 —— 而路径要留着，
    /// 排查要的正是它。
    #[test]
    fn a_url_with_credentials_in_it_logs_without_them() {
        assert_eq!(
            url_for_log("https://noke:app-pass@dav.example.com/dav/books/蜗牛.epub"),
            "https://***@dav.example.com/dav/books/蜗牛.epub"
        );
        // 没写凭据时要原样，不能把主机当 userinfo 吃掉。
        assert_eq!(
            url_for_log("https://dav.example.com/dav/books/蜗牛.epub"),
            "https://dav.example.com/dav/books/蜗牛.epub"
        );
        // 路径里的 `@`（邮箱式目录名）不算凭据。
        assert_eq!(
            url_for_log("https://dav.example.com/a@b/c"),
            "https://dav.example.com/a@b/c"
        );
    }

    fn client(responses: Vec<Response>) -> (Client, Arc<Mock>) {
        let mock = Arc::new(Mock::new(responses));
        (
            Client::new(endpoint(), Arc::clone(&mock) as Arc<dyn Transport>),
            mock,
        )
    }

    /// 一份最小可用的 207：被查询的集合自己 + 一个中文目录 + 一个带全属性的大文件。
    const SHELF: &str = r#"<?xml version="1.0"?>
    <d:multistatus xmlns:d="DAV:">
      <d:response>
        <d:href>/dav/books/</d:href>
        <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>
      </d:response>
      <d:response>
        <d:href>/dav/books/%E6%96%97%E6%88%98%E7%8B%82%E6%BD%AE/</d:href>
        <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat>
      </d:response>
      <d:response>
        <d:href>/dav/books/shejie.epub</d:href>
        <d:propstat><d:prop>
          <d:resourcetype/>
          <d:getcontentlength>15144675</d:getcontentlength>
          <d:getlastmodified>Sat, 11 Dec 2019 22:55:00 GMT</d:getlastmodified>
          <d:getetag>"a1b2"</d:getetag>
        </d:prop></d:propstat>
      </d:response>
    </d:multistatus>"#;

    fn ok207(xml: &str) -> Response {
        Response::new(207, xml.as_bytes().to_vec())
    }

    #[test]
    fn entity_in_a_display_name_survives_whole() {
        // 书名里带 `&` 与实体的真实样子。解析器要是把文本事件按「一见就清标记」处理，
        // 这里就会只剩头一段；`&amp;` 不被反转义则会把 `&` 原样留在名字里。
        let xml = r#"<d:multistatus xmlns:d="DAV:"><response>
          <d:href>/dav/books/a.epub</d:href>
          <d:propstat><d:prop>
            <d:displayname>蜗牛 &amp; 熊 &#20013;，与“引号”</d:displayname>
          </d:prop></d:propstat>
        </response></d:multistatus>"#;
        let (client, _mock) = client(vec![ok207(xml)]);
        let entries = client.list("").unwrap();
        assert_eq!(entries[0].name, "蜗牛 & 熊 中，与“引号”");
    }

    #[test]
    fn list_returns_one_level_with_decoded_paths() {
        let (client, mock) = client(vec![ok207(SHELF)]);
        let entries = client.list("").expect("应当解析出列表");
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        // 被查询的集合自己不该出现在结果里。
        assert_eq!(paths, ["斗战狂潮", "shejie.epub"]);
        assert!(entries[0].is_dir, "目录靠 resourcetype 里的 collection 判");
        assert!(!entries[1].is_dir);
        assert_eq!(entries[1].size, Some(15144675));
        assert_eq!(entries[1].etag.as_deref(), Some("\"a1b2\""));
        assert_eq!(
            entries[1].modified.as_deref(),
            Some("Sat, 11 Dec 2019 22:55:00 GMT")
        );
        assert_eq!(mock.last_request().method, Method::Propfind);
    }

    #[test]
    fn propfind_carries_depth_one_and_basic_credentials() {
        let (client, mock) = client(vec![ok207(SHELF)]);
        client.list("").unwrap();
        let req = mock.last_request();
        // 钉住这个值：以后改凭据来源（钥匙串、应用密码）时这条必须跟着变，
        // 否则等于悄悄换了认证方式而没人知道。`noke:app-pass` 的 base64。
        assert_eq!(
            req.headers[0],
            ("Authorization".into(), "Basic bm9rZTphcHAtcGFzcw==".into())
        );
        assert!(
            req.headers.iter().any(|(k, v)| k == "Depth" && v == "1"),
            "PROPFIND 必须带 Depth"
        );
        assert_eq!(req.url, "https://dav.example.com/dav/books/");
        assert!(!req.body.is_empty());
    }

    #[test]
    fn paths_are_encoded_segment_by_segment() {
        assert_eq!(
            encode_path("斗战狂潮/第 1 章.epub"),
            "%E6%96%97%E6%88%98%E7%8B%82%E6%BD%AE/%E7%AC%AC%201%20%E7%AB%A0.epub"
        );
        // 这三个字符不编码会改变 URL 的结构，不是「不好看」的问题。
        assert_eq!(encode_path("a#b"), "a%23b");
        assert_eq!(encode_path("a?b"), "a%3Fb");
        assert_eq!(encode_path("50%"), "50%25");
        assert_eq!(encode_path(""), "");
    }

    #[test]
    fn decode_then_encode_round_trips_a_percent_in_the_name() {
        // 服务器给 `a%25b.epub`，解出 `a%b.epub`，再编码必须回到 `a%25b.epub`。
        // 任何一步漏了就会二次编码成 `%2525`，然后 404 —— 而且只在带 `%` 的书名上。
        let decoded = percent_decode("a%25b.epub");
        assert_eq!(decoded, "a%b.epub");
        assert_eq!(encode_path(&decoded), "a%25b.epub");
    }

    #[test]
    fn lone_percent_survives_instead_of_eating_the_entry() {
        // 有的服务器不编码字面 `%`。解码器既不能丢条目也不能崩。
        assert_eq!(percent_decode("100%25"), "100%");
        assert_eq!(percent_decode("100%zz"), "100%zz");
        assert_eq!(percent_decode("100%"), "100%", "末尾断掉的序列原样留着");
        assert_eq!(
            percent_decode("+"),
            "+",
            "`+` 是表单编码的规矩，路径里不适用"
        );
    }

    #[test]
    fn href_base_variants_all_resolve() {
        let prefix = "/dav/books/";
        assert_eq!(
            relative_path(prefix, "/dav/books/a.epub").unwrap().0,
            "a.epub"
        );
        assert_eq!(relative_path(prefix, "a.epub").unwrap().0, "a.epub");
        assert_eq!(
            relative_path(prefix, "/proxy/dav/books/a.epub").unwrap().0,
            "a.epub"
        );
        assert_eq!(
            relative_path(prefix, "/dav/books/%E5%AD%90/").unwrap().0,
            "子"
        );
        // 认不出基准就丢弃，别猜一个路径出来：猜错的表现是下载 404，比列表少一项更难查。
        assert!(relative_path(prefix, "/srv/other/a.epub").is_none());
        // 尾斜杠要带出来，它是「这是个目录」的第二条证据。
        assert!(relative_path(prefix, "/dav/books/sub/").unwrap().1);
        // 集合自己 → 空串，由 `children` 负责丢弃。
        assert_eq!(relative_path(prefix, "/dav/books/").unwrap().0, "");
    }

    #[test]
    fn unprefixed_multistatus_parses_too() {
        // 默认命名空间的写法（不带 `d:` 前缀）在真实服务器上同样存在。
        let xml = r#"<multistatus xmlns="DAV:"><response><href>/dav/books/a.epub</href>
        <propstat><prop><getcontentlength>10</getcontentlength></prop></propstat></response></multistatus>"#;
        let (client, _mock) = client(vec![ok207(xml)]);
        let entries = client.list("").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "a.epub");
        assert_eq!(entries[0].size, Some(10));
        assert_eq!(
            entries[0].name, "a.epub",
            "没有 displayname 时用路径末段顶名"
        );
    }

    #[test]
    fn self_row_dropped_deep_rows_cut_failed_rows_dropped() {
        // 一台无视 `Depth: 1` 的服务器：整棵树都回来了，还夹着一条 404。
        let xml = r#"<d:multistatus xmlns:d="DAV:">
          <d:response><d:href>/dav/books/</d:href></d:response>
          <d:response><d:href>/dav/books/keep.epub</d:href></d:response>
          <d:response><d:href>/dav/books/sub/deep.epub</d:href></d:response>
          <d:response><d:href>/dav/books/gone.epub</d:href>
            <d:status>HTTP/1.1 404 Not Found</d:status></d:response>
        </d:multistatus>"#;
        let (client, _mock) = client(vec![ok207(xml)]);
        let entries = client.list("").unwrap();
        assert_eq!(
            entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
            ["keep.epub"]
        );
    }

    #[test]
    fn displayname_wins_over_the_path_tail() {
        let xml = r#"<d:multistatus xmlns:d="DAV:"><response>
          <d:href>/dav/books/x-123.epub</d:href>
          <d:propstat><d:prop><d:displayname>斗战狂潮.epub</d:displayname></d:prop></d:propstat>
        </response></d:multistatus>"#;
        let (client, _mock) = client(vec![ok207(xml)]);
        let entries = client.list("").unwrap();
        assert_eq!(entries[0].name, "斗战狂潮.epub");
        assert_eq!(
            entries[0].path, "x-123.epub",
            "显示名可以换，取文件的路径不能跟着换"
        );
    }

    #[test]
    fn digest_only_server_gets_its_own_error() {
        let mut resp = Response::new(401, Vec::new());
        resp.headers
            .push(("www-authenticate".into(), "Digest realm=\"dav\"".into()));
        assert!(matches!(
            check_status(&resp, &[207]),
            Err(Error::DigestRequired)
        ));

        // 同时提供 Basic 的服务器要给「认证被拒」，不是「不支持 Digest」—— 这种是能修好的。
        let mut both = Response::new(401, Vec::new());
        both.headers.push((
            "WWW-Authenticate".into(),
            "Basic realm=\"dav\", Digest realm=\"dav\"".into(),
        ));
        assert!(matches!(
            check_status(&both, &[207]),
            Err(Error::Unauthorized { status: 401 })
        ));
        // 头完全没有（有些实现就只回 401）也要落在「认证被拒」这一档。
        assert!(matches!(
            check_status(&Response::new(401, Vec::new()), &[207]),
            Err(Error::Unauthorized { status: 401 })
        ));
    }

    #[test]
    fn other_status_shapes_say_what_they_are() {
        assert!(check_status(&Response::new(207, Vec::new()), &[207, 200]).is_ok());
        assert!(matches!(
            check_status(&Response::new(404, Vec::new()), &[207]),
            Err(Error::NotFound)
        ));
        assert!(matches!(
            check_status(&Response::new(403, Vec::new()), &[207]),
            Err(Error::Forbidden { status: 403 })
        ));
        // 507 是配额不足，备份写不进去时的真实答案，别只报「未知错误」。
        assert!(matches!(
            check_status(&Response::new(507, Vec::new()), &[201]),
            Err(Error::Unexpected { status: 507 })
        ));
        let err = check_status(&Response::new(405, Vec::new()), &[207]).unwrap_err();
        assert!(err.to_string().contains("WebDAV"), "{err}");
    }

    #[test]
    fn list_of_a_subdirectory_queries_that_collection_url() {
        let (client, mock) = client(vec![ok207(SHELF)]);
        client.list("斗战狂潮").unwrap();
        assert_eq!(
            mock.last_request().url,
            "https://dav.example.com/dav/books/%E6%96%97%E6%88%98%E7%8B%82%E6%BD%AE/"
        );
    }

    #[test]
    fn download_fills_the_sink_then_finishes_it_once() {
        // sink 通过一份共享记录留下自己的结局，因为 `Box<dyn Sink>` 的所有权进了传输层，
        // 调用方拿不回来 —— 真实实现（写到临时文件再改名）也是这个形状。
        #[derive(Clone, Default)]
        struct Log(Arc<Mutex<Vec<String>>>);
        impl Sink for Log {
            fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("write:{}", chunk.len()));
                Ok(())
            }
            fn finish(&mut self) -> std::io::Result<()> {
                self.0.lock().unwrap().push("finish".into());
                Ok(())
            }
            fn abort(&mut self) {
                self.0.lock().unwrap().push("abort".into());
            }
        }
        let log = Log::default();
        let (client, _mock) = client(vec![Response::new(200, Vec::new())]);
        let n =
            client.download("a.epub", &mut log.clone(), &Cancel::new()).unwrap();
        assert_eq!(n, 7, "abc + defg");
        assert_eq!(
            log.0.lock().unwrap().as_slice(),
            ["write:3", "write:4", "finish"],
            "finish 恰好一次，abort 一次没有"
        );
    }

    #[test]
    fn cancelled_download_aborts_instead_of_finishing() {
        #[derive(Clone, Default)]
        struct Log(Arc<Mutex<Vec<String>>>);
        impl Sink for Log {
            fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("write:{}", chunk.len()));
                Ok(())
            }
            fn finish(&mut self) -> std::io::Result<()> {
                self.0.lock().unwrap().push("finish".into());
                Ok(())
            }
            fn abort(&mut self) {
                self.0.lock().unwrap().push("abort".into());
            }
        }
        let log = Log::default();
        let (client, _mock) = client(vec![Response::new(200, Vec::new())]);
        let cancel = Cancel::new();
        cancel.trigger();
        let err = client.download("a.epub", &mut log.clone(), &cancel)
            .expect_err("取消要当成一次失败返回");
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        assert_eq!(log.0.lock().unwrap().as_slice(), ["abort"]);
    }

    #[test]
    fn put_and_mkdir_target_the_same_escaped_url() {
        let (client, mock) = client(vec![
            Response::new(201, Vec::new()),
            Response::new(204, Vec::new()),
        ]);
        client.mkdir("备份/书库").unwrap();
        client.put("备份/书库/folio.json", b"{}".to_vec()).unwrap();
        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen[0].method, Method::Mkcol);
        assert!(
            seen[0].url.ends_with('/') && seen[0].url.contains("%E5%A4%87"),
            "{}",
            seen[0].url
        );
        assert_eq!(seen[1].method, Method::Put);
        assert_eq!(seen[1].body, b"{}");
        assert!(!seen[1].url.ends_with('/'), "写文件不该带尾斜杠");
        assert!(
            seen[1].headers.iter().any(|(k, _)| k == "Content-Type"),
            "PUT 要声明类型"
        );
    }

    #[test]
    fn mkdir_on_an_existing_collection_counts_as_success() {
        // 第二次跑备份会撞「目录已存在」，各家给的状态码不统一，流程不该因此中断。
        for status in [405u16, 409, 423] {
            let (client, _mock) = client(vec![Response::new(status, Vec::new())]);
            client.mkdir("备份").expect("已存在就等于可用");
        }
    }

    #[test]
    fn status_code_extraction_tolerates_the_reason_phrase() {
        assert_eq!(status_code("HTTP/1.1 404 Not Found"), Some(404));
        assert_eq!(status_code("HTTP/1.0 200 OK"), Some(200));
        assert_eq!(status_code("not a status"), None);
    }

    #[test]
    fn endpoint_normalizes_the_base_and_exposes_the_path_prefix() {
        assert_eq!(
            Endpoint::new("https://host/dav", "u", "p").base,
            "https://host/dav/"
        );
        assert_eq!(
            Endpoint::new("https://host/dav/", "u", "p").path_prefix(),
            "/dav/"
        );
        assert_eq!(
            Endpoint::new("https://host", "u", "p").path_prefix(),
            "/",
            "根挂载点也要有个能比对的前缀"
        );
    }
}
