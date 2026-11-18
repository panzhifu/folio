//! 按固定阅读版本装配文档；动态分章格式读取准备时保存的章节产物。

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use folio_format::document::{ContentRef, Document};
use folio_format::{
    ContentKind, CoverRef, DocMeta, FontBytes, FontRef, PageRegion, PageText, Resource,
};
use serde::{Deserialize, Serialize};

use crate::error::{Error, IoResultExt as _, Result};
use folio_model::revision::BookRevision;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChapterArtifact {
    pub path: String,
    pub hash: String,
    pub bytes: u64,
}

#[derive(Deserialize)]
struct Marker {
    version: u32,
    book_uuid: String,
    rev_no: i64,
    index_generation: i64,
    main: String,
    content_hash: String,
    resource_hash: String,
    #[serde(default)]
    chapter_sources: Vec<ChapterArtifact>,
}

pub fn open_revision_document(
    revision: BookRevision,
    meta: DocMeta,
    content: folio_format::Content,
    passphrase: Option<folio_format::Passphrase>,
) -> Result<Box<dyn Document>> {
    let path = PathBuf::from(&revision.managed_path);
    let root = path
        .parent()
        .ok_or_else(|| invalid(&path, "阅读版本缺少目录"))?;
    let marker_path = root.join(".folio-revision.json");
    let artifacts = if revision.state == "legacy-unverified" {
        return Err(invalid(&path, "旧阅读版本尚未验证"));
    } else if marker_path.is_file() {
        let mut bytes = Vec::new();
        std::fs::File::open(&marker_path)
            .io_path(&marker_path)?
            .take((2 << 20) + 1)
            .read_to_end(&mut bytes)
            .io_path(&marker_path)?;
        if bytes.len() > 2 << 20 {
            return Err(invalid(&marker_path, "阅读版本清单超过容量"));
        }
        let marker: Marker = serde_json::from_slice(&bytes)?;
        if marker.version != 1
            || marker.content_hash != revision.content_hash
            || marker.resource_hash != revision.resource_hash
            || marker.book_uuid != revision.book_uuid
            || marker.rev_no != revision.fence.rev_no
            || marker.index_generation != revision.fence.index_generation
            || Path::new(&marker.main).file_name() != path.file_name()
        {
            return Err(invalid(&marker_path, "阅读版本清单身份不符"));
        }
        for item in &marker.chapter_sources {
            if item.path.is_empty()
                || item.path.contains(['\\', ':'])
                || Path::new(&item.path)
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
                || item.bytes > 16 << 20
            {
                return Err(invalid(&marker_path, "章节产物路径或容量非法"));
            }
        }
        if !marker.chapter_sources.is_empty()
            && content.flow_chapter_count() != marker.chapter_sources.len()
        {
            return Err(invalid(&marker_path, "章节产物数量与索引不一致"));
        }
        marker.chapter_sources
    } else {
        return Err(invalid(&marker_path, "缺少固定阅读版本清单"));
    };
    let inner = folio_format::document::with_container(meta, content, &path, passphrase);
    Ok(Box::new(VersionDocument {
        inner,
        root: root.into(),
        artifacts,
    }))
}

trait ChapterCount {
    fn flow_chapter_count(&self) -> usize;
}
impl ChapterCount for folio_format::Content {
    fn flow_chapter_count(&self) -> usize {
        match self {
            Self::Reflow(flow) => flow.chapters.len(),
            Self::Fixed(_) => 0,
        }
    }
}

fn invalid(path: &Path, why: &str) -> Error {
    Error::Io {
        path: path.into(),
        source: why.to_string().into(),
    }
}

struct VersionDocument {
    inner: Box<dyn Document>,
    root: PathBuf,
    artifacts: Vec<ChapterArtifact>,
}
impl Document for VersionDocument {
    fn meta(&self) -> &DocMeta {
        self.inner.meta()
    }
    fn kind(&self) -> ContentKind {
        self.inner.kind()
    }
    fn content(&self) -> ContentRef<'_> {
        self.inner.content()
    }
    fn image_bytes(&self, href: &str) -> Option<Vec<u8>> {
        self.inner.image_bytes(href)
    }
    fn resources(&self) -> Vec<Resource> {
        self.inner.resources()
    }
    fn resource_bytes(&self, href: &str) -> Option<Vec<u8>> {
        self.inner.resource_bytes(href)
    }
    fn warm(&self) {
        self.inner.warm();
    }
    fn stylesheet(&self) -> String {
        self.inner.stylesheet()
    }
    fn font_charset(&self) -> &[u32] {
        self.inner.font_charset()
    }
    fn fonts(&self, wanted: &[String]) -> Vec<FontBytes> {
        self.inner.fonts(wanted)
    }
    fn font_refs(&self, wanted: &[String]) -> Option<Vec<FontRef>> {
        self.inner.font_refs(wanted)
    }
    fn font_refs_in(&self, wanted: &[String]) -> Option<Vec<FontRef>> {
        self.inner.font_refs_in(wanted)
    }
    fn read_font(&self, font: &FontRef) -> Option<Vec<u8>> {
        self.inner.read_font(font)
    }
    fn cover_ref(&self) -> Option<&CoverRef> {
        self.inner.cover_ref()
    }
    fn set_cover_ref(&mut self, cover: Option<CoverRef>) {
        self.inner.set_cover_ref(cover);
    }
    fn render_page(&self, page: usize, edge: u32) -> Option<Vec<u8>> {
        self.inner.render_page(page, edge)
    }
    fn render_page_region(
        &self,
        page: usize,
        region: PageRegion,
        width: u32,
        height: u32,
    ) -> Option<Vec<u8>> {
        self.inner.render_page_region(page, region, width, height)
    }
    fn page_text(&self, page: usize) -> Option<PageText> {
        self.inner.page_text(page)
    }
    fn page_texts(&self) -> Option<Vec<String>> {
        self.inner.page_texts()
    }
    fn chapter_source(&self, index: usize) -> folio_format::Result<String> {
        if self.artifacts.is_empty() {
            return self.inner.chapter_source(index);
        }
        let failed = || folio_format::Error::ChapterUnavailable {
            path: self.root.clone(),
            index,
            at: "固定章节产物".into(),
        };
        let item = self.artifacts.get(index).ok_or_else(failed)?;
        let path = self.root.join(&item.path);
        let root = self.root.canonicalize().map_err(|_| failed())?;
        let actual = path.canonicalize().map_err(|_| failed())?;
        if !actual.starts_with(root) {
            return Err(failed());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&actual)
            .map_err(|_| failed())?
            .take(item.bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failed())?;
        if bytes.len() as u64 != item.bytes
            || folio_base::identity::sha256_of_bytes(&bytes).to_hex() != item.hash
        {
            return Err(failed());
        }
        String::from_utf8(bytes).map_err(|_| failed())
    }
}

pub(crate) fn write_chapter_artifacts(
    doc: &dyn Document,
    root: &Path,
) -> Result<Vec<ChapterArtifact>> {
    if !matches!(
        doc.format(),
        folio_format::Format::Text | folio_format::Format::Markdown
    ) {
        return Ok(Vec::new());
    }
    let Some(flow) = doc.flow() else {
        return Ok(Vec::new());
    };
    let directory = root.join(".folio-chapters");
    std::fs::create_dir(&directory).io_path(&directory)?;
    let mut files = Vec::new();
    let mut total = 0usize;
    for index in 0..flow.chapters.len() {
        let source = doc.chapter_source(index)?;
        total = total.saturating_add(source.len());
        if source.len() > 16 << 20 || total > 64 << 20 {
            return Err(invalid(&directory, "固定章节产物超过容量预算"));
        }
        let name = format!(".folio-chapters/{index}.xhtml");
        let path = root.join(&name);
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .io_path(&path)?;
        file.write_all(source.as_bytes()).io_path(&path)?;
        file.sync_all().io_path(&path)?;
        files.push(ChapterArtifact {
            path: name,
            hash: folio_base::identity::sha256_of_bytes(source.as_bytes()).to_hex(),
            bytes: source.len() as u64,
        });
    }
    Ok(files)
}
