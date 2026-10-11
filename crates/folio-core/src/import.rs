//! 导入管线：把「一本书的出处」抽象成 [`Source`]，一条管线通吃所有来源。
//!
//! 抽象边界只圈一件事：**候选 → 书库里那份本地副本**。拷进书库、解析、落库
//! （`Store::add_book` + `save_document`）都是管线自己的事，在后台线程上逐本完成 ——
//! [`crate::store::Store`] 现在是 `Send + Sync` 的（连接锁在内部 `Mutex` 上），落库
//! 不必再回界面线程。锁纪律见 `store` 的模块文档：**stage 与 parse 绝不持锁**，
//! 只有 register 那一小段持，后台导一本书时界面最多等几十毫秒。
//!
//! 七种来源的差别全在 `candidates` 与 `stage` 两个方法里，管线只有 [`import_from`]
//! 一写：
//!
//! | 来源 | candidates | stage |
//! | --- | --- | --- |
//! | 选择文件 `Picked` | 选择器交回的路径 | `fs::copy` |
//! | 批量路径 `Paths` | 调用方给的一批（清单勾选 / 传书落盘） | `fs::copy` |
//! | 整机扫描 `Scan` | 走目录只收认识的扩展名 | `fs::copy` |
//! | 文件夹监听 `Watch` | inotify 事件攒出的新文件 | `fs::copy` |
//! | Wi-Fi 传书 `Transfer` | 传书服务器收到的每份上传 | `fs::copy` |
//! | 链接下载 `Url` | 一条 URL | 流式 GET 落成副本 |
//! | WebDAV `WebDav` | PROPFIND ∩ 需要下载的条目 | DAV 下载落成副本 |
//!
//! 监听、传书、WebDAV 同步自己都不是管线的一部分 —— 它们是往队列塞活的**驱动者**
//! （见 `crate::watch`），把 [`Source`] 交给 [`submit_import`] 就行。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::document;
use crate::error::{Error, IoResultExt as _, Result};
use crate::remote::{Method, Request, Sink, Transport};
use crate::store::Store;
use crate::task::{StageCtx, TaskHandle, TaskManager};

/// 这次导入属于哪一类来源。日志与「这批是谁交的」用，不参与任何分支逻辑。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    /// 读者在系统选择器里亲手挑的。
    Picked,
    /// 调用方直接给的一批本地路径（扫描清单勾选、传书落盘、监听攒批）。
    Paths,
    /// 整机扫描找出来的。
    Scan,
    /// 监听文件夹时事件攒出来的。
    Watch,
    /// 传书服务器收到的。
    Transfer,
    /// 一条网络直链。
    Url,
    /// WebDAV 同步的下载半边。
    WebDav,
}

/// 一个候选的出处。本地是一份路径；远端是「能下载到它的 URL」加上
/// 「它在自己那一层里的路径」（同步状态表按后者记账）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Local(PathBuf),
    Remote {
        url: String,
        path: String,
        size: Option<u64>,
    },
}

/// 一个候选 = 一本书的出处。`name` 就是落库副本的文件名 —— `books.path`
/// 是书的身份，所以名字在进管线之前就得定下来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub origin: Origin,
    pub name: String,
    pub size: Option<u64>,
}

/// 目标位置已有同名文件时的处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameClash {
    /// 覆盖。读者亲手挑的（选择器 / 传书 / 清单勾选）重名算同一本 —— 重复导入
    /// 同一路径是「更新」，不是第二本书。
    Overwrite,
    /// 落盘加后缀（`书名-2.epub`）。扫描与监听宁可多一格，也不该把库里那本
    /// 指向一份新的文件。同名**同**字节数视为同一份内容，照常覆盖 —— 判定用
    /// 文件大小而不是逐字节比对，一次扫描要过几千个文件，为一种罕见情形把每次
    /// 导入都多读两遍不值。
    Suffix,
}

/// 一次导入共用的处置策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// 目标副本路径已经在库里就整本跳过。扫描 / 监听开着它：重扫一遍不该把
    /// 同样的书再拷一遍、再落一遍库。
    pub skip_existing: bool,
    /// 副本落进书库目录。`false` = 就地登记那一本（书已经在设备上，不拷）——
    /// 现在没有来源用它，留给「把躺在别处的书登记进库好让进度有 id」那类入口。
    pub copy_in: bool,
    /// 同名文件已经在目标位置时的处置。
    pub on_name_clash: NameClash,
}

/// 一个来源。一个实现管一种出处；实现整个住在后台作业里，所以必须 `Send + Sync`。
pub trait Source: Send + Sync + 'static {
    fn kind(&self) -> ImportKind;
    fn policy(&self) -> Policy;
    /// 交出这一批候选。列目录（扫描）、PROPFIND（远端）都发生在这一步，
    /// 所以它拿得到两级读数与取消 —— 这一慢是**整批的**慢。
    fn candidates(&self, dest_dir: &Path, ctx: &StageCtx) -> Result<Vec<Candidate>>;
    /// 把一个候选变成 `dest_dir` 里（`copy_in = false` 时原地）那份本地副本，
    /// 返回副本路径。下载与拷贝的字节级读数从这里报。
    fn stage(&self, candidate: &Candidate, dest_dir: &Path, ctx: &StageCtx) -> Result<PathBuf>;
}

/// 落库成功的一条结果。界面拿 `book_id` 清封面缓存、重查架面；
/// `dest` 与 `title` 只为日志与测试 —— 章节表已经落库，正文按位置回容器取
/// （见 `plans/text-out-of-db.md`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    pub book_id: i64,
    pub dest: PathBuf,
    pub title: String,
}

/// 一个候选的结果。`Ok(None)` = 按策略跳过（库里已有）；`Err` = 这一本坏了，
/// 不拖累整批。
#[derive(Debug)]
pub struct ImportOutcome {
    pub candidate: Candidate,
    pub result: Result<Option<Registered>>,
}

/// 管线本体：候选 → 跳过判定 → 副本 → 解析 → 落库，**逐本进行，逐本报数**。
///
/// - **顺序是硬的**：`stage → parse → register`，中间任何一本坏了只让它自己那条
///   `Err`，其余的照走。
/// - **取消**在书与书之间检查；置位就停下，**已经落库的那些是真的** —— 界面收到
///   `Cancelled` 重查一次架面就能看见它们。
/// - `candidates` 交不出（整个来源坏了，比如根目录读不了）返回空批 —— 逐本各自
///   成败的立场在这里够不着，一条 `Err` 没有候选可挂，只能进日志。
pub fn import_from(
    source: &dyn Source,
    dest_dir: &Path,
    store: &Store,
    ctx: &StageCtx,
) -> Vec<ImportOutcome> {
    let policy = source.policy();
    let kind = source.kind();
    let candidates = match source.candidates(dest_dir, ctx) {
        Ok(candidates) => candidates,
        Err(err) => {
            tracing::error!("import: {kind:?} 交不出候选：{err:#}");
            return Vec::new();
        }
    };
    let total = candidates.len() as u64;
    tracing::info!("import: {kind:?} 一批 {total} 本 → {dest_dir:?}");
    let mut outcomes = Vec::with_capacity(candidates.len());
    for (done, candidate) in candidates.iter().enumerate() {
        if ctx.cancel().triggered() {
            break;
        }
        ctx.item(done as u64, total);
        let result = import_candidate(source, candidate, dest_dir, store, policy, ctx);
        outcomes.push(ImportOutcome {
            candidate: candidate.clone(),
            result,
        });
    }
    ctx.item(total, total);
    outcomes
}

/// 一本书的完整旅程：跳过判定 → 副本 → 解析 → 登记书目 → 落章节。
fn import_candidate(
    source: &dyn Source,
    candidate: &Candidate,
    dest_dir: &Path,
    store: &Store,
    policy: Policy,
    ctx: &StageCtx,
) -> Result<Option<Registered>> {
    let name = candidate.name.as_str();
    // 跳过判定按**副本路径**：`books.path` 是书的身份，路径命中就是同一本。
    // 名字对得上但字节数不同的那种不算已有 —— 宁可多一格，不要少一格。
    let planned = if policy.copy_in {
        dest_dir.join(name)
    } else {
        match &candidate.origin {
            Origin::Local(path) => path.clone(),
            Origin::Remote { .. } => {
                return Err(Error::Io {
                    path: PathBuf::from(name),
                    source: "远端候选不支持就地登记（copy_in = false）"
                        .to_string()
                        .into(),
                });
            }
        }
    };
    if policy.skip_existing && store.book_by_path(&planned)?.is_some() {
        tracing::debug!("import: 《{name}》库里已有，跳过");
        return Ok(None);
    }

    // 副本与解析都不持锁；`store` 只在下面 register 那两行里被碰到。
    let dest = source.stage(candidate, dest_dir, ctx)?;
    let document = document::open(&dest)?;
    let title = document.meta().title.clone();
    let book_id = store.add_book(&dest, document.meta())?;
    store.save_document(book_id, document.as_ref())?;
    tracing::info!("import: 《{title}》→ 第 {book_id} 本（{dest:?}）");
    Ok(Some(Registered {
        book_id,
        dest,
        title,
    }))
}

/// 重扫一本**已经入库**的书：文件换了，或者索引是旧算法算的。
///
/// 与导入共用「解析 → 落库」那一段，唯一不同的是**不改 `book_id`** —— 进度、批注、
/// 阅读统计都挂在它上面。章序按同一套规则重算，所以多数情况下位置还对得上；
/// 对不上的由 `Locator::clamp_to_chapter` 夹回本章。
pub fn reindex(store: &Store, book_id: i64) -> Result<()> {
    let row = store.book(book_id)?.ok_or(Error::NoSuchBook(book_id))?;
    let path = PathBuf::from(&row.path);
    let document = document::open(&path)?;
    // 顺手刷新 `size` / `mtime`：判定「文件换没换」靠的就是这两个数。
    store.add_book(&path, document.meta())?;
    store.save_document(book_id, document.as_ref())?;
    tracing::info!("import: 重扫第 {book_id} 本《{}》", row.title);
    Ok(())
}

/// 把一个来源包成一个后台作业交给 `manager`。返回的 [`TaskHandle`] 就是这批导入的
/// 全部接口：两级进度、取消、等完成。
pub fn submit_import(
    manager: &TaskManager<Vec<ImportOutcome>>,
    source: Box<dyn Source>,
    dest_dir: PathBuf,
    store: Arc<Store>,
) -> TaskHandle<Vec<ImportOutcome>> {
    manager.submit(move |ctx| Ok(import_from(source.as_ref(), &dest_dir, &store, ctx)))
}

/// 本地来源共用的 `stage`：把候选那份文件拷进 `dest_dir`，返回副本路径。
///
/// [`NameClash::Suffix`] 时，目标已有同名文件**且字节数不同**就另起 `名字-2.扩展名`
/// （被占就继续往后找）；字节数相同视为同一份内容，覆盖无害。
pub(crate) fn stage_local(
    candidate: &Candidate,
    dest_dir: &Path,
    clash: NameClash,
) -> Result<PathBuf> {
    let Origin::Local(source_path) = &candidate.origin else {
        return Err(Error::Io {
            path: PathBuf::from(&candidate.name),
            source: "本地 stage 不该接到远端候选".to_string().into(),
        });
    };
    std::fs::create_dir_all(dest_dir).io_path(dest_dir)?;
    let mut dest = dest_dir.join(&candidate.name);
    if clash == NameClash::Suffix && dest.exists() && differs(source_path, &dest) {
        dest = suffixed_dest(dest_dir, &candidate.name);
    }
    std::fs::copy(source_path, &dest).io_path(&dest)?;
    Ok(dest)
}

/// 两份文件的字节数是否不同。读不到大小（权限、竞态）按「不同」处理：
/// 落一个后缀副本比悄悄覆盖一份不知情的文件安全。
fn differs(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.len() != b.len(),
        _ => true,
    }
}

/// `名字.epub` 被占时的去处：`名字-2.epub`、`名字-3.epub`……找到第一个空位。
fn suffixed_dest(dest_dir: &Path, name: &str) -> PathBuf {
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    let ext = Path::new(name)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 2.. {
        let candidate = dest_dir.join(format!("{stem}-{n}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("u64 级的后缀循环不可能耗尽")
}

/// 一批**现成的本地路径** —— 七种来源里六种最终都落成它：选择器交回的算
/// [`ImportKind::Picked`]，其余（清单勾选、传书、监听）由调用方带着自己的
/// [`ImportKind`] 与 [`Policy`] 进来。
pub struct PathsSource {
    paths: Vec<PathBuf>,
    kind: ImportKind,
    policy: Policy,
}

impl PathsSource {
    /// 选择器选的文件：读者亲手挑的，重名算更新。
    pub fn picked(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            kind: ImportKind::Picked,
            policy: Policy {
                skip_existing: false,
                copy_in: true,
                on_name_clash: NameClash::Overwrite,
            },
        }
    }

    /// 调用方直接给的一批路径。`kind` 与 `policy` 由调用方定 —— 扫描、监听、
    /// 传书各自的立场（见 [`Policy`] 与 [`NameClash`] 的文档）。
    pub fn with_policy(paths: Vec<PathBuf>, kind: ImportKind, policy: Policy) -> Self {
        Self {
            paths,
            kind,
            policy,
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

    fn policy(&self) -> Policy {
        self.policy
    }

    fn candidates(&self, _dest_dir: &Path, _ctx: &StageCtx) -> Result<Vec<Candidate>> {
        Ok(Self::candidates_from(&self.paths))
    }

    fn stage(&self, candidate: &Candidate, dest_dir: &Path, _ctx: &StageCtx) -> Result<PathBuf> {
        stage_local(candidate, dest_dir, self.policy.on_name_clash)
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
fn url_name(url: &str) -> String {
    // 剥掉 scheme:// 与 authority（到第一个 / 为止），末段才是路径里的东西；
    // 查询串与锚点不算名字。剥不出路径（URL 就是个裸域名）→ 占位名。
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let after_authority = after_scheme
        .split_once('/')
        .map(|(_, rest)| rest)
        .unwrap_or("");
    let path = after_authority.split(['?', '#']).next().unwrap_or(after_authority);
    let last = path
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or_default();
    let decoded = percent_decode(last);
    let cleaned: String = decoded
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
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
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 2 <= bytes.len() - 1 + 1 {
            let hex = bytes.get(i + 1..i + 3);
            if let Some(hex) = hex
                && let Ok(byte) = u8::from_str_radix(&String::from_utf8_lossy(hex), 16)
            {
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

    fn policy(&self) -> Policy {
        // 读者亲手贴的链接：重名算更新；同一链接再导一次应当刷新那本书。
        Policy {
            skip_existing: false,
            copy_in: true,
            on_name_clash: NameClash::Overwrite,
        }
    }

    fn candidates(&self, _dest_dir: &Path, ctx: &StageCtx) -> Result<Vec<Candidate>> {
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

    fn stage(&self, candidate: &Candidate, dest_dir: &Path, ctx: &StageCtx) -> Result<PathBuf> {
        let Origin::Remote { url, .. } = &candidate.origin else {
            return Err(Error::Io {
                path: PathBuf::from(&candidate.name),
                source: "URL 候选不该是本地出处".to_string().into(),
            });
        };
        std::fs::create_dir_all(dest_dir).io_path(dest_dir)?;
        let dest = dest_dir.join(&candidate.name);
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        Candidate, ImportKind, ImportOutcome, NameClash, Origin, PathsSource, Policy, Source,
        import_from, stage_local, submit_import,
    };
    use crate::store::Store;
    use crate::task::{StageCtx, TaskEvent, TaskManager};

    /// 从一批事件的 `Finished` 里捞出结果。
    fn outcomes_of(events: Vec<TaskEvent<Vec<ImportOutcome>>>) -> Vec<ImportOutcome> {
        events
            .into_iter()
            .find_map(|e| match e {
                TaskEvent::Finished(Ok(outcomes)) => Some(outcomes),
                _ => None,
            })
            .expect("导入作业应以 Finished(Ok(..)) 收尾")
    }

    fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    /// URL 候选的名字：路径末段、百分号解码、查询串与锚点不算、非法字符换下划线。
    #[test]
    fn url_names_come_from_the_last_path_segment() {
        use super::url_name;
        assert_eq!(url_name("https://example.com/books/三体.epub"), "三体.epub");
        assert_eq!(
            url_name("https://example.com/get?token=abc&file=%E4%B9%A6.epub"),
            "get",
            "查询串不算名字"
        );
        assert_eq!(url_name("https://example.com/a%20b.epub"), "a b.epub");
        assert!(
            starts_with_download(&url_name("https://example.com/")),
            "只剩根路径就给占位名"
        );
        assert_eq!(url_name("https://example.com/a:b.epub"), "a_b.epub");
    }

    fn starts_with_download(name: &str) -> bool {
        name.starts_with("download-")
    }

    /// URL 导入端到端（假传输）：下载 → 落成副本 → 解析入库；取消则半个文件都不留。
    #[test]
    fn url_import_streams_into_a_copy_and_registers() {
        use crate::remote::{Error as RemoteError, Response, Sink, Transport};

        /// 假传输：正文按 3 字节一块发；`cancel_after` 块之后置位取消旗标，
        /// 模拟读者在下载中途按下取消。
        struct Fixed {
            data: Vec<u8>,
            cancel_after: Option<usize>,
        }
        impl Transport for Fixed {
            fn request(&self, _req: crate::remote::Request) -> crate::remote::Result<Response> {
                Ok(Response::new(200, self.data.clone()))
            }
            fn download(
                &self,
                _req: crate::remote::Request,
                sink: &mut dyn Sink,
                cancel: &crate::remote::Cancel,
            ) -> crate::remote::Result<u64> {
                for (sent, chunk) in self.data.chunks(3).enumerate() {
                    if Some(sent) == self.cancel_after {
                        cancel.trigger();
                    }
                    if cancel.triggered() {
                        sink.abort();
                        return Err(RemoteError::Cancelled);
                    }
                    sink.write(chunk).map_err(RemoteError::Io)?;
                }
                sink.finish().map_err(RemoteError::Io)?;
                Ok(self.data.len() as u64)
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("books");
        let store = Store::in_memory().unwrap();

        // 正常下载：名字取自 URL 末段，副本与正文都在。
        let source = super::UrlSource::new(
            "https://example.com/%E4%B9%A6.txt".into(),
            Arc::new(Fixed {
                data: "网络来的正文".as_bytes().to_vec(),
                cancel_after: None,
            }),
        );
        let outcomes = import_from(&source, &lib, &store, &StageCtx::detached());
        assert_eq!(outcomes.len(), 1);
        let registered = outcomes[0].result.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(registered.dest, lib.join("书.txt"));
        assert_eq!(store.shelf("").unwrap().len(), 1);
        assert!(
            !lib.join(".书.txt.part").exists(),
            "finish 之后临时文件该改成正式名"
        );

        // 中途取消：这一本落一条 Err，副本与临时残留都不该有，库里也没有那行。
        let store2 = Store::in_memory().unwrap();
        let source = super::UrlSource::new(
            "https://example.com/取消.epub".into(),
            Arc::new(Fixed {
                data: "永远不会写完".as_bytes().to_vec(),
                cancel_after: Some(1),
            }),
        );
        let outcomes = import_from(&source, &lib, &store2, &StageCtx::detached());
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0].result, Err(_)), "取消落一条 Err");
        assert!(store2.shelf("").unwrap().is_empty());
        assert!(!lib.join("取消.epub").exists());
        assert!(!lib.join(".取消.epub.part").exists(), "abort 要清掉临时文件");
    }

    /// picked 语义的端到端：好的登记上架（书目 + 章节都落库），坏的那本只留一条
    /// `Err`；副本落在书库目录里、原件原封不动。
    ///
    /// 这条门从前住在界面侧（`ui/shared/library.rs`），落库搬进管线后跟着搬过来 ——
    /// 它钉的是「`books.path` 记的是书库里的副本」「章节要跟着落库」两条契约，
    /// 与是谁把文件交进来的无关。
    #[test]
    fn picked_files_are_registered_into_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let picked = tmp.path().join("原处");
        let a = write(&picked, "a.txt", "第一本\n");
        let b = write(&picked, "b.mobi", "后端还不认这个扩展名\n");
        let lib = tmp.path().join("books");

        let manager = TaskManager::new(1);
        let store = Arc::new(Store::in_memory().unwrap());
        let handle = submit_import(
            &manager,
            Box::new(PathsSource::picked(vec![a.clone(), b.clone()])),
            lib.clone(),
            store.clone(),
        );
        let outcomes = outcomes_of(handle.events.into_iter().collect());

        assert_eq!(outcomes.len(), 2, "两个源都要有结果");
        let registered: Vec<_> = outcomes
            .iter()
            .filter_map(|o| o.result.as_ref().ok().and_then(|r| r.as_ref()))
            .collect();
        assert_eq!(registered.len(), 1, "好的那本要登记上，坏的不拖垮整批");
        assert!(
            registered[0].dest.starts_with(&lib),
            "入库的副本要落在书库目录里，实际 {:?}",
            registered[0].dest
        );
        assert_eq!(
            std::fs::read(&registered[0].dest).unwrap(),
            std::fs::read(&a).unwrap(),
            "拷过去的内容要和原件一模一样"
        );
        assert!(a.exists(), "导入只拷一份，原件不该被动");

        let shelf = store.shelf("").unwrap();
        assert_eq!(shelf.len(), 1, "架上要看得见刚导进来的那一本");
        let row = store.book(shelf[0].id).unwrap().unwrap();
        assert!(
            Path::new(&row.path).starts_with(&lib),
            "入库的副本要落在确认过的书库目录里，实际记的是 {}",
            row.path
        );
        assert!(
            store.load_flow(shelf[0].id).unwrap().is_some(),
            "章节要跟着落库，不然点进去是一本空书"
        );
    }

    /// 同一份文件导两次：副本还是同一个文件，库里还是同一行 —— 重复导入同一路径
    /// 是更新，不是第二本书。
    #[test]
    fn reimporting_the_same_file_updates_the_same_row() {
        let tmp = tempfile::tempdir().unwrap();
        let picked = tmp.path().join("原处");
        let source = write(&picked, "a.txt", "第一版\n");
        let lib = tmp.path().join("books");
        let store = Arc::new(Store::in_memory().unwrap());
        let manager = TaskManager::new(1);
        let submit = |manager: &TaskManager<Vec<ImportOutcome>>| {
            submit_import(
                manager,
                Box::new(PathsSource::picked(vec![source.clone()])),
                lib.clone(),
                store.clone(),
            )
        };

        let first = outcomes_of(submit(&manager).events.into_iter().collect());
        let second = outcomes_of(submit(&manager).events.into_iter().collect());
        let first = first.into_iter().next().unwrap().result.unwrap().unwrap();
        let second = second.into_iter().next().unwrap().result.unwrap().unwrap();

        assert_eq!(first.dest, second.dest, "两次导入要落在同一个副本上");
        assert_eq!(first.book_id, second.book_id, "库里还是同一行");
        assert_eq!(store.shelf("").unwrap().len(), 1, "不出现第二格");
    }

    /// skip_existing：目标路径已在库里就整本跳过（`Ok(None)`），文件不再拷、
    /// 行不再写 —— 扫描与监听的「重跑一遍无事发生」全靠它。
    #[test]
    fn skip_existing_leaves_registered_books_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let picked = tmp.path().join("原处");
        let a = write(&picked, "a.txt", "第一本\n");
        let lib = tmp.path().join("books");
        let store = Arc::new(Store::in_memory().unwrap());
        let manager = TaskManager::new(1);
        let policy = Policy {
            skip_existing: true,
            copy_in: true,
            on_name_clash: NameClash::Suffix,
        };

        // 第一次走 picked 登记进库；第二次同一份文件、skip_existing 开着。
        let first = submit_import(
            &manager,
            Box::new(PathsSource::picked(vec![a.clone()])),
            lib.clone(),
            store.clone(),
        );
        assert!(outcomes_of(first.events.into_iter().collect())[0]
            .result
            .as_ref()
            .unwrap()
            .is_some());
        let second = import_from(
            &PathsSource::with_policy(vec![a.clone()], ImportKind::Watch, policy),
            &lib,
            &store,
            &StageCtx::detached(),
        );
        assert_eq!(second.len(), 1);
        assert_eq!(
            second[0].result.as_ref().unwrap(),
            &None,
            "库里已有的要跳过，而不是再来一遍"
        );
    }

    /// Suffix：同名**不同**字节落 `-2` 副本，不覆盖；同名同字节还是那一份。
    #[test]
    fn a_name_clash_with_different_bytes_lands_a_suffixed_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("books");
        std::fs::create_dir_all(&lib).unwrap();
        let existing = lib.join("a.txt");
        std::fs::write(&existing, "已在库里的一份\n").unwrap();

        let candidate = |body: &str| {
            let src = write(&tmp.path().join("原处"), "a.txt", body);
            Candidate {
                origin: Origin::Local(src),
                name: "a.txt".into(),
                size: None,
            }
        };
        // 字节数不同 → -2。
        let suffixed = stage_local(&candidate("另一本不一样长的书\n"), &lib, NameClash::Suffix).unwrap();
        assert_eq!(suffixed, lib.join("a-2.txt"), "不同字节的同名文件要另起一份");
        assert!(std::fs::read_to_string(&existing).unwrap() == "已在库里的一份\n", "原来的那份不许被动");
        // 字节数相同 → 原位覆盖。
        let same = stage_local(&candidate("已在库里的一份\n"), &lib, NameClash::Suffix).unwrap();
        assert_eq!(same, existing, "同字节的视为同一份，落回原位");
    }

    /// 一上来就取消：一个候选都不碰。
    #[test]
    fn cancelling_before_the_first_book_touches_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let picked = tmp.path().join("原处");
        let a = write(&picked, "a.txt", "第一本\n");
        let lib = tmp.path().join("books");
        let store = Store::in_memory().unwrap();

        let ctx = StageCtx::detached();
        ctx.cancel().trigger();
        let source = PathsSource::picked(vec![a]);
        let outcomes = import_from(&source, &lib, &store, &ctx);
        assert!(outcomes.is_empty(), "取消后不该再处理任何候选");
        assert!(!lib.join("a.txt").exists(), "副本也不该落");
    }

    /// 取消落在两本**之间**：已经进去的那本是真的，后面的不再来 ——
    /// 这是「架面上的取消按钮」能做的前提。
    #[test]
    fn cancelling_between_books_keeps_what_already_landed() {
        let tmp = tempfile::tempdir().unwrap();
        let picked = tmp.path().join("原处");
        let a = write(&picked, "a.txt", "第一本\n");
        let b = write(&picked, "b.txt", "第二本\n");
        let lib = tmp.path().join("books");
        let store = Store::in_memory().unwrap();

        /// 两本的假来源：第一本 stage 完就置位取消，让循环停在第二本之前。
        struct CancelAfterFirst {
            paths: Vec<std::path::PathBuf>,
            staged: AtomicUsize,
        }
        impl Source for CancelAfterFirst {
            fn kind(&self) -> ImportKind {
                ImportKind::Paths
            }
            fn policy(&self) -> Policy {
                Policy {
                    skip_existing: false,
                    copy_in: true,
                    on_name_clash: NameClash::Overwrite,
                }
            }
            fn candidates(&self, _dest: &Path, _ctx: &StageCtx) -> crate::Result<Vec<Candidate>> {
                Ok(PathsSource::candidates_from(&self.paths))
            }
            fn stage(
                &self,
                candidate: &Candidate,
                dest_dir: &Path,
                ctx: &StageCtx,
            ) -> crate::Result<std::path::PathBuf> {
                let dest = stage_local(candidate, dest_dir, NameClash::Overwrite)?;
                if self.staged.fetch_add(1, Ordering::SeqCst) == 0 {
                    ctx.cancel().trigger();
                }
                Ok(dest)
            }
        }

        let ctx = StageCtx::detached();
        let source = CancelAfterFirst {
            paths: vec![a, b],
            staged: AtomicUsize::new(0),
        };
        let outcomes = import_from(&source, &lib, &store, &ctx);
        assert_eq!(outcomes.len(), 1, "第二本不该再被处理");
        assert!(outcomes[0].result.as_ref().unwrap().is_some());
        assert_eq!(store.shelf("").unwrap().len(), 1, "已进去的那本在库里");
    }
}
