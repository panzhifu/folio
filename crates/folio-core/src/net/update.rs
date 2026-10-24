//! 检查更新：GitHub release 那一问，与「这份二进制能不能被它更新」。
//!
//! 这一层**只回答三个问题**：最新那版是多少、它的更新日志写了什么、这一端该下哪个文件。
//! 下载、校验之后的落地、拉起安装器都不在这儿 —— 那些要么要文件系统与线程，要么要 JNI，
//! 全都不是 core 能碰的（core 不许依赖 gpui，见 `lib.rs` 顶部）。
//!
//! ## 问的是本仓库的 release
//!
//! 源码与安装包现在在同一个公开仓库 `panzhifu/folio` 上（2026-10-24 起；此前源码私有、包发在
//! 另一个公开仓，那需要一个读者提不出、CI 又写得动的第二份凭据，代价比它解决的问题多）。
//! 这一问**匿名可读**，限流按读者自己的 IP 计（60 次/小时），不需要也不许内置任何 token：
//! 客户端里的密钥人人能提出来，那等于把发布权限发给所有读者 —— 下面那条
//! `the_request_...` 测试钉的就是这一条，不只是 URL 长什么样。
//!
//! ## 为什么用同步 `Transport`
//!
//! 与 `net/remote/mod.rs` 同一条理由：出网的作业跑在后台线程上，「一个请求流到底」用阻塞读
//! 最直白，core 里因此不需要任何执行器。界面那侧怎么把它丢回主线程是 `folio-app` 的事。
//!
//! ## 错误为什么分三档，而且不许合并
//!
//! 「连不上」「被 GitHub 限流」「这仓库还没发过版」是三种完全不同的事实：第一种读者能自己修
//! （换个网络），第二种要等，第三种意味着**发布那边坏了**而不是读者这边。合并成一档
//! 「检查失败」会让人去翻自己家的路由器。所以 [`Error`] 是那三句话的形状，界面直接照着念。

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use super::remote::{Cancel, Error as TransportError, Method, Request, Sink, Transport};

/// 最新发布版那条 release 的 API 地址。
pub const LATEST_API: &str = "https://api.github.com/repos/panzhifu/folio/releases/latest";

/// 按 tag 取**某一条指定** release 的地址模板（`{tag}` 由 [`release_by_tag`] 填）。
///
/// 要这一问而不是只用 `latest`，是因为「更新日志」那一格显示的是**手上这一版**改了什么：
/// 装了 0.1.1 而最新是 0.2.0 时，`latest` 的正文讲的是 0.2.0，把它当「你这版的说明」给读者
/// 看是一句假话。
pub const RELEASE_BY_TAG_API: &str = "https://api.github.com/repos/panzhifu/folio/releases/tags/";

/// 给读者在浏览器里打开的那一页（检查失败时也要有个地方可去）。
pub const RELEASES_PAGE: &str = "https://github.com/panzhifu/folio/releases/latest";

/// 一次检查的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// 手上这份已经是最新。远端**更旧**也走这一档，理由写在 [`check`] 的文档上。
    Current,
    /// 有更新的版本可用，带着它的正文与资产。
    Newer(Release),
}

/// 一条 release 里 folio 用得上的那几样。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// `tag_name` 去掉 `v` 前缀之后的版本。
    pub version: Version,
    /// release 正文（Markdown），就是「更新日志」那一格显示的东西。
    pub notes: String,
    /// 发布仓库里挂的那些文件。
    pub assets: Vec<Asset>,
}

/// 一个安装包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    /// 下载直链（会 302 跳到 `objects.githubusercontent.com`）。
    pub url: String,
    /// 字节数。界面上「这个包 36 MB，要下吗」那一句话就靠它。
    pub size: u64,
    /// GitHub 给的 `sha256:<hex>`；老 release 或某些端点可能没有，所以是 `Option`。
    pub digest: Option<String>,
}

impl Asset {
    /// 剥掉 `sha256:` 前缀的小写十六进制摘要；没有、长度不对、或带非十六进制字符时 `None`。
    ///
    /// 长度这一关不是洁癖：摘要字段将来可能换成别的算法（`sha512:…`），那时**认不出来**比
    /// 「拿一段 128 位的串去比 64 位的期望值」要好 —— 后者会安静地报「校验不过」。
    pub fn sha256_hex(&self) -> Option<&str> {
        let raw = self.digest.as_deref()?;
        let hex = raw.strip_prefix("sha256:").unwrap_or(raw).trim();
        (hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(hex)
    }
}

/// 一次检查失败的原因，三档各有一句人话可说（见模块顶部那段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// DNS / 连接 / TLS / 超时：请求根本没成为一次 HTTP 往返。
    Unreachable(String),
    /// 403。未登录打 GitHub 的配额是 60 次/小时/IP，所以这一档**几乎总是**限流而不是禁访。
    RateLimited,
    /// 404，或 release 列表是空的：这仓库还没发过版。
    NoRelease,
    /// 认不出的状态码。带着它，界面上那句「服务器返回 5xx」才是真话。
    Unexpected { status: u16 },
    /// 响应读不懂：不是 JSON、缺 `tag_name`、版本号不像版本号。
    Malformed(String),
}

impl fmt::Display for Error {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(why) => write!(out, "连不上：{why}"),
            Self::RateLimited => write!(out, "GitHub 对这个地址的匿名请求限流了（每小时 60 次）"),
            Self::NoRelease => write!(out, "这个发布仓库还没有公开过任何版本"),
            Self::Unexpected { status } => write!(out, "服务器返回 {status}"),
            Self::Malformed(why) => write!(out, "响应读不懂：{why}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// 这一档有没有一句**固定的**话可说（带上下文的那两档没有，它们自己就是那句话）。
    pub fn fixed_hint(&self) -> Option<&'static str> {
        match self {
            Self::RateLimited => Some("稍后再试一次；同一个网络下别人也用得掉这份配额"),
            Self::NoRelease => Some("作者还没把这一版发出去"),
            _ => None,
        }
    }
}

impl From<TransportError> for Error {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::Transport(message) => Self::Unreachable(message),
            // GitHub 的限流回 403，而 `remote` 那一层的 403 叫「没有权限」—— 对读者来说
            // 后者是一句会让人去翻账号设置的假话，所以在这儿换一次名。
            TransportError::Forbidden { .. } => Self::RateLimited,
            TransportError::NotFound => Self::NoRelease,
            TransportError::Unexpected { status } => Self::Unexpected { status },
            // 401 单独一档：folio **从不带凭据**，所以 401 意味着对面不是我们以为的那个端点。
            other => Self::Malformed(other.to_string()),
        }
    }
}

/// 问一次「最新是哪版」，并与手上这份比。
///
/// 比较结果**只有两种走向**：远端更新 → [`Outcome::Newer`]；其余（同版、更旧）→
/// [`Outcome::Current`]。远端更旧时不提示「降级到 vX」是刻意的：那条路对读者没有任何好处，
/// 而「已经是最新」在那一刻就是事实。
pub fn check(transport: &dyn Transport, local: &str) -> Result<Outcome, Error> {
    let raw_local = local;
    let local = parse_version(raw_local).map_err(|reason| {
        // 本地版本号是编译期写进二进制的，它读不懂不是网络事故 —— 报成 Malformed 才对得上症状。
        Error::Malformed(format!("本应用的版本号 {raw_local:?} 读不懂：{reason}"))
    })?;
    let request = Request::new(Method::Get, LATEST_API.to_owned())
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    let response = transport.request(request)?;
    let latest = parse_release(&response.body)?;
    if latest.version > local {
        Ok(Outcome::Newer(latest))
    } else {
        Ok(Outcome::Current)
    }
}

/// 取**指定那一版**的 release（「更新日志」那一格用它讲手上这份，而不是讲最新那份）。
///
/// `tag` 由调用方拼（`v{CARGO_PKG_VERSION}`）。这里对字符集做一次**白名单**检查：它是
/// 编译期写进二进制的那串，理论上不是输入，但它要进 URL 的**路径段** —— 一段带 `..` 或
/// `?` 的东西拼进路径，症状是打到别的端点上去，比在这里拒掉难查得多。
pub fn release_by_tag(transport: &dyn Transport, tag: &str) -> Result<Release, Error> {
    if tag.is_empty()
        || !tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(Error::Malformed(format!("{tag:?} 不像一个 tag")));
    }
    let request = Request::new(Method::Get, format!("{RELEASE_BY_TAG_API}{tag}"))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    let response = transport.request(request)?;
    parse_release(&response.body)
}

/// 只解析，不比较：给「更新日志」那一格用（它要的是**当前这版**的正文，不需要先检查更新）。
pub fn parse_release(body: &[u8]) -> Result<Release, Error> {
    let raw: RawRelease = serde_json::from_slice(body)
        .map_err(|error| Error::Malformed(format!("响应不是预期的 JSON：{error}")))?;
    let tag = raw.tag_name.trim();
    if tag.is_empty() {
        return Err(Error::Malformed("release 里没有 tag_name".to_owned()));
    }
    let version = parse_version(tag).map_err(Error::Malformed)?;
    Ok(Release {
        version,
        notes: raw.body.unwrap_or_default(),
        assets: raw
            .assets
            .into_iter()
            .map(|asset| Asset {
                name: asset.name,
                url: asset.browser_download_url,
                size: asset.size,
                digest: asset.digest,
            })
            .collect(),
    })
}

#[derive(serde::Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(serde::Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

/// 挑这一端该下的那个文件。
///
/// 判据是**名字后缀**，不是「第一个像安装包的」：挑错一个 `.deb` 去给 Android 装，症状是
/// 安装器报一句看不懂的错，而读者会以为包坏了。挑不到就返回 `None`，界面那句是
/// 「这一端暂时没有可用的包」—— 那是真话，比硬凑一个下载好。
pub fn asset_for_platform(release: &Release) -> Option<&Asset> {
    let suffix: &str = if cfg!(target_os = "android") {
        "-arm64-v8a.apk"
    } else if cfg!(target_os = "macos") {
        "-macos-arm64.zip"
    } else if cfg!(target_os = "windows") {
        "-windows-x64.exe"
    } else if cfg!(target_os = "linux") {
        "-linux-x86_64.tar.xz"
    } else {
        // HarmonyOS / iOS：这一版不发安装包（见 README「发版」），所以这里没有候选。
        return None;
    };
    release
        .assets
        .iter()
        .find(|asset| asset.name.ends_with(suffix))
}

/// 按**完整文件名**挑（校验和清单要用名字对齐，名字比后缀可靠）。
pub fn asset_named<'a>(release: &'a Release, name: &str) -> Option<&'a Asset> {
    release.assets.iter().find(|asset| asset.name == name)
}

/// 解析 `sha256sums.txt` 那一族行：`<hex>  <文件名>`（两个空格是 `sha256sum` 文本模式的形状）。
///
/// 认不出的行**跳过而不是报错**：那份文件是 CI 现生成的，多一行空行或注释不该让读者的
/// 一次更新检查失败；但真要用的那一行缺失就是缺失（返回的表里没有那个键，调用方据此报
/// 「包不完整」）。
pub fn parse_checksums(text: &str) -> HashMap<String, String> {
    let mut sums = HashMap::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // 文本模式两个空格；二进制模式是一个空格加 `*`。两种都收。
        let split = match line.split_once("  ") {
            Some(half) => Some(half),
            None => line
                .split_once(' ')
                .map(|(digest, name)| (digest, name.strip_prefix('*').unwrap_or(name))),
        };
        let Some((digest, name)) = split else {
            continue;
        };
        let digest = digest.trim();
        if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            sums.insert(name.trim().to_owned(), digest.to_ascii_lowercase());
        }
    }
    sums
}

/// 算一个文件的 SHA-256（流式，不把 30 MB 的包读进内存）。
pub fn sha256_of_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// 文件内容与期望摘要是否一致。**期望值缺失时返回 `false`**：没有可比的东西就是没验过。
pub fn matches_sha256(path: &Path, expected_hex: Option<&str>) -> bool {
    let Some(expected) = expected_hex else {
        return false;
    };
    match sha256_of_file(path) {
        Ok(actual) => equal_ignore_case(&actual, expected.trim()),
        // 读不到文件（被删了、权限不对）与「校验不过」在界面上是同一句话：包不完整。
        Err(_) => false,
    }
}

/// 大小写无关地比两段十六进制，且不提前返回。
///
/// 摘要校验不是秘密比较（构造碰撞要能拿到同一个文件），选不短路的写法只有一个理由：
/// 将来有人把这条复用到真正秘密的地方时，提前返回那一族 bug 不会等他踩。
fn equal_ignore_case(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a.to_ascii_lowercase() ^ b.to_ascii_lowercase();
    }
    diff == 0
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// 把包下到 `dest`，边下边报进度，可取消。
///
/// 三件事只有这一处管：
///
/// - **临时名 + 改名**。半截文件留在 `dest` 上的话，下一次「校验通过」校验的是一个残缺的包，
///   而症状是安装器报一句看不懂的错。所以先写 `dest.part`，`Sink::finish` 才改名。
/// - **下之前先清掉上一轮的残留**。一个 APK 三十来 MB，装在读者手机上不是我们该占的地方；
///   下载目录里同一时刻只该有一个候选。
/// - **取消只在块与块之间生效**（[`super::remote::Transport::download`] 那条契约），
///   所以「取消」之后最多还会多写一个块，不会留下半开的 fd。
pub fn download_to_file(
    transport: &dyn Transport,
    url: &str,
    dest: &Path,
    cancel: &Cancel,
    progress: impl FnMut(u64) + Send,
) -> Result<(), Error> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::Unreachable(format!("建不出目录：{e}")))?;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                // 只清 `.apk` 与 `.part`：那一目录里读者的东西我们不碰。
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.ends_with(".apk") || name.ends_with(".part") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
    let part = part_path(dest);
    let mut sink = FileSink {
        file: std::fs::File::create(&part)
            .map_err(|e| Error::Unreachable(format!("写不出 {part:?}：{e}")))?,
        written: 0,
        dest: dest.to_path_buf(),
        part: part.clone(),
        progress,
    };
    let request = Request::new(Method::Get, url.to_owned());
    match transport.download(request, &mut sink, cancel) {
        Ok(_) => Ok(()),
        Err(super::remote::Error::Cancelled) => Err(Error::Unreachable("已取消".into())),
        Err(other) => Err(other.into()),
    }
}

/// 下载中的那个临时名：`<本名>.part`。
fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".into());
    name.push_str(".part");
    dest.with_file_name(name)
}

/// `F` 是泛型而不是 `Box<dyn FnMut>`：装箱会逼出一条 `'static` 要求，
/// 而调用处（界面）恰恰要给的是一个借用局部状态的闭包。那条要求对这里没有任何好处。
struct FileSink<F> {
    file: std::fs::File,
    written: u64,
    dest: PathBuf,
    part: PathBuf,
    progress: F,
}

// `Sink: Send`，所以进度闭包也得是 Send：它会在下载线程上被调。
impl<F: FnMut(u64) + Send> Sink for FileSink<F> {
    fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        use std::io::Write as _;
        self.file.write_all(chunk)?;
        self.written += chunk.len() as u64;
        (self.progress)(self.written);
        Ok(())
    }

    fn finish(&mut self) -> std::io::Result<()> {
        use std::io::Write as _;
        self.file.flush()?;
        // 改名成功才算下完：失败时留着 `.part`，下一次开头会被清掉。
        std::fs::rename(&self.part, &self.dest)
    }

    fn abort(&mut self) {
        let _ = std::fs::remove_file(&self.part);
    }
}

/// 一个版本号：`主.次.修订` 加一个可选的预发布后缀。
///
/// **不是完整的 semver 实现**，是它的一个够用子集；两处简化都写在下面。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// 预发布标记（`rc1`、`alpha.2`）。`None` 是正式版。
    pub pre: Option<String>,
}

/// 序关系**手写**，不用 derive —— 这是这一层最容易判反的一处：
///
/// `#[derive(PartialOrd, Ord)]` 会按字段顺序比，而 `Option` 的派生序是 `None < Some(_)`。
/// 于是「正式版」那个 `None` 排在「预发布版」前面，`1.2.0 < 1.2.0-rc1` —— 症状是 rc 用户
/// 永远收不到正式版，而**不会有任何报错**。
impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let triple =
            (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch));
        if triple != Ordering::Equal {
            return triple;
        }
        match (self.pre.as_deref(), other.pre.as_deref()) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            // 同三元组的两个预发布版：按字符串比。**这是简化**——完整 semver 里
            // 点分的数字段要按数值比（`alpha.2 < alpha.10`）。folio 的预发布只用
            // `-rc1`、`-rc2` 这种一位数后缀；真出现 `alpha.10` 那天再补这一段。
            (Some(a), Some(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.pre {
            Some(pre) => write!(out, "{}.{}.{}-{pre}", self.major, self.minor, self.patch),
            None => write!(out, "{}.{}.{}", self.major, self.minor, self.patch),
        }
    }
}

/// 解析 `0.1.1` / `v0.1.1` / `0.1` / `0.2.0-rc1`。
///
/// 两处有意的宽松：
/// - **接受 `v` 前缀**，因为 GitHub 的 tag 惯例就是 `v0.1.1`，而 `CARGO_PKG_VERSION` 没有。
///   不接受的话，比较的第一件事就是自己跟自己不一致。
/// - **接受两段**（`0.1` == `0.1.0`），因为 `Cargo.toml` 允许写它。
///
/// 一处有意的严格：**第四段一律不认**。`0.1.1.2` 是「谁在某一处手写了第四个号」的信号，
/// 把它悄悄截断成 `0.1.1` 会让下一次比较建立在一个没人同意的读法上。
pub fn parse_version(input: &str) -> Result<Version, String> {
    let trimmed = input.trim().trim_start_matches(['v', 'V']);
    let (core, pre) = match trimmed.split_once('-') {
        Some((core, pre)) => (core, Some(pre.to_owned())),
        None => (trimmed, None),
    };
    if core.is_empty() {
        return Err(format!("{input:?} 里没有 主.次.修订 那一段"));
    }
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() > 3 {
        return Err(format!("{input:?} 有 {} 段，只认三段以内", parts.len()));
    }
    let number = |part: &str| -> Result<u64, String> {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!("{input:?} 里 {part:?} 那一段不是数字"));
        }
        part.parse::<u64>()
            .map_err(|_| format!("{input:?} 里 {part:?} 大到装不进 u64"))
    };
    let major = number(parts[0])?;
    // `get` 给的是 `Option<&&str>`，`copied` 一下才对得上 `number` 的签名。
    let minor = parts.get(1).copied().map(number).transpose()?.unwrap_or(0);
    let patch = parts.get(2).copied().map(number).transpose()?.unwrap_or(0);
    if pre.as_ref().is_some_and(|pre| pre.is_empty()) {
        return Err(format!("{input:?} 带了一个空的预发布后缀"));
    }
    Ok(Version {
        major,
        minor,
        patch,
        pre,
    })
}

#[cfg(test)]
mod tests {
    //! 全部离线：一个假 `Transport` 递预先准备好的响应，无网络、无窗口。
    //! 比较与解析是这一层唯一会**安静**出错的地方 —— 判错一个版本，读者要么收不到更新，
    //! 要么被催着去装一个更旧的包，两种都不会有报错。

    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::super::remote::{Cancel, Request, Response, Sink, Transport};
    use super::*;

    struct Mock {
        answers: Mutex<VecDeque<Result<Response, TransportError>>>,
        seen: Mutex<Vec<Request>>,
    }

    impl Mock {
        fn ok(body: &str) -> Self {
            Self::new([Ok(Response::new(200, body.as_bytes().to_vec()))])
        }
        fn err(error: TransportError) -> Self {
            Self::new([Err(error)])
        }
        fn new(answers: impl IntoIterator<Item = Result<Response, TransportError>>) -> Self {
            Self {
                answers: Mutex::new(answers.into_iter().collect()),
                seen: Mutex::new(Vec::new()),
            }
        }
        fn last(&self) -> Request {
            self.seen
                .lock()
                .unwrap()
                .last()
                .cloned()
                .expect("假传输没有收到任何请求")
        }
    }

    impl Transport for Mock {
        fn request(&self, req: Request) -> super::super::remote::Result<Response> {
            self.seen.lock().unwrap().push(req);
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .expect("假传输的响应已用尽：用例少给了一个")
        }

        fn download(
            &self,
            _req: Request,
            _sink: &mut dyn Sink,
            _cancel: &Cancel,
        ) -> super::super::remote::Result<u64> {
            // 这一层不下东西；下载的用例住在 app 侧。
            Err(TransportError::Cancelled)
        }
    }

    /// 一条真形状 release JSON 的**最小版**：字段名与 GitHub 一致，多余字段一概不写。
    /// 多余字段不写是因为 serde 默认忽略未知字段，而那正是这里要的宽松 ——
    /// 见 [`a_release_with_fields_we_never_read_still_parses`]。
    fn release(tag: &str, body: &str, assets: &[(&str, u64)]) -> String {
        let assets: Vec<String> = assets
            .iter()
            .map(|(name, size)| {
                format!(
                    r#"{{"name":"{name}","browser_download_url":"https://example.test/{name}","size":{size},"digest":null}}"#
                )
            })
            .collect();
        format!(
            r#"{{"tag_name":"{tag}","name":"Folio {tag}","body":{},"assets":[{}]}}"#,
            serde_json::to_string(body).unwrap(),
            assets.join(",")
        )
    }

    #[test]
    fn a_tag_with_the_v_prefix_compares_against_a_bare_cargo_version() {
        // 判反的那一格：GitHub 给 `v0.1.2`，`env!("CARGO_PKG_VERSION")` 给 `0.1.1`。
        let mock = Mock::ok(&release("v0.1.2", "x", &[]));
        assert!(matches!(check(&mock, "0.1.1").unwrap(), Outcome::Newer(_)));
    }

    #[test]
    fn version_comparison_is_numeric_not_lexicographic() {
        // `0.1.10` 与 `0.1.9`：字典序判 `0.1.10 < 0.1.9`，于是第十个补丁版永远推不到任何人。
        assert!(parse_version("0.1.10").unwrap() > parse_version("0.1.9").unwrap());
        assert!(parse_version("0.2.0").unwrap() > parse_version("0.1.99").unwrap());
        assert!(parse_version("1.0.0").unwrap() > parse_version("0.99.99").unwrap());
        assert_eq!(
            parse_version("1.0.0").unwrap(),
            parse_version("v1.0.0").unwrap(),
            "`v` 前缀不该造出两个不同的版本"
        );
    }

    #[test]
    fn a_prerelease_is_older_than_its_final_version() {
        // 这条测的就是上面那段「别用 derive」：派生序会把 None 排在 Some 前面，
        // 于是 rc 用户永远收不到正式版，而且没有任何报错。
        let rc = parse_version("1.2.0-rc1").unwrap();
        let final_ = parse_version("1.2.0").unwrap();
        assert!(final_ > rc, "{final_} 应比 {rc} 新");
        assert!(rc < final_, "反方向也要对，别只测一边");
        assert_eq!(rc.to_string(), "1.2.0-rc1");
        assert_eq!(final_.to_string(), "1.2.0");
    }

    #[test]
    fn an_older_or_equal_remote_release_never_offers_a_downgrade() {
        for (remote, local) in [("v0.1.1", "0.1.1"), ("v0.1.0", "0.1.1")] {
            let mock = Mock::ok(&release(remote, "x", &[]));
            assert_eq!(
                check(&mock, local).unwrap(),
                Outcome::Current,
                "远端 {remote} 而本地 {local}：不该提示更新"
            );
        }
    }

    #[test]
    fn the_three_failure_kinds_stay_three_failure_kinds() {
        // 合并成一档就是这一格存在的理由：三种成因给读者三句不同的话。
        let cases: Vec<(TransportError, Error)> = vec![
            (
                TransportError::Transport("dns 解析失败".into()),
                Error::Unreachable("dns 解析失败".into()),
            ),
            (
                TransportError::Forbidden { status: 403 },
                Error::RateLimited,
            ),
            (TransportError::NotFound, Error::NoRelease),
            (
                TransportError::Unexpected { status: 502 },
                Error::Unexpected { status: 502 },
            ),
        ];
        for (from, want) in cases {
            let mock = Mock::err(from);
            assert_eq!(check(&mock, "0.1.1").unwrap_err(), want);
        }
        // 限流与「没发过版」各有一句人话，且两句不同 —— 界面念的就是它们。
        assert_ne!(
            Error::RateLimited.fixed_hint(),
            Error::NoRelease.fixed_hint()
        );
        assert!(Error::RateLimited.to_string().contains("限流"));
    }

    #[test]
    fn a_garbage_response_is_malformed_and_never_a_panic() {
        for body in [
            "",
            "not json",
            "{}",
            r#"{"tag_name":""}"#,
            r#"{"tag_name":"0.1.1.2"}"#,
        ] {
            let mock = Mock::ok(body);
            let error = check(&mock, "0.1.1").unwrap_err();
            assert!(
                matches!(error, Error::Malformed(_)),
                "响应 {body:?} 应报「读不懂」，实际 {error:?}"
            );
        }
    }

    #[test]
    fn a_release_with_fields_we_never_read_still_parses() {
        // GitHub 会往 release 里加字段。多出来的那些不该让读者的检查失败。
        let body = r#"{"tag_name":"v9.9.9","body":"b","assets":[],"draft":false,
            "prerelease":false,"author":{"login":"someone"},"upload_url":"https://x"}"#;
        let parsed = parse_release(body.as_bytes()).unwrap();
        assert_eq!(parsed.version, parse_version("9.9.9").unwrap());
        assert_eq!(parsed.notes, "b");
        assert!(parsed.assets.is_empty());
    }

    #[test]
    fn the_request_carries_the_api_media_type_and_points_at_the_public_repo() {
        let mock = Mock::ok(&release("v0.1.1", "x", &[]));
        let _ = check(&mock, "0.1.1");
        let req = mock.last();
        assert_eq!(req.url, LATEST_API);
        assert!(
            req.url
                .starts_with("https://api.github.com/repos/panzhifu/folio/releases"),
            "问的必须是这个公开仓库的 release；URL 里残留任何别的仓库名（例如已删掉的 \
             folio-releases）都会让读者那边永远「取不到」"
        );
        // 这一条比 URL 那条要紧：客户端里的密钥人人能提出来，带上任何凭据就等于把发布权限
        // 发给所有读者。WebDAV 那条链确实会写 Authorization（`net/remote/dav.rs`），所以
        // 「这一条链不该有」不是显然的事，得钉住。
        assert!(
            !req.headers
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case("authorization")),
            "检查更新必须匿名可读，不许带任何凭据"
        );
        assert!(
            req.headers
                .iter()
                .any(|(key, value)| key == "Accept" && value.contains("github+json")),
            "没带 Accept 时 GitHub 给的形状与文档那份不是一回事"
        );
    }

    #[test]
    fn asking_for_one_tag_hits_that_tag_and_refuses_a_path_like_input() {
        let mock = Mock::ok(&release("v0.1.1", "这一版改了什么", &[]));
        let release = release_by_tag(&mock, "v0.1.1").unwrap();
        assert_eq!(release.notes, "这一版改了什么");
        assert_eq!(mock.last().url, format!("{RELEASE_BY_TAG_API}v0.1.1"));

        // tag 进的是 URL 的**路径段**。它来自编译期那串版本道理应不是输入，但拼进路径的
        // 东西一旦带上 `..` 或 `?`，症状是打到别的端点上去 —— 在这儿拒掉比事后难查便宜。
        for bad in ["", "../latest", "v0.1.1?per_page=1", "v0.1.1/x", "v0.1.1#f"] {
            assert!(
                matches!(
                    release_by_tag(&Mock::ok("{}"), bad),
                    Err(Error::Malformed(_))
                ),
                "{bad:?} 不该被拼进 URL"
            );
        }
    }

    #[test]
    fn asset_selection_is_by_exact_suffix_and_never_settles_for_anything() {
        let parsed = parse_release(
            release(
                "v0.2.0",
                "x",
                &[
                    ("folio-0.2.0-linux-x86_64.deb", 1),
                    ("folio-0.2.0-macos-arm64.zip", 2),
                    ("sha256sums.txt", 3),
                ],
            )
            .as_bytes(),
        )
        .unwrap();
        if cfg!(target_os = "linux") {
            // 这一端要的是 tar.xz，而它**不在**上面那批里 → None，
            // 而不是「给一个看起来像安装包的」。
            assert!(asset_for_platform(&parsed).is_none());
        }
        if cfg!(target_os = "macos") {
            assert_eq!(
                asset_for_platform(&parsed).map(|asset| asset.name.as_str()),
                Some("folio-0.2.0-macos-arm64.zip")
            );
        }
        // 后缀必须成对命中：`.deb` 不是 `.tar.xz`，哪怕它更像安装包。
        assert!(asset_named(&parsed, "folio-0.2.0-linux-x86_64.deb").is_some());
        assert!(asset_named(&parsed, "folio-0.2.0-win32.exe").is_none());
    }

    #[test]
    fn a_digest_only_counts_when_it_is_actually_a_sha256() {
        let base = Asset {
            name: "a".into(),
            url: "u".into(),
            size: 1,
            digest: None,
        };
        let short = Asset {
            digest: Some("sha256:00ff11".into()),
            ..base.clone()
        };
        assert_eq!(short.sha256_hex(), None, "长度不对的摘要不算有");
        let other_algorithm = Asset {
            digest: Some(format!("sha512:{}", "0f".repeat(64))),
            ..base.clone()
        };
        assert_eq!(
            other_algorithm.sha256_hex(),
            None,
            "换了算法时宁可报「没摘要」，也不要拿 128 位去比 64 位"
        );
        let good = Asset {
            digest: Some(format!("sha256:{}", "0f".repeat(32))),
            ..base.clone()
        };
        assert_eq!(good.sha256_hex(), Some(&*"0f".repeat(32)));
        let bare = Asset {
            digest: Some("0f".repeat(32)),
            ..base.clone()
        };
        assert_eq!(bare.sha256_hex(), Some(&*"0f".repeat(32)), "没前缀时也认");
        assert_eq!(base.sha256_hex(), None);
    }

    /// 一条只回给定几块的传输：`download` 走的是 `Transport::download` 那条 finish/abort 契约。
    struct Chunked {
        chunks: Vec<Vec<u8>>,
        status: u16,
    }

    impl Transport for Chunked {
        fn request(&self, _req: Request) -> super::super::remote::Result<Response> {
            unreachable!("下载这一路不走 request")
        }

        fn download(
            &self,
            _req: Request,
            sink: &mut dyn Sink,
            cancel: &Cancel,
        ) -> super::super::remote::Result<u64> {
            if self.status != 200 {
                sink.abort();
                return Err(super::super::remote::Error::Unexpected {
                    status: self.status,
                });
            }
            let mut written = 0;
            for chunk in &self.chunks {
                if cancel.triggered() {
                    sink.abort();
                    return Err(super::super::remote::Error::Cancelled);
                }
                sink.write(chunk).map_err(super::super::remote::Error::Io)?;
                written += chunk.len() as u64;
            }
            sink.finish().map_err(super::super::remote::Error::Io)?;
            Ok(written)
        }
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("folio-dl-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_finished_download_lands_under_the_final_name_only() {
        let dir = temp_dir("ok");
        let dest = dir.join("folio.apk");
        let mut seen = Vec::new();
        let transport = Chunked {
            chunks: vec![vec![1u8; 4], vec![2u8; 4]],
            status: 200,
        };
        download_to_file(
            &transport,
            "https://x/folio.apk",
            &dest,
            &Cancel::new(),
            |done| seen.push(done),
        )
        .unwrap();
        assert!(dest.is_file(), "下完该在最终名上");
        assert!(!dir.join("folio.apk.part").exists(), "临时名该消失");
        assert_eq!(seen, vec![4, 8], "进度按块累计，不是最后一次性给");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_cancelled_or_failed_download_leaves_no_half_file() {
        // 这条是那一整段存在的理由：半截包留在最终名上，下一次「校验通过」校验的就是垃圾。
        let dir = temp_dir("cancel");
        let dest = dir.join("folio.apk");
        let cancel = Cancel::new();
        cancel.trigger();
        let transport = Chunked {
            chunks: vec![vec![9u8; 4]],
            status: 200,
        };
        assert!(
            download_to_file(&transport, "https://x/folio.apk", &dest, &cancel, |_| {}).is_err()
        );
        assert!(!dest.exists());
        assert!(
            !dir.join("folio.apk.part").exists(),
            "取消之后 .part 也要一起清掉"
        );

        let transport = Chunked {
            chunks: vec![vec![9u8; 4]],
            status: 500,
        };
        assert!(
            download_to_file(
                &transport,
                "https://x/folio.apk",
                &dest,
                &Cancel::new(),
                |_| {}
            )
            .is_err()
        );
        assert!(!dest.exists() && !dir.join("folio.apk.part").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn starting_a_download_clears_the_previous_apk_but_nothing_else() {
        // 那一目录里只有我们自己下的东西才该被动；读者的东西一个字节都不碰。
        let dir = temp_dir("sweep");
        std::fs::write(dir.join("stale.apk"), b"old").unwrap();
        std::fs::write(dir.join("keep.txt"), b"mine").unwrap();
        let transport = Chunked {
            chunks: vec![b"new".to_vec()],
            status: 200,
        };
        download_to_file(
            &transport,
            "https://x/f.apk",
            &dir.join("f.apk"),
            &Cancel::new(),
            |_| {},
        )
        .unwrap();
        assert!(!dir.join("stale.apk").exists(), "上一轮的残留该清掉");
        assert!(
            dir.join("keep.txt").is_file(),
            "不是 .apk/.part 的东西不许碰"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_checksum_listing_tolerates_stray_lines_but_not_a_missing_entry() {
        assert!(parse_checksums("").is_empty(), "空文件不该编出条目来");
        let text = format!(
            "# generated\n\n{}  folio-0.1.1-arm64-v8a.apk\n{} *folio-0.1.1-x.bin\nnot-a-line\n",
            "ab".repeat(32),
            "cd".repeat(32),
        );
        let sums = parse_checksums(&text);
        assert_eq!(
            sums.get("folio-0.1.1-arm64-v8a.apk").map(String::as_str),
            Some(&*"ab".repeat(32))
        );
        assert!(
            sums.contains_key("folio-0.1.1-x.bin"),
            "二进制模式那个 `*` 要能剥掉"
        );
        assert_eq!(sums.len(), 2, "认不出的行跳过，但别多出一条");
        assert!(!sums.contains_key("not-a-line"));
    }

    #[test]
    fn a_file_verifies_only_against_its_real_digest() {
        let dir = std::env::temp_dir().join(format!("folio-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("payload.bin");
        std::fs::write(&file, b"folio").unwrap();
        // `printf folio | sha256sum` 量出来的，不是抄的。
        let want = "4631f244ad4495271986f2ad47fdb560fb8d94727e723e4939fe4e2448bbb156";
        assert_eq!(sha256_of_file(&file).unwrap(), want);
        assert!(matches_sha256(&file, Some(want)), "真摘要必须验过");
        assert!(
            !matches_sha256(&file, Some(&"0".repeat(64))),
            "假摘要必须验不过"
        );
        assert!(
            !matches_sha256(&file, None),
            "没有可比的东西就是没验过 —— 不许当成通过"
        );
        assert!(
            !matches_sha256(&dir.join("nope.bin"), Some(want)),
            "文件读不到时报「校验不过」而不是 panic"
        );
        assert!(
            matches_sha256(&file, Some(&want.to_ascii_uppercase())),
            "GitHub 那侧不保证小写，而我们算出来的一定是小写"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
