//! 听书的**遥控面**：通知 / 锁屏 / 耳机线控的命令落点，与「现在念到哪儿」的通知同步。
//!
//! 单开一间是因为 `speech.rs` 已经贴着行数基线；这一间收的都是「会话 ↔ 系统媒体面」
//! 的那一层胶水。命令队列在 `services::speech_remote`（JNI 线程收下、外壳渲染帧转来），
//! 这里只管把命令翻成状态机的动作、把状态翻成通知。

use std::ops::Range;

use gpui_kit::Context;

use crate::ui::features::reader::model::session::ReaderSession;
use crate::ui::features::reader::model::speech::Listen;

impl ReaderSession {
    /// 通知 / 锁屏 / 耳机上的传输命令落在这里（词表见 `services::speech_remote`）：
    /// "play" / "pause" 翻转播停，"stop" 收摊。"next" / "previous" 不在这 —— 那两颗要
    /// 页上的跟读脚本，归 `ReadingPage::apply_speech_commands` 分流。
    pub(in crate::ui::features::reader) fn apply_remote_speech(
        &mut self,
        commands: &[&str],
        cx: &mut Context<Self>,
    ) {
        for command in commands {
            match *command {
                "play" | "pause" => self.toggle_speech(cx),
                "stop" => {
                    self.stop_speech();
                    cx.notify();
                }
                _ => {}
            }
        }
    }

    /// 通知与锁屏同步一份「现在念到哪儿」：开口 / 换句 / 暂停 / 收摊各喊一声（状态机里
    /// 那三个转折点）。播着就刷新 MediaStyle 通知（前台服务顺带保住进程不被回收），
    /// 收摊就整份撤掉。通知权限的首次请求在这里捎带（后台线程、不等人）。
    pub(in crate::ui::features::reader) fn sync_speech_remote(&self) {
        crate::ui::platform::request_notifications();
        let (playing, chapter, range) = match self.speech.state.clone() {
            Listen::Idle => {
                crate::ui::platform::speech_remote_hide();
                return;
            }
            Listen::Playing { chapter, range } => (true, chapter, range),
            Listen::Paused { chapter, range } => (false, chapter, range),
        };
        let subtitle = self.speech_paragraph_text(chapter, &range);
        crate::ui::platform::speech_remote_show(playing, &self.title(), &subtitle);
    }

    /// 通知副题的那段文本：整段截断到 120 字。取不到就空着 —— 通知只剩标题也不碍事。
    fn speech_paragraph_text(&self, chapter: usize, range: &Range<usize>) -> String {
        self.book()
            .and_then(|book| book.span_doc(chapter))
            .and_then(|doc| {
                doc.paragraph_range(range.start)
                    .map(|span| doc.text_map().slice(span))
            })
            .map(|text| text.chars().take(120).collect())
            .unwrap_or_default()
    }
}
