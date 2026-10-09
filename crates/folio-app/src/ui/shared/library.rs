//! 书库管理里两端共用的那几件事：书往哪儿拷、怎么认出一份副本、删一本书删掉哪些东西。
//!
//! 判据只有一条：**库里的行一定删，文件只删我们自己拷进来的那份**。两端现在都是
//! 「先复制到书库再入库」（见 [`stage`]），所以删掉一本书不该牵连到读者自己手里
//! 那个原件 —— 原件在别的目录里，从书库里移掉一本书不等于把人家硬盘上的文件删了。

use std::path::{Path, PathBuf};

use folio_core::Library;
use folio_core::Store;

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

/// 本应用的 Android 包名。应用专属外部目录的路径里有它，
/// 必须与 `crates/folio-app/android/app/build.gradle.kts` 的 `applicationId` 一致。
pub const PACKAGE: &str = "dev.folio.reader";

/// 能当书库用的候选目录：本应用**真的写得进去**的那几个。
///
/// 为什么是候选而不是文件夹选择器：Android 的 SAF 选目录返回的是 tree URI
/// （`content://…`）而不是文件系统路径，而拷副本用的是 `std::fs` —— fork 的
/// `prompt_for_paths` 正因此显式拒绝 `directories`。要真支持任选目录，得在 Java 侧
/// 另写一段「往 tree 里落文件」的实现。
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

/// 把选中的文件复制进书库，返回那份常驻副本的路径。
///
/// 复制而不是引用，是为了让「书还在」这件事不依赖原文件之后被移走、改名或删掉；
/// 入库的那份内容（章节、图片、CSS）本来就在 SQLite 里，`books.path` 真正的作用是
/// **书的身份**：同一路径重复导入算更新，不会多出第二本。
///
/// 副本保留原文件名，代价是**同名即同书** —— 两本都叫 `book.epub` 的不同文件会互相
/// 覆盖。这比「每次导入都多出一格」好修，也好向读者解释。
pub fn stage(source: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let name = source
        .file_name()
        .ok_or_else(|| std::io::Error::other("选择结果没有文件名"))?;
    let dest = dir.join(name);
    std::fs::copy(source, &dest)?;
    Ok(dest)
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
pub fn delete_book(store: &Store, id: i64, staged_dir: Option<&Path>) -> folio_core::Result<()> {
    let path = store.book(id).ok().flatten().map(|row| row.path);
    store.remove_book(id)?;
    let copy = path
        .as_deref()
        .and_then(|path| staged_dir.and_then(|dir| owned_copy(path, dir)));
    if let Some(copy) = copy
        && let Err(err) = std::fs::remove_file(&copy)
    {
        log::warn!("delete_book: 副本 {copy:?} 没删掉：{err}");
    }
    Ok(())
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
    //! 删一本书里最贵的那条判据：**只有应用自己收下的文件才跟着删**。
    //!
    //! 判错的代价不对称。库里存的路径在两端长得一样 —— 移动端是
    //! `<数据根>/books/x.epub`（导入时我们拷的），桌面端是 `/home/…/我的书/x.epub`
    //! （读者自己的）。前者不删是留垃圾，后者删了是删掉人家硬盘上的原件，所以两头都要钉住。

    use std::path::Path;

    use crate::ui::shared::library::{delete_book, owned_copy, stage, target_dir};
    use folio_core::Library;
    use folio_core::Store;
    use folio_core::format::{Chapter, DocMeta, FlowDoc, Format};

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
            chapters: vec![Chapter {
                title: "第一章".into(),
                source: "正文".into(),
                spine_href: "c1.xhtml".into(),
            }],
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
        assert_eq!(target_dir(&Library { dir: "   ".into() }), None);
        assert_eq!(
            target_dir(&Library {
                dir: " /x/books ".into()
            }),
            Some(Path::new("/x/books").to_path_buf())
        );
    }

    #[test]
    fn staging_copies_the_file_into_the_library_under_its_own_name() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("原处").join("a.epub");
        touch(&source);
        let books = dir.path().join("books");

        let dest = stage(&source, &books).unwrap();

        assert_eq!(dest, books.join("a.epub"), "副本保留原文件名");
        assert!(source.exists(), "原件不该被动");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            std::fs::read(&source).unwrap()
        );
        // 同一路径重复导入落到同一个文件上，`books.path` 才是书的身份。
        assert_eq!(stage(&source, &books).unwrap(), dest);
    }

    #[test]
    fn deleting_a_book_removes_the_row_its_chapters_and_our_copy() {
        let dir = tempfile::tempdir().unwrap();
        let books = dir.path().join("books");
        let staged = books.join("a.epub");
        touch(&staged);

        let store = Store::in_memory().unwrap();
        let id = store.add_book(&staged, &meta("甲")).unwrap();
        store.save_flow(id, &flow()).unwrap();

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
        store.save_flow(id, &flow()).unwrap();

        delete_book(&store, id, Some(&books)).unwrap();

        assert!(store.book(id).unwrap().is_none(), "从书库里移掉……");
        assert!(original.exists(), "……不等于把读者的原文件删了");
    }
}
