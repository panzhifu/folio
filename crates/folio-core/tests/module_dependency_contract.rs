//! 架构契约：模块依赖的三条禁令。
//!
//! `lib.rs` 头注释写的依赖方向靠人记必然漂移（《后端架构评审与优化》§2.8），
//! 这里把它钉成可执行的：每个分区的源码里不许出现指向被禁模块的 `crate::…` 路径。
//! 与 `engine_input_contract.rs` 同一套扫法：剥掉 `//` 行注释后按子串匹配 ——
//! `use crate::store::X` 与内联的 `crate::store::X` 一视同仁，注释里提到不算。
//!
//! 白名单是**已知欠账**，每条注明由哪一刀还（`doc/后端架构评审与优化.md`）；
//! 还清一条删一条，删空之后这条门就是严格的。新增越界直接红，不许再进白名单。

use std::path::{Path, PathBuf};

/// (分区目录, 被禁的路径前缀, 白名单: (相对 src 的文件路径, 欠账说明))
const BANS: &[(&str, &[&str], &[(&str, &str)])] = &[
    // 内容区只往「下」依赖（folio-format / folio-model / infra），不认识存储与导入。
    (
        "content",
        &["crate::store", "crate::ingest"],
        &[
            // 刀 3 还：批注类型已在 folio-model，改 folio_model 路径后删。
            ("content/annotation.rs", "批注类型还走 store 旧路径"),
        ],
    ),
    // 存储层不认识网络协议：同步 DTO 归 sync，SQL 只吃数据形状。
    (
        "store",
        &["crate::net"],
        &[
            // 刀 4 还：BookEntry 搬进 sync 后改 crate::sync::BookEntry。
            ("store/sync.rs", "同步 DTO 还住在 net::lansync"),
        ],
    ),
    // infra 是叶子，被所有人依赖而不依赖任何功能分区；组合根与错误聚合
    // 搬到 crate 根之后（刀 5），这里剩下的应该只有 config 的 account 一处
    // （account 不在禁单里，见 infra/mod.rs 头注释的 settings 层欠账）。
    (
        "infra",
        &[
            "crate::store",
            "crate::ingest",
            "crate::net",
            "crate::content",
            "crate::layout",
        ],
        &[
            // 刀 5 还：组合根整体搬 crate 根。
            ("infra/services.rs", "组合根还住在 infra"),
            // 刀 5 还：错误聚合随组合根出 infra。
            ("infra/error.rs", "错误聚合还住在 infra"),
            // 刀 6 还：novel / ImageScale 类型搬 folio-model。
            ("infra/config.rs", "novel 与 ImageScale 还引用 content/store 类型"),
            // 刀 3 还：测试里的 AnnotationStyle 改 folio_model 路径。
            ("infra/book_settings.rs", "测试引用 AnnotationStyle 走 store 路径"),
        ],
    ),
];

fn rust_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().is_some_and(|e| e == "rs") {
            out.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path.to_path_buf());
        }
    }
}

#[test]
fn module_dependencies_stay_one_directional() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenses = Vec::new();

    for (dir, banned, allowlist) in BANS {
        let mut files = Vec::new();
        rust_files(&src.join(dir), &mut files);
        assert!(
            !files.is_empty(),
            "src/{dir} 里一个 .rs 都没有，路径写错了？"
        );

        for file in files {
            let rel = file
                .strip_prefix(&src)
                .expect("扫到的文件都在 src 下")
                .to_string_lossy()
                .replace('\\', "/");
            // 白名单按文件整份豁免：都是几天内要还的欠账，还一刀删一条。
            if allowlist.iter().any(|(f, _)| *f == rel) {
                continue;
            }
            let text = std::fs::read_to_string(&file).expect("源码该是 UTF-8");
            for (ix, line) in text.lines().enumerate() {
                // 行注释与文档注释都长 `//`，剥掉之后剩下的才是真正的代码。
                let code = line.split("//").next().unwrap_or("");
                for needle in *banned {
                    if code.contains(needle) {
                        offenses.push(format!(
                            "folio-core/src/{rel}:{}  违禁边 `{needle}`  {}",
                            ix + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }
    }

    assert!(
        offenses.is_empty(),
        "模块依赖出现了被禁的边 —— 方向写在 lib.rs 头注释里，这里负责让它成立。\n\
         新依赖要么改成从 folio-model / folio-base / folio-format 拿类型，要么把它\n\
         属于的组合根 / 错误聚合挪出 infra。白名单是欠账，不许往里加新条目。\n{}",
        offenses.join("\n")
    );
}
