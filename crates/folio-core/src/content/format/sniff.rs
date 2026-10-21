//! 格式判定：三层，**在落地之前**把不是书的东西挡掉。
//!
//! 只看魔数会漏，只看扩展名会错（`plans/导入管线.md` 第五节）：EPUB 和 CBZ 都是 ZIP，
//! 头四字节都是 `PK\x03\x04`，与 DOCX、JAR、普通压缩包无法区分；TXT 与 Markdown
//! 根本没有魔数；FB2 是 XML。所以判据分三层，每一层只负责自己判得动的那一档：
//!
//! ```text
//! 扩展名   粗筛（批量导入时排掉杂物，省掉无谓的哈希）—— 不当最终裁决
//! 魔数     PDF、PalmDB（偏移 60 处 `BOOKMOBI`）在这里就定了；ZIP 与 ITSF 只定到
//!          「是个容器」
//! 容器     打开来读条目：EPUB 认第一个条目，CBZ 认「条目全是图」，
//!          否则认「里面躺着哪几条书」—— 那一个包是**几本书**，逐条各走一遍管线
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

use libchm::{ChmFile, EntrySel};

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
    /// 认得出是什么，但这一条路交不出一个能直接读的格式。带上的是给人看的那一句。
    ///
    /// **现在只剩一条**：「压缩包里装着书」，而那一条不是空缺而是兜底 —— 正常路径上管线
    /// 已经逐条抽出来了，走到那一格说明抽取本身挂了。四条原先在这档的都已改判成
    /// [`Verdict::Readable`]：MOBI / AZW3（有了 provider）、UTF-16 文本（裸文件的读入口
    /// 从 `read_to_string` 换成 `format::charset` 那两条读法，那两个 BOM 现在解得开）、
    /// PDF（`format::pdf`：栅格化引擎接上了，一页画多大见那条 `RENDER_EDGE`），
    /// 以及上面那条压缩包。
    ///
    /// 留着这一档而不是把它并进 [`Verdict::NotABook`]：「这种书还不支持」与「这不是书」
    /// 对读者是两个动作（等我们支持 / 换一个文件），并成一句就等于什么都没说。
    Known(&'static str),
    /// 三层都走完了：不是能当书收的东西。
    NotABook,
    /// 壳说得出是个容器（ZIP 的头四字节、或 ITSF 那四个字节），但目录读不出来
    /// （截断、坏包）。
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
    /// 就等于什么都没说。类别用 [`crate::infra::error::ImportReject`] 而不是文案 ——
    /// 报表要按类别分组，而文案会改。
    ///
    /// 书名不在这里拼：判定层不知道「哪一本」这件事归谁说，调用方拿着候选名自己套
    /// （见 `crate::ingest::import`）。
    pub(crate) fn rejection(self) -> Option<(crate::infra::error::ImportReject, String)> {
        use crate::infra::error::ImportReject;
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
    // RAR 与 7z 的壳只能说「这是个压缩包」：是不是「条目全是图」由 provider 去确认。
    // 两者都不许落到「这不是书」——那对读者是另一句话。
    if head.starts_with(b"Rar!\x1A\x07") {
        return Verdict::Readable(Format::Cbr);
    }
    if head.starts_with(b"7z\xBC\xAF\x27\x1C") {
        return Verdict::Readable(Format::Cb7);
    }
    if head.starts_with(b"AT&TFORM") {
        return Verdict::Readable(Format::Djvu);
    }
    if is_tar(&head) {
        return Verdict::Readable(Format::Cbt);
    }
    // PalmDB 的数据库类型在偏移 60：`TEXt`/`REAd` 是 PalmDoc 文本，`BOOKMOBI` 是
    // MOBI 家族。后者里还藏着 AZW4（PalmDB 内嵌 PDF），要开记录看。
    if is_palmdb(&head) {
        return palm(path, &head);
    }
    // CHM 与 ZIP 同一档：`ITSF` 那四个字节只说得出「这是个 ITS 容器」，说不出它是不是
    // 一本读得动的书，所以要开进去看（下面 `chm` 那一层）。
    if head.starts_with(b"ITSF") {
        return chm(path);
    }
    if head.starts_with(b"PK\x03\x04") {
        return container(path);
    }
    bare_file(display_name, &head)
}

/// POSIX/GNU tar 的 `ustar` 魔数在偏移 257；不用扩展名当判据。
fn is_tar(head: &[u8]) -> bool {
    head.len() >= 262 && &head[257..262] == b"ustar"
}

/// PalmDB 的三类书：MOBI 家族（`BOOKMOBI`）、PalmDoc 文本（`TEXt` 或 `BOOK`+`REAd`）。
fn is_palmdb(head: &[u8]) -> bool {
    // `BOOK` 是电子书大类（MOBI / AZW4 都在里面，creator 各不相同），`TEXt` 是 PalmDoc。
    head.len() >= 68 && (head[60..64] == *b"BOOK" || head[60..64] == *b"TEXt")
}

/// 打开 PalmDB 判细类。MOBI 家族里先看记录里有没有 PDF：AZW4 就是「PalmDB 装 PDF」，
/// 不是另一种 PDF 引擎。
fn palm(path: &Path, head: &[u8]) -> Verdict {
    use super::palmdb::PalmDb;
    let db = match PalmDb::open(path) {
        Ok(db) => db,
        // 类型字段像书、记录表却读不出来：坏书，不是「不是书」。
        Err(_) => return Verdict::CorruptContainer,
    };
    if head[60..64] == *b"BOOK" && super::azw4::pdf_record(&db).is_some() {
        return Verdict::Readable(Format::Azw4);
    }
    if head[60..68] == *b"BOOKMOBI" {
        return Verdict::Readable(Format::Mobi);
    }
    if super::pdb::is_palmdoc(&db) {
        return Verdict::Readable(Format::Pdb);
    }
    Verdict::NotABook
}

/// 第三层（ITSF 那一支）：**真把容器开一次**。
///
/// 判据用 [`ChmFile::open`] 的成败，而不是自己再解析一遍 ITSF 头 —— 同一件事写两份
/// 迟早会漂，而漂的症狀正是「判定收下了、provider 打不开」这种说错的话。开一次实测
/// 55 µs，比后面那趟 SHA-256 便宜三个数量级。
///
/// 开得出还要**目录里有可读的条目**：只装了索引与图的 ITS 容器不是书，
/// 与 CBZ 那一条「至少有一页」同立场。
fn chm(path: &Path) -> Verdict {
    let mut file = match ChmFile::open(path) {
        Ok(file) => file,
        // 头是 ITSF 而容器开不动：截断或坏了，与「这不是书」分开。
        Err(_) => return Verdict::CorruptContainer,
    };
    match file.entries(EntrySel::NORMAL | EntrySel::FILES) {
        Ok(entries) if !entries.is_empty() => Verdict::Readable(Format::Chm),
        Ok(_) => Verdict::NotABook,
        Err(_) => Verdict::CorruptContainer,
    }
}

/// 第二层：头部字节。只有这里能一锤定音的才在这一层返回。
fn magic(head: &[u8]) -> Option<Verdict> {
    if head.starts_with(b"%PDF") {
        return Some(Verdict::Readable(Format::Pdf));
    }
    // RTF 的魔数必须在**这一层**，不能在裸文件那一层：合法的 RTF 里可以有 NUL
    // （`\binN` 后面就是裸字节，WMF 插图与嵌入字体都这么写），而裸文件那一支的第一道门
    // 是「头部有 NUL 就不是书」。真书里 `\bin` 常常就落在开头几百字节内，那道门会把
    // 一整本带插图的 RTF 挡在门外，而拒绝的说法是「这不是书」——完全说不清。
    if starts_with_rtf(head) {
        return Some(Verdict::Readable(Format::Rtf));
    }
    // PalmDB 的数据库类型在偏移 60..68，但「MOBI 还是 AZW4」要开记录才知道，
    // 所以那一档挪到 [`identify`] 的 `palm` 那一支。
    // UTF-16 的两个 BOM **不在这一层判**，挪到 [`bare_file`] 那一支去了。理由是那一层
    // 一锤定音不了：`FF FE` 只说得出「这是文本、且不是 UTF-8」，说不出它是 `.md` 还是
    // `.html`，而在这里写死成 `Text` 会把一本 `书.md` 的分章规则整个丢掉。
    // 它也不再是「认得出来但读不动」：裸文件的读入口已经换成 `format::charset` 那两条
    // 那两个 BOM 解得开（`format::charset` 里那条 UTF-16 分支早就跑在 CHM 上）。
    None
}

/// 头部是不是 RTF 文档。
///
/// 两种写法都认：正解是 `{\rtf` 开头，规范还允许第一行是 `%RTF…` 注释行
/// （真见过，来自某些版本的 WordPerfect）。前面留不留空白也认 —— 有生产者会在
/// 文件最前面写一个空行，而那一形态在 Windows 的记事本里改过一次就出来了。
///
/// 只嗅头部这一小段，不做「整本里含有 `{\rtf` 就算」—— 一个讨论 RTF 的 `.txt`
/// 会含这几个字符，而它是一篇文档不是一本书。
///
/// 反方向也一样成立：**给 RTF 加一条扩展名判据是多余的**。规范写着 RTF 文件必须以
/// `{\rtf` 开头，所以这一层就是它的全部判据；一个叫 `书.rtf` 却没有这个开头的文件
/// 确实不是 RTF，它落到裸文件那一支的「有扩展名但不认」被拒掉 —— 那正是一次说得清的拒绝。
fn starts_with_rtf(head: &[u8]) -> bool {
    // 注释行 + `{\rtf` 最长也就几十字节，读这么多足够定夺。
    let text = String::from_utf8_lossy(&head[..head.len().min(120)]);
    let text = text.trim_start();
    if text.starts_with(r"{\rtf") {
        return true;
    }
    // `%RTF1` 那一行之后才是真正的文档。
    text.starts_with('%')
        .then(|| {
            text.split_once('\n')
                .map_or("", |(_, rest)| rest.trim_start())
        })
        .is_some_and(|rest| rest.starts_with(r"{\rtf"))
}

/// 一个包里躺着的那一本书。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Inner {
    /// **容器里的原始条目名**：取字节要的就是这一串（归一化名回不去反斜杠那一族）。
    pub(crate) entry: String,
    /// 它的文件名。判定与书名要用的是这一个扩展名线索 —— 外层那一个 `.zip`
    /// 在退开之后已经说不清任何事了。
    pub(crate) name: String,
}

/// 一个 ZIP 在「该不该退那一层」这一问上的样子。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ZipShape {
    /// 头四字节不是 `PK\x03\x04`：谈不上容器。
    NotAZip,
    /// 是 ZIP 而中央目录读不出来（截断、坏包）。
    Corrupt,
    /// 容器自己**就是**一本书（EPUB：第一个条目是 `mimetype`；CBZ：条目全是图）。
    /// **不许退**：把 EPUB 的条目一个个抽出来，那本书就散了。
    Book(Format),
    /// 里面装着书，一条或多条。多条就是合集包 —— 逐条各算一本书。
    Holds(Vec<Inner>),
    /// 一个包，但里面没有我们认得的书：DOCX、JAR、普通压缩包、坏掉的漫画包。
    Nothing,
}

/// 认一个 ZIP。**判定层、导入管线退那一层、扫描粗筛三处共用这一份答案**：
/// 各数一遍条目的结果迟早会在同一个包上分叉（同一本书一会儿算候选、一会儿算「不是书」，
/// 而两边都看起来是对的）。
pub(crate) fn zip_shape(path: &Path) -> ZipShape {
    let head = match read_head(path) {
        Some(head) => head,
        // 连头都读不到：谈不上「是个包」，也谈不上不是 —— 按坏文件说。
        None => return ZipShape::Corrupt,
    };
    if !head.starts_with(b"PK\x03\x04") {
        return ZipShape::NotAZip;
    }
    let mut zip = match Zip::open(path) {
        Ok(zip) => zip,
        Err(_) => return ZipShape::Corrupt,
    };
    // EPUB 的正解：第一个条目必须是 `mimetype`，内容是 `application/epub+zip`。
    // 规范还要求它「存而不压缩」（所以硬读偏移 38 那 20 个字节就能判）；这里走的是
    // 中央目录，压缩方法只剩优化意义，**不当判据**——真书的打包工具并不都守这一条。
    let mimetype = zip.head_entry().and_then(|(name, _stored)| {
        (name == "mimetype").then(|| {
            std::str::from_utf8(&zip.read(&name).unwrap_or_default())
                .map(str::trim)
                .unwrap_or_default()
                .to_string()
        })
    });
    if mimetype.as_deref() == Some("application/epub+zip") {
        return ZipShape::Book(Format::Epub);
    }
    let names = zip.names();
    let has = |want: &str| names.iter().any(|name| name.eq_ignore_ascii_case(want));
    // ODT 的正解与 EPUB 同一条：第一个条目 `mimetype` 说自己是 OpenDocument。
    // 兜底认 `content.xml` + `META-INF/manifest.xml` —— 有些转换器不守「mimetype 第一条」。
    if mimetype.as_deref() == Some("application/vnd.oasis.opendocument.text")
        || (has("content.xml") && has("META-INF/manifest.xml"))
    {
        return ZipShape::Book(Format::Odt);
    }
    // DOCX 没有 mimetype 条目：`[Content_Types].xml` + `word/document.xml` 就是它的形状。
    if has("[Content_Types].xml") && has("word/document.xml") {
        return ZipShape::Book(Format::Docx);
    }
    // 噪音条目（目录、`.DS_Store`、`__MACOSX/`）不参与判断——它们既不是页，
    // 也不该让一个正常包被评成「不是漫画包」。
    let entries: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| !is_container_noise(name))
        .collect();
    // CBZ：剩下的条目**全部**是图片，而且至少有一张。判据用的是
    // `cbz::is_page_image` 那一份，不再写第二份。
    if !entries.is_empty() && entries.iter().all(|name| is_page_image(name)) {
        return ZipShape::Book(Format::Cbz);
    }
    // HTMLZ：一个装着网页书的包。根下有 `index.html` 就是它；没有索引页时，
    // 「至少一份 HTML + 至少一张图」也按它收 —— 这样的包退层只带走 HTML、会把图丢掉。
    let html_count = entries.iter().filter(|name| is_html_entry(name)).count();
    let root_index = entries.iter().any(|name| {
        name.eq_ignore_ascii_case("index.html") || name.eq_ignore_ascii_case("index.htm")
    });
    let has_image = entries.iter().any(|name| is_page_image(name));
    if html_count > 0 && (root_index || has_image) {
        return ZipShape::Book(Format::Htmlz);
    }
    // 剩下的那一族：把**每一条像书的条目**都交出去，一条算一本书。
    //
    // 「像书」只看扩展名有没有 provider 认领，所以图片不算 —— 一个 `书.fb2.zip`
    // 里那十几张图是**正文的外链资源**，退一层只带走正文那一份，图跟着丢
    // （管线那一格记一条 warn）。反过来，一个「几十张图 + 一个 `info.txt`」的包
    // 会得出一本 `info.txt` 那样的小书：这是「逐条各判一次」这条规则的正面代价，
    // 而它出现在扫描的候选清单上时读者一眼看得见。
    let mut held: Vec<Inner> = Vec::new();
    for name in entries {
        let Some(ext) = extension_of(name) else {
            continue;
        };
        if !super::supported_extensions()
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&ext))
        {
            continue;
        }
        let Some(entry) = zip.locate(name) else {
            continue;
        };
        held.push(Inner {
            entry,
            name: file_name(name).to_string(),
        });
    }
    if held.is_empty() {
        return ZipShape::Nothing;
    }
    ZipShape::Holds(held)
}

/// 条目名里的文件部分（容器里的路径分隔符两种都认）。
fn file_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// 这一条是不是 HTML 文档（HTMLZ 的章节单位）。
fn is_html_entry(name: &str) -> bool {
    matches!(
        extension_of(name).as_deref(),
        Some("html" | "htm" | "xhtml")
    )
}

/// 第三层：ZIP 家族。判据全在 [`zip_shape`]，这一格只把那份答案换成判定面那四种说法。
fn container(path: &Path) -> Verdict {
    match zip_shape(path) {
        ZipShape::Book(format) => Verdict::Readable(format),
        // 走到这一格意味着管线没能替它退开（抽那一条时读挂了，或有人直接拿一个
        // 没解开的包来问判定）。**不许报成「这不是书」**：里面确实有一本书。
        ZipShape::Holds(_) => Verdict::Known("压缩包里装着书（要先逐条抽出来）"),
        ZipShape::Corrupt => Verdict::CorruptContainer,
        // 头四字节不是 ZIP 却走到了这一层 = 调用方那道 `PK` 检查与这里读的不是同一份
        // 字节（文件在两次读之间被换掉或删掉了）。那一档与「坏包」是同一句话。
        ZipShape::NotAZip => Verdict::CorruptContainer,
        ZipShape::Nothing => Verdict::NotABook,
    }
}

/// 裸文件那一支：扩展名优先，认不出再看壳。
fn bare_file(display_name: &str, head: &[u8]) -> Verdict {
    // UTF-16 的两个 BOM 必须排在「有 NUL 就不是文本」那道门**前面**：那种正文每两个
    // 字节就落一个 `00`，那道门会把每一本 UTF-16 的书都挡在外面，而给出的说法是
    // 「这不是书」。同一条位置上的道理在这个文件里写过三次 —— RTF 的 `\bin` 排在
    // `magic()` 那一层，CHM 的容器排在 ZIP 那一层，这里是第三次。
    let utf16 = head.starts_with(b"\xff\xfe") || head.starts_with(b"\xfe\xff");
    if !utf16 && head.contains(&0) {
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
    if utf16 {
        // 没扩展名而有 BOM：BOM 就是它自己说了「我是文本」，按纯文本收 —— 与下面
        // 「剩下的当纯文本收」同一立场，只是这一条不必再看字节的形状。
        return Verdict::Readable(Format::Text);
    }
    let Some(prefix) = utf8_prefix(head) else {
        // 头部既不是合法 UTF-8，又没有 BOM，也没有扩展名替它说话：这时候连「它是文本」
        // 都不确定。**不猜码页** —— 实测过分不开：一段简体 GBK 的字节流拿 Big5 去解同样
        // 一个坏字节都没有，得到的却是一串夹着注音符号的怪汉字，屏幕上查不出错
        // （判据与理由写在 `format::charset` 的模块头）。有扩展名的那一族由扩展名承担
        // 「这是文本」这句话，所以轮得到 provider 去解；这一条两样都没有。
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

    use super::{Verdict, ZipShape, extension_of, identify, utf8_prefix, zip_shape};
    use crate::content::format::Format;

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

    /// 摆一个最小 PalmDB：头 78 字节 + 记录表 + 记录内容。
    fn palmdb_at(dir: &Path, name: &str, db_name: &str, records: &[&[u8]]) -> PathBuf {
        const HEADER: usize = 78;
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        let mut head = vec![0u8; HEADER];
        head[..db_name.len().min(32)].copy_from_slice(&db_name.as_bytes()[..db_name.len().min(32)]);
        head[60..64].copy_from_slice(b"BOOK");
        head[64..68].copy_from_slice(b"MOBI");
        head[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
        file.write_all(&head).unwrap();
        let mut start = HEADER + records.len() * 8;
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

    /// PalmDoc 的 PalmDB：类型 `TEXt`、creator `REAd`。
    fn palmdoc_at(dir: &Path, name: &str, records: &[&[u8]]) -> PathBuf {
        const HEADER: usize = 78;
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        let mut head = vec![0u8; HEADER];
        head[60..64].copy_from_slice(b"TEXt");
        head[64..68].copy_from_slice(b"REAd");
        head[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
        file.write_all(&head).unwrap();
        let mut start = HEADER + records.len() * 8;
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

    const EPUB_ENTRIES: &[(&str, &[u8], bool)] = &[
        ("mimetype", b"application/epub+zip", true),
        ("META-INF/container.xml", b"<container/>", true),
    ];

    /// 一条 FB2 正文的字节。写成常量是因为 `zip_at` 那一串里塞转义引号，
    /// 三个用例各写一遍会读不动。
    const FB2_ENTRY: &str = "<FictionBook><body><p>x</p></body></FictionBook>";

    /// 「这个包列出了哪几本」按名字读出来，比逐条 `match` 好看。
    /// 不是 `Holds` 就直接红 —— 这几条门要问的都是「列出了什么」，形状不对没什么可比的。
    fn names_of(shape: &ZipShape) -> Vec<&str> {
        match shape {
            ZipShape::Holds(inner) => inner.iter().map(|b| b.name.as_str()).collect(),
            other => panic!("该是 Holds，实际 {other:?}"),
        }
    }

    /// 第一层就定的两种：PDF 与 PalmDB。名字都不许影响结果。
    #[test]
    fn pdf_and_palmdb_land_by_magic_not_by_name() {
        let tmp = dir("magic");
        let pdf = file_at(tmp.path(), "book.pdf", b"%PDF-1.7\n");
        assert_eq!(
            identify("book.pdf", &pdf),
            Verdict::Readable(Format::Pdf),
            "PDF 由魔数定"
        );
        // PalmDB 的记录表必须真的开得出来：判定层现在要打开记录看它是不是 AZW4。
        let mobi = palmdb_at(tmp.path(), "book.other", "书", &[b"MOBI-HEADER"]);
        assert_eq!(
            identify("book.other", &mobi),
            Verdict::Readable(Format::Mobi),
            "PalmDB 里没有 PDF 记录就是 MOBI"
        );
        let azw4 = palmdb_at(
            tmp.path(),
            "print.other",
            "印刷版",
            &[b"MOBI-HEADER", b"%PDF-1.7\n"],
        );
        assert_eq!(
            identify("print.other", &azw4),
            Verdict::Readable(Format::Azw4),
            "PalmDB 里有 PDF 记录就是 AZW4"
        );
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
        // `mimetype` 排在第二位就不是 EPUB（规范定死它是第一个条目）。而包里那一条
        // `extra.txt` 按「逐条各判一次」算一本书 —— 这一格从 `NotABook` 改成了
        // 「认得出来，要先抽」，因为「不是 EPUB」与「里面没书」是两句话。
        assert_eq!(
            identify("a.epub", &wrong_head),
            Verdict::Known("压缩包里装着书（要先逐条抽出来）")
        );
        assert_eq!(names_of(&zip_shape(&wrong_head)), vec!["extra.txt"]);
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
        // 混进正文的包不是漫画包 —— 而按「逐条各判一次」这一族的新规矩，那一条
        // `story.txt` 是一本书，只是还压在包里没解开。所以这一格从 `NotABook` 改成了
        // 「认得出来，要先抽」：读者照样被拒，但话说得对（这不是杂物包）。
        assert_eq!(
            identify("b.cbz", &mixed),
            Verdict::Known("压缩包里装着书（要先逐条抽出来）"),
            "混了正文的包不是漫画包，而正文那一条算一本书"
        );
        assert_eq!(names_of(&zip_shape(&mixed)), vec!["story.txt"]);
    }

    /// 一个装着书的包：**逐条列出来**，每条算一本。`identify` 那一头说「认得出来，
    /// 但要先抽出来」—— 抽出那一步在管线（`ingest::import::import_candidate`），
    /// 走到判定层这一格说明它没能替读者退开那一层。
    #[test]
    fn a_pack_lists_every_book_it_holds() {
        let tmp = dir("pack");
        let one = zip_at(
            tmp.path(),
            "书.fb2.zip",
            &[("书.fb2", FB2_ENTRY.as_bytes(), true)],
        );
        // 外层那个名字不参与：`x.zip` 与 `书.fb2.zip` 给出同一份答案。
        assert_eq!(
            identify("书.fb2.zip", &one),
            Verdict::Known("压缩包里装着书（要先逐条抽出来）")
        );
        assert_eq!(names_of(&zip_shape(&one)), vec!["书.fb2"]);

        let two = zip_at(
            tmp.path(),
            "合集.zip",
            &[
                ("上卷.fb2", FB2_ENTRY.as_bytes(), true),
                ("下卷.fb2", FB2_ENTRY.as_bytes(), true),
            ],
        );
        assert_eq!(
            names_of(&zip_shape(&two)),
            vec!["上卷.fb2", "下卷.fb2"],
            "合集包按容器里的条目序列出 —— 章节序由此来"
        );
    }

    /// 图片不算书：一个 `书.fb2.zip` 里那十几张图是**正文的外链资源**，
    /// 列出来的只有那一条正文（抽出来之后图留在外面那一层，管线那句 info 说了）。
    #[test]
    fn images_in_a_pack_are_resources_not_books() {
        let tmp = dir("pack-img");
        let path = zip_at(
            tmp.path(),
            "book.zip",
            &[
                ("book.fb2", FB2_ENTRY.as_bytes(), true),
                ("images/pic.png", b"\x89PNG", true),
                ("images/two.jpg", b"\xff\xd8\xff", true),
            ],
        );
        assert_eq!(
            names_of(&zip_shape(&path)),
            vec!["book.fb2"],
            "三条目里只认出一条书"
        );
    }

    /// `__MACOSX/._book.fb2` 那种 mac 打包留下的影子**不许当成第二本书**：
    /// 噪音过滤用的是 CBZ 那一份判据，两处共用，不各写一遍。
    #[test]
    fn a_mac_shadow_entry_does_not_become_a_second_book() {
        let tmp = dir("pack-mac");
        let path = zip_at(
            tmp.path(),
            "book.zip",
            &[
                ("book.fb2", FB2_ENTRY.as_bytes(), true),
                ("__MACOSX/._book.fb2", b"junk", true),
                ("book/", b"", true),
            ],
        );
        assert_eq!(
            names_of(&zip_shape(&path)),
            vec!["book.fb2"],
            "影子条目与目录都不算一本"
        );
    }

    /// 一条像书的都没有（DOCX、JAR、普通压缩包）才是 `Nothing`，判不成书也不报坏包。
    #[test]
    fn an_ordinary_archive_holds_no_book() {
        let tmp = dir("pack-nothing");
        let path = zip_at(
            tmp.path(),
            "备份.zip",
            &[
                ("data.json", b"{}", true),
                ("bin/blob", b"\x00\x01", true),
                ("notes/", b"", true),
            ],
        );
        assert_eq!(zip_shape(&path), ZipShape::Nothing);
        assert_eq!(identify("备份.zip", &path), Verdict::NotABook);
    }

    /// EPUB 与 CBZ 自己就是书：**不许退**。把 EPUB 的条目挨个抽出来，那本书就散了。
    #[test]
    fn a_container_that_is_itself_a_book_is_never_split() {
        let tmp = dir("pack-keep");
        let epub = zip_at(tmp.path(), "a.epub", EPUB_ENTRIES);
        assert_eq!(zip_shape(&epub), ZipShape::Book(Format::Epub));
        let cbz = zip_at(
            tmp.path(),
            "b.cbz",
            &[
                ("001.jpg", b"\xff\xd8\xff", true),
                ("002.png", b"\x89PNG", true),
            ],
        );
        assert_eq!(zip_shape(&cbz), ZipShape::Book(Format::Cbz));
    }

    /// 不是 zip 的东西在这一问上就是「谈不上容器」，不占任何一趟容器读。
    #[test]
    fn a_bare_file_is_not_a_pack() {
        let tmp = dir("pack-bare");
        let path = file_at(tmp.path(), "a.fb2", FB2_ENTRY.as_bytes());
        assert_eq!(zip_shape(&path), ZipShape::NotAZip);
    }

    /// 扫描粗筛的**包那一半**：判据是里面有什么，不是外面叫什么。
    /// 这一条守的是最坏的一种漂 —— 一个叫 `x.zip` 的真 EPUB 或真 FB2 包在架面上凭空消失，
    /// 而界面那句「扫到 N 本」听起来像是扫全了。
    #[test]
    fn the_prefilter_opens_a_pack_and_asks_what_is_inside() {
        let tmp = dir("prefilter");
        let pack = zip_at(
            tmp.path(),
            "书.fb2.zip",
            &[("书.fb2", FB2_ENTRY.as_bytes(), true)],
        );
        assert!(
            crate::content::format::looks_like_a_book(&pack),
            "装着书的包要进候选"
        );
        let epub = zip_at(tmp.path(), "a.zip", EPUB_ENTRIES);
        assert!(
            crate::content::format::looks_like_a_book(&epub),
            "叫 .zip 的真 EPUB 也要进候选"
        );
        let junk = zip_at(tmp.path(), "杂物.zip", &[("data.json", b"{}", true)]);
        assert!(
            !crate::content::format::looks_like_a_book(&junk),
            "普通压缩包不进：粗筛省的就是它那一趟哈希"
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

    /// CHM 那一支走的是**容器那一层**：`ITSF` 只说得出「这是个 ITS 容器」，判据是
    /// 「开得出、而且目录里有条目」。名字不许参与裁决。
    ///
    /// 三条都拿真样本当底子（截断那一条尤其重要 —— 它就是「坏书」与「不是书」的分界）：
    /// 判据一改这里就红，不会出现「判定收下了、provider 打不开」那种说错的话。
    /// 样本怎么来的见 `tests/fixtures/oss/chm/README.md`。
    #[test]
    fn a_chm_is_decided_by_its_directory_not_by_the_signature_or_the_name() {
        let tmp = dir("chm");
        let real = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oss/chm/minimal-zh-utf8.chm");
        assert!(real.exists(), "缺样本：{}", real.display());
        for name in ["x.bin", "书.chm", "没扩展名"] {
            assert_eq!(
                identify(name, &real),
                Verdict::Readable(Format::Chm),
                "{name}：目录开得动就是书，名字不参与"
            );
        }

        // 只留头部那 200 个字节：`ITSF` 在，目录指针指向文件外面。
        let head = std::fs::read(&real).unwrap();
        let path = file_at(tmp.path(), "cut.chm", &head[..200]);
        assert_eq!(
            identify("cut.chm", &path),
            Verdict::CorruptContainer,
            "截断的容器要说「这本坏了」，不是「这不是书」"
        );

        // 一个穿着 `.chm` 扩展名的裸文本：容器那一层轮不到它，而裸文件那一支的扩展名
        // 清单里**故意没有** chm —— CHM 按定义就是容器，裸文件不可能是它。
        let fake = file_at(tmp.path(), "fake.chm", "正文而已\n".as_bytes());
        assert_eq!(
            identify("fake.chm", &fake),
            Verdict::NotABook,
            "没有容器的 `.chm` 不收：替它猜一个格式等于多一本坏书"
        );
    }

    /// DOCX：`[Content_Types].xml` + `word/document.xml` 就是形状，名字不参与。
    #[test]
    fn docx_is_decided_by_its_parts_not_the_extension() {
        let tmp = dir("docx");
        let path = zip_at(
            tmp.path(),
            "a.zip",
            &[
                ("[Content_Types].xml", b"<Types/>", true),
                ("word/document.xml", b"<w:document/>", true),
            ],
        );
        assert_eq!(identify("a.zip", &path), Verdict::Readable(Format::Docx));
    }

    /// ODT：`mimetype` 第一条就是它的正解；`content.xml` + manifest 是兜底。
    #[test]
    fn odt_is_decided_by_its_mimetype_or_manifest() {
        let tmp = dir("odt");
        let path = zip_at(
            tmp.path(),
            "a.zip",
            &[
                ("mimetype", b"application/vnd.oasis.opendocument.text", true),
                ("content.xml", b"<office:document-content/>", true),
            ],
        );
        assert_eq!(identify("a.zip", &path), Verdict::Readable(Format::Odt));

        let fallback = zip_at(
            tmp.path(),
            "b.zip",
            &[
                ("content.xml", b"<office:document-content/>", true),
                ("META-INF/manifest.xml", b"<manifest/>", true),
            ],
        );
        assert_eq!(identify("b.zip", &fallback), Verdict::Readable(Format::Odt));
    }

    /// HTMLZ：根下有 `index.html` 就按一本书收，不许退层把图片丢掉。
    #[test]
    fn an_htmlz_is_one_book_not_a_pack() {
        let tmp = dir("htmlz");
        let path = zip_at(
            tmp.path(),
            "书.zip",
            &[
                ("index.html", b"<html><body>x</body></html>", true),
                ("Images/pic.png", b"\x89PNG", true),
            ],
        );
        assert_eq!(identify("书.zip", &path), Verdict::Readable(Format::Htmlz));
    }

    /// PalmDoc：PalmDB 的类型是 `TEXt`，记录 0 有 PalmDoc 头才是书。
    #[test]
    fn palmdoc_is_decided_by_the_palmdoc_header() {
        let tmp = dir("pdb");
        let mut header = vec![0u8; 16];
        header[0..2].copy_from_slice(&1u16.to_be_bytes());
        header[4..8].copy_from_slice(&4u32.to_be_bytes());
        header[8..10].copy_from_slice(&1u16.to_be_bytes());
        header[10..12].copy_from_slice(&4096u16.to_be_bytes());
        let path = palmdoc_at(tmp.path(), "书.pdb", &[&header, b"text"]);
        assert_eq!(identify("书.pdb", &path), Verdict::Readable(Format::Pdb));
    }

    /// CBR：RAR 魔数先收下，条目是不是图由 provider 确认。
    #[test]
    fn a_rar_magic_is_cbr() {
        let tmp = dir("cbr");
        let path = file_at(tmp.path(), "书.cbr", b"Rar!\x1A\x07\x00rest");
        assert_eq!(identify("书.cbr", &path), Verdict::Readable(Format::Cbr));
    }

    /// 7z 魔数：CB7 由 provider 去确认条目是不是图，判定层先收下。
    #[test]
    fn a_7z_magic_is_cb7() {
        let tmp = dir("cb7");
        let path = file_at(tmp.path(), "书.cb7", b"7z\xBC\xAF\x27\x1Crest");
        assert_eq!(identify("书.cb7", &path), Verdict::Readable(Format::Cb7));
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

    /// 「有 NUL 就不是文本」那道门是为**没有 BOM** 的字节流立的。UTF-16 的正文里每两个
    /// 字节就有一个 `00`，所以那两个 BOM 必须排在门前面，而排前面之后扩展名照旧说话：
    /// BOM 只负责放行，不负责宣布这是哪种文本。
    ///
    /// 把 `utf16` 那一判撤掉，这一条三处全红（前两条退成 `NotABook`，第三条退成
    /// `NotABook` 而不是 `Markdown`）—— 门不是空转的。
    #[test]
    fn a_utf16_bom_opens_the_nul_gate_without_choosing_the_family() {
        let tmp = dir("utf16");
        // 「中文」两个字的 UTF-16 码元，后面再跟一串 NUL：非 BOM 的字节流会死在那道门上。
        let units: Vec<u16> = "\u{4e2d}\u{6587}".encode_utf16().collect();
        let mut le = b"\xff\xfe".to_vec();
        le.extend(units.iter().copied().flat_map(u16::to_le_bytes));
        le.extend([0u8, 0, 0, 0]);
        let mut be = b"\xfe\xff".to_vec();
        be.extend(units.iter().copied().flat_map(u16::to_be_bytes));
        be.extend([0u8, 0, 0, 0]);
        for (name, want) in [
            ("书.txt", Verdict::Readable(Format::Text)),
            ("书.md", Verdict::Readable(Format::Markdown)),
            ("书.html", Verdict::Readable(Format::Html)),
        ] {
            assert_eq!(
                identify(name, &file_at(tmp.path(), name, &le)),
                want,
                "{name}"
            );
            assert_eq!(
                identify(name, &file_at(tmp.path(), name, &be)),
                want,
                "{name} BE"
            );
        }
        // 没有扩展名时 BOM 自己就是那句「我是文本」，按纯文本收（与「剩下的当纯文本收」
        // 同一立场），而**不是**因为下面那条 UTF-8 前缀判据放它过去 —— 那种字节流
        // 一定过不了 `utf8_prefix`。
        assert_eq!(
            identify("无扩展名", &file_at(tmp.path(), "无扩展名", &le)),
            Verdict::Readable(Format::Text)
        );
        // 反向：真有 NUL 又没有 BOM 的还是「不是书」；扩展名不认识的一律不放行。
        let nul = file_at(tmp.path(), "a.bin", b"\x00\x01\x02\x03");
        assert_eq!(identify("a.bin", &nul), Verdict::NotABook);
        assert_eq!(
            identify("书.doc", &file_at(tmp.path(), "书.doc", &le)),
            Verdict::NotABook,
            "BOM 不许把一条不认的扩展名救活：那是另一个格式，不是编码问题"
        );
    }

    /// RTF 由**魔数**定，而它必须在 `magic()` 那一层：合法 RTF 里的 `\binN` 后面是
    /// 裸字节（可以含 NUL），而裸文件那一支第一道门就是「头部有 NUL 就不是书」——
    /// 放在下面那一层，一整本带插图的 RTF 会被说成「这不是书」。
    #[test]
    fn rtf_lands_by_magic_even_when_it_contains_a_nul() {
        let tmp = dir("rtf");
        // `\bin4` 后面那四个字节是真的二进制数据，里面有一个 NUL 与一对花括号。
        let mut with_bin = r"{\rtf1\ansi\uc0\pict\wmetafile8\bin4".as_bytes().to_vec();
        with_bin.extend_from_slice(&[0x00, b'}', b'{', 0x00]);
        with_bin.extend_from_slice(r"\par 正文}".as_bytes());
        for name in ["a.rtf", "a.bin", "无扩展名"] {
            let path = file_at(tmp.path(), name, &with_bin);
            assert_eq!(
                identify(name, &path),
                Verdict::Readable(Format::Rtf),
                "{name}：带 NUL 的合法 RTF 不能被判成不是书"
            );
        }
        // 规范允许的第一行 `%RTF` 注释。
        let commented = file_at(
            tmp.path(),
            "b.rtf",
            "%RTF1 说明\r\n{\\rtf1\\ansi 正文}".as_bytes(),
        );
        assert_eq!(
            identify("b.rtf", &commented),
            Verdict::Readable(Format::Rtf)
        );
        // 反过来：一个叫 `书.rtf` 却没有这个开头的文件确实不是 RTF，拒掉而不猜。
        let fake = file_at(tmp.path(), "书.rtf", "其实是一段纯文本\n".as_bytes());
        assert_eq!(identify("书.rtf", &fake), Verdict::NotABook);
        // 而头部含有这几个字符的一篇**讨论文档**不算 RTF：判据只看开头。
        let essay = file_at(
            tmp.path(),
            "c.txt",
            "RTF 文件的头是 {\\rtf1 这个串\n".as_bytes(),
        );
        assert_eq!(identify("c.txt", &essay), Verdict::Readable(Format::Text));
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
