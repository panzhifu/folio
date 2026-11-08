//! 词典库：导入、删除、查词的纯文件活。
//!
//! 这一层不认「界面」：给目录就搬文件，给源列表就查词，返回的都是纯数据。
//! 列表的真相源（配置里 `Config::dictionary.installed`）仍由 app 持有 —— core 只管
//! 盘上那一半：导入是原子拷贝（先落 `.import-<id>` 再改名，失败整趟撤掉），删除连
//! 目录带缓存一起清。`lookup_in` 是纯函数式入口：调用方给源列表，它给结果，core 的
//! 解析测试与 app 的导入测试由此可以分开写。
//!
//! 词典缓存（[`Cache`]）是**进程级**的：查词一次可能跨几本词典，重复打开几十 MB
//! 的文件代价全落在「换一本书再查同一个词」上。全仓只此一份，删除词典后由 app 调
//! [`clear_cache`] 失效掉。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::dict::{Cache, StarDict};
use crate::infra::error::{Error, Result};

/// 导入完成后交回调用方的一条书目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedDict {
    pub id: String,
    pub name: String,
    pub words: u64,
}

/// 一本可以查的词典：配置里那一条 + 它在盘上的目录。
#[derive(Debug, Clone)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
}

/// 一条查词结果。`source` 是词典书名，弹层里标出来自哪一本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub source: String,
    pub word: String,
    pub phonetic: Option<String>,
    pub meaning: String,
}

/// 查词。`sources` 由调用方按当时的配置拼好，后台那趟不再读全局配置。
pub fn lookup_in(sources: &[Source], query: &str) -> Result<Vec<Hit>> {
    let query = crate::dict::normalize_query(query);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut cache = cache().lock().unwrap_or_else(|poison| poison.into_inner());
    let mut hits = Vec::new();
    let mut first_error: Option<Error> = None;
    let mut loaded = 0usize;
    for source in sources {
        let dict = match cache.get(&source.dir) {
            Ok(dict) => dict,
            Err(err) => {
                tracing::warn!(dict = %source.name, error = %err, "词典: 打不开，跳过");
                first_error.get_or_insert(err);
                continue;
            }
        };
        loaded += 1;
        for entry in dict.lookup(&query, 3) {
            hits.push(Hit {
                source: source.name.clone(),
                word: entry.word,
                phonetic: entry.phonetic,
                meaning: entry.meaning,
            });
            if hits.len() >= 6 {
                break;
            }
        }
        if hits.len() >= 6 {
            break;
        }
    }
    if hits.is_empty()
        && loaded == 0
        && let Some(err) = first_error
    {
        return Err(err);
    }
    Ok(hits)
}

/// 进程级的词典缓存：只留最近两本，别再拿手机内存换冷启动。
pub fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::new(2)))
}

/// 把进程级词典缓存整个清掉。删除词典后必须调：删掉的那一本可能下一秒又以同一个
/// id 重导（后缀会避开，但手工改配置能撞上），缓存里按路径留着的旧内容不能顶上来。
pub fn clear_cache() {
    cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clear();
}

/// 把一个文件夹里的 StarDict 词典复制到 `root` 下。
///
/// `taken` 是配置里已经占用的 id；同一批里新导入的 id 也会占住，重复导入会得到
/// `-2` / `-3` 这样的后缀，而不是覆盖上一本。
pub fn import_folder(
    src: &Path,
    root: &Path,
    taken: &BTreeSet<String>,
) -> Result<Vec<ImportedDict>> {
    let ifos = find_ifos(src)?;
    if ifos.is_empty() {
        return Err(dict_error(src, "目录里没有 .ifo"));
    }
    std::fs::create_dir_all(root).map_err(|err| io_error(root, err))?;
    let mut taken = taken.clone();
    let mut imported = Vec::new();
    let outcome: Result<()> = (|| {
        for ifo in ifos {
            let meta = StarDict::metadata_of(&ifo)?;
            let stem = ifo
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| dict_error(&ifo, "文件名不是合法 UTF-8"))?
                .to_string();
            let base = safe_id(&stem);
            let id = unique_id(&base, &mut taken, root);
            let tmp = root.join(format!(".import-{id}"));
            if tmp.exists() {
                std::fs::remove_dir_all(&tmp).map_err(|err| io_error(&tmp, err))?;
            }
            std::fs::create_dir_all(&tmp).map_err(|err| io_error(&tmp, err))?;
            if let Err(err) = copy_dictionary(&ifo, &stem, &tmp) {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(err);
            }
            let target = root.join(&id);
            std::fs::rename(&tmp, &target).map_err(|err| {
                let _ = std::fs::remove_dir_all(&tmp);
                io_error(&target, err)
            })?;
            imported.push(ImportedDict {
                id,
                name: meta.bookname,
                words: meta.wordcount,
            });
        }
        Ok(())
    })();
    // 一本失败就把这一趟已经落下的几本一起撤掉：配置会在失败时不写，留下的只是
    // 没人认领的目录。下次导入同一个文件夹时会撞上 -2 后缀，读者看到重复项。
    if let Err(err) = outcome {
        for one in &imported {
            let _ = std::fs::remove_dir_all(root.join(&one.id));
        }
        return Err(err);
    }
    Ok(imported)
}

/// 删掉 `<root>/<id>`。目录已经不在算成功 —— 配置里那一条也该跟着摘掉。
pub fn remove_folder(root: &Path, id: &str) -> Result<()> {
    if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\']) {
        return Err(dict_error(root, format!("不合法的词典 id：{id:?}")));
    }
    let dir = root.join(id);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io_error(&dir, err)),
    }
}

fn copy_dictionary(ifo: &Path, stem: &str, tmp: &Path) -> Result<()> {
    let src = ifo
        .parent()
        .ok_or_else(|| dict_error(ifo, "这个 .ifo 没有所在目录"))?;
    std::fs::copy(
        ifo,
        tmp.join(ifo.file_name().expect("已经找到的文件有名字")),
    )
    .map_err(|err| io_error(ifo, err))?;
    for suffix in ["idx", "idx.gz", "dict", "dict.dz", "syn"] {
        let from = src.join(format!("{stem}.{suffix}"));
        if from.is_file() {
            let to = tmp.join(format!("{stem}.{suffix}"));
            std::fs::copy(&from, &to).map_err(|err| io_error(&from, err))?;
        }
    }
    Ok(())
}

fn find_ifos(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir).map_err(|err| io_error(dir, err))?;
    let mut ifos = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| io_error(dir, err))?;
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
    Ok(ifos)
}

fn safe_id(stem: &str) -> String {
    let mut out = String::new();
    for ch in stem.chars() {
        if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
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
        "dict".to_string()
    } else {
        out
    }
}

fn unique_id(base: &str, taken: &mut BTreeSet<String>, root: &Path) -> String {
    let mut candidate = base.to_string();
    let mut n = 2;
    while taken.contains(&candidate) || root.join(&candidate).exists() {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    taken.insert(candidate.clone());
    candidate
}

fn io_error(path: &Path, err: std::io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source: Box::new(err),
    }
}

fn dict_error(path: &Path, why: impl Into<String>) -> Error {
    Error::Dictionary {
        path: path.to_path_buf(),
        why: why.into(),
        source: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Source, import_folder, lookup_in, remove_folder};
    use std::collections::BTreeSet;
    use std::io::Write as _;
    use std::path::Path;

    /// 造一本最小 StarDict：`.ifo` + `.idx` + `.dict`，`sametypesequence=m`。
    fn write_dict(dir: &Path, words: &[(&str, &str)]) {
        let stem = "mini";
        let mut dict = Vec::new();
        let mut offsets = Vec::new();
        for (_, meaning) in words {
            offsets.push((dict.len() as u32, meaning.len() as u32 + 1));
            dict.extend_from_slice(meaning.as_bytes());
            dict.push(0);
        }
        let mut rows: Vec<(&str, u32, u32)> = words
            .iter()
            .zip(offsets)
            .map(|((word, _), (offset, size))| (*word, offset, size))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(b.0));
        let mut idx = Vec::new();
        for (word, offset, size) in &rows {
            idx.extend_from_slice(word.as_bytes());
            idx.push(0);
            idx.extend_from_slice(&offset.to_be_bytes());
            idx.extend_from_slice(&size.to_be_bytes());
        }
        let ifo = format!(
            "StarDict's dict ifo file\nversion=2.4.2\nbookname=Mini Dict\nwordcount={}\nidxfilesize={}\nsametypesequence=m\n",
            words.len(),
            idx.len()
        );
        std::fs::write(dir.join(format!("{stem}.ifo")), ifo).expect("写 ifo");
        std::fs::write(dir.join(format!("{stem}.idx")), idx).expect("写 idx");
        std::fs::write(dir.join(format!("{stem}.dict")), dict).expect("写 dict");
    }

    #[test]
    fn imports_and_looks_up_a_folder() {
        let src = tempfile::tempdir().expect("源目录");
        write_dict(src.path(), &[("apple", "苹果"), ("banana", "香蕉")]);
        let root = tempfile::tempdir().expect("词典根");

        let imported = import_folder(src.path(), root.path(), &BTreeSet::new()).expect("导入");
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].name, "Mini Dict");
        assert_eq!(imported[0].words, 2);

        let source = Source {
            id: imported[0].id.clone(),
            name: imported[0].name.clone(),
            dir: root.path().join(&imported[0].id),
        };
        let hits = lookup_in(&[source], "apple").expect("查词");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].word, "apple");
        assert_eq!(hits[0].meaning, "苹果");
        assert_eq!(hits[0].source, "Mini Dict");
    }

    #[test]
    fn importing_the_same_folder_twice_gets_a_suffix() {
        let src = tempfile::tempdir().expect("源目录");
        write_dict(src.path(), &[("apple", "苹果")]);
        let root = tempfile::tempdir().expect("词典根");
        let first = import_folder(src.path(), root.path(), &BTreeSet::new()).expect("第一次");
        let taken: BTreeSet<String> = first.iter().map(|one| one.id.clone()).collect();
        let second = import_folder(src.path(), root.path(), &taken).expect("第二次");
        assert_ne!(first[0].id, second[0].id, "重名该加后缀");
        assert!(second[0].id.starts_with(&first[0].id));
        assert!(root.path().join(&second[0].id).is_dir());
    }

    #[test]
    fn removing_a_folder_takes_it_off_disk() {
        let src = tempfile::tempdir().expect("源目录");
        write_dict(src.path(), &[("apple", "苹果")]);
        let root = tempfile::tempdir().expect("词典根");
        let imported = import_folder(src.path(), root.path(), &BTreeSet::new()).expect("导入");
        remove_folder(root.path(), &imported[0].id).expect("删除");
        assert!(!root.path().join(&imported[0].id).exists());
        // 再删一次不报错：配置摘得掉才是目的。
        remove_folder(root.path(), &imported[0].id).expect("重复删除");
    }

    #[test]
    fn a_folder_without_ifo_is_rejected() {
        let src = tempfile::tempdir().expect("源目录");
        std::fs::File::create(src.path().join("readme.txt"))
            .expect("写")
            .write_all(b"no dictionary here")
            .expect("写");
        let root = tempfile::tempdir().expect("词典根");
        assert!(import_folder(src.path(), root.path(), &BTreeSet::new()).is_err());
    }
}
