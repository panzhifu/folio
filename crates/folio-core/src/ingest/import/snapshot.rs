//! 候选解析与独占版本目录的发布；不修改当前书目选择。
use super::capture::Captured;
use crate::content::document;
use crate::content::format::{ParseBudget, sniff};
use crate::error::{Error, ImportReject, IoResultExt as _, Result};
use crate::store::{RevisionFence, Store};
use std::path::{Path, PathBuf};
pub(super) fn parse_capture(
    store: &Store,
    captured: &Captured,
    budget: ParseBudget,
) -> Result<Box<dyn folio_format::document::Document>> {
    let name = captured
        .main
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let verdict = sniff::identify(&name, &captured.main);
    if verdict.format().is_none() {
        let (reject, why) = verdict
            .rejection()
            .unwrap_or((ImportReject::Unreadable, "无法识别阅读版本".into()));
        return Err(Error::Rejected {
            name: name.into(),
            why,
            reject,
            source: None,
        });
    }
    let passphrase = store.passphrase_for_hash(&captured.hash).ok().flatten();
    let opened = document::open_with(&captured.main, budget, passphrase.clone());
    let opened = opened.and_then(|doc| {
        if matches!(
            doc.format(),
            folio_format::Format::Text | folio_format::Format::Markdown
        ) {
            let mut provider = folio_format::provider_for(&captured.main)
                .ok_or_else(|| folio_format::Error::UnsupportedFormat(name.to_string()))?;
            provider.set_passphrase(passphrase.clone());
            let (meta, content) = provider.read(&captured.main)?;
            Ok(document::with_container(
                meta,
                content,
                &captured.main,
                passphrase,
            ))
        } else {
            Ok(doc)
        }
    });
    opened.map_err(|error| Error::Rejected {
        name: name.into(),
        why: error.to_string(),
        reject: match error {
            folio_format::Error::Encrypted { .. } => ImportReject::Encrypted,
            folio_format::Error::NeedsPassword { .. } => ImportReject::NeedsPassword,
            _ => ImportReject::Unreadable,
        },
        source: Some(Box::new(error)),
    })
}

pub(super) struct PublishedCapture {
    pub(super) dir: PathBuf,
    pub(super) main: PathBuf,
    owned: bool,
}
impl PublishedCapture {
    pub(super) fn commit(mut self) {
        self.owned = false;
    }
}
impl Drop for PublishedCapture {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

pub(super) fn publish_capture(
    captured: &Captured,
    root: &Path,
    uuid: &str,
    fence: RevisionFence,
    doc: &dyn folio_format::document::Document,
) -> Result<PublishedCapture> {
    let chapter_sources =
        crate::content::version_document::write_chapter_artifacts(doc, &captured.dir)?;
    let parent = root.join("versions").join(uuid);
    std::fs::create_dir_all(&parent).io_path(&parent)?;
    let dir = parent.join(format!("{}-{}", fence.rev_no, fence.index_generation));
    std::fs::create_dir(&dir).io_path(&dir)?;
    let published = PublishedCapture {
        main: dir.join(captured.main.file_name().unwrap()),
        dir,
        owned: true,
    };
    fn files(from: &Path, to: &Path) -> Result<()> {
        for entry in std::fs::read_dir(from).io_path(from)? {
            let entry = entry.io_path(from)?;
            let path = entry.path();
            let dest = to.join(entry.file_name());
            if entry.file_type().io_path(&path)?.is_dir() {
                std::fs::create_dir(&dest).io_path(&dest)?;
                files(&path, &dest)?;
            } else {
                super::publication::create_copy(&path, &dest, true).io_path(&dest)?;
                std::fs::remove_file(&path).io_path(&path)?;
            }
        }
        Ok(())
    }
    let comic_cache = folio_format::comic_cache::dir_for(&captured.main);
    files(&captured.dir, &published.dir)?;
    folio_format::comic_cache::relocate(&comic_cache, &published.main);
    let manifest = published.dir.join(".folio-revision.json");
    let json = serde_json::json!({"version":1,"book_uuid":uuid,"rev_no":fence.rev_no,"index_generation":fence.index_generation,"main":captured.main.file_name().unwrap().to_string_lossy(),"content_hash":captured.hash,"resource_hash":captured.resource_hash,"resources":serde_json::from_str::<serde_json::Value>(&captured.manifest)?,"chapter_sources":chapter_sources});
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&manifest)
        .io_path(&manifest)?;
    use std::io::Write as _;
    file.write_all(&serde_json::to_vec(&json)?)
        .io_path(&manifest)?;
    file.sync_all().io_path(&manifest)?;
    Ok(published)
}
