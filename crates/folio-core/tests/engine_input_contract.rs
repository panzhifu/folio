//! 架构契约：**排版引擎不认识「格式」**。
//!
//! 这是「一种中间表示、多个 importer」能成立的前提：引擎只吃 DOM + 样式 + 度量 + 视口，
//! 格式差异在 provider 里就被抹平成 `Content::{ Reflow, Fixed }`。一旦引擎里出现
//! `if format == Format::Epub`，新增一个格式就要改引擎 —— 多格式支持的成本从
//! 「加一个 provider」变成「改引擎」，而引擎是所有格式共用的那块地方。
//!
//! 为什么用扫源码的测试而不是类型系统：`Format` 是公开类型，任何模块都能 `use`，
//! Rust 没有「只有 provider 模块能看见」这种可见性粒度。所以用一条会失败得很明确的
//! 守卫钉住它 —— 注释里提到 `Format` 不算（先剥掉 `//` 再说），这样解释性文字可以
//! 照写，真正的代码引用跑不掉。

use std::path::{Path, PathBuf};

/// 引擎的三层。`content/format/` 与 `content/document/` 不在其列：前者正是干
/// 「认格式」这件事的地方，后者是格式差异的出口。
///
/// 路径跟着 `content/` 那次分区走（`css/` 与 `dom/` 现在都在 `src/content/` 底下，
/// 而 `css/` 进一步收在 `content/format/` 里 —— 它是**排版用的样式层**，
/// 与格式无关，但和 `format` 一起被搬了）。这条断言本身就是「路径写错了会红」，
/// 所以它在分区之后立刻红了一次，是它该做的事。
const ENGINE_DIRS: &[&str] = &["src/layout", "src/content/format/css", "src/content/dom"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_layout_engine_never_learns_about_formats() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenses = Vec::new();

    for dir in ENGINE_DIRS {
        let mut files = Vec::new();
        rust_files(&root.join(dir), &mut files);
        assert!(!files.is_empty(), "{dir} 里一个 .rs 都没有，路径写错了？");

        for file in files {
            let text = std::fs::read_to_string(&file).expect("源码该是 UTF-8");
            for (ix, line) in text.lines().enumerate() {
                // 行注释与文档注释都长 `//`，剥掉之后剩下的才是真正的代码。
                let code = line.split("//").next().unwrap_or("");
                if code.contains("Format") {
                    offenses.push(format!(
                        "{}:{}  {}",
                        file.strip_prefix(root).unwrap_or(&file).display(),
                        ix + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        offenses.is_empty(),
        "排版引擎里出现了「格式」—— 格式差异该在 provider 里抹平，\n\
         引擎只认 DOM + 样式 + 度量。要么把这段挪进 provider，要么把它变成\n\
         与来源无关的引擎能力（例如某个 CSS 属性）。\n{}",
        offenses.join("\n")
    );
}

/// **阅读器不许自己从硬编码路径抓书来读。**
///
/// 这条守的是一个真实事故：`ReaderSession::boot` 曾经在几个写死的目录里依次翻
/// `folio-sample.{md,txt,epub}`，找到就打开。那几个目录里包含 `/sdcard/Download`，
/// 而「全盘扫描」（`scan_roots(all_files = true)`）扫的正是 `/storage/emulated/0`
/// —— 于是同一个文件被两处认领：阅读器把它当启动占位读，导入管线把它当一本书收进库。
///
/// 实测症状：书架顶上多出一本《版心样本》，删掉之后下次全盘扫描又回来 —— 因为生成它
/// 的那份文件还在 `Download` 里，而**没有任何一处代码说得出它是怎么进库的**。
///
/// 现在的形状：书一律从书架点开（`ReaderSession::open_book`），`boot` 只进占位屏。
/// 这条断言把「别再偷偷翻文件」写成可执行的：`crates/folio-app/src` 里出现
/// `folio-sample` 就红。
///
/// 注意它扫的是**应用源码**，不是 core —— 这个约定属于界面那一层。
#[test]
fn the_reader_never_opens_a_book_from_a_hardcoded_path() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("core 上一级是 crates/")
        .join("folio-app/src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(!files.is_empty(), "找不到 folio-app 的源码，路径写错了？");

    let mut offenses = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("源码该是 UTF-8");
        for (ix, line) in text.lines().enumerate() {
            // 注释里解释这条事故是允许的（本文件的文档注释就是），
            // 被禁的是**代码**里再出现那个文件名。
            let code = line.split("//").next().unwrap_or("");
            if code.contains("folio-sample") {
                offenses.push(format!("{}:{}  {}", file.display(), ix + 1, line.trim()));
            }
        }
    }
    assert!(
        offenses.is_empty(),
        "阅读器又在从硬编码路径抓书了（这会让「全盘扫描」把同一个文件当书收进库）：\n{}",
        offenses.join("\n")
    );
}
