//! 局域网同步的**协议形状**：路由、报文、缺书对账。
//!
//! 这一层只管「两端怎么说得上话」，不管谁去拨那个连接：
//!
//! - 服务端那一半挂在 [`super::transfer`] 那个 axum 服务器上（`tokio` 只许住在
//!   [`super::transfer`] 的边界里，见 [`super`] 的模块文档），它把这里的 DTO 解出来、
//!   用 [`crate::account`] 验过账号再答；
//! - 客户端那一半走 [`super::remote::Transport`]，与 WebDAV、翻译、检查更新同一个出网口，
//!   所以这一层的编排是**同步形状**的，能在测试里塞一只假传输离线跑完整条往返。
//!
//! 与 [`crate::sync`] 的分工：那里是「一台设备的全量快照长什么样、几份快照怎么合成一份」，
//! 这里是「快照与书怎么在局域网里递给对面那台」。合并规则两边共用，一条都没重写。
//!
//! **账号 id 是句声称，不是凭据**：`/hello` 里那串 id 任何设备都能广播，所以它只用来
//! 让对方决定「值不值得跟你握手」；真正的准入是之后那对 nonce 与双向证明
//! （见 [`crate::account`]）。证明没过，任何一条数据路由都不答。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// 这一版局域网协议。两端各带在自己每一条报文里，`version` 比本机高的对端直接拒：
/// 猜字段比连不上更难查。
pub const PROTOCOL_VERSION: u32 = 1;

/// 一次局域网同步要走的几条路由。都挂在传书那个服务器上，共用那一个端口与那一道
/// 每 IP 退避的账本。
pub const ROUTE_HELLO: &str = "/api/folio/v1/hello";
pub const ROUTE_AUTH: &str = "/api/folio/v1/auth";
pub const ROUTE_MANIFEST: &str = "/api/folio/v1/manifest";
pub const ROUTE_FILE: &str = "/api/folio/v1/file";
pub const ROUTE_SNAPSHOT: &str = "/api/folio/v1/snapshot";

/// 一次连接的会话凭据时长。局域网里一次往返（握手 + 书架清单 + 拉几本缺的书 + 两份快照）
/// 在秒级就完了，给到十分钟只是为了让「拉书拉到一半屏幕熄了」不必重来。
pub const SESSION_TTL_SECS: i64 = 600;

/// 一台设备愿意在一次同步里从对面拉多少本书。超过就是对面那台把整个库摊开了，
/// 而这一次下拉不是「把我这几本补上」的意思。
pub const MAX_PULL_BOOKS: usize = 200;

/// 一本书能从对面送过来的上限。字节是流式读的，所以这一格**不是**内存的界
/// （一 GiB 的书也只是一次 256 KiB），它管的是「一次下拉愿意为一本书等多久」：
/// 家庭 WiFi 上 30–50 MB/s，一 GiB 是二三十秒，已经是「读者会以为卡住了」的那一头。
/// 与传书那条 1 GiB 的上传上限同值 —— 两个方向不该有一个比另一个更宽松。
pub const MAX_PUSH_BYTES: u64 = 1024 * 1024 * 1024;

/// `POST /hello` 的请求体：我是谁（声称）、我这一半的 nonce、我说哪一版协议。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    /// 本机账号 id 的十六进制。对面拿自己算出来的那一个比。
    pub account: String,
    /// 发起方的 nonce，十六进制。与对端那个一起折成会话密钥。
    pub nonce: String,
}

/// `POST /hello` 的响应：对端那一半的 nonce、它出示的证明、这次连接的会话凭据，
/// 还有它此刻的钟。
///
/// `now` 不是装饰：两端各自的表对不上是常态（飞行模式几天、手机自己同步过一次），
/// 而局域网给了唯一一次「当场对表」的机会 —— 拿它对 [`crate::sync::Snapshot::shift_clock`]，
/// 比对那些隔着云端写下来的时间戳可靠。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloReply {
    pub version: u32,
    pub account: String,
    pub nonce: String,
    /// 对端作为接收方出示的证明（[`crate::account::Side::Responder`] 那一句）。
    pub proof: String,
    /// 之后每一条请求都要带的会话凭据。
    pub token: String,
    /// 对端的 UTC 秒。
    pub now: i64,
}

/// `POST /auth` 的请求体：发起方出示它那一句证明。过了这道门才谈数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    pub token: String,
    pub proof: String,
}

/// `GET /manifest` 的响应：对面那台机器上**有副本可拉**的书。
///
/// 只带 `content_hash`、大小与书名。`hash` 就是请求 `/file` 用的键，路径由对端按自己的
/// 数据库解 —— 书库目录在哪儿、副本叫什么名，都是各台自己的事，不该跨机说。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub books: Vec<BookEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookEntry {
    /// `books.content_hash` 那一格：64 个小写十六进制字符。
    pub hash: String,
    pub size: u64,
    /// 只用于日志与报表，让「这次拉了 3 本」说得出具体的书。
    pub title: String,
}

/// 本机缺哪几本。`local_hashes` 是本机 `books.content_hash` 的集合，
/// 缺的那几本按对面给的顺序原样返回。
///
/// 大小不认识的一律不要：对面报 0 字节的条目要么是它自己算错，要么是被改过的报文，
/// 两种都不值得为它开一次连接。
///
/// 一本书的副本在库里就叫 `<哈希前缀>.<扩展名>`，内容相同即同一本 —— 所以这一格
/// 不需要书名、作者之类的模糊匹配，也不会出现「两本书合成一条」。
pub fn missing(local_hashes: &HashSet<String>, remote: &Manifest) -> Vec<BookEntry> {
    remote
        .books
        .iter()
        .filter(|entry| entry.size > 0 && !local_hashes.contains(&entry.hash))
        .take(MAX_PULL_BOOKS)
        .cloned()
        .collect()
}

/// 一次局域网同步的报表。下拉刷新结束后屏幕上那句「补了 2 本、合了 5 条」从这些数来。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// 握上了手、也验过账号的对端数。
    pub peers: usize,
    /// 验不过对方账号、因此没有下文的那些地址（路由器后面常有不是 folio 的设备）。
    pub rejected: usize,
    /// 因为不在本机而拉过来的书。
    pub pulled: usize,
    /// 拉过来但导入拒了（不是书、坏了、带 DRM）。
    pub failed: usize,
    /// 落进本机的进度 / 批注 / 书签 / 设置条数，与 [`crate::sync::Report`] 同一口径。
    pub applied_positions: usize,
    pub applied_annotations: usize,
    pub applied_bookmarks: usize,
}

impl Report {
    /// 这一次有没有动过任何东西。屏幕上「没有可同步的」与「同步失败」是两句话，
    /// 靠这一格分开。
    pub fn touched_anything(&self) -> bool {
        self.pulled > 0
            || self.failed > 0
            || self.applied_positions > 0
            || self.applied_annotations > 0
            || self.applied_bookmarks > 0
    }
}

#[cfg(test)]
mod tests {
    use super::{BookEntry, MAX_PULL_BOOKS, Manifest, Report, missing};
    use std::collections::HashSet;

    fn manifest(entries: Vec<BookEntry>) -> Manifest {
        Manifest {
            version: 1,
            books: entries,
        }
    }

    fn entry(hash: &str, size: u64) -> BookEntry {
        BookEntry {
            hash: hash.to_string(),
            size,
            title: format!("《{hash}》"),
        }
    }

    /// 身份就是内容：本机有那一条哈希就不该再拉一遍，哪怕对面那本的名字完全不同。
    #[test]
    fn a_book_the_local_library_already_has_is_not_pulled_again() {
        let local: HashSet<String> = ["aaaa".to_string(), "bbbb".to_string()]
            .into_iter()
            .collect();
        let remote = manifest(vec![entry("aaaa", 10), entry("cccc", 20)]);

        let missing = missing(&local, &remote);
        assert_eq!(
            missing.iter().map(|e| e.hash.as_str()).collect::<Vec<_>>(),
            vec!["cccc"]
        );
    }

    #[test]
    fn an_empty_library_takes_everything_the_peer_offers() {
        let local = HashSet::new();
        let remote = manifest(vec![entry("aaaa", 10), entry("bbbb", 20)]);
        assert_eq!(
            missing(&local, &remote).len(),
            2,
            "对账不该在本机为空时漏掉任何一本"
        );
    }

    /// 报 0 字节的条目不认：要么对面算错，要么报文被改过，两种都不值得开连接。
    #[test]
    fn a_zero_size_entry_is_not_a_book() {
        let local = HashSet::new();
        let remote = manifest(vec![entry("aaaa", 0), entry("bbbb", 1)]);
        let got = missing(&local, &remote);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].hash, "bbbb");
    }

    /// 对面把整库摊开时封顶，别让一次下拉变成搬完一本书库；截的是顺序后面的，
    /// 剩下的下一轮再来。
    #[test]
    fn a_huge_manifest_is_cut_at_the_cap() {
        let local = HashSet::new();
        let many: Vec<BookEntry> = (0..MAX_PULL_BOOKS + 50)
            .map(|i| entry(&format!("{i:04}"), 1))
            .collect();
        let got = missing(&local, &manifest(many));
        assert_eq!(got.len(), MAX_PULL_BOOKS);
        assert_eq!(got.first().unwrap().hash, "0000", "该留的是前面的那些");
    }

    #[test]
    fn the_report_distinguishes_nothing_to_do_from_failure() {
        assert!(
            !Report::default().touched_anything(),
            "全零的报表就是「这轮没动任何东西」"
        );
        let pulled = Report {
            pulled: 1,
            ..Default::default()
        };
        assert!(pulled.touched_anything());
        // 只有「对面被验账号挡掉」这一种失败没动本机任何东西，它得走另一句话。
        let rejected = Report {
            peers: 1,
            rejected: 3,
            ..Default::default()
        };
        assert!(!rejected.touched_anything());
    }

    /// 报文要能原样过 JSON：两端各写各的，解不回来就是协议对不上。
    #[test]
    fn the_wire_shapes_round_trip_through_json() {
        let hello = super::Hello {
            version: super::PROTOCOL_VERSION,
            account: "0123456789abcdef".into(),
            nonce: "beef".into(),
        };
        let text = serde_json::to_string(&hello).unwrap();
        assert_eq!(
            text,
            r#"{"version":1,"account":"0123456789abcdef","nonce":"beef"}"#
        );

        let reply = super::HelloReply {
            version: super::PROTOCOL_VERSION,
            account: "ff".into(),
            nonce: "aa".into(),
            proof: "bb".into(),
            token: "cc".into(),
            now: 1_800_000_000,
        };
        let back: super::HelloReply =
            serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
        assert_eq!(back, reply);
    }

    /// 读对面报文的容错：多出来的字段（将来加了新东西的老对端）不能把这一端顶掉。
    #[test]
    fn a_manifest_with_unknown_fields_still_parses() {
        let got: Manifest = serde_json::from_str(
            r#"{"version":1,"books":[{"hash":"aa","size":7,"title":"x","future":true}],"extra":1}"#,
        )
        .unwrap();
        assert_eq!(got.books.len(), 1);
        assert_eq!(got.books[0].size, 7);
    }
}
