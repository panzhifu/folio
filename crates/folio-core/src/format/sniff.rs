//! 格式判定：三层，**在落地之前**把不是书的东西挡掉。
//!
//! 只看魔数会漏，只看扩展名会错（`plans/导入管线.md` 第五节）：EPUB 和 CBZ 都是 ZIP，
//! 头四字节都是 `PK\x03\x04`，与 DOCX、JAR、普通压缩包无法区分；TXT 与 Markdown
//! 根本没有魔数；FB2 是 XML。所以判据分三层，每一层只负责自己判得动的那一档：
//!
//! ```text
//! 扩展名   粗筛（批量导入时排掉杂物，省掉无谓的哈希）—— 不当最终裁决
//! 魔数     PDF、PalmDB（偏移 60 处 `BOOKMOBI`）在这里就定了；ZIP 只定到「是个容器」
//! 容器     打开来读条目：EPUB 认第一个条目，CBZ 认「条目全是图」
//! ```
//!
//! **裁决以内容为准，不以名字为准**：一个叫 `书.epub` 的纯文本不会被评为 EPUB
//! （它过不了容器那一层），而一个叫 `foo.bin` 的真 EPUB 会被认出来。扩展名只在
//! 「裸文件」那一支当判据——TXT 与 MD 按定义就分不出来，这不是妥协，是没有办法。
//!
//! 判定面对的是**已经落地的本地文件**（`.part`），所以可以 seek、可以分多次读；
//! 这是第四节拆掉「一趟读同时干三件事」换来的东西之一。

use std::io::Read as _;
use std::path::Path;

use super::Format;
use super::cbz::{is_container_noise, is_page_image};
use super::zip::Zip;

/// 读多少字节起判。PalmDB 的签名在偏移 60，ZIP 的中央目录在文件**末尾**（所以容器
/// 那一层不看这一段，另开容器读），剩下的魔数与 BOM 都在最前面。
const HEAD: usize = 1024;

/// 一本书的判定结果。
///
/// 四个分支各自对应导入报告里的一句不同的话：「收下」/「这种书还不支持」/
/// 「这不是书」/「这个容器是坏的」。把它们并成一个 `Option` 就会让界面只能说
/// 「不支持的格式」，而读者对这三种的处置完全不同（换一个文件、等我们支持、
/// 或者去修那个文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// 判成了，且这一版有 provider 读得动它。
    Readable(Format),
    /// 认得出是什么，但这一版没有能读它的 provider。带上的是给人看的那一句。
    ///
    /// 三条已知的都在这一档：PDF（要栅格化引擎）、MOBI / AZW3（PalmDB 容器，
    /// 还没写那一族）、`.fb2.zip`（认得出来，但要等容器层才能把里面的 `.fb2` 取出来）。
    Known(&'static str),
    /// 三层都走完了：不是能当书收的东西。
    NotABook,
    /// 头四字节说它是个 ZIP，但中央目录读不出来（截断、坏包）。
    ///
    /// 与 [`Verdict::NotABook`] 分开：后者是「这不是书」，前者是「这是一本坏书」，
    /// 后者建议读者换个文件，前者值得把文件留下来查。
    CorruptContainer,
}

impl Verdict {
    /// 判不成的那三种换成「哪一类拒绝 + 给人看的那一句」；判成了返回 `None`。
    ///
    /// 三种分开写是故意的：「这种书还不支持」「这不是书」「这个文件坏了」对读者是三个
    /// 不同的动作（等我们支持、换一个文件、去修那个文件），并成一句「不支持的格式」
    /// 就等于什么都没说。类别用 [`crate::error::ImportReject`] 而不是文案 ——
    /// 报表要按类别分组，而文案会改。
    ///
    /// 书名不在这里拼：判定层不知道「哪一本」这件事归谁说，调用方拿着候选名自己套
    /// （见 `crate::import`）。
    pub(crate) fn rejection(self) -> Option<(crate::error::ImportReject, String)> {
        use crate::error::ImportReject;
        match self {
            Verdict::Readable(_) => None,
            Verdict::Known(kind) => Some((
                ImportReject::UnsupportedFormat,
                format!("{kind}：这一版还读不动它"),
            )),
            Verdict::NotABook => Some((
                ImportReject::UnsupportedFormat,
                "不是能收进书库的格式".to_string(),
            )),
            Verdict::CorruptContainer => Some((
                ImportReject::CorruptContainer,
                "容器读不出来，文件可能是坏的".to_string(),
            )),
        }
    }

    /// 收下时的那个格式。落地要用它定文件名的扩展名（身份来自内容，扩展名来自判定，
    /// 而不是来自来源那个可能被人改过的文件名）。
    ///
    /// 借 `&self` 而不是吃掉自己：调用方拿到 `None` 之后还要接着问「那是哪一种拒绝」
    /// （`rejection` 消费 self），两步都写在同一段里最清楚。
    pub(crate) fn format(&self) -> Option<Format> {
        match self {
            Verdict::Readable(format) => Some(*format),
            _ => None,
        }
    }
}

/// 判定一本书。`display_name` 是来源给的建议名（只用来取扩展名），`path` 是
/// 已经落地的那个本地文件——**内容说了算**。
pub(crate) fn identify(display_name: &str, path: &Path) -> Verdict {
    let head = match read_head(path) {
        Some(head) => head,
        // 连头都读不到：谈不上判定。当成坏文件报，让它走「拒绝 → 删暂存」那条路。
        None => return Verdict::CorruptContainer,
    };

    if let Some(verdict) = magic(&head) {
        return verdict;
    }
    if head.starts_with(b"PK\x03\x04") {
        return container(path);
    }
    bare_file(display_name, &head)
}

/// 第二层：头部字节。只有这里能一锤定音的才在这一层返回。
fn magic(head: &[u8]) -> Option<Verdict> {
    if head.starts_with(b"%PDF") {
        return Some(Verdict::Known("PDF"));
    }
    // PalmDB 的数据库类型在偏移 60..68。MOBI / AZW / AZW3 都是这一个容器。
    if head.len() >= 68 && head[60..68] == *b"BOOKMOBI" {
        return Some(Verdict::Known("MOBI / AZW3"));
    }
    // UTF-16 的两个 BOM。provider 那一条路是 `read_to_string`（只吃 UTF-8），
    // 所以这是「认得出来但读不动」，不是「不是书」——别说成读者手里那是个假文件。
    if head.starts_with(b"\xff\xfe") || head.starts_with(b"\xfe\xff") {
        return Some(Verdict::Known("UTF-16 文本"));
    }
    None
}

/// 第三层：ZIP 家族。三种书共用一个容器，所以它们在这里分家。
fn container(path: &Path) -> Verdict {
    let mut zip = match Zip::open(path) {
        Ok(zip) => zip,
        Err(_) => return Verdict::CorruptContainer,
    };
    // EPUB 的正解：第一个条目必须是 `mimetype`，内容是 `application/epub+zip`。
    // 规范还要求它「存而不压缩」（所以硬读偏移 38 那 20 个字节就能判）；这里走的是
    // 中央目录，压缩方法只剩优化意义，**不当判据**——真书的打包工具并不都守这一条。
    if let Some((name, _stored)) = zip.head_entry()
        && name == "mimetype"
    {
        let body = zip.read(&name).unwrap_or_default();
        if std::str::from_utf8(&body)
            .map(str::trim)
            .is_ok_and(|text| text == "application/epub+zip")
        {
            return Verdict::Readable(Format::Epub);
        }
    }
    let names = zip.names();
    // 噪音条目（目录、`.DS_Store`、`__MACOSX/`）不参与判断——它们既不是页，
    // 也不该让一个正常包被评成「不是漫画包」。
    let entries: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| !is_container_noise(name))
        .collect();
    // `.fb2.zip`：条目里有一个 `.fb2` 就是它。认得出来，但把那个条目取出来要等
    // 容器层（`plans/导入管线.md` 第十节的 `Package`），现在还没有能读它的 provider。
    if entries
        .iter()
        .any(|name| extension_of(name).is_some_and(|ext| ext == "fb2"))
    {
        return Verdict::Known("压缩过的 FB2（.fb2.zip）");
    }
    // CBZ：剩下的条目**全部**是图片，而且至少有一张。判据用的是
    // `cbz::is_page_image` 那一份，不再写第二份。
    if !entries.is_empty() && entries.iter().all(|name| is_page_image(name)) {
        return Verdict::Readable(Format::Cbz);
    }
    // 是个 zip，但不是上面三种：DOCX、JAR、普通压缩包，或者一个坏掉的漫画包。
    Verdict::NotABook
}

/// 裸文件那一支：扩展名优先，认不出再看壳。
fn bare_file(display_name: &str, head: &[u8]) -> Verdict {
    // 文本类的第一道门是「它是不是文本」：头部里有 NUL 就不是。
    if head.contains(&0) {
        return Verdict::NotABook;
    }
    match extension_of(display_name).as_deref() {
        Some("md") | Some("markdown") => return Verdict::Readable(Format::Markdown),
        Some("txt") => return Verdict::Readable(Format::Text),
        Some("html") | Some("htm") | Some("xhtml") => return Verdict::Readable(Format::Html),
        Some("fb2") => return Verdict::Readable(Format::Fb2),
        // **有扩展名但不在这一列里**（`.mobi`、`.docx`，或一个穿着 `.epub` 的裸文件）：
        // 拒掉，不猜。猜成「大概是文本」会把一本读不动的 MOBI 变成架上的一本坏书，
        // 而那本来是一次说得清的拒绝（第五节：判定失败的候选不能在库里留下记录）。
        Some(_) => return Verdict::NotABook,
        // 没有扩展名（下载来的东西常常这样）：只能看壳。
        None => {}
    }
    let Some(prefix) = utf8_prefix(head) else {
        // 头部就不是合法 UTF-8，而后面每一层都只会 `read_to_string` 一遍。
        return Verdict::NotABook;
    };
    if prefix.contains("<FictionBook") {
        return Verdict::Readable(Format::Fb2);
    }
    if prefix.contains("<html") || prefix.contains("<!DOCTYPE html") || prefix.contains("<?xml") {
        return Verdict::Readable(Format::Html);
    }
    // 剩下的当纯文本收。这一条是有意的宽容：`书.txt` 与 `无扩展名的下载` 本来就没有
    // 魔数可看（第五节），把它们挡在门外只会让读者不懂为什么进不去。
    Verdict::Readable(Format::Text)
}

/// 建议名的小写扩展名（不带点）。没有就 `None`——判定不猜名字。
fn extension_of(name: &str) -> Option<String> {
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let (_, ext) = file.rsplit_once('.')?;
    (!ext.is_empty() && !ext.chars().any(char::is_whitespace)).then(|| ext.to_ascii_lowercase())
}

/// 头部这一段是不是「按 UTF-8 读到边界了」。
///
/// 缓冲区末尾被切断的多字节字符不算坏（那只是没读完），所以只在断点离末尾
/// 三个字节以上时才判它真是非法字节——一个字符最长四字节。
fn utf8_prefix(head: &[u8]) -> Option<&str> {
    match std::str::from_utf8(head) {
        Ok(text) => Some(text),
        Err(err) => {
            let upstream = err.valid_up_to();
            (head.len() - upstream <= 3).then(|| std::str::from_utf8(&head[..upstream]).ok())?
        }
    }
}

/// 读头部。读不到（权限、空目录、竞态）返回 `None`，由调用方当坏文件处理。
fn read_head(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(HEAD);
    file.take(HEAD as u64)
        .read_to_end(&mut buf)
        .ok()
        .map(|_| buf)
}

#[cfg(test)]
mod tests {
    //! 判定的断言全部围绕一个事实：**每一层的判据必须真的在起作用**。
    //! 所以同一种容器要配两种名字（对的名字与错的扩展名）各测一次——
    //! 只有两种结果都指向容器本身那一档，才算扩展名没有参与裁决。

    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use super::{Verdict, extension_of, identify, utf8_prefix};
    use crate::format::Format;

    /// 一个临时目录。`tag` 只是让读的人知道这一格在测哪一层，不参与路径。
    fn dir(tag: &str) -> tempfile::TempDir {
        let _ = tag;
        tempfile::tempdir().expect("临时目录")
    }

    /// 合成一个 zip。`stored` 决定第一个条目是不是「存而不压缩」——EPUB 的规范
    /// 要求它是，而这一版判定不拿这条当判据，所以两种都要能过。
    fn zip_at(dir: &Path, name: &str, entries: &[(&str, &[u8], bool)]) -> PathBuf {
        let path = dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (entry, data, stored) in entries {
            let options = zip::write::SimpleFileOptions::default().compression_method(if *stored {
                zip::CompressionMethod::Stored
            } else {
                zip::CompressionMethod::Deflated
            });
            zip.start_file(*entry, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    fn file_at(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    const EPUB_ENTRIES: &[(&str, &[u8], bool)] = &[
        ("mimetype", b"application/epub+zip", true),
        ("META-INF/container.xml", b"<container/>", true),
    ];

    /// 第一层就定的两种：PDF 与 PalmDB。名字都不许影响结果。
    #[test]
    fn pdf_and_palmdb_land_by_magic_not_by_name() {
        let tmp = dir("magic");
        // PalmDB 的签名在偏移 60，前面填够 60 个字节。
        let mut mobi = vec![0u8; 60];
        mobi.extend_from_slice(b"BOOKMOBI");
        mobi.extend_from_slice(&[0u8; 24]);
        for (name, body, want) in [
            ("book.pdf", b"%PDF-1.7\n".to_vec(), Verdict::Known("PDF")),
            ("book.other", mobi.clone(), Verdict::Known("MOBI / AZW3")),
        ] {
            let path = file_at(tmp.path(), name, &body);
            assert_eq!(identify(name, &path), want, "{name} 该由魔数定");
        }
    }

    /// EPUB 的正解：第一个条目叫 `mimetype` 且内容是那个串。**换了文件名也认**，
    /// 而一个叫 `.epub` 的纯文本不能蒙过去。
    #[test]
    fn epub_is_decided_by_the_mimetype_entry_not_the_extension() {
        let tmp = dir("epub");
        let stored = zip_at(tmp.path(), "a.zip", EPUB_ENTRIES);
        assert_eq!(
            identify("a.zip", &stored),
            Verdict::Readable(Format::Epub),
            "叫 .zip 的真 EPUB 要认出来"
        );
        let deflated = zip_at(
            tmp.path(),
            "b.epub",
            &[("mimetype", b"application/epub+zip", false)],
        );
        assert_eq!(
            identify("b.epub", &deflated),
            Verdict::Readable(Format::Epub),
            "压缩了的 mimetype 仍然算 EPUB：判定走中央目录，不靠偏移 38"
        );
        let fake = file_at(
            tmp.path(),
            "c.epub",
            "<html><body>不是容器</body></html>".as_bytes(),
        );
        assert_eq!(
            identify("c.epub", &fake),
            Verdict::NotABook,
            "穿着容器扩展名的裸文件不当书收：容器没了就是坏了，替它猜一个格式只会多一本坏书"
        );
    }

    /// `mimetype` 不是第一个条目，或内容不对，都不是 EPUB。
    #[test]
    fn a_zip_that_only_looks_like_epub_is_not_one() {
        let tmp = dir("not-epub");
        let wrong_head = zip_at(
            tmp.path(),
            "a.epub",
            &[
                ("extra.txt", b"noise", true),
                ("mimetype", b"application/epub+zip", true),
            ],
        );
        assert_eq!(identify("a.epub", &wrong_head), Verdict::NotABook);
        let wrong_body = zip_at(
            tmp.path(),
            "b.epub",
            &[("mimetype", b"application/zip", true)],
        );
        assert_eq!(identify("b.epub", &wrong_body), Verdict::NotABook);
    }

    /// CBZ 的判据是「条目全是图」。混进正文文件的包不算，噪音目录算不到头上。
    #[test]
    fn cbz_needs_every_entry_to_be_a_page_image() {
        let tmp = dir("cbz");
        let pure = zip_at(
            tmp.path(),
            "a.zip",
            &[
                ("001.jpg", b"\xff\xd8\xff", true),
                ("002.png", b"\x89PNG", true),
                ("__MACOSX/._001.jpg", b"noise", true),
                ("cover/", b"", true),
            ],
        );
        assert_eq!(
            identify("a.zip", &pure),
            Verdict::Readable(Format::Cbz),
            "全是图的 zip 就是 CBZ"
        );
        let mixed = zip_at(
            tmp.path(),
            "b.cbz",
            &[
                ("001.jpg", b"\xff\xd8\xff", true),
                ("story.txt", "正文".as_bytes(), true),
            ],
        );
        assert_eq!(
            identify("b.cbz", &mixed),
            Verdict::NotABook,
            "混了正文的包不是漫画包"
        );
    }

    /// 一个 `.fb2` 条目说得出是压缩过的 FB2，但这一版读不动它——要如实报成
    /// 「认出来了」，不许报成「这不是书」。
    #[test]
    fn zipped_fb2_is_recognized_but_not_readable_yet() {
        let tmp = dir("fb2z");
        let path = zip_at(
            tmp.path(),
            "book.zip",
            &[("book.fb2", b"<?xml version=\"1.0\"?><FictionBook/>", true)],
        );
        assert_eq!(
            identify("book.zip", &path),
            Verdict::Known("压缩过的 FB2（.fb2.zip）")
        );
    }

    /// 头四字节是 ZIP 而中央目录坏了：这一档必须与「不是书」分开。
    #[test]
    fn a_truncated_zip_reports_a_corrupt_container_not_a_non_book() {
        let tmp = dir("corrupt");
        // 合法的本地文件头 + 没有中央目录 = `ZipArchive::new` 读不出来。
        let path = file_at(
            tmp.path(),
            "broken.zip",
            b"PK\x03\x04\x14\x00\x00\x00\x00\x00abc",
        );
        assert_eq!(identify("broken.zip", &path), Verdict::CorruptContainer);
    }

    /// 裸文件按扩展名定，TXT 与 Markdown 分不出来是**格式本身没有魔数**，不是实现偷懒。
    #[test]
    fn bare_files_land_by_extension() {
        let tmp = dir("bare");
        for (name, body, want) in [
            ("a.md", "# 一章\n\n正文\n", Format::Markdown),
            ("a.txt", "正文\n", Format::Text),
            ("a.html", "<html><body>x</body></html>", Format::Html),
            ("a.xhtml", "<?xml version=\"1.0\"?><html/>", Format::Html),
            (
                "a.fb2",
                "<?xml version=\"1.0\"?><FictionBook/>",
                Format::Fb2,
            ),
        ] {
            let path = file_at(tmp.path(), name, body.as_bytes());
            assert_eq!(identify(name, &path), Verdict::Readable(want), "{name}");
        }
    }

    /// 没有扩展名的下载：看壳。有 `<FictionBook` 算 FB2，有 `<html` 算 HTML，
    /// 其余按纯文本收（第五节：这两类本来就没有魔数，挡在门外只会让读者不懂）。
    #[test]
    fn an_extensionless_download_is_read_from_its_shell() {
        let tmp = dir("noext");
        let cases = [
            (
                "dl1",
                "<FictionBook xmlns=\"http://www.gribuser.ru/xml/fictionbook/2.0\"/>",
                Format::Fb2,
            ),
            (
                "dl2",
                "<!DOCTYPE html><html><body>x</body></html>",
                Format::Html,
            ),
            ("dl3", "第一本\n第二行\n", Format::Text),
        ];
        for (name, body, want) in cases {
            let path = file_at(tmp.path(), name, body.as_bytes());
            assert_eq!(identify(name, &path), Verdict::Readable(want), "{name}");
        }
    }

    /// 二进制的判据是头部有 NUL。UTF-16 的两个 BOM 单独一档：那是「读不动」，
    /// 不是「不是书」。
    #[test]
    fn binary_and_utf16_are_told_apart() {
        let tmp = dir("binary");
        let nul = file_at(tmp.path(), "a.bin", b"\x00\x01\x02\x03");
        assert_eq!(identify("a.bin", &nul), Verdict::NotABook);
        let utf16 = file_at(tmp.path(), "a.txt", b"\xff\xfe\x00\x4f\x00k");
        assert_eq!(identify("a.txt", &utf16), Verdict::Known("UTF-16 文本"));
    }

    /// 扩展名的取法：不带点、转小写、路径成分先剥掉；扩展名**自己**含空格的不算
    /// （`v1.2 release` 那种版本号不是扩展名），名字里有空格的照算。
    #[test]
    fn the_extension_is_taken_from_the_last_path_segment() {
        assert_eq!(extension_of("三体.EPUB").as_deref(), Some("epub"));
        assert_eq!(extension_of("/a/b/x.txt").as_deref(), Some("txt"));
        assert_eq!(extension_of("no_ext").as_deref(), None);
        assert_eq!(extension_of("a.tar.gz").as_deref(), Some("gz"));
        assert_eq!(extension_of("name .txt").as_deref(), Some("txt"));
        assert_eq!(
            extension_of("v1.2 release").as_deref(),
            None,
            "扩展名里有空格"
        );
    }

    /// 被缓冲区切掉的多字节字符不算坏字节；真坏字节才算。
    #[test]
    fn a_cut_utf8_char_at_the_end_is_not_a_bad_file() {
        let bytes = "三".as_bytes();
        assert_eq!(bytes.len(), 3);
        // 前 1 个字节只是一个字符的开头：断点在末尾两个字节之内，仍然「读到边界了」。
        assert_eq!(utf8_prefix(&bytes[..1]), Some(""));
        assert_eq!(utf8_prefix(&bytes[..2]), Some(""));
        assert_eq!(utf8_prefix(bytes), Some("三"));
        // 非法字节出现在中间就不是切不断的问题。
        assert_eq!(utf8_prefix(&[b'a', 0xff, b'b', b'c', b'd']), None);
    }
}
