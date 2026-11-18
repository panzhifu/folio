//! 书目、来源和阅读版本的公共数据；路径与授权仅在本机使用。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RevisionFence {
    pub rev_no: i64,
    pub index_generation: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookSource {
    pub id: i64,
    pub book_id: i64,
    pub locator: String,
    pub kind: String,
    pub is_primary: bool,
    pub generation: i64,
    pub fingerprint: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookRevision {
    pub book_id: i64,
    pub book_uuid: String,
    pub fence: RevisionFence,
    pub content_hash: String,
    pub resource_hash: String,
    pub managed_path: String,
    pub library_root: String,
    pub resource_manifest: String,
    pub state: String,
    pub source_id: Option<i64>,
    pub source_generation: i64,
}
