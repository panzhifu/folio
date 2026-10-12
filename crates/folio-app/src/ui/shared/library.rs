//! 书库管理里两端共用的那几件事：怎么认出一份副本、删一本书删掉哪些东西、
//! 书库目录从哪儿来。
//!
//! 判据只有一条：**库里的行一定删，文件只删我们自己拷进来的那份**。两端导入时都会
//! 先复制一份进书库（拷贝、解析、落库都在后端管线里，见 `folio_core::import`），
//! 所以删掉一本书不该牵连到读者自己手里那个原件 —— 原件在别的目录里，从书库里
//! 移掉一本书不等于把人家硬盘上的文件删了。

use std::path::{Path, PathBuf};

use folio_core::Library;
use folio_core::Store;
use gpui_kit::{App, PathPromptOptions, Task};

use super::config_model::ConfigHolder;
use super::error::{Context as _, Result};

/// 导入的目标目录：读者确认过的那个。
///
/// 没确认过时是 `None`，调用方**必须**拦住导入而不是拿默认值凑一次 —— 拷贝这件事
/// 会在硬盘上留下东西，留哪儿得先说清楚。
pub fn target_dir(library: &Library) -> Option<PathBuf> {
    library
        .is_confirmed()
        .then(|| PathBuf::from(library.dir.trim()))
}

/// 建议位置：`<数据根>/books`。确认面板里默认填的就是它。
pub fn suggested_dir() -> PathBuf {
    folio_core::paths::books_dir()
}

/// 启动时清掉书库目录里上次崩溃留下的 `.part` 半成品（`plans/基础设施.md` §1.3 末条）。
///
/// 位置没确认过就没什么可清的。清掉几个只记日志 —— 这是自愈动作，不该刷屏。
pub fn sweep_partials(cx: &App) {
    let Some(dir) = target_dir(&ConfigHolder::library(cx)) else {
        return;
    };
    let removed = folio_core::sweep_partials(&dir);
    if removed > 0 {
        tracing::warn!(
            "library: 清掉 {removed} 个上次留下的 .part 半成品（{}）",
            dir.display()
        );
    }
}

/// 整机扫描的起点。判据与 [`candidate_dirs`] 同一条：只扫读者放得进东西的地方 ——
/// 桌面扫**家目录**（`/` 全盘会把 `/proc`、`/sys` 也卷进来，那里没有书），
/// Android 扫共享存储根（本应用自己的外部目录在 `Android/data` 底下，要作为噪音
/// 规则的豁免区传给 [`folio_core::scan::find`]），iOS / 鸿蒙的沙盒就那么大，扫数据根。
///
/// Android 上 `all_files` 说的是「所有文件访问」拿到了没有（[`crate::ui::platform`]::
/// request_storage_access 的结果）：拿到了才扫共享存储根；没拿到时能扫的只有应用
/// 自己的外部目录 —— 那里是 `adb push` 的法定落点，退化路径也有东西可找。
pub fn scan_roots(all_files: bool) -> Vec<PathBuf> {
    if cfg!(target_os = "android") {
        if all_files {
            return vec![PathBuf::from("/storage/emulated/0")];
        }
        return vec![PathBuf::from(format!(
            "/storage/emulated/0/Android/data/{PACKAGE}/files"
        ))];
    }
    // iOS 的沙盒就那么大；鸿蒙的 `cfg!` 不认识 `target_os = "ohos"`（编译目标名
    // 不在标准清单里），桌面分支拿不到 HOME 时也落回数据根 —— 三者共用这条。
    if !cfg!(target_os = "android") && std::env::var_os("HOME").is_none_or(|h| h.is_empty()) {
        return vec![folio_core::paths::data_dir()];
    }
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => vec![PathBuf::from(home)],
        _ => Vec::new(),
    }
}

/// Android 上噪音规则的豁免区：本应用自己的外部目录（`adb push` 的书都堆在那儿），
/// 虽然在 `Android/data` 底下但必须照扫。其他平台没有这一层，返回空。
pub fn scan_noise_exempt() -> Vec<PathBuf> {
    if cfg!(target_os = "android") {
        return candidate_dirs();
    }
    Vec::new()
}

/// 本应用的 Android 包名。应用专属外部目录的路径里有它，
/// 必须与 `crates/folio-app/android/app/build.gradle.kts` 的 `applicationId` 一致。
pub const PACKAGE: &str = "dev.folio.reader";

/// 能当书库用的候选目录：本应用**真的写得进去**的那几个。
///
/// 这是「默认建议」与「没装文件选择器时」的兜底；读者也可以直接用系统文件夹选择器
/// 挑任意目录（[`pick_dir`]）——`gpui-mobile` 的 `prompt_for_paths{directories: true}`
/// 现在走 `ACTION_OPEN_DOCUMENT_TREE`：本地卷（内部存储 / SD 卡）会尽量解析回真实文件
/// 路径，所以 `std::fs` 拷副本这条路走得通；解析不出真实路径的提供方（云盘等）退回
/// tree URI，那种位置拷不了，会在导入时报错而不是静默失败。
///
/// 外部那条排在前面：它在系统文件管理器和 `adb push` 里都看得见，读者既能自己放书，
/// 也能自己核对拷过去的是什么。内部那条就是 [`suggested_dir`]。
pub fn candidate_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(target_os = "android") {
        out.push(PathBuf::from(format!(
            "/storage/emulated/0/Android/data/{PACKAGE}/files/folio/books"
        )));
    }
    out.push(suggested_dir());
    out
}

/// 打开系统文件夹选择器，选中就把书库位置写进配置。返回的 `Task` 要不要持有由调用方定：
/// 静态设置页拿不住句柄，用 `detach()` 让它跑完即可；有实体的页面可以存起来随页面一起取消。
///
/// 取消（`Ok(None)`）与出错都只记一条日志，不改配置 —— 读者点「取消」不该把书库位置清空。
pub fn pick_dir(cx: &mut App) -> Task<()> {
    choose_dir(cx, |cx, dir| {
        let path = dir.display().to_string();
        tracing::info!("pick_dir: 书库位置选为 {path}");
        ConfigHolder::edit_library(cx, |library| library.dir = path);
    })
}

/// 打开系统文件夹选择器，选中把目录交给 `picked`（在界面线程上执行）。
/// 取消与出错只记日志 —— 读者点「取消」不该清掉任何东西。
pub fn choose_dir(
    cx: &mut App,
    picked: impl Fn(&mut App, &std::path::Path) + 'static,
) -> Task<()> {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: None,
    });
    cx.spawn(async move |cx| {
        let picked_dir = match receiver.await {
            Ok(Ok(Some(mut paths))) => paths.pop(),
            Ok(Ok(None)) => None,
            Ok(Err(err)) => {
                tracing::error!("choose_dir: 文件夹选择器失败 {err}");
                None
            }
            Err(_) => None,
        };
        if let Some(dir) = picked_dir {
            cx.update(|cx| picked(cx, &dir));
        }
    })
}

/// 监听的默认去处：`<数据根>/inbox`。开关打开时没选过目录就落在这里 ——
/// 一个独立于书库的「收件箱」，往里丢书就自动入库。
pub fn suggested_watch_dir() -> PathBuf {
    folio_core::paths::data_dir().join("inbox")
}

/// 启动时投一趟「补齐」的后台作业：给没有内容身份的行算哈希，并把书库里的旧名字
/// 副本改成哈希名。
///
/// 这一趟存在的理由是两条决定凑在一起的样子（`plans/导入管线.md` §六 + §一 步 5）：
/// 哈希列是**懒补**的（迁移不许读全库，否则开库那一下要等几百本的顺序读），
/// 而副本改名又必须真的做完（不然「书库目录可以整目录 GC」一直不成立）。
/// 所以它不能放在开库路径上，也不能放在界面线程上 —— 它落在作业池的
/// [`Cleanup`](folio_core::Priority) 档：界面空闲才跑的那一档，也是 `plans/基础设施.md`
/// §1.1 那张表里此前**唯一一个没有作业**的档。
///
/// 位置没确认过、书库没装上时什么都不投（没有目录可搬）。取消没人按：这一趟是
/// 自愈档，做完为止；退出时由 `Services::shutdown` 排干队列（§1.3）。
pub fn schedule_backfill(cx: &App) {
    let Some(dir) = target_dir(&ConfigHolder::library(cx)) else {
        return;
    };
    let Some(store) = super::app_store::handle(cx) else {
        tracing::debug!("library: 书库没装上，补齐这一趟不投");
        return;
    };
    // 句柄直接丢：这一趟没有界面读数（它不改书目内容，只改文件名与身份列）。
    // 作业照跑，跑完自己结束 —— 池在关停时会等它把已排的做完（`plans/基础设施.md` §1.3）。
    let for_log = dir.clone();
    let _report = super::services::pool().submit_with(folio_core::Priority::Cleanup, move |ctx| {
        Ok(folio_core::backfill_copies(&store, &dir, ctx))
    });
    tracing::info!("library: 补齐这一趟已投进 Cleanup 档（{for_log:?}）");
}

/// 删掉一本书：先删库里的行（章节、正文、图片、进度、批注、集合都跟着级联走），
/// 再连带删掉属于书库的那份副本。
///
/// `staged_dir` 由调用方给（生产代码传 [`target_dir`] 那个确认过的目录，没确认过时
/// 传 `None`，那就只删行），是为了让「哪个目录算应用自己的」这条判据在测试里可换 ——
/// 它不该靠改环境变量来挪：那是进程级的，同一二进制里并发跑的别的用例会读到错的那份。
///
/// 顺序是有意的：文件先删而库没删成，会留下一条指向不存在文件的记录，界面上就是
/// 一本点不开的书；库先删成而文件没删掉，只是多占一点空间，读者看不见任何异常。
pub fn delete_book(store: &Store, id: i64, staged_dir: Option<&Path>) -> Result<()> {
    let path = match store.book(id) {
        Ok(row) => row.map(|row| row.path),
        Err(err) => {
            // 查路径是尽力而为：查不到只是不认领那份副本，不该拦住删行 ——
            // 删行才是「删除」的本体，剩下的只是一个占地方的文件。
            tracing::warn!("delete_book: 查第 {id} 本的路径失败：{err}");
            None
        }
    };
    // 封面文件必须在删行**之前**问出来：`cover_hash` 跟着那一行走，删完就问不到了。
    // 它内部会看一眼「还有没有别人指着同一份字节」，所以共用封面不会被带走。
    delete_cover_file(store, id);
    store
        .remove_book(id)
        .with_context(|| format!("删掉第 {id} 本的库内记录"))?;
    let copy = path
        .as_deref()
        .and_then(|path| staged_dir.and_then(|dir| owned_copy(path, dir)));
    let Some(copy) = copy else { return Ok(()) };
    // 内容寻址之后「两条记录共用一个副本」是允许的状态（自愈会把一条记录接到一份已存在的
    // 文件上），所以删之前问一句：还有别人指着它就不删 —— 删了的代价是另一本的正文与
    // 批注一起没，而「多占一个文件」没有任何可见后果
    // （`plans/导入管线.md` §六 末段、§十一 已决第 3 条）。
    let others = store
        .rows_sharing_path(&copy.to_string_lossy(), id)
        // 问不出来（语句出错、库被别处锁着）按「有人指着」处理：偏向少删一次。
        .with_context(|| format!("问 {copy:?} 还有几条记录指着"))
        .unwrap_or(1);
    if others > 0 {
        tracing::info!("delete_book: 副本 {copy:?} 还有 {others} 条记录指着，删行不删文件");
        return Ok(());
    }
    if let Err(err) = std::fs::remove_file(&copy) {
        tracing::warn!("delete_book: 副本 {copy:?} 没删掉：{err}");
    }
    Ok(())
}

/// 删掉这本书的封面文件 —— **只在没有别人指着同一张的时候**。
///
/// 与副本那条规矩同出一源（`plans/导入管线.md` §八 第 4 条末段）：封面是**它自己
/// 字节**的身份，两本书共用一张封面时磁盘上只有一个文件。删掉一本而把文件带走，
/// 会让另一本的封面凭空消失，且没有任何报错。
///
/// 判据全在 [`Store::orphan_cover_file`] 里（共用计数、老库那一档、文件名怎么拼），
/// 这一层只负责删。**必须在 `remove_book` 之前调**：行删掉之后就问不出 `cover_hash`。
///
/// 失败只记日志：多留一个封面文件没有任何可见后果，而「删书失败」不该由封面引起 ——
/// 所以这里不回 `Result`，调用方也没有要处理的分支。
pub fn delete_cover_file(store: &Store, id: i64) {
    let orphan = match store.orphan_cover_file(id) {
        Ok(found) => found,
        Err(err) => {
            // 问不出来按「有人指着」处理：偏向少删一次。
            tracing::warn!("delete_cover_file: 问第 {id} 本的封面失败，留着不删：{err:#}");
            return;
        }
    };
    let Some(file) = orphan else { return };
    if let Err(err) = std::fs::remove_file(&file) {
        tracing::warn!("delete_cover_file: 封面 {file:?} 没删掉：{err}");
    }
}

/// 这条 `books.path` 是不是应用自己拷进书库的那份。
///
/// 判据是「在不在书库目录底下」而不是「存不存在」：后者会把恰好同名、但躺在别处的
/// 读者文件一起算进来。读者中途改过书库位置时，旧书库里那些副本不再被认领 ——
/// 不会误删，只是留在原地。
pub fn owned_copy(path: &str, dir: &Path) -> Option<PathBuf> {
    let file = Path::new(path);
    file.starts_with(dir).then(|| file.to_path_buf())
}

#[cfg(test)]
mod tests {
    //! 这一层只有两件事值得钉住：**导入落库落全了没有**，以及**删一本书删掉的是哪些文件**。
    //!
    //! 删的那条判据最贵，因为判错的代价不对称。库里存的路径在两端长得一样 —— 移动端是
    //! `<数据根>/books/x.epub`（导入时拷的），桌面端是 `/home/…/我的书/x.epub`
    //! （读者自己的）。前者不删是留垃圾，后者删了是删掉人家硬盘上的原件，所以两头都要钉住。

    use std::path::Path;

    use crate::ui::shared::library::{delete_book, owned_copy, target_dir};
    use folio_core::format::{Chapter, DocMeta, FlowDoc, Format};
    use folio_core::{Library, Store};

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec!["作者".into()],
            format: Format::Epub,
            size: 10,
        }
    }

    fn flow() -> FlowDoc {
        FlowDoc {
            chapters: vec![Chapter::inline(
                "第一章",
                // 契约：`Chapter::source` 是引擎就绪的 XHTML。这里不进排版，
                // 但造数据也照契约来，免得下次有人拿它当「纯文本也能塞」的例子。
                "<p>正文</p>",
            )
            .with_spine_href("c1.xhtml")],
            ..Default::default()
        }
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"not really an epub").unwrap();
    }

    #[test]
    fn the_predicate_only_accepts_paths_inside_the_apps_own_directory() {
        let books = Path::new("/data/folio/books");
        assert!(owned_copy("/data/folio/books/a.epub", books).is_some());
        assert!(owned_copy("/home/noke/Books/a.epub", books).is_none());
        // 文件名一样不算同一种东西：判的是位置。
        assert!(owned_copy("/home/noke/books/a.epub", books).is_none());
        // 相对路径没有落进那个目录，就不该被当成副本。
        assert!(owned_copy("a.epub", books).is_none());
    }

    /// 没确认过位置时 `target_dir` 必须是 `None`，而不是悄悄给一个默认值 ——
    /// 导入会往硬盘上拷一份文件，落点没说过就不该动手。
    #[test]
    fn an_unconfirmed_library_blocks_the_import_rather_than_picking_for_the_reader() {
        assert_eq!(target_dir(&Library::default()), None);
        assert_eq!(target_dir(&Library { dir: "   ".into(), watch: None }), None);
        assert_eq!(
            target_dir(&Library {
                dir: " /x/books ".into(),
                watch: None
            }),
            Some(Path::new("/x/books").to_path_buf())
        );
    }

    #[test]
    fn deleting_a_book_removes_the_row_its_chapters_and_our_copy() {
        let dir = tempfile::tempdir().unwrap();
        let books = dir.path().join("books");
        let staged = books.join("a.epub");
        touch(&staged);

        let store = Store::in_memory().unwrap();
        let id = store.add_book(&staged, &meta("甲")).unwrap();
        store.save_flow(id, &flow(), Format::Epub).unwrap();

        delete_book(&store, id, Some(&books)).unwrap();

        assert!(store.book(id).unwrap().is_none(), "书要没");
        assert!(store.load_flow(id).unwrap().is_none(), "章节要跟着级联没");
        assert!(!staged.exists(), "我们自己拷进来的那份要一起删掉");
    }

    #[test]
    fn deleting_a_book_leaves_the_readers_own_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let books = dir.path().join("books");
        std::fs::create_dir_all(&books).unwrap();
        let original = dir.path().join("我的书/a.epub");
        touch(&original);

        let store = Store::in_memory().unwrap();
        let id = store.add_book(&original, &meta("乙")).unwrap();
        store.save_flow(id, &flow(), Format::Epub).unwrap();

        delete_book(&store, id, Some(&books)).unwrap();

        assert!(store.book(id).unwrap().is_none(), "从书库里移掉……");
        assert!(original.exists(), "……不等于把读者的原文件删了");
    }
}
