use std::time::Duration;

use folio_core::SpeechRole;
use folio_core::speech::{self, Engine, Snapshot, Status, VoiceKey};
use gpui_kit::{App, AppContext as _, Context, Entity, Global, Task};

use super::config::ConfigHolder;
use super::l10n::tr;

const MAX_ROLES: usize = 32;

pub struct SpeechState {
    engine: Option<Engine>,
    snapshot: Option<Snapshot>,
    started: bool,
    busy: bool,
    error: Option<String>,
    generation: u64,
    _poll: Option<Task<()>>,
}

impl SpeechState {
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }
    pub fn started(&self) -> bool {
        self.started
    }
    pub fn busy(&self) -> bool {
        self.busy
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn ready(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|s| matches!(s.status, Status::Ready))
    }

    #[cfg(test)]
    pub fn set_engine_for_test(&mut self, engine: Engine) {
        self.snapshot = Some(engine.snapshot());
        self.engine = Some(engine);
        self.started = true;
    }

    #[cfg(test)]
    pub fn set_snapshot_for_test(&mut self, snapshot: Snapshot) {
        self.snapshot = Some(snapshot);
        self.started = true;
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.started = true;
        self.error = None;
        self.generation = self.generation.wrapping_add(1);
        self.engine = speech::prewarm();
        self.snapshot = self.engine.as_ref().map(Engine::snapshot);
        self._poll = self.engine.clone().map(|engine| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(500))
                        .await;
                    let snapshot = engine.snapshot();
                    if this
                        .update(cx, |state, cx| {
                            if state
                                .snapshot
                                .as_ref()
                                .is_none_or(|old| old.revision != snapshot.revision)
                            {
                                state.snapshot = Some(snapshot);
                                cx.notify();
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
        });
        cx.notify();
    }
}

struct SpeechHolder(Entity<SpeechState>);
impl Global for SpeechHolder {}

pub fn handle(cx: &App) -> Option<Entity<SpeechState>> {
    cx.try_global::<SpeechHolder>()
        .map(|holder| holder.0.clone())
}

pub fn entity(cx: &mut App) -> Entity<SpeechState> {
    if let Some(entity) = handle(cx) {
        return entity;
    }
    let entity = cx.new(|_| SpeechState {
        engine: None,
        snapshot: None,
        started: false,
        busy: false,
        error: None,
        generation: 0,
        _poll: None,
    });
    cx.set_global(SpeechHolder(entity.clone()));
    entity
}

/// 由首帧后的宿主钩子调用；失败后仅显式重试，不自行重建后端。
pub fn prewarm(cx: &mut App) {
    if !super::activation::premium(cx) {
        return;
    }
    entity(cx).update(cx, |state, cx| {
        if !state.started {
            state.start(cx);
        }
    });
}

pub fn engine(cx: &mut App) -> Option<Engine> {
    prewarm(cx);
    handle(cx).and_then(|entity| entity.read(cx).engine.clone())
}

pub fn refresh(cx: &mut App) {
    if !super::activation::premium(cx) {
        return;
    }
    entity(cx).update(cx, |state, cx| {
        if state.busy {
            return;
        }
        if state.ready() {
            if let Some(engine) = &state.engine {
                engine.refresh_voices();
            }
        } else {
            state.start(cx);
        }
    });
}

#[derive(Clone)]
pub enum Change {
    Narrator {
        voice: Option<VoiceKey>,
        language: String,
    },
    Role(SpeechRole),
    RemoveRole(String),
}

impl Change {
    fn selection(&self, current: &folio_core::Speech) -> Option<VoiceKey> {
        match self {
            Self::Narrator { voice, .. } => voice.clone(),
            Self::Role(role) => role
                .voice
                .clone()
                .or_else(|| current.narrator_voice.clone()),
            Self::RemoveRole(_) => current.narrator_voice.clone(),
        }
    }

    fn apply(self, speech: &mut folio_core::Speech) {
        match self {
            Self::Narrator { voice, language } => {
                speech.narrator_voice = voice;
                speech.language = language;
            }
            Self::Role(role) => {
                if let Some(old) = speech.roles.iter_mut().find(|old| old.id == role.id) {
                    *old = role;
                } else {
                    speech.roles.push(role);
                }
            }
            Self::RemoveRole(id) => speech.roles.retain(|role| role.id != id),
        }
    }
}

pub fn can_add_role(cx: &App) -> bool {
    ConfigHolder::speech(cx).roles.len() < MAX_ROLES
}

pub fn apply(change: Change, cx: &mut App) -> async_channel::Receiver<Result<(), String>> {
    let (tx, rx) = async_channel::bounded(1);
    let current = ConfigHolder::speech(cx);
    let state = entity(cx);
    state.update(cx, |state, cx| {
        let error = if !super::activation::premium(cx) || !state.ready() {
            Some(tr(cx, "speech_settings.unavailable").to_string())
        } else if state.busy {
            Some(tr(cx, "speech_settings.applying").to_string())
        } else if let Change::Role(role) = &change {
            let is_new = !current.roles.iter().any(|old| old.id == role.id);
            if role.name.trim().is_empty()
                || role.prefix.trim().is_empty()
                || role.id.is_empty()
                || current
                    .roles
                    .iter()
                    .any(|old| old.id != role.id && old.prefix == role.prefix)
            {
                Some(tr(cx, "speech_settings.role_invalid").to_string())
            } else if is_new && current.roles.len() >= MAX_ROLES {
                Some(tr(cx, "speech_settings.role_limit").to_string())
            } else {
                None
            }
        } else {
            None
        };
        if let Some(error) = error {
            state.error = Some(error.clone());
            let _ = tx.try_send(Err(error));
            cx.notify();
            return;
        }
        let Some(engine) = state.engine.clone() else {
            let error = tr(cx, "speech_settings.unavailable").to_string();
            state.error = Some(error.clone());
            let _ = tx.try_send(Err(error));
            cx.notify();
            return;
        };
        let language = match &change {
            Change::Narrator { language, .. } => language.trim().to_string(),
            _ => current.language.clone(),
        };
        let ack = engine.select_voice(change.selection(&current), language);
        let generation = state.generation;
        state.busy = true;
        state.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = ack
                .recv()
                .await
                .unwrap_or_else(|_| Err("Speech backend disconnected".into()));
            let result = this
                .update(cx, |state, cx| {
                    if state.generation != generation {
                        return Err("Speech backend changed".to_string());
                    }
                    state.busy = false;
                    state.snapshot = Some(engine.snapshot());
                    let result = result
                        .map(|_| ConfigHolder::edit_speech(cx, |speech| change.apply(speech)));
                    state.error = result.as_ref().err().cloned();
                    cx.notify();
                    result
                })
                .unwrap_or_else(|_| Err("Speech settings closed".into()));
            let _ = tx.send(result).await;
        })
        .detach();
    });
    rx
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    use super::{Change, apply, entity};
    use crate::ui::services::config::ConfigHolder;
    use folio_core::speech::{Backend, Engine, Status, Voice, VoiceKey};
    use folio_core::{Config, Speech, SpeechRole};
    use gpui_kit::{App, TestAppContext};

    pub fn voice_key(id: &str) -> VoiceKey {
        VoiceKey {
            backend: "fixture".into(),
            engine: "local-engine".into(),
            id: id.into(),
        }
    }

    struct LocalBackend {
        fail: Arc<AtomicBool>,
    }

    impl Backend for LocalBackend {
        fn voices(&mut self) -> Result<Vec<Voice>, String> {
            Ok([("first", false), ("second", false), ("cloud", true)]
                .into_iter()
                .map(|(id, network)| Voice {
                    key: voice_key(id),
                    name: id.into(),
                    language: "zh-Hant-TW".into(),
                    network_required: Some(network),
                    installed: Some(true),
                })
                .collect())
        }
        fn default_voice(&self) -> Option<VoiceKey> {
            Some(voice_key("first"))
        }
        fn select_voice(&mut self, _: &VoiceKey) -> Result<(), String> {
            if self.fail.load(Ordering::SeqCst) {
                Err("fixture rejected voice".into())
            } else {
                Ok(())
            }
        }
        fn speak(&mut self, _: u64, _: &str) -> Result<(), String> {
            panic!("settings must remain silent")
        }
        fn stop(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn set_rate(&mut self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_pitch(&mut self, _: f32) -> Result<(), String> {
            Ok(())
        }
        fn shutdown(&mut self) {}
    }

    pub fn install(cx: &mut App) -> (Engine, Arc<AtomicBool>) {
        let mut config = Config::default();
        config.membership.lifetime = true;
        ConfigHolder::install(config, cx);
        let fail = Arc::new(AtomicBool::new(false));
        let factory_fail = fail.clone();
        let engine = Engine::with_backend(Arc::new(move |_| {
            Ok(Box::new(LocalBackend {
                fail: factory_fail.clone(),
            }))
        }));
        for _ in 0..200 {
            if engine.snapshot().status == Status::Ready {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(engine.snapshot().status, Status::Ready);
        entity(cx).update(cx, |state, _| state.set_engine_for_test(engine.clone()));
        (engine, fail)
    }

    pub fn settle(cx: &mut TestAppContext) {
        for _ in 0..200 {
            cx.background_executor.run_until_parked();
            if !cx.update(|cx| entity(cx).read(cx).busy()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("voice selection did not finish");
    }

    #[gpui_kit::test]
    fn voice_ack_controls_persistence_and_failed_selection_keeps_preference(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        cx.background_executor.allow_parking();
        let (engine, fail) = cx.update(install);
        let first = cx.update(|cx| {
            apply(
                Change::Narrator {
                    voice: Some(voice_key("second")),
                    language: "zh-Hant-TW".into(),
                },
                cx,
            )
        });
        cx.update(|cx| assert!(ConfigHolder::speech(cx).narrator_voice.is_none()));
        settle(cx);
        assert_eq!(first.try_recv().unwrap(), Ok(()));
        cx.update(|cx| {
            assert_eq!(
                ConfigHolder::speech(cx).narrator_voice,
                Some(voice_key("second"))
            );
            assert_eq!(ConfigHolder::speech(cx).language, "zh-Hant-TW");
        });
        fail.store(true, Ordering::SeqCst);
        let second = cx.update(|cx| {
            apply(
                Change::Narrator {
                    voice: Some(voice_key("first")),
                    language: "en-US".into(),
                },
                cx,
            )
        });
        settle(cx);
        assert!(second.try_recv().unwrap().is_err());
        cx.update(|cx| {
            assert_eq!(
                ConfigHolder::speech(cx).narrator_voice,
                Some(voice_key("second"))
            );
            assert_eq!(ConfigHolder::speech(cx).language, "zh-Hant-TW");
            assert!(entity(cx).read(cx).error().is_some());
        });
        engine.shutdown();
    }

    #[test]
    fn role_edits_follow_identity_and_removal_keeps_other_roles() {
        let mut speech = Speech::default();
        for id in ["first", "second"] {
            Change::Role(SpeechRole {
                id: id.into(),
                name: id.into(),
                prefix: format!("{id}："),
                voice: None,
            })
            .apply(&mut speech);
        }
        Change::Role(SpeechRole {
            id: "second".into(),
            name: "Renamed".into(),
            prefix: "Bob:".into(),
            voice: None,
        })
        .apply(&mut speech);
        Change::RemoveRole("first".into()).apply(&mut speech);
        assert_eq!(speech.roles.len(), 1);
        assert_eq!(speech.roles[0].id, "second");
        assert_eq!(speech.roles[0].name, "Renamed");
        assert_eq!(speech.roles[0].prefix, "Bob:");
    }
}
