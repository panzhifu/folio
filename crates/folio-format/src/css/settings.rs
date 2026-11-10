//! 读者对图片尺寸那一档。
//!
//! 类型本身住在 folio-model（format 的样式计算、core 的配置、app 的设置页三个
//! 消费方共用的数据词汇）；这里只保留旧路径的 re-export ——
//! `folio_format::css::settings::ImageScale` / `folio_format::css::ImageScale`
//! 与 core 的 `config::ImageScale` 拿到的都是同一个类型，配置文件零迁移。

pub use folio_model::reading::ImageScale;
