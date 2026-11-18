use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_channel::{Receiver, Sender};

use crate::engine::{Request, Shared, Submission};
use crate::{
    Backend, BackendEvent, BackendFactory, Command, Delivery, Event, Snapshot, Status, VoiceKey,
    resolve_voice,
};
pub(super) fn run(
    factory: BackendFactory,
    input: Receiver<Submission>,
    events: Sender<Delivery>,
    shared: Arc<Shared>,
    initialized: std::sync::mpsc::Sender<()>,
) {
    let (callback_tx, callbacks) = async_channel::unbounded();
    let mut backend = match factory(callback_tx) {
        Ok(backend) => backend,
        Err(error) => {
            shared.update(|snapshot| {
                snapshot.status = Status::Failed(error.clone());
                snapshot.last_error = Some(error);
            });
            let _ = events.try_send(Delivery {
                generation: None,
                event: Event::Ready(false),
            });
            return;
        }
    };
    let voices = backend.voices();
    if shared.stopping.load(Ordering::Acquire) {
        backend.shutdown();
        shared.update(|snapshot| {
            if !matches!(snapshot.status, Status::Failed(_)) {
                snapshot.status = Status::Unavailable;
            }
        });
        return;
    }
    let voices = match voices {
        Ok(voices) => voices,
        Err(error) => {
            backend.shutdown();
            shared.update(|snapshot| {
                snapshot.status = Status::Failed(error.clone());
                snapshot.last_error = Some(error);
            });
            let _ = events.try_send(Delivery {
                generation: None,
                event: Event::Ready(false),
            });
            return;
        }
    };
    let default = backend.default_voice();
    // 与超时观察者同锁发布 Ready，晚到的构造结果不得复活旧代。
    {
        let mut snapshot = shared
            .snapshot
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if shared.stopping.load(Ordering::Acquire) {
            if !matches!(snapshot.status, Status::Failed(_)) {
                snapshot.status = Status::Unavailable;
                snapshot.revision = snapshot.revision.wrapping_add(1);
            }
            drop(snapshot);
            backend.shutdown();
            return;
        }
        snapshot.status = Status::Ready;
        snapshot.voices = voices;
        snapshot.default_voice = default;
        snapshot.selected_voice = None;
        snapshot.revision = snapshot.revision.wrapping_add(1);
    }
    let _ = initialized.send(());
    let _ = events.try_send(Delivery {
        generation: None,
        event: Event::Ready(true),
    });
    let mut worker = Worker {
        backend,
        events,
        shared: shared.clone(),
        active: None,
        next_id: 0,
        current: None,
        preferred: None,
        language: String::new(),
    };
    while !shared.stopping.load(Ordering::Acquire) {
        for _ in 0..32 {
            if shared.stopping.load(Ordering::Acquire) {
                break;
            }
            let Ok(request) = input.try_recv() else {
                break;
            };
            worker.request(request);
        }
        for event in worker.backend.poll_events() {
            worker.callback(event);
        }
        for _ in 0..128 {
            let Ok(event) = callbacks.try_recv() else {
                break;
            };
            worker.callback(event);
        }
        if !input.is_empty() || !callbacks.is_empty() || shared.stopping.load(Ordering::Acquire) {
            continue;
        }
        let wait = if worker.active.is_some() {
            Duration::from_millis(10)
        } else {
            shared.idle_wait
        };
        // unpark 的 token 跨越检查到 park 之间的窗口，命令不会等完整空闲周期。
        std::thread::park_timeout(wait);
    }
    worker.active = None;
    let _ = worker.backend.stop();
    worker.backend.shutdown();
    shared.update(|snapshot| {
        if !matches!(snapshot.status, Status::Failed(_)) {
            snapshot.status = Status::Unavailable;
        }
    });
}

struct Worker {
    backend: Box<dyn Backend>,
    events: Sender<Delivery>,
    shared: Arc<Shared>,
    active: Option<(u64, u64)>,
    next_id: u64,
    current: Option<VoiceKey>,
    preferred: Option<VoiceKey>,
    language: String,
}

impl Worker {
    fn snapshot(&self) -> Snapshot {
        self.shared
            .snapshot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn emit(&self, event: Event, generation: u64) {
        let _ = self.events.try_send(Delivery {
            generation: Some(generation),
            event,
        });
    }

    fn failure(&mut self, error: String, generation: u64) {
        self.active = None;
        let _ = self.backend.stop();
        self.shared
            .update(|snapshot| snapshot.last_error = Some(error.clone()));
        self.emit(Event::Failed(error), generation);
    }

    fn stop(&mut self) -> Result<(), String> {
        self.active = None;
        self.backend.stop()
    }

    fn choose(
        &mut self,
        preferred: Option<&VoiceKey>,
        language: &str,
        explicit: bool,
    ) -> Result<VoiceKey, String> {
        let snapshot = self.snapshot();
        if explicit
            && preferred.is_some_and(|key| {
                !snapshot
                    .voices
                    .iter()
                    .any(|voice| &voice.key == key && voice.is_local())
            })
        {
            return Err("The selected voice is not installed locally".into());
        }
        let key = resolve_voice(
            &snapshot.voices,
            preferred,
            language,
            snapshot.default_voice.as_ref(),
        )
        .map(|voice| voice.key.clone())
        .ok_or_else(|| "No installed offline voice is available for this language".to_string())?;
        if self.current.as_ref() != Some(&key) {
            self.current = None;
            self.shared
                .update(|snapshot| snapshot.selected_voice = None);
            self.backend.select_voice(&key)?;
            self.current = Some(key.clone());
        }
        Ok(key)
    }

    fn speak(
        &mut self,
        text: String,
        preferred: Option<VoiceKey>,
        language: String,
        parameters: Option<(f32, f32)>,
        generation: u64,
    ) -> Result<(), String> {
        self.stop()?;
        if text.is_empty() || text.contains('\0') {
            return Err("Speech text is empty or contains a null character".into());
        }
        if let Some((rate, pitch)) = parameters {
            if !rate.is_finite() || !pitch.is_finite() {
                return Err("Invalid speech parameters".into());
            }
            self.backend.set_rate(rate.clamp(0.5, 2.0))?;
            self.backend.set_pitch(pitch.clamp(0.5, 2.0))?;
        }
        let key = self.choose(preferred.as_ref(), &language, false)?;
        self.shared.update(|snapshot| {
            snapshot.selected_voice = Some(key);
            snapshot.last_error = None;
        });
        self.next_id = self.next_id.wrapping_add(1);
        self.active = Some((self.next_id, generation));
        self.backend.speak(self.next_id, &text)
    }

    fn request(&mut self, submission: Submission) {
        let generation = submission.generation;
        let result = match submission.request {
            Request::Command(Command::Stop) => self.stop(),
            Request::Command(Command::SetRate(value)) => {
                if value.is_finite() {
                    self.backend.set_rate(value.clamp(0.5, 2.0))
                } else {
                    Err("Invalid speech rate".into())
                }
            }
            Request::Command(Command::SetPitch(value)) => {
                if value.is_finite() {
                    self.backend.set_pitch(value.clamp(0.5, 2.0))
                } else {
                    Err("Invalid speech pitch".into())
                }
            }
            Request::Command(Command::Speak { text }) => self.speak(
                text,
                self.preferred.clone(),
                self.language.clone(),
                None,
                generation,
            ),
            Request::Command(Command::SpeakWithVoice {
                text,
                voice,
                language,
                rate,
                pitch,
            }) => self.speak(text, voice, language, Some((rate, pitch)), generation),
            Request::Select(preferred, language, reply) => {
                let result = self
                    .stop()
                    .and_then(|()| self.choose(preferred.as_ref(), &language, true));
                match result {
                    Ok(key) => {
                        self.preferred = preferred;
                        self.language = language;
                        self.shared.update(|snapshot| {
                            snapshot.selected_voice = Some(key.clone());
                            snapshot.last_error = None;
                        });
                        self.emit(Event::VoiceChanged, generation);
                        let _ = reply.try_send(Ok(Some(key)));
                    }
                    Err(error) => {
                        self.failure(error.clone(), generation);
                        let _ = reply.try_send(Err(error));
                    }
                }
                Ok(())
            }
            Request::Refresh => self.refresh(generation),
        };
        if let Err(error) = result {
            self.failure(error, generation);
        }
    }

    fn refresh(&mut self, generation: u64) -> Result<(), String> {
        let voices = self.backend.voices()?;
        let default = self.backend.default_voice();
        let invalid = self.current.as_ref().is_some_and(|key| {
            !voices
                .iter()
                .any(|voice| &voice.key == key && voice.is_local())
        });
        self.shared.update(|snapshot| {
            snapshot.voices = voices;
            snapshot.default_voice = default;
        });
        if invalid {
            self.stop()?;
            self.current = None;
            let preferred = self.preferred.clone();
            let language = self.language.clone();
            let fallback = self.choose(preferred.as_ref(), &language, false).ok();
            self.shared
                .update(|snapshot| snapshot.selected_voice = fallback);
            self.failure(
                "The active voice is no longer available; playback has paused".into(),
                generation,
            );
        }
        Ok(())
    }

    fn callback(&mut self, event: BackendEvent) {
        let id = match &event {
            BackendEvent::Begin(id)
            | BackendEvent::Done(id)
            | BackendEvent::Failed(id, _)
            | BackendEvent::Cancelled(id) => *id,
        };
        let Some((active_id, generation)) = self.active else {
            return;
        };
        if active_id != id {
            return;
        }
        match event {
            BackendEvent::Begin(_) => {
                self.emit(Event::Begin, generation);
            }
            BackendEvent::Done(_) => {
                self.active = None;
                self.emit(Event::Done, generation);
            }
            BackendEvent::Failed(_, error) => self.failure(error, generation),
            BackendEvent::Cancelled(_) => {
                self.failure("Speech was cancelled by the system".into(), generation)
            }
        }
    }
}
