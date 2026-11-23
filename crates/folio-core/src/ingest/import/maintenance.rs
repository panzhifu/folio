//! Legacy 维护与受控资产清理。未知旧身份只报告，不据当前字节改名或重标锚点。
//! 封面补齐使用明确资产根；平铺孤儿清扫也核对全部登记版本的引用。

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::content::format::ParseBudget;

use crate::infra::paths;
use crate::infra::task::StageCtx;
use crate::ingest::identity;

use crate::store::CoverBackfillRow;
use crate::store::Store;

/// 旧 API 的兼容报告。无可靠原建锚身份时不得据当前磁盘字节认领或改名。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackfillReport {
    pub hashed: usize,
    pub renamed: usize,
    pub retargeted: usize,
    pub skipped: usize,
}

/// 已退役的自动认身份入口：只统计待验证行。使用 prepare_legacy_revision 捕获候选，
/// 无法证明原内容身份时由用户确认；不更新哈希、不改路径、不移动字节。
pub fn backfill_copies(store: &Store, _book_dir: &Path, ctx: &StageCtx) -> BackfillReport {
    match store.rows_missing_hash() {
        Ok(rows) => {
            ctx.item(rows.len() as u64, rows.len() as u64);
            BackfillReport {
                skipped: rows.len(),
                ..Default::default()
            }
        }
        Err(error) => {
            tracing::warn!("无法统计待验证旧书目：{error}");
            BackfillReport::default()
        }
    }
}
/// 这一版「封面补齐」的记号。改了补齐的判据或解析法就往上加，老机器下次开机再跑一趟。
///
/// 存在库的 `prefs` 表里（v1 就在），不另建表、不进迁移清单 —— 迁移是「库长什么样」，
/// 而这是一趟「跑没跑过」的作业记账，改它不需要动 schema。
pub const COVER_BACKFILL_VERSION: i64 = 1;
pub(super) const COVER_BACKFILL_KEY: &str = "cover_backfill";

/// 一次翻多少行。分页是为了让取消点之间的库锁持有时间有界，也让「扫到一半退出」
/// 下次能从断点接着走；内存不是这里的约束。
pub(super) const COVER_BACKFILL_PAGE: usize = 32;
/// 每趟最多重试几条失败记录：一本坏书不该把 `Cleanup` 档一直占着。
pub(super) const COVER_BACKFILL_RETRY_PAGE: usize = 8;
/// 第 n 次失败等多久再试（秒）；超过档位就按最后一档半小时。
pub(super) const COVER_BACKFILL_BACKOFF_SECS: [i64; 4] = [30, 120, 600, 1_800];
/// 账本上限；满了先丢最早该重试的那条。宁可漏一次重试，也不让 `prefs` 里的标记无界长。
pub(super) const COVER_BACKFILL_LEDGER_CAP: usize = 512;
/// 扫到哪本书的断点：书号严格大于它的行还没看。
pub(super) const COVER_BACKFILL_CURSOR_KEY: &str = "cover_backfill.cursor";
/// 没补成的那几本及其退避时刻，存成一小段 JSON。
pub(super) const COVER_BACKFILL_LEDGER_KEY: &str = "cover_backfill.retry";

/// 封面补齐一趟的账。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CoverBackfillReport {
    /// 看过的行数（含本来就好的）。
    pub scanned: u64,
    /// 真的重新抽出来、写回 `covers/` 的行数。
    pub restored: u64,
    /// 描述在、文件也在的行，或源文件已经不在的行。
    pub skipped: u64,
    /// 源文件在、但解析 / 写盘没成的行。
    pub failed: u64,
    /// 从失败账本里到点重试的行数（无论这次成没成）。
    pub retried: u64,
    /// 这趟过后账本里还压着几条等下次重试。
    pub pending: u64,
}

/// **把丢掉的封面补回来**：v15/v16 那次升级把 `books.cover` 这个 BLOB 删了，却没有
/// 先把老行写成 `covers/` 文件；`covers/` 被清过、只导数据的备份恢复过来、以及某次
/// 重扫把 `cover_hash` 写成 NULL 的行，症状都是「架面上那张图不见了」。
///
/// 封面字节不可再生于库（v16 起库里只有两格描述），唯一来源是那份书文件，所以这一趟
/// 必须回容器读一次。它跑在 `Cleanup` 档、按 `prefs` 里的记号**整趟只扫一次**，不是
/// 每次开机都把全库解一遍 —— 与 `backfill_copies` 同一条懒补口径
///
/// 与「一次扫完」的差别在三处：
/// - 分页扫，断点记在 `cover_backfill.cursor`；被取消就停在断点上，下次接着扫。
/// - 没补成的行进失败账本，按 30 s / 2 min / 10 min / 30 min 退避重试；成功即销账。
/// - 版本记号在扫到底时才写：失败不等于没扫完，没补成的那几本由账本继续盯。
pub fn backfill_covers(store: &Store, ctx: &StageCtx) -> CoverBackfillReport {
    let mut report = CoverBackfillReport::default();
    let mut ledger = CoverBackfillLedger::load(store);
    let now = unix_seconds();
    // 记号读不出来按「没扫过」处理：多扫一趟是幂等的。
    let scanned = match store.pref_number(COVER_BACKFILL_KEY) {
        Ok(done) => done.unwrap_or(0),
        Err(err) => {
            tracing::warn!("import: 封面补齐问不到记号：{err:#}");
            0
        }
    };
    if scanned < COVER_BACKFILL_VERSION {
        scan_cover_backfill(store, ctx, &mut report, &mut ledger, now);
    }
    if !ctx.cancel().triggered() {
        retry_cover_backfill(store, ctx, &mut report, &mut ledger, now);
    }
    ledger.save(store);
    report.pending = ledger.failures.len() as u64;
    if report.restored > 0 {
        // 架面把「没有封面」也缓存了，得让它下一帧重查一次，否则封面补回来了还画占位。
        crate::bump_cover_epoch();
    }
    if report.restored > 0 || report.failed > 0 || report.retried > 0 {
        tracing::info!(
            "import: 封面补齐一趟 —— 看过 {} 本，补回 {}，跳过 {}，没补成 {}；重试 {} 条，账本还欠 {} 条",
            report.scanned,
            report.restored,
            report.skipped,
            report.failed,
            report.retried,
            report.pending
        );
    }
    report
}

/// 从断点一页一页往后扫；全部扫到底才写版本记号。
fn scan_cover_backfill(
    store: &Store,
    ctx: &StageCtx,
    report: &mut CoverBackfillReport,
    ledger: &mut CoverBackfillLedger,
    now: i64,
) {
    let mut cursor = match store.pref_number(COVER_BACKFILL_CURSOR_KEY) {
        Ok(cursor) => cursor.unwrap_or(0),
        Err(err) => {
            tracing::warn!("import: 封面补齐问不到断点，从头扫：{err:#}");
            0
        }
    };
    loop {
        let rows = match store.cover_backfill_page(cursor, COVER_BACKFILL_PAGE) {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!("import: 封面补齐翻不动下一页：{err:#}");
                return;
            }
        };
        if rows.is_empty() {
            break;
        }
        let short = rows.len() < COVER_BACKFILL_PAGE;
        for row in &rows {
            if ctx.cancel().triggered() {
                // 断点停在上一本处理完的位置；账本一起存，下次从这儿接着扫。
                save_cover_cursor(store, cursor);
                tracing::info!("import: 封面补齐被取消，停在第 {cursor} 本之后");
                return;
            }
            report.scanned += 1;
            ctx.item(report.scanned, 0);
            match restore_cover(store, row) {
                RestoreCover::Already | RestoreCover::Unavailable => report.skipped += 1,
                RestoreCover::Restored => {
                    report.restored += 1;
                    ledger.clear(row.id);
                }
                RestoreCover::Failed => {
                    report.failed += 1;
                    ledger.note_failure(row.id, now);
                }
            }
            cursor = row.id;
        }
        // 每页存一次断点：进程被杀也只丢这一页的进度。
        save_cover_cursor(store, cursor);
        if short {
            break;
        }
    }
    if let Err(err) = store.set_pref_number(COVER_BACKFILL_KEY, COVER_BACKFILL_VERSION) {
        // 记号没写上，下次会再扫一趟；已补好的被「文件在」挡掉，不会重复写盘。
        tracing::warn!("import: 封面补齐的记号没写上：{err:#}");
    }
    save_cover_cursor(store, 0);
}

/// 从失败账本里取有界几条到点重试；补成了销账，仍没补成的按新档位再等。
fn retry_cover_backfill(
    store: &Store,
    ctx: &StageCtx,
    report: &mut CoverBackfillReport,
    ledger: &mut CoverBackfillLedger,
    now: i64,
) {
    let due = ledger.due(now, COVER_BACKFILL_RETRY_PAGE);
    if due.is_empty() {
        return;
    }
    let rows = match store.cover_backfill_rows(&due) {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!("import: 封面补齐取不到重试的行：{err:#}");
            return;
        }
    };
    for row in &rows {
        if ctx.cancel().triggered() {
            return;
        }
        report.retried += 1;
        match restore_cover(store, row) {
            RestoreCover::Failed => ledger.note_failure(row.id, now),
            RestoreCover::Restored => {
                report.restored += 1;
                ledger.clear(row.id);
            }
            RestoreCover::Already | RestoreCover::Unavailable => ledger.clear(row.id),
        }
    }
    // 账本里有、库里已经没有这行的（上次扫过之后删了书）：直接销账，不然永远等不到。
    let seen: HashSet<i64> = rows.iter().map(|row| row.id).collect();
    for id in due {
        if !seen.contains(&id) {
            ledger.clear(id);
        }
    }
}

fn save_cover_cursor(store: &Store, cursor: i64) {
    if let Err(err) = store.set_pref_number(COVER_BACKFILL_CURSOR_KEY, cursor) {
        tracing::warn!("import: 封面补齐的断点没写上：{err:#}");
    }
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// 一次失败的退避档，按连续失败次数取；档用尽后一直按最后一档等。
fn cover_backfill_backoff(failures: u32) -> i64 {
    let index = usize::try_from(failures.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(COVER_BACKFILL_BACKOFF_SECS.len() - 1);
    COVER_BACKFILL_BACKOFF_SECS[index]
}

/// 一条没补成的记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CoverBackfillFailure {
    /// 书号。
    id: i64,
    /// 连续失败次数，决定退避档。
    failures: u32,
    /// 下次可以重试的 Unix 秒。
    retry_at: i64,
}

/// 失败账本：只记 `restore_cover` 回 `Failed` 的行。
///
/// 账本存 `prefs`，不建表：它是补扫作业的临时记账，丢了最多重扫一趟（幂等），
/// 不值得为它加迁移。落盘形状就是一个顶层数组，旧版本读不懂就按空账本重来。
#[derive(Debug, Default)]
struct CoverBackfillLedger {
    failures: Vec<CoverBackfillFailure>,
}

impl CoverBackfillLedger {
    fn load(store: &Store) -> Self {
        match store.pref_text(COVER_BACKFILL_LEDGER_KEY) {
            Ok(Some(text)) => match serde_json::from_str::<Vec<CoverBackfillFailure>>(&text) {
                Ok(failures) => Self { failures },
                Err(err) => {
                    tracing::warn!("import: 封面补齐的失败账本读不动，按空账本重来：{err:#}");
                    Self::default()
                }
            },
            Ok(None) => Self::default(),
            Err(err) => {
                tracing::warn!("import: 封面补齐问不到失败账本：{err:#}");
                Self::default()
            }
        }
    }

    fn save(&self, store: &Store) {
        let Ok(text) = serde_json::to_string(&self.failures) else {
            return;
        };
        if let Err(err) = store.set_pref_text(COVER_BACKFILL_LEDGER_KEY, &text) {
            tracing::warn!("import: 封面补齐的失败账本没写上：{err:#}");
        }
    }

    /// 销账：补成了、或已确认没得补（源文件不在 / 描述与文件都在）。
    fn clear(&mut self, id: i64) {
        self.failures.retain(|failure| failure.id != id);
    }

    /// 记一次失败；同一本连败就往后推，不同本按各自次数取档。
    fn note_failure(&mut self, id: i64, now: i64) {
        if let Some(failure) = self.failures.iter_mut().find(|failure| failure.id == id) {
            failure.failures = failure.failures.saturating_add(1);
            failure.retry_at = now.saturating_add(cover_backfill_backoff(failure.failures));
            return;
        }
        self.failures.push(CoverBackfillFailure {
            id,
            failures: 1,
            retry_at: now.saturating_add(cover_backfill_backoff(1)),
        });
        if self.failures.len() > COVER_BACKFILL_LEDGER_CAP
            && let Some(index) = self
                .failures
                .iter()
                .enumerate()
                .min_by_key(|(_, failure)| failure.retry_at)
                .map(|(index, _)| index)
        {
            // 丢最早该重试的那条：它多半是反复失败的老书，留着只会一直占位。
            self.failures.remove(index);
        }
    }

    /// 到点该重试的书号，最早的先来；一趟只给有界条数。
    fn due(&self, now: i64, limit: usize) -> Vec<i64> {
        let mut due: Vec<&CoverBackfillFailure> = self
            .failures
            .iter()
            .filter(|failure| failure.retry_at <= now)
            .collect();
        due.sort_by_key(|failure| (failure.retry_at, failure.id));
        due.into_iter()
            .take(limit)
            .map(|failure| failure.id)
            .collect()
    }
}

enum RestoreCover {
    /// 描述在、`covers/` 里文件也在。
    Already,
    /// 重新抽出来写回去了。
    Restored,
    /// 源文件不在 / 这本书本来就没封面，没得补。
    Unavailable,
    /// 源文件在，但解析或写盘失败。
    Failed,
}

fn restore_cover(store: &Store, row: &CoverBackfillRow) -> RestoreCover {
    let root = match store.book_asset_root(row.id) {
        Ok(Some(root)) => root,
        _ => return RestoreCover::Failed,
    };
    // 两格都有、且拼出来的文件真的在 → 这一本没事。
    if let (Some(hash), Some(mime)) = (row.cover_hash.as_deref(), row.cover_mime.as_deref())
        && !hash.is_empty()
        && !mime.is_empty()
        && crate::content::assets::cover::resolve(&root, hash, mime).is_some()
    {
        return RestoreCover::Already;
    }
    let path = Path::new(&row.path);
    if !path.is_file() {
        return RestoreCover::Unavailable;
    }
    // 带口令的 PDF 少这一格就解不出首页；口令按整本记，本机自持。
    let passphrase = store.passphrase_for_book(row.id).ok().flatten();
    let document = match crate::content::document::open_with(
        path,
        // 后台清理档一次只处理一本，别为一本封面把整机核数都占上。
        ParseBudget::AtMost(1),
        passphrase,
    ) {
        Ok(document) => document,
        Err(err) => {
            tracing::debug!(book_id = row.id, "import: 封面补齐读不动这本书：{err:#}");
            return RestoreCover::Failed;
        }
    };
    let Some(bytes) = document.cover() else {
        return RestoreCover::Unavailable;
    };
    if bytes.is_empty() {
        return RestoreCover::Unavailable;
    }
    let Some(cover) = super::pipeline::write_cover(&root, bytes) else {
        return RestoreCover::Failed;
    };
    match store.set_cover_ref(row.id, &cover.hash, &cover.mime) {
        Ok(()) => RestoreCover::Restored,
        Err(err) => {
            tracing::warn!(book_id = row.id, "import: 封面补齐写不回描述：{err:#}");
            RestoreCover::Failed
        }
    }
}

/// 扫掉上一次留下的半成品。
///
/// `.{名字}.part` 这种临时名在正常路径上要么被 rename 成正式副本，要么被 `abort` /
/// `pipeline` 里那个 `abandon` 删掉，只有进程被杀才会留在原地。清两处：暂存区 `<书库>/.staging` 是
/// 现在的去处，书库根也照扫 —— 改成这个形状之前，半成品就留在根上。
///
/// 返回清掉几个。目录读不了就返回 0：这是启动路径上的自愈动作，失败不该让程序起不来
/// （级别按「自愈」那一档走）。
/// 两个区的名字都以点开头，而 `scan` 那条「以点开头的目录跳过」顺带保证了半成品
/// 永远不会被当成一本书 —— 这一条不靠这里删得干净。
pub fn sweep_partials(book_dir: &Path) -> usize {
    sweep_partials_in(book_dir) + empty_dir(&paths::staging_dir(book_dir))
}

/// 崩溃在「rename 进书库根 → `add_book`」之间会留下的一份**哈希名副本**，而库里没有
/// 对应的行：这一趟把它收掉，返回删掉几份。
///
/// 为什么需要它：导入那一段的磁盘动作是「解析 → 改名进书库根 → 落行」。改名是原子的，
/// 但改名与落行之间不是 —— 进程在这两步之间被杀，书库根上就多一份没人认领的副本。
/// 它不影响正确性（书库里没有行，谁也打不开它），但会永远占着那一份字节；而且下一次
/// 导入同一本书时，落地的目标名与它相同，读者看到的是「又占了一份」。
///
/// **只认我们自己的命名**：`<16 位 base32>.<已注册扩展名>`（[`identity::is_hash_stem`]
/// 与 provider 注册表各答一半，与写侧同源）。读者自己放进来、恰好叫
/// `我自己的书.epub` 的那一份不在这个形状里，一根汗毛都不动。
///
/// 认领按**文件名**而不是完整路径：书库位置可能在两次启动之间被改过，同一个副本换个
/// 写法还是同一份；而哈希名按内容唯一，同名即同一份字节。这样最坏也只是**少删**一份
/// （另一个目录里有同名副本），不会误删正在用的那一份。
///
/// 一小时的宽限是给**第二个实例**的：本进程在启动时没有任何导入在飞，但同一份书库可能
/// 正被另一个进程导入（它正停在改名之后、落行之前的窗口里）。崩溃留下的那一份要等到
/// 再下一次启动才收掉，这是刻意的取舍 —— 多留一份文件没有可见后果，删错一份正在用的
/// 副本是另一回事。
pub fn sweep_orphan_copies(store: &Store, book_dir: &Path) -> usize {
    const ORPHAN_GRACE: Duration = Duration::from_secs(60 * 60);

    // 问不出「哪些副本有人认领」就不动：少删一次永远比删错安全。
    let claimed: HashSet<String> = match store.list_books() {
        Ok(books) => {
            let mut paths = Vec::new();
            for row in books {
                let revisions = match store.revisions(row.id) {
                    Ok(revisions) => revisions,
                    Err(error) => {
                        tracing::warn!("无法核对历史版本引用，停止孤儿清理：{error}");
                        return 0;
                    }
                };
                paths.push(row.path);
                paths.extend(revisions.into_iter().map(|r| r.managed_path));
            }
            paths
                .into_iter()
                .filter_map(|path| {
                    Path::new(&path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .collect()
        }
        Err(err) => {
            tracing::warn!("import: 问不出「哪些副本有人认领」，这一趟不清孤儿副本：{err:#}");
            return 0;
        }
    };
    let Ok(entries) = std::fs::read_dir(book_dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        // 只碰书库根上**普通文件**：目录、符号链接一律不认。
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !is_copy_name(&name) || claimed.contains(name.as_ref()) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= ORPHAN_GRACE);
        if !old {
            tracing::debug!("import: {name} 没人认领，但还新，先留着");
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => {
                removed += 1;
                tracing::info!("import: 清掉没人认领的副本 {name}");
            }
            Err(err) => tracing::warn!("import: 没人认领的副本 {name} 没删掉：{err}"),
        }
    }
    removed
}

/// `<哈希主体>.<已注册扩展名>` —— 与 [`identity::ContentHash::file_name`] 拼出来的
/// 那一种同形。扩展名那一半问 provider 注册表，`copy_extension` 与它同源。
fn is_copy_name(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    // 写侧只产出小写扩展名（`ContentHash::file_name` 归一化过）；大写那一档留给读者自己
    // 的文件，不在这趟清理的范围内。
    identity::is_hash_stem(stem)
        && !ext.chars().any(|c| c.is_ascii_uppercase())
        && crate::content::format::supports_extension(ext)
}

/// 清一个目录里没人认领的 `.part`（**只删这一种名字**）。
///
/// 书库根上那一层放着正经副本，判据必须窄：`.{名字}.part` 是旧形状留下的半成品，
/// 别的都可能是读者自己的文件或者已经入库的一本。目录不存在（从没导过书）算 0 个
/// 而不是错误 —— 这是启动路径上的自愈动作。
fn sweep_partials_in(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let partial = name.starts_with('.') && name.ends_with(".part");
        if partial && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 把暂存区整个清空，返回清掉几项。
///
/// 与上面那条判据不同，这里**不需要挑名字**：`.staging` 按定义就是中间态的去处
/// （步 2），里面任何一样东西都不可能是权威数据。而且现在
/// 管线在里面对副本改过一次名（`readable_stem` 那一步），留下的中间文件不带 `.part`
/// 尾巴 —— 按 `.part` 挑着删就会一次次漏掉。目录本身留着，下次导入还要用。
fn empty_dir(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        // 里面可能还套着目录（一次导入留一层），整棵子树都是垃圾。
        let gone = if path.is_dir() {
            std::fs::remove_dir_all(&path).is_ok()
        } else {
            std::fs::remove_file(&path).is_ok()
        };
        if gone {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::{
        COVER_BACKFILL_BACKOFF_SECS, COVER_BACKFILL_LEDGER_CAP, CoverBackfillFailure,
        CoverBackfillLedger, cover_backfill_backoff,
    };

    fn failure(id: i64, failures: u32, retry_at: i64) -> CoverBackfillFailure {
        CoverBackfillFailure {
            id,
            failures,
            retry_at,
        }
    }

    #[test]
    fn a_failure_waits_longer_every_time_until_the_last_step() {
        assert_eq!(cover_backfill_backoff(1), 30);
        assert_eq!(cover_backfill_backoff(2), 120);
        assert_eq!(cover_backfill_backoff(3), 600);
        assert_eq!(cover_backfill_backoff(4), 1_800);
        assert_eq!(cover_backfill_backoff(9), 1_800, "档用尽后一直按最后一档等");
        assert_eq!(cover_backfill_backoff(1), COVER_BACKFILL_BACKOFF_SECS[0]);
    }

    #[test]
    fn only_failures_whose_time_has_come_are_retried_in_order() {
        let ledger = CoverBackfillLedger {
            failures: vec![failure(7, 1, 300), failure(3, 1, 100), failure(5, 2, 100)],
        };
        assert_eq!(ledger.due(99, 8), Vec::<i64>::new(), "都没到点");
        assert_eq!(ledger.due(100, 8), vec![3, 5], "先到点的先来；同一刻按书号");
        assert_eq!(ledger.due(100, 1), vec![3], "一趟只给有界条数");
        assert_eq!(ledger.due(299, 8), vec![3, 5], "300 那条要等到 300 之后");
        assert_eq!(ledger.due(400, 8), vec![3, 5, 7], "到点的都排上，仍按时刻与书号");
    }

    #[test]
    fn a_full_ledger_drops_the_earliest_retry() {
        let mut ledger = CoverBackfillLedger::default();
        for id in 0..COVER_BACKFILL_LEDGER_CAP as i64 {
            ledger.note_failure(id, 1_000 + id);
        }
        assert_eq!(ledger.failures.len(), COVER_BACKFILL_LEDGER_CAP);
        ledger.note_failure(9_999, 5_000);
        assert_eq!(ledger.failures.len(), COVER_BACKFILL_LEDGER_CAP, "账本不该无界长");
        assert!(
            ledger.failures.iter().all(|failure| failure.id != 0),
            "丢的是最早该重试的那条"
        );
    }

    #[test]
    fn a_second_failure_pushes_the_retry_out() {
        let mut ledger = CoverBackfillLedger::default();
        ledger.note_failure(4, 1_000);
        assert_eq!(ledger.failures[0].retry_at, 1_030, "第一次失败等 30 秒");
        ledger.note_failure(4, 1_010);
        assert_eq!(ledger.failures[0].failures, 2);
        assert_eq!(ledger.failures[0].retry_at, 1_130, "第二次失败等 2 分钟");
        ledger.clear(4);
        assert!(ledger.failures.is_empty(), "补成了就销账");
    }
}
