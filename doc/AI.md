# AI API 设计

AI 问答：一层 provider（OpenAI 兼容的 `chat/completions`）+ 三档预设问法 +
切章规则草案。形状照翻译立：同一套 Transport、同一套错误分档、离线可测。
**机制住 folio-ai crate**（与翻译同 crate，「同一件事的两张脸」）；
`folio_core::net::ai::*` 旧路径经 shim 沿用——配置到参数的映射与
「这一格存不存在」的 None 判据就在那个入口层。

## 使用顺序（一屏看完）

```rust
// 1. 配置齐了才有这格：没开 / 地址空 / 没 key → None（界面那格不出现）
let Some(ai) = configured(&translation_config, &ai_config) else { return };

// 2. 三档预设问法挑一档，拼出提示词，连 Transport 一起递进去
let system = Ask::Explain.system(answer_lang);        // Terms / Plain 另两档
let user   = 拼好的上下文（书名、章名、选中的那段，见 Asked）;
let answer = ai.ask(&transport, &system, &user)?;

// 3. Answer 弹层摆出来；密钥与正文不进日志
```

## API 一览

```rust
// 入口层在 folio-core（shim）；机制在 folio-ai crate。
pub fn configured(translation: &TranslationConfig, ai: &Ai) -> Option<Box<dyn Assistant>>

pub trait Assistant: Send + Sync {
    fn cache_key(&self) -> String;   // 缓存键：带模型与地址指纹，密钥不进键
    fn label(&self) -> String;       // 弹层头部那行名字（含模型名）
    fn ask(&self, http: &dyn Transport, system: &str, user: &str)
        -> Result<Answer, AiError>;
}

pub enum Ask { Explain, Terms, Plain }   // 三档预设：这段说什么 / 术语典故 / 用现在的话讲
ChatCompletion::new(base_url, model, api_key)   // 唯一 provider，实现 Assistant
```

- 地址与密钥**共用翻译那一档**配置（`OpenAi`），AI 配置只答「要不要用、用哪只模型」。
- 上下文与尺寸闸：`Asked`（书名 / 章名 / 选段）拼 user 提示词；
  `CONTEXT_BUDGET` 选区前后各收 300 字、`MAX_INPUT_CHARS` 12k 最后一道闸、
  `MAX_ANSWER_CHARS` 答案收 4k；`clip(text, limit)` 收文本。
- 切章草案：`draft_system(lang)` / `draft_prompt(head, candidates)`——
  给 txt 分章规则出 AI 建议（novel::draft 那条路）。

## 优化（现有实现里已经做了的）

- **与翻译同源**——同一套 Transport / 错误分档 / 响应解析形状，一份经验两处用。
- **密钥不复制第二份**——多一处落盘面就多一处泄漏面。
- **离线可测**——假传输 + 固定应答，请求与解析逐字段钉死。
- **日志纪律**——只有模型名、字符数、耗时、错误档位可出声；正文与答案不进日志。
- **None 即不出现**——没配置的界面不摆「按下去报错」的按钮。
