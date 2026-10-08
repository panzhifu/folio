//! Folio 把自己的文件放在哪里。
//!
//! 两类目录，各自按各操作系统的标准位置走，并且各留一个环境变量可以整体挪走：
//!
//! ```text
//! 配置  ~/.config/folio            config.json、themes/
//! 数据  ~/.local/share/folio       书库数据库
//! ```
//!
//! `FOLIO_CONFIG_DIR` / `FOLIO_DATA_DIR` 存在时优先生效：测试要把配置读写关在
//! 临时目录里，另一台机器上也可能想把整个应用目录挪走。取不到系统目录时（裸容器、
//! 环境残缺）退回 `$HOME` 下的点目录，程序仍然能启动，只是会把落盘位置说出来。

use std::path::{Path, PathBuf};

/// 各标准目录下用来分隔本应用的子目录名。
pub const APP_DIR: &str = "folio";

/// 挪走配置根的环境变量。
pub const CONFIG_DIR_ENV: &str = "FOLIO_CONFIG_DIR";

/// 挪走数据根的环境变量。
pub const DATA_DIR_ENV: &str = "FOLIO_DATA_DIR";

/// 环境变量的值 → 路径，空串按「没设」处理（`FOO=` 是清掉，不是指到当前目录）。
/// 拆成纯函数是为了能测：`set_var` 在并发测试里会改到别人读的环境。
fn resolved(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value
        .filter(|p| !p.as_os_str().is_empty())
        .map(PathBuf::from)
}

/// 环境变量指的路径，取不到「没设」时返回 `None`。
fn env_dir(variable: &str) -> Option<PathBuf> {
    resolved(std::env::var_os(variable))
}

/// `<标准目录>/folio`；取不到标准目录时退回 `~/<fallback>/folio`。
fn root(pick: impl Fn() -> Option<PathBuf>, fallback: &str, variable: &str) -> PathBuf {
    if let Some(dir) = env_dir(variable) {
        return dir;
    }
    match pick() {
        Some(dir) => dir.join(APP_DIR),
        None => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"));
            home.join(fallback).join(APP_DIR)
        }
    }
}

/// 配置根：`~/.config/folio`，可用 [`CONFIG_DIR_ENV`] 挪走。
pub fn config_dir() -> PathBuf {
    root(dirs::config_dir, ".config", CONFIG_DIR_ENV)
}

/// 数据根：`~/.local/share/folio`，可用 [`DATA_DIR_ENV`] 挪走。
pub fn data_dir() -> PathBuf {
    root(dirs::data_dir, ".local/share", DATA_DIR_ENV)
}

/// 配置文件：`<配置根>/config.json`。
pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// 用户自己放主题 JSON 的目录：`<配置根>/themes`。
///
/// 目录不存在也照常返回路径 —— 里面的东西全是可选的，缺目录只是没有额外主题。
pub fn themes_dir() -> PathBuf {
    config_dir().join("themes")
}

/// 书库数据库：`<数据根>/folio.db`。
pub fn db_path() -> PathBuf {
    data_dir().join("folio.db")
}

/// 建出 `path` 的父目录。写文件前都要过一次：首次启动时这些根目录还不存在。
pub fn prepare_dir(path: &Path) -> std::io::Result<()> {
    match path.parent() {
        Some(dir) => std::fs::create_dir_all(dir),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::{APP_DIR, config_dir, config_path, data_dir, db_path, resolved, root, themes_dir};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    /// 注入路径来验，不用 `set_var`：环境变量是进程级的，并发跑测试时一改
    /// 就会让别的用例读到错的那份。变量名给一个必然不存在的，确保走的是 `pick()`。
    #[test]
    fn both_roots_end_in_the_app_directory() {
        let dir = PathBuf::from("/tmp/folio-roots");
        assert_eq!(
            root(|| Some(dir.clone()), ".config", "FOLIO_TEST_UNSET"),
            dir.join(APP_DIR)
        );

        // 取不到系统目录时的兜底也要落在应用子目录里，而不是直接写进家目录。
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        assert_eq!(
            root(|| None, ".myhome-fallback", "FOLIO_TEST_UNSET"),
            home.join(".myhome-fallback").join(APP_DIR)
        );
    }

    #[test]
    fn the_derived_paths_sit_under_their_own_root() {
        assert_eq!(config_path(), config_dir().join("config.json"));
        assert_eq!(themes_dir(), config_dir().join("themes"));
        assert_eq!(db_path(), data_dir().join("folio.db"));
    }

    /// 空串按「没设」处理，否则 `FOLIO_CONFIG_DIR=` 会把配置指到当前目录。
    #[test]
    fn an_empty_value_is_not_a_directory() {
        assert_eq!(resolved(Some(OsString::from(""))), None);
        assert_eq!(resolved(None), None);
        assert_eq!(
            resolved(Some(OsString::from("/tmp/elsewhere"))),
            Some(PathBuf::from("/tmp/elsewhere"))
        );
    }

    #[test]
    fn prepare_dir_makes_the_parent_and_tolerates_a_bare_filename() {
        let dir = std::env::temp_dir().join("folio-prepare-dir");
        let _ = std::fs::remove_dir_all(&dir);
        super::prepare_dir(&dir.join("config.json")).unwrap();
        assert!(dir.is_dir());
        assert!(super::prepare_dir(Path::new("config.json")).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
