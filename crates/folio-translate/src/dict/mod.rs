//! StarDict 本地词典：解析、清洗、精确查找。
//!
//! 这一层只认磁盘上的词典目录，不认「界面」：`.ifo` 里的书目信息、`.idx` 的词表、
//! `.dict` 的释义都按 [StarDict 文件格式](https://github.com/huzheng001/stardict-3/blob/master/dict/doc/StarDictFileFormat)
//! 读。`.idx.gz` / `.dict.dz` 就是 gzip 流（dictzip 的随机访问扩展在顺序解压下可以忽略），
//! 所以不新增解码依赖。
//!
//! **内存账**：打开一本词典会把 `.idx` 与 `.dict` 整个读进内存再排序词表。StarDict 的
//! 常规体积在这条线上（几十 MB），而查词要的是零延迟与可跨词典合并；分块随机读会让
//! 每次查词都回到磁盘，收益在手机上抵不过那一跳。上限写死在 [`MAX_IDX_BYTES`] /
//! [`MAX_DICT_BYTES`]，超了就在导入那一步拒掉，而不是运行到一半 OOM。

pub mod library;

pub use library::{Hit, ImportedDict, Source};

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

/// 词典这一层的错误，**自持**（与 folio-format / folio-net 同一条口径）：core 的伞
/// 枚举不再有 `Dictionary` 变体，消费方拿到的 Display 文案与搬来前一格不变。
///
/// 约定照旧：每个变体都带上下文（哪个路径、为什么），底层错误留在 `#[source]` 链上，
/// 不压成字符串。
#[derive(Debug, thiserror::Error)]
pub enum DictError {
    #[error("读 {path} 失败：{source}")]
    Io {
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// StarDict 词典读不动：缺文件、版本不认识、索引与数据对不上。
    ///
    /// 单独立一条而不是复用 [`DictError::Io`]：设置页要按「这一本词典坏了」提示并让
    /// 读者删掉重导，而不是把一份路径拼进通用的读盘错误里；导入那一趟也要能原样把它
    /// 交给弹层。
    #[error("词典 {path:?} 读不动：{why}")]
    Dictionary {
        path: PathBuf,
        why: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    },
}

pub type Result<T> = std::result::Result<T, DictError>;

/// `.idx` 解压后的上限。词表按 4/8 字节偏移 + 词本身算，32 MiB 已经覆盖百万词级。
const MAX_IDX_BYTES: u64 = 32 * 1024 * 1024;
/// `.dict` 解压后的上限：常见的大型英汉词典在 100 MB 上下，留一倍余量。
const MAX_DICT_BYTES: u64 = 256 * 1024 * 1024;
/// `.ifo` 只有几行；读它时留一个宽容的上限，防的是把别的文件改名成 `.ifo`。
const MAX_IFO_BYTES: u64 = 1024 * 1024;

/// `.ifo` 里的书目信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    /// 词典自己的书名（StarDict 的 `bookname`）。
    pub bookname: String,
    /// 声明的词条数；缺这一行时是 0。
    pub wordcount: u64,
    /// `version=` 原值，例如 `2.4.2` / `3.0.0`。
    pub version: String,
    /// 索引里的偏移是 32 位还是 64 位。
    pub offset_bits: u32,
    /// `sametypesequence` 原值；空串表示每条记录自带类型字节。
    pub sametypesequence: String,
}

/// 一条可展示的词条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub word: String,
    /// 音标（`t` 型字段）。没有就是 `None`。
    pub phonetic: Option<String>,
    /// 释义正文：已去标签、解码实体、保留段落换行。
    pub meaning: String,
}

/// 一本打开了的 StarDict 词典。
pub struct StarDict {
    meta: Meta,
    data: Vec<u8>,
    entries: Vec<Indexed>,
}

#[derive(Debug, Clone)]
struct Indexed {
    word: String,
    /// 大小写折叠后的键，排序与二分都读它。
    lower: String,
    offset: usize,
    size: usize,
}

impl StarDict {
    /// 只读 `.ifo` 与文件存在性，不读大文件。导入前先用它校验。
    pub fn metadata(dir: &Path) -> Result<Meta> {
        let files = Files::locate(dir)?;
        files.meta()
    }

    /// 只读某一个 `.ifo` 的书目信息。给「一个文件夹里放着好几本词典」的导入用。
    pub fn metadata_of(ifo: &Path) -> Result<Meta> {
        Files::from_ifo(ifo.to_path_buf())?.meta()
    }

    /// 打开一本词典。目录里必须恰好有一个 `.ifo`。
    pub fn open(dir: &Path) -> Result<Self> {
        let files = Files::locate(dir)?;
        let meta = files.meta()?;
        let idx = files.read_idx()?;
        let data = files.read_dict()?;
        let mut entries = parse_index(&idx, meta.offset_bits, &files.idx, data.len())?;
        entries.sort_by(|a, b| a.lower.cmp(&b.lower).then_with(|| a.word.cmp(&b.word)));
        Ok(Self {
            meta,
            data,
            entries,
        })
    }

    pub fn bookname(&self) -> &str {
        &self.meta.bookname
    }

    pub fn wordcount(&self) -> u64 {
        self.meta.wordcount
    }

    /// 精确查找（大小写不敏感），最多回 `limit` 条。
    ///
    /// 同一词头在词典里出现多次时按存储顺序返回；调用方自己决定合并 / 截断。
    pub fn lookup(&self, query: &str, limit: usize) -> Vec<Entry> {
        if limit == 0 {
            return Vec::new();
        }
        let key = query.to_lowercase();
        let start = self
            .entries
            .partition_point(|entry| entry.lower.as_str() < key.as_str());
        let mut out = Vec::new();
        for indexed in &self.entries[start..] {
            if indexed.lower != key {
                break;
            }
            if let Some(entry) = self.record(indexed) {
                out.push(entry);
                if out.len() >= limit {
                    break;
                }
            }
        }
        out
    }

    fn record(&self, indexed: &Indexed) -> Option<Entry> {
        let end = indexed.offset.checked_add(indexed.size)?;
        let bytes = self.data.get(indexed.offset..end)?;
        let (phonetic, meaning) = parse_fields(bytes, &self.meta.sametypesequence);
        if meaning.is_empty() {
            return None;
        }
        Some(Entry {
            word: indexed.word.clone(),
            phonetic,
            meaning,
        })
    }
}

/// 查询词归一：去首尾空白与常见标点，内部空白折成一个空格。
pub fn normalize_query(raw: &str) -> String {
    const EDGE_PUNCT: &[char] = &[
        '。', '，', '、', '；', '：', '！', '？', '…', '—', '"', '\'', '“', '”', '‘', '’', '（',
        '）', '(', ')', '【', '】', '[', ']', '《', '》', '「', '」', '『', '』', '<', '>', '《',
        '》', ',', '.', ';', ':', '!', '?', '-', '·',
    ];
    raw.trim_matches(|c: char| c.is_whitespace() || EDGE_PUNCT.contains(&c))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

struct Files {
    stem: String,
    ifo: PathBuf,
    idx: PathBuf,
    dict: PathBuf,
}

impl Files {
    fn locate(dir: &Path) -> Result<Self> {
        let mut ifos = Vec::new();
        let entries = fs::read_dir(dir).map_err(|err| DictError::Io {
            path: dir.to_path_buf(),
            source: Box::new(err),
        })?;
        for entry in entries {
            let entry = entry.map_err(|err| DictError::Io {
                path: dir.to_path_buf(),
                source: Box::new(err),
            })?;
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("ifo"))
            {
                ifos.push(path);
            }
        }
        ifos.sort();
        let ifo = match ifos.as_slice() {
            [one] => one.clone(),
            [] => return Err(dict_error(dir, "目录里没有 .ifo")),
            _ => return Err(dict_error(dir, "目录里不止一个 .ifo，请一本一个文件夹")),
        };
        Self::from_ifo(ifo)
    }

    fn from_ifo(ifo: PathBuf) -> Result<Self> {
        let dir = ifo
            .parent()
            .ok_or_else(|| dict_error(&ifo, "这个 .ifo 没有所在目录"))?;
        let stem = ifo
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| dict_error(&ifo, "文件名不是合法 UTF-8"))?
            .to_string();
        let plain_idx = dir.join(format!("{stem}.idx"));
        let gz_idx = dir.join(format!("{stem}.idx.gz"));
        let idx = if plain_idx.is_file() {
            plain_idx
        } else if gz_idx.is_file() {
            gz_idx
        } else {
            return Err(dict_error(dir, "缺 .idx / .idx.gz"));
        };
        let plain_dict = dir.join(format!("{stem}.dict"));
        let dz_dict = dir.join(format!("{stem}.dict.dz"));
        let dict = if plain_dict.is_file() {
            plain_dict
        } else if dz_dict.is_file() {
            dz_dict
        } else {
            return Err(dict_error(dir, "缺 .dict / .dict.dz"));
        };
        Ok(Self {
            stem,
            ifo,
            idx,
            dict,
        })
    }

    fn meta(&self) -> Result<Meta> {
        let text = read_text(&self.ifo, MAX_IFO_BYTES)?;
        let mut bookname = String::new();
        let mut wordcount = 0u64;
        let mut version = String::new();
        let mut offset_bits = 32u32;
        let mut sametypesequence = String::new();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "bookname" => bookname = value.to_string(),
                "wordcount" => wordcount = value.parse().unwrap_or(0),
                "version" => version = value.to_string(),
                "idxoffsetbits" => {
                    if value == "64" {
                        offset_bits = 64;
                    }
                }
                "sametypesequence" => sametypesequence = value.to_string(),
                _ => {}
            }
        }
        if version.is_empty() {
            return Err(dict_error(&self.ifo, "缺 version"));
        }
        if !(version.starts_with("2.") || version.starts_with("3.")) {
            return Err(dict_error(
                &self.ifo,
                format!("不认识的 StarDict 版本 {version}"),
            ));
        }
        if version.starts_with("3.") {
            offset_bits = 64;
        }
        if bookname.is_empty() {
            bookname = self.stem.clone();
        }
        Ok(Meta {
            bookname,
            wordcount,
            version,
            offset_bits,
            sametypesequence,
        })
    }

    fn read_idx(&self) -> Result<Vec<u8>> {
        read_bytes(&self.idx, MAX_IDX_BYTES, is_gzip(&self.idx))
    }

    fn read_dict(&self) -> Result<Vec<u8>> {
        read_bytes(&self.dict, MAX_DICT_BYTES, is_gzip(&self.dict))
    }
}

fn is_gzip(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz") || ext.eq_ignore_ascii_case("dz"))
}

fn read_text(path: &Path, max: u64) -> Result<String> {
    let bytes = read_bytes(path, max, false)?;
    String::from_utf8(bytes).map_err(|err| dict_error(path, format!("不是 UTF-8：{err}")))
}

fn read_bytes(path: &Path, max: u64, gzip: bool) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|err| DictError::Io {
        path: path.to_path_buf(),
        source: Box::new(err),
    })?;
    let mut out = Vec::new();
    let read = if gzip {
        let mut decoder = GzDecoder::new(BufReader::new(file));
        decoder
            .by_ref()
            .take(max + 1)
            .read_to_end(&mut out)
            .map_err(|err| dict_error(path, format!("解压失败：{err}")))?;
        out.len() as u64
    } else {
        let mut reader = BufReader::new(file);
        reader
            .by_ref()
            .take(max + 1)
            .read_to_end(&mut out)
            .map_err(|err| DictError::Io {
                path: path.to_path_buf(),
                source: Box::new(err),
            })?;
        out.len() as u64
    };
    if read > max {
        return Err(dict_error(
            path,
            format!("文件太大（超过 {} MiB）", max / (1024 * 1024)),
        ));
    }
    Ok(out)
}

fn parse_index(
    bytes: &[u8],
    offset_bits: u32,
    path: &Path,
    data_len: usize,
) -> Result<Vec<Indexed>> {
    let stride = if offset_bits == 64 { 12 } else { 8 };
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let Some(nul) = bytes[pos..].iter().position(|byte| *byte == 0) else {
            return Err(dict_error(path, "索引的词头没有以 0 结尾"));
        };
        let word_bytes = &bytes[pos..pos + nul];
        let after = pos + nul + 1;
        let Some(tail) = bytes.get(after..after + stride) else {
            return Err(dict_error(path, "索引最后一条不完整"));
        };
        let (offset, size) = if offset_bits == 64 {
            (
                u64::from_be_bytes(tail[0..8].try_into().expect("8 字节")) as usize,
                u32::from_be_bytes(tail[8..12].try_into().expect("4 字节")) as usize,
            )
        } else {
            (
                u32::from_be_bytes(tail[0..4].try_into().expect("4 字节")) as usize,
                u32::from_be_bytes(tail[4..8].try_into().expect("4 字节")) as usize,
            )
        };
        pos = after + stride;
        if size == 0 {
            continue;
        }
        let Some(end) = offset.checked_add(size) else {
            return Err(dict_error(path, "索引里的偏移溢出"));
        };
        if end > data_len {
            return Err(dict_error(
                path,
                format!("索引指向 {end}，而 .dict 只有 {data_len} 字节"),
            ));
        }
        let word = String::from_utf8_lossy(word_bytes).to_string();
        if word.is_empty() {
            continue;
        }
        out.push(Indexed {
            lower: word.to_lowercase(),
            word,
            offset,
            size,
        });
    }
    Ok(out)
}

/// 一条记录 →（音标，释义）。
///
/// `sametypesequence` 为空时数据自带类型字节：小写类型以 0 结尾，大写类型带 4 字节
/// 大端长度前缀。有 `sametypesequence` 时字段按声明顺序排，不再带类型字节。
fn parse_fields(data: &[u8], sametypesequence: &str) -> (Option<String>, String) {
    let mut meaning: Vec<String> = Vec::new();
    let mut phonetic: Option<String> = None;
    let mut pos = 0usize;
    let take = |pos: &mut usize, kind: char| -> Option<Vec<u8>> {
        if kind.is_ascii_uppercase() {
            let len_bytes = data.get(*pos..*pos + 4)?;
            let len = u32::from_be_bytes(len_bytes.try_into().ok()?) as usize;
            let start = *pos + 4;
            let end = start.checked_add(len)?;
            let payload = data.get(start..end)?.to_vec();
            *pos = end;
            Some(payload)
        } else {
            let rest = data.get(*pos..)?;
            let nul = rest.iter().position(|byte| *byte == 0)?;
            let payload = rest[..nul].to_vec();
            *pos += nul + 1;
            Some(payload)
        }
    };
    if sametypesequence.is_empty() {
        while pos < data.len() {
            let kind = data[pos] as char;
            pos += 1;
            let Some(payload) = take(&mut pos, kind) else {
                break;
            };
            absorb(kind, &payload, &mut phonetic, &mut meaning);
        }
    } else {
        for kind in sametypesequence.chars() {
            let Some(payload) = take(&mut pos, kind) else {
                break;
            };
            absorb(kind, &payload, &mut phonetic, &mut meaning);
        }
    }
    (phonetic, meaning.join("\n").trim().to_string())
}

fn absorb(kind: char, payload: &[u8], phonetic: &mut Option<String>, meaning: &mut Vec<String>) {
    let semantic = match kind {
        // `W` / `P` 是音频与图片，`r` / `R` 是资源清单。释义卡只摆文字，这四类整段丢掉。
        // 其余大写字母只是「同类型 + 4 字节长度前缀」，语义仍按小写走（`M` 就是 `m`）。
        'W' | 'P' | 'r' | 'R' => return,
        other => other.to_ascii_lowercase(),
    };
    let raw = String::from_utf8_lossy(payload);
    let raw = raw.trim_end_matches('\0').trim();
    if raw.is_empty() {
        return;
    }
    match semantic {
        't' => {
            if phonetic.is_none() {
                *phonetic = Some(raw.to_string());
            }
        }
        'm' | 'l' | 'k' | 'y' => meaning.push(raw.to_string()),
        'h' | 'g' | 'x' | 'w' => {
            let plain = plain_text(raw);
            if !plain.is_empty() {
                meaning.push(plain);
            }
        }
        _ => {}
    }
}

/// HTML / Pango / XDXF 一类字段转纯文本。
///
/// 只做阅读器需要的那几件事：`<br>` / `<p>` / `<div>` / `<li>` 转换行，其余标签丢掉，
/// 常见实体与数字实体解码，空白折叠。表格与图片不在这里展开 —— 词典卡是一段可滚动的
/// 文本，把 `<table>` 画成真表格是另一件事。
fn plain_text(raw: &str) -> String {
    let mut stripped = String::with_capacity(raw.len());
    let mut in_tag = false;
    let mut tag = String::new();
    for ch in raw.chars() {
        if in_tag {
            if ch == '>' {
                let name = tag
                    .trim_start_matches('/')
                    .trim()
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if matches!(
                    name.as_str(),
                    "br" | "p"
                        | "div"
                        | "li"
                        | "tr"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "def"
                        | "k"
                ) {
                    stripped.push('\n');
                }
                in_tag = false;
                tag.clear();
            } else {
                tag.push(ch);
            }
        } else if ch == '<' {
            in_tag = true;
            tag.clear();
        } else {
            stripped.push(ch);
        }
    }
    let decoded = decode_entities(&stripped);
    let mut out = String::with_capacity(decoded.len());
    let mut last_was_space = false;
    for ch in decoded.chars() {
        if ch == '\n' {
            while out.ends_with(' ') {
                out.pop();
            }
            if !out.ends_with("\n\n") && !out.is_empty() {
                out.push('\n');
            }
            last_was_space = false;
        } else if ch.is_whitespace() {
            if !last_was_space && !out.ends_with('\n') {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out.trim().to_string()
}

fn decode_entities(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        let tail = &rest[index..];
        let Some(end) = tail.find(';').filter(|end| *end <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let name = &tail[1..end];
        if let Some(ch) = entity_char(name) {
            out.push(ch);
            rest = &tail[end + 1..];
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn entity_char(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            let code =
                if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                    u32::from_str_radix(hex, 16).ok()?
                } else {
                    let dec = name.strip_prefix('#')?;
                    dec.parse().ok()?
                };
            char::from_u32(code)
        }
    }
}

fn dict_error(path: &Path, why: impl Into<String>) -> DictError {
    DictError::Dictionary {
        path: path.to_path_buf(),
        why: why.into(),
        source: None,
    }
}

/// 打开过的词典缓存：查词一次可能跨几本词典，重复打开几十 MB 的文件代价全落在
/// 「换一本书再查同一个词」上。只留最近两本 —— 再多就是拿手机内存换一次冷启动。
#[derive(Default)]
pub struct Cache {
    loaded: VecDeque<(PathBuf, std::sync::Arc<StarDict>)>,
    capacity: usize,
}

impl Cache {
    pub fn new(capacity: usize) -> Self {
        Self {
            loaded: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// 取一本已打开的词典；`None` 时才去读盘。命中会把它挪到队尾。
    pub fn get(&mut self, dir: &Path) -> Result<std::sync::Arc<StarDict>> {
        if let Some(index) = self.loaded.iter().position(|(path, _)| path == dir) {
            let item = self.loaded.remove(index).expect("刚找到的下标");
            let dict = item.1.clone();
            self.loaded.push_back(item);
            return Ok(dict);
        }
        let dict = std::sync::Arc::new(StarDict::open(dir)?);
        while self.loaded.len() >= self.capacity {
            self.loaded.pop_front();
        }
        self.loaded.push_back((dir.to_path_buf(), dict.clone()));
        Ok(dict)
    }

    pub fn clear(&mut self) {
        self.loaded.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::StarDict;
    use super::normalize_query;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write as _;
    use std::path::Path;

    struct Built {
        _dir: tempfile::TempDir,
        path: std::path::PathBuf,
    }

    /// 在临时目录里造一本词典。`entries` 给出（词，释义字节）；释义按 `sametypesequence`
    /// 要求的形状写：`m` 是「释义 + 0」，空 `sametypesequence` 是「m + 释义 + 0」。
    fn build(
        version: &str,
        sametypesequence: &str,
        entries: &[(&str, Vec<u8>)],
        gzip: bool,
    ) -> Built {
        let dir = tempfile::tempdir().expect("临时目录");
        let stem = "test-dict";
        let mut dict = Vec::new();
        let mut idx = Vec::new();
        let mut offsets = Vec::new();
        for (word, data) in entries {
            offsets.push((dict.len() as u64, data.len() as u32));
            dict.extend_from_slice(data);
            idx.extend_from_slice(word.as_bytes());
            idx.push(0);
        }
        // 词表必须按词典自己的顺序；测试数据按调用方给的顺序，先排序再重算偏移。
        let mut rows: Vec<(&str, u64, u32)> = entries
            .iter()
            .zip(offsets)
            .map(|((word, _), (offset, size))| (*word, offset, size))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        idx.clear();
        for (word, offset, size) in &rows {
            idx.extend_from_slice(word.as_bytes());
            idx.push(0);
            if version.starts_with("3.") {
                idx.extend_from_slice(&(*offset).to_be_bytes());
            } else {
                idx.extend_from_slice(&(*offset as u32).to_be_bytes());
            }
            idx.extend_from_slice(&size.to_be_bytes());
        }
        let ifo = format!(
            "StarDict's dict ifo file\nversion={version}\nbookname=Test Dictionary\nwordcount={}\nidxfilesize={}\nsametypesequence={sametypesequence}\n",
            entries.len(),
            idx.len()
        );
        std::fs::write(dir.path().join(format!("{stem}.ifo")), ifo).expect("写 ifo");
        if gzip {
            write_gz(&dir.path().join(format!("{stem}.idx.gz")), &idx);
            write_gz(&dir.path().join(format!("{stem}.dict.dz")), &dict);
        } else {
            std::fs::write(dir.path().join(format!("{stem}.idx")), &idx).expect("写 idx");
            std::fs::write(dir.path().join(format!("{stem}.dict")), &dict).expect("写 dict");
        }
        Built {
            path: dir.path().to_path_buf(),
            _dir: dir,
        }
    }

    fn write_gz(path: &Path, bytes: &[u8]) {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).expect("压");
        std::fs::write(path, encoder.finish().expect("收尾")).expect("写 gzip");
    }

    fn meaning(text: &str) -> Vec<u8> {
        let mut data = text.as_bytes().to_vec();
        data.push(0);
        data
    }

    #[test]
    fn looks_up_case_insensitively() {
        let built = build(
            "2.4.2",
            "m",
            &[("Apple", meaning("苹果")), ("banana", meaning("香蕉"))],
            false,
        );
        let dict = StarDict::open(&built.path).expect("打开");
        let hit = dict.lookup("apple", 4);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].word, "Apple");
        assert_eq!(hit[0].meaning, "苹果");
        assert!(dict.lookup("APPLE", 4).len() == 1);
        assert!(dict.lookup("pear", 4).is_empty());
    }

    #[test]
    fn reads_gzipped_index_and_dictzip() {
        let built = build("2.4.2", "m", &[("apple", meaning("苹果"))], true);
        let dict = StarDict::open(&built.path).expect("打开 gzip 词典");
        assert_eq!(dict.lookup("apple", 1)[0].meaning, "苹果");
    }

    #[test]
    fn metadata_does_not_need_the_dict_file() {
        let built = build("2.4.2", "m", &[("apple", meaning("苹果"))], false);
        let meta = StarDict::metadata(&built.path).expect("读 metadata");
        assert_eq!(meta.bookname, "Test Dictionary");
        assert_eq!(meta.wordcount, 1);
        assert_eq!(meta.offset_bits, 32);
    }

    #[test]
    fn strips_html_entities_and_tags() {
        let built = build(
            "2.4.2",
            "h",
            &[(
                "apple",
                meaning("<b>apple</b><br/>a &amp; b &lt;c&gt; <span>x</span>"),
            )],
            false,
        );
        let dict = StarDict::open(&built.path).expect("打开");
        let text = &dict.lookup("apple", 1)[0].meaning;
        assert!(
            !text.contains("<b>") && !text.contains("<span>"),
            "标签没剥干净：{text}"
        );
        assert!(text.contains("a & b <c>"), "实体没解开：{text}");
    }

    #[test]
    fn parses_records_without_sametypesequence() {
        let mut data = b"m".to_vec();
        data.extend_from_slice("苹果\n".as_bytes());
        data.push(0);
        data.extend_from_slice(b"t");
        data.extend_from_slice("ˈæp.əl".as_bytes());
        data.push(0);
        let built = build("2.4.2", "", &[("apple", data)], false);
        let dict = StarDict::open(&built.path).expect("打开");
        let hit = &dict.lookup("apple", 1)[0];
        assert_eq!(hit.phonetic.as_deref(), Some("ˈæp.əl"));
        assert_eq!(hit.meaning, "苹果");
    }

    #[test]
    fn reads_64_bit_offsets() {
        let built = build("3.0.0", "m", &[("apple", meaning("苹果"))], false);
        let dict = StarDict::open(&built.path).expect("打开 64 位索引");
        assert_eq!(dict.lookup("apple", 1)[0].meaning, "苹果");
    }

    #[test]
    fn rejects_an_index_pointing_past_the_dict() {
        let built = build("2.4.2", "m", &[("apple", meaning("苹果"))], false);
        // 把 .dict 截短，索引还在原处。
        let dict_path = built.path.join("test-dict.dict");
        let bytes = std::fs::read(&dict_path).expect("读");
        std::fs::write(&dict_path, &bytes[..bytes.len() - 1]).expect("截短");
        assert!(StarDict::open(&built.path).is_err());
    }

    #[test]
    fn normalizes_punctuation_and_whitespace() {
        assert_eq!(normalize_query("  「apple」， "), "apple");
        assert_eq!(normalize_query("take   off"), "take off");
        assert_eq!(normalize_query("……"), "");
    }
}
