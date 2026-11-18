use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backup::{FORMAT, Manifest};
use crate::error::{Error, IoResultExt as _};
use crate::{Config, Result, Store};

const JOURNAL: &str = ".folio-restore.json";
const LOCK: &str = ".folio-restore.lock";

#[derive(Clone, Copy, Debug)]
pub struct RestoreLimits {
    pub files: usize,
    pub total_bytes: u64,
    pub entry_bytes: u64,
}

impl Default for RestoreLimits {
    fn default() -> Self {
        Self {
            files: 100_000,
            total_bytes: 32 << 30,
            entry_bytes: 8 << 30,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RestorePreview {
    pub manifest: Manifest,
    pub unpacked_bytes: u64,
    pub contains_credentials: bool,
    pub credentials: Vec<&'static str>,
}

#[derive(Clone, Debug)]
pub struct Restored {
    pub root: PathBuf,
    pub database: PathBuf,
    pub config: PathBuf,
    pub library: PathBuf,
    pub dicts: PathBuf,
    pub fonts: PathBuf,
    pub themes: PathBuf,
    pub books: usize,
    pub include_files: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    archive_hash: String,
    ready: bool,
    books: usize,
    include_files: bool,
    #[serde(default = "keep_credentials")]
    include_credentials: bool,
}

fn keep_credentials() -> bool {
    true
}

fn invalid(path: &Path, why: &str) -> Error {
    Error::Backup {
        path: path.to_path_buf(),
        why: why.into(),
    }
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', ':', '\0'])
        && name.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && !part.chars().any(char::is_control)
        })
}

fn allowed_name(name: &str) -> bool {
    safe_name(name)
        && (matches!(name, "manifest.json" | "config.json" | "folio.db")
            || ["library/", "dicts/", "themes/", "fonts/"]
                .iter()
                .any(|prefix| name.starts_with(prefix)))
}

fn inspect(file: &File, path: &Path, limits: RestoreLimits) -> Result<RestorePreview> {
    let mut archive = zip::ZipArchive::new(file.try_clone().io_path(path)?)
        .map_err(|e| invalid(path, &e.to_string()))?;
    if archive.len() > limits.files {
        return Err(invalid(path, "备份文件数量超限"));
    }
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| invalid(path, &e.to_string()))?;
        if entry.is_dir() {
            return Err(invalid(path, "备份不接受显式目录条目"));
        }
        if !allowed_name(entry.name()) || !names.insert(entry.name().to_lowercase()) {
            return Err(invalid(path, "备份条目路径非法或大小写重名"));
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 != 0 && mode & 0o170000 != 0o100000)
        {
            return Err(invalid(path, "备份含链接或特殊文件"));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| invalid(path, "备份大小溢出"))?;
        if entry.size() > limits.entry_bytes || total > limits.total_bytes {
            return Err(invalid(path, "备份解压大小超限"));
        }
    }
    for required in ["manifest.json", "config.json", "folio.db"] {
        if !names.contains(required) {
            return Err(invalid(path, "备份缺少清单、配置或数据库"));
        }
    }
    let manifest: Manifest = read_json(&mut archive, "manifest.json", 64 * 1024, path)?;
    if manifest.format != FORMAT {
        return Err(invalid(path, "不支持的备份格式"));
    }
    if manifest.files != archive.len().saturating_sub(3) as u64 {
        return Err(invalid(path, "备份文件数与清单不一致"));
    }
    if !manifest.include_files
        && names
            .iter()
            .any(|name| name.starts_with("library/") || name.starts_with("dicts/"))
    {
        return Err(invalid(path, "数据备份含未声明的书文件"));
    }
    let config: Config = read_json(&mut archive, "config.json", 4 << 20, path)?;
    if config.version > crate::CONFIG_VERSION {
        return Err(invalid(path, "备份配置来自更新版本"));
    }
    let credentials = credential_fields(&config);
    Ok(RestorePreview {
        manifest,
        unpacked_bytes: total,
        contains_credentials: !credentials.is_empty(),
        credentials,
    })
}

fn credential_fields(config: &Config) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if !config.sync.password.is_empty() {
        fields.push("sync.password");
    }
    if !config.account.key.is_empty() {
        fields.push("account.key");
    }
    if !config.translation.openai.api_key.is_empty() {
        fields.push("translation.openai.api_key");
    }
    if !config.translation.deepl.api_key.is_empty() {
        fields.push("translation.deepl.api_key");
    }
    if config.vendors.iter().any(|v| !v.api_key.is_empty()) {
        fields.push("vendors.api_key");
    }
    if !config.cloud.webdav.password.is_empty() {
        fields.push("cloud.webdav.password");
    }
    if !config.cloud.baidu.access_token.is_empty() || !config.cloud.baidu.refresh_token.is_empty() {
        fields.push("cloud.baidu.token");
    }
    if !config.cloud.aliyun.access_token.is_empty() || !config.cloud.aliyun.refresh_token.is_empty()
    {
        fields.push("cloud.aliyun.token");
    }
    if config
        .opds
        .catalogs
        .iter()
        .any(|catalog| !catalog.password.is_empty())
    {
        fields.push("opds.catalogs.password");
    }
    fields
}

fn read_json<T: serde::de::DeserializeOwned>(
    archive: &mut zip::ZipArchive<File>,
    name: &str,
    max: u64,
    path: &Path,
) -> Result<T> {
    let mut entry = archive
        .by_name(name)
        .map_err(|e| invalid(path, &e.to_string()))?;
    if entry.size() > max {
        return Err(invalid(path, "备份元数据超限"));
    }
    let mut bytes = Vec::new();
    (&mut entry)
        .take(max + 1)
        .read_to_end(&mut bytes)
        .io_path(path)?;
    if bytes.len() as u64 > max {
        return Err(invalid(path, "备份元数据超限"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn preview_restore(archive: &Path, limits: RestoreLimits) -> Result<RestorePreview> {
    let file = File::open(archive).io_path(archive)?;
    inspect(&file, archive, limits)
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

pub(super) fn checked_directory(path: &Path) -> Result<()> {
    for part in path.ancestors() {
        let metadata = std::fs::symlink_metadata(part).io_path(part)?;
        if linked(&metadata) || !metadata.is_dir() {
            return Err(invalid(path, "恢复目录包含链接或不是目录"));
        }
    }
    Ok(())
}

fn create_parents(root: &Path, relative: &Path) -> Result<()> {
    let mut path = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(invalid(relative, "恢复路径不能包含父目录或绝对路径"));
        }
        path.push(component);
        match std::fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(invalid(&path, &error.to_string())),
        }
        let metadata = std::fs::symlink_metadata(&path).io_path(&path)?;
        if linked(&metadata) || !metadata.is_dir() {
            return Err(invalid(&path, "恢复目录不能包含链接或文件"));
        }
    }
    Ok(())
}

fn private_file(path: &Path, truncate: bool) -> Result<File> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && (linked(&metadata) || !metadata.is_file())
    {
        return Err(invalid(path, "恢复文件不能是链接或目录"));
    }
    let mut options = OpenOptions::new();
    options
        .write(true)
        .read(true)
        .create(true)
        .truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).io_path(path)
}

fn write_journal(root: &Path, value: &Journal) -> Result<()> {
    let path = root.join(JOURNAL);
    let part = root.join(".folio-restore.json.part");
    let mut file = private_file(&part, true)?;
    file.write_all(&serde_json::to_vec(value)?).io_path(&part)?;
    file.sync_all().io_path(&part)?;
    drop(file);
    std::fs::rename(&part, &path).io_path(&path)?;
    Ok(())
}

fn restored(root: PathBuf, journal: &Journal) -> Restored {
    Restored {
        database: root.join("folio.db"),
        config: root.join("preferences/config.json"),
        library: root.join("library"),
        dicts: root.join("dicts"),
        fonts: root.join("fonts"),
        themes: root.join("preferences/themes"),
        root,
        books: journal.books,
        include_files: journal.include_files,
    }
}

/// 恢复到新目录；不会替换活动书库。ready 标记落盘后才可切换启动根。
/// 同包重试复用未完成目录；已完成重试只返回，不覆盖恢复后产生的新数据。
pub fn restore_into(
    archive_path: &Path,
    destination: &Path,
    limits: RestoreLimits,
) -> Result<Restored> {
    restore_with_credentials(archive_path, destination, limits, true)
}

/// `include_credentials` 为 false 时只清除恢复副本的凭据；源包与活动配置保持原样。
pub fn restore_with_credentials(
    archive_path: &Path,
    destination: &Path,
    limits: RestoreLimits,
    include_credentials: bool,
) -> Result<Restored> {
    let file = File::open(archive_path).io_path(archive_path)?;
    let preview = inspect(&file, archive_path, limits)?;
    let mut hash_reader = file.try_clone().io_path(archive_path)?;
    use std::io::{Seek, SeekFrom};
    hash_reader.seek(SeekFrom::Start(0)).io_path(archive_path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = hash_reader.read(&mut buffer).io_path(archive_path)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let archive_hash: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let parent = destination
        .parent()
        .ok_or_else(|| invalid(destination, "恢复目录没有父目录"))?
        .canonicalize()
        .io_path(destination)?;
    checked_directory(&parent)?;
    let name = destination
        .file_name()
        .ok_or_else(|| invalid(destination, "缺少恢复目录名"))?;
    let root = parent.join(name);
    let fresh = match std::fs::create_dir(&root) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(invalid(&root, &e.to_string())),
    };
    checked_directory(&root)?;
    #[cfg(unix)]
    if fresh {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).io_path(&root)?;
    }
    if !fresh && !root.join(JOURNAL).is_file() {
        return Err(invalid(&root, "目标目录已存在且不属于本次恢复"));
    }
    let lock_path = root.join(LOCK);
    // OS 锁随句柄/进程释放，崩溃遗留的锁文件不阻止下一次恢复。
    let _lock = private_file(&lock_path, false)?;
    _lock
        .try_lock()
        .map_err(|_| invalid(&root, "恢复目录正被其它操作占用或无法加锁"))?;
    let mut journal = if fresh {
        Journal {
            version: 1,
            archive_hash,
            ready: false,
            books: 0,
            include_files: preview.manifest.include_files,
            include_credentials,
        }
    } else {
        let journal_path = root.join(JOURNAL);
        if linked(&std::fs::symlink_metadata(&journal_path).io_path(&journal_path)?) {
            return Err(invalid(&root, "恢复标记不能是链接"));
        }
        let bytes = std::fs::read(&journal_path).io_path(&journal_path)?;
        if bytes.len() > 16 * 1024 {
            return Err(invalid(&root, "恢复标记过大"));
        }
        let journal: Journal =
            serde_json::from_slice(&bytes).map_err(|e| invalid(&journal_path, &e.to_string()))?;
        if journal.version != 1
            || journal.archive_hash != archive_hash
            || journal.include_credentials != include_credentials
        {
            return Err(invalid(&root, "恢复标记与备份不匹配"));
        }
        if journal.ready {
            return Ok(restored(root, &journal));
        }
        journal
    };
    write_journal(&root, &journal)?;
    for name in ["folio.db-wal", "folio.db-shm"] {
        let path = root.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if !linked(&metadata) && metadata.is_file() => {
                std::fs::remove_file(&path).io_path(&path)?
            }
            Ok(_) => return Err(invalid(&path, "恢复副本附档非法")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(invalid(&path, &error.to_string())),
        }
    }
    extract_into(file, &root, archive_path, limits)?;
    Store::inspect_backup(&root.join("folio.db"))?;
    let store = Store::open_at(&root.join("folio.db"))?;
    let books = store.list_books()?;
    if !books.is_empty() {
        let old = preview
            .manifest
            .library_dir
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid(archive_path, "有书目但缺少原书库根"))?;
        journal.books = store.relocate_backup_library(old, &root.join("library"))?;
    }
    if preview.manifest.include_files && !store.unavailable_sources()?.is_empty() {
        return Err(invalid(archive_path, "完整备份缺少可读书文件"));
    }
    store.checkpoint_restored()?;
    drop(store);
    Store::inspect_backup(&root.join("folio.db"))?;
    let config_path = root.join("preferences/config.json");
    let mut config: Config =
        serde_json::from_slice(&std::fs::read(&config_path).io_path(&config_path)?)?;
    config.library.dir = root.join("library").to_string_lossy().into_owned();
    config.library.watch = None;
    config.dictionary.dir = root.join("dicts").to_string_lossy().into_owned();
    config.sync.enabled = false;
    config.sync.device_id = crate::sync::new_device_id();
    config.account.enabled = false;
    config.logging.dir = String::new();
    if !include_credentials {
        clear_credentials(&mut config);
    }
    // Restored credentials stay in the private config; only network actions are disabled by default.
    let config_json = serde_json::to_string_pretty(&config)?;
    let config_part = root.join("preferences/config.json.part");
    let mut config_file = private_file(&config_part, true)?;
    config_file
        .write_all(config_json.as_bytes())
        .io_path(&config_part)?;
    config_file.sync_all().io_path(&config_part)?;
    drop(config_file);
    std::fs::rename(&config_part, &config_path).io_path(&config_path)?;
    journal.ready = true;
    write_journal(&root, &journal)?;
    Ok(restored(root, &journal))
}

pub(super) fn extract_into(
    file: File,
    root: &Path,
    archive_path: &Path,
    limits: RestoreLimits,
) -> Result<()> {
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| invalid(archive_path, &e.to_string()))?;
    if archive.len() > limits.files {
        return Err(invalid(archive_path, "备份文件数量超限"));
    }
    let mut extracted_names = BTreeSet::new();
    let mut actual = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| invalid(archive_path, &e.to_string()))?;
        // 校验到使用之间源 ZIP 仍可能被另一写者修改；提取端自己守路径边界。
        if entry.is_dir()
            || !allowed_name(entry.name())
            || !extracted_names.insert(entry.name().to_lowercase())
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 != 0 && mode & 0o170000 != 0o100000)
            || entry.size() > limits.entry_bytes
        {
            return Err(invalid(archive_path, "提取条目路径、类型或大小非法"));
        }
        let relative = if entry.name() == "config.json" || entry.name().starts_with("themes/") {
            Path::new("preferences").join(entry.name())
        } else {
            PathBuf::from(entry.name())
        };
        let path = root.join(relative);
        let parent = path
            .parent()
            .ok_or_else(|| invalid(&path, "无效条目父目录"))?;
        create_parents(
            root,
            parent
                .strip_prefix(root)
                .map_err(|_| invalid(parent, "恢复路径越界"))?,
        )?;
        let mut output = private_file(&path, true)?;
        let copied = std::io::copy(
            &mut (&mut entry).take(limits.entry_bytes.saturating_add(1)),
            &mut output,
        )
        .io_path(&path)?;
        actual = actual
            .checked_add(copied)
            .ok_or_else(|| invalid(archive_path, "解压总量溢出"))?;
        if copied != entry.size() || copied > limits.entry_bytes || actual > limits.total_bytes {
            return Err(invalid(archive_path, "解压产物大小不匹配或超限"));
        }
        output.sync_all().io_path(&path)?;
    }
    Ok(())
}

fn clear_credentials(config: &mut Config) {
    config.sync.password.clear();
    config.account.key.clear();
    config.translation.openai.api_key.clear();
    config.translation.deepl.api_key.clear();
    for vendor in &mut config.vendors {
        vendor.api_key.clear();
    }
    config.cloud.webdav.password.clear();
    config.cloud.baidu.access_token.clear();
    config.cloud.baidu.refresh_token.clear();
    config.cloud.aliyun.access_token.clear();
    config.cloud.aliyun.refresh_token.clear();
    for catalog in &mut config.opds.catalogs {
        catalog.password.clear();
    }
}

pub(super) fn validate_ready_root(root: &Path) -> Result<()> {
    checked_directory(root)?;
    let marker = root.join(JOURNAL);
    let metadata = std::fs::symlink_metadata(&marker).io_path(&marker)?;
    if linked(&metadata) || !metadata.is_file() || metadata.len() > 16 * 1024 {
        return Err(invalid(root, "恢复标记不可用"));
    }
    let journal: Journal = serde_json::from_slice(&std::fs::read(&marker).io_path(&marker)?)?;
    if journal.version != 1 || !journal.ready {
        return Err(invalid(root, "恢复尚未准备完成"));
    }
    checked_directory(&root.join("preferences"))?;
    for name in ["preferences/config.json", "folio.db"] {
        let path = root.join(name);
        let metadata = std::fs::symlink_metadata(&path).io_path(&path)?;
        if linked(&metadata) || !metadata.is_file() {
            return Err(invalid(&path, "恢复文件不是普通文件"));
        }
    }
    let config_path = root.join("preferences/config.json");
    if std::fs::metadata(&config_path).io_path(&config_path)?.len() > 4 << 20 {
        return Err(invalid(&config_path, "恢复配置过大"));
    }
    let config: Config =
        serde_json::from_slice(&std::fs::read(&config_path).io_path(&config_path)?)?;
    if config.version > crate::CONFIG_VERSION {
        return Err(invalid(&config_path, "恢复配置来自更新版本"));
    }
    Store::inspect_backup(&root.join("folio.db"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::Path;

    use crate::backup::restore::extract_into;
    use crate::backup::{
        Manifest, Options, RestoreLimits, Roots, create, preview_restore, restore_into,
        restore_with_credentials,
    };
    use crate::{
        AnnotationStyle, BookSettings, Config, DocMeta, Format, Locator, Store, TextAnchor,
    };

    fn fixture(root: &Path) -> (Store, Config, Roots, i64) {
        let library = root.join("books");
        std::fs::create_dir(&library).unwrap();
        let path = library.join("a.md");
        std::fs::write(&path, "# One\n\nBook text").unwrap();
        let store = Store::open_at(&root.join("folio.db")).unwrap();
        let id = store
            .add_book(
                &path,
                &DocMeta {
                    title: "Book".into(),
                    format: Format::Markdown,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .add_annotation(
                id,
                &TextAnchor::point(0, 2),
                "keep",
                "amber",
                AnnotationStyle::Highlight,
            )
            .unwrap();
        store
            .set_position(
                id,
                "paged",
                &Locator {
                    chapter: 0,
                    offset: 4,
                },
            )
            .unwrap();
        store.add_bookmark(id, &TextAnchor::point(0, 3)).unwrap();
        let mut settings = BookSettings::default();
        settings
            .reader
            .insert("font_size".into(), serde_json::json!(23));
        store.set_book_settings(id, &settings).unwrap();
        let mut config = Config::default();
        config.library.dir = library.to_string_lossy().into_owned();
        config.sync.password = "restore-secret".into();
        let roots = Roots {
            backups: root.join("backups"),
            data: root.into(),
            library: Some(library),
            dicts: None,
            fonts: None,
            themes: None,
        };
        (store, config, roots, id)
    }

    #[test]
    fn restore_into_new_root_preserves_data_and_ready_retry_does_not_overwrite_edits() {
        let source = tempfile::tempdir().unwrap();
        let (store, config, roots, id) = fixture(source.path());
        let archive = create(&store, &config, &roots, Options::default(), &mut |_, _| {}).unwrap();
        assert!(
            preview_restore(&archive.path, RestoreLimits::default())
                .unwrap()
                .contains_credentials
        );
        let destination = source.path().join("restored");
        let result = restore_into(&archive.path, &destination, RestoreLimits::default()).unwrap();
        let restored = Store::open_at(&result.database).unwrap();
        assert_eq!(restored.annotations(id).unwrap()[0].note, "keep");
        assert_eq!(restored.position(id).unwrap().unwrap().1.offset, 4);
        assert_eq!(restored.bookmarks(id).unwrap().len(), 1);
        assert_eq!(restored.book_settings(id).unwrap().reader["font_size"], 23);
        assert_eq!(
            restored.book(id).unwrap().unwrap().path,
            result.library.join("a.md").to_string_lossy()
        );
        assert!(restored.unavailable_sources().unwrap().is_empty());
        let saved: Config =
            serde_json::from_slice(&std::fs::read(&result.config).unwrap()).unwrap();
        assert_eq!(saved.sync.password, "restore-secret");
        restored
            .set_book_meta(id, "edited after restore", &[])
            .unwrap();
        drop(restored);
        restore_into(&archive.path, &destination, RestoreLimits::default()).unwrap();
        assert_eq!(
            Store::open_at(&result.database)
                .unwrap()
                .book(id)
                .unwrap()
                .unwrap()
                .title,
            "edited after restore"
        );
        assert_eq!(store.book(id).unwrap().unwrap().title, "Book");
    }

    #[test]
    fn restore_rejects_foreign_destination_oversize_and_archive_traversal() {
        let source = tempfile::tempdir().unwrap();
        let (store, config, roots, _) = fixture(source.path());
        let archive = create(&store, &config, &roots, Options::default(), &mut |_, _| {}).unwrap();
        let occupied = source.path().join("occupied");
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(occupied.join("mine"), "keep").unwrap();
        assert!(restore_into(&archive.path, &occupied, RestoreLimits::default()).is_err());
        assert_eq!(
            std::fs::read_to_string(occupied.join("mine")).unwrap(),
            "keep"
        );
        assert!(
            preview_restore(
                &archive.path,
                RestoreLimits {
                    total_bytes: 1,
                    ..Default::default()
                }
            )
            .is_err()
        );
        let bad = source.path().join("bad.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&bad).unwrap());
        writer
            .start_file("../outside", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"bad").unwrap();
        writer.finish().unwrap();
        assert!(preview_restore(&bad, RestoreLimits::default()).is_err());
        assert!(!source.path().join("outside").exists());
    }

    #[test]
    fn restore_data_only_retains_unavailable_books_and_can_resume_interrupted_extraction() {
        let source = tempfile::tempdir().unwrap();
        let (store, config, roots, id) = fixture(source.path());
        let archive = create(
            &store,
            &config,
            &roots,
            Options {
                include_files: false,
            },
            &mut |_, _| {},
        )
        .unwrap();
        let target = source.path().join("restored");
        let restored = restore_into(&archive.path, &target, RestoreLimits::default()).unwrap();
        let original_journal: serde_json::Value =
            serde_json::from_slice(&std::fs::read(target.join(".folio-restore.json")).unwrap())
                .unwrap();
        let mut interrupted = original_journal;
        interrupted["ready"] = false.into();
        std::fs::write(
            target.join(".folio-restore.json"),
            serde_json::to_vec(&interrupted).unwrap(),
        )
        .unwrap();
        std::fs::write(&restored.database, b"partial").unwrap();
        std::fs::write(
            target.join(".folio-restore.lock"),
            b"stale after process exit",
        )
        .unwrap();
        restore_into(&archive.path, &target, RestoreLimits::default()).unwrap();
        let after = Store::open_at(&restored.database).unwrap();
        assert_eq!(after.annotations(id).unwrap()[0].note, "keep");
        assert_eq!(after.unavailable_sources().unwrap().len(), 1);
        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(target.join("manifest.json")).unwrap()).unwrap();
        assert!(!manifest.include_files);
    }

    #[test]
    fn restore_credentials_choice_covers_all_fields_without_changing_the_archive() {
        let temp = tempfile::tempdir().unwrap();
        let (store, mut config, roots, _) = fixture(temp.path());
        let secret = "RESTORE_CREDENTIAL_SENTINEL";
        config.sync.password = secret.into();
        config.account.key = secret.into();
        config.translation.openai.api_key = secret.into();
        config.translation.deepl.api_key = secret.into();
        config.vendors.push(crate::Vendor {
            api_key: secret.into(),
            ..Default::default()
        });
        config.cloud.webdav.password = secret.into();
        config.cloud.baidu.access_token = secret.into();
        config.cloud.baidu.refresh_token = secret.into();
        config.cloud.aliyun.access_token = secret.into();
        config.cloud.aliyun.refresh_token = secret.into();
        config.opds.catalogs.push(crate::OpdsCatalog {
            password: secret.into(),
            ..Default::default()
        });
        let archive = create(&store, &config, &roots, Options::default(), &mut |_, _| {}).unwrap();
        let before = std::fs::read(&archive.path).unwrap();
        for include in [false, true] {
            let destination = temp.path().join(if include { "with" } else { "without" });
            let restored = restore_with_credentials(
                &archive.path,
                &destination,
                RestoreLimits::default(),
                include,
            )
            .unwrap();
            let saved = std::fs::read_to_string(&restored.config).unwrap();
            assert_eq!(saved.contains(secret), include);
            assert!(
                !restored.root.join("config.json").exists(),
                "不能在恢复根留下原始凭据副本"
            );
            assert!(
                restore_with_credentials(
                    &archive.path,
                    &destination,
                    RestoreLimits::default(),
                    !include
                )
                .is_err()
            );
        }
        assert_eq!(std::fs::read(&archive.path).unwrap(), before);
        assert_eq!(config.sync.password, secret);
    }

    #[test]
    fn restore_extraction_revalidates_a_changed_archive_before_truncating_any_output() {
        let temp = tempfile::tempdir().unwrap();
        let (store, config, roots, _) = fixture(temp.path());
        let archive = create(&store, &config, &roots, Options::default(), &mut |_, _| {}).unwrap();
        preview_restore(&archive.path, RestoreLimits::default()).unwrap();
        let victim = temp.path().join("victim");
        std::fs::write(&victim, b"keep original").unwrap();
        let target = temp.path().join("restore-root");
        std::fs::create_dir(&target).unwrap();
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive.path).unwrap());
        writer
            .start_file(
                "library/../../victim",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(b"overwrite").unwrap();
        writer.finish().unwrap();
        assert!(
            extract_into(
                std::fs::File::open(&archive.path).unwrap(),
                &target,
                &archive.path,
                RestoreLimits::default()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep original");
        assert_eq!(std::fs::read_dir(target).unwrap().count(), 0);
    }
}
