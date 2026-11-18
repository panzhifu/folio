//! Legacy 维护与受控资产清理。未知旧身份只报告，不据当前字节改名或重标锚点。
//! 封面补齐使用明确资产根；平铺孤儿清扫也核对全部登记版本的引用。

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;
use std::time::SystemTime;

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
}

/// **把丢掉的封面补回来**：v15/v16 那次升级把 `books.cover` 这个 BLOB 删了，却没有
/// 先把老行写成 `covers/` 文件；`covers/` 被清过、只导数据的备份恢复过来、以及某次
/// 重扫把 `cover_hash` 写成 NULL 的行，症状都是「架面上那张图不见了」。
///
/// 封面字节不可再生于库（v16 起库里只有两格描述），唯一来源是那份书文件，所以这一趟
/// 必须回容器读一次。它跑在 `Cleanup` 档、按 `prefs` 里的记号**整机只跑一次**，不是
/// 每次开机都把全库解一遍 —— 与 `backfill_copies` 同一条懒补口径
/// （`plans/基础设施.md` §1.2 第 1 条）。
///
/// 取消点在书与书之间，**被取消就不写记号**：已经补好的那些下次会被「文件在」挡掉，
/// 没走到的那些下次接着补。
pub fn backfill_covers(store: &Store, ctx: &StageCtx) -> CoverBackfillReport {
    let mut report = CoverBackfillReport::default();
    // 先问记号，跑过就整趟不读全库。读不出来按「没跑过」处理：多跑一趟是幂等的。
    if let Ok(Some(done)) = store.pref_number(COVER_BACKFILL_KEY)
        && done >= COVER_BACKFILL_VERSION
    {
        return report;
    }
    let rows = match store.rows_for_cover_backfill() {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!("import: 封面补齐问不到书目：{err:#}");
            return report;
        }
    };
    let total = rows.len() as u64;
    let mut cancelled = false;
    for (done, row) in rows.iter().enumerate() {
        if ctx.cancel().triggered() {
            cancelled = true;
            tracing::info!("import: 封面补齐被取消，已看 {done} / {total} 本");
            break;
        }
        ctx.item(done as u64, total);
        report.scanned += 1;
        match restore_cover(store, row) {
            RestoreCover::Already => report.skipped += 1,
            RestoreCover::Restored => report.restored += 1,
            RestoreCover::Unavailable => report.skipped += 1,
            RestoreCover::Failed => report.failed += 1,
        }
    }
    ctx.item(total, total);
    if !cancelled
        && let Err(err) = store.set_pref_number(COVER_BACKFILL_KEY, COVER_BACKFILL_VERSION)
    {
        // 记号没写上，下次会再跑一趟；已补好的被「文件在」挡掉，不会重复写盘。
        tracing::warn!("import: 封面补齐的记号没写上：{err:#}");
    }
    if report.restored > 0 {
        // 架面把「没有封面」也缓存了，得让它下一帧重查一次，否则封面补回来了还画占位。
        crate::bump_cover_epoch();
    }
    if report.restored > 0 || report.failed > 0 {
        tracing::info!(
            "import: 封面补齐一趟 —— 看过 {} 本，补回 {}，跳过 {}，没补成 {}",
            report.scanned,
            report.restored,
            report.skipped,
            report.failed
        );
    }
    report
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
/// （`plans/基础设施.md` §1.3 末条，级别按 §3.1 的「自愈」那一档走）。
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
/// （`plans/导入管线.md` §一 步 2），里面任何一样东西都不可能是权威数据。而且现在
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
