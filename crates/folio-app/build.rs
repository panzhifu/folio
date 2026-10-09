//! `folio-app` 的构建脚本：把上游 gpui-kit 的主题集嵌进来。
//!
//! 主题 JSON **不复制进本仓库**。它们只存在于 gpui-kit 的源码目录里 —— 发布到
//! crates.io 的那个 crate 并不带 `themes/` —— 所以这里用 `cargo metadata` 问出
//! gpui-kit 的源码到底在哪，把每个主题拷到 `OUT_DIR`，再生成一份由 `theme.rs`
//! `include!` 的清单。这样仓库里的主题永远和依赖里的那份一致，也不会因为多了一副本而漂移。
//!
//! `FOLIO_THEMES_DIR` 可以直接指到一个 `themes/` 目录，用来覆盖上面的查找。
//!
//! 找不到**不当成错误**：框架自带的 Default Light / Default Dark 本来就在
//! `ThemeRegistry` 里，缺的只是那套可选的额外主题，选择器变短而已。
//!
//! 只用 `std`：为构建脚本加一个依赖会重新解析 lockfile，可能把钉住的 gpui-kit
//! commit 挪走，所以 `cargo metadata` 的 JSON 直接扫 `manifest_path` 字符串，不上 serde。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

fn main() {
    // 自定义 cfg：`mobile` = Android / iOS / HarmonyOS。
    // `lib.rs` 用它把桌面模块与 `mobile` 模块互斥编译。必须显式声明 check-cfg，
    // 否则 rustc 会对 `#[cfg(mobile)]` 报 unexpected_cfgs。
    println!("cargo:rustc-check-cfg=cfg(mobile)");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if matches!(target_os.as_str(), "android" | "ios") || target_env == "ohos" {
        println!("cargo:rustc-cfg=mobile");
    }

    println!("cargo:rerun-if-env-changed=FOLIO_THEMES_DIR");
    println!("cargo:rerun-if-changed=../../Cargo.lock");
    // 文案表是 `rust_i18n::i18n!` 在编译期读的，cargo 自己不知道这件事：
    // 不写这一行，改了 `locales/*.toml` 也不会重编，旧文案会一直留在二进制里。
    println!("cargo:rerun-if-changed=../folio-core/locales");
    // 关于页嵌的是 `docs/` 里那两份原文（见 `ui/shared/about.rs`），所以它们改了要重编，
    // 否则二进制里留着旧的一份，而「实际情况」正是那一页唯一的存在理由。
    println!("cargo:rerun-if-changed=../../docs");
    emit_dependency_env();

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR 由 cargo 设好"));
    let mut generated = String::from(
        "// 由 build.rs 生成 —— 构建时嵌入的上游 gpui-kit 主题集。\n\
         pub(super) static BUILTIN_THEME_JSON: &[&str] = &[\n",
    );

    match locate_themes_dir() {
        Some(dir) => {
            let dest = out.join("themes");
            fs::create_dir_all(&dest).expect("建出 OUT_DIR/themes");
            for stem in theme_stems(&dir) {
                let src = dir.join(format!("{stem}.json"));
                let Ok(json) = fs::read_to_string(&src) else {
                    continue;
                };
                fs::write(dest.join(format!("{stem}.json")), json).expect("写入主题副本");
                writeln!(
                    generated,
                    "    include_str!(concat!(env!(\"OUT_DIR\"), \"/themes/{stem}.json\")),"
                )
                .expect("写 String 不会失败");
            }
        }
        None => println!(
            "cargo:warning=没找到 gpui-kit 的 themes/ 目录；只有框架自带的两套主题可选\
             （设 FOLIO_THEMES_DIR 指到一个 themes/ 目录即可）"
        ),
    }

    generated.push_str("];\n");
    fs::write(out.join("folio_themes.rs"), generated).expect("写出 folio_themes.rs");
}

/// 找 gpui-kit 的 `themes/`：先看显式覆盖，再问 `cargo metadata` 依赖真正的源码位置。
fn locate_themes_dir() -> Option<PathBuf> {
    if let Some(dir) = env::var_os("FOLIO_THEMES_DIR") {
        return Some(PathBuf::from(dir));
    }
    let metadata = cargo_metadata()?;
    // 主题目录在 checkout 的根部，和 gpui-kit 这个 crate 平级而不是在它里面，
    // 所以从它的 manifest 往上走，直到遇见一个有内容的 `themes/`。
    for manifest in manifest_paths(&metadata).filter(|path| path.contains("gpui-kit")) {
        for ancestor in Path::new(manifest).ancestors() {
            let themes = ancestor.join("themes");
            if themes.join("catppuccin.json").is_file() {
                return Some(themes);
            }
        }
    }
    None
}

/// 跑 `cargo metadata`（带 `--locked`，保证构建脚本不会改 lock），返回它的 stdout。
fn cargo_metadata() -> Option<String> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version=1", "--locked"])
        .current_dir(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())?
}

/// 逐个交出 metadata JSON 里的 `manifest_path` 字符串值。会构建 GUI 的平台上
/// 这些路径不含需要 JSON 转义的字符，所以直接扫串就够了，不必为此加一个 serde 依赖。
fn manifest_paths(json: &str) -> impl Iterator<Item = &str> {
    const KEY: &str = "\"manifest_path\":\"";
    json.match_indices(KEY).filter_map(|(i, _)| {
        let rest = &json[i + KEY.len()..];
        rest.find('"').map(|end| &rest[..end])
    })
}

/// 目录里每个 `<stem>.json` 的 stem，排过序 —— 嵌入顺序就是选择器里的顺序，
/// 不能随目录遍历顺序变。
fn theme_stems(dir: &Path) -> Vec<String> {
    println!("cargo:rerun-if-changed={}", dir.display());
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut stems: Vec<String> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .collect();
    stems.sort();
    stems
}

/// 把锁文件里的依赖身份抄成编译期常量，给关于页显示用。
///
/// 为什么在构建期做而不是让界面写死：那几串版本号和 commit 是**这份二进制**到底是什么
/// 编出来的唯一凭据。手写的一份和 `Cargo.lock` 迟早分叉，而分叉的恰好是最需要准的信息。
/// 锁文件已经挂了 `rerun-if-changed`，所以换依赖时这些常量一定跟着变。
fn emit_dependency_env() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let Ok(lock) = fs::read_to_string(manifest.join("../../Cargo.lock")) else {
        // 读不到锁文件不算错误：关于页那几行会显示「未知」，界面照常起。
        return;
    };
    for (package, variable) in [
        ("gpui-pre", "FOLIO_GPUI_PRE"),
        ("gpui-kit", "FOLIO_KIT"),
        ("gpui-pre-mobile", "FOLIO_MOBILE"),
    ] {
        let version = lock_field(&lock, package, "version").unwrap_or_default();
        // git 源的 `source` 以 `#<完整 commit>` 结尾；发布版没有这一段。
        let rev = lock_field(&lock, package, "source")
            .and_then(|source| source.rsplit_once('#').map(|(_, rev)| rev.to_string()))
            .unwrap_or_default();
        let rev = if rev.is_empty() {
            String::new()
        } else {
            format!(" ({})", &rev[..7.min(rev.len())])
        };
        println!("cargo:rustc-env={variable}={version}{rev}");
    }
}

/// 锁文件里某个 `[[package]]` 块下的一个字段值（不含两侧引号）。
fn lock_field(lock: &str, package: &str, field: &str) -> Option<String> {
    let header = format!("name = \"{package}\"\n");
    let start = lock.find(&header)?;
    let block = &lock[start
        ..lock[start..]
            .find("\n\n[[package]]")
            .map_or(lock.len(), |end| start + end)];
    let prefix = format!("{field} = \"");
    block
        .lines()
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .map(|value| value.trim_end_matches('"').to_string())
}
