//! 整机扫描：把设备里散落的书籍文件找出来，交一张候选清单。
//!
//! 扫描**只找不导**：家目录里 `.txt` / `.md` 多半是 README 与笔记，全量直导会把
//! 书架灌满杂物 —— 所以它交 [`ScanReport`]，由读者勾选之后走 [`crate::import`]
//! （勾选 = 读者亲手挑的，重名算更新）。监听（`crate::watch`）的初扫复用同一个
//! 遍历器，只是目标目录由读者指定、不需要清单。
//!
//! 跳过规则、预算与取消都在这里：EACCES 一律跳过并计数，绝不让一次权限不足
//! 毁掉整轮；隐藏目录与噪音清单挡住 `.git`、`Android/data` 这些明知没书的地方；
//! 深度与数量预算防一次扫描走完几十万条目录。符号链接的**目录**不跟进（环会
//! 自我循环），链接的**文件**照收（拷副本时读得到就行）。

use std::path::{Path, PathBuf};

use crate::format::supports_extension;
use crate::import::{Candidate, ImportKind, Origin, Source, stage_local};
use crate::task::StageCtx;
use crate::Result;

/// 递归深度上限。8 层在真实设备上已经深过任何「人放书的地方」，
/// 再深的目录多半是程序自己的数据。
const DEFAULT_MAX_DEPTH: usize = 8;

/// 访问条目数上限：防失控的最后一道闸。到顶就截断，报告里带 [`ScanReport::truncated`]。
const DEFAULT_MAX_VISITED: u64 = 200_000;

/// 候选数上限。超过它说明这台设备的「书」多得该让读者换个扫法，而不是全摆出来。
const DEFAULT_MAX_CANDIDATES: usize = 5_000;

/// 一轮扫描的产出。
#[derive(Debug, Default)]
pub struct ScanReport {
    /// 找到的候选，按遍历顺序（同目录的挨在一起，清单页按目录分组正好用）。
    pub candidates: Vec<Candidate>,
    /// 读不了（权限不足、竞态）的条目数。规则性跳过（隐藏、噪音、被排除）
    /// **不**计入 —— 那些是「明知没书」，不是「没看见」；界面上那句
    /// 「跳过 N 处」要说的是后者。
    pub skipped: usize,
    /// 命中预算被截断 —— 结果不完整，界面必须提示，不然读者以为扫全了。
    pub truncated: bool,
}

/// 扫描参数。测试用小预算来钉「截断」与「深度」这两条，生产用 [`Limits::default`]。
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_depth: usize,
    pub max_visited: u64,
    pub max_candidates: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
            max_visited: DEFAULT_MAX_VISITED,
            max_candidates: DEFAULT_MAX_CANDIDATES,
        }
    }
}

/// 走一遍给定的根目录，交出候选清单。多根共享同一份预算与同一张清单。
///
/// `noise_exempt` 是噪音规则的豁免区：`Android/data` 整体没书，但**本应用自己的**
/// 外部目录就在它底下（`adb push` 进来的书都在那儿），那一段要照扫。
///
/// 进度是不定进度（`item(已找到, 0)`）—— 扫描前不知道有多少书，知道的只有
/// 「已经找到几本」。取消随时生效，已找到的部分照常交回。
pub fn find(roots: &[PathBuf], exclude: &[PathBuf], noise_exempt: &[PathBuf], ctx: &StageCtx) -> ScanReport {
    find_with(roots, exclude, noise_exempt, Limits::default(), ctx)
}

/// [`find`] 的可调预算版本（测试用；生产一律 [`find`]）。
pub fn find_with(
    roots: &[PathBuf],
    exclude: &[PathBuf],
    noise_exempt: &[PathBuf],
    limits: Limits,
    ctx: &StageCtx,
) -> ScanReport {
    let mut walker = Walker {
        exclude,
        noise_exempt,
        limits,
        report: ScanReport::default(),
        visited: 0,
        found: 0,
    };
    for root in roots {
        if ctx.cancel().triggered() || walker.report.truncated {
            break;
        }
        walker.walk_dir(root, 0, ctx);
    }
    if !ctx.cancel().triggered() {
        ctx.item(walker.found, 0);
    }
    walker.report
}

/// 名字或位置明摆着没书的地方。点开头（`.git`、`.cache`、`.Trash`）由
/// 隐藏规则统一挡掉；`Android/data` 与 `Android/obb` 是别的应用的地盘，
/// 权限之外也不该翻。
fn is_noise(path: &Path, name: &str, exempt: &[PathBuf]) -> bool {
    match name {
        "node_modules" | "AppData" | "Library" | "Trash" => true,
        // `Android/{data,obb}` 整块是别的应用的地盘。豁免是**逐层**判的：
        // 自己的应用目录在 data 底下（adb push 的书都堆在那儿），所以通往
        // 豁免区的每一层都得放行，豁免区之外的分支照剪。
        _ => {
            is_under_android_private(path)
                && !exempt
                    .iter()
                    .any(|dir| path.starts_with(dir) || dir.starts_with(path))
        }
    }
}

/// `path` 是否在 `Android/data` 或 `Android/obb` 这两块私有地盘上（含其自身）。
fn is_under_android_private(path: &Path) -> bool {
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        if component.as_os_str() == "Android"
            && components
                .peek()
                .is_some_and(|next| matches!(next.as_os_str().to_str(), Some("data" | "obb")))
        {
            return true;
        }
    }
    false
}

struct Walker<'a> {
    exclude: &'a [PathBuf],
    noise_exempt: &'a [PathBuf],
    limits: Limits,
    report: ScanReport,
    visited: u64,
    found: u64,
}

impl Walker<'_> {
    fn walk_dir(&mut self, dir: &Path, depth: usize, ctx: &StageCtx) {
        if ctx.cancel().triggered() || self.report.truncated {
            return;
        }
        if depth > self.limits.max_depth || self.excluded(dir) {
            return;
        }
        // 读不了（权限、不是目录）都记一笔然后走人：一次扫描的成败不押在
        // 任何一处读不了的目录上。
        let Ok(entries) = std::fs::read_dir(dir) else {
            self.report.skipped += 1;
            return;
        };
        for entry in entries {
            if ctx.cancel().triggered() || self.report.truncated {
                return;
            }
            self.visited += 1;
            if self.visited > self.limits.max_visited {
                self.report.truncated = true;
                return;
            }
            let Ok(entry) = entry else {
                self.report.skipped += 1;
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                self.report.skipped += 1;
                continue;
            };
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if file_type.is_dir() {
                if name.starts_with('.')
                    || is_noise(&path, &name, self.noise_exempt)
                    || self.excluded(&path)
                {
                    continue;
                }
                // 目录符号链接不跟进：指向祖先的链接会自我循环。
                if !file_type.is_symlink() {
                    self.walk_dir(&path, depth + 1, ctx);
                }
            } else {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or_default();
                if !supports_extension(ext) {
                    continue;
                }
                if self.report.candidates.len() >= self.limits.max_candidates {
                    self.report.truncated = true;
                    return;
                }
                let size = std::fs::metadata(&path).ok().map(|m| m.len());
                self.report.candidates.push(Candidate {
                    origin: Origin::Local(path),
                    name,
                    size,
                });
                self.found += 1;
                // 每 25 本报一次：一次扫描几秒到几十秒，界面得看见它在动。
                if self.found.is_multiple_of(25) {
                    ctx.item(self.found, 0);
                }
            }
        }
    }

    fn excluded(&self, path: &Path) -> bool {
        self.exclude.iter().any(|dir| path.starts_with(dir))
    }
}

/// [`Source`] 形状的扫描。生产路径走 [`find`] + 清单勾选；这个实现留给
/// 「把整轮扫描的结果直接全导」的调用方（测试与将来的批处理）。
pub struct ScanSource {
    roots: Vec<PathBuf>,
    exclude: Vec<PathBuf>,
    noise_exempt: Vec<PathBuf>,
}

impl ScanSource {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            exclude: Vec::new(),
            noise_exempt: Vec::new(),
        }
    }

    /// 额外要跳过的目录（书库目录由 `candidates` 自动补上）。
    pub fn exclude(mut self, dirs: Vec<PathBuf>) -> Self {
        self.exclude = dirs;
        self
    }

    /// 噪音规则的豁免区，见 [`find`]。
    pub fn noise_exempt(mut self, dirs: Vec<PathBuf>) -> Self {
        self.noise_exempt = dirs;
        self
    }
}

impl Source for ScanSource {
    fn kind(&self) -> ImportKind {
        ImportKind::Scan
    }

    fn candidates(&self, dest_dir: &Path, ctx: &StageCtx) -> Result<Vec<Candidate>> {
        let mut exclude = self.exclude.clone();
        exclude.push(dest_dir.to_path_buf());
        Ok(find(&self.roots, &exclude, &self.noise_exempt, ctx).candidates)
    }

    fn stage(&self, candidate: &Candidate, staging_dir: &Path, ctx: &StageCtx) -> Result<PathBuf> {
        stage_local(candidate, staging_dir, ctx)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Limits, ScanSource, find_with};
    use crate::import::{ImportKind, Source as _};
    use crate::task::StageCtx;

    fn touch(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn names(report: &super::ScanReport) -> Vec<String> {
        report
            .candidates
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }

    /// 找得到深处的书；隐藏、噪音、`Android/{data,obb}`、书库目录自己都不进候选。
    #[test]
    fn finds_books_and_skips_the_known_noise() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("存储");
        touch(&root, "顶层.epub", "x");
        touch(&root, "深/一点/的/地方/深处.fb2", "x");
        touch(&root, ".隐藏目录/藏起来的.epub", "x");
        touch(&root, "node_modules/不是书.epub", "x");
        touch(&root, "Android/data/别人家/不是书.epub", "x");
        touch(&root, "Android/obb/也不是.epub", "x");
        let library = root.join("书库");
        touch(&library, "书库里的.epub", "x");
        touch(&root, "随手.txt", "正文"); // txt 认，要进候选
        touch(&root, "随手.md", "正文"); // md 认
        touch(&root, "图片.png", "png"); // 不认识的扩展名

        let ctx = StageCtx::detached();
        let report = find_with(&[root.clone()], &[library], &[], Limits::default(), &ctx);
        let mut got = names(&report);
        got.sort();
        assert_eq!(
            got,
            vec!["深处.fb2", "随手.md", "随手.txt", "顶层.epub"],
            "隐藏 / 噪音 / Android 私有目录 / 书库自己都不该进候选，实际 {got:?}"
        );
        assert_eq!(report.skipped, 0, "规则性跳过不算「读不了」");
        assert!(!report.truncated);
    }

    /// 读不了的条目记进 skipped 而不是毁掉整轮。Unix 上把一个目录的权限收干净
    /// 制造 `read_dir` 失败（以 root 跑测试的环境会无视权限，那条断言只能豁免）。
    #[cfg(unix)]
    #[test]
    fn unreadable_entries_are_counted_and_ignored() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "a.epub", "x");
        let locked = root.join("上锁的目录");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let ctx = StageCtx::detached();
        let report = find_with(&[root.to_path_buf()], &[], &[], Limits::default(), &ctx);
        assert_eq!(names(&report), vec!["a.epub"], "别的书照找");
        // root 会无视 0o000 —— 权限真生效（自己也读不了）才断言计数。
        if std::fs::read_dir(&locked).is_err() {
            assert_eq!(report.skipped, 1, "读不了的要计数");
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// 深度剪枝：更深的一层不该到。剪枝是**策略**（与噪音目录同类 ——
    /// 「人不会把书放那么深」），不是截断，`truncated` 不置位。
    #[test]
    fn depth_limit_stops_the_descent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "a/太深.epub", "x"); // 深度 1
        touch(root, "a/b/更深.epub", "x"); // 深度 2
        touch(root, "顶层.epub", "x");

        let ctx = StageCtx::detached();
        let report = find_with(
            &[root.to_path_buf()],
            &[],
            &[],
            Limits {
                max_depth: 1,
                max_visited: 1_000,
                max_candidates: 100,
            },
            &ctx,
        );
        let got = names(&report);
        assert!(got.contains(&"太深.epub".to_string()), "深度 1 的要找到");
        assert!(
            !got.contains(&"更深.epub".to_string()),
            "深度 2 的不该到，实际 {got:?}"
        );
        assert!(!report.truncated, "剪枝是策略，不是截断");
    }

    /// 候选数预算到顶：到数就停，报告要承认截断。
    #[test]
    fn candidate_limit_stops_the_walk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for n in 1..=5 {
            touch(root, &format!("凑数{n}.epub"), "x");
        }

        let ctx = StageCtx::detached();
        let report = find_with(
            &[root.to_path_buf()],
            &[],
            &[],
            Limits {
                max_depth: 8,
                max_visited: 1_000,
                max_candidates: 2,
            },
            &ctx,
        );
        assert_eq!(report.candidates.len(), 2, "候选数到顶就停");
        assert!(report.truncated, "还有 3 本没找，该承认不完整");
    }

    /// 一上来就取消：什么都不找。
    #[test]
    fn cancelling_before_the_walk_touches_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "a.epub", "x");
        let ctx = StageCtx::detached();
        ctx.cancel().trigger();
        let report = find_with(&[tmp.path().to_path_buf()], &[], &[], Limits::default(), &ctx);
        assert!(report.candidates.is_empty());
    }

    /// 噪音豁免：`Android/data` 底下被豁免的那一段照扫 —— 本应用自己的
    /// 外部目录（`adb push` 的书都堆在那儿）就在它底下。
    #[test]
    fn noise_exempt_carves_a_hole_into_android_data() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "Android/data/别家/不是书.epub", "x");
        let ours = root.join("Android/data/dev.folio.reader");
        touch(&ours, "push进来的.epub", "x");

        let ctx = StageCtx::detached();
        let report = find_with(&[root.to_path_buf()], &[], &[ours], Limits::default(), &ctx);
        assert_eq!(names(&report), vec!["push进来的.epub"]);
    }

    /// 扫描这一类来源交得出「这是扫描」这一个身份，别的立场它说了不算。
    ///
    /// 旧形状里这里断言的是 `Policy { skip_existing: true, on_name_clash: Suffix }`：
    /// 全量直导不该覆盖库里的书。那两件事现在都不再是来源的立场 —— 重扫同一本会在
    /// 判重那一步被认成同一本书（`Imported::Duplicate`），而副本名由内容哈希定，
    /// 撞不上别人那一份。两条都由 `import::tests` 钉住，这里只留 `kind`。
    #[test]
    fn scan_source_reports_its_kind() {
        assert_eq!(ScanSource::new(vec![]).kind(), ImportKind::Scan);
    }
}
