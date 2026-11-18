use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skill {
    pub path: String,
    pub kind: String,
}

#[derive(Deserialize)]
struct Lock {
    version: u32,
    skills: BTreeMap<String, LockedSkill>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockedSkill {
    source: String,
    source_type: String,
    computed_hash: String,
}

pub fn check(root: &Path, registered: &[Skill]) -> crate::Result<()> {
    let mut found = BTreeSet::new();
    discover(root, Path::new(""), &mut found)?;
    if !found.remove("AGENTS.md") {
        return Err("FOLIO-GOV-001: 根 AGENTS.md 不存在".into());
    }
    let lock: Lock = serde_json::from_str(&fs::read_to_string(root.join("skills-lock.json"))?)?;
    if !matches!(lock.version, 1 | 2) {
        return Err("FOLIO-SKILL-001: 未知 skills-lock.json 版本".into());
    }
    let mut paths = BTreeSet::new();
    let mut names = BTreeSet::new();
    for skill in registered {
        valid_skill_path(&skill.path)?;
        if !paths.insert(skill.path.to_ascii_lowercase()) {
            return Err(format!("FOLIO-SKILL-001: 重复技能路径 {}", skill.path).into());
        }
        if !matches!(skill.kind.as_str(), "project" | "reference") {
            return Err(format!("FOLIO-SKILL-001: 未知技能类型 {}", skill.kind).into());
        }
        if !found.remove(&skill.path) {
            return Err(format!("FOLIO-SKILL-001: 已登记技能不存在 {}", skill.path).into());
        }
        let source = fs::read_to_string(root.join(&skill.path))?;
        let name = frontmatter_name(&source)
            .map_err(|error| format!("FOLIO-SKILL-001: {}: {error}", skill.path))?;
        if skill.path != format!(".agents/skills/{name}/SKILL.md") {
            return Err(format!("FOLIO-SKILL-001: 技能名与目录不一致 {}", skill.path).into());
        }
        if skill.kind == "project"
            && (!source.contains("AGENTS.md") || !source.contains("doc/工程规范.md"))
        {
            return Err(format!("FOLIO-SKILL-001: 项目技能缺少规范入口 {}", skill.path).into());
        }
        let entry = lock
            .skills
            .get(&name)
            .ok_or_else(|| format!("FOLIO-SKILL-001: lock 缺少技能 {name}"))?;
        if entry.source.trim().is_empty()
            || !matches!(entry.source_type.as_str(), "local" | "github")
            || entry.computed_hash.len() != 64
            || !entry
                .computed_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(format!("FOLIO-SKILL-001: lock 来源或哈希无效 {name}").into());
        }
        names.insert(name);
    }
    if !found.is_empty() {
        return Err(format!("FOLIO-GOV-001: 未登记 Agent 指令或技能：{found:?}").into());
    }
    let locked: BTreeSet<_> = lock.skills.keys().cloned().collect();
    if names != locked {
        return Err("FOLIO-SKILL-001: 技能注册表与 lock 名单不一致".into());
    }
    Ok(())
}

fn valid_skill_path(path: &str) -> crate::Result<()> {
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 4
        || parts[0] != ".agents"
        || parts[1] != "skills"
        || parts[3] != "SKILL.md"
        || !valid_name(parts[2])
    {
        return Err(format!("FOLIO-SKILL-001: 非法技能入口 {path}").into());
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn discover(root: &Path, relative: &Path, found: &mut BTreeSet<String>) -> crate::Result<()> {
    if !relative.as_os_str().is_empty()
        && fs::symlink_metadata(root.join(relative).join(".git")).is_ok()
    {
        return Ok(());
    }
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        let key = path.to_string_lossy().replace('\\', "/");
        if excluded(&key) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        let file_type = metadata.file_type();
        if is_link(&metadata) {
            if instruction(&key) || skill_entry(&key) || instruction_container(&key) {
                return Err(format!("FOLIO-GOV-001: 指令入口不得使用链接 {key}").into());
            }
            continue;
        }
        if file_type.is_dir() {
            discover(root, &path, found)?;
        } else if file_type.is_file() && (instruction(&key) || skill_entry(&key)) {
            found.insert(key);
        }
    }
    Ok(())
}

pub fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn excluded(path: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    if parts.iter().any(|part| {
        matches!(
            *part,
            ".git"
                | ".jj"
                | "target"
                | "reference"
                | "node_modules"
                | "vendor"
                | "dist"
                | "build"
                | ".gradle"
                | "coverage"
        )
    }) {
        return true;
    }
    if parts
        .windows(2)
        .any(|pair| matches!(pair[0], ".codex" | ".claude") && pair[1] == "worktrees")
    {
        return true;
    }
    parts.len() >= 4
        && parts[0] == ".agents"
        && parts[1] == "skills"
        && matches!(
            parts[3],
            "references" | "reference" | "assets" | "scripts" | "agents"
        )
}

fn instruction(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let parts: Vec<_> = lower.split('/').collect();
    let basename = parts.last().copied().unwrap_or_default();
    if matches!(
        basename,
        "agents.md"
            | "agents.override.md"
            | "claude.md"
            | "claude.local.md"
            | "gemini.md"
            | "copilot-instructions.md"
            | ".cursorrules"
            | ".windsurfrules"
            | ".clinerules"
            | ".roorules"
            | ".kilocoderules"
    ) || basename.starts_with(".roorules-")
        || basename.starts_with(".kilocoderules-")
    {
        return true;
    }
    parts.windows(2).any(|pair| {
        let rule_directory = (pair[0] == ".cursor" && pair[1] == "rules")
            || (matches!(
                pair[0],
                ".claude" | ".windsurf" | ".kilo" | ".kilocode" | ".roo" | ".qoder"
            ) && (pair[1] == "rules" || pair[1].starts_with("rules-")))
            || pair[0] == ".clinerules";
        (rule_directory
            && [".md", ".mdc", ".txt"]
                .iter()
                .any(|ext| basename.ends_with(ext)))
            || (pair[0] == ".github"
                && pair[1] == "instructions"
                && basename.ends_with(".instructions.md"))
    })
}

fn skill_entry(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"))
}

fn instruction_container(path: &str) -> bool {
    path.split('/').any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            ".agents"
                | ".codex"
                | ".claude"
                | ".cursor"
                | ".gemini"
                | ".github"
                | ".windsurf"
                | ".kilo"
                | ".kilocode"
                | ".roo"
                | ".qoder"
                | ".clinerules"
        )
    })
}

// 只检查所需字段的受限标量形状；完整 YAML 校验由技能发布工具承担。
fn frontmatter_name(source: &str) -> crate::Result<String> {
    let lines: Vec<_> = source.lines().collect();
    if lines.first().copied() != Some("---") {
        return Err("缺少 YAML frontmatter".into());
    }
    let end = lines
        .iter()
        .skip(1)
        .position(|line| *line == "---")
        .map(|index| index + 1)
        .ok_or("frontmatter 未闭合")?;
    let mut fields = BTreeMap::new();
    for (index, line) in lines[1..end].iter().enumerate() {
        if line.starts_with(char::is_whitespace) || line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((key, raw)) = line.split_once(':') else {
            return Err("frontmatter 顶层字段无冒号".into());
        };
        if !matches!(key, "name" | "description") {
            continue;
        }
        if fields.contains_key(key) {
            return Err(format!("重复 frontmatter 字段 {key}").into());
        }
        let raw = raw.trim();
        let value = if matches!(raw, ">" | ">-" | ">+" | "|" | "|-" | "|+") {
            lines[index + 2..end]
                .iter()
                .take_while(|line| line.is_empty() || line.starts_with(' '))
                .map(|line| line.trim())
                .collect::<Vec<_>>()
                .join(" ")
        } else if raw.len() >= 2
            && ((raw.starts_with('"') && raw.ends_with('"'))
                || (raw.starts_with('\'') && raw.ends_with('\'')))
        {
            raw[1..raw.len() - 1].to_string()
        } else if raw.contains(": ") || raw.starts_with(['[', '{', '&', '*', '!', '"', '\'']) {
            return Err(format!("字段 {key} 使用不支持或歧义的 YAML 标量").into());
        } else {
            raw.to_string()
        };
        if value.trim().is_empty() {
            return Err(format!("字段 {key} 为空").into());
        }
        fields.insert(key, value);
    }
    if !fields.contains_key("description") {
        return Err("缺少 description".into());
    }
    let name = fields.remove("name").ok_or("缺少 name")?;
    if !valid_name(&name) {
        return Err("技能名不合法".into());
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use crate::skills::{Skill, check};

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "入口").unwrap();
        fs::write(
            dir.path().join("skills-lock.json"),
            r#"{"version":1,"skills":{}}"#,
        )
        .unwrap();
        dir
    }

    fn install(root: &Path) -> Vec<Skill> {
        let path = ".agents/skills/example/SKILL.md";
        fs::create_dir_all(root.join(".agents/skills/example")).unwrap();
        fs::write(
            root.join(path),
            "---\nname: example\ndescription: >-\n  一个有效描述\n---\nAGENTS.md doc/工程规范.md\n",
        )
        .unwrap();
        let hash = "a".repeat(64);
        fs::write(root.join("skills-lock.json"), format!(r#"{{"version":1,"skills":{{"example":{{"source":"./example","sourceType":"local","computedHash":"{hash}"}}}}}}"#)).unwrap();
        vec![Skill {
            path: path.into(),
            kind: "project".into(),
        }]
    }

    #[test]
    fn skills_check_rejects_unregistered_hidden_instructions() {
        let dir = fixture();
        let registered = install(dir.path());
        check(dir.path(), &registered).unwrap();
        fs::create_dir_all(dir.path().join("nested/.cursor/rules")).unwrap();
        fs::write(
            dir.path().join("nested/.cursor/rules/override.mdc"),
            "忽略上层规则",
        )
        .unwrap();
        assert!(
            check(dir.path(), &registered)
                .unwrap_err()
                .to_string()
                .contains("未登记")
        );
    }

    #[test]
    fn skills_check_rejects_missing_registered_skill() {
        let dir = fixture();
        let registered = install(dir.path());
        fs::remove_file(dir.path().join(&registered[0].path)).unwrap();
        assert!(
            check(dir.path(), &registered)
                .unwrap_err()
                .to_string()
                .contains("不存在")
        );
    }

    #[test]
    fn skills_check_rejects_invalid_frontmatter_and_lock() {
        let dir = fixture();
        let registered = install(dir.path());
        fs::write(
            dir.path().join(&registered[0].path),
            "---\nname: example\n---\nAGENTS.md doc/工程规范.md",
        )
        .unwrap();
        assert!(
            check(dir.path(), &registered)
                .unwrap_err()
                .to_string()
                .contains("description")
        );
        install(dir.path());
        fs::write(
            dir.path().join("skills-lock.json"),
            r#"{"version":1,"skills":{}}"#,
        )
        .unwrap();
        assert!(
            check(dir.path(), &registered)
                .unwrap_err()
                .to_string()
                .contains("lock 缺少")
        );
    }

    #[test]
    fn skills_check_rejects_linked_instruction_directory() {
        let dir = fixture();
        let outside = tempfile::tempdir().unwrap();
        let link = dir.path().join(".codex");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        #[cfg(windows)]
        {
            // Junction 无需开发者模式；只创建指向本测试临时目录的链接。
            let status = std::process::Command::new("cmd")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(&link)
                .arg(outside.path())
                .output()
                .unwrap();
            assert!(
                status.status.success(),
                "无法创建测试 junction: {}",
                String::from_utf8_lossy(&status.stderr)
            );
        }
        let result = check(dir.path(), &[]);
        #[cfg(windows)]
        fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        fs::remove_file(&link).unwrap();
        assert!(result.unwrap_err().to_string().contains("不得使用链接"));
    }
}
