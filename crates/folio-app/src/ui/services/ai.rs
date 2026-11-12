//! AI 的应用侧：连接探针（测试连接 / 问一句）、模型预设表与 AI 屏「正在编辑哪条档案」。
//!
//! 状态挂 `Entity` 而不是 plain `Global`，理由与 [`super::sync`] 同一份：设置页的取值
//! 闭包只在重绘时被调，后台那趟回来若不 `notify`，那一格会永远停在「正在测…」。
//!
//! 这一层只做编排：请求机制在 [`folio_core::net::ai`]，配置在 [`ConfigHolder`]。
//! 探针不是库里的作业 —— 不写进度行、不占运行名额，与同步页那一颗「测试连接」同一条。

use folio_core::net::ai::{AiError, Assistant as _, ChatCompletion, list_models};
use gpui_kit::{App, AppContext as _, Context, Entity, Global};

use crate::ui::services::config::ConfigHolder;

/// 连接探针的状态。`Ok` 里两格各自独立：「测试连接」填 models、「问一句」填 reply ——
/// 两个动作共用一条忙线，但谁的结果都不清掉对方那格。
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Probe {
    #[default]
    Idle,
    Probing,
    /// 上一次测试到了什么。有些档案（比如火山方舟）的模型清单接口不给东西，
    /// models 是 `None` 不算失败 —— 那一格本来就允许自由填。
    Ok {
        models: Option<Vec<String>>,
        reply: Option<String>,
    },
    Failed(String),
}

impl Probe {
    /// 两个探针动作共用一条忙线。
    pub fn busy(&self) -> bool {
        matches!(self, Probe::Probing)
    }
}

#[derive(Debug, Default)]
pub struct AiState {
    probe: Probe,
    /// AI 屏「编辑哪一条档案」的选中态（档案 id）。`None` = 还没动过，落第一条。
    editing: Option<String>,
    /// 待确认删除的档案 id（两段式确认，与 storage 页同一个形状：整行换成面板）。
    deleting: Option<String>,
}

impl AiState {
    pub fn probe(&self) -> &Probe {
        &self.probe
    }

    pub fn busy(&self) -> bool {
        matches!(self.probe, Probe::Probing)
    }

    /// AI 屏正在编辑的档案：记住的那条还在列表里就它，否则第一条（没有则 `None`）。
    pub fn editing(&self, cx: &App) -> Option<String> {
        let vendors = ConfigHolder::vendors(cx);
        match &self.editing {
            Some(id) if vendors.iter().any(|vendor| &vendor.id == id) => Some(id.clone()),
            _ => vendors.first().map(|vendor| vendor.id.clone()),
        }
    }

    pub fn set_editing(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.editing = id;
        cx.notify();
    }

    /// 删除确认的两段：ask 摆面板，answer 动手。删除后引用它的功能回退第一条
    /// （`resolve_vendor` 的规则，界面上说得清），探针结果一起清掉 —— 它说的
    /// 是上一条档案的事。
    pub fn ask_delete(&mut self, id: String, cx: &mut Context<Self>) {
        self.deleting = Some(id);
        cx.notify();
    }

    pub fn answer_delete(&mut self, yes: bool, cx: &mut Context<Self>) {
        if let (true, Some(id)) = (yes, self.deleting.take()) {
            ConfigHolder::edit_vendors(cx, |vendors| {
                vendors.retain(|vendor| vendor.id != id);
            });
            self.probe = Probe::Idle;
            self.editing = None;
        } else {
            self.deleting = None;
        }
        cx.notify();
    }

    /// 待确认删除的是哪一条（面板与取消钮都要认回自己的行）。
    pub fn deleting(&self) -> Option<&String> {
        self.deleting.as_ref()
    }

    fn begin(&mut self, cx: &mut Context<Self>) {
        self.probe = Probe::Probing;
        cx.notify();
    }

    fn settle_models(&mut self, outcome: Result<Vec<String>, AiError>, cx: &mut Context<Self>) {
        self.probe = match outcome {
            Ok(ids) => match &self.probe {
                Probe::Ok { reply, .. } => Probe::Ok {
                    models: Some(ids),
                    reply: reply.clone(),
                },
                _ => Probe::Ok {
                    models: Some(ids),
                    reply: None,
                },
            },
            Err(error) => {
                tracing::warn!("AI 连接测试失败：{error}");
                Probe::Failed(error.to_string())
            }
        };
        cx.notify();
    }

    fn settle_reply(&mut self, outcome: Result<String, AiError>, cx: &mut Context<Self>) {
        self.probe = match outcome {
            Ok(text) => match &self.probe {
                Probe::Ok { models, .. } => Probe::Ok {
                    models: models.clone(),
                    reply: Some(text),
                },
                _ => Probe::Ok {
                    models: None,
                    reply: Some(text),
                },
            },
            Err(error) => {
                tracing::warn!("AI 问一句失败：{error}");
                Probe::Failed(error.to_string())
            }
        };
        cx.notify();
    }
}

pub struct AiHolder(pub Entity<AiState>);

impl Global for AiHolder {}

pub fn handle(cx: &App) -> Option<Entity<AiState>> {
    cx.try_global::<AiHolder>().map(|holder| holder.0.clone())
}

/// 「正在编辑哪条」的只读口径：取值闭包手里只有 `&App`，不许为它建实体。
/// 没装过实体（还没点开过设置页）就按「落第一条」算 —— 与 [`AiState::editing`] 同一条。
pub fn editing(cx: &App) -> Option<String> {
    let picked = handle(cx).and_then(|entity| entity.read(cx).editing.clone());
    picked
        .filter(|id| {
            ConfigHolder::vendors(cx)
                .iter()
                .any(|vendor| &vendor.id == id)
        })
        .or_else(|| {
            ConfigHolder::vendors(cx)
                .first()
                .map(|vendor| vendor.id.clone())
        })
}

/// 取句柄，没有就建一个（与 `sync::entity` 同一条理由：设置页构造时手里就是
/// `&mut App`，不必等外壳先装）。
pub fn entity(cx: &mut App) -> Entity<AiState> {
    if let Some(entity) = handle(cx) {
        return entity;
    }
    let entity = cx.new(|_| AiState::default());
    cx.set_global(AiHolder(entity.clone()));
    entity
}

/// 「测试连接」：拉一次模型清单。清单到了 = 地址与密钥都对，还顺手把「推荐模型」
/// 下拉填上。`base_url` 与 `api_key` 由调用方解析好递进来 —— 探针不认识配置节，
/// AI 屏与翻译屏各按自己那一路给。
pub fn test_connection(cx: &mut App, base_url: String, api_key: String) {
    let state = entity(cx);
    if state.read(cx).busy() {
        return;
    }
    state.update(cx, |state, cx| state.begin(cx));
    let http = folio_core::net::remote::ureq::shared();
    let work = cx.background_spawn(async move { list_models(&*http, &base_url, &api_key) });
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let outcome = work.await;
        let _ = weak.update(cx, |state, cx| state.settle_models(outcome, cx));
    })
    .detach();
}

/// 「问一句」：发一次最小的真请求（诚实的探针 —— 只有模型的原话能证明对面是模型）。
/// 原话截 80 个字符上屏，全文进日志的只有长度（日志纪律：内容不上）。
pub fn ask_once(cx: &mut App, base_url: String, api_key: String, model: String) {
    let state = entity(cx);
    if state.read(cx).busy() {
        return;
    }
    state.update(cx, |state, cx| state.begin(cx));
    let provider = ChatCompletion::new(&base_url, &model, &api_key);
    let http = folio_core::net::remote::ureq::shared();
    let work = cx.background_spawn(async move {
        provider
            .ask(
                &*http,
                "",
                "This is a connection test. Reply with one short word.",
            )
            .map(|answer| folio_core::net::ai::clip(answer.text.trim(), ECHO_CHARS))
    });
    let weak = state.downgrade();
    cx.spawn(async move |cx| {
        let outcome = work.await;
        let _ = weak.update(cx, |state, cx| state.settle_reply(outcome, cx));
    })
    .detach();
}

/// 探针回答上屏的长度上限。
const ECHO_CHARS: usize = 80;

/// 「新建供应商」的预设：名称 + 出厂地址。**预设不是白名单** —— 接入点 id（火山方舟
/// 的 `ep-…`）、relay 名、比表新的模型都要能填，地址与模型随时可以整格改写。
pub struct VendorPreset {
    pub name: &'static str,
    pub base_url: &'static str,
}

pub const VENDOR_PRESETS: &[VendorPreset] = &[
    VendorPreset {
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
    },
    VendorPreset {
        name: "DeepSeek",
        base_url: "https://api.deepseek.com/v1",
    },
    VendorPreset {
        name: "Kimi",
        base_url: "https://api.moonshot.cn/v1",
    },
    VendorPreset {
        name: "智谱 GLM",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
    },
    VendorPreset {
        name: "火山方舟",
        base_url: "https://ark.cn-beijing.volces.com/api/v3",
    },
    VendorPreset {
        name: "阿里百炼",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    },
    VendorPreset {
        name: "硅基流动",
        base_url: "https://api.siliconflow.cn/v1",
    },
    VendorPreset {
        name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
    },
];

/// 按档案地址的 host 给「推荐模型」的预设清单。**建议而非白名单**：清单只为少打字，
/// 模型格始终允许自由填 —— 火山方舟的接入点 id 各家不同，干脆不给预设。
pub fn model_presets(base_url: &str) -> &'static [&'static str] {
    let host = folio_core::host_of(base_url);
    match host.as_str() {
        "api.openai.com" => &["gpt-4o-mini", "gpt-4o", "gpt-4.1"],
        "api.deepseek.com" => &["deepseek-chat", "deepseek-reasoner"],
        "api.moonshot.cn" => &["kimi-k2-0905-preview", "kimi-latest", "moonshot-v1-8k"],
        "open.bigmodel.cn" => &["glm-4.6", "glm-4.5-flash"],
        "dashscope.aliyuncs.com" => &["qwen-plus", "qwen-max", "qwen-turbo"],
        "api.siliconflow.cn" => &["deepseek-ai/DeepSeek-V3", "Qwen/Qwen2.5-7B-Instruct"],
        "openrouter.ai" => &["openai/gpt-4o-mini", "deepseek/deepseek-chat"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    //! 预设表的形状：预设的地址必须抠得出 host、模型预设按 host 找得到、
    //! 认不出的 host 给空表（自由填兜底）。

    use super::{ECHO_CHARS, VENDOR_PRESETS, model_presets};

    /// 每条预设的地址都得是能落格的 http(s) URL：预填错了，新建出来的档案就是坏的。
    #[test]
    fn every_preset_address_is_an_http_url() {
        for preset in VENDOR_PRESETS {
            assert!(
                preset.base_url.starts_with("https://") || preset.base_url.starts_with("http://"),
                "{} 的地址不是 URL：{}",
                preset.name,
                preset.base_url
            );
            assert!(!preset.name.is_empty(), "预设不该没有名字");
        }
    }

    /// 预设的地址都该在自己 host 的模型预设表下命中：新建 → 推荐模型有东西可挑。
    /// 火山方舟除外 —— 接入点 id 各家不同，那一条**故意**不给预设。
    #[test]
    fn presets_pair_with_their_model_tables() {
        for preset in VENDOR_PRESETS {
            let models = model_presets(preset.base_url);
            if preset.name == "火山方舟" {
                assert!(models.is_empty(), "方舟的接入点 id 各家不同，不该给预设");
            } else {
                assert!(
                    !models.is_empty(),
                    "{} 新建之后推荐模型是空的：预设表漏了它的 host",
                    preset.name
                );
            }
        }
    }

    /// 认不出的 host 给空表：那是「自由填」的兜底，不是「没这回事」。
    #[test]
    fn an_unknown_host_gets_no_presets() {
        assert!(model_presets("https://my-gateway.example/v1").is_empty());
        assert!(model_presets("").is_empty());
        assert_eq!(ECHO_CHARS, 80);
    }
}
