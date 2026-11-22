//! 听书屏底部那两块：语速一格 + 章进度那一排（倒退 / 进度 / 快进）。
//!
//! 单独成子模块的理由只有一条：`reading_page.rs` 在 FOLIO-GOV-004 的债务基线上，只许变短，
//! 而这两块是那一屏里最独立的两段 —— 只吃几个读数、两颗钮与一条进度。语速从顶栏搬下来也
//! 是同一次整理：倍率与「这一章念到哪儿」都在调节播报，摆成上下两行才是一条线，顶栏就留给
//! 「关掉 / 封面 / 文稿」。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex};
use gpui_kit::{
    Anchor, AnyElement, App, Context, Entity, IntoElement, ParentElement, SharedString, Styled,
    Window, px,
};

use folio_core::speech::{Voice, VoiceKey};

use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::{tf, tr};

use super::{LISTEN_FALLBACK_CPS, LISTEN_TEXT_W, ListenSentence, ReadingPage, sentence_index};

/// 语速菜单的档。设置页那条连续滑条仍可按 0.05 微调；这里给的是「一眼能挑」的几档 ——
/// 进度条上方那颗钮打开的就是这张小单子，不逼读者去设置页找。
const LISTEN_RATES: [f32; 7] = [0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

/// 听书屏「快进」一次跳多少秒。朗读是**句级**的，落不进半句里：跳过 N 秒的落点是
/// 「起点至少走过 N 秒的第一句」，秒数按实测语速折成字数（见 [`super::ReaderSession::speech_chars_per_sec`]
/// 与 [`skip_target`]）。一章剩下的时长不够 N 秒时就停在最后一句，跨章仍归 `Done` 接力。
pub(super) const LISTEN_SKIP_SECONDS: f32 = 15.0;

/// 「倒退」一次跳多少秒。读者按这一颗是为了重听刚过去的那一两句，五秒比十五秒更好找；
/// 落点规则与快进同一条（方向反过来，走到章首还不够就停在第一句）。
const LISTEN_REWIND_SECONDS: f32 = 5.0;

/// 语速的显示名：尾零去掉，小数保留（`1.00` 念成 `1×`，`0.75` 一字不动）。
fn rate_label(rate: f32) -> String {
    let mut text = format!("{rate:.2}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    format!("{text}×")
}

/// 音色与语速那一行：左端挑「谁在念」，右端挑「念多快」，压在章进度条上方。
///
/// 两格都只写配置：`speak_sentence` 每句现读 `narrator_voice` 与 `rate`，下一句开口就带上，
/// 不另开命令通路（与设置页同一条口径）。
///
/// 单子用 [`PopupMenu`]：选中一档由 `checked` 画勾（不靠颜色猜），点外面 / 按返回的收场
/// 归组件自己。锚在**右上** = 往下开、右对齐，钮贴着那条进度的右端，单子不该往上飞出屏。
pub(super) fn tuning_row(page: Entity<ReadingPage>, cx: &App) -> AnyElement {
    let rate = ConfigHolder::speech(cx).rate;
    let speed_label: SharedString = rate_label(rate).into();
    let rate_name = tr(cx, "listen.rate");
    let rate_a11y = format!("{rate_name} {speed_label}");
    let anchor_page = page.clone();
    let speed_menu = move |menu: PopupMenu, _: &mut Window, cx: &mut Context<PopupMenu>| {
        let current = ConfigHolder::speech(cx).rate;
        let mut menu = menu.min_w(px(112.));
        for option in LISTEN_RATES {
            let page = anchor_page.clone();
            menu = menu.item(
                PopupMenuItem::new(rate_label(option))
                    .checked((current - option).abs() < 0.001)
                    .on_click(move |_, _, cx| {
                        // 下一句开口时 `speak_sentence` 会随发 `SetRate`：这里只改配置，
                        // 不另开一条命令通路（与设置页同一条口径）。
                        ConfigHolder::edit_speech(cx, |speech| speech.rate = option);
                        page.update(cx, |_, cx| cx.notify());
                    }),
            );
        }
        menu
    };
    let speed = Button::new("listen-rate")
        .ghost()
        .small()
        .rounded_full()
        .icon(IconName::Gauge)
        .label(speed_label)
        .tooltip(rate_name.clone().to_string())
        .accessibility_label(rate_a11y)
        .dropdown_menu_with_anchor(Anchor::TopRight, speed_menu);
    h_flex()
        .w_full()
        .max_w(px(LISTEN_TEXT_W))
        .mx_auto()
        .justify_between()
        .gap_3()
        .pb_2()
        .child(voice_button(page, cx))
        .child(speed)
        .into_any_element()
}

/// 音色那一格：本机已安装的离线音色单子。
///
/// 只列 `is_local()` 的音色 —— 听书一路是离线的，线上音色在这一屏没有落点（与设置页
/// 那一格同一条口径）。当前音色不在这份单子里（换过引擎 / 卸掉了）时钮上照旧念它，并说
/// 「暂不可用」：悄悄换成别的音色比报出来更糟。
fn voice_button(page: Entity<ReadingPage>, cx: &App) -> AnyElement {
    let current = ConfigHolder::speech(cx).narrator_voice.clone();
    let voices: Vec<Voice> = crate::ui::services::speech::handle(cx)
        .and_then(|entity| entity.read(cx).snapshot().cloned())
        .map(|snapshot| {
            snapshot
                .voices
                .into_iter()
                .filter(Voice::is_local)
                .collect()
        })
        .unwrap_or_default();
    let label = voice_label(current.as_ref(), &voices, cx);
    let name = tr(cx, "speech_settings.voice");
    let a11y = format!("{name} {label}");
    let pick = page;
    let checked = current.clone();
    let items = voices;
    Button::new("listen-voice")
        .ghost()
        .small()
        .rounded_full()
        .icon(IconName::AudioLines)
        .label(label.to_string())
        .tooltip(name.to_string())
        .accessibility_label(a11y)
        .dropdown_menu(move |menu, _, cx| {
            let page = pick.clone();
            let mut menu = menu.min_w(px(200.)).item(
                PopupMenuItem::new(tr(cx, "speech_settings.system_voice"))
                    .checked(checked.is_none())
                    .on_click(move |_, _, cx| {
                        ConfigHolder::edit_speech(cx, |speech| speech.narrator_voice = None);
                        page.update(cx, |_, cx| cx.notify());
                    }),
            );
            for voice in &items {
                let page = pick.clone();
                let key = voice.key.clone();
                menu = menu.item(
                    PopupMenuItem::new(format!("{} · {}", voice.name, voice.language))
                        .checked(checked.as_ref() == Some(&key))
                        .on_click(move |_, _, cx| {
                            let key = key.clone();
                            ConfigHolder::edit_speech(cx, |speech| {
                                speech.narrator_voice = Some(key)
                            });
                            page.update(cx, |_, cx| cx.notify());
                        }),
                );
            }
            menu.scrollable(true)
        })
        .into_any_element()
}

/// 钮上那句音色名：认得出来就写名字（太长截断，这一行右边还站着语速），认不出就照实说
/// 「暂不可用」并把存着的那个 id 带上。
fn voice_label(current: Option<&VoiceKey>, voices: &[Voice], cx: &App) -> SharedString {
    let Some(current) = current else {
        return tr(cx, "speech_settings.system_voice");
    };
    match voices.iter().find(|voice| &voice.key == current) {
        Some(voice) => short_voice(&voice.name).into(),
        None => tf(
            cx,
            "speech_settings.saved_unavailable",
            &[("voice", &current.id)],
        )
        .into(),
    }
}

/// 音色名截到 14 个字。再长就把语速那颗钮挤出这一行 —— 工具提示里有全名。
fn short_voice(name: &str) -> String {
    const MAX: usize = 14;
    if name.chars().count() <= MAX {
        return name.to_string();
    }
    let kept: String = name.chars().take(MAX).collect();
    format!("{kept}…")
}

/// 章进度那一排：倒退、进度、快进。
///
/// 进度按**字符**算（调用方给的就是这个数），两颗钮各自跳固定的秒数 —— 倒退五秒在左、
/// 快进十五秒在右，拇指落在哪一边都只剩一种可能。
pub(super) fn chapter_row(
    progress: f32,
    position: SharedString,
    page: Entity<ReadingPage>,
    can_rewind: bool,
    can_forward: bool,
    cx: &App,
) -> AnyElement {
    let rewind_label = tf(
        cx,
        "listen.backward",
        &[("seconds", &format!("{LISTEN_REWIND_SECONDS:.0}"))],
    );
    let forward_label = tf(
        cx,
        "listen.forward",
        &[("seconds", &format!("{LISTEN_SKIP_SECONDS:.0}"))],
    );
    let rewind_page = page.clone();
    let forward_page = page;
    h_flex()
        .w_full()
        .max_w(px(LISTEN_TEXT_W))
        .mx_auto()
        .items_center()
        .gap_3()
        .pb_4()
        .child(
            Button::new("listen-skip-backward")
                .ghost()
                .icon(IconName::Rewind)
                .tooltip(rewind_label.clone().to_string())
                .accessibility_label(rewind_label)
                .disabled(!can_rewind)
                .on_click(move |_, _, cx| rewind_page.update(cx, skip_backward)),
        )
        .child(
            h_flex().flex_1().min_w_0().child(
                Progress::new("listen-chapter-progress")
                    .small()
                    .value(progress)
                    .accessibility_label(position),
            ),
        )
        .child(
            Button::new("listen-skip-forward")
                .ghost()
                .icon(IconName::FastForward)
                .tooltip(forward_label.clone().to_string())
                .accessibility_label(forward_label)
                .disabled(!can_forward)
                .on_click(move |_, _, cx| forward_page.update(cx, skip_forward)),
        )
        .into_any_element()
}

/// 「快进 N 秒」的落点：从这一句**开头**起算，跳过句子的估算时长，取第一句起点已经走过
/// N 秒的那一句；章末剩下的不够就停在最后一句，已经在最后一句就 `None`。
pub(super) fn skip_target(
    sentences: &[ListenSentence],
    from: usize,
    seconds: f32,
    chars_per_sec: f32,
) -> Option<usize> {
    if from + 1 >= sentences.len() {
        return None;
    }
    let cps = chars_per_sec.max(0.1);
    let mut elapsed = 0.0;
    for (ix, sentence) in sentences.iter().enumerate().skip(from) {
        // `from` 自己那一句是起点，不算「已经走过的路」：这一格只在再看下一句时判。
        if ix > from && elapsed >= seconds {
            return Some(ix);
        }
        elapsed += sentence.chapter.len() as f32 / cps;
    }
    Some(sentences.len() - 1)
}

/// 「倒退 N 秒」的落点：从 `from` 往前一句一句数，走到累计时长够 N 秒的那一句；走到章首
/// 还不够就停在第一句。`from` 已经在第一句（或越界）时是 `None` —— 没有可退的。
fn rewind_target(
    sentences: &[ListenSentence],
    from: usize,
    seconds: f32,
    chars_per_sec: f32,
) -> Option<usize> {
    if from == 0 || from >= sentences.len() {
        return None;
    }
    let cps = chars_per_sec.max(0.1);
    let mut elapsed = 0.0;
    let mut target = 0;
    for ix in (0..from).rev() {
        target = ix;
        elapsed += sentences[ix].chapter.len() as f32 / cps;
        if elapsed >= seconds {
            break;
        }
    }
    Some(target)
}

/// 当前这一句的位置与实测语速折出的字数/秒。两个跳转口（进 / 退）共用这一份折算。
fn cursor(page: &ReadingPage, cx: &App) -> Option<(usize, usize, f32)> {
    let (chapter, range) = page.reader.read(cx).speech_range()?;
    let script = page.listen_script.clone()?;
    if script.chapter != chapter {
        return None;
    }
    let ix = sentence_index(&script.sentences, range.start)?;
    let rate = ConfigHolder::speech(cx).rate.max(0.01);
    let chars_per_sec = page
        .reader
        .read(cx)
        .speech_chars_per_sec()
        .unwrap_or(LISTEN_FALLBACK_CPS)
        * rate;
    Some((ix, chapter, chars_per_sec))
}

/// 快进 N 秒：落到 [`skip_target`] 算出的那一句，从它的开头念。走的是与上一句/下一句同一条
/// `speak_from` —— 暂停时按下去也从那一句开口，三个跳转口行为一致。
pub(super) fn skip_forward(page: &mut ReadingPage, cx: &mut Context<ReadingPage>) {
    let Some((ix, chapter, chars_per_sec)) = cursor(page, cx) else {
        return;
    };
    let Some(script) = page.listen_script.clone() else {
        return;
    };
    let Some(target) = skip_target(&script.sentences, ix, LISTEN_SKIP_SECONDS, chars_per_sec)
    else {
        return;
    };
    speak_at(page, cx, chapter, &script.sentences[target]);
}

/// 倒退 N 秒：规则同 [`skip_forward`]，落点由 [`rewind_target`] 给。
pub(super) fn skip_backward(page: &mut ReadingPage, cx: &mut Context<ReadingPage>) {
    let Some((ix, chapter, chars_per_sec)) = cursor(page, cx) else {
        return;
    };
    let Some(script) = page.listen_script.clone() else {
        return;
    };
    let Some(target) = rewind_target(&script.sentences, ix, LISTEN_REWIND_SECONDS, chars_per_sec)
    else {
        return;
    };
    speak_at(page, cx, chapter, &script.sentences[target]);
}

/// 从这一句的开头开口。手动划过文稿的那一格跟着清掉：读者按的是「回到这儿」，
/// 不该过四秒又被自动跟读抢回去。
fn speak_at(
    page: &mut ReadingPage,
    cx: &mut Context<ReadingPage>,
    chapter: usize,
    at: &ListenSentence,
) {
    let want = at.chapter.clone();
    page.listen_manual_at = None;
    page.reader
        .update(cx, |reader, cx| reader.speak_from(chapter, want, cx));
}

#[cfg(test)]
mod tests {
    use super::{LISTEN_SKIP_SECONDS, ListenSentence, rewind_target, skip_target};

    fn script(count: usize) -> Vec<ListenSentence> {
        (0..count)
            .map(|ix| ListenSentence {
                chapter: ix * 20..ix * 20 + 20,
                script: 0..0,
            })
            .collect()
    }

    /// 音色名太长要截：这一行右边还站着语速，长名字会把那颗钮挤出去。
    #[test]
    fn long_voice_names_are_shortened() {
        assert_eq!(super::short_voice("Chinese"), "Chinese");
        assert_eq!(
            super::short_voice("一二三四五六七八九十一二三四"),
            "一二三四五六七八九十一二三四"
        );
        assert_eq!(
            super::short_voice("一二三四五六七八九十一二三四五"),
            "一二三四五六七八九十一二三四…"
        );
    }

    /// 语速的显示名：尾零去掉，小数保留（`1.00` 念成 `1×`，`0.75` 一字不动）。
    #[test]
    fn rate_labels_drop_trailing_zeros() {
        assert_eq!(super::rate_label(1.0), "1×");
        assert_eq!(super::rate_label(0.75), "0.75×");
        assert_eq!(super::rate_label(1.25), "1.25×");
        assert_eq!(super::rate_label(0.5), "0.5×");
        assert_eq!(super::rate_label(2.0), "2×");
    }

    /// 快进的落点：六句、每句 20 字；4 字/秒 → 每句 5 秒，15 秒正好三句。
    #[test]
    fn skipping_forward_lands_on_the_first_sentence_past_the_seconds() {
        let sentences = script(6);
        assert_eq!(
            skip_target(&sentences, 0, LISTEN_SKIP_SECONDS, 4.0),
            Some(3)
        );
        // 倍率翻倍（每句 2.5 秒）：15 秒要走六句，章里不够 → 停在最后一句。
        assert_eq!(skip_target(&sentences, 0, 15.0, 8.0), Some(5));
        // 从中间起算同理：第 2 句起走满 15 秒落到第 5 句；第 3 句起不够，也停在它。
        assert_eq!(skip_target(&sentences, 2, 15.0, 4.0), Some(5));
        assert_eq!(skip_target(&sentences, 3, 15.0, 4.0), Some(5));
        // 已经在最后一句：没有可跳的（按钮当场禁用）。
        assert_eq!(skip_target(&sentences, 5, 15.0, 4.0), None);
        // 空表 / 越界起点不 panic。
        assert_eq!(skip_target(&[], 0, 15.0, 4.0), None);
        assert_eq!(skip_target(&sentences, 99, 15.0, 4.0), None);
    }

    /// 倒退的落点：五秒 = 一句（4 字/秒、每句 20 字）。章首再往前没有了就停在第一句，
    /// 已经在第一句时 `None`（按钮禁用）。
    #[test]
    fn rewinding_lands_on_the_sentence_the_seconds_reach() {
        let sentences = script(6);
        assert_eq!(rewind_target(&sentences, 3, 5.0, 4.0), Some(2));
        // 五秒要走一句多一点：从第 5 句退到第 4 句就够。
        assert_eq!(rewind_target(&sentences, 5, 5.0, 4.0), Some(4));
        // 章首不够五秒：停在第一句，不留在原地。
        assert_eq!(rewind_target(&sentences, 1, 15.0, 4.0), Some(0));
        // 就在第一句 / 空表 / 越界：没有可退的。
        assert_eq!(rewind_target(&sentences, 0, 5.0, 4.0), None);
        assert_eq!(rewind_target(&[], 2, 5.0, 4.0), None);
        assert_eq!(rewind_target(&sentences, 99, 5.0, 4.0), None);
    }
}
