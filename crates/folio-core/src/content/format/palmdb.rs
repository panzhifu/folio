//! PalmDB 容器的记录寻址 —— MOBI / AZW / AZW3 共用的那一层。
//!
//! PalmDB 是一个**记录数据库**：文件头之后是一张定长表，第 i 项给出第 i 条记录
//! 相对文件开头的偏移。没有条目名、没有中央目录，所以它的寻址比 ZIP 一族简单一个
//! 量级，但也因此**没法按名字找**：格式那一层（[`super::mobi`]）自己把「一张图」
//! 译成「第几号记录」，这一层只按号给字节。
//!
//! ## 为什么单列一个文件而不是并进 [`super::container::Container`]
//!
//! `plans/导入管线.md` 第十节要的是「容器家族比格式少，所以寻址只写一份」，这一层
//! 就是那份。并进阶梯式的 `Container` 现在还谈不上：`Container` 的键是**归一化条目名**，
//! 而 PalmDB 的键是记录号，把两种键塞进同一个 `read(href)` 要么给 PalmDB 编一套
//! 假路径命名空间，要么让 `Container` 知道格式知识 —— 两条都是把漂移藏进类型里。
//! 等 PDF（第三种家族）落地时一起决定那一格怎么长，比现在先编一套更便宜。
//!
//! ## 与 ZIP 那一族的三处相反
//!
//! **一、索引在文件开头**，不在末尾。ZIP 的中央目录在末尾，所以每次重开容器都要
//! 把几千个条目名重解析一遍 —— 那才是 `Container` 懒开 + 共享句柄存在的理由。
//! PalmDB 的记录表是 `78 + 8 × 记录数` 字节（一本八百条记录的書不到 7 KB），
//! 重开一次的代价就是一次小读，所以 provider 每次现开现读是可接受的，
//! 不需要先把句柄挂到阅读会话上。
//!
//! **二、记录长度是推出来的，不是写的**。表里只有每条记录的起始偏移，第 i 条的长度
//! = 第 i+1 条的起点 − 第 i 条的起点，最后一条到文件尾。所以**偏移必须单调不减**，
//! 一处倒挂就是文件坏了 —— 这一条在 `open` 里当结构校验做，不做的话后面每次读
//! 都会拿到一段负长度或越界切片。
//!
//! **三、尾部填充**。PalmOS 把每条记录补齐到 2 字节边界（常见的还有 4 的倍数），
//! 由 `attributes` 那一位决定。填充不属于内容，但它在偏移表里占了位置，
//! 所以每条记录的**真实长度只能到下一条的起点**，填充留在末尾由格式层剥掉
//! （MOBI 那一层的「extra data」剥除见 [`super::mobi::extra_data_len`]，它读的是
//! 记录末尾那几个字节，不是这里的长度）。

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use crate::Result;
use crate::infra::error::IoResultExt as _;

/// 数据库头固定的字节数：名字 32 + 属性/版本 4 + 三个时间戳 12 + 修改号 4 +
/// appInfo/sortInfo 两个偏移 8 + 类型 4 + 创建者 4 + 种子 4 + 下一条表偏移 4 +
/// 记录数 2。记录信息表紧接其后（`78`）。
const HEADER: u64 = 78;
/// 记录信息表一项的字节数：起始偏移 4 + 属性 1 + 内部 id 3。
const ENTRY: usize = 8;
/// 记录数的上限。按 4096 一条正文记录算，262144 条就是 1 GB 正文 —— 真书到不了，
/// 而一张越界的表会让下面那次 `read_to_vec` 先分配几百 MB 再失败。
const MAX_RECORDS: usize = 262_144;

/// 一条记录的字节范围与头里那三格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PalmRecord {
    /// 相对文件开头的起始偏移。
    pub start: u64,
    /// 到下一条记录起点为止的字节数（含 PalmOS 的尾部填充）。
    pub len: usize,
    /// 记录属性字节（位 0 是那一条「删除中」标记；填充规则看它的位 6）。
    pub attributes: u8,
    /// 表里那 3 个字节读成的内部 id。MOBI 用它认记录（图片、索引各占一串号），
    /// 但**不当身份**：真书里常见全 0，寻址只用序号。
    pub uid: u32,
}

/// 开起来的 PalmDB：一张记录表 + 一个供反复取的句柄。
///
/// **取字节走 `&self`**：一本 AZW3 的正文与图片是排版线程与绘制线程共用的一条字节源
/// （`Document: Send + Sync`，见 `container.rs` 模块头的第二条约束），所以文件句柄
/// 只能用 `Mutex` 包住，不能靠 `&mut self` 要独占。锁中毒（另一个线程持着它 panic 了）
/// 时**继续用**：里面的 `File` 是个只读游标，半途 panic 不会让它变成半个状态，
/// 而在这里拒绝服务等于把一次后台 panic 升级成「这本书打不开」。
#[derive(Debug)]
pub(crate) struct PalmDb {
    path: PathBuf,
    /// 头里那 32 字节的名字（Mobipocket 写的是「书名 + 作者」，只当标题兜底用）。
    db_name: String,
    /// 数据库类型（偏移 60 那四字节，书类都是 `BOOK`）。
    db_type: [u8; 4],
    records: Vec<PalmRecord>,
    file: std::sync::Mutex<std::fs::File>,
}

impl PalmDb {
    /// 读头 + 记录表。**不**读任何记录内容。
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let mut file = std::fs::File::open(path).io_path(path)?;
        let file_len = file.metadata().io_path(path)?.len();
        if file_len < HEADER {
            return Err(crate::Error::UnsupportedFormat(
                "PalmDB 头不够长".to_string(),
            ));
        }
        let mut head = [0u8; HEADER as usize];
        file.read_exact(&mut head).io_path(path)?;
        let count = read_u16(&head, 76) as usize;
        if count == 0 || count > MAX_RECORDS {
            return Err(crate::Error::UnsupportedFormat(if count == 0 {
                "PalmDB 一条记录都没有".to_string()
            } else {
                format!("PalmDB 记录数超出合理范围：{count}")
            }));
        }

        let table_len = count
            .checked_mul(ENTRY)
            .and_then(|n| u64::try_from(n).ok())
            .filter(|n| HEADER + n <= file_len)
            .ok_or_else(|| crate::Error::UnsupportedFormat("PalmDB 记录表被截断".to_string()))?;
        let mut table = vec![0u8; table_len as usize];
        file.seek(SeekFrom::Start(HEADER)).io_path(path)?;
        file.read_exact(&mut table).io_path(path)?;

        // 起点先整列读出来，长度再由「下一条的起点」推：这样倒挂一眼就能看出来，
        // 而不是到某一条才算出一个负数。
        let mut starts = Vec::with_capacity(count);
        for i in 0..count {
            let at = i * ENTRY;
            let start = read_u32(&table, at) as u64;
            if start > file_len || (i > 0 && start < starts[i - 1]) {
                return Err(crate::Error::UnsupportedFormat(format!(
                    "PalmDB 记录表不单调或越界：第 {i} 条起点 {start}，文件长 {file_len}"
                )));
            }
            starts.push(start);
        }
        let records = starts
            .iter()
            .enumerate()
            .map(|(i, &start)| {
                let at = i * ENTRY;
                let end = starts.get(i + 1).copied().unwrap_or(file_len);
                PalmRecord {
                    start,
                    len: (end - start) as usize,
                    attributes: table[at + 4],
                    uid: read_u24(&table, at + 5),
                }
            })
            .collect();

        Ok(Self {
            path: path.to_path_buf(),
            db_name: trim_nul(&head[..32]),
            db_type: [head[60], head[61], head[62], head[63]],
            records,
            file: std::sync::Mutex::new(file),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// 头里的数据库名。Mobipocket 在这一格写的是「书名」，`EXTH` 缺席时它是唯一的标题来源。
    pub(crate) fn db_name(&self) -> &str {
        &self.db_name
    }

    /// 数据库类型四字节。`BOOK` 之外（`TEXT`、`DATA`、`zLch` 词典那些）不该当书读。
    pub(crate) fn db_type(&self) -> &[u8; 4] {
        &self.db_type
    }

    pub(crate) fn record_count(&self) -> usize {
        self.records.len()
    }

    /// 第 `index` 条记录的元数据；号外是 `None`。
    pub(crate) fn record(&self, index: usize) -> Option<&PalmRecord> {
        self.records.get(index)
    }

    /// 第 `index` 条记录的全部字节。号外、空记录、读不到都是 `None` ——
    /// 与 [`super::zip`] 那一族同一条口径：取不到字节不等于这本书坏了。
    pub(crate) fn read(&self, index: usize) -> Option<Vec<u8>> {
        self.read_bounded(index, usize::MAX)
    }

    /// 第 `index` 条记录**开头**至多 `max` 个字节，越界的尾巴裁掉。
    ///
    /// 只为一嗅图片魔数、或读一条记录的头几格而不把 2 MB 的正文扛回来。
    pub(crate) fn read_head(&self, index: usize, max: usize) -> Option<Vec<u8>> {
        self.read_bounded(index, max)
    }

    /// 第一条以 `magic` 开头的记录号。
    ///
    /// PalmDB 没有名字，格式层只能按「记录里装的是什么」找：AZW4 的那条 PDF 记录走的就是
    /// 这一条，判定层（`format::sniff::palm`）与 provider 共用它，所以「这是不是 AZW4」
    /// 在两边是同一个判据。只读每条记录的头几个字节，不把正文扛回来。
    pub(crate) fn find_record_starting_with(&self, magic: &[u8]) -> Option<usize> {
        (0..self.records.len()).find(|&index| {
            self.read_head(index, magic.len())
                .is_some_and(|head| head == magic)
        })
    }

    /// 两条取字节的路共用的一步：`&self` + 锁里的游标。
    ///
    /// 读不满（文件被截短、竞态改了它）当取不到，而不是把半截 buffer 交出去：
    /// 半个 EXTH、半个正文记录都会解析成**看着合法**的垃圾，那比失败更难查。
    fn read_bounded(&self, index: usize, max: usize) -> Option<Vec<u8>> {
        let record = *self.records.get(index)?;
        let want = record.len.min(max);
        if want == 0 {
            return Some(Vec::new());
        }
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let mut buf = vec![0u8; want];
        file.seek(SeekFrom::Start(record.start)).ok()?;
        match file.read_exact(&mut buf) {
            Ok(()) => Some(buf),
            Err(_) => None,
        }
    }
}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

/// PalmDB 全线大端：32 位字段按四个字节读。
fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// 记录表里那三个字节内部 id（大端）。
fn read_u24(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([0, bytes[at], bytes[at + 1], bytes[at + 2]])
}

/// PalmDB 的定长名字字段：NUL 结尾、不保证有终止符，尾部也可能带空格。
fn trim_nul(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).trim().to_string()
}

#[cfg(test)]
mod tests {
    //! PalmDB 的夹具是手工摆字节摆出来的：这一层的契约就是「按表给字节」，
    //! 所以断言看的也是表，不是任何一本书。

    use std::io::Write as _;
    use std::path::Path;

    use super::{HEADER, PalmDb};

    /// 摆一个最小 PalmDB：头 78 字节 + `entries` 那张表 + 依表放好的记录内容。
    ///
    /// `records` 里每条是「内容字节」，起点与长度由这里算出来写进表 —— 夹具与
    /// 被测代码各摆各的表，那些断言就只证明了解析器自洽。
    fn db_at(dir: &Path, name: &str, db_name: &str, records: &[&[u8]]) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        let mut head = vec![0u8; HEADER as usize];
        head[..db_name.len()].copy_from_slice(db_name.as_bytes());
        head[60..64].copy_from_slice(b"BOOK");
        head[64..68].copy_from_slice(b"MOBI");
        head[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
        file.write_all(&head).unwrap();
        // 数据区从「头 + 表」之后开始，紧挨着排：真实工具会补到 2 字节边界，
        // 这里不补，因为长度本来就是由下一条起点推的，补不补都不该改变结果。
        let mut start = HEADER as usize + records.len() * 8;
        let mut table = Vec::with_capacity(records.len() * 8);
        for record in records {
            table.extend_from_slice(&(start as u32).to_be_bytes());
            table.push(0);
            table.extend_from_slice(&[0, 0, 0]);
            start += record.len();
        }
        file.write_all(&table).unwrap();
        for record in records {
            file.write_all(record).unwrap();
        }
        path
    }

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("临时目录")
    }

    /// 记录的长度**不在表里**，是从下一条的起点推出来的：最后一条到文件尾。
    /// 这一条断言守的是那条推算 —— 它错了，每一本书都会少读或多读一截。
    #[test]
    fn record_length_is_derived_from_the_next_start() {
        let dir = tmp();
        let path = db_at(
            dir.path(),
            "a.pdb",
            "书名",
            &[b"first", b"second-record", b"tail"],
        );
        let db = PalmDb::open(&path).unwrap();
        assert_eq!(db.record_count(), 3);
        assert_eq!(db.read(0).as_deref(), Some(&b"first"[..]));
        assert_eq!(db.read(1).as_deref(), Some(&b"second-record"[..]));
        // 最后一条吃到文件尾：没有「下一条」可减，也不该短一截。
        assert_eq!(db.read(2).as_deref(), Some(&b"tail"[..]));
    }

    /// 名字与类型是头里那两格定长字段：NUL 之后的都不算。
    #[test]
    fn the_name_and_type_come_from_the_header() {
        let dir = tmp();
        let path = db_at(dir.path(), "a.pdb", "三体", &[b"x"]);
        let db = PalmDb::open(&path).unwrap();
        assert_eq!(db.db_name(), "三体");
        assert_eq!(db.db_type(), b"BOOK");
    }

    /// 只有 `read_head` 会走的那条短路：要 4 个字节就只给 4 个，长记录不必整条扛回来。
    #[test]
    fn a_short_read_does_not_carry_the_whole_record() {
        let dir = tmp();
        let path = db_at(dir.path(), "a.pdb", "n", &[b"JPEG-BYTES-LOP-SIDE"]);
        let db = PalmDb::open(&path).unwrap();
        assert_eq!(db.read_head(0, 4).as_deref(), Some(&b"JPEG"[..]));
        // 要得比记录还长时裁到记录长度，而不是把下一条的开头粘进来。
        assert_eq!(
            db.read_head(0, 4096).as_deref(),
            Some(&b"JPEG-BYTES-LOP-SIDE"[..])
        );
    }

    /// 号外的读取是 `None` 而不是 panic：容器缺了一条记录不等于这本书读不了。
    #[test]
    fn out_of_range_indexes_are_none_not_panics() {
        let dir = tmp();
        let path = db_at(dir.path(), "a.pdb", "n", &[b"only"]);
        let db = PalmDb::open(&path).unwrap();
        assert!(db.read(1).is_none());
        assert!(db.read_head(9, 4).is_none());
        assert!(db.record(1).is_none());
    }

    /// 表说第 5 条从偏移 9000 开始，而文件只有那么长：结构校验必须在 `open` 就挡住，
    /// 否则后面每次读都拿到一段越界的范围。
    #[test]
    fn a_record_table_beyond_the_file_is_rejected_at_open() {
        let dir = tmp();
        let path = db_at(dir.path(), "a.pdb", "n", &[b"one", b"two"]);
        // 把第二条的起点推到文件尾之后。
        let at = HEADER as usize + 8;
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[at..at + 4].copy_from_slice(&(900_000u32).to_be_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            PalmDb::open(&path).is_err(),
            "起点越过文件尾的记录表该在 open 就拒"
        );
    }

    /// 起点倒挂（后一条比前一条还靠前）同样是 `open` 拒：推出来的长度是负的。
    #[test]
    fn a_non_monotonic_table_is_rejected_at_open() {
        let dir = tmp();
        let path = db_at(dir.path(), "a.pdb", "n", &[b"one", b"two"]);
        let at = HEADER as usize;
        let mut bytes = std::fs::read(&path).unwrap();
        // 第一条的起点推到第二条之后。
        bytes[at..at + 4].copy_from_slice(&(u32::MAX / 2).to_be_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert!(PalmDb::open(&path).is_err(), "记录表必须单调不减");
    }

    /// 空表、被截断的表、比头还短的文件：三种「读不出结构」都在 open 定性，
    /// 不让调用方拿到一个「零记录」或「半张表」的容器去猜。
    #[test]
    fn truncated_and_empty_databases_fail_at_open() {
        let dir = tmp();
        let empty = dir.path().join("empty.pdb");
        std::fs::write(&empty, [0u8; 20]).unwrap();
        assert!(PalmDb::open(&empty).is_err(), "头都不够长");

        let path = db_at(dir.path(), "a.pdb", "n", &[b"one"]);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[76..78].copy_from_slice(&0u16.to_be_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert!(PalmDb::open(&path).is_err(), "零条记录不是书");

        // 表说有三条，实际只放了一条的空间：表本身越界。
        let path = db_at(dir.path(), "b.pdb", "n", &[b"one"]);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[76..78].copy_from_slice(&3u16.to_be_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert!(PalmDb::open(&path).is_err(), "记录表被截断");
    }

    /// 空记录（起点相等 = 长度 0）读回来是空的字节，不是 `None`：
    /// 「这条记录真的是空的」与「号取不到」在调用方是两件事。
    #[test]
    fn an_empty_record_reads_as_empty_bytes() {
        let dir = tmp();
        let path = db_at(dir.path(), "a.pdb", "n", &[b"", b"x"]);
        let db = PalmDb::open(&path).unwrap();
        assert_eq!(db.record(0).map(|r| r.len), Some(0));
        assert_eq!(db.read(0).as_deref(), Some(&[][..]));
    }

    /// **这一条钉的是 KF8 那一支的前提**：一本 AZW3 的字节源要跨过排版线程与绘制线程
    /// 共用（`Document: Send + Sync`），所以取字节必须是 `&self`。
    /// 句柄要是只能 `&mut self` 拿到，那一侧就得把整本书的字节先抄一份。
    #[test]
    fn the_handle_is_shared_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PalmDb>();

        let dir = tmp();
        let path = db_at(
            dir.path(),
            "a.pdb",
            "n",
            &[b"record-one", b"record-two", b"record-three"],
        );
        let db = std::sync::Arc::new(PalmDb::open(&path).unwrap());
        let handles: Vec<_> = (0..3u8)
            .map(|i| {
                let db = std::sync::Arc::clone(&db);
                std::thread::spawn(move || db.read(i as usize))
            })
            .collect();
        let got: Vec<Option<Vec<u8>>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            got,
            vec![
                Some(b"record-one".to_vec()),
                Some(b"record-two".to_vec()),
                Some(b"record-three".to_vec()),
            ],
            "三个线程各取一条记录，谁的字节都不该串到别人身上"
        );
    }
}
