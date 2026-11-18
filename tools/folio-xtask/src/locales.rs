use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Result;

fn messages(value: &toml::Value, prefix: &str, out: &mut BTreeMap<String, String>) -> Result<()> {
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table {
                let key = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                messages(child, &key, out)?;
            }
        }
        toml::Value::String(text) if !text.trim().is_empty() => {
            if out.insert(prefix.into(), text.clone()).is_some() {
                return Err(format!("重复的文案键：{prefix}").into());
            }
        }
        _ => return Err(format!("文案必须是非空字符串：{prefix}").into()),
    }
    Ok(())
}

fn placeholders(mut text: &str) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    while let Some((_, rest)) = text.split_once("%{") {
        let (name, tail) = rest.split_once('}').ok_or("未闭合的文案占位符")?;
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err("文案占位符必须有合法名字".into());
        }
        names.insert(name.into());
        text = tail;
    }
    Ok(names)
}

pub fn check(root: &Path) -> Result<()> {
    let directory = root.join("crates/folio-core/locales");
    let mut languages = BTreeMap::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_none_or(|extension| extension != "toml")
        {
            continue;
        }
        if !entry.file_type()?.is_file() {
            return Err("语言资源只接受普通文件".into());
        }
        let text = std::fs::read_to_string(entry.path())?;
        let value: toml::Value = text.parse()?;
        let mut entries = BTreeMap::new();
        messages(&value, "", &mut entries)?;
        if entries.is_empty() {
            return Err("语言资源不得为空".into());
        }
        languages.insert(entry.file_name().to_string_lossy().into_owned(), entries);
    }
    let base = languages.get("en.toml").ok_or("缺少英文语言资源")?;
    if languages.len() < 2 {
        return Err("语言资源覆盖不足".into());
    }
    for (language, entries) in &languages {
        let missing: Vec<_> = base
            .keys()
            .filter(|key| !entries.contains_key(*key))
            .collect();
        let extra: Vec<_> = entries
            .keys()
            .filter(|key| !base.contains_key(*key))
            .collect();
        if !missing.is_empty() || !extra.is_empty() {
            return Err(
                format!("FOLIO-GOV-002 {language} 缺少 {missing:?}，多出 {extra:?}").into(),
            );
        }
        for (key, text) in entries {
            if placeholders(text)? != placeholders(&base[key])? {
                return Err(format!("FOLIO-GOV-002 {language} {key} 占位符不一致").into());
            }
        }
    }
    println!(
        "语言资源检查通过：{} 门语言、每门 {} 个键；占位符与非空值一致。",
        languages.len(),
        base.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::locales::check;

    #[test]
    fn locale_gate_rejects_missing_keys_empty_values_and_wrong_placeholders() {
        let dir = tempfile::tempdir().unwrap();
        let locales = dir.path().join("crates/folio-core/locales");
        std::fs::create_dir_all(&locales).unwrap();
        std::fs::write(locales.join("en.toml"), "hello = \"Hello %{name}\"\n").unwrap();
        let translated = locales.join("zh-CN.toml");
        std::fs::write(&translated, "hello = \"你好 %{name}\"\n").unwrap();
        check(dir.path()).unwrap();
        for invalid in [
            "other = \"你好\"",
            "hello = \" \"",
            "hello = \"你好 %{user}\"",
            "hello = \"你好 %{name\"",
        ] {
            std::fs::write(&translated, invalid).unwrap();
            assert!(check(dir.path()).is_err(), "应拒绝 {invalid}");
        }
        std::fs::remove_file(&translated).unwrap();
        assert!(check(dir.path()).is_err());
    }
}
