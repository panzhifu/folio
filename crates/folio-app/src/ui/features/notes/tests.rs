//! 「笔记」那一屏的折堆：`load_groups` 的分组、组内排序与出处填法。
//! 只依赖公开 API，库用 `Store::in_memory()` —— 不碰真实书库。

use std::path::Path;

use folio_core::content::anchor::TextAnchor;
use folio_core::content::format::{Chapter, DocMeta, FlowDoc, Format};
use folio_core::store::{AnnotationStyle, Store};

use super::{load_groups, snippet};

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
        .add_annotation(
            b,
            &anchor(0, 0),
            "晚的",
            "rose",
            AnnotationStyle::default(),
        )
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
