//! 账号：一台安装自持的一把 128-bit 密钥，与「两台设备算不算同一个账号」的判据。
//!
//! 独立成 crate（2026-11-12，阶段三拆分；原 folio-core::account 原样搬来，「换 crate
//! 不换形状」：core 经模块别名沿用旧路径）。判据：它的全部依赖就是 `ring`（HMAC 与
//! OS 熵），而它同时被 core 的配置 schema（`AccountKey` 那格）与 folio-net 的局域网
//! 两族（传书 / 同步握手）消费 —— 住在任何一边都会让另一边绕路。
//!
//! 局域网同步没有服务器可问，所以「账号」不可能是一条权威记录 —— 它能有的全部含义是
//! **两台设备各自持有同一把密钥**这件事本身。本模块因此只管三件事：密钥的生成与编解码、
//! 由密钥派生的账号 id 与肉眼校验码、以及一次连接开头那对双向证明。
//!
//! 证明而不是声称：扫到一台设备说自己是某个 id，这句话不值一文 —— 局域网里任何设备都能
//! 广播一个抄来的 id。所以握手要证的是**持有那把密钥**：两端各出一个 nonce，各自算出同一把
//! 会话密钥，再各自出示「我给对方的那一句」的 MAC；两侧都验过才算同一个账号。密钥本身
//! 从不出口，id 是它的派生值，反推不回密钥。
//!
//! 密钥怎么送到第二台设备上（配对）不在这里做数学：那一步走传书已有的配对码 + 每 IP
//! 指数退避，当一次性在线门用；配对完成后配对码作废，长期信任就只剩这把密钥。也因此这把
//! 密钥的强度不能按配对码那个量级（八位数字约 26.6 bit）来定 —— 它撑的是整架书加上全部
//! 批注与进度的读取权。

use ring::rand::SecureRandom;
use ring::{hmac, rand};
use std::fmt;
use std::sync::OnceLock;

/// 密钥长度：16 字节 = 128 bit。
pub const KEY_LEN: usize = 16;

/// 账号 id 的字节数。它只用来回答「扫到的这台值不值得握手」，不参与机密性。
pub const ID_LEN: usize = 8;

/// 握手 nonce 的字节数。
pub const NONCE_LEN: usize = 16;

/// 证明（MAC）的字节数：HMAC-SHA256 的全长，不截。
pub const PROOF_LEN: usize = 32;

/// 肉眼校验码取 id 的前几个字节 —— 5 字节正好 40 bit，折成 base32 不留零头。
const CHECK_CODE_BYTES: usize = 5;

/// 肉眼校验码的字符数。
pub const CHECK_CODE_LEN: usize = 8;

/// RFC 4648 的 base32 字母表（小写），与 `folio_base::identity` 给内容哈希起文件名用的是同一张表。
const BASE32: [u8; 32] = *b"abcdefghijklmnopqrstuvwxyz234567";

/// 派生各取一个域分隔串：同一把密钥在不同用途下必须算出不同的值，否则一处泄露的 MAC
/// 就能在另一处冒充。这几串是协议的一部分，改了旧版设备就全连不上。
const DOMAIN_ID: &[u8] = b"folio/account/id";
const DOMAIN_SESSION: &[u8] = b"folio/lan/session";
const DOMAIN_INITIATOR: &[u8] = b"folio/lan/proof/initiator";
const DOMAIN_RESPONDER: &[u8] = b"folio/lan/proof/responder";

/// OS 熵那一个句柄：`SystemRandom` 每次构造都要开一次 `getrandom`，共用一份。
fn rng() -> &'static rand::SystemRandom {
    static RNG: OnceLock<rand::SystemRandom> = OnceLock::new();
    RNG.get_or_init(rand::SystemRandom::new)
}

/// 字节 → 小写十六进制。密钥、id、nonce、证明上 Wire 都走这一种编码。
fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).expect("高 4 位必定是 0..15"));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).expect("低 4 位必定是 0..15"));
    }
    out
}

/// 十六进制 → 字节。长度不是偶数、含非十六进制字符都当解不出来；大小写都收，
/// 因为这一串可能被从屏幕上复制过一次。
fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        out.push(byte);
    }
    Some(out)
}

/// 本机那把账号密钥。
///
/// `Debug` 手写而不是 derive：这一串落进日志就等于账号丢了，而配置结构整个 `Debug` 一遍
/// 是排错时最自然的一个动作。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AccountKey([u8; KEY_LEN]);

impl fmt::Debug for AccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountKey(已隐去)")
    }
}

impl AccountKey {
    /// 新生成一把。熵来自 OS（`getrandom`），不是时钟低位 —— 后者的低几位在同一台机器上
    /// 几乎是常量，那正是传书配对码当年被量出来的毛病。
    pub fn generate() -> Option<Self> {
        let mut bytes = [0u8; KEY_LEN];
        rng().fill(&mut bytes).ok()?;
        Some(Self(bytes))
    }

    /// 解配置里那一串。空串、长度不对、含非十六进制字符都回 `None`，调用方按
    /// 「这台安装还没有账号」处理。
    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = from_hex(text.trim())?;
        if bytes.len() != KEY_LEN {
            return None;
        }
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&bytes);
        Some(Self(key))
    }

    /// 落配置用这一串（32 个小写十六进制字符）。
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    /// 这个账号的 id：`HMAC-SHA256(密钥, "folio/account/id")` 的前 8 字节。
    ///
    /// 它是派生值而不是随机数，所以同一把密钥在两台设备上算出来必然相同 —— 「是不是同一个
    /// 账号」这一判因此不需要任何网络往返就能先粗筛一遍。
    pub fn id(&self) -> AccountId {
        let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &self.0), DOMAIN_ID);
        let mut id = [0u8; ID_LEN];
        id.copy_from_slice(&tag.as_ref()[..ID_LEN]);
        AccountId(id)
    }

    /// 屏幕上给读者看的那 8 个字符（id 前 5 字节的 base32）。
    ///
    /// 用途只有一个：配对完成后在两台设备上各摆一次，读者对一眼就知道中间没有第三台设备
    /// 塞进这一段局域网。明文 HTTP 的局域网里这是成本最低的篡改检出。
    pub fn check_code(&self) -> String {
        let id = self.id();
        // 5 字节装不进一次取组，先攒成一个 40 位的整数，再自高位起每组取 5 位。
        let mut bits: u64 = 0;
        for byte in &id.0[..CHECK_CODE_BYTES] {
            bits = (bits << 8) | u64::from(*byte);
        }
        let mut out = String::with_capacity(CHECK_CODE_LEN);
        for i in (0..CHECK_CODE_LEN).rev() {
            let index = usize::try_from((bits >> (5 * i)) & 0b11111).expect("5 位必定是 0..31");
            out.push(BASE32[index] as char);
        }
        out
    }

    /// 算这次连接的会话密钥。两端传进来的 `initiator` / `responder` 必须是同一对 nonce
    /// 且顺序一致 —— 谁先开口连谁就是发起方，与它是哪一端无关。
    pub fn session(&self, initiator: &Nonce, responder: &Nonce) -> Session {
        let mut message = Vec::with_capacity(DOMAIN_SESSION.len() + NONCE_LEN * 2);
        message.extend_from_slice(DOMAIN_SESSION);
        message.extend_from_slice(&initiator.0);
        message.extend_from_slice(&responder.0);
        let derived = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &self.0), &message);
        Session {
            key: hmac::Key::new(hmac::HMAC_SHA256, derived.as_ref()),
        }
    }
}

/// 账号 id。可以在局域网里明文出现，它推不回密钥。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccountId([u8; ID_LEN]);

impl AccountId {
    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = from_hex(text.trim())?;
        if bytes.len() != ID_LEN {
            return None;
        }
        let mut id = [0u8; ID_LEN];
        id.copy_from_slice(&bytes);
        Some(Self(id))
    }

    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }
}

impl fmt::Debug for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AccountId({})", self.to_hex())
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// 握手里的一个 nonce。每个连接各现出，不复用。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Nonce([u8; NONCE_LEN]);

impl Nonce {
    /// 现出一个。回 `None` 是 OS 熵源失败，调用方当作「这一次连接开不了」。
    pub fn generate() -> Option<Self> {
        let mut bytes = [0u8; NONCE_LEN];
        rng().fill(&mut bytes).ok()?;
        Some(Self(bytes))
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = from_hex(text.trim())?;
        if bytes.len() != NONCE_LEN {
            return None;
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&bytes);
        Some(Self(nonce))
    }

    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }
}

impl fmt::Debug for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Nonce({})", self.to_hex())
    }
}

/// 一次连接里双方各自的位置。先开口连的那一端是发起方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Initiator,
    Responder,
}

impl Side {
    fn domain(self) -> &'static [u8] {
        match self {
            Side::Initiator => DOMAIN_INITIATOR,
            Side::Responder => DOMAIN_RESPONDER,
        }
    }
}

/// 一次握手的会话密钥。两端由同一把账号密钥加同一对 nonce 算出来，所以不必传它。
#[derive(Clone)]
pub struct Session {
    key: hmac::Key,
}

impl Session {
    /// 处在 `side` 这个位置的一方该出示的证明。
    ///
    /// 签的是**自己那一个**标签：两侧各有一句，验证的一方按对面的标签重算。两条规则交叉
    /// 用（一方签自己的、另一方验发起方的）会让两侧算出不同的串，握手就在每一台上失败。
    pub fn own_proof(&self, side: Side) -> Proof {
        self.sign(side.domain())
    }

    /// 验对面出示的证明，`peer_side` 是对面那个位置。比较走 `ring` 的常量时间实现，不自己
    /// 写逐字节 —— 这一句的结果是一扇能读整架书的门，别让响应时间成为可测的信号。
    pub fn verify(&self, peer_side: Side, proof: &Proof) -> bool {
        hmac::verify(&self.key, peer_side.domain(), proof.as_bytes()).is_ok()
    }

    fn sign(&self, message: &[u8]) -> Proof {
        let tag = hmac::sign(&self.key, message);
        let mut proof = [0u8; PROOF_LEN];
        proof.copy_from_slice(tag.as_ref());
        Proof(proof)
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Session(会话密钥)")
    }
}

/// 一条证明（HMAC-SHA256 的全长 32 字节）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Proof([u8; PROOF_LEN]);

impl Proof {
    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = from_hex(text.trim())?;
        if bytes.len() != PROOF_LEN {
            return None;
        }
        let mut proof = [0u8; PROOF_LEN];
        proof.copy_from_slice(&bytes);
        Some(Self(proof))
    }

    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    fn as_bytes(&self) -> &[u8; PROOF_LEN] {
        &self.0
    }
}

impl fmt::Debug for Proof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Proof({})", self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        AccountId, AccountKey, CHECK_CODE_LEN, ID_LEN, KEY_LEN, NONCE_LEN, Proof, Side, from_hex,
        to_hex,
    };

    /// 配对之后要落配置、也要随配对报文传一次：解不回来就是两台设备各自握着两把不相干的密钥。
    #[test]
    fn key_round_trips_through_hex() {
        let key = AccountKey::generate().expect("OS 熵在测试里必定可用");
        let text = key.to_hex();
        assert_eq!(text.len(), KEY_LEN * 2, "16 字节要写成 32 个十六进制字符");
        assert_eq!(AccountKey::from_hex(&text), Some(key));
        // 屏幕上复制过一次、末尾带了换行的情形。
        assert_eq!(AccountKey::from_hex(&format!(" {text}\n")), Some(key));
    }

    /// 「空串就是还没有账号」是上层的一条判据，靠这里把坏输入收干净。
    #[test]
    fn a_key_string_that_is_not_a_key_reads_as_no_account() {
        let cases: Vec<String> = vec![
            String::new(),
            " ".to_string(),
            "z".to_string(),
            "zz".to_string(),
            "0123".to_string(),
            "ab".repeat(KEY_LEN - 1),
            "ab".repeat(KEY_LEN + 1),
        ];
        for bad in cases {
            assert_eq!(AccountKey::from_hex(&bad), None, "不该被解成密钥：{bad:?}");
        }
    }

    /// 密钥不进 `Debug`：配置整份打出来是排错的常态。
    #[test]
    fn the_key_never_shows_up_in_debug_output() {
        let key = AccountKey::generate().unwrap();
        let printed = format!("{key:?}");
        assert!(
            !printed.contains(&key.to_hex()),
            "`Debug` 把密钥打了出来：{printed}"
        );
        assert!(
            printed.contains("隐去"),
            "该留一句明确的隐去标记：{printed}"
        );
    }

    #[test]
    fn two_generated_keys_are_not_the_same_key() {
        let first = AccountKey::generate().unwrap();
        let second = AccountKey::generate().unwrap();
        assert_ne!(first, second, "两次生成撞同一把就是熵源退化");
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn id_is_stable_eight_bytes_and_reparses() {
        let key = AccountKey::generate().unwrap();
        let id = key.id();
        assert_eq!(id, key.id(), "id 是派生值，重算必须一样");
        let text = id.to_hex();
        assert_eq!(text.len(), ID_LEN * 2);
        assert_eq!(AccountId::from_hex(&text), Some(id));
        assert_eq!(AccountKey::from_hex(&key.to_hex()).unwrap().id(), id);
    }

    #[test]
    fn check_code_is_eight_base32_characters() {
        let key = AccountKey::generate().unwrap();
        let code = key.check_code();
        assert_eq!(code.chars().count(), CHECK_CODE_LEN);
        let alphabet = "abcdefghijklmnopqrstuvwxyz234567";
        for ch in code.chars() {
            assert!(alphabet.contains(ch), "校验码混进了字母表外的字符：{ch}");
        }
        assert_eq!(code, key.check_code());
        assert_ne!(code, AccountKey::generate().unwrap().check_code());
    }

    #[test]
    fn both_sides_prove_and_verify_across_one_session() {
        let key = AccountKey::generate().unwrap();
        let initiator = nonce();
        let responder = nonce();
        // 两端各算各的：只有 nonce 的顺序传对了，会话密钥才对得上。
        let a = key.session(&initiator, &responder);
        let b = key.session(&initiator, &responder);

        let from_a = a.own_proof(Side::Initiator);
        let from_b = b.own_proof(Side::Responder);
        assert!(b.verify(Side::Initiator, &from_a), "发起方的证明该被验过");
        assert!(a.verify(Side::Responder, &from_b), "接收方的证明该被验过");
    }

    /// 门禁本身：另一把密钥算出的证明必须进不来。
    #[test]
    fn a_proof_from_another_account_is_rejected() {
        let mine = AccountKey::generate().unwrap();
        let theirs = AccountKey::generate().unwrap();
        let (initiator, responder) = (nonce(), nonce());

        let a = mine.session(&initiator, &responder);
        let attacker = theirs.session(&initiator, &responder);
        assert!(!a.verify(Side::Responder, &attacker.own_proof(Side::Responder)));
        assert!(!a.verify(Side::Responder, &attacker.own_proof(Side::Initiator)));
    }

    /// 方向要分开：发起方那句拿去当接收方那句，必须验不过，否则一侧的泄露就两侧通吃。
    #[test]
    fn the_two_sides_do_not_share_one_proof() {
        let key = AccountKey::generate().unwrap();
        let session = key.session(&nonce(), &nonce());
        let as_initiator = session.own_proof(Side::Initiator);
        let as_responder = session.own_proof(Side::Responder);
        assert_ne!(as_initiator, as_responder, "两侧该算出不同的串");
        assert!(!session.verify(Side::Initiator, &as_responder));
        assert!(!session.verify(Side::Responder, &as_initiator));
    }

    /// nonce 参与派生：换个 nonce 就不是同一次连接，抄来的证明不能重放。
    #[test]
    fn a_stale_nonce_breaks_the_proof() {
        let key = AccountKey::generate().unwrap();
        let initiator = nonce();
        let fresh = nonce();
        let stale = nonce();

        let a = key.session(&initiator, &fresh);
        let replayed = key.session(&initiator, &stale);
        assert!(!a.verify(Side::Responder, &replayed.own_proof(Side::Responder)));
    }

    #[test]
    fn nonces_are_distinct_and_reparsable() {
        let first = nonce();
        let second = nonce();
        assert_ne!(
            first.to_hex(),
            second.to_hex(),
            "两个 nonce 相同就是熵源退化"
        );
        assert_eq!(first.to_hex().len(), NONCE_LEN * 2);
        assert_eq!(crate::Nonce::from_hex(&first.to_hex()), Some(first));
        assert_eq!(crate::Nonce::from_hex(&second.to_hex()), Some(second));
    }

    #[test]
    fn proof_hex_round_trips() {
        let key = AccountKey::generate().unwrap();
        let proof = key.session(&nonce(), &nonce()).own_proof(Side::Initiator);
        let text = proof.to_hex();
        assert_eq!(text.len(), 64);
        assert_eq!(Proof::from_hex(&text), Some(proof));
        assert_eq!(Proof::from_hex(&text[..62]), None, "少两个字符就该解不出");
    }

    #[test]
    fn hex_helpers_reject_what_they_should() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xff]), "000fff");
        assert_eq!(from_hex("0g"), None, "非十六进制字符");
        assert_eq!(from_hex("0"), None, "奇数长度不是十六进制");
        assert_eq!(
            from_hex("0F"),
            Some(vec![0x0f]),
            "大小写都收：这一串可能被复制过一次"
        );
    }

    fn nonce() -> crate::Nonce {
        crate::Nonce::generate().expect("OS 熵在测试里必定可用")
    }
}
