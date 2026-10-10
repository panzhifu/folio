//! 导入管线：把「读者选中的文件 → 可入库的书」做成一条可取消、可报进度的后台作业。
//!
//! 建在 [`crate::task`] 之上，只做两件与格式无关的事：**拷进书库目录**、**解析**。
//! 落库（`Store::add_book` / `save_document`）刻意留在调用方 —— `Store` 里的
//! `rusqlite::Connection` 是 `Send` 但**不是** `Sync`，把它搬到工作线程上要么挪走、
//! 要么套一层锁；而库写入是毫秒级的一步，留在它本来的线程上最干净。管线的职责到此为止：
//! 把最贵的拷贝与解析（解析一本 EPUB 可能是几十毫秒到几百毫秒）挪到后台。

use std::path::{Path, PathBuf};

use crate::document::Document;
use crate::error::{Error, Result};
use crate::task::{Cancel, Progress, TaskHandle, TaskManager};

/// 一个成功解析、等着入库的书。
pub struct Imported {
    /// 读者选中的原文件。
    pub source: PathBuf,
    /// 拷进书库后的副本（后续 `add_book` 记的就是它）。
    pub dest: PathBuf,
    pub title: String,
    /// 解析好的文档。落库交给调用方，见模块文档。
    pub document: Box<dyn Document>,
}

/// 一个源文件的结果。**逐个源各自成败**：一次选多本是常态，坏掉的那一本不该
/// 把其余的也挡在架外。
pub struct ImportOutcome {
    pub source: PathBuf,
    pub result: Result<Imported>,
}

/// 导入管线的本体：把 `sources` 逐个「拷进 `dest_dir` → 解析」。
///
/// - **进度**：每开始一个源报一次 `done/total`（配一句「导入《名字》」），收尾再报一次
///   完成。`total = sources.len()`。
/// - **取消**：在两个源**之间**检查 [`Cancel`]；置位就停下，已处理的那些结果照常返回。
///   单个源内部（拷贝 / 解析）不可中断 —— 那要各自的实现支持，见 `remote::Cancel` 的说明。
/// - **失败**：某个源解析不了只影响它自己（结果里是一条 `Err`），不中断整批。
///
/// 这个函数是同步阻塞的，直接被 [`submit_import`] 包成后台作业。
pub fn import_books(
    sources: &[PathBuf],
    dest_dir: &Path,
    progress: &Progress<Vec<ImportOutcome>>,
    cancel: &Cancel,
) -> Vec<ImportOutcome> {
    let total = sources.len() as u64;
    let mut outcomes = Vec::with_capacity(sources.len());
    for (done, source) in sources.iter().enumerate() {
        if cancel.triggered() {
            break;
        }
        let name = source.file_name().map_or_else(
            || source.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        progress.report(done as u64, total, format!("导入《{name}》"));
        outcomes.push(ImportOutcome {
            source: source.clone(),
            result: import_one(source, dest_dir),
        });
    }
    progress.report(total, total, "完成");
    outcomes
}

/// 把整批导入包成一个后台作业交给 `manager`。返回的 [`TaskHandle`] 就是这批导入的接口：
/// 看进度、取消、等完成。
pub fn submit_import(
    manager: &TaskManager<Vec<ImportOutcome>>,
    sources: Vec<PathBuf>,
    dest_dir: PathBuf,
) -> TaskHandle<Vec<ImportOutcome>> {
    manager.submit(move |progress, cancel| Ok(import_books(&sources, &dest_dir, progress, cancel)))
}

/// 一个源：拷进书库 → 解析。解析不了时那条 `Err` 会被上层收进 [`ImportOutcome`]。
fn import_one(source: &Path, dest_dir: &Path) -> Result<Imported> {
    let dest = stage(source, dest_dir)?;
    let document = crate::document::open(&dest)?;
    let title = document.meta().title.clone();
    Ok(Imported {
        source: source.to_path_buf(),
        dest,
        title,
        document,
    })
}

/// 把 `source` 拷进 `dir`，返回副本路径。文件名保持不变，同名的后到覆盖先到
/// （与旧入口一致；真正的「重名怎么办」是书库策略，不在这条通用管线里）。
fn stage(source: &Path, dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    let name = source.file_name().ok_or_else(|| {
        io_err(
            source,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "选择结果里没有文件名"),
        )
    })?;
    let dest = dir.join(name);
    std::fs::copy(source, &dest).map_err(|e| io_err(&dest, e))?;
    Ok(dest)
}

fn io_err(path: &Path, error: std::io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source: Box::new(error),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;

    use super::{ImportOutcome, Progress, import_books, submit_import};
    use crate::task::{Cancel, TaskEvent, TaskManager};

    fn drain<R>(events: Receiver<TaskEvent<R>>) -> Vec<TaskEvent<R>> {
        events.into_iter().collect()
    }

    fn outcomes_of(events: Vec<TaskEvent<Vec<ImportOutcome>>>) -> Vec<ImportOutcome> {
        events
            .into_iter()
            .find_map(|e| match e {
                TaskEvent::Finished(Ok(o)) => Some(o),
                _ => None,
            })
            .expect("导入作业应以 Finished(Ok(..)) 收尾")
    }

    /// 每个源都被拷进书库、解析成文档。
    #[test]
    fn import_stages_and_parses_each_source() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), "第一本\n").unwrap();
        std::fs::write(src.join("b.txt"), "第二本\n").unwrap();
        let sources = vec![src.join("a.txt"), src.join("b.txt")];
        let lib = tmp.path().join("lib");

        let manager = TaskManager::<Vec<ImportOutcome>>::new(1);
        let handle = submit_import(&manager, sources.clone(), lib.clone());
        let outcomes = outcomes_of(drain(handle.events));

        assert_eq!(outcomes.len(), 2);
        for (outcome, source) in outcomes.into_iter().zip(sources) {
            assert_eq!(outcome.source, source);
            let imported = outcome
                .result
                .unwrap_or_else(|e| panic!("纯文本该能解析：{e}"));
            assert!(imported.dest.exists(), "副本该落在书库目录里");
            assert!(imported.dest.starts_with(&lib));
        }
    }

    /// 坏掉的那一本不挡其余的：一批里有不认识的文件，好的照样入库。
    #[test]
    fn a_bad_source_does_not_sink_the_batch() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("good.txt"), "好的\n").unwrap();
        std::fs::write(src.join("bad.mobi"), "不认识的扩展名\n").unwrap();
        let sources = vec![src.join("good.txt"), src.join("bad.mobi")];

        let manager = TaskManager::<Vec<ImportOutcome>>::new(1);
        let handle = submit_import(&manager, sources, tmp.path().join("lib"));
        let outcomes = outcomes_of(drain(handle.events));

        assert_eq!(outcomes.len(), 2, "两个源都要有结果");
        assert!(outcomes.iter().any(|o| o.result.is_ok()), "好的那本要成功");
        assert!(
            outcomes.iter().any(|o| o.result.is_err()),
            "坏的那本要是一条 Err，而不是把整批拖垮"
        );
    }

    /// 一上来就取消：一个源都不碰（取消在源与源之间检查）。
    #[test]
    fn cancel_stops_the_pipeline_between_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let sources: Vec<std::path::PathBuf> = (0..3)
            .map(|i| {
                let path = src.join(format!("{i}.txt"));
                std::fs::write(&path, "正文\n").unwrap();
                path
            })
            .collect();

        let cancel = Cancel::new();
        cancel.trigger();
        let outcomes = import_books(
            &sources,
            &tmp.path().join("lib"),
            &Progress::detached(),
            &cancel,
        );
        assert!(outcomes.is_empty(), "取消后不该再处理任何源");
    }
}
