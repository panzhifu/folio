//! 本地 StarDict 词典服务：导入、删除、查词。
//!
//! **列表的真相源是配置**（`Config::dictionary.installed`），不是磁盘扫描：导入完成时
//! 写一条，删除时摘一条。这样设置页和阅读器的动作条读的是同一份数，测试也不会因为
//! 开发机上恰好放了几本词典而多出一行。
//!
//! 文件活（复制 / 删除）走常驻作业池的 `Cleanup` 档，不在界面线程上搬几十 MB；
//! 查词走 `Interactive` 档，结果原样交给会话。`lookup_in` 是纯函数式的入口：调用方
//! 给源列表，它给结果，这样 core 的解析测试与 app 的导入测试可以分开写。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use folio_core::dict::{self, Cache, StarDict};
use folio_core::{DictionaryEntry, Priority};
use gpui_kit::{App, AppContext as _, Context, Entity, Global};

use crate::ui::services::config::ConfigHolder;
use crate::ui::services::library::{self, Pick};
use crate::ui::services::resident;

/// 导入完成后交回界面的一条书目。
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

/// 导入 / 删除那一趟的状态。设置页 observe 它。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Importing,
    ImportFailed(String),
    DeleteFailed(String),
}

pub struct DictionaryState {
    pub status: Status,
}

pub struct DictionaryHolder(pub Entity<DictionaryState>);

impl Global for DictionaryHolder {}

/// 取状态实体，没有就建。与 `library::access` 同一形状：设置页在构造时手里就是
/// `&mut App`，不必等外壳先装。
pub fn entity(cx: &mut App) -> Entity<DictionaryState> {
    if let Some(holder) = cx.try_global::<DictionaryHolder>() {
        return holder.0.clone();
    }
    let entity = cx.new(|_| DictionaryState {
        status: Status::default(),
    });
    cx.set_global(DictionaryHolder(entity.clone()));
    entity
}

/// 当前状态；没装实体时是 `Idle`（纯视图测试不建作业）。
pub fn status(cx: &App) -> Status {
    cx.try_global::<DictionaryHolder>()
        .map(|holder| holder.0.read(cx).status.clone())
        .unwrap_or_default()
}

/// 已导入的词典，按导入顺序。
pub fn entries(cx: &App) -> Vec<DictionaryEntry> {
    ConfigHolder::dictionary(cx).installed
}

/// 这一台机器现在有没有能查的词典。动作条每帧问一次，只读配置。
pub fn available(cx: &App) -> bool {
    let config = ConfigHolder::dictionary(cx);
    config
        .installed
        .iter()
        .any(|entry| config.is_enabled(&entry.id))
}

/// 拼出这一趟要查的目录列表。
pub fn enabled_sources(cx: &App) -> Vec<Source> {
    let config = ConfigHolder::dictionary(cx);
    let root = config.root();
    config
        .installed
        .iter()
        .filter(|entry| config.is_enabled(&entry.id))
        .map(|entry| Source {
            id: entry.id.clone(),
            name: entry.name.clone(),
            dir: root.join(&entry.id),
        })
        .collect()
}

/// 开系统文件夹选择器，挑中了就导入。
///
/// Android 上「没给所有文件访问」那一问由 `choose_dir` 统一摆；设置页按
/// [`Pick::Dictionary`] 认出自己，并把 `access_question` 摆回那一格。
pub fn import_from_picker(cx: &mut App) {
    library::choose_dir(cx, Pick::Dictionary, |cx, dir| {
        import(cx, dir.to_path_buf());
    })
    .detach();
}

/// 导入一个文件夹（里面可以有一本或多本 StarDict 词典）。
pub fn import(cx: &mut App, src: PathBuf) {
    let config = ConfigHolder::dictionary(cx);
    let root = config.root();
    let taken: BTreeSet<String> = config
        .installed
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    let state = entity(cx);
    state.update(cx, |state, cx| {
        state.status = Status::Importing;
        cx.notify();
        let (tx, rx) = async_channel::bounded(1);
        resident::pool().submit_with(Priority::Cleanup, move |_| {
            let result = import_folder(&src, &root, &taken).map(|imported| (root, imported));
            let _ = tx.send_blocking(result);
            Ok(())
        });
        cx.spawn(async move |this, cx| {
            let Ok(result) = rx.recv().await else {
                return;
            };
            let _ = this.update(cx, |state, cx| state.finish_import(result, cx));
        })
        .detach();
    });
}

/// 删掉一本（磁盘目录与配置里的那一条一起）。
pub fn remove(cx: &mut App, id: String) {
    let config = ConfigHolder::dictionary(cx);
    let root = config.root();
    let state = entity(cx);
    state.update(cx, |state, cx| {
        state.status = Status::Importing;
        cx.notify();
        let (tx, rx) = async_channel::bounded(1);
        let job_id = id.clone();
        resident::pool().submit_with(Priority::Cleanup, move |_| {
            let result = remove_folder(&root, &job_id);
            let _ = tx.send_blocking(result);
            Ok(())
        });
        cx.spawn(async move |this, cx| {
            let Ok(result) = rx.recv().await else {
                return;
            };
            let _ = this.update(cx, |state, cx| state.finish_remove(id, result, cx));
        })
        .detach();
    });
}

/// 打开 / 关掉一本。写配置，不动磁盘。
pub fn set_enabled(cx: &mut App, id: &str, enabled: bool) {
    let id = id.to_string();
    ConfigHolder::edit_dictionary(cx, |dictionary| {
        dictionary.disabled.retain(|disabled| disabled != &id);
        if !enabled {
            dictionary.disabled.push(id.clone());
        }
    });
    if let Some(holder) = cx.try_global::<DictionaryHolder>() {
        let state = holder.0.clone();
        state.update(cx, |_, cx| cx.notify());
    }
}

impl DictionaryState {
    fn finish_import(
        &mut self,
        result: folio_core::Result<(PathBuf, Vec<ImportedDict>)>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok((root, imported)) => {
                if imported.is_empty() {
                    self.status = Status::ImportFailed("这个文件夹里没有 .ifo".to_string());
                } else {
                    ConfigHolder::edit_dictionary(cx, |dictionary| {
                        if dictionary.dir.is_empty() {
                            dictionary.dir = root.display().to_string();
                        }
                        dictionary.installed.extend(imported.into_iter().map(|one| {
                            DictionaryEntry {
                                id: one.id,
                                name: one.name,
                                words: one.words,
                            }
                        }));
                    });
                    self.status = Status::Idle;
                }
            }
            Err(err) => self.status = Status::ImportFailed(err.to_string()),
        }
        cx.notify();
    }

    fn finish_remove(
        &mut self,
        id: String,
        result: folio_core::Result<()>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(()) => {
                ConfigHolder::edit_dictionary(cx, |dictionary| {
                    dictionary.installed.retain(|entry| entry.id != id);
                    dictionary.disabled.retain(|disabled| disabled != &id);
                });
                // 删掉的那一本可能下一秒又以同一个 id 重导（后缀会避开，但手工改配置能撞上）：
                // 缓存里按路径留着的旧内容不能顶上来。
                cache()
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .clear();
                self.status = Status::Idle;
            }
            Err(err) => self.status = Status::DeleteFailed(err.to_string()),
        }
        cx.notify();
    }
}

/// 查词。`sources` 由调用方按当时的配置拼好，后台那趟不再读全局配置。
pub fn lookup_in(sources: &[Source], query: &str) -> folio_core::Result<Vec<Hit>> {
    let query = dict::normalize_query(query);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut cache = cache().lock().unwrap_or_else(|poison| poison.into_inner());
    let mut hits = Vec::new();
    let mut first_error: Option<folio_core::Error> = None;
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
fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::new(2)))
}

/// 把一个文件夹里的 StarDict 词典复制到 `root` 下。
///
/// `taken` 是配置里已经占用的 id；同一批里新导入的 id 也会占住，重复导入会得到
/// `-2` / `-3` 这样的后缀，而不是覆盖上一本。
pub fn import_folder(
    src: &Path,
    root: &Path,
    taken: &BTreeSet<String>,
) -> folio_core::Result<Vec<ImportedDict>> {
    let ifos = find_ifos(src)?;
    if ifos.is_empty() {
        return Err(dict_error(src, "目录里没有 .ifo"));
    }
    std::fs::create_dir_all(root).map_err(|err| io_error(root, err))?;
    let mut taken = taken.clone();
    let mut imported = Vec::new();
    let outcome: folio_core::Result<()> = (|| {
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
pub fn remove_folder(root: &Path, id: &str) -> folio_core::Result<()> {
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

fn copy_dictionary(ifo: &Path, stem: &str, tmp: &Path) -> folio_core::Result<()> {
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

fn find_ifos(dir: &Path) -> folio_core::Result<Vec<PathBuf>> {
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

fn io_error(path: &Path, err: std::io::Error) -> folio_core::Error {
    folio_core::Error::Io {
        path: path.to_path_buf(),
        source: Box::new(err),
    }
}

fn dict_error(path: &Path, why: impl Into<String>) -> folio_core::Error {
    folio_core::Error::Dictionary {
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
