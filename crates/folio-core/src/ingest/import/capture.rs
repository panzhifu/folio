//! 捕获独占暂存快照。解析只接触已复制的字节，相对资源不能逃出授权原件目录。

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

use folio_format::ext::FormatExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::error::{Error, IoResultExt as _, Result};
use crate::infra::identity;
use crate::infra::task::StageCtx;

const MAX_RESOURCES: usize = 1024;
const MAX_RESOURCE_BYTES: u64 = 64 << 20;
const MAX_BUNDLE_BYTES: u64 = 256 << 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ResourceFile {
    pub path: String,
    pub hash: String,
    pub bytes: u64,
}

pub(super) struct Captured {
    pub dir: PathBuf,
    pub main: PathBuf,
    pub hash: String,
    pub resource_hash: String,
    pub manifest: String,
}

impl Captured {
    pub fn fingerprint(&self) -> String {
        format!("{}:{}", self.hash, self.resource_hash)
    }
}

impl Drop for Captured {
    fn drop(&mut self) {
        // dir 只由本操作 create_dir 成功后拥有，位于调用方的 .staging 下。
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub(super) fn issue(path: &Path, why: &str) -> Error {
    Error::Io {
        path: path.into(),
        source: why.to_string().into(),
    }
}

pub(super) fn copy_hash(from: &Path, to: &Path, ctx: &StageCtx) -> Result<String> {
    copy_hash_limited(from, to, ctx, 8 << 30)
}

fn copy_hash_limited(from: &Path, to: &Path, ctx: &StageCtx, limit: u64) -> Result<String> {
    let mut source = std::fs::File::open(from).io_path(from)?;
    let before = source.metadata().io_path(from)?;
    if !before.is_file() {
        return Err(issue(from, "来源不是普通文件"));
    }
    let mut dest = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .io_path(to)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut count = 0u64;
    let result = (|| {
        loop {
            if ctx.cancel().triggered() {
                return Err(issue(from, "捕获已取消"));
            }
            let got = source.read(&mut buffer).io_path(from)?;
            if got == 0 {
                break;
            }
            if count.saturating_add(got as u64) > limit {
                return Err(issue(from, "捕获的实际字节超过容量预算"));
            }
            dest.write_all(&buffer[..got]).io_path(to)?;
            hash.update(&buffer[..got]);
            count += got as u64;
            ctx.bytes(count, Some(before.len()), "捕获阅读版本");
        }
        dest.sync_all().io_path(to)?;
        let after = std::fs::metadata(from).io_path(from)?;
        if before.len() != count
            || before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
        {
            return Err(issue(from, "捕获期间原件发生变化，请稍后重试"));
        }
        Ok(hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    })();
    if result.is_err() {
        drop(dest);
        let _ = std::fs::remove_file(to);
    }
    result
}

pub(super) fn capture_staged(
    name: &str,
    staged: PathBuf,
    staging: &Path,
    original: Option<&Path>,
    hash: Option<String>,
    ctx: &StageCtx,
) -> Result<Captured> {
    let dir = staging.join(crate::sync::new_device_id());
    std::fs::create_dir(&dir).io_path(&dir)?;
    let leaf = safe_name(name);
    let main = dir.join(leaf);
    if let Err(err) = std::fs::rename(&staged, &main).io_path(&main) {
        let _ = std::fs::remove_dir(&dir);
        return Err(err);
    }
    let mut captured = Captured {
        dir,
        main,
        hash: String::new(),
        resource_hash: String::new(),
        manifest: "[]".into(),
    };
    captured.hash = match hash {
        Some(hash) => hash,
        None => identity::sha256_of_path(&captured.main)
            .io_path(&captured.main)?
            .to_hex(),
    };
    if let Some(format) = folio_format::sniff::identify(name, &captured.main).format() {
        let named = captured.main.with_extension(format.copy_extension());
        if named != captured.main {
            std::fs::rename(&captured.main, &named).io_path(&named)?;
            captured.main = named;
        }
    }
    let extension = captured
        .main
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "html" | "htm" | "xhtml" | "md" | "markdown"
    ) {
        let raw = std::fs::read(&captured.main).io_path(&captured.main)?;
        if raw.len() > folio_format::limits::MAX_DOM_BYTES {
            return Err(issue(&captured.main, "资源引用枚举超过文本预算"));
        }
        let references = if matches!(extension.as_str(), "md" | "markdown") {
            folio_format::markdown::resource_hrefs_bytes(&raw)
                .ok_or_else(|| issue(&captured.main, "无法解析 Markdown 资源引用"))?
        } else {
            html_refs(&folio_format::html::decode_resource_markup(&raw))?
        };
        let files = capture_resources(&captured.dir, original, references, ctx)?;
        captured.manifest = serde_json::to_string(&files)?;
        if !files.is_empty() {
            captured.resource_hash =
                identity::sha256_of_bytes(captured.manifest.as_bytes()).to_hex();
        }
    }
    Ok(captured)
}

pub(super) fn safe_name(name: &str) -> String {
    let name = Path::new(name)
        .file_name()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let clean: String = name
        .chars()
        .filter(|c| !crate::infra::paths::is_unsafe_name_char(*c))
        .take(100)
        .collect();
    if clean.trim().is_empty() {
        "book".into()
    } else {
        clean
    }
}

fn html_refs(text: &str) -> Result<Vec<String>> {
    let dom = folio_format::dom::Dom::parse(text.as_bytes());
    let mut refs = Vec::new();
    for (id, _) in dom.elements() {
        if matches!(dom.tag_name(id), Some("img" | "source" | "audio" | "video")) {
            if let Some(src) = dom.attr(id, "src") {
                refs.push(src.into());
            }
            if let Some(src) = dom.attr(id, "poster") {
                refs.push(src.into());
            }
            if dom.attr(id, "srcset").is_some() {
                return Err(issue(Path::new("srcset"), "暂不支持冻结 srcset 多候选资源"));
            }
        }
        if dom.tag_name(id) == Some("link")
            && dom.attr(id, "rel").is_some_and(|s| {
                s.split_whitespace()
                    .any(|r| r.eq_ignore_ascii_case("stylesheet"))
            })
            && let Some(href) = dom.attr(id, "href")
        {
            refs.push(href.into());
        }
        if let Some(style) = dom.attr(id, "style") {
            refs.extend(css_refs(style)?);
        }
        if dom.tag_name(id) == Some("style") {
            refs.extend(css_refs(&dom.raw_text_of_subtree(id))?);
        }
    }
    Ok(refs)
}

fn capture_resources(
    destination: &Path,
    original: Option<&Path>,
    references: Vec<String>,
    ctx: &StageCtx,
) -> Result<Vec<ResourceFile>> {
    let Some(original) = original else {
        if references.iter().any(|r| local_reference(r).is_some()) {
            return Err(issue(
                destination,
                "此来源未提供相对资源访问范围，无法冻结阅读版本",
            ));
        }
        return Ok(Vec::new());
    };
    let root = original
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .io_path(original)?;
    let mut queue: VecDeque<(PathBuf, String)> = references
        .into_iter()
        .map(|r| (PathBuf::new(), r))
        .collect();
    let mut seen = BTreeSet::new();
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    while let Some((base, raw)) = queue.pop_front() {
        let Some(relative) = local_reference(&raw) else {
            continue;
        };
        let joined = base.join(relative);
        let mut normalized = PathBuf::new();
        for part in joined.components() {
            match part {
                Component::Normal(s) => normalized.push(s),
                Component::CurDir => {}
                Component::ParentDir if normalized.pop() => {}
                _ => return Err(issue(&joined, "资源路径越过已授权原件目录")),
            }
        }
        let key = normalized.to_string_lossy().replace('\\', "/");
        if key.is_empty() || !seen.insert(key.clone()) {
            continue;
        }
        if seen.len() > MAX_RESOURCES {
            return Err(issue(&root, "相对资源数量超过预算"));
        }
        let source = root
            .join(&normalized)
            .canonicalize()
            .io_path(root.join(&normalized))?;
        if !source.starts_with(&root) {
            return Err(issue(&source, "资源链接越过已授权原件目录"));
        }
        let bytes = std::fs::metadata(&source).io_path(&source)?.len();
        total = total
            .checked_add(bytes)
            .ok_or_else(|| issue(&source, "资源容量溢出"))?;
        if bytes > MAX_RESOURCE_BYTES || total > MAX_BUNDLE_BYTES {
            return Err(issue(&source, "相对资源超过捕获容量预算"));
        }
        let target = destination.join(&normalized);
        std::fs::create_dir_all(target.parent().unwrap()).io_path(&target)?;
        let hash = copy_hash_limited(
            &source,
            &target,
            ctx,
            MAX_RESOURCE_BYTES.min(MAX_BUNDLE_BYTES - total + bytes),
        )?;
        if normalized
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("css"))
        {
            let css = std::fs::read_to_string(&target).io_path(&target)?;
            if css.len() > folio_format::limits::MAX_CSS_BYTES {
                return Err(issue(&source, "CSS资源超过枚举预算"));
            }
            let parent = normalized
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .to_path_buf();
            queue.extend(css_refs(&css)?.into_iter().map(|r| (parent.clone(), r)));
        }
        files.insert(
            key.clone(),
            ResourceFile {
                path: key,
                hash,
                bytes,
            },
        );
    }
    Ok(files.into_values().collect())
}

fn local_reference(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('#') || raw.starts_with("//") || raw.contains(":") {
        return None;
    }
    Some(folio_format::asset_href(raw))
}

/// 只枚举 url() 与 @import 字符串；未知转义明确拒绝，不把动态依赖冒充已冻结。
fn css_refs(text: &str) -> Result<Vec<String>> {
    if text.contains('\\') {
        return Err(issue(Path::new("CSS"), "资源引用的 CSS 转义暂不支持冻结"));
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut refs = Vec::new();
    while i < bytes.len() {
        if bytes[i..].starts_with(b"/*") {
            let end = text[i + 2..]
                .find("*/")
                .ok_or_else(|| issue(Path::new("CSS"), "未闭合 CSS 注释"))?;
            i += end + 4;
            continue;
        }
        let url = bytes[i..]
            .get(..4)
            .is_some_and(|s| s.eq_ignore_ascii_case(b"url("));
        let import = bytes[i..]
            .get(..7)
            .is_some_and(|s| s.eq_ignore_ascii_case(b"@import"));
        if url || import {
            i += if url { 4 } else { 7 };
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i == bytes.len() {
                break;
            }
            if matches!(bytes[i], b'\'' | b'"') {
                let quote = bytes[i];
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                if i == bytes.len() {
                    return Err(issue(Path::new("CSS"), "未闭合 CSS 资源字符串"));
                }
                refs.push(text[start..i].into());
                i += 1;
            } else if url {
                let start = i;
                while i < bytes.len() && bytes[i] != b')' {
                    i += 1;
                }
                if i == bytes.len() {
                    return Err(issue(Path::new("CSS"), "未闭合 CSS url"));
                }
                refs.push(text[start..i].trim().into());
                i += 1;
            }
            continue;
        }
        if matches!(bytes[i], b'\'' | b'"') {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
        }
        i += 1;
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use crate::infra::task::StageCtx;
    use crate::ingest::import::capture::{capture_staged, copy_hash, copy_hash_limited};
    use std::path::Path;

    fn capture(path: &Path, staging: &Path) -> super::Captured {
        std::fs::create_dir_all(staging).unwrap();
        let part = staging.join(".case.part");
        let hash = copy_hash(path, &part, &StageCtx::detached()).unwrap();
        capture_staged(
            path.file_name().unwrap().to_str().unwrap(),
            part,
            staging,
            Some(path),
            Some(hash),
            &StageCtx::detached(),
        )
        .unwrap()
    }

    #[test]
    fn resource_capture_follows_css_imports_fonts_and_background_images() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir_all(source.join("css")).unwrap();
        std::fs::write(
            source.join("book.html"),
            "<link rel='stylesheet' href='css/main.css'><p>Text</p>",
        )
        .unwrap();
        std::fs::write(
            source.join("css/main.css"),
            "@import 'other.css'; p{background:url('../image.png')}",
        )
        .unwrap();
        std::fs::write(
            source.join("css/other.css"),
            "@import 'main.css'; @font-face{font-family:Book;src:url('../font.otf')}",
        )
        .unwrap();
        std::fs::write(source.join("image.png"), b"image").unwrap();
        std::fs::write(source.join("font.otf"), b"font").unwrap();
        let captured = capture(&source.join("book.html"), &dir.path().join("staging"));
        let files: Vec<super::ResourceFile> = serde_json::from_str(&captured.manifest).unwrap();
        assert_eq!(files.len(), 4);
        assert_eq!(
            std::fs::read(captured.dir.join("image.png")).unwrap(),
            b"image"
        );
        assert_eq!(
            std::fs::read(captured.dir.join("font.otf")).unwrap(),
            b"font"
        );
    }

    #[test]
    fn image_only_change_creates_a_different_resource_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("book.md");
        let image = dir.path().join("image.png");
        std::fs::write(&source, "# Book\n\n![image](image.png)").unwrap();
        std::fs::write(&image, b"one").unwrap();
        let first = capture(&source, &dir.path().join("staging"));
        std::fs::write(&image, b"two").unwrap();
        let second = capture(&source, &dir.path().join("staging"));
        assert_eq!(first.hash, second.hash);
        assert_ne!(first.resource_hash, second.resource_hash);
    }

    #[test]
    fn escaping_or_missing_resource_rejects_the_snapshot_without_leaving_staging() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let staging = dir.path().join("staging");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(dir.path().join("private.png"), b"private").unwrap();
        for reference in ["../private.png", "missing.png"] {
            let path = source.join("book.html");
            std::fs::write(&path, format!("<img src='{reference}'>")).unwrap();
            let part = staging.join(".case.part");
            let hash = copy_hash(&path, &part, &StageCtx::detached()).unwrap();
            assert!(
                capture_staged(
                    "book.html",
                    part,
                    &staging,
                    Some(&path),
                    Some(hash),
                    &StageCtx::detached()
                )
                .is_err()
            );
            assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
        }
        assert_eq!(
            std::fs::read(dir.path().join("private.png")).unwrap(),
            b"private"
        );
    }

    #[test]
    fn actual_copy_budget_failure_removes_only_the_partial() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        std::fs::write(&source, b"five!").unwrap();
        assert!(copy_hash_limited(&source, &target, &StageCtx::detached(), 4).is_err());
        assert!(!target.exists());
        assert_eq!(std::fs::read(source).unwrap(), b"five!");
    }
}
