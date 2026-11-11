//! Folio 的公共数据模型：文档、书目、锚点、位置与记录的**类型**。
//!
//! 这是诸 crate 里唯一「只有数据」的一层：格式解析（产）、排版引擎（排）与书库（存）
//! 共用同一套词汇，谁都不必为了几个类型依赖另一家的实现。边界由依赖图强制：这里
//! 没有 DOM、没有解析、没有排版、没有 SQLite 与网络，也不依赖
//! `folio-base` / `folio-format` / `folio-core`。
//!
//! 三条规矩，新东西想进来先过这三问：
//!
//! - **只有数据**：struct / enum + serde 派生与纯算术方法，零 I/O、零解析、零排版。
//!   行为留在消费它的 crate（锚点重定位在 folio-core，`copy_extension` 问 provider
//!   注册表在 folio-format —— 都是扩展 trait）。
//! - **两个以上 crate 要用才进来**：只用一个的留在原 crate。
//! - **换 crate 不换形状**：serde 表示逐字节稳定，库里的载荷不做迁移。
//!
//! - [`anchor`] —— 锚点：坐标 + 找回坐标的文本指纹（重定位算法在 folio-core）。
//! - [`char_class`] —— 词 / 空白 / 换行 / 其他：划词吸附共用的字符分类。
//! - [`content`] —— 内容两族：可重排 [`content::FlowDoc`] 与定版式 [`content::FixedDoc`]。
//! - [`format`] —— 章表与书目：[`format::Format`]、[`format::Chapter`]、[`format::DocMeta`]。
//! - [`novel`] —— `.txt` 分章规则的读者差量（出厂表与引擎在 folio-format）。
//! - [`position`] —— 阅读位置：存字符偏移，不认页码。
//! - [`reading`] —— 阅读偏好的档位词汇（如图往哪块尺寸折）。
//! - [`types`] —— 书库记录的行形状（批注 / 书签 / 集合 / 统计）。

/// 两个「存名字不存值」的档位枚举（`AnnotationStyle` / `ImageScale`）共用同一份
/// serde 口径：序列化成 `as_str()` 的键名，反序列化交 `parse()` 把坏值回落默认档。
/// 抽成宏是为了两处不各写一遍 —— 漏改一处就是「写出去的键名读不回来」。
/// 定义在模块声明之前，子模块（types / reading）直接调用；展开处各自已
/// `use serde::{Deserialize, Serialize};`。
macro_rules! serde_as_string {
    ($ty:ty) => {
        impl Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let text = String::deserialize(d)?;
                Ok(Self::parse(&text))
            }
        }
    };
}

pub mod anchor;
pub mod char_class;
pub mod content;
pub mod format;
pub mod novel;
pub mod position;
pub mod reading;
pub mod sync;
pub mod types;

pub use anchor::TextAnchor;
pub use position::Locator;
pub use reading::BookSettings;
pub use types::Tag;

#[cfg(test)]
mod boundary_tests {
    /// 这一层只有数据：清单里出现解析、排版、数据库、网络、界面或任何兄弟 crate，
    /// 就说明边界又漏了。逐行看键名而不是子串：`image` 会撞进注释里的英文单词。
    #[test]
    fn the_manifest_stays_data_only() {
        let manifest = include_str!("../Cargo.toml");
        let forbidden = [
            "rusqlite",
            "libsqlite3-sys",
            "tokio",
            "axum",
            "ureq",
            "notify",
            "dirs",
            "tts",
            "html5ever",
            "quick-xml",
            "flate2",
            "zip",
            "image",
            "thiserror",
            "chrono",
            "folio-base",
            "folio-format",
            "folio-core",
            "folio-app",
            "gpui",
        ];
        for line in manifest.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let key = line.split(['=', '.', ' ']).next().unwrap_or("");
            assert!(
                !forbidden.contains(&key),
                "folio-model 的清单里出现了 `{key}`：这一层的边界是只有数据，\
                 不认解析 / 排版 / 数据库 / 网络 / 界面"
            );
        }
    }
}
