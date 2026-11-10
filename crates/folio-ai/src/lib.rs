//! AI 问答与选区翻译：把一段书本文交给一个外部模型，拿回一段文字。
//!
//! 「同一件事的两张脸」住在一个 crate：共享传输（[`folio_net`] 的 `Transport`）、
//! 共享语言名表与错误分档的纪律，一份实现的经验两处用 —— 从 folio-core::net 的
//! `ai` / `translate` 原样搬来（「换 crate 不换形状」：core 经 shim 沿用旧路径，
//! 消费方一行不改）。
//!
//! 分工：本 crate 只收**纯字段**（地址、模型、密钥、引擎档），不认识配置文件 ——
//! 配置到参数的映射与「这一格存不存在」的判据（没开 / 地址空 / 没填 key 不出网）
//! 住在 folio-core 的入口层（`net::ai::configured` / `net::translate::configured`）。
//! 日志纪律两条链同守：正文与答案都不进日志。
//!
//! - [`ai`] —— 选区问 AI 与切章规则草案：一层 provider（OpenAI 兼容的
//!   `chat/completions`）与三档预设问法的提示词。
//! - [`translate`] —— 三只引擎（Google 免 key、OpenAI 兼容、DeepL）的请求/响应规则。

pub mod ai;
pub mod translate;
