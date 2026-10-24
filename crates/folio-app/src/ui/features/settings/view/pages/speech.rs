//! 「听书」页：语速、音高与念到哪翻到哪。
//!
//! 语速/音高的上下限是**语义档**（0.5–2.0，常速 1.0），直接引用 [`Speech`] 的区间
//! 常量而不抄第二份 —— 那对常量同时是配置归一的判据（`infra::config::Speech::normalized`），
//! 滑条与它各说各话就会出现「滑条给得出、配置存不下」的一格。也不写引擎报的那档
//! （Android 0.1–10.0）：那一档问不得（`min_*`/`max_*` 在 Android 上是 `todo!()`），
//! 语义档换算成引擎档住在 `folio_core::speech::gate`，这一页只认语义值。
//!
//! 各项**即时生效**，没有「应用」按钮：语速/音高下一段开口就换挡（会话每段随发
//! `SetRate`/`SetPitch`，见 `reader/model/speech.rs`），auto_scroll 下一回 `Done`
//! 就按新档走。

use folio_core::Speech;

use super::super::*;

pub(in crate::ui::features::settings) fn speech_page(cx: &App) -> Page {
    page(
        tr(cx, "page.speech"),
        IconName::BookHeadphones,
        vec![
            group(
                tr(cx, "group.speech_voice"),
                vec![
                    number(
                        cx,
                        NumberSpec {
                            title: "field.speech_rate",
                            description: "field.speech_rate.desc",
                            keywords: &["rate", "speed", "tts", "语速", "速度"],
                            min: *Speech::RATE_RANGE.start(),
                            max: *Speech::RATE_RANGE.end(),
                            step: 0.05,
                        },
                        |cx| ConfigHolder::speech(cx).rate,
                        |v, cx| ConfigHolder::edit_speech(cx, |p| p.rate = v),
                    ),
                    number(
                        cx,
                        NumberSpec {
                            title: "field.speech_pitch",
                            description: "field.speech_pitch.desc",
                            keywords: &["pitch", "tone", "音调", "音高"],
                            min: *Speech::PITCH_RANGE.start(),
                            max: *Speech::PITCH_RANGE.end(),
                            step: 0.05,
                        },
                        |cx| ConfigHolder::speech(cx).pitch,
                        |v, cx| ConfigHolder::edit_speech(cx, |p| p.pitch = v),
                    ),
                ],
            ),
            group(
                tr(cx, "group.speech_reading"),
                vec![
                    Row::toggle(
                        tr(cx, "field.speech_auto_scroll"),
                        |cx: &App| ConfigHolder::speech(cx).auto_scroll,
                        |val: bool, cx| ConfigHolder::edit_speech(cx, |p| p.auto_scroll = val),
                    )
                    .description(tr(cx, "field.speech_auto_scroll.desc"))
                    .keywords(&["auto", "scroll", "follow", "自动", "翻页", "跟随"]),
                ],
            ),
        ],
    )
}
