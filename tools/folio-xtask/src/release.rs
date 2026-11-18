use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::Result;

pub fn check(root: &Path, base: Option<&str>, artifacts: Option<&Path>) -> Result<()> {
    let cargo: toml::Value = std::fs::read_to_string(root.join("Cargo.toml"))?.parse()?;
    let version = cargo["workspace"]["package"]["version"]
        .as_str()
        .ok_or("缺少产品版本")?;
    let gradle =
        std::fs::read_to_string(root.join("crates/folio-app/android/app/build.gradle.kts"))?;
    let name = assignment(&gradle, "versionName").ok_or("缺少 Android versionName")?;
    let code: u64 = assignment(&gradle, "versionCode")
        .ok_or("缺少 Android versionCode")?
        .parse()?;
    if name != version || code == 0 {
        return Err("Android/Cargo 版本不一致或 versionCode 无效".into());
    }
    if assignment(&gradle, "minSdk") != Some("26") {
        return Err("Android minSdk 必须与 NDK API 26 一致".into());
    }
    if let Some(base) = base {
        let old = crate::git(
            root,
            &[
                "show",
                &format!("{base}:crates/folio-app/android/app/build.gradle.kts"),
            ],
        )?;
        let old_name = assignment(&old, "versionName").ok_or("基准缺少 versionName")?;
        let old_code: u64 = assignment(&old, "versionCode")
            .ok_or("基准缺少 versionCode")?
            .parse()?;
        if code < old_code || (old_name != name && code <= old_code) {
            return Err("发布版本变化必须增加 Android versionCode".into());
        }
    }
    for file in [
        "LICENSE",
        "THIRD-PARTY-NOTICES.txt",
        "packaging/icons/folio.png",
        "packaging/icons/folio.ico",
        "crates/folio-app/android/app/src/main/java/dev/gpui/mobile/FolioSpeech.java",
        "crates/folio-app/android/app/src/main/java/dev/gpui/mobile/FolioSourceAccess.java",
    ] {
        let metadata = std::fs::metadata(root.join(file))
            .map_err(|error| format!("发布资产不可用：{file}：{error}"))?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err(format!("发布资产为空：{file}").into());
        }
    }
    let proguard =
        std::fs::read_to_string(root.join("crates/folio-app/android/app/proguard-rules.pro"))?;
    if !keeps_speech_bridge(&proguard) {
        return Err("缺少 FolioSpeech R8 keep".into());
    }
    let source_keep: String = proguard
        .lines()
        .filter_map(|l| l.split('#').next())
        .flat_map(str::split_whitespace)
        .collect();
    if !source_keep.contains("-keepclassdev.gpui.mobile.FolioSourceAccess{*;}") {
        return Err("缺少持久原件访问桥的 R8 keep".into());
    }
    if let Some(dir) = artifacts {
        verify_artifacts(dir)?;
    }
    println!("发布源检查通过：{version} / Android {code}。未执行平台安装或签名验收。");
    Ok(())
}

fn assignment<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    let mut values = source.lines().filter_map(|line| {
        let (left, right) = line.trim().split_once('=')?;
        (left.trim() == key).then(|| {
            right
                .split("//")
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('"')
        })
    });
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn keeps_speech_bridge(source: &str) -> bool {
    let rules: String = source
        .lines()
        .filter_map(|line| line.split('#').next())
        .flat_map(str::split_whitespace)
        .collect();
    ["-keepclassdev.gpui.mobile.FolioSpeech{*;}"]
        .iter()
        .any(|rule| rules.contains(rule))
}

pub fn checksums(text: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (hash, name) = line
            .split_once(char::is_whitespace)
            .ok_or("校验清单行无文件名")?;
        let name = name.trim().trim_start_matches('*');
        if hash.len() != 64
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            || name.contains(['/', '\\', ':'])
            || name == "."
            || name == ".."
            || name.is_empty()
        {
            return Err("校验清单的 hash 或文件名无效".into());
        }
        if out.insert(name.into(), hash.to_ascii_lowercase()).is_some() {
            return Err("校验清单文件名重复".into());
        }
    }
    if out.is_empty() {
        return Err("校验清单为空".into());
    }
    Ok(out)
}

pub fn verify_artifacts(dir: &Path) -> Result<()> {
    let expected = checksums(&std::fs::read_to_string(dir.join("sha256sums.txt"))?)?;
    let mut names = std::collections::BTreeSet::new();
    for item in std::fs::read_dir(dir)? {
        let item = item?;
        if crate::skills::is_link(&std::fs::symlink_metadata(item.path())?)
            || !item.file_type()?.is_file()
        {
            return Err("产物目录只接受普通文件".into());
        }
        let name = item
            .file_name()
            .into_string()
            .map_err(|_| "产物名不是 UTF-8")?;
        if name == "sha256sums.txt" {
            continue;
        }
        names.insert(name.clone());
        let hash = expected
            .get(&name)
            .ok_or_else(|| format!("产物未登记校验：{name}"))?;
        let mut file = std::fs::File::open(item.path())?;
        let mut digest = Sha256::new();
        let mut bytes = [0; 65536];
        loop {
            let count = file.read(&mut bytes)?;
            if count == 0 {
                break;
            }
            digest.update(&bytes[..count]);
        }
        let actual: String = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if actual != *hash {
            return Err(format!("产物校验失败：{name}").into());
        }
    }
    if names != expected.keys().cloned().collect() {
        return Err("清单引用了缺失产物".into());
    }
    println!("{} 个发布文件 SHA-256 校验通过。", names.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::release::{check, checksums, verify_artifacts};

    #[test]
    fn release_sources_reject_missing_assets_and_mismatched_versions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let gradle = "versionName = \"0.1.5\"\nversionCode = 5\nminSdk = 26\n";
        for (path, content) in [
            ("Cargo.toml", "[workspace.package]\nversion = \"0.1.5\"\n"),
            ("crates/folio-app/android/app/build.gradle.kts", gradle),
            ("LICENSE", "MIT"),
            ("THIRD-PARTY-NOTICES.txt", "notices"),
            ("packaging/icons/folio.png", "png"),
            ("packaging/icons/folio.ico", "ico"),
            (
                "crates/folio-app/android/app/src/main/java/dev/gpui/mobile/FolioSpeech.java",
                "bridge",
            ),
            (
                "crates/folio-app/android/app/src/main/java/dev/gpui/mobile/FolioSourceAccess.java",
                "source bridge",
            ),
            (
                "crates/folio-app/android/app/proguard-rules.pro",
                "-keep class dev.gpui.mobile.FolioSpeech { *; }\n-keep class dev.gpui.mobile.FolioSourceAccess { *; }",
            ),
        ] {
            let target = root.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, content).unwrap();
        }
        check(root, None, None).unwrap();
        let proguard = root.join("crates/folio-app/android/app/proguard-rules.pro");
        std::fs::write(
            &proguard,
            "-keep class dev.gpui.mobile.FolioSpeech { *; }\n# -keep class dev.gpui.mobile.FolioSourceAccess { *; }",
        )
        .unwrap();
        assert!(
            check(root, None, None)
                .unwrap_err()
                .to_string()
                .contains("原件访问桥")
        );
        std::fs::write(
            &proguard,
            "-keep class dev.gpui.mobile.FolioSpeech { *; }\n-keep class dev.gpui.mobile.FolioSourceAccess { *; }",
        )
        .unwrap();
        std::fs::remove_file(root.join("LICENSE")).unwrap();
        assert!(
            check(root, None, None)
                .unwrap_err()
                .to_string()
                .contains("LICENSE")
        );
        std::fs::write(root.join("LICENSE"), "MIT").unwrap();
        std::fs::write(
            root.join("crates/folio-app/android/app/build.gradle.kts"),
            gradle.replace("0.1.5", "0.1.6"),
        )
        .unwrap();
        assert!(check(root, None, None).is_err());
        std::fs::write(
            root.join("crates/folio-app/android/app/build.gradle.kts"),
            format!("{gradle}\nversionCode = 1\n"),
        )
        .unwrap();
        assert!(check(root, None, None).is_err());
        std::fs::write(
            root.join("crates/folio-app/android/app/build.gradle.kts"),
            gradle,
        )
        .unwrap();
        let proguard = root.join("crates/folio-app/android/app/proguard-rules.pro");
        std::fs::write(&proguard, "# -keep class dev.gpui.mobile.FolioSpeech { *; }\n-keepclassmembers class dev.gpui.mobile.FolioSpeech { native <methods>; }").unwrap();
        assert!(
            check(root, None, None)
                .unwrap_err()
                .to_string()
                .contains("R8")
        );
        std::fs::write(
            root.join("crates/folio-app/android/app/build.gradle.kts"),
            gradle.replace("minSdk = 26", "minSdk = 25"),
        )
        .unwrap();
        std::fs::write(proguard, "-keep class dev.gpui.mobile.FolioSpeech { *; }").unwrap();
        assert!(
            check(root, None, None)
                .unwrap_err()
                .to_string()
                .contains("minSdk")
        );
    }

    #[test]
    fn release_checksums_reject_traversal_duplicates_and_changed_artifacts() {
        assert!(checksums(&format!("{}  ../secret", "a".repeat(64))).is_err());
        assert!(checksums(&format!("{0}  a\n{0}  a", "a".repeat(64))).is_err());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("folio.zip"), b"hello").unwrap();
        std::fs::write(
            dir.path().join("sha256sums.txt"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824  folio.zip\n",
        )
        .unwrap();
        verify_artifacts(dir.path()).unwrap();
        std::fs::write(dir.path().join("folio.zip"), b"changed").unwrap();
        assert!(verify_artifacts(dir.path()).is_err());
    }
}
