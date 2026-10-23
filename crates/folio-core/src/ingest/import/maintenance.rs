//! 两趟只动库、不动来源的维护：补齐（给还没身份的行认身份，并把旧名字的副本改成哈希名）
//! 与启动时扫半成品。
//!
//! 判据是这一趟读不读**来源**：两趟都只面对库里那一行与它记的那个路径，所以都不进暂存区，
//! 也不走 `ingest` / `index`（重扫那条在 [`pipeline`](super::pipeline)）。

use std::path::Path;

use crate::content::format::Format;
use crate::infra::error::IoResultExt as _;
use crate::infra::paths;
use crate::infra::task::StageCtx;
use crate::ingest::identity;
use crate::store::BookRow;
use crate::store::Store;

/// 一趟补齐：给「还没有内容身份」的行算哈希，并把旧名字的副本改成哈希名。
///
/// 这是懒补的另一半（`plans/导入管线.md` §六 与 §一 步 5）。为什么不塞进迁移里：
/// 几百本的一次性顺序读不该压在开库那一下（`plans/基础设施.md` §1.2 第 1 条），
/// 可它又必须真的跑完 —— 不然「书库目录可以整目录 GC」「同名即同书」这些结论一直不成立。
/// 所以它是 `Cleanup` 档的活（界面空闲才跑的那一档，见 [`crate::infra::task::Priority`]），
/// 取消检查点落在书与书之间：**已经改过名的那些是真的**，剩下的一趟再来。
///
/// 四种情形分开处置，各自计数：
///
/// - **文件不在**（被删、没权限）→ 跳过。**不删行**：那条记录指向不存在的文件是
///   [`Store::stale_reason`] 早就认得出的状态，而这里没有资格替读者决定丢书。
/// - **副本躺在书库之外**（老的一就地登记那批，或读者中途改过书库位置）→ 只写身份列，
///   不动文件：那个目录不是应用自己的，搬它是越界（判据与界面的 `owned_copy` 同一条）。
/// - **目标名已经被占**（同哈希的另一行早就落过地）→ 不改名，把这一行改指那个在的位置。
///   两行共用一个文件是允许的状态，删书时由 [`Store::rows_sharing_path`] 兜住。
/// - 其余 → rename 成 `<哈希前缀>.<格式>`，回写位置与身份。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// 写进身份列的行数（含只写身份没搬文件的）。
    pub hashed: usize,
    /// 真的改了名的行数。
    pub renamed: usize,
    /// 因为目标名已在而改指别处的行数。
    pub retargeted: usize,
    /// 读不到文件 / 算不出哈希 / rename 失败而跳过的行数。
    pub skipped: usize,
}

/// 跑一趟补齐：给 `content_hash IS NULL` 的行认身份，并把书库里的旧名字副本改成哈希名。
///
/// 情形怎么分、为什么单独一趟，写在 [`Backfill_copies 的类型文档`](`BackfillReport`) 里。
/// 返回值只用于日志与测试断言 —— 这一趟没有「失败」一说：跳过的行留着 NULL，
/// 下一趟还会再来（自愈档，`plans/基础设施.md` §3.1）。
pub fn backfill_copies(store: &Store, book_dir: &Path, ctx: &StageCtx) -> BackfillReport {
    let mut report = BackfillReport::default();
    let rows = match store.rows_missing_hash() {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!("import: 补齐问不到「哪些行还没身份」：{err:#}");
            return report;
        }
    };
    let total = rows.len() as u64;
    for (done, row) in rows.iter().enumerate() {
        if ctx.cancel().triggered() {
            tracing::info!("import: 补齐被取消，已处理 {done} / {total} 行");
            break;
        }
        ctx.item(done as u64, total);
        match backfill_one(store, book_dir, row) {
            Some((renamed, retargeted)) => {
                report.hashed += 1;
                if renamed {
                    report.renamed += 1;
                }
                if retargeted {
                    report.retargeted += 1;
                }
            }
            None => report.skipped += 1,
        }
    }
    ctx.item(total, total);
    if total > 0 {
        tracing::info!(
            "import: 补齐一趟 —— 认身份 {} 行（改名 {}、改指 {}），跳过 {}",
            report.hashed,
            report.renamed,
            report.retargeted,
            report.skipped
        );
    }
    report
}

/// 一行。`Some((改过名没有, 改指过没有))` = 身份写进去了；`None` = 这一行跳过。
fn backfill_one(store: &Store, book_dir: &Path, row: &BookRow) -> Option<(bool, bool)> {
    let path = Path::new(&row.path);
    let hash = match identity::sha256_of_path(path) {
        Ok(hash) => hash,
        Err(err) => {
            tracing::debug!("import: 第 {} 本读不出内容（{path:?}）：{err}", row.id);
            return None;
        }
    };
    if let Err(err) = store.set_content_hash(row.id, &hash.to_hex(), identity::HASH_ALGO) {
        tracing::warn!("import: 第 {} 本的身份没写进去：{err:#}", row.id);
        return None;
    }
    // 书库之外的副本只写身份：搬它是越界。
    if !path.starts_with(book_dir) {
        return Some((false, false));
    }
    let dest = book_dir.join(hash.file_name(&extension_of(row, path)));
    if dest == path {
        return Some((false, false)); // 已经是哈希名了。
    }
    // 那个位置已经有行占着（`books.path` 上有 UNIQUE）：只认身份、不改名、不改指。
    // 两条记录共用一个文件在当下是**造不出来**的状态，要等「内容变了但认得出是同一本」
    // 那条更新路（§十一 已决第 2 条 + 待决第 3 条）落地才会出现；那时这一句改指才是正解，
    // 现在硬改只会撞在 UNIQUE 上。留这一支而不直接 rename，是为了不把另一行的正文盖掉。
    if dest.exists() {
        // 这一问自己失败（库锁不住、语句出错）按「有人占着」处理：不改名是安全的一侧。
        let occupied = match store.rows_sharing_path(&dest.to_string_lossy(), row.id) {
            Ok(n) => n > 0,
            Err(err) => {
                tracing::warn!(
                    "import: 第 {} 本问不出 {dest:?} 被谁占着，先不改名：{err:#}",
                    row.id
                );
                true
            }
        };
        if occupied {
            tracing::info!(
                "import: 第 {} 本的哈希名位置 {dest:?} 已被别的行占着，只认身份不改名",
                row.id
            );
            return Some((false, false));
        }
        // 文件在而没有任何行认领（上一次崩溃留下的孤儿）：这一行改指过去，白丢一份 IO。
        if let Err(err) = store.retarget_book_path(row.id, &dest) {
            tracing::warn!("import: 第 {} 本改指 {dest:?} 失败：{err:#}", row.id);
            return None;
        }
        return Some((false, true));
    }
    if let Err(err) = std::fs::rename(path, &dest).io_path(&dest) {
        tracing::warn!(
            "import: 第 {} 本的副本 {path:?} 没改成 {dest:?}：{err:#}",
            row.id
        );
        return None;
    }
    if let Err(err) = store.retarget_book_path(row.id, &dest) {
        // 名字已经改了而位置没写回：这条记录指向不存在的旧名。当场说清并把文件放回去，
        // 比留下一本「点不开」的书好。
        tracing::warn!(
            "import: 第 {} 本改了名却写不回位置：{err:#}，把副本放回 {path:?}",
            row.id
        );
        let _ = std::fs::rename(&dest, path);
        return None;
    }
    Some((true, false))
}

/// 补齐时给副本用的扩展名：库里那一行 `format` 说是哪个格式就用哪个（那是当初判定过的
/// 结论），认不出来就沿用现在这个文件的扩展名，什么都剩不下时留空（文件名就只有哈希）。
/// 返回 `String` 而不是 `&str`：两条来源的生命周期不同（一条是 `'static` 的格式名、
/// 一条借自路径），拼成一个拥有自己数据的串最省事，也最容易看清没有借错。
fn extension_of(row: &BookRow, path: &Path) -> String {
    match Format::parse(&row.format) {
        Some(format) => format.copy_extension().to_string(),
        None => path
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default(),
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
