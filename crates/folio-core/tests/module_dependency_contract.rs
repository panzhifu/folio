//! 架构契约：模块依赖的三条禁令。
//!
//! `lib.rs` 头注释写的依赖方向靠人记必然漂移（《后端架构评审与优化》§2.8），
//! 这里把它钉成可执行的：每个分区的源码里不许出现指向被禁模块的 `crate::…` 路径。
//! 与 `engine_input_contract.rs` 同一套扫法：剥掉 `//` 行注释后按子串匹配 ——
//! `use crate::store::X` 与内联的 `crate::store::X` 一视同仁，注释里提到不算。
//!
//! 白名单曾记**已知欠账**，每条注明由哪一刀还；还清一条删一条，现在已删空，
//! 这条门是严格的。新增越界直接红，不许再进白名单。

use std::path::{Path, PathBuf};

/// (分区目录, 被禁的路径前缀, 白名单: (相对 src 的文件路径, 欠账说明))
type Ban = (
    &'static str,
    &'static [&'static str],
    &'static [(&'static str, &'static str)],
);

const BANS: &[Ban] = &[
    // 内容区只往「下」依赖（folio-format / folio-model / infra），不认识存储与导入。
    ("content", &["crate::store", "crate::ingest"], &[]),
    // 存储层不认识网络协议：同步 DTO 归 sync，SQL 只吃数据形状。
    ("store", &["crate::net"], &[]),
    // infra 是叶子，被所有人依赖而不依赖任何功能分区。组合根与错误聚合搬到
    // crate 根（刀 5）、novel / ImageScale 归位 folio-model（刀 6）之后，
    // 这一格已无欠账，白名单为空。
    (
        "infra",
        &[
            "crate::store",
            "crate::ingest",
            "crate::net",
            "crate::content",
            "crate::layout",
        ],
        &[],
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
