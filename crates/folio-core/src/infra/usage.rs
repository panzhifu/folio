//! 磁盘上那一遍账：本应用占了多少，分别占在哪些去处。
//!
//! 为什么要有这一层：设置里的「存储」要把占用摆成图，而「哪一类多大」这件事只有一个
//! 说得出口的判据 —— **按目录归属分**。 [`crate::infra::paths`] 是唯一的路径来源，所以
//! 这一层的分类规则就是它那份形状的镜像；别处（界面、测试、将来的诊断 dump）不该再拼
//! 一遍路径、再各自数一遍。
//!
//! 三条规矩：
//!
//! 1. **路径全由调用方给**（[`Areas`]）。`FOLIO_DATA_DIR` 那一层确实能把根整体挪走，但
//!    环境变量是进程级的：并发测试里一改，别的用例就读到错的那份（`paths` 的测试为同样
//!    的理由把注入留在纯函数上）。所以这里收一份显式的 [`Areas`]，而
//!    [`Areas::from_app`] 替界面把 `paths` 那一套填好。
//! 2. **每个文件只算一次**。归类的判据是「它落在哪一处去处的最长前缀里」，判完再累加，
//!    所以读者把书库指到 `<数据根>/books`（默认位置）时那几百 MB 不会既进「书籍」又进
//!    「其它」。根目录也因此只走一遍：书库在数据根里面时不再单独走它。
//! 3. **不跟着符号链接走**。`DirEntry::file_type()` 是 lstat 的语义：链接指向的目录不进、
//!    指向的文件不算。沙箱外的一大片东西因此不会被算进本应用头上，循环链接也走不成。
//!
//! 读不进去的目录不 silently 算成 0：它记进 [`Usage::unreadable`]，界面据此说「有 N 处
//! 没读到」，而不是摆一个像是结论的零。

use std::path::{Path, PathBuf};

/// 一类去处。顺序就是明细行的顺序（图表自己按大小排）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Area {
    /// 书库根上那些书本文件（导进来的副本）。
    Books,
    /// `<书库>/covers`：封面，按封面自己的字节哈希命名。
    Covers,
    /// `<书库>/.thumbs`：缩略图缓存。删掉只需重生成。
    Thumbnails,
    /// `<书库>/.staging`：导入没走完的半成品。
    Staging,
    /// 库文件本体连 `-wal`、`-shm` 那两条附档。
    Database,
    /// 当前正在写的那一份日志。
    Logs,
    /// 轮转出来的旧日志（`folio.log.1` …）。正在写的那一份不算在这里。
    RotatedLogs,
    /// `<数据根>/cache/comics`：漫画解压缓存。删掉只需重解。
    Comics,
    /// `<数据根>/inbox`：收件箱（监听与 Wi-Fi 传书的落点）。
    Inbox,
    /// 配置根：`config.json` 与 `themes/`。
    Config,
    /// 数据根里不属于上面任何一类的地方。不为零时它是一句真话：有东西没被账目认领。
    Other,
}

impl Area {
    /// 全部去处。遍历与「每类都得有一行」的回归门都按这一份走。
    pub const ALL: [Area; 11] = [
        Area::Books,
        Area::Covers,
        Area::Thumbnails,
        Area::Staging,
        Area::Database,
        Area::Logs,
        Area::RotatedLogs,
        Area::Comics,
        Area::Inbox,
        Area::Config,
        Area::Other,
    ];
}

/// 要量的那几处去处。
///
/// 后四类挂在 [`Self::library`] 底下（书库根是读者确认过的那个目录，`covers` / `.thumbs`
/// / `.staging` 是它的三个区，见 `plans/导入管线.md` 第一、八节）。书库没确认过时它是
/// `None`，那三类自然就没有字节。
#[derive(Clone, Debug)]
pub struct Areas {
    /// 配置根。
    pub config: PathBuf,
    /// 数据根。「其它」兜的正是它。
    pub data: PathBuf,
    /// 库文件本体。`-wal` 与 `-shm` 由这个名字推出来。
    pub db: PathBuf,
    /// 日志目录。
    pub logs: PathBuf,
    /// 当前正在写的那一份日志；`None` = 文件日志关着，此时目录里剩下的都是旧份。
    pub log_active: Option<PathBuf>,
    /// 收件箱。
    pub inbox: PathBuf,
    /// 漫画解压缓存。
    pub comics: PathBuf,
    /// 读者确认过的书库根；没确认过时 `None`。
    pub library: Option<PathBuf>,
}

impl Areas {
    /// 按 [`crate::infra::paths`] 填一份，只把两处**依配置而定**的入参留给调用方：
    /// 书库位置（读者在设置里确认的那个）与日志落在哪儿（`logging.dir` 可以指到别处，
    /// 关掉文件日志时根本没有那一份）。界面要的正是这一句；测试自己拼 [`Areas`]。
    pub fn from_app(library: Option<PathBuf>, log_active: Option<PathBuf>) -> Self {
        let data = crate::infra::paths::data_dir();
        let logs = log_active
            .as_ref()
            .and_then(|path| path.parent())
            .map(|dir| dir.to_path_buf())
            .unwrap_or_else(crate::infra::paths::log_dir);
        Self {
            config: crate::infra::paths::config_dir(),
            db: crate::infra::paths::db_path(),
            inbox: crate::infra::paths::inbox_dir(),
            comics: crate::infra::paths::comics_cache_dir(),
            logs,
            log_active,
            data,
            library,
        }
    }

    /// 书库根；没确认过时 `None`。
    fn library_root(&self) -> Option<&Path> {
        self.library.as_deref()
    }

    /// 这个文件属于哪一类。判据是前缀，先判的赢，所以顺序就是**特异性**顺序。
    fn classify(&self, file: &Path) -> Area {
        if is_db_sibling(file, &self.db) {
            return Area::Database;
        }
        if file.starts_with(&self.logs) {
            return if self.log_active.as_deref() == Some(file) {
                Area::Logs
            } else {
                Area::RotatedLogs
            };
        }
        if file.starts_with(&self.inbox) {
            return Area::Inbox;
        }
        if file.starts_with(&self.comics) {
            return Area::Comics;
        }
        if let Some(root) = self.library_root() {
            if file.starts_with(crate::infra::paths::staging_dir(root)) {
                return Area::Staging;
            }
            if file.starts_with(crate::infra::paths::covers_dir(root)) {
                return Area::Covers;
            }
            if file.starts_with(crate::infra::paths::thumbs_dir(root)) {
                return Area::Thumbnails;
            }
            if file.starts_with(root) {
                return Area::Books;
            }
        }
        if file.starts_with(&self.config) {
            return Area::Config;
        }
        Area::Other
    }

    /// 要走一遍的根。书库在配置根或数据根里面时**不再单独走它** —— 那遍已经覆盖了它，
    /// 再走一次就是同一个文件算两遍。
    fn walk_roots(&self) -> Vec<&Path> {
        let mut roots = vec![self.config.as_path(), self.data.as_path()];
        // `let` 链而不是套一层 `if`：两个条件说的是同一件事（这一遍已经覆盖了书库）。
        if let Some(library) = self.library_root()
            && !library.starts_with(&self.data)
            && !library.starts_with(&self.config)
        {
            roots.push(library);
        }
        roots
    }
}

/// 库文件的两条附档：同名的 `-wal` 与 `-shm`（WAL 模式下一直在长）。
fn is_db_sibling(file: &Path, db: &Path) -> bool {
    if file == db {
        return true;
    }
    let Some(name) = db.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(candidate) = file.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    candidate == format!("{name}-wal") || candidate == format!("{name}-shm")
}

/// 一处去处的账。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub area: Area,
    /// 常规文件的字节数之和。**不是**磁盘占用：稀疏文件与 4 KiB 一格的块对齐都不在这里折，
    /// 那一档差别对「哪一类是大头」这个问题没有影响。
    pub bytes: u64,
    /// 数到几个文件。界面用它说「N 个文件」与「旧日志几份」。
    pub files: u64,
}

/// 整本账。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// 每一类一条，顺序是 [`Area::ALL`]。零字节的那几类也在里面 —— 「这一类现在没有东西」
    /// 是一条结论，不是缺项。
    pub entries: Vec<Entry>,
    /// 总字节。与 [`Self::entries`] 之和恒等（有条回归门钉着）。
    pub total: u64,
    /// 几个目录想读而读不进来（权限、沙箱、断开的挂载点）。不为零时那一句「一共占了多少」
    /// 是**下界**而不是结论。
    pub unreadable: usize,
}

impl Usage {
    /// 取某一类的账。[`Areas`] 之外传进来的类别不会缺项，因此这里不返回 `Option`。
    pub fn get(&self, area: Area) -> Entry {
        self.entries
            .iter()
            .find(|entry| entry.area == area)
            .copied()
            .unwrap_or(Entry {
                area,
                bytes: 0,
                files: 0,
            })
    }

    /// 有东西的那几类，按字节从大到小。图表要的正是这一份：切片按大小排才读得出主次。
    pub fn filled(&self) -> Vec<Entry> {
        let mut filled: Vec<Entry> = self
            .entries
            .iter()
            .copied()
            .filter(|entry| entry.bytes > 0)
            .collect();
        filled.sort_by_key(|entry| std::cmp::Reverse(entry.bytes));
        filled
    }
}

/// 走一遍 [`Areas`]，交出每一类的字节与文件数。
///
/// 这是一次**同步**的目录遍历：几百到几万个 `stat`，所以要跑在后台线程上
/// （`folio_app::ui::services::storage` 把它交给作业池）。这里不碰线程也不碰界面。
pub fn measure(areas: &Areas) -> Usage {
    let mut ledger = Ledger::default();
    // 库文件那三条按名字直接取：读者可能把库放在数据根之外（`Store::open` 收路径），
    // 那时走哪一根都碰不到它。走目录时把它们剔掉，所以这一笔不会重复。
    for path in db_files(areas) {
        if let Some(size) = regular_file_bytes(&path) {
            ledger.add(Area::Database, size);
        }
    }
    for root in areas.walk_roots() {
        walk(root, areas, &mut ledger);
    }
    Usage {
        entries: Area::ALL
            .iter()
            .enumerate()
            .map(|(index, area)| Entry {
                area: *area,
                bytes: ledger.bytes[index],
                files: ledger.files[index],
            })
            .collect(),
        total: ledger.bytes.iter().sum(),
        unreadable: ledger.unreadable,
    }
}

/// 库文件本体连它的两条附档。
fn db_files(areas: &Areas) -> [PathBuf; 3] {
    [
        areas.db.clone(),
        sibling(&areas.db, "-wal"),
        sibling(&areas.db, "-shm"),
    ]
}

/// 同一个父目录下、名字是「本体 + 后缀」的那一条。
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => path.with_file_name(format!("{name}{suffix}")),
        // 本体连名字都没有（`/`、空路径）时这一条注定不存在，交回一个不可能命中的路径。
        None => Path::new(suffix).to_path_buf(),
    }
}

/// [`Area`] 在累加数组里的下标。
fn area_index(area: Area) -> usize {
    Area::ALL
        .iter()
        .position(|candidate| *candidate == area)
        .unwrap_or(Area::ALL.len() - 1)
}

/// 常规文件的大小；目录、链接、读不到的都回 `None`。
///
/// 用 `symlink_metadata` 而不是 `metadata`：后者会跟着链接走，那正好是这一层不要的语义。
fn regular_file_bytes(path: &Path) -> Option<u64> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(meta.len())
}

/// 一遍遍历的累加器。每类一格，所以「总数 = 各类之和」这条恒等式不看第二份账也成立。
#[derive(Default)]
struct Ledger {
    bytes: [u64; Area::ALL.len()],
    files: [u64; Area::ALL.len()],
    unreadable: usize,
}

impl Ledger {
    fn add(&mut self, area: Area, bytes: u64) {
        let index = area_index(area);
        self.bytes[index] += bytes;
        self.files[index] += 1;
    }
}

/// 走一遍目录。库文件三条在这里跳过 —— 它们由 [`measure`] 单独记。
fn walk(dir: &Path, areas: &Areas, ledger: &mut Ledger) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // 目录不存在不算「读不到」：那一个区没有东西就是零。只有**存在而读不进**才记一笔，
        // 那一笔是让界面改口说「这是下界」的唯一依据。
        if dir.exists() {
            ledger.unreadable += 1;
        }
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // `file_type()` 不跟着链接走：链接指向的目录不进，指向的文件既不算字节也不数。
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk(&path, areas, ledger);
            continue;
        }
        if !kind.is_file() || is_db_sibling(&path, &areas.db) {
            continue;
        }
        let Some(size) = regular_file_bytes(&path) else {
            continue;
        };
        ledger.add(areas.classify(&path), size);
    }
}

#[cfg(test)]
mod tests {
    //! 这一层的价值全在**归属判对**与**不重复**上：分类错一格，界面上那条结论就是错的；
    //! 同一个文件算两遍，「一共占了多少」就凭空涨一截。所以每条测试都现造一棵树，
    //! 每个文件写进去多少字节是自己定的，回来逐类比数。
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use super::{Area, Areas, measure};

    /// 在 `dir` 下写出一个 `size` 字节的文件（连父目录一起建）。
    ///
    /// 字节是真的写进去的，不是靠 `set_len` 撑：稀疏文件的 `len` 与磁盘占用本来就两个数，
    /// 夹具不该借那一格便宜。
    fn file(dir: &Path, name: &str, size: usize) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut handle = std::fs::File::create(&path).unwrap();
        handle.write_all(&vec![b'x'; size]).unwrap();
        handle.flush().unwrap();
        path
    }

    /// 一棵把每个去处都摆上的树：数据根、配置根，书库就落在**默认那个位置**
    /// （`<数据根>/books`）—— 那正是重复计算最容易发生的地方。
    fn fixture(root: &Path) -> Areas {
        let data = root.join("data");
        let config = root.join("config");
        let library = data.join("books");
        file(&data, "folio.db", 100);
        file(&data, "folio.db-wal", 10);
        file(&data, "folio.db-shm", 20);
        file(&data, "logs/folio.log", 5);
        file(&data, "logs/folio.log.1", 6);
        file(&data, "logs/folio.log.2", 7);
        file(&data, "inbox/新来的.epub", 30);
        file(&data, "cache/comics/abc/page-001.png", 40);
        // 不属于任何一类的一笔：它是「其它」那一格唯一该收到的东西。
        file(&data, "stray.bin", 11);
        file(&config, "config.json", 3);
        file(&config, "themes/夜间.json", 4);
        file(&library, "书一本.epub", 70);
        file(&library, "covers/封面哈希.jpg", 12);
        file(&library, ".thumbs/400/封面哈希.jpg", 8);
        file(&library, ".staging/.半成品.1.part", 9);
        Areas {
            config,
            data: data.clone(),
            db: data.join("folio.db"),
            logs: data.join("logs"),
            log_active: Some(data.join("logs/folio.log")),
            inbox: data.join("inbox"),
            comics: data.join("cache/comics"),
            library: Some(library),
        }
    }

    /// 每一类收到的字节必须是它那一处的字节，且**一个文件不进两类**。
    ///
    /// 这条是把 [`Areas::classify`] 的优先级与 [`Areas::walk_roots`] 的去重一起钉住：
    /// 书库默认就躺在数据根里，先走数据根再走书库的那版写法会把那 70+12+8+9 字节
    /// 各算两遍，而界面上只会多出半个看不见的数。
    #[test]
    fn every_area_gets_its_own_bytes_and_no_file_is_counted_twice() {
        let tmp = tempfile::tempdir().unwrap();
        let usage = measure(&fixture(tmp.path()));
        let expected = [
            (Area::Books, 70u64, 1u64),
            (Area::Covers, 12, 1),
            (Area::Thumbnails, 8, 1),
            (Area::Staging, 9, 1),
            (Area::Database, 130, 3),
            (Area::Logs, 5, 1),
            (Area::RotatedLogs, 13, 2),
            (Area::Comics, 40, 1),
            (Area::Inbox, 30, 1),
            (Area::Config, 7, 2),
            (Area::Other, 11, 1),
        ];
        for (area, bytes, files) in expected {
            let entry = usage.get(area);
            assert_eq!(
                (entry.bytes, entry.files),
                (bytes, files),
                "{area:?} 那一步账数不对"
            );
        }
        // 总数就是各格之和：没有任何字节属于两格，也没有哪一格漏在账外。
        let sum: u64 = usage.entries.iter().map(|entry| entry.bytes).sum();
        assert_eq!(usage.total, sum, "总数与各类之和对不上");
        // 130 + 18 + 30 + 40 + 11 + 7 + 99 = 335，一个字节不多、一个字节不少。
        assert_eq!(usage.total, 335, "整棵夹具写进去的就是这 335 字节");
        assert_eq!(usage.unreadable, 0, "每个目录都读得进，不该记失败");
    }

    /// 书库指到**数据根外面**时那一遍也得走到，且不重复。
    ///
    /// 桌面与 Android 外部目录都是这一种（`library.rs` 的 `candidate_dirs` 把外部那条排在
    /// 前面）。这一条与上一条方向相反：把 `walk_roots` 的去重写反（书库永远不单独走）时
    /// 这一条红，写在外面时（永远单独走）上一条红。
    #[test]
    fn a_library_outside_the_data_root_is_walked_once() {
        let tmp = tempfile::tempdir().unwrap();
        let mut areas = fixture(tmp.path());
        let outside = tmp.path().join("书库");
        file(&outside, "别处的书.epub", 60);
        file(&outside, "covers/别处封面.jpg", 6);
        areas.library = Some(outside);
        // 数据根里那个默认位置还在：里面的东西已经不是「书」，是没被认领的字节。
        let usage = measure(&areas);
        assert_eq!(usage.get(Area::Books).bytes, 60);
        assert_eq!(usage.get(Area::Covers).bytes, 6);
        // 原 `<数据根>/books` 那 99 字节 + 数据根里那笔 stray，一同落到「其它」。
        assert_eq!(usage.get(Area::Other).bytes, 99 + 11);
        // 原来那 335 一个字节都没丢（只是从「书」改口成「其它」），新写的 66 全收到。
        assert_eq!(usage.total, 335 + 66, "换书库位置不该让字节变多或变少");
    }

    /// 正在写的那一份日志与轮转出来的旧份是两格。
    ///
    /// 界面上旧的那一格配一颗「清掉」，而当前这一份**不给删**：fd 还攥着它，删掉之后
    /// 日志只会静默写进一个没人看的 inode（`infra::logging` 那份 `LogFile` 每 4 MiB 才轮转
    /// 重开）。两格混成一格，那颗按钮就会长得像能删当前那份。
    #[test]
    fn the_active_log_is_split_from_its_rotated_predecessors() {
        let tmp = tempfile::tempdir().unwrap();
        let usage = measure(&fixture(tmp.path()));
        assert_eq!(usage.get(Area::Logs).files, 1);
        assert_eq!(usage.get(Area::RotatedLogs).files, 2);
        // 关掉文件日志（没有当前那一份）时，目录里剩下的都算旧份。
        let mut areas = fixture(tmp.path());
        areas.log_active = None;
        let usage = measure(&areas);
        assert_eq!(usage.get(Area::Logs).bytes, 0);
        assert_eq!(usage.get(Area::RotatedLogs).bytes, 18);
    }

    /// 链接指向的东西不算在本应用头上，也不跟着它走进别处。
    ///
    /// 判据是 `DirEntry::file_type()` 与 `symlink_metadata` 都不跟着链接走。这条不只是为了
    /// 数字准：读者（或一本恶意书留下的目录）在书库里放一个指向 `/` 的链接，跟着走就是
    /// 在界面线程之外把整块盘数一遍。
    #[cfg(unix)]
    #[test]
    fn a_symlink_is_neither_counted_nor_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let areas = fixture(tmp.path());
        let outside = tmp.path().join("外面");
        file(&outside, "大块.bin", 9_000);
        let library = areas.library.clone().unwrap();
        std::os::unix::fs::symlink(&outside, library.join("链到外面")).unwrap();
        // 一条指向大文件的链接：`is_file()` 对它是假（lstat 语义），因此连字节都不取。
        std::os::unix::fs::symlink(outside.join("大块.bin"), library.join("链到文件.bin")).unwrap();
        let usage = measure(&areas);
        assert_eq!(usage.get(Area::Books).bytes, 70, "链接不该给书库添字节");
        assert_eq!(usage.get(Area::Books).files, 1, "链接不该给书库添文件数");
        assert_eq!(usage.total, 335, "整本账不该因为两条链接涨起来");
    }

    /// 什么都没建时是零，而不是报错或 panic。
    ///
    /// 首次启动就是这个状态：两个根还不存在，书库没确认过。这一屏那时候正需要摆出来。
    #[test]
    fn an_untouched_installation_measures_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let areas = Areas {
            config: tmp.path().join("config"),
            data: tmp.path().join("data"),
            db: tmp.path().join("data/folio.db"),
            logs: tmp.path().join("data/logs"),
            log_active: None,
            inbox: tmp.path().join("data/inbox"),
            comics: tmp.path().join("data/cache/comics"),
            library: None,
        };
        let usage = measure(&areas);
        assert_eq!(usage.total, 0);
        assert!(usage.filled().is_empty(), "零占用不该有任何一类上榜");
        assert_eq!(usage.unreadable, 0, "目录不存在是「没有」，不是「读不到」");
        // 每一类都得有一条，哪怕是零：界面按它决定「这一格摆不摆」。
        assert_eq!(usage.entries.len(), Area::ALL.len());
    }

    /// 「存在而读不进去」要记一笔，好让界面把那句总数说成下界。
    ///
    /// 有 root 权限时 0o000 也照样读得进（那不是一个失败条件），所以这条先把「读不进去」
    /// 这件事自己验一遍，验不成就原样放行 —— 放行的是夹具的前提，不是判据。
    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_opened_is_reported() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().unwrap();
        let mut areas = fixture(tmp.path());
        let locked = tmp.path().join("data/locked");
        file(&locked, "里的.bin", 50);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read_dir(&locked).is_ok() {
            // 这一台机器上 chmod 拦不住读（root），夹具的前提不成立。
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        areas.library = None;
        let usage = measure(&areas);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(usage.unreadable, 1, "读不进的那一处该被记下来");
        assert_eq!(
            usage.get(Area::Other).bytes,
            11 + 99,
            "读不进的那一处只少它自己的 50 字节，别处照数"
        );
    }

    /// `Areas::from_app` 那份默认填法：除两处依配置而定的入参，其余**全部**由 `paths` 给。
    ///
    /// 这条钉的是「这一层与路径那一层不会各说一套」。它不碰 `FOLIO_DATA_DIR`（环境变量是
    /// 进程级的，并发用例互相读得到），验的是形状：各处去处各挂在哪个根下面，以及书库没
    /// 确认过时那三个区不认领任何字节。
    #[test]
    fn the_app_defaults_follow_paths_and_only_take_the_two_config_dirs() {
        use super::super::paths;
        use crate::infra::paths::APP_DIR;

        let areas = Areas::from_app(None, None);
        let data = paths::data_dir();
        assert_eq!(areas.data, data);
        assert_eq!(areas.config, paths::config_dir());
        assert_eq!(areas.db, paths::db_path());
        assert_eq!(areas.logs, paths::log_dir());
        assert_eq!(areas.inbox, paths::inbox_dir());
        assert_eq!(areas.comics, paths::comics_cache_dir());
        assert_eq!(areas.log_active, None, "没给日志文件时那一格就是「没有」");
        assert_eq!(areas.library, None, "书库没确认过时不该凭空造一个去处");
        // 两个根各自落在应用子目录里（`APP_DIR`），不是直接写进标准目录。
        assert!(areas.data.ends_with(APP_DIR) && areas.config.ends_with(APP_DIR));

        // 给了日志文件与书库时各归各处：日志目录跟着那个文件走，而不是跟着数据根。
        let log = Path::new("/别处/现场/folio.log");
        let library = Path::new("/书库");
        let areas = Areas::from_app(Some(library.to_path_buf()), Some(log.to_path_buf()));
        assert_eq!(areas.logs, Path::new("/别处/现场"));
        assert_eq!(areas.library.as_deref(), Some(library));
        assert!(
            areas.data.ends_with(APP_DIR),
            "换了书库位置不该动数据根：两句是各算各的"
        );

        // 那三个区是从**书库根**推出来的，判据就落在 classify 那一处。
        assert_eq!(
            areas.classify(&library.join("covers/一张.jpg")),
            Area::Covers,
            "封面区挂在确认过的那个书库根下面"
        );
        assert_eq!(
            areas.classify(&library.join(".thumbs/400/一张.jpg")),
            Area::Thumbnails
        );
        assert_eq!(
            areas.classify(&library.join(".staging/.半成品.1.part")),
            Area::Staging
        );
        assert_eq!(areas.classify(&library.join("一本.epub")), Area::Books);
    }

    /// 明细的排序与筛零：图表按大小切分，列表也只摆真有东西的那几类。
    #[test]
    fn filled_sorts_by_size_and_drops_the_empty_areas() {
        let tmp = tempfile::tempdir().unwrap();
        let usage = measure(&fixture(tmp.path()));
        let filled = usage.filled();
        assert_eq!(filled[0].area, Area::Database, "最大那类该排第一");
        assert_eq!(filled.first().unwrap().bytes, 130);
        assert!(
            filled.windows(2).all(|pair| pair[0].bytes >= pair[1].bytes),
            "filled 必须从大到小"
        );
        assert!(
            filled.iter().all(|entry| entry.bytes > 0),
            "零字节的那几类不该上榜"
        );
        // 夹具里每一类都有东西，所以这一版是一条不落。
        assert_eq!(filled.len(), Area::ALL.len());
    }
}
