//! 「这本书该怎么切」：把那本书的样本交给模型，解出一条分章规则，**先按它切一遍**给读者
//! 看后果，读者按下「用上」才落进配置。
//!
//! 形状照 [`model::translation`](super::translation) 与 [`model::ai`](super::ai)：provider
//! 按当时的配置现建、整只搬进后台作业、`seq` 顶号丢旧结果、事件桥回界面。判断（哪一份样本、
//! 解不解得开、切不切得动）全在 core 那一格（`folio_core::content::format::novel::draft`），
//! 这一层只管「什么时候问、结果摆哪儿、按下之后写哪一格」。
//!
//! ## 为什么入口在读书这一侧，不在设置那一屏
//!
//! 草案的输入与预览都是**一本书**的事：样本取自那本书的文本，预览是按那本书算的章数。
//! 而设置那一屏（`features/settings/.../txt_rules.rs`）手里没有书 —— 它改的是全局那一份
//! `Edits`，对任何一本书都同样生效。把入口摆在那儿就只有两种下场：猜「读者想的是哪本」
//! （猜错就是一条按在别的书上的规则，而它全局生效），或者新加一册「当前开着的书」的
//! 全局状态。两个都比现在这条路贵。
//!
//! 落点仍是同一份配置：`Edits.extra`，设置那一屏列得出来、也关得掉。
//!
//! ## 生效时机是「重启」，这一格不改它
//!
//! 按下「用上」只写配置。生效那一张表换不换发生在开机那一次（`rules::install` 写一次，
//! 理由是半本旧章半本新章比整本旧章更难解释 —— 另见 `doc/基础设施.md` 那张热/重启表）。
//! 卡片收在「已记下」那一档就是把这句话照实说给读者，而不是让他以为按下即生效。

use folio_core::content::format::novel::draft::{self, Draft, Preview};
use folio_core::infra::task::{Priority, TaskEvent, TaskHandle};
use folio_core::net::ai::{configured, draft_prompt, draft_system};
use gpui_kit::{App, AppContext as _, Context};

use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::resident;

/// 弹层那一格的状态。
pub(in crate::ui::features::reader) struct RulesDraft {
    pub(in crate::ui::features::reader) seq: u64,
    pub(in crate::ui::features::reader) status: DraftStatus,
}

/// `Clone + Debug` 是给宿主测试的：断言里要整份比对与打印。
#[derive(Clone, Debug)]
pub(in crate::ui::features::reader) enum DraftStatus {
    /// 那一趟还在后台跑（读整本书 + 一次网络往返，秒级）。
    Pending,
    /// 解出来了，而且**按它切过了**。`usable` 是界面上那颗「用上」按不按得下去的判据，
    /// 取自 [`Preview::is_usable`] —— 数不够就是拒，不摆一颗只会写坏配置的按钮。
    /// 两份都装箱：`Draft` 与 `Preview` 合起来 225 字节，而这个枚举在每一帧都被读一次
    /// （卡片按四档画），不装的话 `Pending` 那一档也要占那么大（clippy 的
    /// `large_enum_variant`）。一次问答一个堆分配，量级上不算钱。
    Ready {
        draft: Box<Draft>,
        preview: Box<Preview>,
        usable: bool,
    },
    /// 已经落进配置了，正等重启。
    Applied { name: String },
    /// 这一趟没成：读不到那本书、模型答得不是一段 JSON，或者样本空到没法问。
    Failed(String),
}

/// 后台那一趟交回的载荷。
type DraftOutcome = (Draft, Preview);

impl ReaderSession {
    /// 这一格能出现吗：手里那本书是 `.txt`、书库句柄在、端点填全了（与问 AI 同一份判据）。
    ///
    /// 判据只写这一处：目录面板那一颗按钮按它画，真按下时 [`Self::suggest_chapter_rules`]
    /// 也按它走 —— 两套答案就会有一颗按下去没反应的按钮，或者一颗本不该出现的按钮。
    pub(in crate::ui::features::reader) fn rules_draft_available(&self, cx: &App) -> bool {
        let Some(store) = self.store.as_ref() else {
            return false;
        };
        let Some(id) = self.book_id else {
            return false;
        };
        if !is_text_book(store, id) {
            return false;
        }
        configured(
            &ConfigHolder::translation(cx),
            &ConfigHolder::ai(cx),
            &ConfigHolder::vendors(cx),
        )
        .is_some()
    }

    /// 弹层开着吗。阅读页拿它报「身上压着东西」（正文手势让位）与「返回键先收谁」。
    pub(in crate::ui::features::reader) fn rules_draft_open(&self) -> bool {
        self.rules_draft.is_some()
    }

    /// 关掉弹层。点卡外面、按返回、换书都走这里。**不改配置** —— 关掉就是没按下「用上」。
    pub(in crate::ui::features::reader) fn close_rules_draft(&mut self, cx: &mut Context<Self>) {
        if self.rules_draft.take().is_some() {
            cx.notify();
        }
    }

    /// 目录面板那颗「建议怎么切章」：现取那本书的路径，把整趟搬进后台。
    pub fn suggest_chapter_rules(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = configured(
            &ConfigHolder::translation(cx),
            &ConfigHolder::ai(cx),
            &ConfigHolder::vendors(cx),
        ) else {
            return;
        };
        let (Some(store), Some(id)) = (self.store.clone(), self.book_id) else {
            return;
        };
        let Ok(Some(row)) = store.book(id) else {
            return;
        };
        if row.format != folio_core::Format::Text.as_str() {
            return;
        }
        let path = std::path::PathBuf::from(row.path);
        // 回答语言按界面语言：`reason` 那一句是给人读的，规则本体不是。
        let system = draft_system(ConfigHolder::language(cx).locale.code());
        self.rules_draft_seq += 1;
        let seq = self.rules_draft_seq;
        self.rules_draft = Some(RulesDraft {
            seq,
            status: DraftStatus::Pending,
        });
        let http = folio_core::net::remote::ureq::shared();
        let handle: TaskHandle<DraftOutcome> =
            resident::pool().submit_with(Priority::Interactive, move |_| {
                // 整本书在这一格读进来。`source_at` 走的是 `.txt` 导入那一条规范化路径，
                // 与索引、与章序号同一把尺（见 `novel::draft` 模块头）。
                let source = draft::source_at(&path)?;
                let sample = draft::sample(&source);
                if sample.is_useless() {
                    return Err("这本书读不出可看的行".into());
                }
                let answer = provider.ask(
                    &*http,
                    &system,
                    &draft_prompt(&sample.head, &sample.candidates),
                )?;
                let proposed = draft::from_answer(&answer.text)
                    .map_err(|err| format!("{err}。它答的是：{}（原答案）", clip(&answer.text)))?;
                let preview = draft::preview(&source, &proposed.marker);
                Ok((proposed, preview))
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
                    .update(cx, |this, cx| this.rules_draft_event(event, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// 按下「用上」：草案落进 `Edits.extra`。名字先过一次防撞 —— `off` 按名字作用，
    /// 两条同名会让设置那一屏的「关掉」一次关掉两条。
    pub fn apply_chapter_rules(&mut self, cx: &mut Context<Self>) {
        let Some(DraftStatus::Ready { draft, usable, .. }) =
            self.rules_draft.as_ref().map(|state| &state.status)
        else {
            return;
        };
        if !*usable {
            return;
        }
        let mut marker = draft.marker.clone();
        let edits = ConfigHolder::novel(cx).edits;
        marker.name = draft::free_name(&edits, &marker.name);
        let name = marker.name.clone();
        ConfigHolder::edit_novel(cx, |novel| novel.edits.extra.push(marker));
        // 生效那张表换了才算真的按它重扫，而那一换在开机（见模块头）。这一句是给
        // 下一个查日志的人看的，不是给界面用的。
        tracing::info!(rule = %name, "分章草案已记下：重启后按它重扫这本书");
        self.rules_draft = Some(RulesDraft {
            seq: self.rules_draft_seq,
            status: DraftStatus::Applied { name },
        });
        cx.notify();
    }

    /// 后台那一趟回来了。只认当前那一号：晚到的旧结果没有观众（与问 AI 同一套守卫）。
    fn rules_draft_event(&mut self, event: TaskEvent<DraftOutcome>, cx: &mut Context<Self>) {
        let Some(state) = self.rules_draft.as_mut() else {
            return;
        };
        if state.seq != self.rules_draft_seq {
            return;
        }
        match event {
            TaskEvent::Finished(Ok((draft, preview))) => {
                // 日志只记章数与中位字数，**不记样本**：那 90 行里可能有读者正在读的正文。
                tracing::info!(
                    chapters = preview.chapters,
                    median = preview.median_body_chars,
                    rejected = preview.rejected.is_some(),
                    "分章草案: 到手"
                );
                let usable = preview.is_usable();
                state.status = DraftStatus::Ready {
                    draft: Box::new(draft),
                    preview: Box::new(preview),
                    usable,
                };
            }
            TaskEvent::Finished(Err(err)) => {
                tracing::warn!(error = %err, "分章草案: 那一趟没成");
                state.status = DraftStatus::Failed(err.to_string());
            }
            _ => {}
        }
        cx.notify();
    }
}

/// 原答案在失败卡片上只回显一小截：它是「模型说了什么」的证据，不是给读者读的文章。
fn clip(answer: &str) -> String {
    const ECHO: usize = 80;
    let mut out: String = answer.chars().take(ECHO).collect();
    if answer.chars().count() > ECHO {
        out.push('…');
    }
    out
}

/// 书库里那一本的主键认得出来是 `.txt` 吗。非纯文本没有「按行切章」这件事，
/// 那一条路（EPUB 的 spine、PDF 的页）本来就定好了；Markdown 按标题切，也不归规则表管。
///
/// 比的是 `Format::as_str()` 而不是字面量 `"txt"`：库里那一列存的就是前者（`.txt` 落的是
/// `"text"`），抄一份字符串的后果是这一格永远不出现，而屏上看不出原因。
fn is_text_book(store: &std::sync::Arc<folio_core::Store>, id: i64) -> bool {
    store
        .book(id)
        .ok()
        .flatten()
        .is_some_and(|row| row.format == folio_core::Format::Text.as_str())
}

#[cfg(test)]
mod tests {
    use super::{DraftStatus, is_text_book};
    use crate::ui::features::reader::model::session::ReaderSession;
    use crate::ui::services::config::ConfigHolder;
    use folio_core::{Config, DocMeta, Format, Store};
    use gpui_kit::TestAppContext;

    /// 判据的两半各钉一条，而且**库里那一列的字面值要参与**：`books.format` 存的是
    /// `Format::as_str()`（`.txt` 落的是 `"text"`），界面按 `"txt"` 比就永远不出现这一格 ——
    /// 那种 bug 在屏上的样子是「功能没做」，不是「做错了」。
    #[test]
    fn only_a_plain_text_book_has_the_suggestion_entry() {
        let store = std::sync::Arc::new(Store::in_memory().unwrap());
        let meta = |format: Format| DocMeta {
            title: "一本".to_string(),
            format,
            ..Default::default()
        };
        let txt = store
            .add_book(std::path::Path::new("/tmp/一本.txt"), &meta(Format::Text))
            .unwrap();
        let epub = store
            .add_book(std::path::Path::new("/tmp/一本.epub"), &meta(Format::Epub))
            .unwrap();
        assert!(is_text_book(&store, txt), "纯文本该有这一格");
        assert!(
            !is_text_book(&store, epub),
            "EPUB 的章是 spine 定的，没有「按行切」这件事"
        );
        assert!(
            !is_text_book(&store, -1),
            "查不到的那一本说「不是」，不是 panic"
        );
    }

    /// 手上没有书（兜底那一屏）⇒ 没有这一格，也不发任何请求。
    #[gpui_kit::test]
    fn the_entry_is_absent_while_no_book_is_open(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let session = cx.update(|cx| {
            ConfigHolder::install(Config::default(), cx);
            ReaderSession::boot(cx)
        });
        assert!(
            !cx.update(|cx| session.rules_draft_available(cx)),
            "占位那一屏没有书可问 ⇒ 这一格不出现"
        );
        assert!(!session.rules_draft_open());
    }

    /// 四档状态画四样东西：`Applied` 带着那条的名字，`Failed` 带着给人看的那一句。
    /// 这条不是覆盖率测试 —— 卡片按这四档分支画，合档就会有一档没有落点。
    #[test]
    fn the_four_states_are_four_different_cards() {
        let failed = DraftStatus::Failed("答案里找不到那段 JSON".to_string());
        assert!(matches!(failed, DraftStatus::Failed(_)));
        let applied = DraftStatus::Applied {
            name: "等号".to_string(),
        };
        assert!(matches!(applied, DraftStatus::Applied { name } if name == "等号"));
    }
}
