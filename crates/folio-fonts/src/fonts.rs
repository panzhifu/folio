//! 读者自己装进来的字体：内置清单，与把它落到盘上的那几步。
//!
//! 清单**随二进制走**（[`MANIFEST_JSON`]，`include_str!`）：改清单 = 随版本发布。
//! 不学书源那样热更，也不像别的阅读器那样挂一份自己的远端 JSON —— 那等于自己养一份
//! CDN 的可用性。这里的字体全是钉了 tag 的 GitHub 直链（release 资产与 raw 链接），
//! 可用性跟着上游仓库走，中间不再加一层会坏的东西；清单里的 `size` 是发版时逐条
//! `HEAD` 实测的字节数，下载完对不上就拒装（见 [`download_item`]）。
//!
//! 一份字体一个目录（`<fonts 根>/<id>/<文件>`），与词典同形：重名、半装状态与删除
//! 都收敛在目录这一层。装（下载与导入）先落 `<根>/.part-<id>/`，完成后整体改名 ——
//! `<id>` 要么完整出现要么不出现，盘上不留下认得一半的一份；`.part-<id>` 是下一次
//! 重装时开头就清掉的残渣。
//!
//! 下载用同步 [`Transport`]，与 `net::update` 同一条理由：出网的作业跑在后台线程上，
//! 「一个请求流到底」用阻塞读最直白，这一层因此不需要任何执行器。字节**原样落盘**
//! （woff / woff2 也存原样）：解回 sfnt 是注册前那一脚的事，与清单 / 下载同住这里
//! （[`decode_font_file`]），与随书字体同一条路。

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use folio_base::paths;
use folio_net::{Cancel, Error as TransportError, Method, Request, Sink, Transport};

/// 被套上上下文的底层错误；与 `folio_format::Error` 的 `Source` 同一形状。
pub type Source = Box<dyn std::error::Error + Send + Sync + 'static>;

pub type Result<T> = std::result::Result<T, FontsError>;

/// 字体库这一层的错误：清单、下载、落盘装删。
///
/// 与 `folio_format::Error` / folio-translate 的 `DictError` 同一条口径 —— 错误自持、
/// 带上下文、底层原因留在 `source` 链上。机制有自己的 crate，错误词表跟机制走：
/// 原 `folio_core::Error::Font` 变体随迁删除，core 的伞枚举不再代持这一档。
#[derive(Debug, thiserror::Error)]
pub enum FontsError {
    /// 一份字体装不上：清单里没有这个 id、下载下来的字节大小对不上、解不出来。
    ///
    /// 单独立一条而不是复用 [`FontsError::Io`]：设置页要按「这一份字体装坏了」提示
    /// 并让读者删掉重装，而不是把一条路径拼进通用的读盘错误里。
    #[error("字体 {path:?} 装不上：{why}")]
    Font {
        path: PathBuf,
        why: String,
        #[source]
        source: Option<Source>,
    },

    /// 落盘读写失败。
    #[error("读 {path} 失败：{source}")]
    Io {
        path: PathBuf,
        #[source]
        source: Source,
    },

    /// 读者取消了下载：**不是失败**，界面回 `Idle` 就好。单独立一条让调用方按类型
    /// 分档（匹配文案是脆的）；保留 transport 那句「已取消」的原文。
    #[error("已取消")]
    Cancelled,
}

/// 清单文本随二进制走；`include_str!` 的路径相对本文件（`crates/folio-fonts/font_manifest.json`）。
const MANIFEST_JSON: &str = include_str!("../font_manifest.json");

/// 内置清单。每项一份字体一个文件（`url` 直指那一份）；同一家族的另一档字重
/// 在清单里是**另一项** —— 配置里 `FontEntry` 按文件记账，两档字重装删互不牵连。
pub fn manifest() -> &'static Manifest {
    static MANIFEST: OnceLock<Manifest> = OnceLock::new();
    // 内置数据坏了是开发期的错：就地红，别等读者点到「下载」才炸出一句看不懂的。
    MANIFEST.get_or_init(|| {
        Manifest::parse(MANIFEST_JSON).expect("内置字体清单必须是合法的（有测试盯着）")
    })
}

/// 内置清单的一份。
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// 清单格式版本；将来换成远端清单时靠它判兼容。
    pub version: u32,
    #[serde(default)]
    pub items: Vec<FontItem>,
}

/// 清单里的一款字体。
#[derive(Debug, Clone, Deserialize)]
pub struct FontItem {
    /// 装、删、对配置用的键。
    pub id: String,
    /// 界面上展示的名字（与字体文件内部家族名是两回事，后者注册后才知道）。
    pub name: String,
    /// 中文一句话描述。
    pub desc: String,
    /// 英文一句话描述；界面语言不是 zh 时用。
    #[serde(default)]
    pub desc_en: String,
    /// 字节数，发版时逐条实测。下载完成的**大小核对**与界面上「约 25 MB」都靠它。
    pub size: u64,
    /// 首选下载直链（钉 tag）。**先试哪一个**按读者网络的现实排（谁更能下完谁在前），
    /// 不按上游权威性排 —— 字节对不对由 `size` 核对把着，与源无关。
    pub url: String,
    /// 备用源，按序试。对着持续掉速的国际线路（实测 raw 拉长传会掉到 13 KB/s），有
    /// CDN 的镜像常常是能下完与下不完的差别；但**慢而没死**的源救不了 —— 那要靠
    /// 界面上的进度与取消。镜像末段的文件名必须与首选一致（落盘名按首选取），这条
    /// 由清单评审守，代码里不重复判。
    #[serde(default)]
    pub mirrors: Vec<String>,
    /// 官网 / 仓库，设置页外链用的那个。
    #[serde(default)]
    pub official: String,
    pub license: License,
}

/// 一款字体的许可证：设置页上那颗外链的名字与去处。
#[derive(Debug, Clone, Deserialize)]
pub struct License {
    pub name: String,
    pub url: String,
}

impl Manifest {
    /// 解析并校验。清单是仓库里的一份数据，两条底线在这里守：id 不空不重，
    /// url 必须是 https —— 明文的 http 直链等于把装进正文的字节交给中间人。
    pub fn parse(json: &str) -> Result<Manifest> {
        // 原来靠 core 伞枚举的 `Json` 变体接 `?`；自持之后这里自己套上下文，
        // 原始的 serde 错误留在链上。
        let manifest: Manifest = serde_json::from_str(json).map_err(|err| FontsError::Font {
            path: MANIFEST_PATH.into(),
            why: format!("清单不是合法 JSON：{err}"),
            source: Some(Box::new(err)),
        })?;
        let mut seen = BTreeSet::new();
        for item in &manifest.items {
            if item.id.trim().is_empty() || !seen.insert(item.id.as_str()) {
                return Err(font_error(
                    MANIFEST_PATH,
                    format!("清单里 id 重复或为空：{:?}", item.id),
                ));
            }
            if !item.url.starts_with("https://") {
                return Err(font_error(
                    MANIFEST_PATH,
                    format!("{} 的下载地址不是 https：{}", item.id, item.url),
                ));
            }
            for mirror in &item.mirrors {
                if !mirror.starts_with("https://") {
                    return Err(font_error(
                        MANIFEST_PATH,
                        format!("{} 的镜像地址不是 https：{}", item.id, mirror),
                    ));
                }
            }
        }
        Ok(manifest)
    }

    /// 按 id 取一款；设置页从「已装列表」跳回清单时用。
    pub fn item(&self, id: &str) -> Option<&FontItem> {
        self.items.iter().find(|item| item.id == id)
    }
}

/// 清单在仓库里的逻辑路径；错误文案里指认清单自身时用。
const MANIFEST_PATH: &str = "font_manifest.json";

/// 装得下的字体扩展名。woff / woff2 存原样、注册前解回 sfnt（见模块注释）；
/// 其余的 fontdb 与 gpui 都直接吃。
pub const INSTALLABLE_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc", "woff", "woff2"];

/// 下载清单里的一款字体，落到 `<root>/<id>/<文件名>`，交回最终路径。
///
/// 传输由调用方递进来（[`folio_net::ureq::shared`] 的那一份，或测试里的
/// 假传输）—— 与 `net::update::download_to_file` 同一形状，离线测试才写得出来。
///
/// 源的顺序是主源在前、镜像按清单给序跟进：一个源失败（连不上、大小对不上）就换
/// 下一个，取消是**读者的话**，不换源直接回。文件名按主源取，暂存目录整个失败
/// 才清 —— 下一轮重装开头也会清。
///
/// 先装进 `<root>/.part-<id>/` 再整体改名（见模块注释）；`<id>` 已在时先删旧的 ——
/// 重装一次就是一次完整的替换，不留新旧两份混着的目录。每个源下完都按清单里的
/// `size` 核对字节数：对不上意味着中间有网关压过或上游换了资产，装进正文的字节
/// 不能靠猜，当场拒。
pub fn download_item(
    item: &FontItem,
    transport: &dyn Transport,
    root: &Path,
    cancel: &Cancel,
    mut progress: impl FnMut(u64) + Send,
) -> Result<PathBuf> {
    let file_name = file_name_of(&item.url)?;
    let staging = root.join(format!(".part-{}", item.id));
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|err| io_error(&staging, err))?;
    }
    std::fs::create_dir_all(&staging).map_err(|err| io_error(&staging, err))?;

    let urls = std::iter::once(item.url.as_str()).chain(item.mirrors.iter().map(String::as_str));
    let mut last_err = None;
    for (nth, url) in urls.enumerate() {
        match download_one(
            url,
            &file_name,
            &staging,
            item.size,
            transport,
            cancel,
            &mut progress,
        ) {
            Ok(()) => {
                last_err = None;
                break;
            }
            // 取消不换源：那是读者的话，不是哪个源的错。
            Err(err @ FontsError::Cancelled) => return Err(err),
            Err(err) => {
                tracing::warn!("fonts: 第 {} 个源失败，换下一个：{}", nth + 1, err);
                last_err = Some(err);
            }
        }
    }
    if let Some(err) = last_err {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err);
    }

    let target = root.join(&item.id);
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(|err| io_error(&target, err))?;
    }
    std::fs::rename(&staging, &target).map_err(|err| {
        let _ = std::fs::remove_dir_all(&staging);
        io_error(&target, err)
    })?;
    Ok(target.join(file_name))
}

/// 对一个源下完整份：建 `.part` 文件、流到底、按 `expected` 核对大小。
/// 失败时把半份文件清掉（暂存目录留给下一个源或下一轮）。
fn download_one(
    url: &str,
    file_name: &str,
    staging: &Path,
    expected: u64,
    transport: &dyn Transport,
    cancel: &Cancel,
    progress: &mut (impl FnMut(u64) + Send),
) -> Result<()> {
    let part = staging.join(file_name);
    let mut sink = PartSink {
        file: std::fs::File::create(&part).map_err(|err| io_error(&part, err))?,
        path: part.clone(),
        written: 0,
        // 进度闭包跨源复用，这里转一手。
        progress: |received| progress(received),
    };
    let request = Request::new(Method::Get, url.to_owned());
    let outcome = transport.download(request, &mut sink, cancel);
    match outcome {
        Ok(_) => {}
        // 已取消的半份在 abort 里清。取消保持 remote 层的类型（`Error::Remote(Cancelled)`）：
        // 调用方按类型分档「取消不算失败」，匹配文案是脆的。
        Err(TransportError::Cancelled) => return Err(FontsError::Cancelled),
        Err(err) => {
            let _ = std::fs::remove_file(&part);
            return Err(transport_error(&part, err));
        }
    }
    if expected > 0 && sink.written != expected {
        let _ = std::fs::remove_file(&part);
        return Err(font_error(
            &part,
            format!(
                "下载到的字节数对不上（清单说 {expected}，拿到 {}）：换网络重试，或等清单更新",
                sink.written
            ),
        ));
    }
    Ok(())
}

/// 把本地一份字体文件拷进 `<root>/<id>/<file>`（导入的落盘半程），交回最终路径。
///
/// 调用方先定好 `id`（[`dir_id`] 洗名、[`unique_id`] 避撞）与洗过的文件名；这里只管
/// 原子地落：先拷进 `.part-<id>` 目录再整体改名，拷一半失败盘上不留认得一半的一份。
pub fn install_copy(src: &Path, root: &Path, id: &str, file_name: &str) -> Result<PathBuf> {
    if !INSTALLABLE_EXTENSIONS.contains(
        &src.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref()
            .unwrap_or_default(),
    ) {
        return Err(font_error(
            src,
            format!(
                "不是装得下的字体（认 {}）",
                INSTALLABLE_EXTENSIONS.join(" / ")
            ),
        ));
    }
    let staging = root.join(format!(".part-{id}"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|err| io_error(&staging, err))?;
    }
    std::fs::create_dir_all(&staging).map_err(|err| io_error(&staging, err))?;
    let part = staging.join(file_name);
    std::fs::copy(src, &part).map_err(|err| io_error(src, err))?;

    let target = root.join(id);
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(|err| io_error(&target, err))?;
    }
    std::fs::rename(&staging, &target).map_err(|err| {
        let _ = std::fs::remove_dir_all(&staging);
        io_error(&target, err)
    })?;
    Ok(target.join(file_name))
}

/// 从洗好的名字起一个目录 id：不落盘的字符换 `_`，首尾空白与点去掉，压到 80 字。
/// 洗空了回落 `font` —— 空串当目录名是另一类事故。
pub fn dir_id(stem: &str) -> String {
    let mut out = String::new();
    for ch in stem.chars() {
        if paths::is_unsafe_name_char(ch) {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    let out = out
        .trim_matches(|c: char| c.is_whitespace() || c == '.')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    let out: String = out.chars().take(80).collect();
    if out.is_empty() {
        "font".to_string()
    } else {
        out
    }
}

/// 避开已占用的 id：根下有同名目录、或 `taken` 里已有的，加 `-2` / `-3` 往后找。
/// 同一批里刚起的 id 也会占住（`taken` 是调用方拿着可变的集合），连导两份同名不撞车。
pub fn unique_id(base: &str, taken: &mut BTreeSet<String>, root: &Path) -> String {
    let mut candidate = base.to_string();
    let mut n = 2;
    while taken.contains(&candidate) || root.join(&candidate).exists() {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    taken.insert(candidate.clone());
    candidate
}

/// 删掉 `<root>/<id>`。目录已经不在算成功 —— 配置里那一条也该跟着摘掉。
pub fn remove(root: &Path, id: &str) -> Result<()> {
    if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\']) {
        return Err(font_error(root, format!("不合法的字体 id：{id:?}")));
    }
    let dir = root.join(id);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io_error(&dir, err)),
    }
}

/// 洗一个文件名：不落盘的字符换 `_`。清单直链的末段与导入的原始文件名都过这一脚。
pub fn wash_file_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if paths::is_unsafe_name_char(ch) {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    out
}

/// 从 URL 末段取文件名：剥查询串、剥路径，落盘的字符换 `_`，洗空了拒装 ——
/// 我们的清单里 URL 都是钉死的直链，走到「洗空」说明清单被改坏了。
fn file_name_of(url: &str) -> Result<String> {
    let tail = url.split(['?', '#']).next().unwrap_or(url);
    let name = tail.rsplit('/').next().unwrap_or_default();
    if name.is_empty() {
        return Err(font_error(
            MANIFEST_PATH,
            format!("URL 末段取不出文件名：{url}"),
        ));
    }
    Ok(wash_file_name(name))
}

/// 下到 `.part` 的那份：块块进文件，`finish` 冲盘，`abort` 清残渣。
///
/// 结构与 `net::update::FileSink` 同形而**不共用**：那边的 `finish` 顺手改名到最终
/// 路径（单文件语义），这边改名发生在**目录**这一层（多一份半装状态要一起收）。
/// 硬掰成一份会让两边的语义互相将就。
struct PartSink<P> {
    file: std::fs::File,
    path: PathBuf,
    written: u64,
    progress: P,
}

// `Sink: Send`，所以进度闭包也得是 Send：它会在下载线程上被调。
impl<P: FnMut(u64) + Send> Sink for PartSink<P> {
    fn write(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        self.file.write_all(chunk)?;
        self.written += chunk.len() as u64;
        (self.progress)(self.written);
        Ok(())
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }

    fn abort(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn font_error(path: impl Into<PathBuf>, why: impl Into<String>) -> FontsError {
    FontsError::Font {
        path: path.into(),
        why: why.into(),
        source: None,
    }
}

fn io_error(path: &Path, err: std::io::Error) -> FontsError {
    FontsError::Io {
        path: path.to_path_buf(),
        source: Box::new(err),
    }
}

fn transport_error(path: &Path, err: TransportError) -> FontsError {
    FontsError::Font {
        path: path.to_path_buf(),
        why: err.to_string(),
        source: Some(Box::new(err)),
    }
}

/// 一份字体文件的字节是哪一种（按头四个字节的魔数认）。判据与
/// [`folio_format::woff(2)::is_*`] 同源：那两份只认自己的魔数，
/// 这里把五类收齐 —— 注册入口只该有一个。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFileKind {
    /// WOFF 1.0，注册前解回 sfnt。
    Woff,
    /// WOFF 2.0，同上。
    Woff2,
    /// sfnt 一族（TTF / OTF / TTC），原样注册。
    Sfnt,
    /// 不是认得的字体。
    Unknown,
}

/// 按头四个字节的魔数认。
pub fn font_kind(bytes: &[u8]) -> FontFileKind {
    if bytes.len() < 4 {
        return FontFileKind::Unknown;
    }
    match &bytes[..4] {
        b"wOFF" => FontFileKind::Woff,
        b"wOF2" => FontFileKind::Woff2,
        b"ttcf" | b"OTTO" | [0x00, 0x01, 0x00, 0x00] => FontFileKind::Sfnt,
        _ => FontFileKind::Unknown,
    }
}

/// 把一份字体文件的字节解成可注册的 sfnt：woff / woff2 解回，sfnt 原样交出，
/// 认不出的回 `None`。自装字体的注册入口（开机重注册与装完当场注册）共用这一脚 ——
/// 从前「按魔数分五类」与「解回 sfnt」写在 app 侧一处，现在与清单/下载同住这里。
pub fn decode_font_file(raw: &[u8]) -> Option<Vec<u8>> {
    match font_kind(raw) {
        FontFileKind::Sfnt => Some(raw.to_vec()),
        FontFileKind::Woff => folio_format::woff::decode(raw),
        FontFileKind::Woff2 => folio_format::woff2::decode(raw),
        FontFileKind::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    /// 魔数认五类：woff / woff2 要解，ttf / otf / ttc 原样，其余不收。
    /// 2026-11-09 从 `folio_app::…::platform::user_fonts` 搬来（`kind_of` 下沉为
    /// [`font_kind`]，见上）。
    #[test]
    fn the_magic_sniffer_sorts_the_five_kinds() {
        use super::{FontFileKind, font_kind};
        use FontFileKind::{Sfnt, Unknown, Woff, Woff2};
        assert!(matches!(font_kind(b"wOF2\x00"), Woff2));
        assert!(matches!(font_kind(b"wOFF\x00"), Woff));
        assert!(matches!(font_kind(b"ttcf\x00"), Sfnt));
        assert!(matches!(font_kind(b"OTTO\x00"), Sfnt));
        assert!(matches!(font_kind(&[0x00, 0x01, 0x00, 0x00]), Sfnt));
        assert!(matches!(font_kind(b"PK\x03\x04"), Unknown), "zip 不收");
        assert!(matches!(font_kind(b"nope"), Unknown));
        assert!(matches!(font_kind(b"abc"), Unknown), "不足四字节不收");
    }

    use super::{
        FontItem, Manifest, dir_id, download_item, install_copy, manifest, remove, unique_id,
    };
    use folio_net::{Cancel, Error as TransportError, Request, Sink, Transport};
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    /// 内置清单必须解析得过，id 不重、地址全是 https、字节数是真的：
    /// 这三条是 [`download_item`] 大小核对与设置页展示的地基，清单手改坏一条
    /// 就在这里红，别等读者点到「下载」。
    #[test]
    fn the_bundled_manifest_is_valid() {
        let manifest = manifest();
        assert!(manifest.version >= 1);
        assert!(manifest.items.len() >= 4, "清单至少要有四款候选");
        let mut ids = BTreeSet::new();
        for item in &manifest.items {
            assert!(ids.insert(item.id.as_str()), "id 重复：{}", item.id);
            assert!(item.url.starts_with("https://"), "{} 不是 https", item.id);
            assert!(item.size > 0, "{} 的字节数没实测过", item.id);
            assert!(!item.license.name.is_empty());
            assert!(!item.name.is_empty());
        }
    }

    /// 同 id 再取一次拿到同一条；没有的 id 回 None 而不是炸。
    #[test]
    fn item_lookup_by_id() {
        let manifest = manifest();
        let first = &manifest.items[0];
        let found = manifest.item(&first.id).expect("第一项按 id 要找得到");
        assert_eq!(found.url, first.url);
        assert!(manifest.item("不存在的 id").is_none());
    }

    /// 假传输：`download` 把预备好的字节分几块写进 sink，块间查取消 ——
    /// 与真 `Transport` 的契约同形，离线测落盘的那一半。
    struct Chunked {
        body: Vec<u8>,
        chunk: usize,
        /// 触发后下一次块边界让传输报取消，模拟读者按了「取消」。
        cancel_after: Mutex<Option<usize>>,
    }

    impl Chunked {
        fn with(body: Vec<u8>, chunk: usize) -> Self {
            Self {
                body,
                chunk,
                cancel_after: Mutex::new(None),
            }
        }
    }

    impl Transport for Chunked {
        fn request(&self, _req: Request) -> folio_net::Result<folio_net::Response> {
            unreachable!("字体下载只走 download")
        }

        fn download(
            &self,
            _req: Request,
            sink: &mut dyn Sink,
            cancel: &Cancel,
        ) -> folio_net::Result<u64> {
            let mut sent = 0;
            let mut nth = 0;
            while sent < self.body.len() {
                if cancel.triggered() {
                    sink.abort();
                    return Err(TransportError::Cancelled);
                }
                let end = (sent + self.chunk).min(self.body.len());
                sink.write(&self.body[sent..end])
                    .map_err(|err| TransportError::Transport(err.to_string()))?;
                sent = end;
                nth += 1;
                if let Some(after) = *self.cancel_after.lock().unwrap()
                    && nth >= after
                {
                    cancel.trigger();
                }
            }
            sink.finish()
                .map_err(|err| TransportError::Transport(err.to_string()))?;
            Ok(sent as u64)
        }
    }

    fn item_with(url: &str, size: u64) -> FontItem {
        FontItem {
            id: "sample".to_string(),
            name: "样例".to_string(),
            desc: String::new(),
            desc_en: String::new(),
            size,
            url: url.to_string(),
            mirrors: Vec::new(),
            official: String::new(),
            license: super::License {
                name: "SIL OFL 1.1".to_string(),
                url: "https://example.test/ofl".to_string(),
            },
        }
    }

    /// 主源失败换镜像：落盘的还是主源的文件名，字节来自镜像，暂存收干净。
    #[test]
    fn a_failed_primary_falls_through_to_the_mirror() {
        struct Picky;
        impl Transport for Picky {
            fn request(&self, _req: Request) -> folio_net::Result<folio_net::Response> {
                unreachable!("字体下载只走 download")
            }
            fn download(
                &self,
                req: Request,
                sink: &mut dyn Sink,
                _cancel: &Cancel,
            ) -> folio_net::Result<u64> {
                if req.url.contains("mirror") {
                    sink.write(b"mirror-bytes")
                        .map_err(|err| TransportError::Transport(err.to_string()))?;
                    sink.finish()
                        .map_err(|err| TransportError::Transport(err.to_string()))?;
                    Ok(12)
                } else {
                    Err(TransportError::Transport("主源挂了".to_string()))
                }
            }
        }
        let root = tempfile::tempdir().expect("字体根");
        let mut item = item_with("https://example.test/fonts/Demo-Regular.ttf", 12);
        item.mirrors = vec!["https://mirror.example.test/Demo-Regular.ttf".to_string()];
        let landed =
            download_item(&item, &Picky, root.path(), &Cancel::new(), |_| {}).expect("镜像要接住");
        assert_eq!(landed, root.path().join("sample").join("Demo-Regular.ttf"));
        assert_eq!(
            std::fs::read(&landed).expect("读回"),
            b"mirror-bytes",
            "字节要来自镜像"
        );
        assert!(!root.path().join(".part-sample").exists(), "暂存要收掉");
    }

    /// 下载整趟：`.part-<id>` 收成 `<id>`，文件在里面，进度走到位，大小核对过。
    #[test]
    fn a_download_lands_as_a_whole_directory() {
        let root = tempfile::tempdir().expect("字体根");
        let body = vec![7u8; 2048];
        let transport = Chunked::with(body.clone(), 300);
        let item = item_with(
            "https://example.test/fonts/Demo-Regular.ttf",
            body.len() as u64,
        );

        let mut last = 0u64;
        let landed = download_item(&item, &transport, root.path(), &Cancel::new(), |received| {
            last = received;
        })
        .expect("下载要成");
        assert_eq!(last, body.len() as u64, "进度要走到最后一个块");
        assert!(!root.path().join(".part-sample").exists(), "暂存目录要收掉");
        assert_eq!(landed, root.path().join("sample").join("Demo-Regular.ttf"));
        assert_eq!(
            std::fs::read(&landed).expect("读回来"),
            body,
            "字节要原样落盘"
        );
    }

    /// 大小核对：清单说 100、拿到 99，当场拒且暂存清掉，不留认得一半的一份。
    #[test]
    fn a_size_mismatch_rejects_the_install() {
        let root = tempfile::tempdir().expect("字体根");
        let body = vec![7u8; 99];
        let transport = Chunked::with(body, 300);
        let item = item_with("https://example.test/Demo-Regular.ttf", 100);
        let err = download_item(&item, &transport, root.path(), &Cancel::new(), |_| {})
            .expect_err("大小对不上要拒");
        assert!(err.to_string().contains("对不上"), "{}", err);
        assert!(
            !root.path().join(".part-sample").exists(),
            "拒装时暂存要清掉"
        );
        assert!(!root.path().join("sample").exists(), "正式目录不能出现");
    }

    /// 取消：块间触发取消旗标，下载当场停，正式目录不出现；残渣留给下一次开头清。
    #[test]
    fn a_cancelled_download_leaves_no_target() {
        let root = tempfile::tempdir().expect("字体根");
        let body = vec![7u8; 4096];
        let transport = Chunked::with(body, 256);
        *transport.cancel_after.lock().unwrap() = Some(2);
        let item = item_with("https://example.test/Demo-Regular.ttf", 0);
        let cancel = Cancel::new();
        let err =
            download_item(&item, &transport, root.path(), &cancel, |_| {}).expect_err("取消要报错");
        assert!(err.to_string().contains("已取消"), "{}", err);
        assert!(!root.path().join("sample").exists(), "正式目录不能出现");
        assert!(
            !root
                .path()
                .join(".part-sample")
                .join("Demo-Regular.ttf")
                .exists(),
            "半份文件要被 abort 清掉"
        );
        // 下一次重装开头把残渣目录收掉。
        let good = Chunked::with(vec![1u8; 10], 100);
        let item = item_with("https://example.test/Demo-Regular.ttf", 10);
        download_item(&item, &good, root.path(), &Cancel::new(), |_| {}).expect("重装要成");
        assert!(
            root.path()
                .join("sample")
                .join("Demo-Regular.ttf")
                .is_file()
        );
    }

    /// 导入：拷进 `<root>/<id>/<file>`；重名 id 加后缀；删掉之后根下无痕；
    /// 再删一次不报错；不认的扩展名当场拒。
    #[test]
    fn install_copy_uniquifies_and_removes() {
        let root = tempfile::tempdir().expect("字体根");
        let src = tempfile::tempdir().expect("来源目录");
        let font = src.path().join("我的字体.ttf");
        std::fs::write(&font, b"ttf-bytes").expect("写假字体");

        let taken = &mut BTreeSet::new();
        let id = unique_id(&dir_id("我的字体"), taken, root.path());
        let landed = install_copy(&font, root.path(), &id, "我的字体.ttf").expect("导入");
        assert_eq!(landed, root.path().join(&id).join("我的字体.ttf"));
        assert_eq!(
            std::fs::read(&landed).expect("读回"),
            b"ttf-bytes",
            "字节要原样"
        );

        // 同名再导：id 加后缀，旧的还在。
        let second = unique_id(&dir_id("我的字体"), taken, root.path());
        assert_ne!(id, second, "重名该加后缀");
        install_copy(&font, root.path(), &second, "我的字体.ttf").expect("第二次导入");
        assert!(root.path().join(&id).join("我的字体.ttf").is_file());

        remove(root.path(), &id).expect("删除");
        assert!(!root.path().join(&id).exists(), "删完目录要不在");
        remove(root.path(), &id).expect("重复删除算成功");

        let not_a_font = src.path().join("readme.txt");
        std::fs::write(&not_a_font, b"no").expect("写");
        assert!(
            install_copy(&not_a_font, root.path(), "x", "readme.txt").is_err(),
            "不认的扩展名要拒"
        );
    }

    /// `dir_id` 的洗法：不落盘的字符换 `_`、首尾空白与点去掉、洗空回落 `font`。
    #[test]
    fn dir_id_washes_unsafe_characters() {
        assert_eq!(dir_id("a/b:c"), "a_b_c");
        // 换成 `_` 之后不是空串的，就是个合法目录名，不硬回落。
        assert_eq!(dir_id("///"), "___");
        assert_eq!(dir_id(" . "), "font");
        assert_eq!(dir_id(""), "font");
    }

    /// 残渣目录（上次取消留下的 `.part-<id>`）在重装开头被清掉 —— 用文件系统
    /// 直接摆一个残渣，然后走导入的原子半程看它收干净。
    #[test]
    fn a_stale_part_directory_is_cleared_on_reinstall() {
        let root = tempfile::tempdir().expect("字体根");
        let stale = root.path().join(".part-样例");
        std::fs::create_dir_all(&stale).expect("摆残渣");
        std::fs::write(stale.join("half.ttf"), b"half").expect("写残渣");

        let src = tempfile::tempdir().expect("来源");
        let font = src.path().join("样例.ttf");
        std::fs::write(&font, b"full").expect("写");
        install_copy(&font, root.path(), "样例", "样例.ttf").expect("重装");
        assert!(!stale.exists(), "残渣目录要被清掉");
        assert_eq!(
            std::fs::read(root.path().join("样例").join("样例.ttf")).expect("读回"),
            b"full"
        );
    }

    /// 清单校验的两条底线：id 重复、非 https 直链，都要当场红。
    #[test]
    fn manifest_validation_rejects_bad_items() {
        assert!(Manifest::parse(r#"{"version":1,"items":[]}"#).is_ok());
        let dup = r#"{"version":1,"items":[
            {"id":"a","name":"A","desc":"","size":1,"url":"https://x/a.ttf","license":{"name":"","url":""}},
            {"id":"a","name":"A2","desc":"","size":1,"url":"https://x/a2.ttf","license":{"name":"","url":""}}]}"#;
        let err = Manifest::parse(dup).expect_err("id 重复要红");
        assert!(err.to_string().contains("id"), "{}", err);
        let plain = r#"{"version":1,"items":[
            {"id":"a","name":"A","desc":"","size":1,"url":"http://x/a.ttf","license":{"name":"","url":""}}]}"#;
        assert!(Manifest::parse(plain).is_err(), "http 直链要红");
    }
}
