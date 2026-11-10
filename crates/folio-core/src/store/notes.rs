//! 笔记聚合：把全库的批注与书签折成「一本书一张卡」的跨书清单。
//!
//! 读者隔了些日子回来，要的是「我在哪些书里留了哪些话」。这一层的主语因此是**书**，
//! 不是逐条的话：按书分组、书签在前批注在后（各自按章序、同章按钉下的顺序）、组间
//! 按最近动静倒序。出处（章名）来自库里那张章标题表 —— 这一屏没有排版可问，库是权威。
//! 渲染那一层（卡片、弹层、撤销胶囊）住在 app：这里只交纯数据，封面也只交**编码字节**
//! —— 解码成可画的图是 gpui 那一侧的事。

use std::collections::HashMap;
use std::path::PathBuf;

use crate::content::assets::cover::CoverRef;
use crate::store::Store;

/// 清单里的一行。书签与批注各占一行，字段是两族的并集 —— 与目录面板「批注」那一格
/// 同一个形状，只是出处那句在这里由库里的章标题表给（这一屏没有排版可问）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkEntry {
    pub is_bookmark: bool,
    pub id: i64,
    pub chapter: usize,
    pub offset: usize,
    /// 章名。库里没有那一行（定版式没有章表、老库缺号）时是空串，界面就不显出处。
    pub place: String,
    pub quote: String,
    pub note: String,
    pub color: Option<String>,
    pub stale: bool,
    pub created_at: i64,
}

/// 一本书的那一组，也就是列表上的那张卡。
#[derive(Debug, Clone, Default)]
pub struct BookGroup {
    pub book_id: i64,
    pub title: String,
    /// 组内最近一次动静。组间按它倒序：最近留过话的书在最前。
    pub latest: i64,
    pub marks: Vec<MarkEntry>,
    /// 这一本的**累计**阅读秒数（`reading_sessions` 合计）。取不到按 0 处理：
    /// 「这本书还没读过」是常见状态，不是异常，卡片就念「0 秒」那一档的句子。
    pub seconds: i64,
    /// 卡片左边那一格封面的**编码字节**（列表档缩略图，取不到时是原图）。
    /// `None` = 画一块灰底占位，封面是装饰，不是内容的入口。
    pub cover: Option<Vec<u8>>,
}

/// 从库里折出按书分组的清单。整趟只在切进这一屏（或本屏删改之后）跑一次，
/// `render` 拿到的已经是能直接画的东西。
pub fn load_groups(store: &Store) -> crate::error::Result<Vec<BookGroup>> {
    let mut groups: HashMap<i64, BookGroup> = HashMap::new();
    for card in store.all_annotations()? {
        let annotation = &card.annotation;
        let group = groups
            .entry(annotation.book_id)
            .or_insert_with(|| BookGroup {
                book_id: annotation.book_id,
                title: card.book_title.clone(),
                latest: annotation.created_at,
                marks: Vec::new(),
                seconds: 0,
                cover: None,
            });
        group.latest = group.latest.max(annotation.created_at);
        group.marks.push(MarkEntry {
            is_bookmark: false,
            id: annotation.id,
            chapter: annotation.chapter as usize,
            offset: annotation.anchor.start,
            place: String::new(),
            quote: snippet(&annotation.anchor.exact),
            note: annotation.note.trim().to_string(),
            color: Some(annotation.color.clone()),
            stale: annotation.stale,
            created_at: annotation.created_at,
        });
    }
    for card in store.all_bookmarks()? {
        let bookmark = &card.bookmark;
        let group = groups.entry(bookmark.book_id).or_insert_with(|| BookGroup {
            book_id: bookmark.book_id,
            title: card.book_title.clone(),
            latest: bookmark.created_at,
            marks: Vec::new(),
            seconds: 0,
            cover: None,
        });
        group.latest = group.latest.max(bookmark.created_at);
        group.marks.push(MarkEntry {
            is_bookmark: true,
            id: bookmark.id,
            chapter: bookmark.chapter as usize,
            offset: bookmark.anchor.start,
            place: String::new(),
            quote: snippet(&bookmark.anchor.exact),
            note: String::new(),
            color: None,
            stale: false,
            created_at: bookmark.created_at,
        });
    }
    // 封面的那两格描述（书库根 + 封面名分）只有 `shelf()` 一条语句给，所以取一次整架再
    // 只留下这一屏用得到的那几本：界面不该自己再从 `books.path` 反推一遍书库根 ——
    // 那是第二处真相。
    let mut cover_refs: HashMap<i64, (PathBuf, CoverRef)> = HashMap::new();
    for item in store.shelf("").unwrap_or_default() {
        if let (Some(root), Some(cover)) = (item.cover_root, item.cover_ref)
            && groups.contains_key(&item.id)
        {
            cover_refs.insert(item.id, (root, cover));
        }
    }
    // 每组一张章标题表：出处那一句在这里填 —— 这一屏没有排版可问，库里那张表是权威。
    for group in groups.values_mut() {
        let titles = store.chapter_titles(group.book_id).unwrap_or_default();
        // 累计时长是每本一条聚合语句，行数就是「留过话的书」那几本；读不到按 0 处理
        // （「还没读过」不是异常）。
        group.seconds = store
            .book_reading(group.book_id)
            .map(|reading| reading.seconds)
            .unwrap_or_default();
        group.cover = cover_bytes(store, cover_refs.get(&group.book_id), group.book_id);
        // 书签在前、批注在后（与目录面板「批注」那一格同一份读法），各自按章序排，
        // 同章按钉下的顺序。
        group
            .marks
            .sort_by_key(|m| (!m.is_bookmark, m.chapter, m.created_at));
        for mark in &mut group.marks {
            mark.place = titles
                .get(mark.chapter)
                .map(|chapter| chapter.title.trim().to_string())
                .unwrap_or_default();
        }
    }
    let mut out: Vec<BookGroup> = groups.into_values().collect();
    out.sort_by(|a, b| b.latest.cmp(&a.latest).then_with(|| a.title.cmp(&b.title)));
    Ok(out)
}

/// 一句原文在清单里显多长。`exact` 可能带着排版换行，先折空白再截 —— 与目录面板
/// 「批注」那一格同一条口径。
pub fn snippet(exact: &str) -> String {
    let folded: String = exact.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = folded.chars().take(48).collect();
    if folded.chars().count() > 48 {
        out.push('…');
    }
    out
}

/// 卡片上那行读数的两个数：批注几条、书签几枚。就在本屏那份清单里数，不另查库。
pub fn counts(group: &BookGroup) -> (usize, usize) {
    let notes = group.marks.iter().filter(|mark| !mark.is_bookmark).count();
    (notes, group.marks.len() - notes)
}

/// 搜索词落在一本书上算不算对上：书名，或这一本里任何一条引文、笔记与出处。
///
/// 词在订阅那一刻已经折成小写，这里只折候选。出处也要对：读者记不住的往往是「这句
/// 出在第几章」，而章名是他手上唯一的名词。
pub fn matches_query(group: &BookGroup, query: &str) -> bool {
    let hits = |text: &str| text.to_lowercase().contains(query);
    hits(&group.title)
        || group
            .marks
            .iter()
            .any(|mark| hits(&mark.quote) || hits(&mark.note) || hits(&mark.place))
}

/// 一格封面的字节。**先试磁盘上那一份列表档缩略图**（`.thumbs/96/`，导入时与卡片档
/// 一起生成，专为「一行一本」那种小图）：卡片那一格只有 48×72，用不上原图的像素，
/// 而解码出的字节交给 gpui 后要常驻内存，省下来的是整列的常驻内存。取不到（老库没
/// 生成、写不进目录）就**退回原图** —— 缩略图是可丢弃的加速品，不是功能的入口。
/// 原图字节只在缩略图这条路走不通时才去读，免得为一格小图做两次文件 IO。
fn cover_bytes(
    store: &Store,
    cover: Option<&(PathBuf, CoverRef)>,
    book_id: i64,
) -> Option<Vec<u8>> {
    let small = cover.and_then(|(root, description)| {
        let path = crate::content::assets::thumb::thumb_path(
            root,
            crate::infra::paths::THUMB_LIST_PX,
            description,
        );
        std::fs::read(path).ok()
    });
    if small.is_some() {
        return small;
    }
    store.book_cover(book_id).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::{BookGroup, MarkEntry, counts, load_groups, matches_query, snippet};
    use crate::ChapterExt;
    use crate::content::anchor::TextAnchor;
    use crate::content::format::{Chapter, DocMeta, FlowDoc, Format};
    use crate::store::{AnnotationStyle, Store};
    use std::path::Path;

    fn meta(title: &str) -> DocMeta {
        DocMeta {
            title: title.into(),
            authors: vec!["作者甲".into()],
            format: Format::Epub,
            size: 4096,
            ..DocMeta::default()
        }
    }

    /// 一条有指纹的锚点。坐标本身随便取，测试要的是它能不能原样回来。
    fn anchor(chapter: u32, start: usize) -> TextAnchor {
        TextAnchor {
            chapter,
            start,
            end: start + 4,
            exact: "重点句子".into(),
            prefix: "前面的字".into(),
            suffix: "后面的字".into(),
            path: String::new(),
        }
    }

    /// 一本带章标题表的书 —— 出处那一句要从库里折出来。
    fn book(store: &Store, title: &str, file: &str, chapters: &[&str]) -> i64 {
        let id = store.add_book(Path::new(file), &meta(title)).unwrap();
        store
            .save_flow(
                id,
                &FlowDoc {
                    chapters: chapters.iter().map(|t| Chapter::inline(*t, "")).collect(),
                    ..Default::default()
                },
                Format::Epub,
            )
            .unwrap();
        id
    }

    #[test]
    fn groups_by_book_with_bookmarks_first_and_places_resolved() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "书甲", "/books/a.epub", &["第一章", "第二章"]);
        let b = book(&store, "书乙", "/books/b.epub", &["乙一"]);
        store
            .add_annotation(
                a,
                &anchor(1, 10),
                "甲的笔记",
                "rose",
                AnnotationStyle::default(),
            )
            .unwrap();
        store.add_bookmark(a, &anchor(1, 0)).unwrap();
        store
            .add_annotation(
                b,
                &anchor(0, 0),
                "乙的笔记",
                "amber",
                AnnotationStyle::default(),
            )
            .unwrap();

        let groups = load_groups(&store).unwrap();
        assert_eq!(groups.len(), 2, "按书分组");
        let ga = groups.iter().find(|g| g.book_id == a).unwrap();
        assert_eq!(ga.marks.len(), 2);
        assert!(ga.marks[0].is_bookmark, "书签在前");
        assert!(!ga.marks[1].is_bookmark);
        assert_eq!(ga.marks[0].place, "第二章", "出处来自库里的章标题表");
        assert_eq!(ga.marks[1].place, "第二章");
        let gb = groups.iter().find(|g| g.book_id == b).unwrap();
        assert_eq!(gb.marks[0].place, "乙一");
    }

    /// 组间排序：最近有动静的书在最前 —— 读者回来先看到他刚才在读的那本。
    #[test]
    fn the_group_with_the_latest_activity_comes_first() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "书甲", "/books/a.epub", &["一"]);
        let b = book(&store, "书乙", "/books/b.epub", &["一"]);
        store
            .add_annotation(
                a,
                &anchor(0, 0),
                "早的",
                "amber",
                AnnotationStyle::default(),
            )
            .unwrap();
        // `now()` 是秒级时钟，同一秒内靠 id 兜底；这里靠「两条一批、后写的 id 更大」
        // 不够稳，直接按创建条数把先后写死：给乙的那条是第二批，created_at >= 甲的那条。
        std::thread::sleep(std::time::Duration::from_millis(1100));
        store
            .add_annotation(b, &anchor(0, 0), "晚的", "rose", AnnotationStyle::default())
            .unwrap();

        let groups = load_groups(&store).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].title, "书乙", "最近留过话的书排最前");
        assert_eq!(groups[1].title, "书甲");
    }

    #[test]
    fn an_excerpt_folds_whitespace_and_truncates() {
        assert_eq!(snippet("甲 乙\n丙丁"), "甲 乙 丙丁");
        let long = "字".repeat(60);
        let got = snippet(&long);
        assert_eq!(got.chars().count(), 49, "48 字加一个省略号");
        assert!(got.ends_with('…'));
    }

    /// 卡片上那句「读了多久」的数从哪来：`reading_sessions` 里**这一本**的合计，按本落到
    /// 它自己那一组上。两条都要钉 —— 只断言甲那个数，「每组都填全库合计」这种错查不出来
    /// （甲的 5640 恰好也是全库的合计）。
    #[test]
    fn each_card_carries_its_own_books_total_seconds() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "书甲", "/books/a.epub", &["一"]);
        let b = book(&store, "书乙", "/books/b.epub", &["一"]);
        store
            .add_annotation(
                a,
                &anchor(0, 0),
                "甲的话",
                "rose",
                AnnotationStyle::default(),
            )
            .unwrap();
        store
            .add_annotation(
                b,
                &anchor(0, 0),
                "乙的话",
                "amber",
                AnnotationStyle::default(),
            )
            .unwrap();
        store.record_session(a, 1_000, 6_040, 1_200).unwrap();
        store.record_session(a, 9_000, 9_600, 100).unwrap();

        let groups = load_groups(&store).unwrap();
        let ga = groups.iter().find(|g| g.book_id == a).unwrap();
        let gb = groups.iter().find(|g| g.book_id == b).unwrap();
        assert_eq!(ga.seconds, 5_640, "两次会话的合计");
        assert_eq!(gb.seconds, 0, "另一本没读过就是 0，不许借别人的数");
    }

    /// 搜索词对上一本书的几条路：书名、某一条的引文、某一条自己写的笔记、某一条的出处
    /// 章名；三条都对不上就不摆这一本。
    ///
    /// 词那一侧的小写由调用方折（app 那格在订阅时就折了），这里折的是候选那一边 ——
    /// 所以拉丁书名那一条要拿**已经小写**的词来问，大小写不敏感才是这样来的。
    #[test]
    fn a_search_word_finds_a_book_by_title_quote_note_or_chapter() {
        let store = Store::in_memory().unwrap();
        let a = book(&store, "书甲", "/books/a.epub", &["第三章"]);
        let b = book(&store, "书乙", "/books/b.epub", &["第一章"]);
        let c = book(&store, "Moby Dick", "/books/c.epub", &["一"]);
        for id in [a, b, c] {
            store
                .add_annotation(
                    id,
                    &anchor(0, 0),
                    if id == a { "我的一句话" } else { "" },
                    "rose",
                    AnnotationStyle::default(),
                )
                .unwrap();
        }

        let groups = load_groups(&store).unwrap();
        let find = |id: i64| groups.iter().find(|g| g.book_id == id).unwrap();
        assert!(matches_query(find(a), "书甲"), "书名对上");
        assert!(
            matches_query(find(a), "重点句子"),
            "引文对上（锚点里 exact 那句截出来的）"
        );
        assert!(matches_query(find(a), "我的"), "自己写的笔记对上");
        assert!(matches_query(find(b), "第一章"), "出处那个章名也算对上");
        assert!(matches_query(find(c), "moby"), "候选那一边折小写");
        assert!(!matches_query(find(b), "书甲"), "书名不在另一本上");
        assert!(!matches_query(find(b), "我的"), "那句笔记只在甲那一本里");
        assert!(!matches_query(find(b), "不存在"), "三条都对不上就不摆");
    }

    /// 卡片读数的两个数就在清单里数：批注与书签各算各的，不另查库。
    #[test]
    fn counts_split_notes_and_bookmarks() {
        let mut group = BookGroup::default();
        group.marks.push(MarkEntry {
            is_bookmark: false,
            id: 1,
            chapter: 0,
            offset: 0,
            place: String::new(),
            quote: String::new(),
            note: String::new(),
            color: None,
            stale: false,
            created_at: 0,
        });
        group.marks.push(MarkEntry {
            is_bookmark: true,
            ..group.marks[0].clone()
        });
        assert_eq!(counts(&group), (1, 1));
    }
}
