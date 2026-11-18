use std::rc::Rc;

use folio_core::Speech;
use folio_core::speech::{Status, VoiceKey};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{AnyElement, ElementId, IntoElement as _, ParentElement as _, Styled as _, div};

use super::super::*;
use crate::ui::components::muted_label;
use crate::ui::services::speech::{self, Change};

#[path = "speech_editor.rs"]
mod editor;

pub(in crate::ui::features::settings) fn speech_page(cx: &App) -> Page {
    if !crate::ui::services::activation::premium(cx) {
        return page(
            tr(cx, "page.speech"),
            IconName::BookHeadphones,
            vec![super::membership::gate_group("membership.gate_speech", cx)],
        );
    }
    page(
        tr(cx, "page.speech"),
        IconName::BookHeadphones,
        vec![
            group(
                tr(cx, "speech_settings.engine"),
                vec![
                    Row::custom(|_, cx| engine_status(cx)),
                    Row::custom_titled(tr(cx, "speech_settings.narrator"), |_, cx| {
                        voice_picker(
                            "speech-narrator",
                            ConfigHolder::speech(cx).narrator_voice,
                            tr(cx, "speech_settings.system_voice"),
                            !can_edit(cx),
                            |voice, cx| {
                                let language = ConfigHolder::speech(cx).language;
                                speech::apply(Change::Narrator { voice, language }, cx);
                            },
                            cx,
                        )
                        .into_any_element()
                    })
                    .keywords(&["voice", "narrator", "音色", "旁白"]),
                    Row::custom_titled(tr(cx, "speech_settings.language"), |_, cx| {
                        let language = ConfigHolder::speech(cx).language;
                        Button::new("speech-language")
                            .label(if language.is_empty() {
                                tr(cx, "speech_settings.automatic").to_string()
                            } else {
                                language
                            })
                            .disabled(!can_edit(cx))
                            .on_click(|_, window, cx| editor::open_language(window, cx))
                            .into_any_element()
                    }),
                ],
            ),
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
                        |cx| ConfigHolder::speech(cx).auto_scroll,
                        |val, cx| ConfigHolder::edit_speech(cx, |p| p.auto_scroll = val),
                    )
                    .description(tr(cx, "field.speech_auto_scroll.desc"))
                    .keywords(&["auto", "scroll", "follow", "自动", "翻页", "跟随"]),
                ],
            ),
            group(
                tr(cx, "speech_settings.roles"),
                vec![Row::custom(|_, cx| roles(cx)).keywords(&[
                    "role",
                    "character",
                    "dialogue",
                    "角色",
                    "对白",
                ])],
            ),
        ],
    )
}

fn can_edit(cx: &App) -> bool {
    speech::handle(cx).is_some_and(|entity| {
        let state = entity.read(cx);
        state.ready()
            && !state.busy()
            && state
                .snapshot()
                .is_some_and(|s| s.voices.iter().any(|v| v.is_local()))
    })
}

fn engine_status(cx: &App) -> AnyElement {
    let handle = speech::handle(cx);
    let state = handle.as_ref().map(|entity| entity.read(cx));
    let snapshot = state.and_then(|state| state.snapshot());
    let busy = state.is_some_and(|state| state.busy())
        || snapshot.is_some_and(|snapshot| matches!(snapshot.status, Status::Initializing));
    let key = match snapshot.map(|s| &s.status) {
        Some(Status::Initializing) => "speech_settings.preparing",
        Some(Status::Ready) if state.is_some_and(|s| s.busy()) => "speech_settings.applying",
        Some(Status::Ready)
            if snapshot.is_some_and(|s| !s.voices.iter().any(|voice| voice.is_local())) =>
        {
            "speech_settings.no_voices"
        }
        Some(Status::Ready) => "speech_settings.ready",
        Some(Status::Failed(_)) => "speech_settings.failed",
        Some(Status::Unavailable) => "speech_settings.unavailable",
        None if state.is_some_and(|s| s.started()) => "speech_settings.unavailable",
        None => "speech_settings.not_prepared",
    };
    let error = state.and_then(|s| s.error()).or_else(|| {
        snapshot.and_then(|s| match &s.status {
            Status::Failed(reason) => Some(reason.as_str()),
            _ => s.last_error.as_deref(),
        })
    });
    let mut content = v_flex().w_full().gap_2().child(
        h_flex()
            .w_full()
            .gap_3()
            .child(div().flex_1().min_w_0().child(tr(cx, key)))
            .child(
                Button::new("speech-refresh")
                    .label(tr(cx, "speech_settings.refresh").to_string())
                    .disabled(busy)
                    .on_click(|_, _, cx| speech::refresh(cx)),
            ),
    );
    content = content.child(muted_label(tr(cx, "speech_settings.local_only"), cx));
    if let Some(error) = error {
        content = content.child(div().text_sm().text_color(cx.theme().danger).child(tf(
            cx,
            "speech_settings.failure_reason",
            &[("reason", &error)],
        )));
    }
    content.into_any_element()
}

pub(super) fn voice_label(
    current: Option<&VoiceKey>,
    inherited: SharedString,
    cx: &App,
) -> SharedString {
    let Some(current) = current else {
        return inherited;
    };
    let handle = speech::handle(cx);
    let voice = handle
        .as_ref()
        .and_then(|entity| entity.read(cx).snapshot())
        .and_then(|snapshot| snapshot.voices.iter().find(|voice| &voice.key == current));
    match voice {
        Some(voice) if voice.is_local() => format!("{} · {}", voice.name, voice.language).into(),
        _ => tf(
            cx,
            "speech_settings.saved_unavailable",
            &[("voice", &current.id)],
        )
        .into(),
    }
}

pub(super) fn voice_picker(
    id: impl Into<ElementId>,
    current: Option<VoiceKey>,
    inherited: SharedString,
    disabled: bool,
    on_change: impl Fn(Option<VoiceKey>, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let label = voice_label(current.as_ref(), inherited.clone(), cx);
    let voices = speech::handle(cx)
        .and_then(|entity| entity.read(cx).snapshot().cloned())
        .map(|snapshot| {
            snapshot
                .voices
                .into_iter()
                .filter(|voice| voice.is_local())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let on_change = Rc::new(on_change);
    Button::new(id)
        .label(label.to_string())
        .dropdown_caret(true)
        .disabled(disabled)
        .dropdown_menu(move |menu, _, _| {
            let change = on_change.clone();
            let mut menu = menu.item(
                PopupMenuItem::new(inherited.clone())
                    .checked(current.is_none())
                    .on_click(move |_, _, cx| change(None, cx)),
            );
            for voice in &voices {
                let change = on_change.clone();
                let key = voice.key.clone();
                menu = menu.item(
                    PopupMenuItem::new(format!(
                        "{} · {} · {}",
                        voice.name, voice.language, voice.key.engine
                    ))
                    .checked(current.as_ref() == Some(&key))
                    .on_click(move |_, _, cx| change(Some(key.clone()), cx)),
                );
            }
            menu.scrollable(true)
        })
}

fn roles(cx: &App) -> AnyElement {
    let settings = ConfigHolder::speech(cx);
    let disabled = !can_edit(cx);
    let mut content = v_flex()
        .w_full()
        .gap_3()
        .child(muted_label(tr(cx, "speech_settings.roles_help"), cx));
    for role in settings.roles {
        let id = role.id.clone();
        let editing = role.clone();
        content = content.child(
            h_flex()
                .w_full()
                .gap_2()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().child(role.name))
                        .child(muted_label(role.prefix, cx))
                        .child(muted_label(
                            voice_label(role.voice.as_ref(), tr(cx, "speech_settings.inherit"), cx),
                            cx,
                        )),
                )
                .child(
                    Button::new(SharedString::from(format!("speech-edit-{id}")))
                        .small()
                        .ghost()
                        .label(tr(cx, "speech_settings.edit").to_string())
                        .disabled(disabled)
                        .on_click(move |_, window, cx| {
                            editor::open_role(Some(editing.clone()), window, cx)
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("speech-remove-{id}")))
                        .small()
                        .ghost()
                        .label(tr(cx, "speech_settings.remove").to_string())
                        .disabled(disabled)
                        .on_click(move |_, _, cx| {
                            speech::apply(Change::RemoveRole(id.clone()), cx);
                        }),
                ),
        );
    }
    content
        .child(
            Button::new("speech-role-add")
                .label(tr(cx, "speech_settings.add").to_string())
                .disabled(disabled || !speech::can_add_role(cx))
                .on_click(|_, window, cx| editor::open_role(None, window, cx)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::voice_label;
    use crate::ui::features::settings::{SettingsPage, SettingsView};
    use crate::ui::services::config::ConfigHolder;
    use crate::ui::services::speech::{
        entity,
        tests::{install, settle, voice_key},
    };
    use folio_core::speech::{Snapshot, Status};
    use folio_core::{Config, SpeechRole};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AppContext as _, Bounds, TestAppContext, WindowBounds, WindowOptions, px, size,
    };

    #[gpui_kit::test]
    fn speech_page_disables_voice_and_role_actions_without_local_candidates(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let (handle, _) = cx.update(|cx| {
            let mut config = Config::default();
            config.membership.lifetime = true;
            ConfigHolder::install(config, cx);
            entity(cx).update(cx, |state, _| {
                state.set_snapshot_for_test(Snapshot {
                    status: Status::Ready,
                    ..Snapshot::default()
                })
            });
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(1000.), px(1000.)),
                    ))),
                    ..WindowOptions::default()
                },
                cx,
                |_, cx| cx.new(|cx| SettingsView::new(SettingsPage::Speech, cx)),
            )
            .unwrap()
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("speech-narrator").disabled(), Some(true));
            assert_eq!(window.find("speech-role-add").disabled(), Some(true));
            window.click("speech-role-add", cx);
            assert!(window.try_find("speech-role-name").is_none());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn speech_role_form_saves_text_and_voice_then_removes_by_identity(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.background_executor.allow_parking();
        let (engine, _) = cx.update(install);
        let (handle, _) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(1000.), px(1000.)),
                    ))),
                    ..WindowOptions::default()
                },
                cx,
                |_, cx| cx.new(|cx| SettingsView::new(SettingsPage::Speech, cx)),
            )
            .unwrap()
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("speech-role-add", cx);
            window.click("speech-role-name", cx);
            window.input("阿青", cx);
            window.click("speech-role-prefix", cx);
            window.input("阿青：", cx);
            window.click("speech-role-voice", cx);
            assert!(window.within("popup-menu").try_find(3usize).is_none());
            window.within("popup-menu").click(2usize, cx);
            window.click("speech-editor-save", cx);
        })
        .unwrap();
        settle(cx);
        let role = cx.update(|cx| {
            let roles = ConfigHolder::speech(cx).roles;
            assert_eq!(roles.len(), 1);
            let role = roles[0].clone();
            assert_eq!(role.name, "阿青");
            assert_eq!(role.prefix, "阿青：");
            assert_eq!(
                role.voice,
                Some(voice_key("second")),
                "online voice must not appear in the picker"
            );
            role
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click(format!("speech-remove-{}", role.id), cx);
        })
        .unwrap();
        settle(cx);
        cx.update(|cx| assert!(ConfigHolder::speech(cx).roles.is_empty()));
        engine.shutdown();
    }

    #[gpui_kit::test]
    fn speech_keeps_missing_voice_preference_and_role_form_cancels_without_writing(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        cx.background_executor.allow_parking();
        let (engine, _) = cx.update(install);
        cx.update(|cx| {
            ConfigHolder::edit_speech(cx, |speech| {
                speech.narrator_voice = Some(voice_key("removed"))
            });
            let shown = voice_label(Some(&voice_key("removed")), "Automatic".into(), cx);
            assert!(shown.contains("removed"));
            assert_eq!(
                ConfigHolder::speech(cx).narrator_voice,
                Some(voice_key("removed"))
            );
            ConfigHolder::edit_speech(cx, |speech| {
                speech.roles.push(SpeechRole {
                    id: "stable-role".into(),
                    name: "Original".into(),
                    prefix: "Original:".into(),
                    voice: None,
                })
            });
        });
        let (handle, _) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Default::default(),
                        size(px(1000.), px(1000.)),
                    ))),
                    ..WindowOptions::default()
                },
                cx,
                |_, cx| cx.new(|cx| SettingsView::new(SettingsPage::Speech, cx)),
            )
            .unwrap()
        });
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("speech-edit-stable-role", cx);
            window.click("speech-role-name", cx);
            window.input("unsaved", cx);
            window.press("escape", cx);
            assert_eq!(ConfigHolder::speech(cx).roles[0].name, "Original");
            assert_eq!(
                ConfigHolder::speech(cx).narrator_voice,
                Some(voice_key("removed"))
            );
        })
        .unwrap();
        engine.shutdown();
    }
}
