use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use folio_core::SpeechRole;
use folio_core::speech::VoiceKey;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, WindowExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Window, div,
};

use crate::ui::components::muted_label;
use crate::ui::services::config::ConfigHolder;
use crate::ui::services::l10n::tr;
use crate::ui::services::speech::{self, Change};

enum Mode {
    Role(String),
    Language,
}

struct SpeechEditor {
    mode: Mode,
    name: Entity<InputState>,
    text: Entity<InputState>,
    voice: Option<VoiceKey>,
    busy: bool,
    closed: bool,
    error: Option<String>,
    _watch: Subscription,
}

pub(super) fn open_role(role: Option<SpeechRole>, window: &mut Window, cx: &mut App) {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let role = role.unwrap_or_else(|| {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        SpeechRole {
            id: format!(
                "role-{time:x}-{:x}",
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ),
            ..SpeechRole::default()
        }
    });
    open(
        Mode::Role(role.id),
        role.name,
        role.prefix,
        role.voice,
        window,
        cx,
    );
}

pub(super) fn open_language(window: &mut Window, cx: &mut App) {
    open(
        Mode::Language,
        String::new(),
        ConfigHolder::speech(cx).language,
        None,
        window,
        cx,
    );
}

fn open(
    mode: Mode,
    name: String,
    text: String,
    voice: Option<VoiceKey>,
    window: &mut Window,
    cx: &mut App,
) {
    let editor = cx.new(|cx| {
        let state = speech::entity(cx);
        SpeechEditor {
            mode,
            name: cx.new(|cx| InputState::new(window, cx).default_value(name)),
            text: cx.new(|cx| InputState::new(window, cx).default_value(text)),
            voice,
            busy: false,
            closed: false,
            error: None,
            _watch: cx.observe(&state, |_, _, cx| cx.notify()),
        }
    });
    let first = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        let form = editor.read(cx);
        let is_role = matches!(form.mode, Mode::Role(_));
        let busy = form.busy;
        let focus = if is_role { &form.name } else { &form.text };
        let focus = focus.read(cx).focus_handle(cx);
        if first.replace(false) {
            focus.focus(window, cx);
        }
        let title = tr(
            cx,
            if is_role {
                "speech_settings.role_title"
            } else {
                "speech_settings.language"
            },
        );
        let confirm = editor.clone();
        let closing = editor.clone();
        dialog
            .title(title)
            .child(editor.clone())
            .overlay_closable(!busy)
            .close_button(!busy)
            .on_ok(move |_, window, cx| {
                confirm.update(cx, |form, cx| form.save(window, cx));
                false
            })
            .on_cancel(move |_, _, cx| !closing.read(cx).busy)
            .on_close({
                let closing = editor.clone();
                move |_, _, cx| closing.update(cx, |form, _| form.closed = true)
            })
    });
}

impl SpeechEditor {
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let text = self.text.read(cx).value().trim().to_string();
        let change = match &self.mode {
            Mode::Role(id) => Change::Role(SpeechRole {
                id: id.clone(),
                name: self.name.read(cx).value().trim().to_string(),
                prefix: text,
                voice: self.voice.clone(),
            }),
            Mode::Language => Change::Narrator {
                voice: ConfigHolder::speech(cx).narrator_voice,
                language: text,
            },
        };
        let result = speech::apply(change, cx);
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = result
                .recv()
                .await
                .unwrap_or_else(|_| Err("Speech backend disconnected".into()));
            let _ = this.update_in(cx, |form, window, cx| {
                if form.closed {
                    return;
                }
                form.busy = false;
                match result {
                    Ok(()) => window.close_dialog(cx),
                    Err(error) => form.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for SpeechEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = v_flex().w_full().gap_3();
        if matches!(self.mode, Mode::Role(_)) {
            content = content
                .child(div().child(tr(cx, "speech_settings.role_name")))
                .child(
                    Input::new(&self.name)
                        .id("speech-role-name")
                        .w_full()
                        .disabled(self.busy),
                )
                .child(div().child(tr(cx, "speech_settings.prefix")))
                .child(
                    Input::new(&self.text)
                        .id("speech-role-prefix")
                        .w_full()
                        .disabled(self.busy),
                )
                .child(muted_label(tr(cx, "speech_settings.prefix_help"), cx));
            let weak = cx.weak_entity();
            content = content
                .child(div().child(tr(cx, "speech_settings.voice")))
                .child(super::voice_picker(
                    "speech-role-voice",
                    self.voice.clone(),
                    tr(cx, "speech_settings.inherit"),
                    self.busy,
                    move |voice, cx| {
                        let _ = weak.update(cx, |form, cx| {
                            form.voice = voice;
                            cx.notify();
                        });
                    },
                    cx,
                ));
        } else {
            content = content
                .child(
                    Input::new(&self.text)
                        .id("speech-language-code")
                        .w_full()
                        .disabled(self.busy),
                )
                .child(muted_label(tr(cx, "speech_settings.language_help"), cx));
        }
        if self.busy {
            content = content.child(muted_label(tr(cx, "speech_settings.applying"), cx));
        }
        if let Some(error) = &self.error {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        content.child(
            DialogFooter::new()
                .child(
                    Button::new("speech-editor-cancel")
                        .label(tr(cx, "speech_settings.cancel").to_string())
                        .disabled(self.busy)
                        .on_click(cx.listener(|form, _, window, cx| {
                            if !form.busy {
                                form.closed = true;
                                window.close_dialog(cx);
                            }
                        })),
                )
                .child(
                    Button::new("speech-editor-save")
                        .primary()
                        .label(tr(cx, "speech_settings.save").to_string())
                        .disabled(self.busy)
                        .on_click(cx.listener(|form, _, window, cx| form.save(window, cx))),
                ),
        )
    }
}
