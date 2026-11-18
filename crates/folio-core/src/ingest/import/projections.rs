//! 内容更新只生成派生位置，原引文、原锚点、同步版本始终保留。

use crate::content::anchor::{TextAnchor, TextAnchorExt as _, TextMap};
use crate::error::Result;
use crate::infra::task::StageCtx;
use crate::store::{BookRevision, RevisionFence, Store};
use folio_format::document::Document;

pub(super) struct Projection {
    kind: &'static str,
    uid: String,
    original: TextAnchor,
    anchor: Option<TextAnchor>,
}

pub(super) fn prepare(
    store: &Store,
    book_id: i64,
    doc: &dyn Document,
    target_hash: &str,
    target: RevisionFence,
    ctx: &StageCtx,
) -> Result<Vec<Projection>> {
    let annotations = store.annotations(book_id)?;
    let bookmarks = store.bookmarks(book_id)?;
    if annotations.is_empty() && bookmarks.is_empty() {
        return Ok(Vec::new());
    }
    let maps = maps(doc, ctx);
    annotations
        .into_iter()
        .map(|r| ("annotation", r.uid, r.anchor))
        .chain(bookmarks.into_iter().map(|r| ("bookmark", r.uid, r.anchor)))
        .map(|(kind, uid, original)| {
            let anchor = if ctx.cancel().triggered() {
                None
            } else {
                let same = store
                    .record_origin(kind, &uid)?
                    .is_some_and(|(origin, hash)| {
                        hash == target_hash
                            && origin.index_generation == target.index_generation
                            && (!matches!(
                                doc.format(),
                                folio_format::Format::Text | folio_format::Format::Markdown
                            ) || origin == target)
                    });
                if same && doc.fixed().is_some() {
                    Some(original.clone())
                } else {
                    locate(&original, &maps, same)
                }
            };
            Ok(Projection {
                kind,
                uid,
                original,
                anchor,
            })
        })
        .collect()
}

fn maps(doc: &dyn Document, ctx: &StageCtx) -> Vec<TextMap> {
    let Some(flow) = doc.flow() else {
        return Vec::new();
    };
    let mut maps = Vec::new();
    let mut bytes = 0usize;
    for index in 0..flow.chapters.len() {
        if ctx.cancel().triggered() {
            break;
        }
        let Ok(source) = doc.chapter_source(index) else {
            return Vec::new();
        };
        bytes = bytes.saturating_add(source.len());
        if bytes > 64 << 20 {
            return Vec::new();
        }
        let dom = folio_format::dom::Dom::parse(source.as_bytes());
        let body = dom
            .elements()
            .find(|(id, _)| dom.tag_name(*id) == Some("body"))
            .map(|(id, _)| id)
            .unwrap_or(dom.document);
        maps.push(TextMap::build(&dom, body));
    }
    maps
}

fn locate(anchor: &TextAnchor, maps: &[TextMap], same: bool) -> Option<TextAnchor> {
    if same {
        let map = maps.get(anchor.chapter as usize)?;
        if anchor.end <= map.chars()
            && (anchor.exact.is_empty() || map.slice(anchor.start..anchor.end) == anchor.exact)
        {
            return Some(anchor.clone());
        }
    }
    if anchor.exact.is_empty()
        || (anchor.exact.chars().count() < 8
            && anchor.prefix.chars().count() + anchor.suffix.chars().count() < 16)
    {
        return None;
    }
    let mut winner = None;
    for (chapter, map) in maps.iter().enumerate() {
        let mut from = 0;
        while let Some(relative) = map.text()[from..].find(&anchor.exact) {
            let at = from + relative;
            let start = map.text()[..at].chars().count();
            let end = start + anchor.exact.chars().count();
            let (prefix, suffix) = map.context(start, end);
            if (anchor.prefix.is_empty() || prefix.ends_with(&anchor.prefix))
                && (anchor.suffix.is_empty() || suffix.starts_with(&anchor.suffix))
            {
                if winner.is_some() {
                    return None;
                }
                winner = Some(TextAnchor::new(map, chapter as u32, start, end));
            }
            from = at + map.text()[at..].chars().next()?.len_utf8();
        }
    }
    winner
}

pub(super) fn apply(
    store: &Store,
    book_id: i64,
    target: RevisionFence,
    projections: Vec<Projection>,
) -> Result<()> {
    for projection in projections {
        store.set_anchor_projection_if_current(
            book_id,
            projection.kind,
            &projection.uid,
            &projection.original,
            target,
            projection.anchor.as_ref(),
        )?;
    }
    Ok(())
}

pub(super) fn position(
    store: &Store,
    book_id: i64,
    previous: &BookRevision,
    next: &dyn Document,
    ctx: &StageCtx,
) -> Result<Option<(String, crate::Locator)>> {
    if previous.state != "active" || next.flow().is_none() {
        return Ok(None);
    }
    let Some((mode, locator)) = store.position_for_revision(book_id, previous.fence)? else {
        return Ok(None);
    };
    let old_path = std::path::Path::new(&previous.managed_path);
    if crate::infra::identity::sha256_of_path(old_path)
        .ok()
        .is_none_or(|h| h.to_hex() != previous.content_hash)
    {
        return Ok(None);
    }
    let old = crate::ingest::updates::open_revision_document(
        store.revision_snapshot(book_id, previous.fence)?,
        store.passphrase_for_hash(&previous.content_hash)?,
    )?;
    let old_maps = maps(old.as_ref(), ctx);
    let Some(map) = old_maps.get(locator.chapter as usize) else {
        return Ok(None);
    };
    let point = locator.offset.min(map.chars());
    let start = point.min(map.chars().saturating_sub(24));
    let end = (start + 24).min(map.chars());
    let anchor = TextAnchor::new(map, locator.chapter, start, end);
    let Some(located) = locate(&anchor, &maps(next, ctx), false) else {
        return Ok(None);
    };
    Ok(Some((
        mode,
        crate::Locator {
            chapter: located.chapter,
            offset: located.start + point - start,
        },
    )))
}

/// 同步接收新记录后可再次计算当前版的显示位置；不修改记录内容或更新时间。
pub fn refresh_anchor_projections(store: &Store, book_id: i64, ctx: &StageCtx) -> Result<()> {
    let Some(active) = store.active_revision(book_id)? else {
        return Ok(());
    };
    if active.state != "active" {
        return Ok(());
    }
    let doc = crate::ingest::updates::open_revision_document(
        store.revision_snapshot(book_id, active.fence)?,
        store.passphrase_for_hash(&active.content_hash)?,
    )?;
    let projections = prepare(
        store,
        book_id,
        doc.as_ref(),
        &active.content_hash,
        active.fence,
        ctx,
    )?;
    apply(store, book_id, active.fence, projections)
}

#[cfg(test)]
mod tests {
    use crate::content::anchor::{TextAnchor, TextMap};
    use crate::ingest::import::projections::locate;

    #[test]
    fn projection_requires_one_context_match_and_counts_overlapping_quotes() {
        let anchor = TextAnchor {
            exact: "an exact phrase".into(),
            end: 15,
            ..Default::default()
        };
        assert!(
            locate(
                &anchor,
                &[TextMap::from_visible_text(
                    "an exact phrase twice an exact phrase".into()
                )],
                false
            )
            .is_none()
        );
        let unique = locate(
            &anchor,
            &[TextMap::from_visible_text(
                "😀 before an exact phrase".into(),
            )],
            false,
        )
        .unwrap();
        assert_eq!(unique.start, 9);
        let overlap = TextAnchor {
            exact: "aaaaaaaa".into(),
            end: 8,
            ..Default::default()
        };
        assert!(
            locate(
                &overlap,
                &[TextMap::from_visible_text("aaaaaaaaa".into())],
                false
            )
            .is_none()
        );
    }
}
