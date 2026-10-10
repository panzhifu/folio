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

/// 引擎的三层。`format/` 与 `document/` 不在其列：前者正是干「认格式」这件事的地方，
/// 后者是格式差异的出口。
const ENGINE_DIRS: &[&str] = &["src/layout", "src/css", "src/dom"];

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
