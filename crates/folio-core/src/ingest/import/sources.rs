//! 七个来源共用的那半边：把候选的字节**送到**暂存区。
//!
//! 这里没有哈希、没有格式判定、没有数据库（§二 不变量三）——只有「拷贝 / 下载 / 起临时名 /
//! 洗名」这四件对所有来源完全一样的事。契约（[`Source`]）在上一级，批次引擎在
//! [`pipeline`](super::pipeline)。

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use crate::infra::error::Error;
use crate::infra::error::IoResultExt as _;
use crate::infra::error::Result;
use crate::infra::paths;
use crate::infra::task::StageCtx;
use crate::net::remote::Method;
use crate::net::remote::Request;
use crate::net::remote::Sink;
use crate::net::remote::Transport;

use super::Candidate;
use super::ImportKind;
use super::Origin;
use super::Source;

/// 本地来源共用的 `stage`：把候选那份文件**分块拷进暂存区**，交回那份 `.part`。
///
/// 自己写循环而不用 `fs::copy`，为的是字节级读数（`ctx.bytes`）：一本几十 MB 的书拷多久
/// 界面要报得出来，而 `fs::copy` 中间什么都看不见。**循环里不看取消旗** ——
/// `plans/基础设施.md` §1.2 第 5 条要求检查点落在单元之间而不是函数调用中间：一次拷贝
/// 是一个单元，半途停下来的结果是「谁也没认领、但确实写了一半」的那个文件。
pub(crate) fn stage_local(
    candidate: &Candidate,
    staging_dir: &Path,
    ctx: &StageCtx,
) -> Result<PathBuf> {
    let Origin::Local(source_path) = &candidate.origin else {
        return Err(Error::Io {
            path: PathBuf::from(&candidate.name),
            source: "本地 stage 不该接到远端候选".to_string().into(),
        });
    };
    std::fs::create_dir_all(staging_dir).io_path(staging_dir)?;
    let dest = staging_dir.join(staged_name(&candidate.name));
    let total = candidate.size.unwrap_or(0);
    copy_into(source_path, &dest, &candidate.name, total, ctx)
}

/// 分块拷贝并报字节数。交回的是暂存区里那份 `.part` 的路径；改成哈希名是管线第 5 步的事。
fn copy_into(from: &Path, to: &Path, display: &str, total: u64, ctx: &StageCtx) -> Result<PathBuf> {
    use std::io::{Read as _, Write as _};
    let mut reader = std::fs::File::open(from).io_path(from)?;
    let mut file = std::fs::File::create(to).io_path(to)?;
    const CHUNK: usize = 64 * 1024;
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    loop {
        let read = reader.read(&mut buf).io_path(from)?;
        if read == 0 {
            break;
        }
        file.write_all(&buf[..read]).io_path(to)?;
        done += read as u64;
        // 总数拿不到就报 `None`：界面画不定进度，而不是编一个会后退的数。
        ctx.bytes(done, (total > 0).then_some(total), display);
    }
    Ok(to.to_path_buf())
}

/// 暂存区里的临时名：`.<建议名前 40 字>.<序号>.part`。
///
/// 带序号是因为同一份文件**可以从同一个来源同时来两次**（选择器里连点、监听攒批里同一
/// 路径出现两轮）。两个候选若写到同一个名字，后写的那一份会盖掉前一份的半成品，之后
/// 算出的哈希属于一份「混合过的」文件 —— 那是最坏的一种错，因为它看起来是成功的。
/// 建议名留在前面只为人看日志时认得出是哪一本；它对最终文件名没有任何影响。
pub(crate) fn staged_name(display_name: &str) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let cleaned: String = display_name
        .chars()
        .filter(|c| !paths::is_unsafe_name_char(*c))
        .take(40)
        .collect();
    let cleaned = cleaned.trim();
    let stem = if cleaned.is_empty() { "book" } else { cleaned };
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(".{stem}.{n}.part")
}

/// 一批**现成的本地路径** —— 七种来源里六种最终都落成它：选择器交回的算
/// [`ImportKind::Picked`]，其余（清单勾选、传书、监听）由调用方带着自己的 [`ImportKind`] 进来。
///
/// 旧形状里那份 `Policy`（立场）现在整条不需要了：副本叫什么由内容哈希定，所以没有
/// 「重名怎么办」这一问；在不在库里由哈希判重定，所以没有「要不要跳过」这一问。
/// 来源之间剩下的唯一差别只有「日志里这是哪一类」。
pub struct PathsSource {
    paths: Vec<PathBuf>,
    kind: ImportKind,
    /// 这些路径是不是**用完即弃的临时副本**。
    ///
    /// 只有系统选择器会先把选中的文件拷进应用自己的 cache（Android 的
    /// `GpuiPathPicker`：`cache/gpui-import-<uuid>/<原名>`；iOS 的 sandbox copy 同理），
    /// 再把拷贝的路径交出来。导入读完之后这些路径就没人要了，Drop 时连那个只装它
    /// 一个文件的临时目录一起清掉。
    ///
    /// 扫描 / 传书 / 监听交的都是读者自己的文件；桌面选择器给的也是原文件 ——
    /// 那几路绝不能打开这个开关，删了就是数据事故。
    remove_on_drop: bool,
}

impl PathsSource {
    /// 选择器选的文件。路径指向读者自己的原件 —— 导入只拷贝，Drop 时不许删。
    pub fn picked(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            kind: ImportKind::Picked,
            remove_on_drop: false,
        }
    }

    /// 选择器交回的是应用自己先拷好的临时副本：读完即弃，Drop 时把文件和那个
    /// `gpui-import-<uuid>` 空目录一起清掉。
    ///
    /// 判据由平台侧给（app 的 `ui::host::picked_files_are_temporary`），这里只记住结果；
    /// 扫描 / 传书 / 监听那几路仍走 [`Self::picked`] / [`Self::new`]。
    pub fn picked_temporary(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            kind: ImportKind::Picked,
            remove_on_drop: true,
        }
    }

    /// 调用方直接给的一批路径，`kind` 说清这是谁交的（扫描 / 监听 / 传书各报各自的名）。
    pub fn new(paths: Vec<PathBuf>, kind: ImportKind) -> Self {
        Self {
            paths,
            kind,
            remove_on_drop: false,
        }
    }

    pub(crate) fn candidates_from(paths: &[PathBuf]) -> Vec<Candidate> {
        paths
            .iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_string_lossy().into_owned();
                let size = std::fs::metadata(path).ok().map(|m| m.len());
                Some(Candidate {
                    origin: Origin::Local(path.clone()),
                    name,
                    size,
                })
            })
            .collect()
    }
}

impl Source for PathsSource {
    fn kind(&self) -> ImportKind {
        self.kind
    }

    fn candidates(&self, _book_dir: &Path, _ctx: &StageCtx) -> Result<Vec<Candidate>> {
        Ok(Self::candidates_from(&self.paths))
    }

    fn stage(&self, candidate: &Candidate, staging_dir: &Path, ctx: &StageCtx) -> Result<PathBuf> {
        stage_local(candidate, staging_dir, ctx)
    }
}

impl Drop for PathsSource {
    fn drop(&mut self) {
        if !self.remove_on_drop {
            return;
        }
        for path in &self.paths {
            remove_temporary(path);
        }
    }
}

/// 删掉一份选择器临时副本，再尽力删掉它那个只装这一份的空目录。
///
/// 父目录用 `remove_dir` 而不是 `remove_dir_all`：正常形状下它是 `gpui-import-<uuid>`，
/// 删完文件就空了；万一哪天路径形状变了，这也不会递归带走别的文件。删除只记
/// debug：释放缓存是自愈动作，不值得为它刷警告，而 `NotFound` 更是预期内的重复清理。
fn remove_temporary(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            tracing::debug!("import: 临时副本 {} 没清掉：{err}", path.display());
            return;
        }
    }
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::remove_dir(parent)
        && !matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
        )
    {
        tracing::debug!("import: 临时目录 {} 没清掉：{err}", parent.display());
    }
}

/// 下载的落盘策略：先写**临时名**，下完（`finish`）改名成正式副本；任何一半途
/// 而废（`abort`）都把临时文件清掉 —— 临时文件要么变成正式文件，要么消失，
/// 不会留下第三种状态（与 [`Transport::download`] 的契约是同一句话）。
struct FileSink {
    dest: PathBuf,
    partial: PathBuf,
    file: Option<std::fs::File>,
}

impl FileSink {
    /// 在 `dest` 旁边开一个 `.{文件名}.part`。同目录保证 `rename` 是原子的。
    fn start(dest: PathBuf) -> Result<Self> {
        let mut partial = dest.clone();
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        partial.set_file_name(format!(".{name}.part"));
        let file = std::fs::File::create(&partial).io_path(&partial)?;
        Ok(Self {
            dest,
            partial,
            file: Some(file),
        })
    }
}

impl Sink for FileSink {
    fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        use std::io::Write as _;
        self.file
            .as_mut()
            .expect("finish/abort 之后不该再写")
            .write_all(chunk)
    }

    fn finish(&mut self) -> std::io::Result<()> {
        let file = self.file.take();
        if let Some(file) = file {
            file.sync_all()?;
        }
        std::fs::rename(&self.partial, &self.dest)
    }

    fn abort(&mut self) {
        self.file.take(); // 关掉句柄，删除才有意义
        let _ = std::fs::remove_file(&self.partial);
    }
}

/// 从一条 URL 取「这本书该叫什么」：路径末段、百分号解码、换掉文件系统
/// 不认的字符。查询串与锚点不算名字的一部分（签名直链的末段通常还是书名）。
/// 什么都剩不下就给一个带时间戳的占位名 —— 解析失败时错误里至少有它。
pub(crate) fn url_name(url: &str) -> String {
    // 剥掉 scheme:// 与 authority（到第一个 / 为止），末段才是路径里的东西；
    // 查询串与锚点不算名字。剥不出路径（URL 就是个裸域名）→ 占位名。
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let after_authority = after_scheme
        .split_once('/')
        .map(|(_, rest)| rest)
        .unwrap_or("");
    let path = after_authority
        .split(['?', '#'])
        .next()
        .unwrap_or(after_authority);
    let last = path
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or_default();
    let decoded = percent_decode(last);
    let cleaned: String = decoded
        .chars()
        .map(|c| {
            if paths::is_unsafe_name_char(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().to_string();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        format!("download-{}", now_secs())
    } else {
        trimmed
    }
}

/// 最小百分号解码：`%XX` 解回字节，其余原样。解码失败（半个 `%`）原样保留 ——
/// 名字是给人看的，不是给文件系统解析的。
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // 凑不齐 `%` 后面那两个字节就当没有解码这回事（下面那片原样推出去）。
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            let hex = &bytes[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(&String::from_utf8_lossy(hex), 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 一条网络直链：粘贴一个 http(s) 地址，下载、落成副本、照常解析入库。
///
/// `stage` 是流式的：响应体按块交给 [`FileSink`]，取消在块与块之间生效 ——
/// 半截的下载留不下半个文件。名字取自 URL 末段（见 [`url_name`]）；
/// 签名直链（`?token=…`）不在查询串里找名字，所以末段通常还是书名。
pub struct UrlSource {
    url: String,
    transport: Arc<dyn Transport>,
}

impl UrlSource {
    pub fn new(url: String, transport: Arc<dyn Transport>) -> Self {
        Self { url, transport }
    }
}

impl Source for UrlSource {
    fn kind(&self) -> ImportKind {
        ImportKind::Url
    }

    fn candidates(&self, _book_dir: &Path, ctx: &StageCtx) -> Result<Vec<Candidate>> {
        let name = url_name(&self.url);
        tracing::debug!("import: URL 候选 {name} ← {}", url_for_log(&self.url));
        ctx.message(format!("下载《{name}》"));
        Ok(vec![Candidate {
            origin: Origin::Remote {
                url: self.url.clone(),
                path: self.url.clone(),
                size: None,
            },
            name,
            size: None,
        }])
    }

    fn stage(&self, candidate: &Candidate, staging_dir: &Path, ctx: &StageCtx) -> Result<PathBuf> {
        let Origin::Remote { url, .. } = &candidate.origin else {
            return Err(Error::Io {
                path: PathBuf::from(&candidate.name),
                source: "URL 候选不该是本地出处".to_string().into(),
            });
        };
        std::fs::create_dir_all(staging_dir).io_path(staging_dir)?;
        // 下载也只有一个去处：暂存区。`FileSink` 在那里面写 `.{名}.part`、下完改名成
        // 暂存区里的这一份，再由管线第 5 步 rename 进根 —— 全程书库根看不到半成品。
        let dest = staging_dir.join(staged_name(&candidate.name));
        let mut sink = FileSink::start(dest.clone())?;
        let req = Request::new(Method::Get, url.clone());
        let total = self.transport.download(req, &mut sink, ctx.cancel())?;
        ctx.bytes(total, Some(total), &candidate.name);
        Ok(dest)
    }
}

/// 日志里的 URL：查询串里常有凭证（签名直链的 token），打进日志前掐掉。
fn url_for_log(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).to_string()
}
