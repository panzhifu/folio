use serde::{Deserialize, Serialize};

use crate::Result;
use folio_format::{
    Chapter, ChapterLocator, ChapterText, Content, CoverRef, DocMeta, FixedDoc, FixedPage, FlowDoc,
    Format, ImageRes, Series, TocEntry, locator_for,
};

#[derive(Serialize, Deserialize)]
struct Image {
    href: String,
    source: String,
    media_type: String,
    width: u32,
    height: u32,
}

impl From<&ImageRes> for Image {
    fn from(i: &ImageRes) -> Self {
        Self {
            href: i.href.clone(),
            source: i.source.clone(),
            media_type: i.media_type.clone(),
            width: i.width,
            height: i.height,
        }
    }
}
impl From<Image> for ImageRes {
    fn from(i: Image) -> Self {
        Self {
            href: i.href,
            source: i.source,
            media_type: i.media_type,
            width: i.width,
            height: i.height,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct IndexedChapter {
    title: String,
    locator: ChapterLocator,
    char_len: Option<usize>,
    images: Vec<folio_format::scan::ImageUse>,
}

#[derive(Serialize, Deserialize)]
enum Index {
    Flow {
        chapters: Vec<IndexedChapter>,
        css: String,
        css_hrefs: Vec<String>,
        toc: Vec<TocEntry>,
        images: Vec<Image>,
        font_charset: Vec<u32>,
        progression_rtl: bool,
        cover_ref: Option<(String, String)>,
    },
    Fixed {
        pages: Vec<FixedPage>,
        toc: Vec<TocEntry>,
        images: Vec<Image>,
        width: u32,
        height: u32,
        cover_ref: Option<(String, String)>,
    },
}

pub(super) fn encode(content: &Content, format: Format) -> Result<String> {
    use folio_format::ext::ChapterExt as _;
    let indexed = match content {
        Content::Reflow(flow) => Index::Flow {
            chapters: flow
                .chapters
                .iter()
                .enumerate()
                .map(|(i, c)| IndexedChapter {
                    title: c.title.clone(),
                    locator: locator_for(format, c, i),
                    char_len: c.char_len(),
                    images: c.images.clone(),
                })
                .collect(),
            css: flow.css.clone(),
            css_hrefs: flow.css_hrefs.clone(),
            toc: flow.toc.clone(),
            images: flow.images.iter().map(Image::from).collect(),
            font_charset: flow.font_charset.clone(),
            progression_rtl: flow.progression_rtl,
            cover_ref: flow
                .cover_ref
                .as_ref()
                .map(|c| (c.hash.clone(), c.mime.clone())),
        },
        Content::Fixed(fixed) => Index::Fixed {
            pages: fixed.pages.clone(),
            toc: fixed.toc.clone(),
            images: fixed.images.iter().map(Image::from).collect(),
            width: fixed.width,
            height: fixed.height,
            cover_ref: fixed
                .cover_ref
                .as_ref()
                .map(|c| (c.hash.clone(), c.mime.clone())),
        },
    };
    Ok(serde_json::to_string(&indexed)?)
}

pub(super) fn decode(json: &str) -> Result<Content> {
    Ok(match serde_json::from_str(json)? {
        Index::Flow {
            chapters,
            css,
            css_hrefs,
            toc,
            images,
            font_charset,
            progression_rtl,
            cover_ref,
        } => Content::Reflow(FlowDoc {
            chapters: chapters
                .into_iter()
                .map(|c| {
                    let spine_href = match &c.locator {
                        ChapterLocator::Entry(href) => href.clone(),
                        _ => String::new(),
                    };
                    Chapter {
                        title: c.title,
                        spine_href,
                        text: ChapterText::At(c.locator),
                        char_len: c.char_len,
                        images: c.images,
                    }
                })
                .collect(),
            css,
            css_hrefs,
            toc,
            images: images.into_iter().map(ImageRes::from).collect(),
            font_charset,
            progression_rtl,
            cover_ref: cover_ref.map(|(hash, mime)| CoverRef { hash, mime }),
            cover: None,
        }),
        Index::Fixed {
            pages,
            toc,
            images,
            width,
            height,
            cover_ref,
        } => Content::Fixed(FixedDoc {
            pages,
            toc,
            images: images.into_iter().map(ImageRes::from).collect(),
            width,
            height,
            cover_ref: cover_ref.map(|(hash, mime)| CoverRef { hash, mime }),
            cover: None,
        }),
    })
}

#[derive(Serialize, Deserialize)]
struct Metadata {
    title: String,
    authors: Vec<String>,
    contributors: Vec<String>,
    subjects: Vec<String>,
    description: String,
    publisher: String,
    published: String,
    language: String,
    isbn: String,
    dctype: String,
    rights: String,
    series: Option<(String, String)>,
    format: Format,
    size: u64,
}

pub(super) fn encode_meta(m: &DocMeta) -> Result<String> {
    Ok(serde_json::to_string(&Metadata {
        title: m.title.clone(),
        authors: m.authors.clone(),
        contributors: m.contributors.clone(),
        subjects: m.subjects.clone(),
        description: m.description.clone(),
        publisher: m.publisher.clone(),
        published: m.published.clone(),
        language: m.language.clone(),
        isbn: m.isbn.clone(),
        dctype: m.dctype.clone(),
        rights: m.rights.clone(),
        series: m
            .series
            .as_ref()
            .map(|s| (s.name.clone(), s.position.clone())),
        format: m.format,
        size: m.size,
    })?)
}

pub(super) fn decode_meta(json: &str) -> Result<DocMeta> {
    let m: Metadata = serde_json::from_str(json)?;
    Ok(DocMeta {
        title: m.title,
        authors: m.authors,
        contributors: m.contributors,
        subjects: m.subjects,
        description: m.description,
        publisher: m.publisher,
        published: m.published,
        language: m.language,
        isbn: m.isbn,
        dctype: m.dctype,
        rights: m.rights,
        series: m.series.map(|(name, position)| Series { name, position }),
        format: m.format,
        size: m.size,
    })
}
