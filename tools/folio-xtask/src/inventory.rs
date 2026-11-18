use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use syn::visit_mut::VisitMut;

use crate::{Result, boundaries};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub version: u32,
    pub files: BTreeMap<String, usize>,
    pub edges: Vec<String>,
}

pub fn snapshot(root: &Path) -> Result<Baseline> {
    let mut files = BTreeMap::new();
    for (path, text) in sources(root)? {
        let count = text.bytes().filter(|byte| *byte == b'\n').count()
            + usize::from(!text.is_empty() && !text.ends_with('\n'));
        if count > 900 {
            files.insert(path, count);
        }
    }
    Ok(Baseline {
        version: 1,
        files,
        edges: boundaries::scan(root)?,
    })
}

pub fn sources(root: &Path) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for dir in ["crates", "tools/folio-xtask/src"] {
        walk(root, &root.join(dir), &mut out)?;
    }
    if out.is_empty() {
        return Err("源码扫描结果为空".into());
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let path = entry.path();
        let name = entry.file_name();
        if matches!(
            name.to_str(),
            Some("target" | "build" | ".gradle" | "jniLibs")
        ) {
            continue;
        }
        if crate::skills::is_link(&std::fs::symlink_metadata(&path)?) {
            return Err(format!("源码扫描不跟随链接：{}", path.display()).into());
        }
        if ty.is_dir() {
            walk(root, &path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push((
                path.strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/"),
                std::fs::read_to_string(&path)?,
            ));
        }
    }
    Ok(())
}

pub fn check_snapshot(current: &Baseline, baseline: &Baseline) -> Result<()> {
    if baseline.version != 1 {
        return Err("未知债务基线版本".into());
    }
    for (path, count) in &current.files {
        match baseline.files.get(path) {
            Some(limit) if limit == count => {}
            Some(limit) if count < limit => {
                return Err(
                    format!("FOLIO-GOV-004 {path} 已缩短到 {count}，请下调基线 {limit}").into(),
                );
            }
            _ => {
                return Err(
                    format!("FOLIO-GOV-004 {path} {count} 行：新增或增长的超限文件").into(),
                );
            }
        }
    }
    for path in baseline.files.keys() {
        if !current.files.contains_key(path) {
            return Err(format!("FOLIO-GOV-004 {path} 已达标或删除，请移除基线").into());
        }
    }
    if current.edges != baseline.edges {
        let added: Vec<_> = current
            .edges
            .iter()
            .filter(|edge| !baseline.edges.contains(edge))
            .collect();
        let removed: Vec<_> = baseline
            .edges
            .iter()
            .filter(|edge| !current.edges.contains(edge))
            .collect();
        return Err(format!(
            "FOLIO-ARCH-002/003 边界变化：新增 {added:?}；已消除（需下调基线）{removed:?}"
        )
        .into());
    }
    Ok(())
}

pub fn ratchet(current: &Baseline, previous: &Baseline) -> Result<()> {
    for (path, count) in &current.files {
        if previous.files.get(path).is_none_or(|old| count > old) {
            return Err(format!("FOLIO-GOV-004 不得新增/提高历史基线：{path}").into());
        }
    }
    for edge in &current.edges {
        if !previous.edges.contains(edge) {
            return Err(format!("FOLIO-GOV-004 不得放宽边界基线：{edge}").into());
        }
    }
    Ok(())
}

struct StripDocs;
impl VisitMut for StripDocs {
    fn visit_attributes_mut(&mut self, attrs: &mut Vec<syn::Attribute>) {
        attrs.retain(|attr| !attr.path().is_ident("doc"));
        for attr in attrs {
            self.visit_attribute_mut(attr);
        }
    }
}

pub fn same_semantics(path: &str, old: &str, new: &str) -> Result<()> {
    if path.ends_with(".toml") {
        if old.parse::<toml::Value>()? != new.parse::<toml::Value>()? {
            return Err(format!("配置值变化：{path}").into());
        }
    } else if path.ends_with(".rs") {
        let mut before = syn::parse_file(old)?;
        let mut after = syn::parse_file(new)?;
        StripDocs.visit_file_mut(&mut before);
        StripDocs.visit_file_mut(&mut after);
        if before != after {
            return Err(format!("Rust 语法变化：{path}").into());
        }
        if old != new {
            println!(
                "{path}：普通语法一致；rustdoc、doc test 与工具指令仍需审阅，不由 AST 等价证明。"
            );
        }
    } else {
        return Err(format!("不支持语义比较：{path}").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::inventory::{Baseline, check_snapshot, ratchet, same_semantics};
    use std::collections::BTreeMap;

    fn baseline(lines: usize, edges: &[&str]) -> Baseline {
        Baseline {
            version: 1,
            files: BTreeMap::from([("src.rs".into(), lines)]),
            edges: edges.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn debt_cannot_grow_or_hide_completed_cleanup() {
        assert!(check_snapshot(&baseline(950, &[]), &baseline(950, &[])).is_ok());
        assert!(check_snapshot(&baseline(951, &[]), &baseline(950, &[])).is_err());
        assert!(check_snapshot(&baseline(949, &[]), &baseline(950, &[])).is_err());
        assert!(ratchet(&baseline(949, &[]), &baseline(950, &["old"])).is_ok());
        assert!(ratchet(&baseline(951, &[]), &baseline(950, &[])).is_err());
        assert!(ratchet(&baseline(950, &["new"]), &baseline(950, &[])).is_err());
    }

    #[test]
    fn comment_comparison_preserves_literals_and_configuration() {
        assert!(
            same_semantics(
                "a.rs",
                "// history\nfn f() { let s = \"//keep\"; }",
                "fn f() { let s = \"//keep\"; }"
            )
            .is_ok()
        );
        assert!(
            same_semantics(
                "a.rs",
                "fn f() { let s = \"//keep\"; }",
                "fn f() { let s = \"//lost\"; }"
            )
            .is_err()
        );
        assert!(same_semantics("a.toml", "# why\na = '#value'", "a = '#value'").is_ok());
        assert!(same_semantics("a.toml", "a = '#value'", "a = ''").is_err());
    }
}
