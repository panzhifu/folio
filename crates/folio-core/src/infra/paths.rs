//! Folio 把自己的文件放在哪里。
//!
//! 两类目录，各自按各操作系统的标准位置走，并且各留一个环境变量可以整体挪走：
//!
//! ```text
//! 配置  ~/.config/folio            config.json、themes/
//! 数据  ~/.local/share/folio       书库数据库、logs/、books/
//! ```
//!
//! `FOLIO_CONFIG_DIR` / `FOLIO_DATA_DIR` 存在时优先生效：测试要把配置读写关在
//! 临时目录里，另一台机器上也可能想把整个应用目录挪走。取不到系统目录时（裸容器、
//! 环境残缺）退回 `$HOME` 下的点目录，程序仍然能启动，只是会把落盘位置说出来。
//!
//! 移动平台两者都不适用 —— Android / iOS 上没有 `$HOME`，也没有「用户配置目录」这个
//! 概念，可写路径只有宿主在启动时给出的那一个应用专属目录。所以那里由宿主调用
//! [`set_app_dirs`] 显式报告，两个根各指一处。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::infra::error::IoResultExt as _;

/// 各标准目录下用来分隔本应用的子目录名。
pub const APP_DIR: &str = "folio";

/// 挪走配置根的环境变量。
pub const CONFIG_DIR_ENV: &str = "FOLIO_CONFIG_DIR";

/// 挪走数据根的环境变量。
pub const DATA_DIR_ENV: &str = "FOLIO_DATA_DIR";

/// 宿主报上来的应用专属目录。只有移动平台会设；设了就是唯一的可写位置。
static HOST_DIRS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();

/// 由宿主在初始化时报告配置根与数据根，用于**没有标准目录的平台**（Android / iOS）。
///
/// 只在第一次调用时生效：这些路径一旦有文件写进去就不能再换，否则第二次启动会
/// 读不到第一次的偏好。环境变量 [`CONFIG_DIR_ENV`] / [`DATA_DIR_ENV`] 仍然优先，
/// 所以调试时可以把整份状态指到别处。
pub fn set_app_dirs(config_dir: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) {
    let _ = HOST_DIRS.set((config_dir.into(), data_dir.into()));
}

/// 宿主报的目录；没报时返回 `None`。
fn host_dirs() -> Option<&'static (PathBuf, PathBuf)> {
    HOST_DIRS.get()
}

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

/// `<标准目录>/folio`；取不到标准目录时（裸容器、环境残缺）退回 `~/<fallback>/folio`，
/// `$HOME` 也没有时用 `/tmp` —— 至少程序能起来，落点会打印出来。
fn standard_or_fallback(pick: impl Fn() -> Option<PathBuf>, fallback: &str) -> PathBuf {
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

/// 三级顺序：环境变量 → 宿主报的应用专属目录 → 操作系统标准位置。
/// 环境变量在最前，所以调试时可以把整份状态指到别处。
///
/// 三个入参都由调用方给值，这里不读环境变量：环境变量是进程级的，
/// 测试里一改就会让并发跑的别的用例读到错的那份。
fn root(
    from_env: Option<PathBuf>,
    from_host: Option<PathBuf>,
    pick: impl Fn() -> Option<PathBuf>,
    fallback: &str,
) -> PathBuf {
    from_env
        .or(from_host)
        .unwrap_or_else(|| standard_or_fallback(pick, fallback))
}

/// 配置根：`~/.config/folio`，可用 [`CONFIG_DIR_ENV`] 挪走；
/// 宿主报过目录（[`set_app_dirs`]）时用那个。
pub fn config_dir() -> PathBuf {
    root(
        env_dir(CONFIG_DIR_ENV),
        host_dirs().map(|dirs| dirs.0.clone()),
        dirs::config_dir,
        ".config",
    )
}

/// 数据根：`~/.local/share/folio`，可用 [`DATA_DIR_ENV`] 挪走；
/// 宿主报过目录（[`set_app_dirs`]）时用那个。
pub fn data_dir() -> PathBuf {
    root(
        env_dir(DATA_DIR_ENV),
        host_dirs().map(|dirs| dirs.1.clone()),
        dirs::data_dir,
        ".local/share",
    )
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

/// 日志目录：`<数据根>/logs`。
///
/// 放在数据根而不是配置根：它是程序自己产出的东西，不是读者编辑的东西，
/// 而且会一直长。[`crate::infra::logging`] 按它排 `folio.log`，写满了轮转成 `.1`、`.2`。
pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

/// 应用自己收下的那份书本文件：`<数据根>/books/<原名>`。
///
/// 只有移动平台会往这里写 —— 系统文件选择器给的是它自己临时目录里的路径，随时会被清，
/// 身份现在由内容哈希定（`books.content_hash`，见 `plans/导入管线.md` §六），`books.path`
/// 只记副本躺在哪儿 —— 但「先复制到应用常驻的位置再入库」这一步没变。**这个目录也就是「哪些文件归本应用所有」的边界**：
/// 删一本书时只有落在这里的文件才允许连带删掉，桌面端记的是读者自己的原文件。
pub fn books_dir() -> PathBuf {
    data_dir().join("books")
}

/// 建出 `path` 的父目录。写文件前都要过一次：首次启动时这些根目录还不存在。
///
/// 返回的是 [`crate::Error`] 而不是 `std::io::Error`：后端对外只有一套错误词汇，
/// 而套上路径的那一条正好说清「哪个目录建不出来」——这在只读文件系统与 Android
/// 沙箱目录那两类真机上是最常撞到的第一步。
pub fn prepare_dir(path: &Path) -> crate::Result<()> {
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(dir).io_path(path)
}

// 下面四个是**书库根**里那几个区（`plans/导入管线.md` 第一、八节的磁盘形状）。
// 都收一个 `root` 而不是读全局：书库位置是读者在设置里确认过的那个目录，
// core 不知道它被配成了什么，也不该知道——知道的人（界面）把它传进来。
//
// 与文档的一处偏离要说明：文档写的是 `<书库>/books/<哈希名>`，这里**副本直接躺在根上**，
// 根就是那一层 `books/`。理由是老库的 `books.path` 有几百条指向现在这个位置，
// 加一层等于给每一条都搬一次家，而搬家本身是一次真实的 IO 与一次真实的失败面。

/// 暂存区：`<书库>/.staging`。导入第 2 步把来源的字节流整个写在这里，
/// 算完哈希、判完重才改名进根。**点开头**是为了让任何「扫目录找书」的逻辑
/// 天然跳过它（半成品不是书，见第九节那条测试门）。
pub fn staging_dir(root: &Path) -> PathBuf {
    root.join(".staging")
}

/// 封面区：`<书库>/covers`。按**封面自己的字节哈希**命名，不按书的哈希
/// （第八节：两本书共用一张系列封面时自然去重，重解析也不改名）。
pub fn covers_dir(root: &Path) -> PathBuf {
    root.join("covers")
}

/// 缩略图缓存的根：`<书库>/.thumbs`。整个目录删掉只需重跑生成——
/// 它是缓存，不是数据，所以库里只记尺寸与格式，不记图像字节。
pub fn thumbs_dir(root: &Path) -> PathBuf {
    root.join(".thumbs")
}

/// 某一档缩略图的去处：`<书库>/.thumbs/<像素数>`。
pub fn thumb_dir(root: &Path, px: u32) -> PathBuf {
    thumbs_dir(root).join(px.to_string())
}

/// 书架卡片那一档的宽（像素）。
pub const THUMB_CARD_PX: u32 = 400;

/// 列表那一档的高（像素）。
pub const THUMB_LIST_PX: u32 = 96;

#[cfg(test)]
mod tests {
    use super::{
        APP_DIR, THUMB_CARD_PX, THUMB_LIST_PX, config_dir, config_path, covers_dir, data_dir,
        db_path, resolved, root, staging_dir, themes_dir, thumb_dir, thumbs_dir,
    };
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    /// 注入路径来验，不用 `set_var`：环境变量是进程级的，并发跑测试时一改
    /// 就会让别的用例读到错的那份（而且 edition 2024 里它本身就是 unsafe）。
    #[test]
    fn both_roots_end_in_the_app_directory() {
        let dir = PathBuf::from("/tmp/folio-roots");
        assert_eq!(
            root(None, None, || Some(dir.clone()), ".config"),
            dir.join(APP_DIR)
        );

        // 取不到系统目录时的兜底也要落在应用子目录里，而不是直接写进家目录。
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        assert_eq!(
            root(None, None, || None, ".myhome-fallback"),
            home.join(".myhome-fallback").join(APP_DIR)
        );
    }

    /// 三级顺序：环境变量压过宿主报的目录，宿主报的又压过操作系统标准位置。
    /// 中间那条是「Android 上只有那一个可写目录」这条事实成立的前提，
    /// 最上面那条是调试时能把整份状态搬走的安全阀。
    #[test]
    fn the_host_directory_beats_the_standard_one_but_not_the_environment() {
        let host = PathBuf::from("/data/user/0/dev.folio.reader/files/folio");
        let standard = PathBuf::from("/home/noke/.config");
        // 没有环境变量时：用宿主报的那份，而且 `APP_DIR` 不再追加 —— 宿主给的已经是最终位置。
        assert_eq!(
            root(
                None,
                Some(host.clone()),
                || Some(standard.clone()),
                ".config"
            ),
            host
        );
        // 环境变量指了位置时：压过宿主报的那份。
        let debug = PathBuf::from("/tmp/folio-debug");
        assert_eq!(
            root(
                Some(debug.clone()),
                Some(host),
                || Some(standard.clone()),
                ".config"
            ),
            debug
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

    /// 书库那几个区都挂在**同一个根**底下，且半成品与缩略图那两个区以点开头：
    /// 「扫目录找书」的那条路（`scan`）因此天然不会把 `.staging` 里没写完的字节、
    /// 也不会把 `.thumbs` 里那些缓存当成一本书。`covers` 不藏：它就是数据。
    #[test]
    fn the_library_areas_hang_under_one_root() {
        let lib = Path::new("/书库");
        assert_eq!(staging_dir(lib), lib.join(".staging"));
        assert_eq!(covers_dir(lib), lib.join("covers"));
        assert_eq!(thumbs_dir(lib), lib.join(".thumbs"));
        assert_eq!(thumb_dir(lib, THUMB_CARD_PX), lib.join(".thumbs/400"));
        assert_eq!(thumb_dir(lib, THUMB_LIST_PX), lib.join(".thumbs/96"));
        // 两档的尺寸是文档第八节点名的两个数，改它要连着改生成那一处。
        assert_eq!((THUMB_CARD_PX, THUMB_LIST_PX), (400, 96));
        // 藏起来的那两个，名字必须真的以点开头。
        let hidden = |name: &Path| {
            name.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        };
        assert!(hidden(&staging_dir(lib)));
        assert!(hidden(&thumbs_dir(lib)));
    }
}
