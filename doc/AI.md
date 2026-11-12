# AI API 设计

AI 问答：一层 provider（OpenAI 兼容的 `chat/completions`）+ 三档预设问法 +
**供应商注册表** + 流式应答（SSE）。形状照翻译立（见 [翻译.md](翻译.md)）：
同一套 Transport、同一套错误分档、离线可测。**机制住 folio-ai crate**
（2026-11-11 起问答单住——翻译机制与本地词典已拆去 folio-translate）；
`folio_core::net::ai::*` 旧路径经 shim 沿用——配置到参数的映射与「这一格
存不存在」的 None 判据就在那个入口层。

## 供应商注册表（`Config.vendors`）

地址与密钥**填一次存一条档案**（`infra::config::Vendor`：id / 名称 / 地址 /
密钥），翻译与 AI 各自引用其中一条、各配各的模型。换供应商只动档案那一处，
不必把翻译屏整屏重填，翻译与 AI 也不再拴在同一个端点上。

- 引用悬空**回退第一条**（`resolve_vendor`），空表回 `None`——那是「还没配置过」。
- 旧版的内联端点（`translation.openai` 自带 base_url / api_key）由
  `Config::migrate_vendors` 折成第一条档案、引用指过去；**幂等**，折完清空。
  出厂地址且 key 空的旧值**不折**——读者从没配置过，不该替他造一条「正文可以
  发去官方 API」的档案。
- 密钥只存这一份（0600 的 config.json）；`Ai::vendor` 留空 = 跟随翻译那档的引用。

## 使用顺序（一屏看完）

```rust
// 1. 配置齐了才有这格：没开 / 注册表空 / 地址空 / 模型两处都空 → None（界面那格不出现）
let Some(ai) = configured(&translation_config, &ai_config, &vendors) else { return };

// 2. 三档预设问法挑一档，拼出提示词，连 Transport 一起递进去；流式边到边交
let system = Ask::Explain.system(answer_lang);        // Terms / Plain 另两档
let user   = 拼好的上下文（书名、章名、选中的那段，见 Asked）;
let answer = ai.ask(&transport, &system, &user)?;     // 整段（切章草案走这条）
let answer = ai.ask_stream(&transport, &system, &user, &mut on_delta, &cancel)?;

// 3. Answer 弹层摆出来；密钥与正文不进日志
```

## API 一览

```rust
// 入口层在 folio-core（shim）；机制在 folio-ai crate。
pub fn configured(translation: &TranslationConfig, ai: &Ai, vendors: &[Vendor])
    -> Option<Box<dyn Assistant>>
pub fn resolved_ai_endpoint(translation: &TranslationConfig, ai: &Ai, vendors: &[Vendor])
    -> Option<(Vendor, String)>   // 解析「会用哪条档案、哪只模型」——探测用，不看开关

pub trait Assistant: Send + Sync {
    fn cache_key(&self) -> String;   // 缓存键：带模型与地址指纹，密钥不进键
    fn label(&self) -> String;       // 弹层头部那行名字（含模型名）
    fn ask(&self, http: &dyn Transport, system: &str, user: &str)
        -> Result<Answer, AiError>;
    fn ask_stream(&self, http: &dyn Transport, system: &str, user: &str,
        on: &mut (dyn FnMut(&str) + Send), cancel: &Cancel)
        -> Result<Answer, AiError>;  // 默认退化为 ask() 整段交付
}

pub enum Ask { Explain, Terms, Plain }   // 三档预设：这段说什么 / 术语典故 / 用现在的话讲
ChatCompletion::new(base_url, model, api_key)   // 唯一 provider，实现 Assistant
pub fn list_models(http: &dyn Transport, base_url: &str, api_key: &str)
    -> Result<Vec<String>, AiError>   // GET {base}/models——探针与推荐模型共用
```

- 地址与密钥来自**被引用的供应商档案**；`Ai` 配置只答「要不要用、用哪条、用哪只模型」。
- 流式骑在 [`Transport::download`](渲染.md 说的是排版，这一条见 folio-net lib.rs) 的
  「非 2xx 状态门 + 分块读 + 块间取消检查点」上——**folio-net 零改动**。SSE 解析
  （`data:` 行、`[DONE]`、usage）在 folio-ai 的 `stream` 模块；**嗅探回退**：第一块
  以 `{` 开头按整份 JSON 解析——网关不理 `stream: true` 也照常出答案。取消后已交出去
  的增量留在调用方手里（打字机不回滚），返回 `Err(Cancelled)`。
- 探针：`list_models` 到清单 = 地址与密钥都对（还顺手填「推荐模型」下拉）；
  「问一句」发一次最小真请求、把模型的原话截 80 字符摆上来——只有这句话能证明
  对面是模型。状态机在 app 的 `services::ai`（照 `services::sync` 那颗「测试连接」）。
- 上下文与尺寸闸：`Asked`（书名 / 章名 / 选段）拼 user 提示词；
  `CONTEXT_BUDGET` 选区前后各收 300 字、`MAX_INPUT_CHARS` 12k 最后一道闸、
  `MAX_ANSWER_CHARS` 答案收 4k；`clip(text, limit)` 收文本。
- 模型预设表在 app 的 `services::ai::model_presets`——按档案 host 给几只常用型号，
  **预设不是白名单**：接入点 id（火山方舟的 `ep-…`）、relay 名、比表新的模型都要能填。
- 切章草案：`draft_system(lang)` / `draft_prompt(head, candidates)`——
  给 txt 分章规则出 AI 建议（novel::draft 那条路）。草案单发 JSON，**不走流式**。
- 提示词里的语言名来自 folio-translate 的 `language_name`（BCP-47 码 → 语言名表
  的一份真源，翻译那侧同样用它）。

## 与翻译的错误对应

两条链意思相同但**类型不共用**：翻译的 `TranslateError::NotConfigured("服务地址")`
出现在 AI 的失败卡片上会说出「引擎」这种不属于这一屏的词。对应关系由 folio-ai 里的
`From<TranslateError> for AiError` 记着——机制分住两个 crate 之后，这一句是唯一的桥。

## 设置界面（`pages/ai.rs`）

三组：**问答**（开关 / 供应商下拉 / 推荐模型下拉 / 模型格）、**连接**（探针状态行 +
测试连接 + 问一句）、**供应商**（编辑哪一条 + 名称 / 地址 / 密钥遮罩行 + ＋新建预设
菜单 / 删除确认面板）。翻译屏的 OpenAI 组摆同一份注册表的引用与模型 + 探针行；
档案编辑器只做一处（AI 屏）。密钥行的遮罩只在显示层（`Row::text(..).secret()`，
两端各画各的 Input，同一个口径）。

## 优化（现有实现里已经做了的）

- **与翻译同形状**——同一套 Transport / 错误分档 / 响应解析纪律，一份经验两处用。
- **密钥只存一份**——注册表一处落盘面（0600），引用不复制。
- **离线可测**——假传输（`Capture` / `Chunks`）分块喂 SSE，请求与解析逐字段钉死。
- **日志纪律**——只有模型名、字符数、耗时、错误档位可出声；正文与答案不进日志。
- **None 即不出现**——没配置的界面不摆「按下去报错」的按钮。
