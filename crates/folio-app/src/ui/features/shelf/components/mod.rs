//! 书架的领域组件：一格卡片（含没有封面那一格）与卡片上那条管理菜单。
//!
//! 它们知道 `ShelfItem`，所以租在这里；网格、搜索结果、信息页都摆同一格卡片。

pub(in crate::ui::features::shelf) mod book_card;
pub(in crate::ui::features::shelf) mod book_menu;

pub(crate) use book_card::AUTHOR_SEP;
