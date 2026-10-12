//! 书架分组：把已经取好的一排书按某个键分成若干组。
//!
//! 纯函数，不碰数据库 —— SQL 只负责「取哪几本」，「怎么摆成组」是数据形状的事。
//! 放在这里才能在两端共用一份，也才能用普通测试钉死组名与组内顺序。
//!
//! 集合（collections）不走这里：它的「组」是读者自己建的、存在库里的一种数据，
//! 而不是从书本身算出来的键，见 [`crate::Store::collection_groups`]。

use std::collections::BTreeMap;

use crate::store::ShelfItem;

/// 没有作者的书落进的组名。
///
/// 用一个说得出话的名字而不是空串：界面上空组名会显示成一片空白，
/// 读者不知道那一堆是什么。
pub const UNKNOWN_AUTHOR: &str = "未知作者";

/// 不属于任何集合的书落进的组名。与 [`UNKNOWN_AUTHOR`] 同一个理由。
pub const UNGROUPED: &str = "未分组";

/// 分组依据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    /// 不分组：整架一个组，组名是空串。
    None,
    /// 按第一位作者（多作者书归给第一个）。
    Author,
    /// 按格式（epub / markdown / fb2 / html …）。
    Format,
}

impl GroupBy {
    /// 界面上按这个顺序列出来，默认第一项是不分组。
    pub const ALL: &'static [GroupBy] = &[GroupBy::None, GroupBy::Author, GroupBy::Format];
}

/// 一组书。`items` 保持入参顺序（也就是书架那条「最近在读在前」的排序）。
///
/// 没有 `Eq`：`ShelfItem` 带着 `f32` 的已读比例（见那条派生上的说明）。
#[derive(Debug, Clone, PartialEq)]
pub struct ShelfGroup {
    pub name: String,
    pub items: Vec<ShelfItem>,
}

/// 按 `by` 分组；组名升序，组内保持原顺序。
pub fn group_shelf(items: Vec<ShelfItem>, by: GroupBy) -> Vec<ShelfGroup> {
    let key_of: fn(&ShelfItem) -> String = match by {
        GroupBy::None => {
            return vec![ShelfGroup {
                name: String::new(),
                items,
            }];
        }
        GroupBy::Author => |item| {
            item.authors
                .first()
                .cloned()
                .unwrap_or_else(|| UNKNOWN_AUTHOR.to_string())
        },
        GroupBy::Format => |item| item.format.clone(),
    };

    let mut groups: BTreeMap<String, Vec<ShelfItem>> = BTreeMap::new();
    for item in items {
        groups.entry(key_of(&item)).or_default().push(item);
    }
    groups
        .into_iter()
        .map(|(name, items)| ShelfGroup { name, items })
        .collect()
}

#[cfg(test)]
mod tests {
    //! 分组：三种依据、缺作者的回落、组名顺序与组内顺序。

    use crate::group::{GroupBy, UNKNOWN_AUTHOR, group_shelf};
    use crate::store::ShelfItem;

    fn item(id: i64, authors: &[&str], format: &str) -> ShelfItem {
        ShelfItem {
            id,
            title: format!("书 {id}"),
            authors: authors.iter().map(|a| (*a).to_string()).collect(),
            format: format.to_string(),
            cover: None,
            last_opened_at: None,
            // 分组这件事不看进度：这一格在下面几条断言里都不参与。
            progress: None,
        }
    }

    #[test]
    fn no_grouping_keeps_one_bucket_and_the_original_order() {
        let items = vec![item(1, &["甲"], "epub"), item(2, &["乙"], "markdown")];
        let groups = group_shelf(items.clone(), GroupBy::None);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "");
        assert_eq!(groups[0].items, items);
    }

    #[test]
    fn author_groups_use_the_first_author_and_order_by_name() {
        let groups = group_shelf(
            vec![
                item(1, &["纪德", "译者"], "epub"),
                item(2, &["莫里亚克"], "epub"),
                item(3, &["纪德"], "markdown"),
            ],
            GroupBy::Author,
        );
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, vec!["纪德", "莫里亚克"], "组名升序");
        assert_eq!(
            groups[0].items.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![1, 3],
            "多作者书归第一位作者，组内保持原顺序"
        );
        assert_eq!(groups[1].items[0].id, 2);
    }

    #[test]
    fn a_book_without_an_author_lands_in_a_named_bucket() {
        let groups = group_shelf(vec![item(1, &[], "text")], GroupBy::Author);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, UNKNOWN_AUTHOR);
    }

    #[test]
    fn format_groups_collect_one_bucket_per_format() {
        let groups = group_shelf(
            vec![
                item(1, &["甲"], "epub"),
                item(2, &["乙"], "markdown"),
                item(3, &["丙"], "epub"),
            ],
            GroupBy::Format,
        );
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, vec!["epub", "markdown"]);
        assert_eq!(
            groups[0].items.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn an_empty_shelf_groups_into_nothing_except_the_no_group_bucket() {
        assert!(group_shelf(Vec::new(), GroupBy::Author).is_empty());
        let groups = group_shelf(Vec::new(), GroupBy::None);
        assert_eq!(groups.len(), 1, "不分组时空架也有一格");
        assert!(groups[0].items.is_empty());
    }
}
