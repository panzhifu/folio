use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use super::pipeline::{Budget, MAX_IMPORT_BOOKS, import_batch, split_budget};
use super::{
    BackfillReport, Candidate, ImportKind, ImportOutcome, ImportReport, Imported, PathsSource,
    Source, backfill_copies, import_from, schedule_reindex, stage_local, submit_import,
    sweep_partials,
};
use crate::infra::metrics::{Metric, take_local};
use crate::infra::task::{StageCtx, TaskEvent, TaskManager};
use crate::store::{StaleReason, Store};

/// 从一批事件的 `Finished` 里捞出结果。
fn outcomes_of(events: Vec<TaskEvent<Vec<ImportOutcome>>>) -> Vec<ImportOutcome> {
    events
        .into_iter()
        .find_map(|e| match e {
            TaskEvent::Finished(Ok(outcomes)) => Some(outcomes),
            _ => None,
        })
        .expect("导入作业应以 Finished(Ok(..)) 收尾")
}

fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

/// 第一条结果必须是「新落地的一本」，否则把实际那一条打出来。
/// 只在「这一批就该收下这一本」的场合用；判重与拒绝的场合直接比 `outcome.result`。
fn added(outcomes: &[ImportOutcome]) -> super::Registered {
    match &outcomes[0].result {
        Ok(Imported::Added(registered)) => registered.clone(),
        other => panic!("应以 Added 收尾，实际 {other:?}"),
    }
}

/// 书库根目录（不含 `.staging` 那一层）里现在有哪些文件，按名字排。
/// 断言比的是「副本集合」而不是某个写死的名字 —— 哈希名不由测试决定，
/// 而「根上只有一份」是说得清的。
fn copies(lib: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(lib)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// 暂存区里剩几个文件。每一批结清之后它必须为空。
fn staged(lib: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(lib.join(".staging"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// URL 候选的名字：路径末段、百分号解码、查询串与锚点不算、非法字符换下划线。
#[test]
fn url_names_come_from_the_last_path_segment() {
    use super::sources::url_name;
    assert_eq!(url_name("https://example.com/books/三体.epub"), "三体.epub");
    assert_eq!(
        url_name("https://example.com/get?token=abc&file=%E4%B9%A6.epub"),
        "get",
        "查询串不算名字"
    );
    assert_eq!(url_name("https://example.com/a%20b.epub"), "a b.epub");
    assert!(
        starts_with_download(&url_name("https://example.com/")),
        "只剩根路径就给占位名"
    );
    assert_eq!(url_name("https://example.com/a:b.epub"), "a_b.epub");
}

fn starts_with_download(name: &str) -> bool {
    name.starts_with("download-")
}

/// URL 导入端到端（假传输）：下载 → 写进暂存 → 算哈希 → 落地成哈希名 → 解析入库；
/// 中途取消则半个文件都不留。
///
/// 名字这一条现在钉的是**建议名不进最终文件名**：URL 末段那个「书.txt」只用于日志与
/// 判定的扩展名线索，副本叫什么由内容说了算（§六）。所以断言比的是「父目录是书库根、
/// 扩展名来自格式判定、主名是 16 个字符的哈希前缀」。
#[test]
fn url_import_streams_into_a_hash_named_copy_and_registers() {
    use crate::net::remote::{Error as RemoteError, Response, Sink, Transport};

    /// 假传输：正文按 3 字节一块发；`cancel_after` 块之后置位取消旗标，
    /// 模拟读者在下载中途按下取消。
    struct Fixed {
        data: Vec<u8>,
        cancel_after: Option<usize>,
    }
    impl Transport for Fixed {
        fn request(
            &self,
            _req: crate::net::remote::Request,
        ) -> crate::net::remote::Result<Response> {
            Ok(Response::new(200, self.data.clone()))
        }
        fn download(
            &self,
            _req: crate::net::remote::Request,
            sink: &mut dyn Sink,
            cancel: &crate::net::remote::Cancel,
        ) -> crate::net::remote::Result<u64> {
            for (sent, chunk) in self.data.chunks(3).enumerate() {
                if Some(sent) == self.cancel_after {
                    cancel.trigger();
                }
                if cancel.triggered() {
                    sink.abort();
                    return Err(RemoteError::Cancelled);
                }
                sink.write(chunk).map_err(RemoteError::Io)?;
            }
            sink.finish().map_err(RemoteError::Io)?;
            Ok(self.data.len() as u64)
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    // 正常下载：字节进暂存、落地成哈希名，正文与书目都在。
    let source = super::UrlSource::new(
        "https://example.com/%E4%B9%A6.txt".into(),
        Arc::new(Fixed {
            data: "网络来的正文".as_bytes().to_vec(),
            cancel_after: None,
        }),
    );
    let outcomes = import_from(&source, &lib, &store, &StageCtx::detached());
    assert_eq!(outcomes.len(), 1);
    let registered = added(&outcomes);
    assert_eq!(
        registered.dest.parent(),
        Some(lib.as_ref()),
        "副本要在书库根上，实际 {:?}",
        registered.dest
    );
    let name = registered
        .dest
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    // `txt` 而不是 `Format::as_str()` 的 `text`：扩展名由 provider 注册表说了算，
    // 因为下一次认领这份副本文件的正是那张表。
    assert_eq!(
        name.rsplit_once('.').unwrap().1,
        "txt",
        "扩展名来自注册表：{name}"
    );
    assert_eq!(
        name.split('.').next().unwrap().len(),
        16,
        "主名是哈希前缀那 16 个 base32 字符：{name}"
    );
    assert!(!name.contains('书'), "建议名不许混进最终文件名：{name}");
    assert_eq!(store.shelf("").unwrap().len(), 1);
    assert_eq!(
        store.rows_missing_hash().unwrap().len(),
        0,
        "导入要把内容身份一起写进去"
    );
    assert!(
        staged(&lib).is_empty(),
        "落地之后暂存区不该留东西：{:?}",
        staged(&lib)
    );

    // 中途取消：这一本落一条 Err，不许多出一份副本、也不许留下半个临时文件，库里没有那行。
    // 比的是「根上没多东西」而不是「根是空的」—— 上面那一次成功导入的副本还在那儿。
    let before = copies(&lib);
    let store2 = Store::in_memory().unwrap();
    let source = super::UrlSource::new(
        "https://example.com/取消.epub".into(),
        Arc::new(Fixed {
            data: "永远不会写完".as_bytes().to_vec(),
            cancel_after: Some(1),
        }),
    );
    let outcomes = import_from(&source, &lib, &store2, &StageCtx::detached());
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].result.is_err(), "取消落一条 Err");
    assert!(store2.shelf("").unwrap().is_empty());
    assert_eq!(
        copies(&lib),
        before,
        "取消的那一本不许在根上留下任何东西：前 {:?} 后 {:?}",
        before,
        copies(&lib)
    );
    assert!(
        staged(&lib).is_empty(),
        "abort 要把临时文件清掉，暂存区不该有残留：{:?}",
        staged(&lib)
    );
}

/// picked 语义的端到端：好的登记上架（书目 + 章节都落库），坏的那本只留一条
/// `Err`；副本按内容命名落进书库目录，原件原封不动。
///
/// 这条门从前住在界面侧（`ui/shared/library.rs`），落库搬进管线后跟着搬过来 ——
/// 它钉的是「副本在书库目录里」「章节要跟着落库」两条契约，与是谁把文件交进来的无关。
#[test]
fn picked_files_are_registered_into_the_store() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    let a = write(&picked, "a.txt", "第一本\n");
    let b = write(&picked, "b.mobi", "后端还不认这个扩展名\n");
    let lib = tmp.path().join("books");

    let manager = TaskManager::new(1);
    let store = Arc::new(Store::in_memory().unwrap());
    let handle = submit_import(
        &manager,
        Box::new(PathsSource::picked(vec![a.clone(), b.clone()])),
        lib.clone(),
        store.clone(),
    );
    let outcomes = outcomes_of(handle.events.into_iter().collect());

    assert_eq!(outcomes.len(), 2, "两个源都要有结果");
    let landed: Vec<_> = outcomes
        .iter()
        .filter_map(|o| match &o.result {
            Ok(Imported::Added(registered)) => Some(registered),
            _ => None,
        })
        .collect();
    assert_eq!(landed.len(), 1, "好的那本要登记上，坏的不拖垮整批");
    assert!(
        landed[0].dest.starts_with(&lib),
        "入库的副本要落在书库目录里，实际 {:?}",
        landed[0].dest
    );
    assert_eq!(
        std::fs::read(&landed[0].dest).unwrap(),
        std::fs::read(&a).unwrap(),
        "拷过去的内容要和原件一模一样"
    );
    assert!(a.exists(), "导入只拷一份，原件不该被动");
    assert!(staged(&lib).is_empty(), "结清之后暂存区该是空的");

    let shelf = store.shelf("").unwrap();
    assert_eq!(shelf.len(), 1, "架上要看得见刚导进来的那一本");
    let row = store.book(shelf[0].id).unwrap().unwrap();
    assert!(
        Path::new(&row.path).starts_with(&lib),
        "库里记的是书库里的副本，实际记的是 {}",
        row.path
    );
    assert!(
        store.load_flow(shelf[0].id).unwrap().is_some(),
        "章节要跟着落库，不然点进去是一本空书"
    );
}

/// 临时副本用完即弃：source Drop 时文件和那个 `gpui-import-<uuid>` 空目录一起清掉。
/// 这条守的是 Android / iOS 选择器 cache 不再一轮轮涨。
#[test]
fn temporary_picked_files_are_removed_when_source_drops() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("gpui-import-测试");
    let file = write(&dir, "书.txt", "正文\n");
    assert!(file.exists());

    drop(PathsSource::picked_temporary(vec![file.clone()]));

    assert!(!file.exists(), "临时副本应在 source 消失时清掉");
    assert!(!dir.exists(), "只装这一份的空临时目录也该一起清掉");
}

/// 临时目录里要是有别人的东西，`remove_dir` 只该失败，不许递归带走。
#[test]
fn temporary_cleanup_keeps_siblings_and_their_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("gpui-import-测试");
    let file = write(&dir, "书.txt", "正文\n");
    let sibling = write(&dir, "别人的.txt", "别动\n");

    drop(PathsSource::picked_temporary(vec![file.clone()]));

    assert!(!file.exists(), "这一份该删");
    assert!(sibling.exists(), "同目录里别的文件不许被带走");
    assert!(dir.exists(), "目录非空时 remove_dir 失败，该留着");
}

/// 普通 `picked` 是读者原文件：Drop 之后一个字节都不许少。
#[test]
fn ordinary_picked_files_are_never_deleted_on_drop() {
    let tmp = tempfile::tempdir().unwrap();
    let file = write(tmp.path(), "读者的书.txt", "正文\n");

    drop(PathsSource::picked(vec![file.clone()]));

    assert!(file.exists(), "桌面选择器给的是原件，Drop 不许动它");
}

/// 走一次真实作业：`submit_import` 结清之后，临时副本和目录也要已经没了 ——
/// Drop 不能只活在 direct 构造的单测里。
#[test]
fn submitted_temporary_source_cleans_up_after_the_job_finishes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("gpui-import-测试");
    let file = write(&dir, "书.txt", "第一本\n");
    let lib = tmp.path().join("books");
    let manager = TaskManager::new(1);
    let store = Arc::new(Store::in_memory().unwrap());

    let handle = submit_import(
        &manager,
        Box::new(PathsSource::picked_temporary(vec![file.clone()])),
        lib,
        store,
    );
    let outcomes = outcomes_of(handle.events.into_iter().collect());

    assert!(matches!(outcomes[0].result, Ok(Imported::Added(_))));
    assert!(!file.exists(), "作业结清后临时副本该没了");
    assert!(!dir.exists(), "作业结清后临时目录也该没了");
}

/// §九 第一条门：**同内容的第二个来源** —— 不落地新文件、不新增记录，
/// 报 `Duplicate` 而不是「成功」。
///
/// 这一条是身份从路径换成哈希的全部意义：同一本书从选择器、扫描、链接各来一次，
/// 架上只该有一格、只该有一套进度与批注。两条来源故意用**不同的建议名与不同的原路径**，
/// 否则旧形状里「同路径 = 同一本」那条也能碰巧过。
#[test]
fn a_second_source_with_the_same_content_lands_no_copy_and_reports_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let first = write(&tmp.path().join("原处"), "三体.txt", "同一本书的字节\n");
    let again = write(
        &tmp.path().join("另一个地方"),
        "vol-1.txt",
        "同一本书的字节\n",
    );
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let outcomes = import_from(
        &PathsSource::picked(vec![first]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    let registered = added(&outcomes);

    let second = import_from(
        &PathsSource::new(vec![again], ImportKind::Scan),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    match &second[0].result {
        Ok(Imported::Duplicate { book_id }) => assert_eq!(
            *book_id, registered.book_id,
            "同内容要挂到已有那一行上，而不是新的一行"
        ),
        other => panic!("同内容的第二个来源要报 Duplicate，实际 {other:?}"),
    }
    assert_eq!(
        copies(&lib).len(),
        1,
        "第二个来源不许产生第二份副本：{:?}",
        copies(&lib)
    );
    assert_eq!(
        store.shelf("").unwrap().len(),
        1,
        "架上不该多出一格（多出一格就是两套进度）"
    );
    assert!(staged(&lib).is_empty(), "命中判重之后那份暂存要被收回");
}

/// §九：**同名、不同内容**是两本书 —— 两条记录、两个哈希名副本，谁也不覆盖谁。
///
/// 旧形状靠 `NameClash::Suffix` 加 `-2` 才挡住这件事，而那个后缀一旦落盘就再也说不清
/// 它是谁；现在两份内容落在两个名字上，界面上靠书名 / 作者 / 日期区分（§七 差额第 3 行
/// 「整条作废」要的就是这个形状）。
#[test]
fn same_name_different_content_lands_two_hash_named_copies() {
    let tmp = tempfile::tempdir().unwrap();
    let dir_a = tmp.path().join("甲");
    let dir_b = tmp.path().join("乙");
    let a = write(&dir_a, "同名的书.txt", "甲这一本的正文\n");
    let b = write(&dir_b, "同名的书.txt", "乙那一本完全不同的正文\n");
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let first = import_from(
        &PathsSource::picked(vec![a]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    let second = import_from(
        &PathsSource::picked(vec![b]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    let (one, two) = (added(&first), added(&second));
    assert_ne!(one.dest, two.dest, "两份内容两个去处，不许撞在同一个名字上");
    assert!(
        !one.dest.to_string_lossy().contains("-2"),
        "不要后缀改名：{}",
        one.dest.display()
    );
    assert_eq!(copies(&lib).len(), 2);
    assert_eq!(store.shelf("").unwrap().len(), 2, "两本书两格");
    assert_eq!(
        std::fs::read(&one.dest).unwrap(),
        std::fs::read(dir_a.join("同名的书.txt")).unwrap(),
        "甲那一本的内容要还在它自己的副本里"
    );
}

/// 判定不过的候选：库里没有那一行，书库根里也不许有那份副本 —— 而它**根本没进过根目录**，
/// 全程只在 `.staging` 里待过，被拒之后就从那里删掉了。
///
/// 这就是 §五 那条硬规则在「判定排在落地之前」这个形状下的样子：拒绝不再需要一次
/// 「收回已落地副本」的动作，代价只付到暂存那一层。
#[test]
fn a_rejected_candidate_leaves_neither_a_row_nor_a_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    // 扩展名我们不认（`.mobi` 在这版没有 provider），内容也不是 PalmDB。
    let bad = write(&picked, "a.mobi", "既不是 BOOKMOBI 也不是任何认识的东西\n");
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let outcomes = import_from(
        &PathsSource::picked(vec![bad]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    assert_eq!(outcomes.len(), 1);
    assert!(
        outcomes[0].result.is_err(),
        "判不出要落一条 Err，界面才报得出「跳过 1（格式不支持）」"
    );
    assert!(store.shelf("").unwrap().is_empty(), "库里不留行");
    assert_eq!(
        ImportReport::tally(&outcomes).unsupported,
        1,
        "这一本要归进「不支持」那一栏，报表才说得出为什么少了一本"
    );
    assert_eq!(copies(&lib), Vec::<String>::new(), "书库根里不许留副本");
    assert_eq!(staged(&lib), Vec::<String>::new(), "暂存区里不许留它");
}

/// 被拒的那一本**碰不到**别的书正在用的副本：拒绝发生在暂存区，而副本在用的是哈希名。
///
/// 旧形状里这条要专门防一次（`Picked` 的处置是覆盖同名文件，于是拒绝时删掉的可能是
/// 上一本正在用的那份正文），那时它钉的是「不许删」。现在这个危险**结构上不存在**：
/// 名字由内容定，两份不同的内容不可能落在同一个路径上 —— 这条测试因此改成钉住结果。
#[test]
fn a_rejected_candidate_never_touches_a_copy_in_use() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let good = write(&picked, "a.txt", "第一本\n");
    let landed = import_from(
        &PathsSource::picked(vec![good]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    let registered = added(&landed);
    let before = std::fs::read(&registered.dest).unwrap();

    // 同名、但头部含 NUL（不是文本）的那一份：会被判定拒绝。
    let same_name = write(&picked, "a.txt", "\u{0}\u{1}\u{2} not text");
    let outcomes = import_from(
        &PathsSource::picked(vec![same_name]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    assert!(
        outcomes[0].result.is_err(),
        "二进制内容要被拒，不该因为同名就收下"
    );
    assert_eq!(
        std::fs::read(&registered.dest).unwrap(),
        before,
        "库里那一行还在用这份副本，它一个字都不该变"
    );
    assert_eq!(store.shelf("").unwrap().len(), 1, "第一次导入的那行照旧在");
}

/// 一上来就取消：一个候选都不碰。
#[test]
fn cancelling_before_the_first_book_touches_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    let a = write(&picked, "a.txt", "第一本\n");
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let ctx = StageCtx::detached();
    ctx.cancel().trigger();
    let source = PathsSource::picked(vec![a]);
    let outcomes = import_from(&source, &lib, &store, &ctx);
    assert!(outcomes.is_empty(), "取消后不该再处理任何候选");
    assert!(copies(&lib).is_empty(), "副本也不该落");
}

/// 一个「**第一本** stage 完就置位取消」的假来源：取消落在什么时候由并行度决定 ——
/// 顺序那一路停在两本之间，并行那一路只剩每路各一本在飞。两条测试共用它。
struct CancelAfterFirst {
    paths: Vec<PathBuf>,
    staged: AtomicUsize,
}

impl CancelAfterFirst {
    fn new(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            staged: AtomicUsize::new(0),
        }
    }
}

impl Source for CancelAfterFirst {
    fn kind(&self) -> ImportKind {
        ImportKind::Paths
    }

    fn candidates(&self, _dest: &Path, _ctx: &StageCtx) -> crate::Result<Vec<Candidate>> {
        Ok(PathsSource::candidates_from(&self.paths))
    }

    fn stage(
        &self,
        candidate: &Candidate,
        staging_dir: &Path,
        ctx: &StageCtx,
    ) -> crate::Result<PathBuf> {
        let staged = stage_local(candidate, staging_dir, ctx)?;
        if self.staged.fetch_add(1, Ordering::SeqCst) == 0 {
            ctx.cancel().trigger();
        }
        Ok(staged)
    }
}

/// 取消落在两本**之间**：已经进去的那本是真的，后面的不再来 ——
/// 这是「架面上的取消按钮」能做的前提。
///
/// 这一条把并行度**钉成 1**：「恰好停在两本之间」只有顺序那一路给得出，而默认那档现在是
/// 按核数并行（`split_budget`），两本会在两路上一起飞。并行那一档的取消上界另有一条
/// （[`a_cancelled_parallel_batch_lands_only_what_was_already_in_flight`]）。
#[test]
fn cancelling_between_books_keeps_what_already_landed() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    let a = write(&picked, "a.txt", "第一本\n");
    let b = write(&picked, "b.txt", "第二本\n");
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let ctx = StageCtx::detached();
    let source = CancelAfterFirst::new(vec![a, b]);
    let outcomes = import_parallel(&source, &lib, &store, &ctx, 1);
    assert_eq!(outcomes.len(), 1, "第二本不该再被处理");
    assert!(matches!(outcomes[0].result, Ok(Imported::Added(_))));
    assert_eq!(store.shelf("").unwrap().len(), 1, "已进去的那本在库里");
}

/// 并行那一档的取消上界：置位之后**不再领新的号**，所以最多只剩「每路各一本」在飞，
/// 而已经落的那些都是真的 —— 界面那句「已取消」指的正是这个集合。
///
/// 与上面那条的分别是真的、不是措辞：顺序那一路能承诺「恰好停在两本之间」，并行那一路
/// 只能承诺「每路各一本以内」。这条钉的是上界（`workers`），钉不成具体的本数 —— 具体几本
/// 落在置位之前跑完，由调度决定，那种断言换台机器就红。
#[test]
fn a_cancelled_parallel_batch_lands_only_what_was_already_in_flight() {
    let workers = 3;
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    let paths: Vec<_> = (0..8)
        .map(|i| write(&picked, &format!("{i}.txt"), &format!("第 {i} 本\n")))
        .collect();
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let ctx = StageCtx::detached();
    let source = CancelAfterFirst::new(paths);
    let outcomes = import_parallel(&source, &lib, &store, &ctx, workers);

    assert!(!outcomes.is_empty(), "第一本已经领走了，该跑完而不是作废");
    assert!(
        outcomes.len() <= workers,
        "置位之后不许再领新的号：最多只剩每路各一本在飞，实际结了 {} 本",
        outcomes.len()
    );
    // 每一条 `Added` 都必须既在架上又在盘上 —— 「取消不收回已经落库的那些」。
    let shelf = store.shelf("").unwrap();
    for outcome in &outcomes {
        let Ok(Imported::Added(registered)) = &outcome.result else {
            continue;
        };
        assert!(
            shelf.iter().any(|row| row.id == registered.book_id),
            "报了 Added 的第 {} 本得在架上看得见",
            registered.book_id
        );
        assert!(registered.dest.exists(), "报了 Added 的副本得在盘上躺着");
    }
    assert_eq!(
        shelf.len(),
        outcomes.iter().filter(|o| o.result.is_ok()).count(),
        "架上的行数要正好等于结清的本数"
    );
    assert!(staged(&lib).is_empty(), "没领过的候选连暂存区都不该碰");
}

/// §九：**判重命中，但那条记录指的副本文件已经不在了** —— 不能报「已存在」把读者挡回去，
/// 而要重新落地并把那一行接回一份内容相同的文件（自愈）。
///
/// 这一条同时钉住「自愈不新增行」：同一份内容两行就是两套进度、两套批注。
#[test]
fn a_duplicate_whose_copy_is_gone_heals_the_existing_row() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    let source = write(&picked, "a.txt", "这一本的正文\n");
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let first = added(&import_from(
        &PathsSource::picked(vec![source.clone()]),
        &lib,
        &store,
        &StageCtx::detached(),
    ));

    // 读者手工删掉了书库里那份副本（那一行因此指向不存在的文件）。
    std::fs::remove_file(&first.dest).unwrap();

    let again = import_from(
        &PathsSource::picked(vec![source]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    let healed = added(&again);
    assert_eq!(
        healed.book_id, first.book_id,
        "自愈不许新增一行 —— 同一份内容两行就是两套进度"
    );
    assert_eq!(healed.dest, first.dest, "哈希名由内容定，落回原来那个位置");
    assert!(
        Path::new(&healed.dest).exists(),
        "那一行要重新指着一个存在的文件：{}",
        healed.dest.display()
    );
    assert_eq!(store.shelf("").unwrap().len(), 1, "架上还是一格");
    assert!(
        store.rows_missing_hash().unwrap().is_empty(),
        "自愈走完了整条管线，身份列也要在"
    );
}

/// 清理函数清两处、不误伤：暂存区里的 `.part` 与书库根上那种旧位置的半成品都收掉，
/// 而**副本一个都不动**（哈希名副本不带点前缀，也不以 `.part` 结尾）。
#[test]
fn sweep_clears_staging_and_legacy_partials_but_keeps_copies() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("books");
    let staging = lib.join(".staging");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join(".三体.txt.7.part"), "半截的下载").unwrap();
    // 管线在暂存区里还改过一次名（解析前那个人看得懂的名字），那种文件不带 `.part` 尾巴
    // —— 按 `.part` 挑着删就会一次次漏在硬盘上。
    std::fs::write(staging.join("三体.txt"), "改名之后被杀留下的").unwrap();
    std::fs::write(lib.join(".旧形状.part"), "上次被杀留下的").unwrap();
    std::fs::write(lib.join("4oymiquy7qobjgx3.txt"), "一本正经的副本").unwrap();

    assert_eq!(
        sweep_partials(&lib),
        3,
        "暂存区两个（一个 .part、一个改过名的中间物）、根上一个"
    );
    assert_eq!(
        copies(&lib),
        vec!["4oymiquy7qobjgx3.txt".to_string()],
        "副本不许动"
    );
    assert!(staged(&lib).is_empty());
    // 暂存区那个目录留着：下一次导入还要用它。
    assert!(staging.exists());
    // 书库根本不存在时是 0 而不是错误：启动路径上的自愈动作不该拦住开机。
    assert_eq!(
        sweep_partials(&tmp.path().join("从没建过的目录")),
        0,
        "读不到目录就当没得清"
    );
}

/// 老库那一批「旧名字副本 + 没有身份」的行：补齐一趟之后身份有了、副本改成哈希名、
/// 位置回写，而**内容一个字节都没变**。
///
/// 这一条钉的是「懒补 + 一次性改名」那两条决定合起来的形状：迁移本身不读全库（行留着
/// `content_hash IS NULL`），改名由 `Cleanup` 档这一趟做，做完记录仍然指得到正文。
#[test]
fn backfill_gives_legacy_copies_an_identity_and_a_hash_name() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("books");
    std::fs::create_dir_all(&lib).unwrap();
    // 旧形状留下的那一本：名字是建议名，库里那一行没有身份。
    let legacy = lib.join("老名字.txt");
    std::fs::write(&legacy, "老库里的正文\n").unwrap();
    let store = Store::in_memory().unwrap();
    let meta = crate::content::format::DocMeta {
        title: "老名字".into(),
        authors: vec![],
        format: crate::content::format::Format::Text,
        size: 0,
        ..crate::content::format::DocMeta::default()
    };
    let id = store.add_book(&legacy, &meta).unwrap();
    assert_eq!(store.rows_missing_hash().unwrap().len(), 1, "起点是没身份");

    let report = backfill_copies(&store, &lib, &StageCtx::detached());
    assert_eq!(
        report,
        BackfillReport {
            hashed: 1,
            renamed: 1,
            retargeted: 0,
            skipped: 0,
        },
        "该改一次名、认一次身份"
    );
    let row = store.book(id).unwrap().unwrap();
    assert_ne!(row.path, legacy.to_string_lossy(), "副本改成哈希名");
    assert!(
        row.path.ends_with(".txt"),
        "扩展名跟着库里那一行的格式、并按注册表认得的那一个写：{}",
        row.path
    );
    assert!(
        !legacy.exists() && Path::new(&row.path).exists(),
        "改完名之后旧位置不该还留着一份"
    );
    assert_eq!(
        std::fs::read_to_string(&row.path).unwrap(),
        "老库里的正文\n",
        "改名只动位置不动内容"
    );
    assert!(
        store.rows_missing_hash().unwrap().is_empty(),
        "身份列补上了"
    );

    // 补齐过的行要能被判重认出来：再导一次同一份字节报已存在，而不是第二格。
    let again = import_from(
        &PathsSource::picked(vec![Path::new(&row.path).to_path_buf()]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    assert!(
        matches!(again[0].result, Ok(Imported::Duplicate { .. })),
        "补齐过的行要能被判重认出来：{:?}",
        again[0].result
    );
}

/// 补齐不许越界：副本躺在书库之外（老的一就地登记那批，或读者中途改过书库位置）时
/// **只写身份、不动文件**。
#[test]
fn backfill_fills_the_hash_without_moving_a_file_we_do_not_own() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("books");
    std::fs::create_dir_all(&lib).unwrap();
    let else_where = tmp.path().join("读者自己的目录");
    let outside = write(&else_where, "在别处的书.txt", "正文在读者那边\n");
    let store = Store::in_memory().unwrap();
    let meta = crate::content::format::DocMeta {
        title: "在别处".into(),
        authors: vec![],
        format: crate::content::format::Format::Text,
        size: 0,
        ..crate::content::format::DocMeta::default()
    };
    let id = store.add_book(&outside, &meta).unwrap();

    let report = backfill_copies(&store, &lib, &StageCtx::detached());
    assert_eq!(
        (report.hashed, report.renamed, report.skipped),
        (1, 0, 0),
        "认了身份而没搬文件"
    );
    assert!(outside.exists(), "读者那一份一个字都不该动、更不该被搬走");
    assert_eq!(
        store.book(id).unwrap().unwrap().path,
        outside.to_string_lossy()
    );
    assert!(store.rows_missing_hash().unwrap().is_empty());
}

/// 「还有别人指着这个文件吗」这一问要问得出数。
///
/// 内容寻址之后「两行共用一个副本」是 `plans/导入管线.md` §六 明确会出现的状态，而它现在
/// 还**造不出来**：`books.path` 上有 UNIQUE，两条行不可能同时记着同一个路径。这一条测试
/// 因此钉三件事 —— 计数在没人用时是 0、改指到别的行身上会被 UNIQUE 挡回来（也就是
/// [`backfill_copies`] 里那句「只认身份不改名」为什么会存在），以及这个口子在
/// 已决第 2 条（内容变了仍认是同一本）落地那天正好用得上。
#[test]
fn a_copy_in_use_by_another_row_is_counted_and_never_overwritten() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("books");
    std::fs::create_dir_all(&lib).unwrap();
    let one = lib.join("一本.txt");
    std::fs::write(&one, "第一本的正文\n").unwrap();
    let store = Store::in_memory().unwrap();
    let meta = crate::content::format::DocMeta {
        title: "一本".into(),
        authors: vec![],
        format: crate::content::format::Format::Text,
        size: 0,
        ..crate::content::format::DocMeta::default()
    };
    let a = store.add_book(&one, &meta).unwrap();
    assert_eq!(
        store.rows_sharing_path(&one.to_string_lossy(), a).unwrap(),
        0,
        "只有自己指着它"
    );

    // 把另一行改指到同一个路径：`path` 的 UNIQUE 拦住 —— 所以「共用」当下不可能出现，
    // 而补齐作业必须先问那一句再决定改不改名。
    std::fs::write(lib.join("另一本.txt"), "另一本的正文\n").unwrap();
    let b = store.add_book(&lib.join("另一本.txt"), &meta).unwrap();
    let err = store
        .retarget_book_path(b, &one)
        .expect_err("同一路径不许被两行同时认领");
    assert!(
        format!("{err:#}").contains("UNIQUE") || format!("{err:#}").contains("constraint"),
        "拦下来的应该是那一条唯一约束，实际：{err:#}"
    );
    assert_eq!(
        store.book(b).unwrap().unwrap().path,
        lib.join("另一本.txt").to_string_lossy(),
        "撞约束那一行不该被改坏"
    );
    assert_eq!(
        std::fs::read(&one).unwrap(),
        "第一本的正文\n".as_bytes(),
        "改指失败更不许动到那份副本的内容"
    );
}

/// 落地的副本**自己还得被认出来**：按内容改名的那一步如果用了 provider 注册表不认的
/// 扩展名，这本书就变成「导进去过、再也打不开」的一具空壳 —— 名字改了而没人认得它。
///
/// 这一条是实测翻红之后补的门：`Format::as_str()` 报的是 `text` / `markdown`，
/// 而注册表认领的是 `txt` / `md`，两者混用正好踩中这一类。所以扩展名一律
/// 由 [`Format::copy_extension`] 问注册表要。
#[test]
fn a_landed_copy_is_still_recognisable_as_that_book() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    // 三种「靠扩展名认领」的代表：纯文本、Markdown、EPUB（容器另算一层判定）。
    let txt = write(&picked, "一本.txt", "正文一行\n");
    let md = write(&picked, "另一本.md", "# 标题\n\n正文\n");
    let outcomes = import_from(
        &PathsSource::picked(vec![txt, md]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    assert_eq!(
        ImportReport::tally(&outcomes).added,
        2,
        "两本都要收下：{:?}",
        outcomes.iter().map(|o| &o.result).collect::<Vec<_>>()
    );

    for name in copies(&lib) {
        let path = lib.join(&name);
        // 判定层按「自己的显示名 + 内容」判：这里给的就是副本文件名。
        let display = name.clone();
        let verdict = crate::content::format::sniff::identify(&display, &path);
        assert!(
            verdict.format().is_some(),
            "副本 {name} 认不出来了：{verdict:?}"
        );
        assert!(
            crate::content::format::provider::provider_for(&path).is_some(),
            "副本 {name} 没有 provider 认领，打开就是「不支持的书籍格式」"
        );
        // 名字的主名必须是那 16 个字符的哈希前缀，不是建议名。
        assert_eq!(name.split('.').next().unwrap().len(), 16, "{name}");
    }
}

/// 一本 `.fb2.zip` 走完整条管线：收进库、副本改名成 `<哈希>.fb2`，而**那份副本还认得自己**。
///
/// 这一条盯的是判定层与 provider 之间那条缝：判定按内容认出压缩过的 FB2，落地却按
/// `Format::Fb2` 问注册表要扩展名 —— 于是副本是一个躺在 `.fb2` 这个名字里的 ZIP。
/// 「名字仍被 Fb2 认领」与「按头部字节真的读得出正文」两件事必须同时成立，
/// 否则就是上一那条门点名的那具空壳：导进去过，再也打不开。
#[test]
fn a_zipped_fb2_lands_and_still_opens() {
    use std::io::Write as _;

    use crate::content::format::{Content, Format, provider_for, sniff};

    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    std::fs::create_dir_all(&picked).unwrap();
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
<description><title-info><book-title>压缩过的一本</book-title></title-info></description>
<body><section><title><p>第一章</p></title><p>正文</p></section></body>
</FictionBook>"#;
    let path = picked.join("一本.fb2.zip");
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("一本.fb2", opts).unwrap();
    zip.write_all(xml.as_bytes()).unwrap();
    zip.finish().unwrap();

    let outcomes = import_from(
        &PathsSource::picked(vec![path]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    let report = ImportReport::tally(&outcomes);
    assert_eq!(
        report.added,
        1,
        "压缩过的 FB2 要收下：{:?}",
        outcomes.iter().map(|o| &o.result).collect::<Vec<_>>()
    );
    assert_eq!(staged(&lib), Vec::<String>::new(), "暂存区要结清");

    let names = copies(&lib);
    assert_eq!(names.len(), 1, "书库根上只该有一份副本：{names:?}");
    let copy_name = &names[0];
    assert!(
        copy_name.ends_with(".fb2"),
        "副本的扩展名来自判定，不是来源那个 `.zip`：{copy_name}"
    );
    let copy = lib.join(copy_name);

    // **库里躺的是解出来的那一份，不是外面那个包**：这一条是「先退一层再判」的全部意义 ——
    // provider 那一边不需要认识容器，`Format` 也不用为「躺在 zip 里」多立一档。
    assert!(
        !std::fs::read(&copy).unwrap().starts_with(b"PK\x03\x04"),
        "副本 {copy_name} 还是一个包，那 provider 就读不动它"
    );

    assert_eq!(
        sniff::identify(copy_name, &copy).format(),
        Some(Format::Fb2),
        "副本 {copy_name} 过不了自己那道判定"
    );
    let provider = provider_for(&copy).expect("副本该有 provider 认领");
    let (meta, content) = provider.read(&copy).expect("副本要读得开");
    assert_eq!(meta.title, "压缩过的一本", "书名取自那串 XML，不是哈希名");
    let Content::Reflow(flow) = content else {
        panic!("FB2 该读成可重排")
    };
    assert_eq!(flow.chapters.len(), 1);
    assert!(
        flow.chapters[0].source().contains("正文"),
        "{}",
        flow.chapters[0].source()
    );
}

/// 一条 FB2 的 XML，书名由参数定 —— 下面那两条门都要在同一个包里放两本**不同**的书。
fn fb2_xml(title: &str) -> String {
    format!(
        r#"<FictionBook><description><title-info><book-title>{title}</book-title></title-info></description><body><section><p>正文</p></section></body></FictionBook>"#
    )
}

/// 一个真 zip：`(条目名, 内容)`。
fn pack(dir: &Path, name: &str, entries: &[(&str, &str)]) -> PathBuf {
    use std::io::Write as _;
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    for (entry, body) in entries {
        zip.start_file(*entry, opts).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    path
}

/// 一个真 RAR：`(条目名, 内容)`。写入口只在测试里开着。
fn pack_rar(dir: &Path, name: &str, entries: &[(&str, &str)]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    let mut builder = rars::Builder::new(rars::ArchiveVersion::Rar50);
    for (entry, body) in entries {
        builder
            .add_bytes(
                entry.as_bytes().to_vec(),
                body.as_bytes().to_vec(),
                None,
                None,
            )
            .unwrap();
    }
    builder.write_to_path(&path, None).unwrap();
    path
}

/// 一个真 7z：压缩器的入口是「把目录压进去」，所以先在临时目录里摆一份。
fn pack_sevenz(dir: &Path, name: &str, entries: &[(&str, &str)]) -> PathBuf {
    let src = dir.join(format!("{name}-src"));
    std::fs::create_dir_all(&src).unwrap();
    for (entry, body) in entries {
        std::fs::write(src.join(entry), body).unwrap();
    }
    let path = dir.join(name);
    sevenz_rust2::compress_to_path(&src, &path).unwrap();
    path
}

/// 一个真 tar：条目名与字节都由调用方给。
fn pack_tar(dir: &Path, name: &str, entries: &[(&str, &str)]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    let file = std::fs::File::create(&path).unwrap();
    let mut builder = tar::Builder::new(file);
    for (entry, body) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, entry, std::io::Cursor::new(body.as_bytes()))
            .unwrap();
    }
    builder.finish().unwrap();
    path
}

/// ZIP 之外的三种归档走同一条退层的路：一个候选交回两本书，副本是解出来的 FB2。
#[test]
fn rar_7z_and_tar_bundles_land_every_book_they_hold() {
    type Packer = fn(&Path, &str, &[(&str, &str)]) -> PathBuf;
    let packers: [(&str, Packer); 3] = [
        ("合集.rar", pack_rar),
        ("全集.7z", pack_sevenz),
        ("全集.tar", pack_tar),
    ];
    for (name, build) in packers {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("books");
        let store = Store::in_memory().unwrap();
        let path = build(
            &tmp.path().join("原处"),
            name,
            &[
                ("01-上卷.fb2", &fb2_xml("上卷")),
                ("02-下卷.fb2", &fb2_xml("下卷")),
            ],
        );

        let outcomes = import_from(
            &PathsSource::picked(vec![path]),
            &lib,
            &store,
            &StageCtx::detached(),
        );
        let report = ImportReport::tally(&outcomes);
        assert_eq!(
            report.added,
            2,
            "{name} 里的两本都要落地：{:?}",
            outcomes.iter().map(|o| &o.result).collect::<Vec<_>>()
        );
        assert_eq!(staged(&lib), Vec::<String>::new(), "{name}：暂存区要结清");

        let names = copies(&lib);
        assert_eq!(names.len(), 2, "{name}：书库根上两份副本：{names:?}");
        for copy_name in names {
            assert!(
                copy_name.ends_with(".fb2"),
                "{name}：副本的扩展名来自判定，不是外层那个归档：{copy_name}"
            );
            let copy = lib.join(&copy_name);
            assert!(
                !std::fs::read(&copy).unwrap().starts_with(b"PK\x03\x04"),
                "{name}：副本 {copy_name} 还是外层那个包"
            );
        }
    }
}

/// ZIP 里的条目用 bzip2 压：整条路（退层 → 落地 → 再判定）都要通。
/// 这一条把 Cargo feature 与 `archive::extract_to` 的 ZIP 那一支串起来。
#[test]
fn a_zip_bundle_with_bzip2_entries_lands_and_still_opens() {
    use std::io::Write as _;

    use crate::content::format::{Content, Format, provider_for, sniff};

    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    std::fs::create_dir_all(&picked).unwrap();
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let path = picked.join("合集.zip");
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Bzip2);
    zip.start_file("一本.fb2", opts).unwrap();
    zip.write_all(fb2_xml("bzip2 包着的一本").as_bytes())
        .unwrap();
    zip.finish().unwrap();

    let outcomes = import_from(
        &PathsSource::picked(vec![path]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    let report = ImportReport::tally(&outcomes);
    assert_eq!(
        report.added,
        1,
        "bzip2 条目要抽得出来：{:?}",
        outcomes.iter().map(|o| &o.result).collect::<Vec<_>>()
    );
    assert_eq!(staged(&lib), Vec::<String>::new(), "暂存区要结清");

    let names = copies(&lib);
    assert_eq!(names.len(), 1);
    let copy = lib.join(&names[0]);
    assert_eq!(
        sniff::identify(&names[0], &copy).format(),
        Some(Format::Fb2)
    );
    let provider = provider_for(&copy).expect("副本该有 provider 认领");
    let (meta, content) = provider.read(&copy).expect("副本要读得开");
    assert_eq!(meta.title, "bzip2 包着的一本");
    assert!(matches!(content, Content::Reflow(_)));
}

/// 一个装着两条正文的包：**一个候选交回两本书**，各带自己那一个名字。
///
/// 这是「逐条各判一次」那条规则的门 —— 报表与架面都要按本数说话，而不是按候选数。
#[test]
fn one_pack_lands_every_book_it_holds() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();
    let pack = pack(
        &picked,
        "合集.zip",
        &[
            ("上卷.fb2", &fb2_xml("上卷")),
            ("下卷.fb2", &fb2_xml("下卷")),
        ],
    );

    let outcomes = import_from(
        &PathsSource::picked(vec![pack]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    let report = ImportReport::tally(&outcomes);
    assert_eq!(
        report.added,
        2,
        "一个包里两本就进两本：{:?}",
        outcomes.iter().map(|o| &o.result).collect::<Vec<_>>()
    );
    assert_eq!(copies(&lib).len(), 2, "两本各一份副本");
    assert_eq!(
        staged(&lib),
        Vec::<String>::new(),
        "外面那一层与半成品都要结清"
    );
    // 每一条顶的是**条目自己**那个名字，不是那个包名：哪一本没进来要说得出是哪一条。
    let given: Vec<String> = outcomes.iter().map(|o| o.candidate.name.clone()).collect();
    assert_eq!(given, vec!["上卷.fb2", "下卷.fb2"], "{given:?}");
    // 架面上两本都在。这里比的是集合而不是顺序 —— `shelf` 那一头的排序由它自己定，
    // 这一条要钉的是「一个包进了两本书」，不是架面怎么摆。
    let mut titles: Vec<String> = store
        .shelf("")
        .unwrap()
        .into_iter()
        .map(|item| item.title)
        .collect();
    titles.sort();
    assert_eq!(titles, vec!["上卷", "下卷"], "{titles:?}");
}

/// 同一本书裸着送来与打包着送来是**同一本**：退那一层排在算身份之前，
/// 所以哈希算的是解出来的那一份字节。
///
/// 这一条钉的是一个顺序，不是一句愿望：先哈希后退层，同一份内容就会因为外面裹没裹
/// 一层 zip 而得两个哈希，架面上就是两本一模一样的书、两套进度。
#[test]
fn the_same_book_bare_and_packed_is_one_book() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();
    let xml = fb2_xml("同一本");
    let bare = write(&picked, "裸的.fb2", &xml);
    let packed = pack(&picked, "包的.fb2.zip", &[("里面.fb2", &xml)]);

    let outcomes = import_from(
        &PathsSource::picked(vec![bare, packed]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    let report = ImportReport::tally(&outcomes);
    assert_eq!(report.added, 1, "先到的那一本进库：{report:?}");
    assert_eq!(
        report.duplicate,
        1,
        "同一份字节第二次到来算已存在：{:?}",
        outcomes.iter().map(|o| &o.result).collect::<Vec<_>>()
    );
    assert_eq!(copies(&lib).len(), 1, "书库根上只该有一份字节");
}

/// 架子上不许出现哈希串：**书名与章名要跟着来源给的那个名字走**。
///
/// 单文件格式（TXT、无标题行的 Markdown）的标题是文件主名回落出来的
/// （书名与章名各一处，住在 `format::text` 与 `format::markdown` 那两族的
/// `title` / `split` 里），而副本按内容命名之后主名就是一串 base32 ——
/// 不接「解析前换一个看得懂的名字」那一步，每一本 TXT 在架上就叫
/// `tuqddpm7nrheuceu`。这一条是哈希名接上之后实测翻出来的，所以钉成门。
/// 反面也要钉：书里真写了标题（Markdown 的一级标题）时不许被建议名盖掉。
#[test]
fn a_plain_text_book_is_titled_by_its_source_name_not_by_its_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    let txt = write(
        &picked,
        "蛇结.txt",
        "没有标题行，正文就这样开始。\n第二行。\n",
    );
    let md = write(&picked, "随便起的文件名.md", "# 真正的书名\n\n正文。\n");
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let outcomes = import_from(
        &PathsSource::picked(vec![txt, md]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    assert_eq!(ImportReport::tally(&outcomes).added, 2);

    let titles: Vec<String> = store
        .shelf("")
        .unwrap()
        .into_iter()
        .map(|item| item.title)
        .collect();
    assert!(
        titles.contains(&"蛇结".to_string()),
        "TXT 的书名要用来源给的那个名字，实际 {titles:?}"
    );
    assert!(
        titles.contains(&"真正的书名".to_string()),
        "Markdown 自己写了标题就用它自己的，建议名不许盖上去：{titles:?}"
    );
    // 「全是小写字母与数字、正好 16 个」就是那串 base32 前缀的形状，别的都算正常书名。
    let looks_like_hash = |t: &str| {
        t.len() == 16
            && t.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    };
    for title in &titles {
        assert!(!looks_like_hash(title), "书名里不该是一串哈希前缀：{title}");
    }

    // 第一章名同理：TXT 那一本只有一章，章名也就是那个回落值。
    let plain = store
        .shelf("")
        .unwrap()
        .into_iter()
        .find(|item| item.title == "蛇结")
        .expect("刚导的那本 TXT");
    let flow = store.load_flow(plain.id).unwrap().expect("章节表在");
    assert_eq!(
        flow.chapters[0].title, "蛇结",
        "目录/章名也不许是哈希串：{}",
        flow.chapters[0].title
    );
}

/// 报表分类计数：一批里成功 / 已存在 / 各类拒绝要分得开，而**认不出类别的错误不许
/// 被悄悄少算**（`plans/基础设施.md` §3.2 那条「批量作业要出报表」的最小一半）。
#[test]
fn a_batch_tallies_into_a_report_with_rejects() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    let good = write(&picked, "好的一本.txt", "正文\n");
    let bad = write(&picked, "杂物.pdf", "%PDF-1.7 这一版还读不动\n");
    let junk = write(&picked, "不是书.dat", "\u{0}\u{1}\u{2}");
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let outcomes = import_from(
        &PathsSource::picked(vec![good, bad, junk]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    let report = ImportReport::tally(&outcomes);
    assert_eq!(report.added, 1, "收下一本");
    assert_eq!(
        report.rejected(),
        2,
        "两本都要算进拒绝，不许悄悄少算：{report:?}"
    );
    // 「杂物.pdf」只有一个 `%PDF` 头，没有对象表也没有 trailer。**它现在不再落进「不支持」
    // 那一栏**：PDF 有 provider 了，判定层放行，失败发生在步 6 的解析那一步 ——
    // 报表说的是「读不动」（重导一次 / 换个文件），而不是「这一版还不支持」（等版本）。
    // 那一格从 2 变 1 正是 PDF 落地带来的读数变化，留着这条门就是为了记下它。
    assert_eq!(
        report.unsupported, 1,
        "二进制不是书 = 「不支持」那一栏；PDF 不再属于这里"
    );
    assert_eq!(
        report.unreadable, 1,
        "只有头的 PDF 该算「读不动」：{report:?}"
    );
    assert_eq!(report.duplicate, 0);
    assert_eq!(report.corrupt + report.encrypted, 0, "这一批里没有那两档");
    assert!(
        report.summary().contains("成功 1"),
        "那句总结要说得出数：{}",
        report.summary()
    );
}

/// 一本 DRM 加密的 EPUB：容器与判定都放行，读到 `META-INF/encryption.xml` 才知道解不开。
///
/// 这一条钉的是三件事，缺一件就算没接上：
///
/// 1. **拒**，不是收下来画一整章乱码（那就是加这一条之前实际在发生的事）；
/// 2. 报表里算**加密**那一栏，不是「读不动」—— 后者叫读者重新导入一次，前者要他换一本
///    无加密的书，是三个动作里唯一现在就该说清的那一个；
/// 3. 库里**一行都不留**（`plans/导入管线.md` §五那条硬规则：判定或解析失败的候选
///    最终不能在库里留下记录），书库根也没见过它。
#[test]
fn an_encrypted_epub_is_refused_and_leaves_no_library_record() {
    use std::io::Write as _;

    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    std::fs::create_dir_all(&picked).unwrap();
    let path = picked.join("借来的书.epub");
    {
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        let mut entry = |name: &str, body: &str| {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        };
        entry("mimetype", "application/epub+zip");
        entry(
            "META-INF/container.xml",
            r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
        );
        entry(
            "META-INF/encryption.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <EncryptedData><EncryptionMethod Algorithm="http://www.adobe.com/DRM/method/2008.09.adbe.xmldsig-hmac#sha256"/></EncryptedData>
</encryption>"#,
        );
        entry(
            "OEBPS/content.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>借来的书</dc:title></metadata>
  <manifest><item id="c1" href="Text/chap1.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#,
        );
        entry(
            "OEBPS/Text/chap1.xhtml",
            "<html><body><p>这一段其实是密文</p></body></html>",
        );
        zip.finish().unwrap();
    }

    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();
    let outcomes = import_from(
        &PathsSource::picked(vec![path]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    let report = ImportReport::tally(&outcomes);
    assert_eq!(report.added, 0, "加密书不该变成架上的一格");
    assert_eq!(
        report.encrypted, 1,
        "报表要说得出「这是加密的」：{report:?}"
    );
    assert_eq!(
        report.unreadable, 0,
        "不许压成「读不动」那一栏（那一句是叫读者重新导入）：{report:?}"
    );
    assert!(
        report.summary().contains("加密 1"),
        "那句总结要点出加密：{}",
        report.summary()
    );
    assert!(
        store.shelf("").unwrap().is_empty(),
        "§五：判定或解析失败的候选不能在库里留下记录"
    );
    // 书库根与暂存区都从没见过它：拒绝发生在暂存区，那份字节已经跟着 `abandon` 走了。
    // 问的是「根上有没有**文件**」而不是「根目录空不空」：`.staging` 那一个目录从第 2 步
    // 起就立在根下，按「非空」判会把每一个被拒的候选都算成落过地（helper 在本文件上面）。
    assert!(
        copies(&lib).is_empty(),
        "书库根里不许有这一本的副本：{:?}",
        copies(&lib)
    );
    assert!(
        staged(&lib).is_empty(),
        "暂存区那份也该跟着 abandon 走：{:?}",
        staged(&lib)
    );
}

// ---- 重扫的线程归属（`plans/阅读管线.md` §七：开书那一屏不等重扫）----

/// 一张纯色 PNG，用来当 CBZ 的一页。
fn png(w: u32, h: u32) -> Vec<u8> {
    use image::ImageEncoder as _;
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}

/// 两页的 CBZ。**为什么不用那份 EPUB 样本**：EPUB 的索引会把封面那一件记进一份
/// 进程全局的量具（`epub::COVER_WORK`，`#[cfg(test)]` 的 `Vec<ThreadId>`），而 `cargo test`
/// 默认并行 —— 拿它当重扫的对象，会叫 `epub::tests` 里那条按长度断言的门漂
/// （实测一加就 3/3 红）。CBZ 同样要开 ZIP（所以 `ContainerOpens` 照样数得出来），
/// 而它的封面是「首页」，走不到 EPUB 那份量具里。
fn cbz_book(dir: &Path) -> std::path::PathBuf {
    use std::io::Write as _;
    let path = dir.join("two-pages.cbz");
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    for (name, bytes) in [("01.png", png(8, 8)), ("02.png", png(8, 8))] {
        zip.start_file(name, opts).unwrap();
        zip.write_all(&bytes).unwrap();
    }
    zip.finish().unwrap();
    path
}

/// 两页的 CB7（7z 漫画包）：条目与 `cbz_book` 同形，只是容器换成只能顺序解压的那一种。
fn cb7_book(dir: &Path) -> std::path::PathBuf {
    let pages = dir.join("cb7-pages");
    std::fs::create_dir_all(&pages).unwrap();
    std::fs::write(pages.join("01.png"), png(8, 8)).unwrap();
    std::fs::write(pages.join("02.png"), png(8, 8)).unwrap();
    let path = dir.join("two-pages.cb7");
    sevenz_rust2::compress_to_path(&pages, &path).unwrap();
    path
}

/// 导入一本只能顺序解压的漫画：**整趟只能解一遍**。
///
/// 这条门是被缓存键的形状叫出来的：那一格是「路径 + 长度 + mtime」，而导入的最后一步
/// 要把 `.staging` 里那个人看得懂的名字改成哈希名 —— 键跟着变，导入那一趟解出来的
/// 几百页就成了没人认领的孤儿，开书时再解一遍。屏幕上看不出任何区别，只是第一次翻页
/// 要等一整本。判据只有 `ComicExtracts` 这一格：两条路给读者的字节一模一样。
///
/// **把 `import.rs` 里那句 `relocate` 拿掉，第二条断言当场红**（`left: 1, right: 0`）。
#[test]
fn importing_a_comic_unpacks_it_exactly_once() {
    use crate::content::format::comic_cache::CacheGuard;
    use crate::infra::metrics::{Metric, take_local};

    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();
    let _guard = CacheGuard::new(tmp.path().join("cache"));
    let book = cb7_book(tmp.path());

    take_local(Metric::ComicExtracts);
    let outcomes = import_from(
        &PathsSource::picked(vec![book]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    let registered = added(&outcomes);
    assert_eq!(
        take_local(Metric::ComicExtracts),
        1,
        "导入那一趟该解一次（页表要在手上才能落库）"
    );

    // 开书那一趟：按**副本**（改名之后的哈希名）再索引一遍，一次都不许重解。
    let provider = crate::content::format::provider_for(&registered.dest)
        .expect("副本的扩展名要认得（`Format::copy_extension` 那条链）");
    assert_eq!(
        copies(&lib),
        vec![
            registered
                .dest
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        ],
        "根上就该是那一份哈希名"
    );
    let (_, content) = provider.index(&registered.dest).unwrap();
    assert!(
        matches!(content, crate::content::format::Content::Fixed(_)),
        "CB7 该读成定版式"
    );
    assert_eq!(
        take_local(Metric::ComicExtracts),
        0,
        "改名把解压缓存一起搬过去了：第 {} 本开书不许再解一遍",
        registered.book_id
    );
}

/// 把库里那一行的索引版本退回「旧算法算的」。`Store` 的连接是私有的，所以走第二条连接
/// （同 `store::tests` 里那句）。
fn make_index_stale(db: &Path, book_id: i64) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute(
        "UPDATE books SET index_version = 0 WHERE id = ?1",
        [book_id],
    )
    .unwrap();
}

/// 投出去的那一趟**不占用调用线程**。`ContainerOpens` 是线程局部计数（`infra::metrics`
/// 那条纪律），所以界面线程那份在提交之后必须还是 0 —— 重扫真的解了容器，但那笔钱记在
/// 池线程上。**在旧写法下这条是红的**：那时 `reindex` 就在调用线程上原地跑完整本书
/// （临时把提交换成原地调用，它给的是 `left: 1, right: 0`）。
///
/// 另一半钉的是「它确实把活做了」：等句柄收尾之后 `stale_reason` 从 `OldIndex` 变回
/// `None`、页表也回来了。**把提交整句换成一个只退号的空作业**，第二条断言立刻红
/// （`left: Some(OldIndex)`）—— 前一半单看会在「重扫根本没人做」时也绿，两条要成对读。
#[test]
fn scheduled_reindex_does_not_run_on_the_calling_thread() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("lib.db");
    let book = cbz_book(tmp.path());
    let store = Arc::new(Store::open_at(&db).unwrap());
    let meta = crate::content::format::DocMeta {
        title: "两页".into(),
        authors: vec![],
        format: crate::content::format::Format::Cbz,
        size: 0,
        ..crate::content::format::DocMeta::default()
    };
    let id = store.add_book(&book, &meta).unwrap();
    make_index_stale(&db, id);
    assert_eq!(
        store.stale_reason(id).unwrap(),
        Some(StaleReason::OldIndex),
        "起点是「索引旧了」"
    );

    take_local(Metric::ContainerOpens); // 归零：断言的是提交这一段里的增量
    let manager = TaskManager::new(2);
    let handle = schedule_reindex(&manager, store.clone(), id).expect("旧索引该投出去");
    assert_eq!(
        take_local(Metric::ContainerOpens),
        0,
        "调用线程一趟容器都不该开"
    );

    handle
        .events
        .into_iter()
        .find_map(|event| match event {
            TaskEvent::Finished(Ok(())) => Some(()),
            TaskEvent::Finished(Err(err)) => panic!("重扫该成功：{err}"),
            TaskEvent::Cancelled => panic!("没人取消过这一趟"),
            _ => None,
        })
        .expect("作业要以 Finished(Ok(..)) 收尾");
    assert_eq!(
        store.stale_reason(id).unwrap(),
        None,
        "重扫做完了，索引又可信"
    );
    let content = store.load_content(id).unwrap().expect("页表该回来了");
    assert!(
        content
            .as_fixed()
            .is_some_and(|fixed| fixed.pages.len() == 2),
        "而且是带着两页回来的"
    );
}

/// 两道门的语义。号记在 `Store` 上而不是进程级 `static`：`book_id` 每份内存库都从 1 起，
/// 挂在进程级集合上会叫并发跑的别的用例互相挡掉对方的提交 —— 那是一条红不了的假门。
#[test]
fn reindex_claims_are_one_per_book_and_failed_ones_stay_out() {
    let store = Store::in_memory().unwrap();
    assert!(store.claim_reindex(7), "第一次该领到号");
    assert!(!store.claim_reindex(7), "同一本在飞时不该有第二趟");
    store.note_reindex(7, true);
    assert!(
        store.claim_reindex(7),
        "成功要退号：`index_version` 下次被 bump 时那一本还得能再投"
    );

    assert!(store.claim_reindex(9), "另一本领自己的号");
    store.note_reindex(9, false);
    assert!(
        !store.claim_reindex(9),
        "试过且失败的，本进程内不再自动重投"
    );
    assert!(store.claim_reindex(10), "抑制只针对那一本，不牵连别本");
}

/// 提交那一侧真的尊重两道门：领不到号就返回 `None` 且不投作业 —— 不会有第二趟全量重读，
/// 也不会把号漏在飞（号只在作业里退）。
#[test]
fn schedule_reindex_respects_the_two_gates() {
    let store = Arc::new(Store::in_memory().unwrap());
    let manager = TaskManager::new(2);

    assert!(store.claim_reindex(3), "先占住「在飞」");
    assert!(
        schedule_reindex(&manager, store.clone(), 3).is_none(),
        "在飞时不再投"
    );

    store.note_reindex(3, false);
    assert!(
        schedule_reindex(&manager, store.clone(), 3).is_none(),
        "失败过的那本也不再自动投"
    );
}

// ── 按书并行 ───────────────────────────────────────────────────────────────────
//
// 下面这几条都把「同时几本」**钉住**（`import_parallel` 直接走批次引擎），而不是跟着
// `import_from` 的默认那一档走：默认按这台机器的核数切（`split_budget`），CI 上是 1、
// 开发机上是 4 —— 拿它当输入的话，同一测试在两台机器上测的是两件不同的事。
// 钉住之后这些条测的是「几路同时在导」那个形状本身。

/// 走批次引擎并把并行度钉成 `books_at_once`。
fn import_parallel(
    source: &dyn Source,
    lib: &Path,
    store: &Store,
    ctx: &StageCtx,
    books: usize,
) -> Vec<ImportOutcome> {
    let candidates = source.candidates(lib, ctx).expect("候选该交得出来");
    let staging = crate::infra::paths::staging_dir(lib);
    crate::infra::paths::prepare_dir(&staging).expect("暂存区该建得起来");
    // `per_book` 一律给到内层上限：这几条要比的只有「同时几本」这一个变量。
    // （预算怎么切成两层由 [`split_budget`] 那条单独钉。）
    import_batch(
        source,
        lib,
        store,
        ctx,
        &staging,
        &candidates,
        Budget {
            books,
            per_book: crate::content::format::epub::MAX_PARSE_WORKERS,
        },
    )
}

/// 一份核数怎么切成「同时几本 × 每本几路」。这三条恒等式都与机器无关，因为它们是从
/// 构造来的：总数不超核数（两处都只封顶、不放大）、至少一本一路、本数随核数不减。
///
/// 最后一条钉的是以前那一版的整除：`核数 ÷ 内层自己那 4 路` 在 6 核（Meizu 20 报的
/// 可用核数）上给的是「1 本」—— 接了并行却仍然逐本串行，而桌面 20 核上看不出这件事。
#[test]
fn one_budget_splits_into_two_layers_without_oversubscribing() {
    for cores in 1..=64 {
        let b = split_budget(cores);
        assert!(b.books >= 1 && b.per_book >= 1, "{cores} 核不能退化成零");
        assert!(
            b.books <= MAX_IMPORT_BOOKS,
            "{cores} 核算出 {b:?}：同时几本越过了封顶"
        );
        assert!(
            b.per_book <= crate::content::format::epub::MAX_PARSE_WORKERS,
            "{cores} 核算出 {b:?}：每本几路越过了内层上限"
        );
        assert!(
            b.books * b.per_book <= cores,
            "{cores} 核算出 {b:?}：乘积超过了这台机器的核数"
        );
    }
    let mut last_books = 0;
    for cores in 1..=64 {
        let b = split_budget(cores);
        assert!(b.books >= last_books, "{cores} 核上同时几本反而少了");
        last_books = b.books;
    }
    assert_eq!(
        split_budget(6).books,
        3,
        "6 核该同时导 3 本，不是退化成串行"
    );
    assert_eq!(
        (split_budget(1).books, split_budget(1).per_book),
        (1, 1),
        "单核就一路，别开线程"
    );
}

/// 凑齐 `n` 路才一起放行；凑不齐两秒后也放行，回「齐过没有」。
///
/// 不用 `std::sync::Barrier::wait`：凑不齐时它**永远挂**，而这里要的失败形状是
/// 「并行没跑起来」（红），不是「CI 卡到 job 超时」。
#[derive(Debug)]
struct Meet {
    n: usize,
    arrived: Mutex<usize>,
    ready: Condvar,
}

impl Meet {
    fn new(n: usize) -> Self {
        Self {
            n,
            arrived: Mutex::new(0),
            ready: Condvar::new(),
        }
    }

    fn arrive(&self) -> bool {
        let mut got = self.arrived.lock().unwrap_or_else(|e| e.into_inner());
        *got += 1;
        self.ready.notify_all();
        if *got >= self.n {
            return true;
        }
        let (got, _) = self
            .ready
            .wait_timeout_while(got, Duration::from_secs(2), |count| *count < self.n)
            .unwrap_or_else(|e| e.into_inner());
        *got >= self.n
    }
}

/// 每一本在 `stage` 里先等其余各路到齐 —— 把「并行」从「可能同时」变成「一定同时」。
///
/// 不等这一步，两本小书完全可能一前一后跑完，那条竞态根本没被试过：测试会绿，而它绿的是
/// 「串行也没坏」。`met` 记的是真的等到过「齐」的有几路。
struct Synchronized {
    paths: Vec<PathBuf>,
    gate: Meet,
    met: AtomicUsize,
}

impl Synchronized {
    fn new(paths: Vec<PathBuf>, n: usize) -> Self {
        Self {
            paths,
            gate: Meet::new(n),
            met: AtomicUsize::new(0),
        }
    }
}

impl Source for Synchronized {
    fn kind(&self) -> ImportKind {
        ImportKind::Paths
    }

    fn candidates(&self, _dest: &Path, _ctx: &StageCtx) -> crate::Result<Vec<Candidate>> {
        Ok(PathsSource::candidates_from(&self.paths))
    }

    fn stage(
        &self,
        candidate: &Candidate,
        staging_dir: &Path,
        ctx: &StageCtx,
    ) -> crate::Result<PathBuf> {
        if self.gate.arrive() {
            self.met.fetch_add(1, Ordering::SeqCst);
        }
        stage_local(candidate, staging_dir, ctx)
    }
}

/// 四路并行导四本：四路**确实同时在飞**，且一个都不少。
///
/// 这条钉的是引擎本身。没有 `met == 4` 那一句的话，「并行」可以退化成一路跑完全部而测试
/// 照绿 —— 那正是这类测试最容易买到的假绿。
#[test]
fn four_books_land_with_four_threads_actually_in_flight() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("原处");
    let paths: Vec<_> = (0..4)
        .map(|i| write(&dir, &format!("{i}.txt"), &format!("第 {i} 本\n")))
        .collect();
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let source = Synchronized::new(paths, 4);
    let outcomes = import_parallel(&source, &lib, &store, &StageCtx::detached(), 4);

    assert_eq!(
        source.met.load(Ordering::SeqCst),
        4,
        "四路该在 stage 那里真的碰到过彼此：没碰到就等于这条没试过并行"
    );
    assert_eq!(outcomes.len(), 4, "四本都要有结果");
    assert!(
        outcomes
            .iter()
            .all(|o| matches!(o.result, Ok(Imported::Added(_)))),
        "四本都该落地，实际 {:?}",
        outcomes.iter().map(|o| &o.result).collect::<Vec<_>>()
    );
    assert_eq!(store.shelf("").unwrap().len(), 4, "架上要看得见四本");
    assert_eq!(copies(&lib).len(), 4, "书库根上要躺着四份副本");
    assert!(staged(&lib).is_empty(), "结清之后暂存区该是空的");
}

/// 报表按**候选原序**交回，不按谁先跑完。
///
/// 界面那句「成功 N / 已存在 M」是聚合的，看不出顺序；但 `ImportOutcome` 是逐本一条，
/// 测试与日志都按候选那一批的位置读它。乱序的那一条要能对上号，否则「第 3 本被拒了」
/// 这种话就没法说。
#[test]
fn a_parallel_batch_reports_the_candidates_in_their_original_order() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("原处");
    let paths: Vec<_> = (0..8)
        .map(|i| write(&dir, &format!("{i}.txt"), &format!("第 {i} 本的字节\n")))
        .collect();
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let source = Synchronized::new(paths, 4);
    let outcomes = import_parallel(&source, &lib, &store, &StageCtx::detached(), 4);

    assert_eq!(outcomes.len(), 8);
    for (at, outcome) in outcomes.iter().enumerate() {
        assert_eq!(
            outcome.candidate.name,
            format!("{at}.txt"),
            "第 {at} 条结果对应的必须是第 {at} 个候选"
        );
    }
}

/// §九 那条门在并行下要**照样成立**：一批里两份内容相同的书，库里只能有一行、
/// 书库里只能有一份副本，另一条报「已存在」。
///
/// 串行时这条是顺序白给的（第二本必然看见第一本刚落好的那一行），并行就没有了：两本同时
/// `book_by_hash`、同时查到「没有」，就各落一行。数据库挡不住 —— `content_hash` 上建的是
/// 普通索引而不是 UNIQUE（`store/schema.rs`）。
///
/// **把 `import_candidate` 里那句 `CLAIMS.hold` 拿掉，这条会变红**（两行、两份副本）。
/// 两本都在 `stage` 那一步等到过彼此，所以这不是「碰巧一前一后」的绿。
#[test]
fn two_identical_books_in_one_parallel_batch_still_land_one_row() {
    let tmp = tempfile::tempdir().unwrap();
    // 不同的原路径、不同的建议名、同样的字节 —— 与那条串行的判重测试同一套设置。
    let a = write(&tmp.path().join("原处"), "甲.txt", "同一本书的字节\n");
    let b = write(&tmp.path().join("另一个地方"), "乙.txt", "同一本书的字节\n");
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let source = Synchronized::new(vec![a, b], 2);
    let outcomes = import_parallel(&source, &lib, &store, &StageCtx::detached(), 2);

    assert_eq!(source.met.load(Ordering::SeqCst), 2, "两本该同时在飞");
    let report = ImportReport::tally(&outcomes);
    assert_eq!(
        (report.added, report.duplicate, report.rejected()),
        (1, 1, 0),
        "一本落地、一本报已存在，实际 {:?}",
        outcomes.iter().map(|o| &o.result).collect::<Vec<_>>()
    );
    assert_eq!(
        store.shelf("").unwrap().len(),
        1,
        "同内容不许有两行 = 两套进度"
    );
    assert_eq!(copies(&lib).len(), 1, "书库根上只该有一份副本");
    assert!(
        staged(&lib).is_empty(),
        "等过号的那本也要把自己那份 .part 退掉，实际剩 {:?}",
        staged(&lib)
    );
}

/// 取消旗已经置位的一批，并行一路也不该碰任何候选。
///
/// 检查点落在**领号之前**（`Batch::run`），所以「一上来就取消」在任何一路都还是
/// 「一个候选都不处理」。并行让「置位后不再导新的」这条的延迟上限从一本变成每路各一本，
/// 但没领过的号绝不许被领。
#[test]
fn a_parallel_batch_that_is_already_cancelled_touches_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("原处");
    let paths: Vec<_> = (0..6)
        .map(|i| write(&dir, &format!("{i}.txt"), &format!("第 {i} 本\n")))
        .collect();
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let ctx = StageCtx::detached();
    ctx.cancel().trigger();
    let source = PathsSource::picked(paths);
    let outcomes = import_parallel(&source, &lib, &store, &ctx, 4);

    assert!(outcomes.is_empty(), "取消了就不该处理任何候选");
    assert!(copies(&lib).is_empty(), "副本也不该落");
    assert!(staged(&lib).is_empty(), "暂存区也不该留下东西");
}

/// CHM 走**完整条**导入：判成书 → 落一份 `<哈希>.chm` → 从库里那一串字认回格式 →
/// 按位置回容器取回正文与图。
///
/// 这一段的价值在于它把四层串起来：`Format::copy_extension`（副本的扩展名来自**判定**，
/// 不是来源那个 `.CHM`）、`Format::parse`（下一次开这本书靠库里那一列认格式）、
/// `locator_for`（CHM 按条目名存位置，漏接就退化成 `Inline("")` = 每章空白）、
/// 以及 `Container` 对 ITSF 故意不接文本、由 provider 带码页去解。
/// 任何一处漏接，单层测试都是绿的，症状却是「书在架上而打开是空的」—— 最难查的那一种。
#[test]
fn a_chm_imports_lands_and_reads_back() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/oss/chm/minimal-zh-gbk.chm");
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("原处");
    std::fs::create_dir_all(&source).unwrap();
    // 名字故意换成中文 + 大写扩展名：副本的定名不许跟着它走。
    let from = source.join("一本旧书.CHM");
    std::fs::copy(&fixture, &from).unwrap();
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let outcomes = import_from(
        &PathsSource::picked(vec![from]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    added(&outcomes);
    let landed = copies(&lib);
    assert_eq!(landed.len(), 1, "只该落一份副本：{landed:?}");
    assert!(
        landed[0].ends_with(".chm"),
        "副本的扩展名来自判定（小写），不是来源那个 `.CHM`：{landed:?}"
    );
    assert!(staged(&lib).is_empty(), "结清之后暂存区要空");

    let path = lib.join(&landed[0]);
    let doc = crate::content::document::open(&path).unwrap();
    assert_eq!(doc.meta().format, crate::content::format::Format::Chm);
    assert_eq!(
        doc.meta().title,
        "最小中文书",
        "书名来自 #SYSTEM，一路要活到界面"
    );
    let text = doc.chapter_source(0).unwrap();
    assert!(
        !text.contains('\u{fffd}'),
        "GBK 那本的正文不许有替换字符：{text}"
    );
    assert!(text.contains("序言"), "第 0 章的正文该是那串汉字：{text}");
    assert!(
        doc.image_bytes("img/fig.png").is_some(),
        "图按 href 现取要拿得到"
    );
}

/// 一本真 PDF 走完整条导入：**收下 → 副本按 `.pdf` 落 → 页表与书签入库 → 首页栅格
/// 落进 `covers/` 且库里指着它**。
///
/// 这一条是「能读能翻」那句验收在 core 侧的形状。为什么用入库的那份夹具而不是
/// 合成的字节：导入那条路上有六件事（判重、落副本、算封面哈希、写缩略图、落库、
/// 报表），任何一件对新格式没接上，症状都是「书在架上但打不开 / 一片占位图」，
/// 而那些只有走完整条管线才看得见。
///
/// 封面这一格同时是那条既存缺陷的回归门：`land_cover` 从前一句 `Reflow` 就 return，
/// `save_fixed_in` 又不写 `cover_hash` / `cover_mime` —— 定版式整族（漫画与 PDF 的
/// 首页栅格）算出来的封面掉进一个没人读的黑洞。
#[test]
fn a_pdf_walks_the_whole_import_pipeline_including_its_cover() {
    let shipped =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oss/pdf/three-pages.pdf");
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("原处");
    std::fs::create_dir_all(&source).unwrap();
    let copied = source.join("三页的一本.pdf");
    std::fs::copy(&shipped, &copied).unwrap();
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    let outcomes = import_from(
        &PathsSource::picked(vec![copied]),
        &lib,
        &store,
        &StageCtx::detached(),
    );
    let registered = added(&outcomes);

    // 副本的扩展名问的是 provider 注册表，不是 `Format::as_str` —— 两处各写一份
    // 就会漂成「导进来一本书，它自己的副本再也认不出来」。
    let names = copies(&lib);
    assert_eq!(names.len(), 1, "副本该有一份：{names:?}");
    assert!(
        names[0].ends_with(".pdf"),
        "副本该按 .pdf 落，实际 {}",
        names[0]
    );

    let fixed = store
        .load_fixed(registered.book_id)
        .unwrap()
        .expect("定版式的页表该在库里");
    assert_eq!(fixed.page_count(), 3, "pdfinfo 说 3 页");
    assert_eq!(fixed.toc.len(), 2, "书签两条该落库");
    assert_eq!(
        fixed.toc.iter().map(|e| e.page).collect::<Vec<_>>(),
        vec![Some(0), Some(1)],
        "落点是页号，不是容器路径"
    );

    // 封面：文件在 `covers/` 下，而库里那两格指着它。
    // 路径问 `cover::cover_path` 而不是在这里自己拼：文件名是哈希的**前 20 字**而不是
    // 完整哈希，写与读必须走同一个函数 —— 各拼一遍的错法是「文件明明在、封面却没了」。
    let cover = fixed
        .cover_ref
        .expect("首页栅格该落进 covers/ 并被库里指着");
    let dest = crate::content::format::cover::cover_path(&lib, &cover.hash, &cover.mime);
    assert!(
        dest.exists(),
        "封面文件该在 {}，covers/ 里实际 {:?}",
        dest.display(),
        std::fs::read_dir(lib.join("covers"))
            .expect("covers/ 该存在")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    );
    assert_eq!(cover.mime, "image/png", "栅格化交回的就是 PNG");

    // 架上那一格：格式名要认得出来（`Format::parse` 与 `as_str` 两张表的往返）。
    let row = store.book(registered.book_id).unwrap().expect("行该在");
    assert_eq!(row.format, "pdf", "{:?}", row.format);
    assert_eq!(
        crate::content::format::Format::parse(&row.format),
        Some(crate::content::format::Format::Pdf)
    );
}

/// §二 那条不变量在并行下的样子：**同一批里同名不同内容的几本，每一本落地的那份字节
/// 必须就是它自己那一份**，而库里那一行的内容身份问回来的必须是同一本。
///
/// 门在暂存区那间小屋上：解析前换的那个「给人看的名字」只由「建议名 + 扩展名」定，
/// 不分小屋的话几本会在同一个路径上互相盖 —— 甲正拿着它解析，乙把同名文件盖进来，
/// 甲再把**乙的字节**改名成**甲的哈希名**（库里那行于是记着甲的身份、躺着乙的内容）。
/// 并行度钉成 4：`import_from` 默认那一档跟着这台机器的核数切，单核机器上这一条
/// 根本碰不到并行，拿它当输入的话同一测试在两台机器上测的是两件不同的事。
#[test]
fn parallel_import_of_same_named_books_keeps_each_copy_its_own_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("books");
    let mut paths = Vec::new();
    for n in 0..16 {
        paths.push(write(
            &tmp.path().join(format!("第{n}本")),
            "同名的书.txt",
            &format!("第 {n} 本的正文，每一本都不一样。\n"),
        ));
    }
    let store = Store::in_memory().unwrap();
    let outcomes = import_parallel(
        &PathsSource::picked(paths),
        &lib,
        &store,
        &StageCtx::detached(),
        4,
    );

    let mut wrong: Vec<String> = Vec::new();
    let mut added = 0;
    for outcome in &outcomes {
        let Ok(Imported::Added(registered)) = &outcome.result else {
            wrong.push(format!(
                "《{}》没收进来：{:?}",
                outcome.candidate.name, outcome.result
            ));
            continue;
        };
        added += 1;
        let super::Origin::Local(from) = &outcome.candidate.origin else {
            panic!("本地候选该是 Origin::Local");
        };
        if std::fs::read(&registered.dest).unwrap_or_default() != std::fs::read(from).unwrap() {
            wrong.push(format!("第 {} 本的副本里是别的字节", registered.book_id));
        }
        // 身份与内容必须对上：拿副本那份字节现算的哈希去问库，问回来的就是这一本自己。
        let hash = crate::ingest::identity::sha256_of_path(&registered.dest)
            .unwrap()
            .to_hex();
        match store.book_by_hash(&hash).unwrap() {
            Some(row) if row.id == registered.book_id => {}
            other => wrong.push(format!(
                "第 {} 本：副本字节算出的身份问不回自己（{other:?}）",
                registered.book_id
            )),
        }
    }
    assert!(
        wrong.is_empty(),
        "{added}/16 本收进来，身份与内容分开的有：{wrong:?}"
    );
    assert_eq!(added, 16, "十六本同名不同内容的书，十六本都该进来");
    assert_eq!(store.shelf("").unwrap().len(), 16, "一本一格");
    assert!(
        staged(&lib).is_empty(),
        "每一本那间小屋走散时该收干净，实际剩 {:?}",
        staged(&lib)
    );
}

/// §一 步 7 那句「库里有这一行就等于能打开」由 [`super::unwind_landing`] 兜住：落地之后
/// 的四种失败形状各自该撤掉什么，都在这里直接喂值问一遍。
///
/// **这里钉住的是回滚本身，不是「`import_one` 在正确的三处调了它」**：那三处的触发条件
/// 都是「库说失败」，而 `Store` 没有注错入口（`add_book` 在一个事务里，要么整行要么
/// 没有行，所以从外面造不出半截状态）。要钉那一段就得给落库加一条 `#[cfg(test)]` 的假失败
/// 开关 —— 值不值这一格由他定。
#[test]
fn unwinding_the_landing_step_takes_back_whichever_pieces_landed() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();
    let first = added(&import_from(
        &PathsSource::picked(vec![write(tmp.path(), "甲.txt", "甲这一本的正文\n")]),
        &lib,
        &store,
        &StageCtx::detached(),
    ));
    let second = added(&import_from(
        &PathsSource::picked(vec![write(tmp.path(), "乙.txt", "乙那一本的正文\n")]),
        &lib,
        &store,
        &StageCtx::detached(),
    ));
    let second_row = store.book(second.book_id).unwrap().expect("行该在");

    // 一、自增那一支：行是我们新加的，撤行 + 撤副本，两边都不留。
    super::pipeline::unwind_landing(&store, None, Some(first.book_id), &first.dest);
    assert!(
        store.book(first.book_id).unwrap().is_none(),
        "行该跟着撤掉（留着就是「库里有行而文件没了」）"
    );
    assert!(!first.dest.exists(), "副本该退掉");

    // 二、自愈那一支：行是别人那一条，位置要退回动手之前那个**不存在**的旧名字。
    let moved = lib.join("改指到新位置.txt");
    std::fs::write(&moved, "乙那一本的正文\n").unwrap();
    store.retarget_book_path(second_row.id, &moved).unwrap();
    super::pipeline::unwind_landing(&store, Some(&second_row), Some(second_row.id), &moved);
    assert_eq!(
        store.book(second_row.id).unwrap().expect("行该还在").path,
        second_row.path,
        "位置该退回动手之前那一格"
    );
    assert!(!moved.exists(), "副本该退掉");

    // 三、行根本没落地（`add_book` 与 `retarget_book_path` 自己失败）：只退副本，
    // 不许去撤一条并不存在的行，也不许连带删掉别人那一本。
    let stray = lib.join("没人认领.txt");
    std::fs::write(&stray, "x").unwrap();
    super::pipeline::unwind_landing(&store, None, None, &stray);
    assert!(!stray.exists(), "副本该退掉");
    assert_eq!(
        store.shelf("").unwrap().len(),
        1,
        "只剩乙那一本：第三支不该带走任何行"
    );
}

/// 落库那几句的假失败旗子（线程局部），出了作用域自动收回。
struct FailHere;

impl Drop for FailHere {
    fn drop(&mut self) {
        super::pipeline::fail_landing(None);
    }
}

/// 让落库的 `which` 那一句在下一次假失败。
///
/// 用它的测试都把并行度钉成 **1**：那时 [`Batch::run`] 就在调用线程上，而旗子是按线程放的。
fn fail_at(which: &'static str) -> FailHere {
    super::pipeline::fail_landing(Some(which));
    FailHere
}

/// §一 步 7 那句「库里有这一行就等于能打开」，逐条钉在**管线真的调了回滚**上：
/// 落库那五句各自假失败一次，看退回去的到底是不是只有已经落好的那几样。
///
/// 前四档都该「一本书没进来，而书库根与暂存区都干干净净」；最后一档正好相反 ——
/// 身份列写不进去**不算这本没进来**（行与副本都在，只是哈希空着，交给补齐）。
#[test]
fn each_landing_failure_takes_back_just_the_pieces_that_landed() {
    let good = |n: u32| format!("第 {n} 本的正文，内容各不相同。\n");

    // 一、判重那一问失败：字节还没换名、没进根、没落库，退掉 `.part` 就该完事。
    {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("books");
        let store = Store::in_memory().unwrap();
        let source = PathsSource::picked(vec![write(tmp.path(), "一问.txt", &good(1))]);
        let _guard = fail_at("book_by_hash");
        let outcomes = import_parallel(&source, &lib, &store, &StageCtx::detached(), 1);
        drop(_guard);
        assert!(outcomes[0].result.is_err(), "库问不出就该拒这一本");
        assert_eq!(copies(&lib), Vec::<String>::new(), "书库根上不许有东西");
        assert!(staged(&lib).is_empty(), "暂存区那一份要退掉");
        assert!(store.shelf("").unwrap().is_empty(), "库里没有那一行");
    }

    // 二、新增那一行失败：副本已经 rename 进根了，得跟着退掉 —— 这正是原来漏掉的那个洞
    // （根上的半成品**没有** `sweep_partials` 兜底，它只认 `.{名字}.part` 那种名字）。
    {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("books");
        let store = Store::in_memory().unwrap();
        let source = PathsSource::picked(vec![write(tmp.path(), "新增.txt", &good(2))]);
        let _guard = fail_at("add_book");
        let outcomes = import_parallel(&source, &lib, &store, &StageCtx::detached(), 1);
        drop(_guard);
        assert!(outcomes[0].result.is_err());
        assert_eq!(copies(&lib), Vec::<String>::new(), "没人认领的副本要退掉");
        assert!(staged(&lib).is_empty());
        assert!(store.shelf("").unwrap().is_empty());
    }

    // 三、`save_document` 失败：行已经插进去了，得连着撤掉（留一行指向没内容的东西最坏）。
    {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("books");
        let store = Store::in_memory().unwrap();
        let source = PathsSource::picked(vec![write(tmp.path(), "章表.txt", &good(3))]);
        let _guard = fail_at("save_document");
        let outcomes = import_parallel(&source, &lib, &store, &StageCtx::detached(), 1);
        drop(_guard);
        assert!(outcomes[0].result.is_err());
        assert_eq!(copies(&lib), Vec::<String>::new(), "副本要退掉");
        assert!(
            store.shelf("").unwrap().is_empty(),
            "刚插的那一行要跟着撤掉"
        );
        assert!(staged(&lib).is_empty());
    }

    // 四、身份列写不进去：**这本照样进来**。行与章节都在、书能打开，只是哈希还空着。
    {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("books");
        let store = Store::in_memory().unwrap();
        let source = PathsSource::picked(vec![write(tmp.path(), "身份.txt", &good(4))]);
        let _guard = fail_at("set_content_hash");
        let outcomes = import_parallel(&source, &lib, &store, &StageCtx::detached(), 1);
        drop(_guard);
        let registered = added(&outcomes);
        assert!(registered.dest.exists(), "副本留着，书能打开");
        assert_eq!(copies(&lib).len(), 1, "根上就那一份");
        assert_eq!(store.shelf("").unwrap().len(), 1, "那一行也留着");
        assert_eq!(
            store.rows_missing_hash().unwrap().len(),
            1,
            "哈希空着是懒补允许的状态，补齐作业会重算它"
        );
    }
}

/// 自愈那一支（命中的行还在、副本文件没了）在 `retarget_book_path` 失败时不许把行改指
/// 到一份没人认领的副本上：位置退回原来那个不存在的文件，新来的那份字节退掉。
#[test]
fn a_failed_heal_leaves_the_row_pointing_where_it_started() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();
    let body = "同一本书，两次送来。\n";
    let first = added(&import_from(
        &PathsSource::picked(vec![write(tmp.path(), "第一次.txt", body)]),
        &lib,
        &store,
        &StageCtx::detached(),
    ));
    // 读者手工删掉了那个副本 —— 判重命中而文件不在，下一本同内容的要走自愈。
    std::fs::remove_file(&first.dest).unwrap();

    let second_file = write(&tmp.path().join("第二次"), "第二次.txt", body);
    let source = PathsSource::picked(vec![second_file]);
    let _guard = fail_at("retarget_book_path");
    let outcomes = import_parallel(&source, &lib, &store, &StageCtx::detached(), 1);
    drop(_guard);

    assert!(outcomes[0].result.is_err(), "改指失败就是这一本没进来");
    let row = store
        .book(first.book_id)
        .unwrap()
        .expect("那一行不能被撤掉");
    assert_eq!(
        row.path,
        first.dest.to_string_lossy(),
        "位置要退回动手之前那一格（哪怕是那个不存在的旧名）"
    );
    assert_eq!(
        copies(&lib),
        Vec::<String>::new(),
        "不许留一份没人认领的副本"
    );
    assert!(staged(&lib).is_empty(), "暂存区要结清");
}

/// 重扫那一路失败**两边都不许动**：那一行本来就在、那份副本是权威的。
/// 这是 `Landing::Rescan` 与 `Landing::New` 唯一真正的分别，所以单独钉一条。
#[test]
fn a_failed_rescan_touches_neither_the_row_nor_the_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();
    let landed = added(&import_from(
        &PathsSource::picked(vec![write(tmp.path(), "重扫.txt", "重扫这一本的正文\n")]),
        &lib,
        &store,
        &StageCtx::detached(),
    ));
    let before = copies(&lib);

    let _guard = fail_at("add_book");
    let err = super::reindex(&store, landed.book_id).expect_err("落库那句假失败，重扫要报失败");
    drop(_guard);

    assert!(format!("{err:#}").contains("假失败"), "{err:#}");
    assert_eq!(copies(&lib), before, "权威副本一个字节都不许动");
    assert!(
        store.book(landed.book_id).unwrap().is_some(),
        "那一行本来就在，失败不许把它带走"
    );
    assert_eq!(store.shelf("").unwrap().len(), 1, "架上还是那一格");
}

/// 测试夹具：`paths` 里等于 `boom` 的那一本在 `stage` 那一步直接 panic，其余照常暂存。
/// 崩点选在 `stage` 而不是 `candidates`，是因为真源因崩长这样：来源或 provider 里一句
/// `unwrap`，而 `stage` 正是并行那一段真正跑起来的地方。
struct Booming {
    paths: Vec<PathBuf>,
    boom: PathBuf,
}

impl Source for Booming {
    fn kind(&self) -> ImportKind {
        ImportKind::Paths
    }

    fn candidates(&self, _book_dir: &Path, _ctx: &StageCtx) -> crate::Result<Vec<Candidate>> {
        Ok(PathsSource::candidates_from(&self.paths))
    }

    fn stage(
        &self,
        candidate: &Candidate,
        staging_dir: &Path,
        ctx: &StageCtx,
    ) -> crate::Result<PathBuf> {
        let super::Origin::Local(path) = &candidate.origin else {
            panic!("本地候选该是 Origin::Local")
        };
        if *path == self.boom {
            panic!("测试：这一本把跑它的线程弄崩了");
        }
        stage_local(candidate, staging_dir, ctx)
    }
}

/// 一批里有一本把跑它的那一路弄崩了（真源因崩长这样：来源或 provider 里一句 `unwrap`）：
/// **只算它自己失败**，另外两本照常进来，报表一条都不许多、也一条都不许少。
///
/// 两种并行度各跑一遍：钉 1 路时整批就在调用线程上跑，钉 2 路时崩的那本在 worker 里 ——
/// 而 `thread::scope` 会把 scoped 线程的 panic 在 scope 结束时再传回调用线程（实测：
/// 不接这一句两种都整批红，`landed` 里已经落好的那些跟着整张报表一起丢，
/// 而 `finish()` 那段「缺的那几本补跑」根本跑不到）。
#[test]
fn a_panicking_book_only_fails_itself() {
    for books in [1usize, 2] {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("books");
        let a = write(&tmp.path().join("甲"), "甲.txt", "甲这一本的正文\n");
        let b = write(&tmp.path().join("乙"), "乙.txt", "乙这一本的正文\n");
        let c = write(&tmp.path().join("丙"), "丙.txt", "丙这一本的正文\n");
        let boom = b.clone();
        let source = Booming {
            paths: vec![a, b, c],
            boom,
        };
        let store = Store::in_memory().unwrap();
        let outcomes = import_parallel(&source, &lib, &store, &StageCtx::detached(), books);

        assert_eq!(outcomes.len(), 3, "{books} 路：三本该有三条结论");
        let report = ImportReport::tally(&outcomes);
        assert_eq!(
            (report.added, report.rejected()),
            (2, 1),
            "{books} 路：{report:?}"
        );
        assert_eq!(report.unreadable, 1, "崩的那本算「读不动」那一档");
        let crashed: Vec<String> = outcomes
            .iter()
            .filter(|o| o.result.is_err())
            .map(|o| o.candidate.name.clone())
            .collect();
        assert_eq!(
            crashed,
            vec!["乙.txt".to_string()],
            "{books} 路：崩的该就是那一本"
        );
        assert_eq!(
            store.shelf("").unwrap().len(),
            2,
            "{books} 路：另外两本要真的在架上"
        );
        assert!(
            staged(&lib).is_empty(),
            "{books} 路：崩在半路也不该在暂存区留下东西，实际 {:?}",
            staged(&lib)
        );
    }
}
