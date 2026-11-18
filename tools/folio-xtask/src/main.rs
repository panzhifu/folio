mod boundaries;
mod governance;
mod inventory;
mod locales;
mod release;
mod skills;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    match command.as_str() {
        "governance" => {
            let base = optional_base(&mut args)?;
            governance::check(&root, base.as_deref())?;
        }
        "boundaries" => {
            no_more(&mut args)?;
            governance::check_boundaries(&root)?;
        }
        "snapshot" => {
            no_more(&mut args)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&inventory::snapshot(&root)?)?
            );
        }
        "change-plan" => {
            no_more(&mut args)?;
            change_plan(&root)?;
        }
        "i18n" => {
            no_more(&mut args)?;
            locales::check(&root)?;
        }
        "release-check" => {
            let mut base = None;
            let mut artifacts = None;
            while let Some(flag) = args.next() {
                let value = args.next().ok_or("release-check 参数缺少值")?;
                match flag.as_str() {
                    "--base" => base = Some(value),
                    "--artifacts" => artifacts = Some(PathBuf::from(value)),
                    _ => return Err(format!("未知参数：{flag}").into()),
                }
            }
            release::check(&root, base.as_deref(), artifacts.as_deref())?;
        }
        "comment-check" => {
            let base = args
                .next()
                .ok_or("用法：cargo xtask comment-check <base> <path>...")?;
            let paths: Vec<_> = args.collect();
            if paths.is_empty() {
                return Err("需要至少一个文件".into());
            }
            for path in paths {
                governance::repository_path(&path)?;
                let old = git(&root, &["show", &format!("{base}:{path}")])?;
                let new = std::fs::read_to_string(root.join(&path))?;
                inventory::same_semantics(&path, &old, &new)?;
                println!("语义一致：{path}");
            }
        }
        "help" | "--help" | "-h" => println!(
            "cargo xtask governance [--base <git-ref>]\n\
             cargo xtask boundaries\n\
             cargo xtask snapshot  # 只输出债务快照，不覆盖基线\n\
             cargo xtask change-plan\n\
             cargo xtask i18n\n\
             cargo xtask release-check [--base <git-ref>] [--artifacts <dir>]\n\
             cargo xtask comment-check <base> <path>..."
        ),
        _ => return Err(format!("未知命令：{command}").into()),
    }
    Ok(())
}

fn optional_base(args: &mut impl Iterator<Item = String>) -> Result<Option<String>> {
    match args.next().as_deref() {
        None => Ok(None),
        Some("--base") => {
            let base = args.next().ok_or("--base 缺少 revision")?;
            no_more(args)?;
            Ok(Some(base))
        }
        _ => Err("只接受 --base <git-ref>".into()),
    }
}

fn no_more(args: &mut impl Iterator<Item = String>) -> Result<()> {
    if args.next().is_some() {
        return Err("有多余参数".into());
    }
    Ok(())
}

pub fn git(root: &Path, args: &[&str]) -> Result<String> {
    let result = Command::new("git").args(args).current_dir(root).output()?;
    if !result.status.success() {
        return Err(format!(
            "git {}：{}",
            args.join(" "),
            String::from_utf8_lossy(&result.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(result.stdout)?)
}

fn change_plan(root: &Path) -> Result<()> {
    if root.join(".jj").exists() {
        let result = Command::new("jj")
            .args(["diff", "--summary"])
            .current_dir(root)
            .output()?;
        if !result.status.success() {
            return Err("jj diff 失败，未生成变更范围".into());
        }
        println!("jj 当前 change：\n{}", String::from_utf8(result.stdout)?);
    } else {
        println!(
            "普通 Git checkout（未检查 jj change）：\n{}",
            git(root, &["status", "--short"])?
        );
    }
    println!(
        "按实际路径选择验证：\n\
        规则/文档/skills：cargo xtask governance\n\
        crate/UI 边界：cargo xtask boundaries\n\
        Rust：cargo test -p <crate> --lib <实际用例名>，或 --test <target> <用例名>\n\
        纯注释：cargo xtask comment-check <base> <path>...\n\
        这里只输出建议，不自动测试、提交、推送或操作手机。全量测试遵循 AGENTS.md。"
    );
    Ok(())
}
