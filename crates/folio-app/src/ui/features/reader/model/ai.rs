//! 选区问 AI：把选中的字连同前后一截上下文交给模型，答案弹在原地。
//!
//! 形状照 [`model::translation`](super::translation)：provider 按当时的配置现建、整只搬进
//! 后台作业、`seq` 顶号丢旧结果、事件桥回界面。两处不同，都是这件事自己的性质：
//!
//! - **不落缓存**。翻译有唯一正确答案（同一段字、同一对语言、同一只模型），所以缓存先行
//!   是对配额与限流的第一道对冲；「这段说什么」本来就不是一个有唯一答案的问题，
//!   把第一次的答案钉给第二次读，是在替读者省钱的同時骗他。
//! - **问法有三档**（[`Ask`]）。弹层头部那个下拉换的就是它，换一次重新问一遍 ——
//!   缓存既然没有，重问的代价就是一次真请求，这是诚实的形状而不是省的形状。
//!
//! 上下文取的是**这一章的归一化文本**（`TextMap::slice`），与锚点同一把尺：偏移不换算
//! 第二遍，就不会出现「选中的是这句、问出去的是那句」。排版没跟上那一帧（`is_current()`
//! 假）与批注落库同一立场 —— 这次问不成，而不是拿着上一章的文本去问。

use folio_core::infra::task::{Priority, TaskEvent, TaskHandle};
use folio_core::net::ai::{Ask, Asked, configured, selection_prompt};
use gpui_kit::{App, AppContext as _, Context, Pixels, Point};

use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::resident;

/// 弹层里那一格的状态。`source` 与 `prompt` 是弹层自己的权威 —— 选区在升起那一刻
/// 就收掉了，换问法时手里只有这两份。
///
/// 存的是**拼好的载荷**而不是前后两段：书名与章名在开弹那一刻就折进去了（出处只在
/// 那一刻是确定的 —— 换问法期间读者可能已经翻到下一章），而换问法只该换系统提示词，
/// 不该把「这次离开设备的字」重新算一遍。
pub(in crate::ui::features::reader) struct AiState {
    pub(in crate::ui::features::reader) seq: u64,
    pub(in crate::ui::features::reader) ask: Ask,
    /// 读者选中的那段（卡片上照实回显一小截）。
    pub(in crate::ui::features) source: String,
    /// 送给模型的那一份正文（见 `Asked`：会离开设备的字的全集）。
    prompt: String,
    /// 卡片头部那行模型名（给人看的）。
    pub(in crate::ui::features::reader) label: String,
    pub(in crate::ui::features::reader) anchor: Option<Point<Pixels>>,
    pub(in crate::ui::features::reader) status: AiStatus,
}

#[derive(Clone, Debug)]
pub(in crate::ui::features::reader) enum AiStatus {
    Pending,
    /// 答案 + 模型报的 token 数（很多网关不给，给了就照实说）。
    Done {
        text: String,
        tokens: Option<u64>,
    },
    Failed(String),
}

/// 这一趟的载荷：起名字是因为它出现在事件通道的类型里，裸元组读不出含义。
type AiOutcome = (String, Option<u64>);

impl ReaderSession {
    /// AI 那一格能出现吗：开了、地址与模型都填全了才行。判据与 provider 的建法共用
    /// 同一份（[`configured`]）—— 「界面上有这一格」与「按下去问得出去」不许有两套答案。
    /// 回 `false` 时卡片里那一格**直接缺席**，不是摆一颗按下去报错的按钮。
    pub fn ai_available(cx: &App) -> bool {
        configured(&ConfigHolder::translation(cx), &ConfigHolder::ai(cx)).is_some()
    }

    /// 弹层开着吗。阅读页拿它报「身上压着东西」（正文手势让位）与「返回键先收谁」。
    pub(in crate::ui::features::reader) fn ai_open(&self) -> bool {
        self.ai.is_some()
    }

    /// 关掉弹层。点卡外面、按返回、换章换书都走这里。
    pub(in crate::ui::features::reader) fn close_ai(&mut self, cx: &mut Context<Self>) {
        if self.ai.take().is_some() {
            cx.notify();
        }
    }

    /// 选区菜单那一格「问 AI」。菜单先收，锚点拍在菜单此刻落地的**顶边中点** ——
    /// 与翻译那一格同一个点，读者的眼睛不用重找。
    pub fn open_ai_question(&mut self, cx: &mut Context<Self>) {
        let Some(selection) = self.selection.clone() else {
            return;
        };
        let Some(text) = self.selected_text() else {
            return;
        };
        let selected: String = text.trim().to_string();
        if selected.is_empty() {
            return;
        }
        let Some((before, after)) = self.context_around(selection.chapter, &selection.range) else {
            // 排版没跟上，或这一章取不到字：这一格问不成，照实不弹（不是弹一张错误卡）。
            tracing::debug!("问 AI: 取不到这一章的正文，这一击不响应");
            return;
        };
        // 出处两格：书名与「这一段摆在哪」—— 后者走 `hit_place_at`，定版式那本书没有章表，
        // 直接取章名会递一个空串过去（与命中列表那屏同一个缺口）。
        let (book, chapter) = (
            self.title().to_string(),
            self.hit_place_at(selection.chapter, cx),
        );
        let anchor = self
            .menu_bounds
            .get()
            .map(|bounds| gpui_kit::point(bounds.center().x, bounds.top()));
        self.selection_menu_open = false;
        let prompt = selection_prompt(&Asked {
            book: &book,
            chapter: &chapter,
            selected: &selected,
            before: &before,
            after: &after,
        });
        let ask = Ask::Explain;
        self.ask_ai(ask, selected, prompt, anchor, cx);
    }

    /// 换问法：载荷是弹层自己存的那份，只换系统提示词，重新问一遍。
    pub fn set_ai_ask(&mut self, ask: Ask, cx: &mut Context<Self>) {
        let Some(state) = self.ai.as_ref() else {
            return;
        };
        let (source, prompt, anchor) = (state.source.clone(), state.prompt.clone(), state.anchor);
        self.ask_ai(ask, source, prompt, anchor, cx);
    }

    /// 两个入口（开弹、换问法）都汇到这里：现建 provider、顶号、挂后台一趟。
    fn ask_ai(
        &mut self,
        ask: Ask,
        source: String,
        prompt: String,
        anchor: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = configured(&ConfigHolder::translation(cx), &ConfigHolder::ai(cx))
        else {
            return;
        };
        let label = provider.label();
        let system = ask.system(&answer_locale(cx));
        self.ai_seq += 1;
        let seq = self.ai_seq;
        self.ai = Some(AiState {
            seq,
            ask,
            label,
            anchor,
            status: AiStatus::Pending,
            source: source.clone(),
            prompt: prompt.clone(),
        });
        let http = folio_core::net::remote::ureq::shared();
        let handle: TaskHandle<AiOutcome> = resident::pool().submit_with(Priority::Interactive, {
            move |_| {
                let got = provider.ask(&*http, &system, &prompt)?;
                Ok((got.text, got.completion_tokens))
            }
        });
        let (forward, events) = async_channel::unbounded();
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
                    .update(cx, |this, cx| this.ai_event(event, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// 后台那趟回来了。只认当前那一号：晚到的旧结果没有观众。
    fn ai_event(&mut self, event: TaskEvent<AiOutcome>, cx: &mut Context<Self>) {
        let Some(state) = self.ai.as_mut() else {
            return;
        };
        if state.seq != self.ai_seq {
            return;
        }
        match event {
            TaskEvent::Finished(Ok((text, tokens))) => {
                // 日志只记长度，不记正文与答案：那可能就是读者正在读的那一页。
                tracing::info!(chars = text.chars().count(), "问 AI: 到手");
                state.status = AiStatus::Done { text, tokens };
            }
            TaskEvent::Finished(Err(err)) => {
                tracing::warn!(error = %err, "问 AI: 那一趟失败");
                state.status = AiStatus::Failed(err.to_string());
            }
            _ => {}
        }
        cx.notify();
    }

    /// 选区前后的上下文（章内归一化文本的字符偏移，与锚点同一把尺）。
    ///
    /// 取不到字的那几种（排版没跟上那一帧、定版式没有字层）一律 `None`：
    /// 拿着上一章的字去问，比不问更坏。
    fn context_around(
        &self,
        chapter: usize,
        range: &std::ops::Range<usize>,
    ) -> Option<(String, String)> {
        let budget = folio_core::net::ai::CONTEXT_BUDGET;
        let map = if let Some(book) = self.fixed() {
            book.page_text(chapter)?.text_map()
        } else {
            let doc = self.book()?.span_doc(chapter)?;
            if !doc.is_current() {
                return None;
            }
            doc.text_map().clone()
        };
        let from = range.start.saturating_sub(budget);
        let to = (range.end + budget).min(map.chars());
        Some((map.slice(from..range.start), map.slice(range.end..to)))
    }
}

/// 回答语言取**界面语言**，不另设一格。理由与翻译那条相反：翻译的目标语是读者的目的地
/// （他要读成哪种语言），而解释是给正在读的他自己的 —— 界面语言就是那一种。
fn answer_locale(cx: &App) -> String {
    ConfigHolder::language(cx).locale.code().to_string()
}

#[cfg(test)]
mod tests {
    use super::ReaderSession;
    use crate::ui::services::config::ConfigHolder;
    use folio_core::{Config, TranslationEngine};
    use gpui_kit::{App, TestAppContext};

    /// 「这一格出不出现」与「按下去问得出去」必须是**同一份判据**（`net::ai::configured`）。
    ///
    /// 分成两套的后果很具体：界面上摆一颗按下去只会报错的按钮，或者反过来 —— 端点明明
    /// 填全了却没有入口。默认必须是「没有这一格」：`ai.enabled` 的默认值 false 就是这条。
    #[gpui_kit::test]
    fn the_ai_cell_appears_only_with_a_filled_endpoint(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            assert!(
                !ReaderSession::ai_available(cx),
                "新装的机器不出现任何 AI 入口（哪怕读者待会儿会去填端点）"
            );

            ConfigHolder::edit_ai(cx, |ai| ai.enabled = true);
            assert!(
                !ReaderSession::ai_available(cx),
                "只拨了开关：地址还是出厂那份、key 没填 ⇒ 不许把正文发去一个读者没配过的服务"
            );

            fill_endpoint(cx, "https://api.example/v1", "translate-model", "");
            assert!(
                ReaderSession::ai_available(cx),
                "开了 + 读者自己填的地址 ⇒ 这一格出现"
            );

            // 模型覆盖：翻译那一档的模型清空，AI 自己填了一只 —— 仍然问得出。
            fill_endpoint(cx, "https://api.example/v1", "", "big-model");
            assert!(
                ReaderSession::ai_available(cx),
                "AI 那一格填了模型就够，不该逼读者替翻译也填一遍"
            );

            // 两处模型都空：不知道该问哪只，退回「没有这一格」。
            fill_endpoint(cx, "https://api.example/v1", "", "");
            assert!(!ReaderSession::ai_available(cx));
        });
    }

    /// 把端点那一档填成给定的三个值（地址 / 翻译用的模型 / AI 覆盖的模型）。
    /// 地址与密钥住在翻译那一档，AI 只带模型覆盖 —— 这条测试顺带钉住那个形状。
    fn fill_endpoint(cx: &mut App, base: &str, translation_model: &str, ai_model: &str) {
        ConfigHolder::edit_translation(cx, |translation| {
            translation.engine = TranslationEngine::OpenAi;
            translation.openai.base_url = base.to_string();
            translation.openai.model = translation_model.to_string();
        });
        ConfigHolder::edit_ai(cx, |ai| {
            ai.enabled = true;
            ai.model = ai_model.to_string();
        });
    }
}
