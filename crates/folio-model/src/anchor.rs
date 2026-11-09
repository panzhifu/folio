//! 一条锚点：坐标 + 用来找回坐标的文本指纹。
//!
//! 这里只有**数据**与它的 JSON 往返。从一张坐标表现取指纹的构造（`new`）、以及
//! 把锚点落回当前文本的重定位（`relocate` 与它的四级降级），都是行为 —— 住在
//! folio-core 的 `content::anchor`（构造要读 folio-format 的 `TextMap`）。

use serde::{Deserialize, Serialize};

/// `exact` / `prefix` / `suffix` / `path` 都是**冗余**的（都能从 `start..end` 推出来），
/// 冗余正是它的用途：源文本变了（不同版本、重新导出、章节顺序动了）时，
/// 只有坐标就只知道「指错了」，而有指纹才能把它找回来。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TextAnchor {
    pub chapter: u32,
    /// 可见文本的字符偏移，含。
    pub start: usize,
    /// 不含。`start == end` 表示一个插入点（进度就是这个形状）。
    pub end: usize,
    /// 选中的原文。进度用不到它，批注与书签用它找回位置。
    pub exact: String,
    /// `exact` 之前的上下文（folio-core 的 `CONTEXT_CHARS` 个字）。
    pub prefix: String,
    /// `exact` 之后的上下文。
    pub suffix: String,
    /// `start` 所在文本节点的 DOM 路径。
    pub path: String,
}

impl TextAnchor {
    /// 只有坐标、没有指纹的那一种（进度）：`Locator` 的一个字符偏移就够。
    pub fn point(chapter: u32, offset: usize) -> Self {
        Self {
            chapter,
            start: offset,
            end: offset,
            ..Default::default()
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// 读不回来的 JSON 一律当「没有锚点」，不是 panic 也不是半份数据。
    /// 数据库里这一列来自旧版本或别的机器，坏一条不该让整本书的标注列表打不开。
    pub fn from_json(json: &str) -> Option<TextAnchor> {
        serde_json::from_str(json).ok()
    }
}
