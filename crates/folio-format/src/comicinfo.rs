//! ComicInfo.xml —— 漫画包旁边的书目便条（ComicRack 那一族整理工具共用的那一份）。
//!
//! 漫画包**自己**没有书目区：CBZ / CB7 / CBR / CBT 就是「一堆按顺序排的图」，这一族的
//! 书名从前只能来自文件名。而实际流通的包里有很大一批带一份 `ComicInfo.xml`：ComicRack /
//! CDisplayEx / Kalila 这些整理工具把书名、作者、系列、期号、出版社、简介、语种写进去，
//! Calibre 导出漫画时同样写。所以这一格不是锦上添花，是这一族**唯一**的元数据来源。
//!
//! **字段口径按 Komga 的那份生产级解析器**（`infrastructure/metadata/comicrack/dto/
//! ComicInfo.kt`，MPL-2.0）：哪些标签存在、`Writer` / `Penciller` 那一族与 `Genre` / `Tags`
//! 都是**逗号分隔**的多值，都是它定的，不靠记忆抄。角色落到 [`DocMeta`] 的哪一格按我们自己
//! 那套语义：`dc:creator` 只收著者（`Writer`），绘者 / 墨线 / 上色 / 字稿 / 封面 / 编辑 /
//! 译者全部进 `dc:contributor` —— 那一格的注释本来就写着「译者、编者、绘者」。
//!
//! **没接的一格，理由写在这儿**：`<Chapters>` 与 `<Scenes>` 用属性写着「第几页起是这一章」，
//! 而那个 `Page` 是 0 起还是 1 起，本机**没有可信权威**（Komga 那份 DTO 根本没建模它，
//! calibre 的漫画入口不读 ComicInfo.xml，手边也没有一份带 `<Chapters>` 的真包）。猜错的后果
//! 不是「目录少一条」而是**每一条都跳到隔壁那一页**，那比不接糟得多。等拿到真样本再补，
//! 补的时候先把基数钉成门。`<Pages>`（逐页类型 / 双页标记）同理。

use quick_xml::Reader;
use quick_xml::events::Event;

use super::{DocMeta, Series};

/// 这份 sidecar 在容器里的名字。
pub(crate) const FILE: &str = "ComicInfo.xml";

/// 这一条是不是那份书目便条。只看文件名部分（它允许躺在包里的子目录），大小写归一 ——
/// `comicinfo.xml` 这种写法真在包里出现。
pub(crate) fn is_comic_info(name: &str) -> bool {
    name.rsplit(['/', '\\'])
        .next()
        .is_some_and(|file| file.eq_ignore_ascii_case(FILE))
}

/// 收进去的字段。标签名一律**归小写再去掉 `-` / `_`** 比对（`LanguageISO` → `languageiso`，
/// `CoverArtist` → `coverartist`），因为这份便条的写法在不同工具之间不统一。
const WANTED: &[(&str, Field)] = &[
    ("title", Field::Title),
    ("series", Field::Series),
    ("number", Field::Number),
    ("volume", Field::Volume),
    ("writer", Field::Writer),
    ("penciller", Field::Contributor),
    ("inker", Field::Contributor),
    ("colorist", Field::Contributor),
    ("letterer", Field::Contributor),
    ("coverartist", Field::Contributor),
    ("editor", Field::Contributor),
    ("translator", Field::Contributor),
    ("publisher", Field::Publisher),
    ("summary", Field::Summary),
    ("year", Field::Year),
    ("month", Field::Month),
    ("languageiso", Field::Language),
    ("genre", Field::Genre),
    ("tags", Field::Tags),
    ("gtin", Field::Gtin),
    ("isbn", Field::Isbn),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Title,
    Series,
    Number,
    Volume,
    Writer,
    Contributor,
    Publisher,
    Summary,
    Year,
    Month,
    Language,
    Genre,
    Tags,
    /// `GTIN`（2.5 起的条码）与 `ISBN`（旧写法）是同一件事的两种写法，取其一。
    Gtin,
    Isbn,
}

/// 解一份 ComicInfo.xml，填进书目。**有值才覆盖**：便条没写的格子留着调用方那份
/// 兜底（文件名主名）。
///
/// 只认「叶子元素的文字」（`<Title>值</Title>`）：属性式的那两族（`<Chapter Page="…"/>`、
/// `<Page Image="…"/>`）这一版不收，理由见模块头。解不开（坏 XML、空文件）就一格都不填 ——
/// 与这一层所有「锦上添花」的格子同一立场：读不到不等于读不动。
pub(crate) fn apply(xml: &[u8], meta: &mut DocMeta) {
    let values = leaf_text(xml);
    if values.is_empty() {
        return;
    }
    // 一个字段可能对应好几个标签（`Penciller` / `Inker` / `Translator` 都落 Contributor），
    // 所以这里是**全部拼起来**再切，而不是取第一个 —— 取第一个的话一本有绘者没译者的书
    // 与一本有译者没绘者的书会给出不同的答案，而那两样都该出现在 contributor 那一行。
    let get = |field: Field| -> String {
        WANTED
            .iter()
            .filter(|(_, known)| *known == field)
            .filter_map(|(name, _)| values.get(*name))
            .cloned()
            .collect::<Vec<_>>()
            .join(",")
    };
    let title = get(Field::Title);
    if !title.is_empty() {
        meta.title = title;
    }
    let writers = split_list(&get(Field::Writer));
    if !writers.is_empty() {
        meta.authors = writers;
    }
    let contributors = split_list(&get(Field::Contributor));
    if !contributors.is_empty() {
        meta.contributors = contributors;
    }
    let series = get(Field::Series);
    if !series.is_empty() {
        // 期号：`Number` 优先（这一族真正用来排序的那一格，且它是**字符串** ——
        // 真包里有 "12A" 这种），空了才退 `Volume`。
        let number = get(Field::Number);
        let position = if number.is_empty() {
            get(Field::Volume)
        } else {
            number
        };
        meta.series = Some(Series {
            name: series,
            position,
        });
    }
    let publisher = get(Field::Publisher);
    if !publisher.is_empty() {
        meta.publisher = publisher;
    }
    let summary = get(Field::Summary);
    if !summary.is_empty() {
        meta.description = summary;
    }
    let year = get(Field::Year);
    if !year.is_empty() {
        // `Year` 与 `Month` 是分开的两个整数。拼成 `YYYY-MM` 而不是折成某一天：
        // `DocMeta::published` 那条契约就是「原样存，保住书给的那一档粒度」。
        let month = get(Field::Month);
        meta.published = match month.parse::<u32>() {
            Ok(month) if (1..=12).contains(&month) => format!("{year}-{month:02}"),
            _ => year,
        };
    }
    let language = get(Field::Language);
    if !language.is_empty() {
        meta.language = language;
    }
    // `Genre` 在前、`Tags` 在后：前者是整理工具给的分类词（会显示成主题那一行），
    // 后者是自由标签，量多而杂，排在后面才不至于把分类淹掉。
    let mut subjects = split_list(&get(Field::Genre));
    for tag in split_list(&get(Field::Tags)) {
        if !subjects.iter().any(|seen| seen == &tag) {
            subjects.push(tag);
        }
    }
    if !subjects.is_empty() {
        meta.subjects = subjects;
    }
    // 两个标识格都在时按 `GTIN` 优先（2.5 起的新写法，值与 ISBN 相同）。
    let mut identifier = get(Field::Gtin);
    if identifier.is_empty() {
        identifier = get(Field::Isbn);
    }
    if !identifier.is_empty() {
        meta.isbn = identifier;
    }
}

/// 逗号分隔的一串人名 / 主题词 → 去空、去重、保序。
///
/// **切逗号而不是分号**：那是 Komga 那份解析器的口径（`splitWithRole` 切 `','`），
/// ComicRack 的写手就按这个生成。真包里 "Ann Nocenti, Mark Sidgwick" 与
/// "Brian Wood; Dan Murphy" 两种都出现过 —— 后者会被当成**一个**人名读进来，
/// 症状是作者栏多一个怪名字，而不是把一个作者错分成两个。这一条比反过来安全。
fn split_list(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(',') {
        let item = part.trim();
        if !item.is_empty() && !out.iter().any(|seen| seen == item) {
            out.push(item.to_string());
        }
    }
    out
}

/// 标签名（归一化后）→ 元素里的文字。**只收 [`WANTED`] 里点名的那些**，其余丢掉 ——
/// 这份便条的字段集一直在长，不认识的不等于错。
///
/// 同名出现多次就拼成一条逗号分隔的串，交给 [`split_list`] 去重 —— 这样「两条 `<Writer>`」
/// 与「一条 `<Writer>a, b</Writer>`」得到同一个答案。
fn leaf_text(xml: &[u8]) -> std::collections::HashMap<String, String> {
    let text = match std::str::from_utf8(xml) {
        Ok(text) => text.to_string(),
        // 这份便条按规范是 UTF-8，但整理工具不都守（ComicRack 写的是带 BOM 的 UTF-8，
        // 别的工具留过 cp1252）。解不出 UTF-8 就当没有这份便条：猜码页会把一行人名
        // 变成一串洞，而那一行是**书目**，不是正文。
        Err(_) => return std::collections::HashMap::new(),
    };
    let mut reader = Reader::from_str(&text);
    let mut out: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut open: Option<String> = None;
    let mut buf = String::new();
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) => {
                let name = normalize(e.name().as_ref());
                // 只在 wanted 上开状态：`<Pages>` / `<Chapters>` 这些容器里的东西一律不收，
                // 而它们的 Start 会把上一个 wanted 元素挤掉（这份便条不嵌套，挤掉就是干净）。
                open = wanted_field(&name).map(|_| name);
                buf.clear();
            }
            Event::Text(e) => {
                if open.is_some() {
                    super::xml_piece(&mut buf, e.as_ref(), false);
                }
            }
            // quick-xml 42 把 `&amp;` / `&#38;` 报成**独立的事件**，文本在实体处被切成两段
            // （实测：`Text("一个城市的") + GeneralRef("#38") + Text("与它的灯。")`）。
            // 只吃 `Text` 的解析器会把实体整颗丢掉，而这一族的字段里 `&` 是常见写法
            // （`Tom & Jerry`、`R&D`）—— 丢掉的症状是书目里少一个字符，屏幕上最难查。
            // 这里把实体还原成 `&名字;` 接着攒，最后统一 unescape。
            Event::GeneralRef(e) => {
                if open.is_some() {
                    super::xml_piece(&mut buf, e.as_ref(), true);
                }
            }
            Event::End(e) => {
                let name = normalize(e.name().as_ref());
                let value = buf.trim().to_string();
                buf.clear();
                if open.as_deref() == Some(name.as_str()) && !value.is_empty() {
                    let entry = out.entry(name).or_default();
                    if !entry.is_empty() {
                        entry.push(',');
                    }
                    entry.push_str(&value);
                }
                open = None;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// 元素名归一化。`LanguageISO` → `languageiso`、`CoverArtist` → `coverartist`，别的工具
/// 写成 `Language_Iso` / `Cover-Artist` / `dc:title` 也落到同一格。
fn normalize(raw: &str) -> String {
    // 前缀先剥掉（`dc:title` 与 `title` 同一格，与 `docx::local_name` 同一条口径），
    // 再小写 + 去掉 `-` 与 `_`。
    raw.rsplit(':')
        .next()
        .unwrap_or(raw)
        .to_ascii_lowercase()
        .replace(['-', '_'], "")
}

fn wanted_field(name: &str) -> Option<Field> {
    WANTED
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, field)| *field)
}
