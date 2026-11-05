//! 选区查词典：把选中的词交给本地 StarDict，弹层里显回释义。
//!
//! 形状与选区翻译同一条：判断与状态迁移在这一层，元素树归阅读页的
//! `reading_page::dictionary_card`。差别只在数据源 —— 这里不出网，查的是读者导入的
//! 本地词典；没有后台池时（宿主测试）当场查完，有池时交给 `Interactive` 档。
//!
//! 结果按 `seq` 顶号：连着点两次，晚到的旧结果在入口被丢掉，屏幕上只留最新那份。

use folio_core::dict::normalize_query;
use folio_core::infra::task::{Priority, TaskError, TaskEvent, TaskHandle};
use gpui_kit::{AppContext as _, Context, Pixels, Point, point};

use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::services::dictionary::{self as service, Hit};
use crate::ui::services::resident;

/// 查词弹层的一格状态。
#[derive(Clone, Debug)]
pub(in crate::ui::features::reader) struct DictionaryState {
    pub(in crate::ui::features::reader) seq: u64,
    /// 弹层自己的权威源文：选区在升起那一刻就收了，换词典不用再读一次选区。
    pub(in crate::ui::features::reader) query: String,
    /// 锚点（窗口坐标）：菜单顶边中点，与译文、问 AI 用的是同一个点。
    pub(in crate::ui::features::reader) anchor: Option<Point<Pixels>>,
    pub(in crate::ui::features::reader) status: DictionaryStatus,
}

#[derive(Clone, Debug)]
pub(in crate::ui::features::reader) enum DictionaryStatus {
    Pending,
    Done { hits: Vec<Hit> },
    Failed(String),
}

impl ReaderSession {
    /// 查词弹层开着吗。阅读页拿它报「身上压着东西」与「返回键先收谁」。
    pub(in crate::ui::features::reader) fn dictionary_open(&self) -> bool {
        self.dictionary.is_some()
    }

    /// 选区菜单那格「词典」：把当前选区交给本地词典。
    ///
    /// 菜单先收 —— 与翻译同一条：菜单与弹层是前后脚的两层，叠着会互相挡摆位。
    pub fn open_dictionary(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.selected_text() else {
            return;
        };
        let query = normalize_query(&text);
        if query.is_empty() {
            return;
        }
        let anchor = self
            .menu_bounds
            .get()
            .map(|bounds| point(bounds.center().x, bounds.top()));
        self.selection_menu_open = false;
        let sources = service::enabled_sources(cx);
        self.dictionary_seq += 1;
        let seq = self.dictionary_seq;
        self.dictionary = Some(DictionaryState {
            seq,
            query: query.clone(),
            anchor,
            status: DictionaryStatus::Pending,
        });
        cx.notify();
        if sources.is_empty() {
            self.finish_dictionary(seq, Ok(Vec::new()), cx);
            return;
        }
        // 有作业池就搬过去；宿主测试没有池，当场查完（与 `image_atlas` 同一条）。
        if let Some(pool) = resident::pool_maybe() {
            let handle: TaskHandle<Vec<Hit>> = pool.submit_with(Priority::Interactive, move |_| {
                service::lookup_in(&sources, &query).map_err(|err| -> TaskError { Box::new(err) })
            });
            let (forward, events) = async_channel::unbounded::<TaskEvent<Vec<Hit>>>();
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
                    let _ = this.update(cx, |this, cx| this.dictionary_event(seq, event, cx));
                }
            })
            .detach();
        } else {
            let result = service::lookup_in(&sources, &query).map_err(|err| err.to_string());
            self.finish_dictionary(seq, result, cx);
        }
    }

    /// 关掉弹层。点卡外面、按返回键、换章换书都走这里。
    pub(in crate::ui::features::reader) fn close_dictionary(&mut self, cx: &mut Context<Self>) {
        if self.dictionary.take().is_some() {
            cx.notify();
        }
    }

    fn dictionary_event(&mut self, seq: u64, event: TaskEvent<Vec<Hit>>, cx: &mut Context<Self>) {
        match event {
            TaskEvent::Finished(Ok(hits)) => self.finish_dictionary(seq, Ok(hits), cx),
            TaskEvent::Finished(Err(err)) => self.finish_dictionary(seq, Err(err.to_string()), cx),
            _ => {}
        }
    }

    fn finish_dictionary(
        &mut self,
        seq: u64,
        result: Result<Vec<Hit>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.dictionary.as_mut() else {
            return;
        };
        if state.seq != seq {
            return;
        }
        state.status = match result {
            Ok(hits) => DictionaryStatus::Done { hits },
            Err(reason) => DictionaryStatus::Failed(reason),
        };
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use folio_core::dict::normalize_query;

    #[test]
    fn the_query_is_trimmed_before_it_reaches_a_dictionary() {
        assert_eq!(normalize_query("  「Apple」， "), "Apple");
        assert_eq!(normalize_query("   "), "");
    }
}
