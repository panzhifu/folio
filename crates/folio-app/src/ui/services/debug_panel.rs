//! 调试页的读数：库文件、书目数与日志尾巴，**只在 debug 构建或 `debug-ui` 特性下编进来**。
//!
//! 这一层存在的理由与 [`super::storage`] 同一条：读盘与查库都在后台线程上，渲染帧只读
//! 已经取回来的快照。区别在门槛 —— 「存储」那一页是给读者看的，它统计整棵数据目录；这一份
//! 是给改代码的人看的，只回答「库在哪儿、多大、几本书、日志最后写了什么」，量级小到一次
//! `stat` 加一次 `COUNT(*)` 规模的查询，但仍然不该出现在 `render` 里。
//!
//! 快照的三种「没有」必须分得开，它们不是同一件事：
//!
//! - **还没读过**（`snapshot == None`）：这一页刚打开，或者屏建出来之后一直没轮到读；
//! - **读过了、但日志关着**（`log_path == None`）：`logging.file = false`，盘上没有那一份；
//! - **读过了、日志还没内容**（`log == None` 但 `log_path` 有）：文件在、是空的。
//!
//! 失败也各记各的：查库失败不该把日志那一段一起判死，反过来也一样。

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use folio_core::infra::logging;
use folio_core::store::{Store, default_db_path};
use gpui_kit::{App, AppContext as _, Context, Entity, Global, Task, WeakEntity};

use crate::ui::services::app_store;
use crate::ui::services::config::ConfigHolder;

/// 日志尾巴读多少字节。与「反馈」那一份（256 KiB）不是同一个数：这里是把人眼看的一小段
/// 贴在设置页上，读多了只会把那一屏撑成一篇日志。
const TAIL_BYTES: u64 = 32 * 1024;
/// 尾巴里留多少行。够看见一次开书的整条链，又不至于把一页设置变成滚动条。
///
/// 这一屏不设最大高度（截在半句上比多滚一段更难读），所以行数就是唯一的闸 —— 二十行
/// 按手机上每行折两三次算，大约一屏多一点。
const TAIL_LINES: usize = 20;

/// 一次读数。
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// 库文件本体。
    pub db_bytes: u64,
    /// `-wal` 附档。WAL 模式下未检查点的写入都在这里，所以库「看起来变小了」要先看它。
    pub wal_bytes: u64,
    /// `-shm` 附档。
    pub shm_bytes: u64,
    /// 书目行数。查库失败时是 `None`（那是「没读到」，不是零本）。
    pub books: Option<usize>,
    /// 查库失败的原因，原样带给界面那一行。
    pub books_error: Option<String>,
    /// 日志文件路径；`logging.file` 关着时是 `None`。
    pub log_path: Option<PathBuf>,
    /// 日志文件总字节。
    pub log_bytes: u64,
    /// 最后 [`TAIL_LINES`] 行。文件在但空着时是 `None`。
    pub log: Option<String>,
    /// 读日志失败的原因。
    pub log_error: Option<String>,
}

#[derive(Default)]
pub struct DebugPanel {
    snapshot: Option<Snapshot>,
    busy: bool,
    /// 在飞那一趟的句柄。掉在这里等于不等结果了，所以必须留一个字段。
    _task: Option<Task<()>>,
}

impl DebugPanel {
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    /// 起一趟读数。已经在读时这一句是空转 —— 那一页的按钮按两下不该排两份活。
    ///
    /// 旧的快照**留着**直到新的回来：读失败时屏幕不该从「上次的数」变成一片空白。
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let db = default_db_path();
        let log = logging::log_file(&ConfigHolder::snapshot(cx).logging);
        let store = app_store::handle(cx);
        let work = cx.background_spawn(async move { read(&db, log, store) });
        self.busy = true;
        self._task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let snapshot = work.await;
            let _ = this.update(cx, |this, cx| {
                this.snapshot = Some(snapshot);
                this.busy = false;
                cx.notify();
            });
        }));
        cx.notify();
    }
}

/// 挂读数的 `Global`。与「存储」「反馈」同一条做法：视图建一次、每个读数处按句柄问。
pub struct DebugPanelHolder(pub Entity<DebugPanel>);

impl Global for DebugPanelHolder {}

pub fn handle(cx: &App) -> Option<Entity<DebugPanel>> {
    cx.try_global::<DebugPanelHolder>()
        .map(|holder| holder.0.clone())
}

/// 取句柄，没有就先建一个。视图与按钮回调给的 `&mut App` 都够用，所以不必等外壳先装。
pub fn entity(cx: &mut App) -> Entity<DebugPanel> {
    if let Some(entity) = handle(cx) {
        return entity;
    }
    let entity = cx.new(|_cx| DebugPanel::default());
    cx.set_global(DebugPanelHolder(entity.clone()));
    entity
}

/// 起一趟读数。拿不到实体时先建一个。
pub fn reload(cx: &mut App) {
    let entity = entity(cx);
    entity.update(cx, |panel, cx| panel.reload(cx));
}

/// 后台那一趟：三处都读，谁失败记谁的账。
fn read(db: &Path, log: Option<PathBuf>, store: Option<Arc<Store>>) -> Snapshot {
    let mut snapshot = Snapshot {
        log_path: log.clone(),
        ..Snapshot::default()
    };
    let (db_bytes, wal_bytes, shm_bytes) = sizes(db);
    snapshot.db_bytes = db_bytes;
    snapshot.wal_bytes = wal_bytes;
    snapshot.shm_bytes = shm_bytes;

    if let Some(store) = store.as_deref() {
        match store.list_books() {
            Ok(books) => snapshot.books = Some(books.len()),
            Err(error) => snapshot.books_error = Some(error.to_string()),
        }
    }

    if let Some(path) = log {
        match tail(&path) {
            Ok((lines, bytes)) => {
                snapshot.log = lines;
                snapshot.log_bytes = bytes;
            }
            Err(error) => snapshot.log_error = Some(error.to_string()),
        }
    }
    snapshot
}

/// 库本体与两条附档的字节。读不到的那一份记 0 —— 缺 `-wal` 是正常状态（刚检查点过），
/// 而「文件不存在」与「零字节」在界面上说的是同一句话，不值得分两档。
fn sizes(db: &Path) -> (u64, u64, u64) {
    let side = |suffix: &str| {
        let mut name = db.as_os_str().to_os_string();
        name.push(suffix);
        std::fs::metadata(PathBuf::from(name))
            .map(|meta| meta.len())
            .unwrap_or(0)
    };
    let main = std::fs::metadata(db).map(|meta| meta.len()).unwrap_or(0);
    (main, side("-wal"), side("-shm"))
}

/// 日志尾巴：从文件末尾倒着取 [`TAIL_BYTES`] 字节，再留最后 [`TAIL_LINES`] 行。
///
/// 从中间切进来时第一行是半截（可能还从 UTF-8 字符中间开始），所以起点不在文件头时丢掉
/// 那一行 —— 一行写了一半的日志比少一行更难读。
fn tail(path: &Path) -> std::io::Result<(Option<String>, u64)> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes)?;

    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let keep = lines.len().saturating_sub(TAIL_LINES);
    let kept = lines[keep..].join("\n");
    Ok(((!kept.is_empty()).then_some(kept), len))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use tempfile::NamedTempFile;

    use super::{TAIL_LINES, tail};

    /// 短文件整份取回：起点在文件头，第一行不许被当成「半截」丢掉。
    #[test]
    fn a_short_log_is_read_from_the_first_line() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "第一行").unwrap();
        writeln!(file, "第二行").unwrap();
        let (lines, bytes) = tail(file.path()).unwrap();
        assert_eq!(lines.as_deref(), Some("第一行\n第二行"));
        assert_eq!(bytes, "第一行\n第二行\n".len() as u64);
    }

    /// 只留最后 [`TAIL_LINES`] 行，且被截掉的那一段从中间开始时不带出半行。
    #[test]
    fn a_long_log_keeps_only_its_tail_lines() {
        let mut file = NamedTempFile::new().unwrap();
        for index in 0..=(TAIL_LINES + 20) {
            writeln!(file, "第 {index} 行 —— 后面补一串字把这一行拉长些").unwrap();
        }
        let (lines, _) = tail(file.path()).unwrap();
        let lines = lines.unwrap();
        let kept: Vec<&str> = lines.lines().collect();
        assert_eq!(kept.len(), TAIL_LINES);
        assert_eq!(
            kept.last().copied(),
            Some(format!("第 {} 行 —— 后面补一串字把这一行拉长些", TAIL_LINES + 20).as_str())
        );
    }

    /// 空文件是「读过、里面没有东西」，不是读失败。
    #[test]
    fn an_empty_log_is_none_but_not_an_error() {
        let file = NamedTempFile::new().unwrap();
        let (lines, bytes) = tail(file.path()).unwrap();
        assert!(lines.is_none());
        assert_eq!(bytes, 0);
    }
}
