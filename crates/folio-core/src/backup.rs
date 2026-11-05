//! 备份包：把这一台设备上**不能再生的那一份**装进一个 zip，方便换机或留档。
//!
//! 包里装什么、不装什么是这一层的核心决定：
//!
//! - **库**：`Store::backup_to` 交出来的 `VACUUM INTO` 单文件副本。热备份不能直接拷
//!   `folio.db` —— WAL 里可能还缺最后一截（理由写在 `store::Store::backup_to`）；
//! - **配置**：`config.json` 的用户层原文。**里面有同步密码与 API 密钥**，所以界面
//!   必须把这件事说出来，这一层不替读者做脱敏：脱敏之后换一台机器恢复，账号全丢，
//!   而备份的第一目的恰恰是「换一台机器还能读」；
//! - **书文件与封面**（`include_files` 开着时）：书库根下除 `.staging` / `.thumbs`
//!   之外的文件。前者是没走完的导入，后者是可再生的缓存；
//! - **词典**：配置里 `installed` 指到的那几本 StarDict；
//! - **主题**：`themes/` 下的自定义 JSON。它跟着配置走，很小，缺了恢复后那一格会指向
//!   一个不存在的名字；
//! - **不装**：日志、漫画解压缓存、缩略图、备份目录自己。这些要么可再生，要么会把
//!   上一份备份套进下一份。
//!
//! 包里另有一份 `manifest.json`，写格式版本、生成时间、是否带文件与文件数。列目录时
//! 读它，所以「哪一份是完整包」不用靠文件名猜；读不出来（旧包、坏包）就当没有清单，
//! 仍在列表里摆着 —— 一份坏包不该让整页读不出东西。
//!
//! 打包先写 `<名字>.part` 再改名：中途失败 / 取消时，备份目录里不会出现一个扩展名是
//! `.zip`、中央目录却没写完的半成品。

use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::infra::config::Config;
use crate::infra::error::{Error, IoResultExt as _};
use crate::infra::time::{self, DAY};
use crate::{Result, Store};

/// 这一版备份包的格式。`manifest.json` 里写它，将来恢复流程按它分叉。
pub const FORMAT: u32 = 1;

/// 打包时要用到的几处磁盘位置。**全部由调用方给**：与 `infra::usage::Areas` 同一条
/// 纪律 —— core 不读全局配置，界面把读者确认过的书库位置交进来，测试也能指到临时目录。
#[derive(Clone, Debug)]
pub struct Roots {
    /// `<数据根>/backups`：新包落这儿，列表与删除也只认这里。
    pub backups: PathBuf,
    /// 数据根：只在遍历书库时用来避开 `backups/`、`logs/` 这些自己人。
    pub data: PathBuf,
    /// 读者确认过的书库根；没确认过时 `None`。
    pub library: Option<PathBuf>,
    /// 已导入词典的根；一本都没有时 `None`。
    pub dicts: Option<PathBuf>,
    /// 自定义主题目录。目录不存在时照样可以给，遍历时自然为空。
    pub themes: Option<PathBuf>,
}

impl Roots {
    /// 按 [`crate::infra::paths`] 与读者配置填一份。书库没确认过时那几类文件没有去处，
    /// 与「存储」那一屏的判据同一条。
    pub fn from_config(config: &Config) -> Self {
        let data = crate::infra::paths::data_dir();
        let library = config
            .library
            .is_confirmed()
            .then(|| PathBuf::from(config.library.dir.trim()));
        let dicts = (!config.dictionary.installed.is_empty()).then(|| config.dictionary.root());
        Self {
            backups: crate::infra::paths::backups_dir(),
            data,
            library,
            dicts,
            themes: Some(crate::infra::paths::themes_dir()),
        }
    }
}

/// 打包选项。目前只有一格：要不要把书文件、封面与词典装进去。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub include_files: bool,
}

impl Default for Options {
    fn default() -> Self {
        // 与 `Config::Backup::default` 同一档：默认完整备份。
        Self {
            include_files: true,
        }
    }
}

impl From<crate::Backup> for Options {
    fn from(backup: crate::Backup) -> Self {
        Self {
            include_files: backup.include_files,
        }
    }
}

/// 包里的清单。字段只增不改；将来的恢复流程按 `format` 分叉。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// 生成时刻（Unix 秒）。列表按它倒序。
    pub created_at: i64,
    /// 生成它的 folio 版本。恢复时拿来说「这份包比现在这版旧/新」。
    pub app_version: String,
    /// 书文件、封面与词典在不在包里。
    pub include_files: bool,
    /// 装进去的**文件**数（库与配置不算）。
    pub files: u64,
    /// 打包时书库根在哪里。将来恢复要按它把库里的绝对路径改到新根。
    pub library_dir: Option<String>,
}

/// 一次成功打包的结果。
#[derive(Clone, Debug)]
pub struct Created {
    pub path: PathBuf,
    pub bytes: u64,
    pub manifest: Manifest,
}

/// 备份目录里的一份包。`manifest` 读不出来时是 `None`（旧包 / 坏包），但这一份仍然
/// 列出来：读者看得见它，删得掉它，比让它从列表里消失好。
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    /// 包文件自己的字节数。
    pub bytes: u64,
    /// 文件系统上的修改时刻。没有清单时用它排序与显示。
    pub modified: i64,
    pub manifest: Option<Manifest>,
}

impl Entry {
    /// 列表显示用的时间：清单里的生成时刻，没有就退回文件修改时刻。
    pub fn created_at(&self) -> i64 {
        self.manifest
            .as_ref()
            .map_or(self.modified, |manifest| manifest.created_at)
    }

    /// 这一份是不是完整包。没有清单时保守地说「只有数据」—— 名字里看不出来的事，
    /// 不替它猜。
    pub fn include_files(&self) -> bool {
        self.manifest
            .as_ref()
            .is_some_and(|manifest| manifest.include_files)
    }

    /// 装进去的文件数；没有清单时 0。
    pub fn files(&self) -> u64 {
        self.manifest.as_ref().map_or(0, |manifest| manifest.files)
    }
}

/// 打好的一份包。`progress(done, total)` 每写一个文件报一次；`total == 0` 表示
/// 没有文件可写（只打库与配置）。
pub fn create(
    store: &Store,
    config: &Config,
    roots: &Roots,
    options: Options,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Created> {
    std::fs::create_dir_all(&roots.backups).io_path(&roots.backups)?;

    // 库先落成一份一致的单文件副本。临时名带进程号与时刻：同一秒里两次备份
    // （测试里会这样）也不会互相覆盖。
    let db_temp = roots.backups.join(format!(
        ".folio-db-{}-{}.tmp",
        std::process::id(),
        time::now()
    ));
    let _guard = TempFile::new(db_temp.clone());
    store.backup_to(&db_temp)?;

    let (files, skipped) = collect_files(options.include_files, roots);
    let manifest = Manifest {
        format: FORMAT,
        created_at: time::now(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        include_files: options.include_files,
        files: files.len() as u64,
        library_dir: roots
            .library
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
    };

    let final_path = unique_path(&roots.backups, time::now());
    let part = final_path.with_extension("zip.part");
    // 中途失败 / 取消时把 `.part` 收掉：备份目录里不留半成品。
    let _part_guard = TempFile::new(part.clone());
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&part).io_path(&part)?);

    write_json(&mut writer, "manifest.json", &manifest, &part)?;
    // 包里的设置跟着这一次的选择走：调用方传 `include_files = false` 时，不要在一个
    // 只装数据的包里留一句「下次也带上书」。
    let mut archived = config.clone();
    archived.backup.include_files = options.include_files;
    write_json(&mut writer, "config.json", &archived, &part)?;

    let db_options = file_options(false);
    writer
        .start_file("folio.db", db_options)
        .map_err(|err| zip_error(&part, err))?;
    let mut db = std::fs::File::open(&db_temp).io_path(&db_temp)?;
    std::io::copy(&mut db, &mut writer).io_path(&db_temp)?;

    // 完整包才写文件；只导数据时 `files` 是空表，这一段自然什么也不写。
    let total = files.len() as u64;
    for (index, file) in files.iter().enumerate() {
        writer
            .start_file(&file.name, file_options(!file.stored))
            .map_err(|err| zip_error(&part, err))?;
        let mut source = std::fs::File::open(&file.source).io_path(&file.source)?;
        std::io::copy(&mut source, &mut writer).io_path(&file.source)?;
        progress(index as u64 + 1, total);
    }

    writer.finish().map_err(|err| zip_error(&part, err))?;
    std::fs::rename(&part, &final_path).io_path(&final_path)?;
    let bytes = std::fs::metadata(&final_path).io_path(&final_path)?.len();

    tracing::info!(
        "backup: 生成 {:?}（{} 字节，{} 个文件，跳过 {} 处自己的目录）",
        final_path,
        bytes,
        files.len(),
        skipped
    );
    Ok(Created {
        path: final_path,
        bytes,
        manifest,
    })
}

/// 备份目录现在有哪些包，按生成时刻倒序。目录不存在时回空表 —— 第一次打开这一页
/// 就是这个状态，不该报错。
pub fn list(roots: &Roots) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let dir = match std::fs::read_dir(&roots.backups) {
        Ok(dir) => dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(err) => {
            return Err(Error::Io {
                path: roots.backups.clone(),
                source: Box::new(err),
            });
        }
    };
    for item in dir {
        let item = item.io_path(&roots.backups)?;
        let path = item.path();
        let is_zip = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
        if !is_zip || !item.file_type().io_path(&path)?.is_file() {
            continue;
        }
        // 半成品用 `.zip.part` 收尾，扩展名不是 zip；上面那条已经挡掉了，这里再挡一次
        // 只是为了把意图写在读代码的人眼前。
        if path.to_string_lossy().ends_with(".part") {
            continue;
        }
        let meta = item.metadata().io_path(&path)?;
        let modified = meta
            .modified()
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        entries.push(Entry {
            manifest: read_manifest(&path),
            path,
            bytes: meta.len(),
            modified,
        });
    }
    entries.sort_by(|a, b| {
        b.created_at()
            .cmp(&a.created_at())
            .then_with(|| b.path.cmp(&a.path))
    });
    Ok(entries)
}

/// 删掉一份包。路径必须落在 `roots.backups` 里 —— 界面只会传列表里的那一条，这一道
/// 只是让「以后有人把路径接错」时删不到别处去。
pub fn remove(roots: &Roots, path: &Path) -> Result<()> {
    if path.parent() != Some(roots.backups.as_path()) {
        return Err(Error::Backup {
            path: path.to_path_buf(),
            why: "不在备份目录里".to_string(),
        });
    }
    std::fs::remove_file(path).io_path(path)?;
    tracing::info!("backup: 删掉 {path:?}");
    Ok(())
}

/// 一份要装进包里的文件。`stored` 表示**不压缩**：书与漫画自己已经是压缩格式，
/// 再 deflate 一遍只烧 CPU 不省字节。
struct PackedFile {
    source: PathBuf,
    /// zip 里的名字，永远用 `/` 分隔。
    name: String,
    stored: bool,
}

/// 收集要装的文件。返回 `(文件表, 跳过的目录数)`。
///
/// `skipped` 只进日志：它是「书库指到数据根时没有把自己人的目录再装一遍」的证据，
/// 不出现在界面上 —— 读者不需要知道这个数。
fn collect_files(include_files: bool, roots: &Roots) -> (Vec<PackedFile>, usize) {
    let mut files = Vec::new();
    let mut skipped = 0;
    if include_files {
        if let Some(library) = &roots.library {
            collect_into(library, "library", roots, &mut files, &mut skipped, false);
        }
        if let Some(dicts) = &roots.dicts {
            collect_into(dicts, "dicts", roots, &mut files, &mut skipped, true);
        }
    }
    if let Some(themes) = &roots.themes {
        collect_into(themes, "themes", roots, &mut files, &mut skipped, true);
    }
    (files, skipped)
}

/// 递归收集。符号链接一律不跟（与 `infra::usage` 那条同义：链接指向的东西不归本应用
/// 所有，跟进去还可能绕成环）。
fn collect_into(
    root: &Path,
    prefix: &str,
    roots: &Roots,
    files: &mut Vec<PackedFile>,
    skipped: &mut usize,
    stored: bool,
) {
    let Ok(meta) = std::fs::metadata(root) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            tracing::warn!("backup: 读不动 {dir:?}，这一支跳过");
            *skipped += 1;
            continue;
        };
        let mut items: Vec<_> = read.flatten().collect();
        items.sort_by_key(|item| item.path());
        for item in items {
            let path = item.path();
            let Ok(kind) = item.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if is_own_dir(&path, roots) || is_cache_dir(&path) {
                    *skipped += 1;
                    continue;
                }
                stack.push(path);
                continue;
            }
            if !kind.is_file() || is_own_file(&path, roots) {
                continue;
            }
            let Some(relative) = path.strip_prefix(root).ok() else {
                continue;
            };
            let name = format!("{prefix}/{}", slash_path(relative));
            files.push(PackedFile {
                source: path,
                name,
                stored,
            });
        }
    }
}

/// 书库指到数据根时，这些目录已经各走各的路（或根本不该进包），不能再顺着书库
/// 走一遍。`cache` / `logs` / `inbox` 是可再生的或临时落点。
fn is_own_dir(path: &Path, roots: &Roots) -> bool {
    path == roots.backups
        || path == roots.dicts.as_deref().unwrap_or(Path::new(""))
        || path == roots.themes.as_deref().unwrap_or(Path::new(""))
        || [
            roots.data.join("logs"),
            roots.data.join("cache"),
            roots.data.join("inbox"),
        ]
        .iter()
        .any(|own| path == own)
}

/// 书库根里的两个缓存区：导入没走完的暂存，以及缩略图。
fn is_cache_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == ".staging" || name == ".thumbs")
}

/// 库文件与它那两条附档、`.part` 半成品不装。
fn is_own_file(path: &Path, roots: &Roots) -> bool {
    if path.starts_with(&roots.backups) {
        return true;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if name.ends_with(".part") {
        return true;
    }
    matches!(name, "folio.db" | "folio.db-wal" | "folio.db-shm")
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// 同名包已经存在时加 `-2`、`-3`：一秒里连按两次不该把前一份覆盖掉。
fn unique_path(dir: &Path, at: i64) -> PathBuf {
    let stem = format!("folio-{}", stamp(at));
    let mut candidate = dir.join(format!("{stem}.zip"));
    let mut index = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem}-{index}.zip"));
        index += 1;
    }
    candidate
}

/// Unix 秒 → `YYYYMMDD-HHMMSS`（UTC）。文件名只用它排序，界面上的时间另按本地时区念。
fn stamp(at: i64) -> String {
    let day = at.div_euclid(DAY);
    let secs = at.rem_euclid(DAY);
    let date = time::civil_from_days(day);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        date.year,
        date.month,
        date.day,
        secs / 3_600,
        secs % 3_600 / 60,
        secs % 60
    )
}

fn write_json<W: std::io::Write + std::io::Seek>(
    writer: &mut zip::ZipWriter<W>,
    name: &str,
    value: &impl Serialize,
    path: &Path,
) -> Result<()> {
    writer
        .start_file(name, file_options(true))
        .map_err(|err| zip_error(path, err))?;
    serde_json::to_writer_pretty(writer, value).io_path(path)?;
    Ok(())
}

fn file_options(deflate: bool) -> zip::write::SimpleFileOptions {
    let options = zip::write::SimpleFileOptions::default()
        // 书库可能大过一个 4 GiB 的文件，中央目录也可能是 65535 条之后；让 zip 自己
        // 在需要时切到 ZIP64，别等写到最后才报「值太大」。
        .large_file(true)
        .unix_permissions(0o644);
    if deflate {
        options.compression_method(zip::CompressionMethod::Deflated)
    } else {
        options.compression_method(zip::CompressionMethod::Stored)
    }
}

fn zip_error(path: &Path, err: zip::result::ZipError) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source: Box::new(err),
    }
}

fn read_manifest(path: &Path) -> Option<Manifest> {
    let file = std::fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut entry = archive.by_name("manifest.json").ok()?;
    let mut text = String::new();
    entry.read_to_string(&mut text).ok()?;
    serde_json::from_str(&text).ok()
}

/// 临时文件的清扫。正常路径上 `Drop` 删一次；改名之后文件已经不在了，删失败也无所谓。
struct TempFile(Option<PathBuf>);

impl TempFile {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    //! 这一层能钉住的是**包里的形状**：清单写得对不对、库是不是一份能打开的一致副本、
    //! 完整包与只导数据那两档差在哪、列表读不读得回来、删除会不会越过备份目录。
    //! 「书文件真的能恢复」要等恢复流程落地，现在只验证它被装进去了。

    use super::*;
    use crate::infra::config::Library;

    fn roots(dir: &Path, include_files: bool) -> (Roots, Config) {
        let data = dir.join("data");
        let library = data.join("books");
        let dicts = data.join("dicts");
        let themes = dir.join("config").join("themes");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::create_dir_all(&dicts).unwrap();
        std::fs::create_dir_all(&themes).unwrap();
        let mut config = Config {
            library: Library {
                dir: library.to_string_lossy().into_owned(),
                watch: None,
            },
            ..Default::default()
        };
        config.backup.include_files = include_files;
        config.dictionary.dir = dicts.to_string_lossy().into_owned();
        config.dictionary.installed = vec![crate::DictionaryEntry {
            id: "cedict".into(),
            name: "汉英词典".into(),
            words: 3,
        }];
        let roots = Roots {
            backups: data.join("backups"),
            data,
            library: Some(library),
            dicts: Some(dicts),
            themes: Some(themes),
        };
        (roots, config)
    }

    fn manifest_of(path: &Path) -> Manifest {
        serde_json::from_str(
            &zip::ZipArchive::new(std::fs::File::open(path).unwrap())
                .unwrap()
                .by_name("manifest.json")
                .map(|mut entry| {
                    let mut text = String::new();
                    std::io::Read::read_to_string(&mut entry, &mut text).unwrap();
                    text
                })
                .unwrap(),
        )
        .unwrap()
    }

    fn names(path: &Path) -> Vec<String> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_string())
            .collect()
    }

    #[test]
    fn a_full_backup_carries_database_config_books_dicts_and_themes() {
        let dir = tempfile::tempdir().unwrap();
        let (roots, config) = roots(dir.path(), true);
        std::fs::write(roots.library.as_ref().unwrap().join("书.epub"), b"book").unwrap();
        std::fs::create_dir_all(roots.library.as_ref().unwrap().join(".staging")).unwrap();
        std::fs::write(
            roots.library.as_ref().unwrap().join(".staging/x.part"),
            b"half",
        )
        .unwrap();
        std::fs::create_dir_all(roots.library.as_ref().unwrap().join(".thumbs/400")).unwrap();
        std::fs::write(
            roots.library.as_ref().unwrap().join(".thumbs/400/a"),
            b"thumb",
        )
        .unwrap();
        std::fs::write(roots.dicts.as_ref().unwrap().join("cedict.ifo"), b"ifo").unwrap();
        std::fs::write(roots.themes.as_ref().unwrap().join("mine.json"), b"{}").unwrap();

        let store = Store::open_at(&roots.data.join("folio.db")).unwrap();
        let created = create(
            &store,
            &config,
            &roots,
            Options {
                include_files: true,
            },
            &mut |_, _| {},
        )
        .unwrap();

        let names = names(&created.path);
        for wanted in [
            "manifest.json",
            "config.json",
            "folio.db",
            "library/书.epub",
            "dicts/cedict.ifo",
            "themes/mine.json",
        ] {
            assert!(
                names.iter().any(|name| name == wanted),
                "缺 {wanted}：{names:?}"
            );
        }
        assert!(
            !names.iter().any(|name| name.contains(".staging")),
            "暂存区是半成品，不该进包：{names:?}"
        );
        assert!(
            !names.iter().any(|name| name.contains(".thumbs")),
            "缩略图可再生成，不该进包：{names:?}"
        );
        let manifest = manifest_of(&created.path);
        assert_eq!(manifest.format, FORMAT);
        assert!(manifest.include_files);
        // 书、词典、主题各一本；库与配置不算在 `files` 里。
        assert_eq!(manifest.files, 3, "{manifest:?}");

        // 库是 `VACUUM INTO` 交出来的一致副本：拿它单独开一次也要能开。
        let tmp = dir.path().join("extract");
        std::fs::create_dir_all(&tmp).unwrap();
        let mut archive =
            zip::ZipArchive::new(std::fs::File::open(&created.path).unwrap()).unwrap();
        archive.extract(&tmp).unwrap();
        let restored = Store::open_at(&tmp.join("folio.db")).unwrap();
        restored.list_books().unwrap();
    }

    #[test]
    fn data_only_backup_keeps_the_database_and_settings_but_no_books() {
        let dir = tempfile::tempdir().unwrap();
        let (roots, config) = roots(dir.path(), false);
        std::fs::write(roots.library.as_ref().unwrap().join("书.epub"), b"book").unwrap();
        std::fs::write(roots.themes.as_ref().unwrap().join("mine.json"), b"{}").unwrap();

        let store = Store::open_at(&roots.data.join("folio.db")).unwrap();
        let created = create(
            &store,
            &config,
            &roots,
            Options {
                include_files: false,
            },
            &mut |_, _| {},
        )
        .unwrap();

        let names = names(&created.path);
        assert!(names.iter().any(|name| name == "folio.db"));
        assert!(!names.iter().any(|name| name.starts_with("library/")));
        assert!(!names.iter().any(|name| name.starts_with("dicts/")));
        // 主题跟着配置走：只导数据也带着它。
        assert!(names.iter().any(|name| name == "themes/mine.json"));
        assert_eq!(manifest_of(&created.path).files, 1);
    }

    #[test]
    fn listing_reads_the_manifest_and_orders_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let (roots, config) = roots(dir.path(), true);
        let store = Store::open_at(&roots.data.join("folio.db")).unwrap();

        let first = create(&store, &config, &roots, Options::default(), &mut |_, _| {}).unwrap();
        // 同一秒里再打一份：文件名要加 `-2`，两份都在列表里。
        let second = create(&store, &config, &roots, Options::default(), &mut |_, _| {}).unwrap();
        assert_ne!(first.path, second.path, "同秒的两份包不能互相覆盖");

        let entries = list(&roots).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.manifest.is_some()));
        assert!(entries.iter().all(|entry| entry.include_files()));
        assert!(entries[0].created_at() >= entries[1].created_at());
    }

    #[test]
    fn remove_refuses_a_path_outside_the_backup_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (roots, _config) = roots(dir.path(), true);
        let outside = dir.path().join("外面.zip");
        std::fs::write(&outside, b"not a backup").unwrap();
        let err = remove(&roots, &outside).unwrap_err();
        assert!(matches!(err, Error::Backup { .. }), "{err:?}");
        assert!(outside.exists(), "备份目录以外的文件一个字节都不该动");
    }
}
