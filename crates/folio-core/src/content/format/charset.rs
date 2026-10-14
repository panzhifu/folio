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

use encoding_rs::Encoding;

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
            .chunks_exact(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect()
    } else {
        bytes[2..]
            .chunks_exact(2)
            .map(|p| u16::from_be_bytes([p[0], p[1]]))
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
}
