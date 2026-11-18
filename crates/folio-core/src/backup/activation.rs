//! 恢复只准备独立副本；持久指针在下一进程打开配置和数据库前生效。

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::backup::restore::validate_ready_root;
use crate::backup::{RestoreLimits, Restored, restore_with_credentials};
use crate::error::{Error, IoResultExt as _};

const POINTER: &str = "restore-root.json";
const REQUIRED: &str = "restore-root.required";

/// 稳定启动根，不随当前已激活的恢复副本变化。测试可显式注入临时目录。
#[derive(Clone)]
pub struct RestoreActivation {
    config: PathBuf,
    data: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    version: u32,
    directory: String,
}

fn invalid(path: &Path, why: &str) -> Error {
    Error::Backup {
        path: path.into(),
        why: why.into(),
    }
}

impl RestoreActivation {
    pub fn new(config: PathBuf, data: PathBuf) -> Self {
        Self { config, data }
    }

    pub fn from_startup() -> Self {
        Self::new(
            folio_base::paths::startup_config_dir(),
            folio_base::paths::startup_data_dir(),
        )
    }

    /// 写入下一次启动的选择；不会修改活动根、原库或原配置。
    pub fn prepare(&self, archive: &Path, include_credentials: bool) -> Result<Restored> {
        std::fs::create_dir_all(&self.config).io_path(&self.config)?;
        std::fs::create_dir_all(&self.data).io_path(&self.data)?;
        let area = self
            .data
            .canonicalize()
            .io_path(&self.data)?
            .join("restores");
        std::fs::create_dir_all(&area).io_path(&area)?;
        crate::backup::restore::checked_directory(&area)?;
        let destination = area.join(crate::sync::new_device_id());
        let restored = restore_with_credentials(
            archive,
            &destination,
            RestoreLimits::default(),
            include_credentials,
        )?;
        self.select(&restored.root)?;
        Ok(restored)
    }

    fn select(&self, root: &Path) -> Result<()> {
        validate_ready_root(root)?;
        let root = root.canonicalize().io_path(root)?;
        let parent = self
            .data
            .join("restores")
            .canonicalize()
            .io_path(&self.data)?;
        if root.parent() != Some(parent.as_path()) {
            return Err(invalid(&root, "恢复副本不在启动根的恢复区"));
        }
        let directory = root
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|name| valid_directory(name))
            .ok_or_else(|| invalid(&root, "恢复目录名称非法"))?;
        // 独立标记先持久化：指针丢失也不能把已写入的新库静默回滚到原库。
        let required = self.config.join(REQUIRED);
        let marker = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&required)
            .io_path(&required)?;
        marker.sync_all().io_path(&required)?;
        #[cfg(unix)]
        File::open(&self.config)
            .io_path(&self.config)?
            .sync_all()
            .io_path(&self.config)?;
        let pointer = self.config.join(POINTER);
        let part = self.config.join(format!(
            ".restore-root-{}.part",
            crate::sync::new_device_id()
        ));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut file = options.open(&part).io_path(&part)?;
            file.write_all(&serde_json::to_vec(&Selection {
                version: 1,
                directory: directory.into(),
            })?)
            .io_path(&part)?;
            file.sync_all().io_path(&part)?;
            drop(file);
            std::fs::rename(&part, &pointer).io_path(&pointer)?;
            #[cfg(unix)]
            File::open(&self.config)
                .io_path(&self.config)?
                .sync_all()
                .io_path(&self.config)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        result
    }

    /// 已选择的根一旦损坏就报错；绝不自动回退旧库或创建空库。
    pub fn selected_root(&self) -> Result<Option<PathBuf>> {
        let pointer = self.config.join(POINTER);
        let mut file = match File::open(&pointer) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let required = self.config.join(REQUIRED);
                return match std::fs::symlink_metadata(&required) {
                    Ok(_) => Err(invalid(
                        &pointer,
                        "启动恢复指针丢失，已停止启动以保护已恢复数据",
                    )),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(invalid(&required, &error.to_string())),
                };
            }
            Err(error) => return Err(invalid(&pointer, &error.to_string())),
        };
        let mut bytes = Vec::new();
        (&mut file)
            .take(4097)
            .read_to_end(&mut bytes)
            .io_path(&pointer)?;
        if bytes.len() > 4096 {
            return Err(invalid(&pointer, "启动恢复指针过大"));
        }
        let selection: Selection = serde_json::from_slice(&bytes)?;
        if selection.version != 1 || !valid_directory(&selection.directory) {
            return Err(invalid(&pointer, "启动恢复指针非法"));
        }
        // 系统根别名（例如 macOS /var）可用；恢复区内部的链接仍由检查器拒绝。
        let root = self
            .data
            .canonicalize()
            .io_path(&self.data)?
            .join("restores")
            .join(selection.directory);
        validate_ready_root(&root)?;
        Ok(Some(root.canonicalize().io_path(&root)?))
    }
}

fn valid_directory(name: &str) -> bool {
    name.len() == 32 && name.bytes().all(|c| c.is_ascii_hexdigit())
}

/// 仅供平台入口调用：必须早于配置、日志、Store 和后台服务。
pub fn activate_restore_on_startup() -> Result<()> {
    if let Some(root) = RestoreActivation::from_startup().selected_root()?
        && !folio_base::paths::set_session_dirs(root.join("preferences"), root.clone())
    {
        return Err(invalid(&root, "本进程的存储根已经固定，不能再次激活"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::backup::{Options, RestoreActivation, Roots, create};
    use crate::{Config, DocMeta, Format, Store};
    use std::path::Path;

    fn archive(root: &Path) -> (Store, std::path::PathBuf) {
        let library = root.join("books");
        std::fs::create_dir_all(&library).unwrap();
        let path = library.join("book.md");
        std::fs::write(&path, "Book").unwrap();
        let store = Store::open_at(&root.join("folio.db")).unwrap();
        store
            .add_book(
                &path,
                &DocMeta {
                    title: "before".into(),
                    format: Format::Markdown,
                    ..Default::default()
                },
            )
            .unwrap();
        let mut config = Config::default();
        config.library.dir = library.to_string_lossy().into();
        config.sync.password = "secret".into();
        let roots = Roots {
            backups: root.join("backups"),
            data: root.into(),
            library: Some(library),
            dicts: None,
            fonts: None,
            themes: None,
        };
        let created = create(&store, &config, &roots, Options::default(), &mut |_, _| {}).unwrap();
        (store, created.path)
    }

    #[test]
    fn restore_activation_keeps_original_writes_and_never_falls_back_after_new_writes() {
        let temp = tempfile::tempdir().unwrap();
        let (original, archive) = archive(temp.path());
        let config = temp.path().join("config");
        let activation = RestoreActivation::new(config.clone(), temp.path().join("data"));
        let restored = activation.prepare(&archive, false).unwrap();
        original.set_book_meta(1, "old session edit", &[]).unwrap();
        let next_boot = RestoreActivation::new(config, temp.path().join("data"));
        assert_eq!(
            next_boot.selected_root().unwrap(),
            Some(restored.root.clone())
        );
        let active = Store::open_at(&restored.database).unwrap();
        assert_eq!(active.book(1).unwrap().unwrap().title, "before");
        let restored_config = Config::load_from(&restored.config);
        assert!(restored_config.sync.password.is_empty());
        active.set_book_meta(1, "new session edit", &[]).unwrap();
        drop(active);
        let later_boot = next_boot.selected_root().unwrap().unwrap();
        assert_eq!(
            Store::open_at(&later_boot.join("folio.db"))
                .unwrap()
                .book(1)
                .unwrap()
                .unwrap()
                .title,
            "new session edit"
        );
        let pointer = next_boot.config.join("restore-root.json");
        let selection = std::fs::read(&pointer).unwrap();
        std::fs::remove_file(&pointer).unwrap();
        assert!(
            next_boot.selected_root().is_err(),
            "指针丢失不能把新库回滚到原库"
        );
        std::fs::write(&pointer, selection).unwrap();
        std::fs::write(&restored.config, b"bad config").unwrap();
        assert!(next_boot.selected_root().is_err());
        assert_eq!(original.book(1).unwrap().unwrap().title, "old session edit");
        assert!(restored.database.exists());
    }

    #[test]
    fn restore_activation_failure_preserves_previous_selection_and_rejects_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let (_original, archive) = archive(temp.path());
        let config = temp.path().join("config");
        let activation = RestoreActivation::new(config.clone(), temp.path().join("data"));
        let first = activation.prepare(&archive, true).unwrap();
        assert!(!Config::load_from(&first.config).sync.password.is_empty());
        let broken = temp.path().join("broken.zip");
        std::fs::write(&broken, b"broken").unwrap();
        assert!(activation.prepare(&broken, false).is_err());
        assert_eq!(activation.selected_root().unwrap(), Some(first.root));
        let second = activation.prepare(&archive, false).unwrap();
        assert_eq!(
            activation.selected_root().unwrap(),
            Some(second.root.clone())
        );
        assert!(Config::load_from(&second.config).sync.password.is_empty());
        std::fs::write(
            config.join("restore-root.json"),
            br#"{"version":1,"directory":"../old"}"#,
        )
        .unwrap();
        assert!(activation.selected_root().is_err());
    }

    #[cfg(any(unix, windows))]
    fn alias(root: &Path, target: &Path, link: &Path) {
        #[cfg(unix)]
        {
            let _ = root;
            std::os::unix::fs::symlink(target, link).unwrap();
        }
        #[cfg(windows)]
        {
            let script = root.join("junction.ps1");
            std::fs::write(&script, "param([string]$Link,[string]$Target)\nNew-Item -ItemType Junction -Path $Link -Target $Target -ErrorAction Stop | Out-Null\n").unwrap();
            assert!(
                std::process::Command::new("powershell.exe")
                    .args([
                        "-NoProfile",
                        "-NonInteractive",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File"
                    ])
                    .arg(script)
                    .arg("-Link")
                    .arg(link)
                    .arg("-Target")
                    .arg(target)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn restore_activation_accepts_a_startup_root_alias_but_rejects_links_inside_the_restore_area() {
        let temp = tempfile::tempdir().unwrap();
        let (_original, archive) = archive(temp.path());
        let data = temp.path().join("actual-data");
        std::fs::create_dir(&data).unwrap();
        let data_alias = temp.path().join("data-alias");
        alias(temp.path(), &data, &data_alias);
        let config = temp.path().join("config");
        let activation = RestoreActivation::new(config.clone(), data_alias);
        let prepared = activation.prepare(&archive, false).unwrap();
        assert_eq!(
            activation.selected_root().unwrap(),
            Some(prepared.root.clone())
        );
        let name = "00000000000000000000000000000000";
        alias(
            temp.path(),
            &prepared.root,
            &data.join("restores").join(name),
        );
        std::fs::write(
            config.join("restore-root.json"),
            format!(r#"{{"version":1,"directory":"{name}"}}"#),
        )
        .unwrap();
        assert!(activation.selected_root().is_err());
    }
}
