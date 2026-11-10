//! 字体域的机制收口：字体**字节**怎么认、怎么装，读者**自己的**字体库怎么管。
//!
//! - [`text`] —— 字节那半：name 表解析、家族登记表、随书字体的装载与子集化。
//!   渲染（断行、行盒、注册进 `TextSystem`）不在这一层 —— 接口（[`text::load::FontSink`]）
//!   在这里，实现（`TextSystemSink`）在 app；与 folio-render 的 `Measure` 同一条
//!   边界画法。原住 folio-core::text（2026-11-12 阶段五拆分原样搬来）。
//! - [`fonts`] —— 读者自己装的字体库那半：内置清单（随二进制走）、下载 / 导入 /
//!   删除的落盘半程、魔数识别与解回 sfnt。原住 folio-core::fonts。
//!
//! 两条线互不依赖：text 只认字节与 folio-format 的文档接口（`folio_format::document`），
//! fonts 只认清单、网络与磁盘。这一层不认识配置文件与书库 —— 字体根目录由调用方递
//! 进来；「按配置挑引擎」的那类入口层适配这里没有：没有可选引擎，机制只有一套。
//!
//! ## 错误自持
//!
//! [`fonts::FontsError`] 与 folio-format / folio-translate 的错误同一条口径：自持、
//! 带上下文、底层原因留在 `source` 链上。原 `folio_core::Error::Font` 变体随迁删除。

pub mod fonts;
pub mod text;
