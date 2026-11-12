//! AI 问答：把一段书本文交给一个外部模型，拿回一段文字。
//!
//! 形状照翻译立（同一套 [`folio_net`] 的 `Transport`、同一套错误分档、同一条
//! 「出网前先判必填项」的纪律），但机制**分住两个 crate**：翻译引擎与本地词典在
//! [`folio_translate`]（2026-11-11 起，原 folio-ai::translate 与 folio-core::dict
//! 搬去那边），本 crate 只剩问答。两条链的错误对应关系由 [`ai`] 模块里的
//! `From<TranslateError> for AiError` 记着——那是两个 crate 之间唯一的桥。
//!
//! 分工：本 crate 只收**纯字段**（地址、模型、密钥），不认识配置文件 ——
//! 配置到参数的映射与「这一格存不存在」的判据（没开 / 地址空 / 没填 key 不出网）
//! 住在 folio-core 的入口层（`net::ai::configured`）。日志纪律照守：
//! 正文与答案都不进日志。
//!
//! - [`ai`] —— 选区问 AI 与切章规则草案：一层 provider（OpenAI 兼容的
//!   `chat/completions`）与三档预设问法的提示词，加「拉模型清单」与流式问答
//!   （SSE 骑在 `Transport::download` 上，解析在 [`stream`]）。

pub mod ai;
mod stream;
