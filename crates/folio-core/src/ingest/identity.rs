//! 内容寻址：把一本书的字节算成一个身份，再落成一个文件名。
//!
//! 书的身份从「它在哪儿」换成「它是什么」（`plans/导入管线.md` 第六节）。换掉之后
//! 三件事一次解决：书库搬家不再让整库认不出、同一本书从三个来源进来只存一份、
//! 读者改了文件名也不丢记录。代价是文件名不再是给人看的那个名字，所以可移植性
//! 要从「存储形状」改成一条显式的**导出**功能。
//!
//! 两个数各管各的用途：**完整 32 字节留在库里**（判等、将来的换算法迁移都要它），
//! **文件名只用前 10 字节的 base32**（正好 16 个字符）。截断在这里是安全的——名字
//! 只是省事的去处，判重永远走那一列完整哈希；而 64 个十六进制字符做文件名会在
//! Android 与 Windows 上咬到路径长度上限。
//!
//! 用 base32 而不是 hex 只为了短。这一份编码只收「长度是 5 的整数倍」的输入，
//! 于是 RFC 4648 的填充规则一条都用不上（10 字节 = 80 位 = 16 个五比特组，整除）。

use std::io::Read as _;
use std::path::Path;

use sha2::{Digest as _, Sha256};

/// 记进 `books.hash_algo` 的那个名字。
///
/// 哈希函数被写进了身份本身，所以「用哪个算的」必须存下来：将来换算法是**重算一列**，
/// 而不是把老数据作废。
pub const HASH_ALGO: &str = "sha256";

/// 文件名取哈希的前几个字节。10 字节 = 80 位 = base32 的 16 个字符。
const NAME_BYTES: usize = 10;

/// RFC 4648 的 base32 字母表（小写）。
const ALPHABET: [u8; 32] = *b"abcdefghijklmnopqrstuvwxyz234567";

/// 一段字节的内容身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// 完整哈希的小写十六进制。**入库用这一串**，不是文件名那一串。
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(char::from_digit((byte >> 4) as u32, 16).expect("高 4 位必定是 0..15"));
            out.push(char::from_digit((byte & 0x0f) as u32, 16).expect("低 4 位必定是 0..15"));
        }
        out
    }

    /// 文件名的主体：前 10 字节的 base32，16 个小写字母数字字符。
    pub fn file_stem(&self) -> String {
        // 10 字节装不进 u64，所以用 u128 当那个 80 位的累加器。
        let mut bits: u128 = 0;
        for byte in &self.0[..NAME_BYTES] {
            bits = (bits << 8) | u128::from(*byte);
        }
        let mut out = String::with_capacity(16);
        // 高位起取组：第 15 组从第 0 位开始，所以位移是 `35 - 5 * i` 的倒序写法。
        for i in (0..16).rev() {
            let index = usize::try_from((bits >> (5 * i)) & 0b11111).expect("5 位必定是 0..31");
            out.push(ALPHABET[index] as char);
        }
        out
    }

    /// 库里那个副本 / 封面该叫什么：`<哈希前缀>.<扩展名>`。
    ///
    /// 扩展名由调用方给（它来自格式判定，不是来自来源那个可能被人改过的文件名），
    /// 这里只把它归一化：去掉可能带的点、转小写。没有扩展名就不留那个点。
    pub fn file_name(&self, ext: &str) -> String {
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        if ext.is_empty() {
            self.file_stem()
        } else {
            format!("{}.{ext}", self.file_stem())
        }
    }
}

/// 已经在内存里的那段字节（封面原图）算身份。
pub fn sha256_of_bytes(bytes: &[u8]) -> ContentHash {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    finish(hasher)
}

/// 本地文件算身份。
///
/// **必须分块读**，不能 `read_to_end`：书库里的副本整本读进内存是几百 MB 级的
/// 一次性开销，而这一条正是导入的热路。
///
/// 读不下去就是**算不出身份**，所以往外传 `io::Error` 而不是给出一个数：把半截内容
/// 的哈希当成结果，等于让一个坏文件与一个真空文件可能同名 —— 那是最坏的一种错。
/// 调用方手里有路径，套 `io_path` 再往外传（见 `crate::infra::error::IoResultExt`）。
pub fn sha256_of_path(path: &Path) -> std::io::Result<ContentHash> {
    const CHUNK: usize = 64 * 1024;
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buf[..read]),
            // 信号打断不是坏消息，重试就是。
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(finish(hasher))
}

/// 两条入口共用的收尾：`Output<Sha256>` 是泛型数组，套上定长数组再进类型。
fn finish(hasher: Sha256) -> ContentHash {
    let out = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&out);
    ContentHash(bytes)
}

#[cfg(test)]
mod tests {
    //! 身份这一层的断言只有一类：**算出来的必须是真的**。
    //! 下面那几个十六进制串不是编的，是 `sha256sum` 对本机同样那份字节出的结果；
    //! base32 那串是 python 的 `b32encode` 对同一份哈希前缀出的结果。

    use super::{ContentHash, HASH_ALGO, NAME_BYTES, sha256_of_bytes, sha256_of_path};

    /// 空输入的 SHA-256。这一条守的是「整条链路上没有一处把哈希算成了别的」。
    #[test]
    fn the_empty_hash_matches_the_known_vector() {
        let hash = sha256_of_bytes(b"");
        assert_eq!(
            hash.to_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // 同一个数从文件那条路也必须一样：两条入口共用同一循环，但入口是各自的。
        let dir = std::env::temp_dir().join("folio-identity-empty");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(sha256_of_path(&path).unwrap(), hash);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 非 ASCII 的一整本正文：字节进得去、身份出得来，与 `sha256sum` 一致。
    #[test]
    fn a_utf8_file_hashes_like_sha256sum() {
        let dir = std::env::temp_dir().join("folio-identity-utf8");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("book.txt");
        std::fs::write(&path, "第一本\n").unwrap();
        let hash = sha256_of_path(&path).unwrap();
        assert_eq!(
            hash.to_hex(),
            "ce76b86475d90467c29445f0632550dc5641ec92f27f25262628c7c3403669b2"
        );
        // 同样的字节从内存那条路必须给同一个身份 —— 封面走的就是那一条。
        assert_eq!(sha256_of_bytes("第一本\n".as_bytes()), hash);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 文件名那一串的形状：16 个字符、只含 base32 字母表、与 python 的实现同值。
    #[test]
    fn the_file_stem_is_sixteen_base32_characters() {
        let hash = sha256_of_bytes(b"");
        assert_eq!(hash.file_stem(), "4oymiquy7qobjgx3");
        assert_eq!(hash.file_stem().len(), 16);
        // 一个字符都不许跳出字母表：路径里冒出 `+` `/` `=` 是要坏事的。
        let alphabet = super::ALPHABET;
        let in_alphabet = |c: char| u8::try_from(c).is_ok_and(|byte| alphabet.contains(&byte));
        assert!(
            hash.file_stem().chars().all(in_alphabet),
            "字母表之外的字符进了文件名：{}",
            hash.file_stem()
        );
        assert_eq!(NAME_BYTES, 10);
    }

    /// 大写字节也要落在字母表里（`2..7` 那几个数字之外的字符），
    /// 所以那条「全小写字母 + 数字」的断言单独再验一次高字节。
    #[test]
    fn high_bytes_still_land_inside_the_alphabet() {
        let hash = ContentHash([0xff; 32]);
        let stem = hash.file_stem();
        assert_eq!(stem, "7777777777777777", "全 1 的 80 位就是 16 个 7");
        assert_eq!(sha256_of_bytes(b"abc").file_stem().len(), 16);
    }

    /// 扩展名归一化：判定给的是 `.EPUB` 还是 `epub` 都落成一个名字，空的不留孤点。
    #[test]
    fn the_extension_is_normalized_into_the_name() {
        let hash = sha256_of_bytes(b"");
        assert_eq!(hash.file_name("epub"), format!("{}.epub", hash.file_stem()));
        assert_eq!(
            hash.file_name(".EPUB"),
            format!("{}.epub", hash.file_stem())
        );
        assert_eq!(hash.file_name(""), hash.file_stem());
    }

    /// 算法名是存进库里那一列的取值，换算法要看得见换过。
    #[test]
    fn the_recorded_algorithm_name_is_sha256() {
        assert_eq!(HASH_ALGO, "sha256");
    }
}
