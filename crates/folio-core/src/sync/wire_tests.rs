use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::content::anchor::TextAnchor;
use crate::content::format::{DocMeta, Format};
use crate::net::remote::dav;
use crate::net::remote::{Cancel, Method, Request, Response, Sink, Transport};
use crate::position::Locator;
use crate::store::{AnnotationStyle, Store};
use crate::sync::{
    AnnotationRecord, BookSnapshot, DeleteRecord, Plan, PositionRecord, Snapshot, Tombstone,
    TombstoneKind, sync_once,
};

#[derive(Clone, Copy)]
enum PutFault {
    Rejected,
    AcknowledgementLost,
}

struct MockDav {
    base: String,
    files: Mutex<HashMap<String, Vec<u8>>>,
    seen: Mutex<Vec<(Method, String)>>,
    next_put_fault: Mutex<Option<PutFault>>,
}

impl MockDav {
    fn new(base: &str, files: HashMap<String, Vec<u8>>) -> Self {
        Self {
            base: base.into(),
            files: Mutex::new(files),
            seen: Mutex::new(Vec::new()),
            next_put_fault: Mutex::new(None),
        }
    }

    fn fail_next_put(&self, fault: PutFault) {
        *self.next_put_fault.lock().unwrap() = Some(fault);
    }

    fn rel(&self, url: &str) -> String {
        url.strip_prefix(&self.base)
            .unwrap_or(url)
            .trim_matches('/')
            .to_string()
    }

    fn path_prefix(&self) -> &str {
        let after = self.base.split_once("://").unwrap().1;
        &after[after.find('/').unwrap()..]
    }

    fn listing(&self, dir: &str) -> String {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut entries: Vec<String> = self
            .files
            .lock()
            .unwrap()
            .keys()
            .filter(|path| path.starts_with(&prefix))
            .filter_map(|path| {
                let rest = path.strip_prefix(&prefix)?;
                (!rest.contains('/')).then(|| rest.to_string())
            })
            .collect();
        entries.sort();
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><d:multistatus xmlns:d=\"DAV:\">",
        );
        for entry in entries {
            // displayname 故意与文件名不同：同步层必须认路径末段，不能拿 displayname
            // 判「这是不是我自己那份」。
            xml.push_str(&format!(
                "<d:response><d:href>{}{}{}</d:href><d:propstat><d:prop><d:resourcetype/><d:displayname>shown</d:displayname></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
                self.path_prefix(),
                prefix,
                entry
            ));
        }
        xml.push_str("</d:multistatus>");
        xml
    }
}

impl Transport for MockDav {
    fn request(&self, req: Request) -> crate::net::remote::Result<Response> {
        self.seen
            .lock()
            .unwrap()
            .push((req.method, req.url.clone()));
        let rel = self.rel(&req.url);
        let response = match req.method {
            Method::Mkcol => Response {
                status: 201,
                headers: Vec::new(),
                body: Vec::new(),
            },
            Method::Put => {
                let fault = self.next_put_fault.lock().unwrap().take();
                if matches!(fault, Some(PutFault::Rejected)) {
                    return Err(crate::net::remote::Error::Transport("PUT rejected".into()));
                }
                self.files.lock().unwrap().insert(rel, req.body);
                if matches!(fault, Some(PutFault::AcknowledgementLost)) {
                    return Err(crate::net::remote::Error::Transport(
                        "PUT response lost after write".into(),
                    ));
                }
                Response {
                    status: 201,
                    headers: Vec::new(),
                    body: Vec::new(),
                }
            }
            Method::Get => match self.files.lock().unwrap().get(&rel) {
                Some(body) => Response {
                    status: 200,
                    headers: Vec::new(),
                    body: body.clone(),
                },
                None => Response {
                    status: 404,
                    headers: Vec::new(),
                    body: Vec::new(),
                },
            },
            Method::Propfind => {
                let xml = self.listing(rel.trim_end_matches('/'));
                Response {
                    status: 207,
                    headers: Vec::new(),
                    body: xml.into_bytes(),
                }
            }
            _ => Response {
                status: 405,
                headers: Vec::new(),
                body: Vec::new(),
            },
        };
        Ok(response)
    }

    fn download(
        &self,
        _req: Request,
        _sink: &mut dyn Sink,
        _cancel: &Cancel,
    ) -> crate::net::remote::Result<u64> {
        Err(crate::net::remote::Error::Cancelled)
    }
}

fn meta() -> DocMeta {
    DocMeta {
        title: "书".into(),
        format: Format::Epub,
        size: 1,
        ..DocMeta::default()
    }
}

fn position(offset: usize, at: i64) -> PositionRecord {
    PositionRecord {
        mode: "paged".into(),
        locator: Locator { chapter: 0, offset },
        updated_at: at,
    }
}

/// 一次完整往返：读别人的快照、应用远端记录与墓碑、写回自己的那份；自己的旧文件
/// 即使读不懂也**不许被读**（它不是事实来源，本机库才是）。
#[test]
fn a_wire_round_trip_applies_remote_state_and_replaces_our_snapshot() {
    let store = Store::in_memory().unwrap();
    let book = store.add_book(Path::new("/books/a.epub"), &meta()).unwrap();
    store.set_content_hash(book, "hash-a", "sha256").unwrap();
    store
        .set_position_at(
            book,
            "paged",
            &Locator {
                chapter: 0,
                offset: 5,
            },
            10,
        )
        .unwrap();
    store
        .add_annotation(
            book,
            &TextAnchor::default(),
            "本机",
            "amber",
            AnnotationStyle::default(),
        )
        .unwrap();
    let local_uid = store.annotations(book).unwrap()[0].uid.clone();

    // 本机那条批注的时间戳是真实的「现在」；远端要压过它，就得给未来的时间。
    let future = crate::infra::time::now() + 100;
    let mut remote_book = BookSnapshot {
        position: Some(position(9, future)),
        annotations: vec![AnnotationRecord {
            uid: "remote-a".into(),
            anchor: TextAnchor::default(),
            note: "远".into(),
            color: "rose".into(),
            style: "wavy".into(),
            created_at: future,
            updated_at: future,
        }],
        ..BookSnapshot::default()
    };
    remote_book.tombstones.push(Tombstone {
        kind: TombstoneKind::Annotation,
        uid: local_uid.clone(),
        deleted_at: future,
    });
    let mut remote = Snapshot::new("dev-a", future);
    remote.books.insert("hash-a".into(), remote_book);

    let mut files = HashMap::new();
    files.insert(
        "folio-sync/dev-a.json".to_string(),
        serde_json::to_vec(&remote).unwrap(),
    );
    // 自己的旧文件故意写成坏 JSON：这趟都不该打开它，更不该因此失败。
    files.insert(
        "folio-sync/dev-local.json".to_string(),
        b"not json".to_vec(),
    );
    let mock = Arc::new(MockDav::new("https://dav.test/dav/", files));
    let client = dav::Client::new(
        dav::Endpoint::new("https://dav.test/dav/", "u", "p"),
        mock.clone(),
    );

    let report = sync_once(&store, &client, "folio-sync", "dev-local", future + 1).unwrap();
    assert_eq!(report.remote_devices, 1);
    assert_eq!(report.applied_positions, 1);
    assert_eq!(report.applied_annotations, 1);
    assert_eq!(report.deleted_annotations, 1);

    let (mode, locator, updated_at) = store.position_with_time(book).unwrap().unwrap();
    assert_eq!(mode, "paged");
    assert_eq!(locator.offset, 9, "远端更新的进度要落到本机");
    assert_eq!(updated_at, future, "要保留远端时间戳，不能写成现在");
    let annotations = store.annotations(book).unwrap();
    assert_eq!(annotations.len(), 1);
    assert_eq!(annotations[0].uid, "remote-a", "远端批注要进来");
    assert!(
        annotations
            .iter()
            .all(|annotation| annotation.uid != local_uid),
        "远端墓碑要删掉本机那条"
    );

    {
        let seen = mock.seen.lock().unwrap();
        assert!(
            !seen
                .iter()
                .any(|(method, url)| *method == Method::Get && url.ends_with("dev-local.json")),
            "自己的旧快照不该被读取"
        );
    }

    let uploaded = mock
        .files
        .lock()
        .unwrap()
        .get("folio-sync/dev-local.json")
        .cloned()
        .expect("自己的快照要写回去");
    let uploaded: Snapshot = serde_json::from_slice(&uploaded).unwrap();
    assert_eq!(uploaded.version, 1);
    assert_eq!(uploaded.device_id, "dev-local");
    let book_snapshot = &uploaded.books["hash-a"];
    assert_eq!(book_snapshot.annotations.len(), 1);
    assert_eq!(book_snapshot.annotations[0].uid, "remote-a");
    assert_eq!(book_snapshot.tombstones.len(), 1);
    assert_eq!(book_snapshot.tombstones[0].uid, local_uid);
}

fn assert_upload_retry_converges(fault: PutFault) {
    let local = Store::in_memory().unwrap();
    let peer = Store::in_memory().unwrap();
    let local_book = local
        .add_book(Path::new("/books/local.epub"), &meta())
        .unwrap();
    let peer_book = peer
        .add_book(Path::new("/books/peer.epub"), &meta())
        .unwrap();
    local
        .set_content_hash(local_book, "hash-a", "sha256")
        .unwrap();
    peer.set_content_hash(peer_book, "hash-a", "sha256")
        .unwrap();
    local
        .set_position_at(
            local_book,
            "paged",
            &Locator {
                chapter: 0,
                offset: 5,
            },
            10,
        )
        .unwrap();
    local
        .add_annotation(
            local_book,
            &TextAnchor::default(),
            "待删除",
            "amber",
            AnnotationStyle::default(),
        )
        .unwrap();
    let removed_uid = local.annotations(local_book).unwrap()[0].uid.clone();
    let version = crate::infra::time::now() + 100;
    peer.apply_sync(&Plan {
        positions: vec![("hash-a".into(), position(9, version))],
        annotation_upserts: vec![(
            "hash-a".into(),
            AnnotationRecord {
                uid: "remote-note".into(),
                anchor: TextAnchor::default(),
                note: "远端保留".into(),
                color: "rose".into(),
                style: "wavy".into(),
                created_at: version,
                updated_at: version,
            },
        )],
        annotation_deletes: vec![DeleteRecord {
            book_hash: "hash-a".into(),
            uid: removed_uid,
            deleted_at: version,
        }],
        ..Plan::default()
    })
    .unwrap();
    let peer_snapshot = peer.collect_sync("dev-peer", version).unwrap().snapshot;
    let mock = Arc::new(MockDav::new(
        "https://dav.test/dav/",
        HashMap::from([(
            "folio-sync/dev-peer.json".into(),
            serde_json::to_vec(&peer_snapshot).unwrap(),
        )]),
    ));
    let client = dav::Client::new(
        dav::Endpoint::new("https://dav.test/dav/", "u", "p"),
        mock.clone(),
    );
    mock.fail_next_put(fault);

    assert!(sync_once(&local, &client, "folio-sync", "dev-local", version + 1).is_err());
    let after_failure = local
        .collect_sync("dev-local", version + 1)
        .unwrap()
        .snapshot;
    assert_eq!(
        after_failure.books, peer_snapshot.books,
        "上传失败不能回滚已应用的进度、批注与墓碑"
    );
    assert_eq!(local.annotations(local_book).unwrap().len(), 1);
    assert_eq!(local.annotations(local_book).unwrap()[0].uid, "remote-note");
    assert_eq!(
        mock.files
            .lock()
            .unwrap()
            .contains_key("folio-sync/dev-local.json"),
        matches!(fault, PutFault::AcknowledgementLost)
    );
    assert_eq!(
        mock.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| *method == Method::Put)
            .count(),
        1,
        "失败不能隐式重发 PUT"
    );

    let retry = sync_once(&local, &client, "folio-sync", "dev-local", version + 2).unwrap();
    assert_eq!(
        (
            retry.applied_positions,
            retry.applied_annotations,
            retry.deleted_annotations
        ),
        (0, 0, 0),
        "重复远端记录应幂等"
    );
    let after_retry = local
        .collect_sync("dev-local", version + 2)
        .unwrap()
        .snapshot;
    assert_eq!(
        after_retry.books, after_failure.books,
        "重试不能抬高记录版本或重复批注/墓碑"
    );
    let uploaded: Snapshot =
        serde_json::from_slice(&mock.files.lock().unwrap()["folio-sync/dev-local.json"]).unwrap();
    assert_eq!(uploaded.books, after_retry.books);

    let peer_report = sync_once(&peer, &client, "folio-sync", "dev-peer", version + 3).unwrap();
    assert_eq!(
        (
            peer_report.applied_positions,
            peer_report.applied_annotations,
            peer_report.deleted_annotations
        ),
        (0, 0, 0)
    );
    assert_eq!(
        peer.collect_sync("dev-peer", version + 3)
            .unwrap()
            .snapshot
            .books,
        after_retry.books,
        "两端数据库最终收敛"
    );
    let repeated = sync_once(&local, &client, "folio-sync", "dev-local", version + 4).unwrap();
    assert_eq!(
        (
            repeated.applied_positions,
            repeated.applied_annotations,
            repeated.deleted_annotations
        ),
        (0, 0, 0)
    );
    assert_eq!(
        local
            .collect_sync("dev-local", version + 4)
            .unwrap()
            .snapshot
            .books,
        after_retry.books
    );
}

#[test]
fn upload_rejection_preserves_applied_state_and_retry_converges() {
    assert_upload_retry_converges(PutFault::Rejected);
}

#[test]
fn uncertain_upload_preserves_applied_state_and_retry_converges() {
    assert_upload_retry_converges(PutFault::AcknowledgementLost);
}
