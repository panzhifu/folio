//! 字节 → 文本：一个文件说自己是什么码页，就按它说的解。
//!
//! 存在的理由是**中文 CHM 的正文不是 UTF-8**。HTML Help Workshop 编出来的书把每个
//! topic 按项目码页（简体 GBK、繁体 Big5、日文 Shift-JIS、韩文 EUC-KR）存进容器，
//! 而 `String::from_utf8` 那一族的失败方式是把非法字节换成 U+FFFD —— 于是整本书
//! 变成一排问号，**且不报任何错**。EPUB 与 FB2 按规范必须 UTF-8，所以这一层
//! 以前没有存在的必要；CHM 是第一个把它顶到门上的格式。
//!
//! ## 顺序是写死的，不是试出来的
//!
//! 按 HTML 标准那一条链走：**BOM > 文档自己的声明 > 容器给的项目码页 > 按 UTF-8 收**。
//!
//! 前两级都是「这个文件自己说的话」，第三级是「装箱的人说的话」。谁赢有实际后果：
//! 一份 UTF-8 的文件里写着 `charset=gb2312`（转格式时最常见的残留）按标准就该解成
//! GBK。这一档不改成「先试 UTF-8」：那等于让猜测试图替文档说话，而它替不了。
//!
//! 最后一档之外不再往下猜：**猜一个码页比承认解不动更危险**。一个字节流既不是合法
//! UTF-8、又没有声明、容器也不报码页时，解成 windows-1252 会得到「一句说得清的英文
//! 乱码」，解成 GBK 会得到两个汉字 —— 屏幕上看不出哪一个错了。这时按 UTF-8 收，
//! 让 U+FFFD 出现在它该出现的地方。
//!
//! ## 谁走这一条链
//!
//! 第一个把它顶到门上的是 CHM（容器报 LCID）。第二个是**裸文件那一族**：单文件 HTML
//! 与 FB2 从前各写一遍 `std::fs::read_to_string`，于是它们连「文件自己声明了码页」
//! 这一档都没走到 —— 中文网页另存的 `.html` 满地是 GBK **且带 `<meta charset>`**，
//! 声明本来是够的，以前只是入口不对。四个读文件的口子现在合并成两处：有声明位的
//! 走 [`read_markup`]，没有的走 [`read_plain`]（这一族不能听声明，理由写在那条上）。
//!
//! ## 裸文本那最后一档
//!
//! 既没有 BOM、又没有声明、也不是合法 UTF-8 的小说 `.txt`（简体 GBK / 繁体 Big5 /
//! 日轻 Shift-JIS / 韩文 EUC-KR）问一次探测器。**上面那句「不猜」没有被推翻**，被推翻的是
//! 「字节合法性与坏字节计数能代替猜」这个想法：实测一段简体 GBK 的字节流拿 Big5 解**同样
//! 零坏字节**，得到的却是一串夹着注音符号的怪汉字 —— 两种都合法时计数给不出判别，而
//! 误读的产物在屏幕上查不出错。所以那一档自带三条笼子：合法 UTF-8 永远先收（问都不问）、
//! 答案要过一张写死的白名单、认不出就按 UTF-8 收留看得见的洞。判据与实测数字写在
//! [`read_plain`]、[`guess_codepage`] 与根 `Cargo.toml` 那条依赖注释上。

use std::path::Path;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;

use crate::Result;
use crate::infra::error::IoResultExt as _;

/// 找声明只看文档头这么多字节。标准的伪算法就是只看头 1024 字节内的 `<meta>`，
/// 真书的声明都在 `<head>` 最前面 —— 读全本为一个字符串不值得。
const PRESCAN: usize = 1024;

/// 把一段字节解成文本。
///
/// `project` 是**容器自己报的码页**（CHM 用 `#SYSTEM` 里那个 LCID 换，见
/// [`from_lcid`]），没有就 `None`。文件级的 BOM 与声明排在它前面，都不中时按 UTF-8 收
/// （理由见模块头那四档顺序）。
pub(crate) fn decode(bytes: &[u8], project: Option<&'static Encoding>) -> String {
    // UTF-16 的两个 BOM 先走：encoding_rs 不给那两个编码做字节解码（它的
    // `decode` 只接 ASCII 兼容与单字节那几族），所以那一路不能过 `decode`。
    if let Some(text) = decode_utf16_bom(bytes) {
        return text;
    }
    let encoding = match declared(bytes) {
        Some(declared) => declared,
        // 容器报的码页与字节不符时按 UTF-8 收。这一档不是省事而是止损：转格式转出来的
        // 书里 `#SYSTEM` 说 GBK 而字符串本身是 UTF-8 的确实存在，拿 GBK 去解会得到
        // 「看起来像汉字」的垃圾，那种垃圾在屏幕上查不出错。反方向的误伤几乎不会发生：
        // GBK / Big5 / Shift-JIS 的中文双字节序列极少同时是合法 UTF-8。
        None => match project {
            Some(project) if std::str::from_utf8(bytes).is_err() => project,
            _ => encoding_rs::UTF_8,
        },
    };
    // 声明写着 `charset=utf-16` 而字节里没有 BOM 的真书是存在的（转格式转坏的），
    // 这时按声明去 `decode` 会撞上 encoding_rs 那条 panic 的路。宁可按 UTF-8 收：
    // 症状是一排问号，而不是一本书打不开。
    if is_utf16(encoding) {
        return encoding_rs::UTF_8.decode(bytes).0.into_owned();
    }
    // 第二个返回值是「实际用的编码」（ISO-2022-JP 那类会自己改口），这里用不上：
    // 排版只吃文本。
    encoding.decode(bytes).0.into_owned()
}

/// 有声明位的格式（单文件 HTML / FB2）的**唯一**读入口：字节 → 文本。
///
/// 这两个 provider 从前各写一遍 `std::fs::read_to_string`，那是同一个缺陷的两次独立
/// 发生：只吃 UTF-8。而码页那四档顺序早就写完并跑在 CHM 上，`encoding_rs` 也早就是
/// folio-core 的直接依赖 —— 缺的只是把这一族接到这条链上。接上之后立刻多解决一类书：
/// 中文网页另存出来的单文件 HTML 满地是 GBK **且自带 `<meta http-equiv=… charset=…>`**，
/// 声明本来是够的，以前照样读不动。
///
/// 走的是 [`decode`] 的 `project = None` 那一路：BOM 与文件自己的声明优先，两样都没有
/// 时按 UTF-8 收。这一族**故意不接探测器**：HTML 与 XML 的规范都留了声明位，缺那一处
/// 的文件本来就是坏的，而本机一本那样的样本都没有 —— 为没量过的形态赌一个码页，正是
/// 模块头禁的那件事。真撞上了再说，判据已经在 [`guess_codepage`] 那边备着。
///
/// 这一条也不改 [`decode`] 的行为：那条「没有容器码页就不猜」的立场被 `charset::tests`
/// 里一条断言钉着，而 CHM 也在同一条链上 —— 容器报了码页的那一族、什么都没报的 markup、
/// 与根本没有声明位的纯文本，是三件不同的事。
///
/// 与 [`read_plain`] 的分别是「这个格式**有没有**声明位」，不是码页策略的松紧：
/// HTML 与 FB2 规范里都留了那一处，纯文本没有。
pub(crate) fn read_markup(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).io_path(path)?;
    Ok(decode(&bytes, None))
}

/// 没有声明位的格式（TXT / Markdown）的读入口：**BOM > 合法 UTF-8 > 一个已知的码页 > UTF-8**。
///
/// 与 [`read_markup`] 的分别只有一条，但那一条会毁掉一整本书：[`decode`] 的第二档是
/// 「文件里写着 `charset=`/`encoding=` 就听它的」，那对 HTML 与 XML 是规范行为，对纯文本
/// 不是 —— 纯文本的每一行都是正文，没有哪一处是指令。一本讲编码的技术书在前 1 KB 里出现
/// `charset=gbk` 那样一串字（真书里就长在代码清单里），按那一档走会把一整本合法 UTF-8
/// 的书解成 GBK，而判定与导入全程报「成功」。所以这一族走 [`decode_plain`]，
/// 那条链里**没有声明那一档**。
///
/// 「合法 UTF-8 先收、绝不交给探测器」不是省时间，是探测器自己的口径逼的：实测把一段
/// UTF-8 的中文截到 1 KiB 喂进去，它答 `windows-1252` —— 尾巴上那半个字符就让它放弃
/// UTF-8 这一档，而 `windows-1252` 解中文 UTF-8 得到的是一串带重音的拉丁垃圾。
///
/// 三档都不中才轮到 [`guess_codepage`]。它认不出（或答案不在白名单里）就按 UTF-8 收，
/// 症状是一排替换字符 —— 看得见的坏，好过猜出一个像汉字的东西。
pub(crate) fn read_plain(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).io_path(path)?;
    Ok(decode_detected(&bytes))
}

/// 没有文件、只有字节时的同一条链（PDB 的 PalmDOC 记录用这一支）。
///
/// 与 [`read_plain`] 逐字同源：合法 UTF-8 先收，否则问一次白名单内的探测器，
/// 认不出就按 UTF-8 收留看得见的洞。分成两处写就会有一处忘了「UTF-8 先收」，
/// 那类症状是中文 UTF-8 被猜成 windows-1252。
pub(crate) fn decode_detected(bytes: &[u8]) -> String {
    // 合法 UTF-8 一律直接收，也不看里面有没有那串关键字：这是最保守的一档，同时是
    // 「纯 ASCII 永远不漂」的原因（ASCII 同时是五种候选码页的合法输入，任何
    // 「比谁坏字节少」的判据在这种文件上都是掷硬币）。
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    decode_plain(bytes, guess_codepage(bytes))
}

/// 探测器答出来才肯采纳的那几族。
///
/// 探测器是为浏览器造的，`guess` **没有「认不出」这一档**（0.1 那个置信度返回值在 1.0
/// 的公共 API 里没了），任何字节流它都给一个答案。latin-1 那一族对中文书永远是错的，
/// 所以名单外的一律当没猜出来，由调用方退回 UTF-8 那个看得见的洞。
const ACCEPTED_CODEPAGES: &[&Encoding] = &[
    encoding_rs::GBK,
    encoding_rs::GB18030,
    encoding_rs::BIG5,
    encoding_rs::SHIFT_JIS,
    encoding_rs::EUC_KR,
];

/// 一次喂给探测器多少字节。
///
/// 实测四种码页（GBK / Big5 / Shift-JIS / EUC-KR）都在 1 KiB 内定下来，而 [`feed`] 的
/// 返回值本身就是「够了，可以停」这个信号，所以一本 6 MB 的书不会全量过一遍探测器。
///
/// [`feed`]: chardetng::EncodingDetector::feed
const DETECT_CHUNK: usize = 1024;

/// 最后那一档：裸文本自己没说话、又不是合法 UTF-8 时，问一次探测器。
///
/// `Utf8Detection::Deny` 是因为能走到这一条就已经确认「整份不是合法 UTF-8」—— 让探测器
/// 这时回一个 UTF-8，等于把同一件坏事用 lossy 再做一遍，而那一件是调用方的兜底、
/// 不该伪装成一个判定结果。
fn guess_codepage(bytes: &[u8]) -> Option<&'static Encoding> {
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    let mut fed = 0usize;
    for chunk in bytes.chunks(DETECT_CHUNK) {
        fed += chunk.len();
        // `last = true` 之后再 feed 会 panic，所以下面两个条件都直接离开循环。
        let decided = detector.feed(chunk, fed == bytes.len());
        if decided || fed == bytes.len() {
            break;
        }
    }
    let found = detector.guess(None, Utf8Detection::Deny);
    ACCEPTED_CODEPAGES.contains(&found).then_some(found)
}

/// 纯文本那一条链：**BOM > 已知码页 > UTF-8**，中间**没有声明那一档**。
///
/// 与 [`decode`] 差的就是那一档，理由见 [`read_plain`]。BOM 仍然先走
/// （[`decode_utf16_bom`] 那一条分支）：那两个字节是文件里的真字节，不是一行可能出现在
/// 正文里的字，所以它不属于「声明」那一族的问题。UTF-8 的 BOM 不用特殊照顾 ——
/// `encoding_rs` 的 `decode` 按 WHATWG 的规定把开头那个 U+FEFF 剥掉。
fn decode_plain(bytes: &[u8], guessed: Option<&'static Encoding>) -> String {
    if let Some(text) = decode_utf16_bom(bytes) {
        return text;
    }
    guessed
        .unwrap_or(encoding_rs::UTF_8)
        .decode(bytes)
        .0
        .into_owned()
}

/// 这个文件自己说的码页：`<meta>` 里的 `charset=` 或 XML 声明里的 `encoding=`。
///
/// 只认**声明**，不认「看起来像什么」—— 后者是猜。没声明回 `None`，由调用方决定
/// 退到哪一档。BOM 不在这里报：那两个编码由 [`decode`] 自己那条先走。
pub(crate) fn declared(bytes: &[u8]) -> Option<&'static Encoding> {
    let head = &bytes[..bytes.len().min(PRESCAN)];
    let at = keyword_end(head)?;
    // 借 encoding_rs 那张 WHATWG 别名表把标签换编码：`gb2312` 与 `gbk` 落到同一个
    // GBK、`sjis` 落到 Shift-JIS 都归它管。自己写对照表迟早在这类别名上漂。
    Encoding::for_label(label_at(head, at).as_bytes())
}

/// 头里第一个 `charset` 或 `encoding` 关键字**之后**的位置。
///
/// 两个关键字各在各的书都有：`<?xml version="1.0" encoding="…"?>` 与
/// `<meta http-equiv="Content-Type" content="text/html; charset=…">`。所以**在的
/// 那几个里取最靠前的**，而不是要求两个都在 —— 少了哪一个都不算没声明。
fn keyword_end(head: &[u8]) -> Option<usize> {
    let hit = |needle: &[u8]| find_ci(head, needle).map(|at| at + needle.len());
    match (hit(b"charset"), hit(b"encoding")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// 从 `at` 起跳过 `=`、引号与空白，取那一段标签字符（字母数字与 `-_.:`）。
fn label_at(head: &[u8], at: usize) -> String {
    let mut i = at;
    while i < head.len() && matches!(head[i], b'=' | b'"' | b'\'' | b' ' | b'\t') {
        i += 1;
    }
    let start = i;
    while i < head.len()
        && (head[i].is_ascii_alphanumeric() || matches!(head[i], b'-' | b'_' | b'.' | b':'))
    {
        i += 1;
    }
    String::from_utf8_lossy(&head[start..i]).into_owned()
}

/// 大小写不敏感地找一段 ASCII 针。
fn find_ci(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
}

fn is_utf16(encoding: &'static Encoding) -> bool {
    encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE
}

/// 带 UTF-16 BOM 的那两种自己解。
///
/// 用 `chunks_exact` 而不是 clippy 建议的 `as_chunks`：截断的文件尾巴上会落单一个字节，
/// 前者丢掉它，后者直接 panic。
fn decode_utf16_bom(bytes: &[u8]) -> Option<String> {
    let little = if bytes.starts_with(b"\xFF\xFE") {
        true
    } else if bytes.starts_with(b"\xFE\xFF") {
        false
    } else {
        return None;
    };
    let units: Vec<u16> = if little {
        bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_le_bytes(*p))
            .collect()
    } else {
        bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_be_bytes(*p))
            .collect()
    };
    Some(String::from_utf16_lossy(&units))
}

/// LCID → 码页。
///
/// `#SYSTEM` 里那个数是**语言 ID** 而不是码页号：LCID = 方言 << 10 | 语言，所以两个
/// 都要看 —— 中文这一族光看语言不够，简体与繁体分家在**方言**那半截上。
///
/// 表只收「不写声明就会错得难看」的那几档。其余回 `None` —— 让调用方退到
/// 「按 UTF-8 收」，比替一个 0x0410（意大利语）硬猜 windows-1252 诚实。
pub(crate) fn from_lcid(lcid: u32) -> Option<&'static Encoding> {
    let primary = (lcid & 0x3FF) as u16;
    let sub = (lcid >> 10) as u16;
    match primary {
        // 中文：zh-TW(1) / zh-HK(3) / zh-MO(5) 是繁体走 Big5，其余（zh-CN、zh-SG
        // 以及方言认不出的）走 GBK —— 真书里简体的数量压倒性地多，而 GBK 是 GB2312
        // 的超集，兜这一边少错。
        0x04 if matches!(sub, 1 | 3 | 5) => Some(encoding_rs::BIG5),
        0x04 => Some(encoding_rs::GBK),
        0x11 => Some(encoding_rs::SHIFT_JIS),
        0x12 => Some(encoding_rs::EUC_KR),
        // 俄语 / 乌克兰语 / 白俄罗斯语
        0x19 | 0x22 | 0x33 => Some(encoding_rs::WINDOWS_1251),
        // 希腊语 / 土耳其语 / 希伯来语 / 阿拉伯语
        0x08 => Some(encoding_rs::WINDOWS_1253),
        0x1F => Some(encoding_rs::WINDOWS_1254),
        0x0D => Some(encoding_rs::WINDOWS_1255),
        0x01 => Some(encoding_rs::WINDOWS_1256),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! 每一档都要有一条**关掉它就变红**的断言。第一组尤其重要：GBK 字节按 UTF-8
    //! 收会得到一排问号，而那一趟不报错，所以必须有一处钉住「解出来是那串汉字」。

    use super::{declared, decode, from_lcid};

    const CN: &str = "中文测试";

    fn gbk_bytes() -> Vec<u8> {
        encoding_rs::GBK.encode(CN).0.to_vec()
    }

    /// **没有声明也没有 BOM 的 GBK 字节**：只有容器报的项目码页能救它。
    #[test]
    fn the_project_codepage_rescues_undeclared_bytes() {
        let bytes = gbk_bytes();
        assert!(
            std::str::from_utf8(&bytes).is_err(),
            "GBK 的「中文测试」不该是合法 UTF-8，否则这条测试没测到东西"
        );
        assert_eq!(
            decode(&bytes, from_lcid(0x0804)),
            CN,
            "LCID 0x0804（zh-CN）该解成那串汉字"
        );
        // 没有项目码页时按 UTF-8 收：出现替换字符，且**这是有意的**（模块头最后一档）。
        let lossy = decode(&bytes, None);
        assert!(
            lossy.contains('\u{fffd}'),
            "不猜码页时该留下看得见的洞，而不是猜出一个像汉字的东西"
        );
    }

    /// 文件自己的声明优先于容器报的那一档：两份不一致时听文件的。
    #[test]
    fn a_meta_declaration_beats_the_project_codepage() {
        // 内容是 UTF-8，容器说 GBK —— 声明赢了才读得对。
        let declared_utf8 = b"<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\"></head><body>\xe4\xb8\xad\xe6\x96\x87</body></html>";
        let text = decode(declared_utf8, Some(encoding_rs::GBK));
        assert!(text.contains("中文"), "声明里的 utf-8 该赢：{text}");
        assert_eq!(
            declared(declared_utf8),
            Some(encoding_rs::UTF_8),
            "取的是 charset 的值，不是 Content-Type"
        );
    }

    /// 反过来也要成立：声明说 GBK 时，容器报 UTF-8 也不算数。这一条钉住
    /// 「前两级是文件自己说的话」不是顺口说的。
    #[test]
    fn a_gbk_declaration_is_believed_even_when_the_container_says_utf8() {
        let head = b"<meta http-equiv=Content-Type content=\"text/html; charset=gbk\"><body>";
        let mut bytes = head.to_vec();
        bytes.extend_from_slice(&gbk_bytes());
        let text = decode(&bytes, Some(encoding_rs::UTF_8));
        assert!(text.ends_with(CN), "声明该赢过容器报的 utf-8：{text}");
    }

    /// BOM 比声明硬：UTF-16LE 带 BOM 的文件，即便 `<meta>` 写着别家也按 BOM 解。
    #[test]
    fn a_bom_beats_a_declaration() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "<a>中文</a>".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode(&bytes, Some(encoding_rs::GBK)), "<a>中文</a>");
    }

    /// 声明写着 utf-16 而**没有** BOM：不许 panic（那是 encoding_rs 不做字节解码的
    /// 一档），退成按 UTF-8 收。
    #[test]
    fn a_utf16_declaration_without_a_bom_does_not_panic() {
        let bytes = b"<meta charset=utf-16>\xe4\xb8\xad\xe6\x96\x87";
        assert_eq!(declared(bytes), Some(encoding_rs::UTF_16LE));
        let text = decode(bytes, None);
        assert!(text.contains('中'), "退到 UTF-8 至少正文还在：{text}");
    }

    /// `charset=gb2312` 这个标签按 WHATWG 就映射到 GBK（它是超集），不另开一档。
    #[test]
    fn the_gb2312_label_resolves_to_gbk() {
        assert_eq!(
            declared(b"<meta http-equiv=Content-Type content=\"text/html; charset=gb2312\">"),
            Some(encoding_rs::GBK)
        );
    }

    /// XML 声明那一条也要认（真 CHM 里 XHTML topic 的头部是 `<?xml … encoding=…?>`）。
    #[test]
    fn the_xml_declaration_is_read_too() {
        assert_eq!(
            declared(b"<?xml version=\"1.0\" encoding=\"Shift_JIS\"?>"),
            Some(encoding_rs::SHIFT_JIS)
        );
    }

    /// 什么都不说时 `declared` 回 `None` —— 猜是调用方的事，不是这一层的事。
    /// LCID 那半截另有一组断言：**繁简分家在方言那一档**，只看语言会全错。
    #[test]
    fn silence_is_reported_as_silence() {
        assert_eq!(
            declared(b"<html><body>\xe4\xb8\xad\xe6\x96\x87</body></html>"),
            None
        );
        assert_eq!(from_lcid(0x0410), None, "意大利语不在表里，如实说不知道");
        assert_eq!(from_lcid(0x0804), Some(encoding_rs::GBK), "zh-CN 简体");
        assert_eq!(from_lcid(0x0404), Some(encoding_rs::BIG5), "zh-TW 繁体");
        assert_eq!(from_lcid(0x0C04), Some(encoding_rs::BIG5), "zh-HK 也是繁体");
        assert_eq!(from_lcid(0x0411), Some(encoding_rs::SHIFT_JIS), "日语");
    }

    /// 合法 UTF-8 且没有声明：原样过，不起替换字符。
    #[test]
    fn clean_utf8_passes_through() {
        assert_eq!(decode(CN.as_bytes(), None), CN);
    }

    /// 容器报了码页、字节却是合法 UTF-8 而且没有声明（转格式转出来的书常见这一形）：
    /// 按 UTF-8 收。这一条是 `decode` 里那一档 `is_err()` 门的存在理由 —— 关掉它，
    /// 得到的是「看起来像汉字」的垃圾而不是问号。
    #[test]
    fn valid_utf8_bytes_beat_a_project_codepage_that_does_not_fit() {
        assert!(
            !gbk_bytes().is_empty() && std::str::from_utf8(&gbk_bytes()).is_err(),
            "对照组：GBK 字节不是合法 UTF-8"
        );
        assert_eq!(
            decode(CN.as_bytes(), Some(encoding_rs::GBK)),
            CN,
            "UTF-8 字节 + 容器说 GBK：该按 UTF-8 收"
        );
    }

    // ——————————————————————————————————————————————————————————————
    // 裸文本那一族（`read_plain` 的链）：探测器、白名单、以及「纯文本没有声明位」那条
    // 与 markup 的唯一分别。每一档都要有一条**关掉它就变红**的断言。
    // ——————————————————————————————————————————————————————————————

    use super::{Encoding, decode_plain, guess_codepage};

    /// 按某个码页编码一段文本，造出「真书里那种字节」。
    fn encoded_in(text: &str, label: &str) -> Vec<u8> {
        Encoding::for_label_no_replacement(label.as_bytes())
            .unwrap_or_else(|| panic!("码页标签 {label} 认不出"))
            .encode(text)
            .0
            .to_vec()
    }

    const SIMP: &str = "大雪封山的那一夜，他终于推开那扇门。门里没有灯火，只有一具伏在案上的尸身。";
    const TRAD: &str = "大雪封山的那一夜，他終於推開那扇門。門裡沒有燈火，只有一具伏在案上的屍身。";
    const JP: &str = "雪が閉ざされたあの夜、ついにあの扉を開けた。扉の向こうに灯りはなかった。";
    const KR: &str = "눈이 산을 봉쇄한 그 밤, 그는 마침내 그 문을 열었다.";

    /// **这条是这个模块里最要紧的一条**：简体 GBK 与繁体 Big5 在字节合法性上分不开
    /// （同一段 GBK 字节拿 Big5 解也是零坏字节），所以「取坏字节最少」那种自制判据会
    /// 掷硬币。探测器分得开 —— 四族各自解回自己那句话。
    #[test]
    fn four_legacy_codepages_each_come_back_as_their_own_text() {
        for (label, text) in [
            ("gbk", SIMP),
            ("big5", TRAD),
            ("shift_jis", JP),
            ("euc-kr", KR),
        ] {
            let bytes = encoded_in(text, label);
            assert!(
                std::str::from_utf8(&bytes).is_err(),
                "{label} 那份该真的不是 UTF-8，否则这条没测到东西"
            );
            let found = guess_codepage(&bytes);
            assert_eq!(
                found.map(|e| e.name().to_ascii_lowercase()),
                Some(label.to_ascii_lowercase()),
                "{label}：探测器该认出是 {label}"
            );
            assert_eq!(
                decode_plain(&bytes, found),
                text,
                "{label}：解回来要逐字相同"
            );
        }
        // 分开得开这两族这件事本身也要钉住：Big5 的字节被 GB18030 解会得到另一种东西。
        let big5 = encoded_in(TRAD, "big5");
        assert_ne!(
            encoding_rs::GB18030.decode(&big5).0.as_ref(),
            TRAD,
            "误读会写出像汉字的话，这条存在就是为了不许把它当等价"
        );
    }

    /// **白名单外的答案一律不采纳。** 探测器是为浏览器造的，任何字节流它都给一个答案、
    /// 也没有「认不出」这一档；latin-1 那一族对中文书永远是错的。拒掉之后调用方按
    /// UTF-8 收，症状是一排替换字符 —— 那是看得见的坏。
    #[test]
    fn an_answer_outside_the_whitelist_is_not_adopted() {
        // 一段法文按 windows-1252 存：不是合法 UTF-8，探测器会给 latin-1 那一族。
        let bytes = encoded_in(
            "Le café était fermé. Nul ici ne comprend la langue.",
            "windows-1252",
        );
        assert!(
            std::str::from_utf8(&bytes).is_err(),
            "这份字节必须真的不是 UTF-8"
        );
        let found = guess_codepage(&bytes);
        assert_eq!(found, None, "白名单外的码页不许被采纳：{found:?}");
        let text = decode_plain(&bytes, found);
        assert!(
            text.contains('\u{fffd}'),
            "退回 UTF-8 就该留下看得见的洞：{text}"
        );
    }

    /// 纯文本那条链**没有声明那一档**，这是它与 [`decode`] 唯一的分别，而这条分别要单独
    /// 钉住：一本 GBK 的技术书正文里出现 `charset=utf-8` 那样一串 ASCII 字（真书里长在
    /// 代码清单里），听声明就会把整本解成一排问号 —— 而猜出来的码页是对的。
    ///
    /// 把 `decode_plain` 合并回 `decode`（有人会觉得两条链该合成一条）红的第一条就是它。
    #[test]
    fn the_plain_chain_skips_a_declaration_the_markup_chain_would_obey() {
        let body = "示例：<meta charset=utf-8>\n";
        let bytes = encoded_in(&format!("{body}{SIMP}"), "gbk");
        // 对照组：markup 那条链会听这一行，于是同一份字节解成问号。
        assert!(
            decode(&bytes, None).contains('\u{fffd}'),
            "声明那一档在 markup 上确实生效（否则这条对照没测到东西）"
        );
        // 纯文本这一族不听：猜出来的 GBK 直接解，正文逐字回来。
        let text = decode_plain(&bytes, guess_codepage(&bytes));
        assert!(text.contains(SIMP), "不该被正文里那行字劫持：{text}");
        assert!(!text.contains('\u{fffd}'), "解得通就不该留洞：{text}");
    }

    /// UTF-8 的 BOM 仍然剥得掉，且剥掉之后不留那个 U+FEFF —— 这一格以前会作为正文首字符
    /// 混进锚点坐标系，把整本书的可见文本偏移顶上一格。
    #[test]
    fn a_utf8_bom_is_stripped_rather_than_kept_as_the_first_character() {
        let mut bytes = b"\xef\xbb\xbf".to_vec();
        bytes.extend_from_slice(SIMP.as_bytes());
        let text = decode_plain(&bytes, None);
        assert_eq!(text, SIMP, "BOM 不该作为正文的第一个字符留下");
        assert!(!text.starts_with('\u{feff}'));
    }
}
