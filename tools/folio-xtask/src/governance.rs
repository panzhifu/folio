use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use serde::Deserialize;

use crate::{Result, git, inventory, skills};

pub const MANIFEST: &str = "config/folio-governance.json";
pub const BASELINE: &str = "config/folio-debt-baseline.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    documents: BTreeMap<String, String>,
    rules: BTreeMap<String, Rule>,
    skills: Vec<skills::Skill>,
    product_dependencies: BTreeMap<String, Vec<String>>,
    rust_ci: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    owner: String,
    scope: Vec<String>,
    enforcement: String,
}

pub fn repository_path(value: &str) -> Result<()> {
    if value.is_empty()
        || value.contains(['\\', ':'])
        || Path::new(value)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(format!("非法仓库相对路径：{value}").into());
    }
    Ok(())
}

fn read_manifest(root: &Path) -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(root.join(MANIFEST))?)?;
    if manifest.version != 1 {
        return Err("未知治理注册表版本".into());
    }
    Ok(manifest)
}

pub fn check(root: &Path, base: Option<&str>) -> Result<()> {
    let manifest = read_manifest(root)?;
    check_documents(root, &manifest)?;
    check_doc_paths(root)?;
    check_facts(root, &manifest)?;
    crate::locales::check(root)?;
    skills::check(root, &manifest.skills)?;
    check_boundaries(root)?;
    let current: inventory::Baseline =
        serde_json::from_str(&std::fs::read_to_string(root.join(BASELINE))?)?;
    match base {
        Some(base) => {
            git(
                root,
                &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
            )?;
            let names = git(root, &["ls-tree", "--name-only", base, "--", BASELINE])?;
            if names.trim().is_empty() {
                println!("基准 revision 尚无债务基线：首次引入，需要审阅初始快照。");
            } else {
                let old: inventory::Baseline =
                    serde_json::from_str(&git(root, &["show", &format!("{base}:{BASELINE}")])?)?;
                inventory::ratchet(&current, &old)?;
            }
        }
        None => println!("未提供 --base，仅验证当前基线；未验证历史基线是否放宽。"),
    }
    println!(
        "治理检查通过：{} 条规则、{} 份文档、{} 条技能；{} 个超限文件、{} 条遗留边界。",
        manifest.rules.len(),
        manifest.documents.len(),
        manifest.skills.len(),
        current.files.len(),
        current.edges.len()
    );
    Ok(())
}

pub fn check_boundaries(root: &Path) -> Result<()> {
    let manifest = read_manifest(root)?;
    check_dependencies(root, &manifest)?;
    let baseline: inventory::Baseline =
        serde_json::from_str(&std::fs::read_to_string(root.join(BASELINE))?)?;
    inventory::check_snapshot(&inventory::snapshot(root)?, &baseline)?;
    println!("crate/UI 边界与文件规模基线通过（遗留允许零增长）。");
    Ok(())
}

fn markdown_files(root: &Path, dir: &Path, files: &mut BTreeSet<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_symlink() {
            return Err(format!("文档不跟随链接：{}", path.display()).into());
        }
        if path.is_dir() {
            markdown_files(root, &path, files)?;
        } else if path.extension().is_some_and(|e| e == "md") {
            files.insert(
                path.strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    Ok(())
}

fn check_documents(root: &Path, manifest: &Manifest) -> Result<()> {
    let mut found = BTreeSet::new();
    markdown_files(root, &root.join("doc"), &mut found)?;
    if found != manifest.documents.keys().cloned().collect() {
        return Err("FOLIO-GOV-001 doc/ 文档与注册表不一致".into());
    }
    for (path, kind) in &manifest.documents {
        repository_path(path)?;
        if ![
            "standard",
            "architecture",
            "guide",
            "plan",
            "progress",
            "adr",
        ]
        .contains(&kind.as_str())
        {
            return Err(format!("未知文档分类：{path} {kind}").into());
        }
    }
    let standard = std::fs::read_to_string(root.join("doc/工程规范.md"))?;
    let ids: BTreeSet<_> = standard
        .lines()
        .filter_map(|line| line.strip_prefix("### "))
        .filter_map(|s| s.split_once(':').map(|p| p.0.to_string()))
        .collect();
    if ids.len()
        != standard
            .lines()
            .filter(|l| l.starts_with("### FOLIO-"))
            .count()
        || ids != manifest.rules.keys().cloned().collect()
    {
        return Err("FOLIO-GOV-003 工程规范 Rule ID 重复或与注册表不一致".into());
    }
    for (id, rule) in &manifest.rules {
        if rule.owner.trim().is_empty()
            || rule.scope.is_empty()
            || rule.scope.iter().any(|s| s.trim().is_empty())
        {
            return Err(format!("{id} 缺少 owner/scope").into());
        }
        if !["governance", "boundaries", "targeted-tests", "review"]
            .contains(&rule.enforcement.as_str())
        {
            return Err(format!("{id} 未知执行方式 {}", rule.enforcement).into());
        }
    }
    let mut link_files: Vec<&str> = manifest.documents.keys().map(String::as_str).collect();
    link_files.extend(["AGENTS.md", "README.md", "CHANGELOG.md"]);
    for entry in link_files {
        let text = std::fs::read_to_string(root.join(entry))?;
        check_links(root, entry, &text)?;
    }
    let entry = std::fs::read_to_string(root.join("AGENTS.md"))?;
    for needed in ["doc/工程规范.md", MANIFEST, "cargo xtask governance"] {
        if !entry.contains(needed) {
            return Err(format!("AGENTS.md 缺少入口 {needed}").into());
        }
    }
    Ok(())
}

pub fn check_links(root: &Path, file: &str, text: &str) -> Result<()> {
    for part in text.split("](").skip(1) {
        let Some((target, _)) = part.split_once(')') else {
            continue;
        };
        let target = target
            .trim_matches(['<', '>'])
            .split('#')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .next()
            .unwrap_or("");
        if target.is_empty() || target.contains("://") || target.starts_with("mailto:") {
            continue;
        }
        let parent = Path::new(file).parent().ok_or("链接文件没有父目录")?;
        if !root.join(parent).join(target).exists() {
            return Err(format!("FOLIO-GOV-001 {file} 失效链接：{target}").into());
        }
    }
    Ok(())
}

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_symlink() {
            continue;
        }
        if path.is_dir() {
            rust_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

fn check_doc_paths(root: &Path) -> Result<()> {
    let mut files = Vec::new();
    for dir in ["crates", "tools"] {
        rust_files(&root.join(dir), &mut files)?;
    }
    files.sort();
    for path in files {
        let rel = path
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&path)?;
        for (index, line) in text.lines().enumerate() {
            for part in line.split('`').skip(1).step_by(2) {
                if !part.ends_with(".md") {
                    continue;
                }
                if !(part.starts_with("doc/") || part.starts_with("plans/")) {
                    continue;
                }
                if !root.join(part).is_file() {
                    return Err(format!(
                        "FOLIO-GOV-005 {rel}:{} 注释指向不存在的文档：{part}",
                        index + 1
                    )
                    .into());
                }
            }
        }
    }
    Ok(())
}

fn toml_at(root: &Path, path: &str) -> Result<toml::Value> {
    Ok(std::fs::read_to_string(root.join(path))?.parse()?)
}

fn dependencies(value: &toml::Value, out: &mut Vec<(String, toml::Value)>) {
    if let Some(table) = value.as_table() {
        for (key, value) in table {
            if key == "dependencies" || key == "build-dependencies" {
                if let Some(table) = value.as_table() {
                    out.extend(table.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            } else if key != "dev-dependencies" {
                dependencies(value, out);
            }
        }
    }
}

fn check_dependencies(root: &Path, manifest: &Manifest) -> Result<()> {
    let workspace = toml_at(root, "Cargo.toml")?;
    let members = workspace["workspace"]["members"]
        .as_array()
        .ok_or("缺少 workspace.members")?;
    let mut names = BTreeSet::new();
    for member in members {
        let dir = member.as_str().ok_or("member 不是字符串")?;
        repository_path(dir)?;
        let value = toml_at(root, &format!("{dir}/Cargo.toml"))?;
        let name = value["package"]["name"].as_str().ok_or("crate 缺少名字")?;
        let mut deps = Vec::new();
        dependencies(&value, &mut deps);
        for (alias, value) in &mut deps {
            let inherited = value.get("workspace").and_then(|v| v.as_bool()) == Some(true);
            if inherited {
                *value = workspace["workspace"]["dependencies"]
                    .get(alias.as_str())
                    .ok_or_else(|| format!("缺少 workspace dependency {alias}"))?
                    .clone();
            }
            if let Some(path) = value.get("path").and_then(|v| v.as_str()) {
                let base = if inherited {
                    root.to_path_buf()
                } else {
                    root.join(dir)
                };
                let package: toml::Value =
                    std::fs::read_to_string(base.join(path).join("Cargo.toml"))?.parse()?;
                let name = package["package"]["name"]
                    .as_str()
                    .ok_or("path dependency 缺少 package.name")?;
                value
                    .as_table_mut()
                    .ok_or("path dependency 必须是 table")?
                    .insert("package".into(), toml::Value::String(name.into()));
            }
        }
        if name == "folio-xtask" {
            if deps.iter().any(|(alias, value)| {
                let package = value
                    .get("package")
                    .and_then(|v| v.as_str())
                    .unwrap_or(alias);
                package.starts_with("folio-") || package.contains("gpui")
            }) {
                return Err("开发工具不得依赖产品 crate/GPUI".into());
            }
            continue;
        }
        names.insert(name.to_string());
        for (alias, value) in &deps {
            let package = value
                .get("package")
                .and_then(|v| v.as_str())
                .unwrap_or(alias);
            if package.contains("gpui") && name != "folio-app" {
                return Err(format!("FOLIO-ARCH-001 {name} 依赖 {package}").into());
            }
        }
        let actual: BTreeSet<_> = deps
            .iter()
            .filter_map(|(alias, value)| {
                let package = value
                    .get("package")
                    .and_then(|v| v.as_str())
                    .unwrap_or(alias);
                package.starts_with("folio-").then(|| package.to_string())
            })
            .collect();
        let actual: Vec<_> = actual.into_iter().collect();
        if manifest.product_dependencies.get(name) != Some(&actual) {
            return Err(format!(
                "FOLIO-ARCH-001 {name} 依赖图发生变化：{actual:?}，需审阅并更新登记"
            )
            .into());
        }
    }
    if names != manifest.product_dependencies.keys().cloned().collect() {
        return Err("product_dependencies 与 workspace 不一致".into());
    }
    Ok(())
}

fn check_facts(root: &Path, manifest: &Manifest) -> Result<()> {
    let cargo = toml_at(root, "Cargo.toml")?;
    let app = toml_at(root, "crates/folio-app/Cargo.toml")?;
    let fork = &cargo["patch"]["crates-io"];
    let rev = fork["gpui-pre"]["rev"].as_str().ok_or("GPUI rev 缺失")?;
    if rev.len() != 40 || !rev.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("GPUI rev 必须为完整 commit".into());
    }
    for name in [
        "gpui-pre",
        "gpui-pre-wgpu",
        "gpui-pre-platform",
        "gpui-pre-apple",
        "gpui-pre-windows",
    ] {
        if fork[name]["rev"].as_str() != Some(rev) {
            return Err(format!("FOLIO-GOV-002 {name} rev 漂移").into());
        }
    }
    if app["dependencies"]["folio-curl"]["rev"].as_str() != Some(rev) {
        return Err("folio-curl rev 与 GPUI 不同".into());
    }
    let lock = toml_at(root, "Cargo.lock")?;
    for name in [
        "gpui-pre",
        "gpui-pre-wgpu",
        "gpui-pre-platform",
        "gpui-pre-apple",
        "gpui-pre-windows",
        "folio-curl",
    ] {
        let packages: Vec<_> = lock["package"]
            .as_array()
            .ok_or("锁文件没有 package")?
            .iter()
            .filter(|p| p["name"].as_str() == Some(name))
            .collect();
        if packages.len() != 1 {
            return Err(format!("锁文件需要唯一的 {name}，实际 {} 份", packages.len()).into());
        }
        let package = packages[0];
        if !package
            .get("source")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.ends_with(&format!("#{rev}")))
        {
            return Err(format!("FOLIO-GOV-002 {name} lock rev 漂移").into());
        }
    }
    for file in [".github/workflows/ci.yml", ".github/workflows/release.yml"] {
        let source = std::fs::read_to_string(root.join(file))?;
        let versions: Vec<_> = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("uses: dtolnay/rust-toolchain@"))
            .collect();
        if versions.is_empty() || versions.iter().any(|v| *v != manifest.rust_ci) {
            return Err(format!("FOLIO-GOV-002 {file} Rust CI 版本漂移").into());
        }
    }
    let ci = std::fs::read_to_string(root.join(".github/workflows/ci.yml"))?;
    if !has_run_command(&ci, "cargo xtask governance --base ") {
        return Err("CI 没有带历史基准的治理命令".into());
    }
    Ok(())
}

fn has_run_command(source: &str, prefix: &str) -> bool {
    let lines: Vec<_> = source.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let field = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        let Some(command) = field.strip_prefix("run: ") else {
            continue;
        };
        if command.starts_with(prefix) {
            return true;
        }
        if !matches!(command, "|" | ">") && !command.starts_with("|-") && !command.starts_with(">-")
        {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        for body in lines.iter().skip(index + 1) {
            if body.trim().is_empty() {
                continue;
            }
            let body_indent = body.len() - body.trim_start().len();
            if body_indent <= indent {
                break;
            }
            let code = body.trim().split('#').next().unwrap_or("").trim();
            if code.starts_with(prefix) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use crate::governance::{
        check_boundaries, check_doc_paths, check_links, has_run_command, repository_path,
    };

    #[test]
    fn paths_and_entry_links_reject_escapes_and_missing_targets() {
        for path in ["../x", "C:/x", "/x", "a\\b", ""] {
            assert!(repository_path(path).is_err(), "{path}");
        }
        assert!(repository_path("doc/工程规范.md").is_ok());
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("exists.md"), "ok").unwrap();
        assert!(check_links(root.path(), "AGENTS.md", "[ok](exists.md)").is_ok());
        assert!(check_links(root.path(), "AGENTS.md", "[bad](missing.md)").is_err());
    }

    #[test]
    fn rust_comments_must_point_at_documents_that_exist() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("crates/demo/src")).unwrap();
        std::fs::create_dir_all(root.path().join("doc")).unwrap();
        std::fs::write(root.path().join("doc/实存.md"), "ok").unwrap();
        let source = root.path().join("crates/demo/src/lib.rs");
        let tick = '\u{60}';
        std::fs::write(&source, format!("//! 见 {tick}doc/实存.md{tick}。\n")).unwrap();
        check_doc_paths(root.path()).unwrap();
        std::fs::write(
            &source,
            format!("//! 见 {tick}doc/已删.md{tick} 那一节。\n"),
        )
        .unwrap();
        let error = check_doc_paths(root.path()).unwrap_err().to_string();
        assert!(error.contains("FOLIO-GOV-005"), "{error}");
        assert!(error.contains("doc/已删.md"), "{error}");
    }

    #[test]
    fn workspace_alias_cannot_hide_a_gpui_dependency() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("config")).unwrap();
        std::fs::create_dir_all(root.path().join("crates/engine")).unwrap();
        std::fs::write(root.path().join("Cargo.toml"), "[workspace]\nmembers=['crates/engine']\n[workspace.dependencies]\nhidden={package='gpui-kit',version='1'}\n").unwrap();
        std::fs::write(
            root.path().join("crates/engine/Cargo.toml"),
            "[package]\nname='folio-core'\n[dependencies]\nhidden.workspace=true\n[target.'cfg(windows)'.dependencies]\nhidden={package='ordinary',version='1'}\n",
        )
        .unwrap();
        std::fs::write(root.path().join("config/folio-governance.json"), r#"{"version":1,"documents":{},"rules":{},"skills":[],"product_dependencies":{"folio-core":[]},"rust_ci":"1.99.0"}"#).unwrap();
        let error = check_boundaries(root.path()).unwrap_err().to_string();
        assert!(error.contains("folio-core 依赖 gpui-kit"), "{error}");
    }

    #[test]
    fn ci_command_must_be_executable_yaml_not_a_comment() {
        assert!(has_run_command(
            "steps:\n  - run: |\n      cargo xtask governance --base HEAD\n",
            "cargo xtask governance --base "
        ));
        assert!(!has_run_command(
            "# cargo xtask governance --base HEAD\nsteps:\n  - run: cargo test\n",
            "cargo xtask governance --base "
        ));
        assert!(!has_run_command(
            "steps:\n  - run: |\n      # cargo xtask governance --base HEAD\n      cargo test\n",
            "cargo xtask governance --base "
        ));
    }
}
