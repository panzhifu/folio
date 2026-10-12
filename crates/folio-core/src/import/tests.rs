use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{
    BackfillReport, Candidate, ImportKind, ImportOutcome, ImportReport, Imported, PathsSource,
    Source, backfill_copies, import_from, stage_local, submit_import, sweep_partials,
};
use crate::store::Store;
use crate::task::{StageCtx, TaskEvent, TaskManager};

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
    use super::url_name;
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
    use crate::remote::{Error as RemoteError, Response, Sink, Transport};

    /// 假传输：正文按 3 字节一块发；`cancel_after` 块之后置位取消旗标，
    /// 模拟读者在下载中途按下取消。
    struct Fixed {
        data: Vec<u8>,
        cancel_after: Option<usize>,
    }
    impl Transport for Fixed {
        fn request(&self, _req: crate::remote::Request) -> crate::remote::Result<Response> {
            Ok(Response::new(200, self.data.clone()))
        }
        fn download(
            &self,
            _req: crate::remote::Request,
            sink: &mut dyn Sink,
            cancel: &crate::remote::Cancel,
        ) -> crate::remote::Result<u64> {
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
    assert!(matches!(outcomes[0].result, Err(_)), "取消落一条 Err");
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

/// 取消落在两本**之间**：已经进去的那本是真的，后面的不再来 ——
/// 这是「架面上的取消按钮」能做的前提。
#[test]
fn cancelling_between_books_keeps_what_already_landed() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("原处");
    let a = write(&picked, "a.txt", "第一本\n");
    let b = write(&picked, "b.txt", "第二本\n");
    let lib = tmp.path().join("books");
    let store = Store::in_memory().unwrap();

    /// 两本的假来源：第一本 stage 完就置位取消，让循环停在第二本之前。
    struct CancelAfterFirst {
        paths: Vec<std::path::PathBuf>,
        staged: AtomicUsize,
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
        ) -> crate::Result<std::path::PathBuf> {
            let staged = stage_local(candidate, staging_dir, ctx)?;
            if self.staged.fetch_add(1, Ordering::SeqCst) == 0 {
                ctx.cancel().trigger();
            }
            Ok(staged)
        }
    }

    let ctx = StageCtx::detached();
    let source = CancelAfterFirst {
        paths: vec![a, b],
        staged: AtomicUsize::new(0),
    };
    let outcomes = import_from(&source, &lib, &store, &ctx);
    assert_eq!(outcomes.len(), 1, "第二本不该再被处理");
    assert!(matches!(outcomes[0].result, Ok(Imported::Added(_))));
    assert_eq!(store.shelf("").unwrap().len(), 1, "已进去的那本在库里");
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
    let meta = crate::format::DocMeta {
        title: "老名字".into(),
        authors: vec![],
        format: crate::format::Format::Text,
        size: 0,
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
    let meta = crate::format::DocMeta {
        title: "在别处".into(),
        authors: vec![],
        format: crate::format::Format::Text,
        size: 0,
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
    let meta = crate::format::DocMeta {
        title: "一本".into(),
        authors: vec![],
        format: crate::format::Format::Text,
        size: 0,
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
        let verdict = crate::format::sniff::identify(&display, &path);
        assert!(
            verdict.format().is_some(),
            "副本 {name} 认不出来了：{verdict:?}"
        );
        assert!(
            crate::format::provider::provider_for(&path).is_some(),
            "副本 {name} 没有 provider 认领，打开就是「不支持的书籍格式」"
        );
        // 名字的主名必须是那 16 个字符的哈希前缀，不是建议名。
        assert_eq!(name.split('.').next().unwrap().len(), 16, "{name}");
    }
}

/// 架子上不许出现哈希串：**书名与章名要跟着来源给的那个名字走**。
///
/// 单文件格式（TXT、无标题行的 Markdown）的标题是 `stem(path)` 回落出来的
/// （`format::markdown` 里那两处），而副本按内容命名之后主名就是一串 base32 ——
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
    assert_eq!(
        report.unsupported, 2,
        "PDF 认得出但不支持、二进制不是书，同属「不支持」这一栏"
    );
    assert_eq!(report.duplicate, 0);
    assert_eq!(report.corrupt + report.unreadable, 0, "这一批里没有另两档");
    assert!(
        report.summary().contains("成功 1"),
        "那句总结要说得出数：{}",
        report.summary()
    );
}
