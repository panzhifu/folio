//! 选区翻译：把选中的字交给外部引擎，弹层里显回译文。
//!
//! 形状与全文搜索同一条：**判断在 model**（这一层），元素树归阅读页的渲染
//! （`reading_page::translation_popover`）。三件事在这里：
//!
//! - **开与换**：选区菜单那格「翻译」（[`ReaderSession::open_translation`]）与
//!   弹层里换目标语言（[`ReaderSession::set_translation_lang`]）都汇到
//!   [`ReaderSession::translate_source`] —— 源文以弹层里那份为准（选区在弹层
//!   升起那一刻就收掉了，换语言时手里只有它自己存的那份）。
//! - **缓存先行**：同一段字、同一对语言、同一个引擎（`folio_core::store` 的
//!   `translations` 表，v24）命中就不出网 —— 这是对付费引擎配额与非官方端点 429 的
//!   第一道对冲，也是「来回看同一段译文」的零延迟路。书库没装时缓存整条跳过，翻译照常。
//! - **新一趟顶掉旧一趟的结果**：每次翻译把 seq 顶上去，晚到的旧结果在事件入口被丢掉
//!   （与撤销胶囊同一套 seq 守卫）。守卫管的是「屏幕上只摆最新那份」，并不取消已经
//!   发出的请求 —— 共用池有两条 worker，连着点两次就有两发同时在飞。对 429 真正的
//!   对冲是缓存先行与读者的操作节奏；要严格的串行车道得另立一条优先级。
//!
//! 出网纪律与全仓同一条：同步 [`Transport`] + `resident` 池 + 事件通道回界面，
//! 与 `toc_panel` 那条搜索链同一个形状。三只引擎（Google 免 key、OpenAI 兼容、
//! DeepL）的请求/响应规则都在 `folio_core::net::translate`，这里只管「按配置挑哪只、
//! 什么时候问、答案摆哪儿」。

use folio_core::infra::task::{Priority, TaskEvent, TaskHandle};
use folio_core::net::translate::{configured, source_hash};
use folio_core::{Translation, TranslationEngine};
use gpui_kit::{App, AppContext as _, Context, Pixels, Point};

use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::resident;

/// 弹层里给的那份语言短清单。端名自己标自己（中文 / English / …），不需要翻译，
/// 所以清单住在 core（`net::translate::TARGET_LANGUAGES`）—— 设置页读的也是同一份。
pub(in crate::ui::features::reader) use folio_core::net::translate::TARGET_LANGUAGES as LANGUAGES;

/// 翻译弹层的一格状态。`source` 是弹层自己的权威 —— 选区在升起那一刻就收了，
/// 换语言时手里只有这一份。
pub(in crate::ui::features::reader) struct TranslationState {
    pub(in crate::ui::features::reader) seq: u64,
    pub(in crate::ui::features::reader) source: String,
    /// 弹层锚点（窗口坐标）：菜单落地位置的原点，升起那一刻拍下来的快照。
    pub(in crate::ui::features::reader) anchor: Option<Point<Pixels>>,
    /// 这一趟用的引擎在缓存表里的键。落答案时用它，不读当时的配置 ——
    /// 请求发出之后读者可能已经在设置里换了引擎。
    pub(in crate::ui::features::reader) provider: String,
    /// 弹层头部那行引擎名。与 `provider` 分开：前者给人看，后者给缓存认。
    pub(in crate::ui::features::reader) engine: String,
    pub(in crate::ui::features::reader) status: TranslationStatus,
}

/// `Clone + Debug` 是给宿主测试的：断言里要整份比对与打印。
#[derive(Clone, Debug)]
pub(in crate::ui::features::reader) enum TranslationStatus {
    /// 那一趟还在后台跑。
    Pending,
    /// 到手了。`cached` 说的是「这份答案来自缓存，没出网」—— 弹层照实说，
    /// 读者知道这一次不用等网络。
    Done {
        text: String,
        detected: Option<String>,
        cached: bool,
    },
    /// 出网或解析失败。理由照实显 —— gtx 是非合同接口，读者该知道是引擎的问题
    /// 还是这一端的问题。
    Failed(String),
}

impl ReaderSession {
    /// 翻译弹层开着吗。阅读页拿它报「身上压着东西」（正文手势让位）。
    pub(in crate::ui::features::reader) fn translation_open(&self) -> bool {
        self.translation.is_some()
    }

    /// 选区菜单那格「翻译」：把当前选区交给引擎。
    ///
    /// 菜单先收 —— 译文弹层与菜单是前后脚的两层，叠着只会互相挡住摆位。
    pub fn open_translation(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.selected_text() else {
            return;
        };
        let text: String = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        // 弹层锚在菜单此刻落地的位置：菜单随后要收，坐标先拍下来。
        // 没量到（首帧还没画过）就给 `None`，渲染那头兜底摆在正文顶部。
        let anchor = self.menu_bounds.get().map(|bounds| bounds.origin);
        self.selection_menu_open = false;
        self.translate_source(text, anchor, cx);
    }

    /// 换目标语言：配置写回（跨书持久，与设置页同一格），源文原样再译一次。
    pub fn set_translation_lang(&mut self, code: &str, cx: &mut Context<Self>) {
        let Some(state) = self.translation.as_ref() else {
            return;
        };
        let (source, anchor) = (state.source.clone(), state.anchor);
        ConfigHolder::edit_translation(cx, |translation| {
            translation.target_lang = code.to_string();
        });
        self.translate_source(source, anchor, cx);
    }

    /// 关掉弹层。阅读页那颗关闭、点弹层外面、按返回、换章换书都走这里。
    pub(in crate::ui::features::reader) fn close_translation(&mut self, cx: &mut Context<Self>) {
        if self.translation.take().is_some() {
            cx.notify();
        }
    }

    /// 把一份源文送出去（或从缓存取回）。开弹与换语两个入口汇到这里，
    /// 所以「按配置挑引擎、seq 顶号、缓存先行、挂后台一趟」只写一遍。
    fn translate_source(
        &mut self,
        source: String,
        anchor: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        // 引擎按当前配置现建，整只搬进后台作业：请求发出之后配置再改，这一趟用的
        // 仍是出发时那份。缓存键也由它给（OpenAI 兼容那档带模型与地址指纹）。
        let config = ConfigHolder::translation(cx);
        let translator = configured(&config);
        let provider = translator.cache_key();
        let engine = engine_label(&config);
        let to = target_lang(cx);
        let hash = source_hash(&source);
        // 缓存先行。读不出缓存当没缓存 —— 缓存坏了不该把翻译整个挡住。
        if let Some(store) = self.store.as_ref()
            && let Ok(Some(text)) = store.cached_translation(&provider, FROM, &to, &hash)
        {
            tracing::debug!("翻译: 缓存命中（{} 字符）", source.chars().count());
            self.translation_seq += 1;
            self.translation = Some(TranslationState {
                seq: self.translation_seq,
                source,
                anchor,
                provider,
                engine,
                status: TranslationStatus::Done {
                    text,
                    detected: None,
                    cached: true,
                },
            });
            cx.notify();
            return;
        }
        // 顶号：晚到的旧结果在事件入口被丢掉（见 `translation_event`）。
        self.translation_seq += 1;
        let seq = self.translation_seq;
        self.translation = Some(TranslationState {
            seq,
            source: source.clone(),
            anchor,
            provider,
            engine,
            status: TranslationStatus::Pending,
        });
        let http = folio_core::net::remote::ureq::shared();
        // 作业闭包的返回值是 `Result<TranslationOutcome, TaskError>`：引擎错误经 `?`
        // 直接装进 Box<dyn Error>，TaskEvent::Finished 里携带的就是它。
        let handle: TaskHandle<TranslationOutcome> =
            resident::pool().submit_with(Priority::Interactive, move |_| {
                let got = translator.translate(&*http, &source, FROM, &to)?;
                Ok((got.text, got.detected))
            });
        // 事件桥：后台线程逐条转发，界面线程消费 —— 与全文搜索同一形状。
        let (forward, events) = async_channel::unbounded::<TaskEvent<TranslationOutcome>>();
        cx.background_spawn(async move {
            for event in handle.events.iter() {
                if forward.send_blocking(event).is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |this, cx| this.translation_event(event, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// 后台那趟回来了。只认当前那一号：晚到的旧结果直接丢 —— seq 已被新的一趟顶掉，
    /// 它的答案没有观众。
    fn translation_event(&mut self, event: TaskEvent<TranslationOutcome>, cx: &mut Context<Self>) {
        let Some(state) = self.translation.as_mut() else {
            return;
        };
        if state.seq != self.translation_seq {
            return;
        }
        match event {
            TaskEvent::Finished(Ok((text, detected))) => {
                tracing::info!(
                    from = state.source.chars().count(),
                    to = text.chars().count(),
                    "翻译: 到手"
                );
                // 顺手落缓存。写失败只记日志：缓存是优化，不是事实。
                if let Some(store) = self.store.as_ref() {
                    let to = target_lang(cx);
                    let hash = source_hash(&state.source);
                    if let Err(err) =
                        store.save_translation(&state.provider, FROM, &to, &hash, &text)
                    {
                        tracing::warn!(error = %err, "翻译: 落缓存失败");
                    }
                }
                state.status = TranslationStatus::Done {
                    text,
                    detected,
                    cached: false,
                };
            }
            TaskEvent::Finished(Err(err)) => {
                tracing::warn!(error = %err, "翻译: 引擎那一趟失败");
                state.status = TranslationStatus::Failed(err.to_string());
            }
            _ => {}
        }
        cx.notify();
    }
}

/// 后台那一趟交回的载荷。起名字是因为它出现在事件通道的类型里，裸元组读不出含义。
type TranslationOutcome = (String, Option<String>);

/// 源语固定「自动检测」：读者选中哪国文字就该按哪国翻，这一格不进配置 ——
/// 配置里留给「译成哪种语言」那一格。
const FROM: &str = "auto";

/// 目标语言：配置里那格有值用它；空串（默认）按界面语言现解析 ——
/// 「跟随」是配置里那个空串表达的选择，解析发生在用的时候。
/// 渲染那头显语言名读的也是这一条（`translation_popover`），别处再写就是两处真相。
pub(in crate::ui::features::reader) fn target_lang(cx: &App) -> String {
    let configured = ConfigHolder::translation(cx).target_lang;
    if !configured.is_empty() {
        return configured;
    }
    ConfigHolder::language(cx).locale.code().to_string()
}

/// 弹层头部那行引擎名。品牌名不翻；OpenAI 兼容那档带上模型 —— 同一个地址换模型
/// 就是另一份译文，读者该看得见当前用的是哪一只。
pub(in crate::ui::features::reader) fn engine_label(config: &Translation) -> String {
    match config.engine {
        TranslationEngine::Google => "Google".to_string(),
        TranslationEngine::OpenAi if config.openai.model.is_empty() => "OpenAI".to_string(),
        TranslationEngine::OpenAi => format!("OpenAI · {}", config.openai.model),
        TranslationEngine::DeepL => "DeepL".to_string(),
    }
}
