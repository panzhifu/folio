use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_channel::{Receiver, Sender};

use crate::{BackendFactory, Command, Delivery, Event, Snapshot, Status, VoiceKey};

pub(super) struct Submission {
    pub(super) generation: u64,
    pub(super) request: Request,
}

pub(super) enum Request {
    Command(Command),
    Select(
        Option<VoiceKey>,
        String,
        Sender<Result<Option<VoiceKey>, String>>,
    ),
    Refresh,
}

pub(super) struct Shared {
    pub(super) snapshot: Mutex<Snapshot>,
    pub(super) stopping: AtomicBool,
    pub(super) idle_wait: Duration,
    thread: OnceLock<std::thread::Thread>,
    generation: Mutex<u64>,
    finished: AtomicBool,
}

impl Shared {
    fn wake(&self) {
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }

    pub(super) fn update(&self, change: impl FnOnce(&mut Snapshot)) {
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        change(&mut snapshot);
        snapshot.revision = snapshot.revision.wrapping_add(1);
    }
}

#[derive(Clone)]
pub struct Engine {
    input_tx: Sender<Submission>,
    event_rx: Receiver<Delivery>,
    shared: Arc<Shared>,
}

impl Engine {
    #[cfg(feature = "test-support")]
    pub fn with_backend(factory: BackendFactory) -> Self {
        Self::start(factory, Duration::from_secs(10))
    }

    #[cfg(feature = "test-support")]
    pub fn shutdown(&self) {
        self.stop_worker();
    }

    pub fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    pub fn playback_generation(&self) -> u64 {
        *self
            .shared
            .generation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// 消费方须在更新状态时校验，不能只在接收队列时校验。
    pub fn accepts(&self, delivery: &Delivery) -> bool {
        delivery
            .generation
            .is_none_or(|generation| generation == self.playback_generation())
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared
            .snapshot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn ready(&self) -> Option<bool> {
        match self.snapshot().status {
            Status::Initializing => None,
            Status::Ready => Some(true),
            Status::Failed(_) | Status::Unavailable => Some(false),
        }
    }

    pub fn send(&self, command: Command) {
        let advances = matches!(
            command,
            Command::Speak { .. } | Command::SpeakWithVoice { .. } | Command::Stop
        );
        self.submit(Request::Command(command), advances);
    }

    /// 竞争接收端，仅当前阅读会话消费；设置与预热读取 snapshot。
    pub fn events(&self) -> Receiver<Delivery> {
        self.event_rx.clone()
    }

    pub fn select_voice(
        &self,
        voice: Option<VoiceKey>,
        language: String,
    ) -> Receiver<Result<Option<VoiceKey>, String>> {
        let (tx, rx) = async_channel::bounded(1);
        if !self.submit(Request::Select(voice, language, tx.clone()), true) {
            let _ = tx.try_send(Err("Speech engine is unavailable".into()));
        }
        rx
    }

    pub fn refresh_voices(&self) {
        self.submit(Request::Refresh, false);
    }

    fn submit(&self, request: Request, advances: bool) -> bool {
        let mut generation = self
            .shared
            .generation
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if advances {
            *generation = generation.wrapping_add(1);
        }
        let sent = self
            .input_tx
            .try_send(Submission {
                generation: *generation,
                request,
            })
            .is_ok();
        self.shared.wake();
        sent
    }

    fn stop_worker(&self) {
        let mut generation = self
            .shared
            .generation
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *generation = generation.wrapping_add(1);
        self.shared.stopping.store(true, Ordering::Release);
        self.shared.wake();
    }

    fn start(factory: BackendFactory, timeout: Duration) -> Self {
        Self::start_with_idle(factory, timeout, Duration::from_secs(1))
    }

    fn start_with_idle(factory: BackendFactory, timeout: Duration, idle_wait: Duration) -> Self {
        let (input_tx, input_rx) = async_channel::unbounded();
        let (event_tx, event_rx) = async_channel::unbounded();
        let shared = Arc::new(Shared {
            snapshot: Mutex::new(Snapshot::default()),
            stopping: AtomicBool::new(false),
            idle_wait,
            thread: OnceLock::new(),
            generation: Mutex::new(0),
            finished: AtomicBool::new(false),
        });
        let engine = Self {
            input_tx,
            event_rx,
            shared: shared.clone(),
        };
        let (init_tx, init_rx) = std::sync::mpsc::channel();
        let worker_shared = shared.clone();
        let worker_events = event_tx.clone();
        let started = std::thread::Builder::new()
            .name("folio-speech".into())
            .spawn(move || {
                let _ = worker_shared.thread.set(std::thread::current());
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::worker::run(
                        factory,
                        input_rx.clone(),
                        worker_events.clone(),
                        worker_shared.clone(),
                        init_tx,
                    );
                }));
                if result.is_err() {
                    worker_shared.update(|snapshot| {
                        snapshot.status = Status::Failed("Speech worker failed".into())
                    });
                    let _ = worker_events.try_send(Delivery {
                        generation: None,
                        event: Event::Failed("Speech worker failed".into()),
                    });
                }
                input_rx.close();
                while let Ok(request) = input_rx.try_recv() {
                    if let Request::Select(_, _, reply) = request.request {
                        let _ = reply.try_send(Err("Speech engine is unavailable".into()));
                    }
                }
                worker_events.close();
                worker_shared.finished.store(true, Ordering::Release);
            });
        if started.is_err() {
            shared.update(|snapshot| {
                snapshot.status = Status::Failed("Could not start speech worker".into())
            });
            shared.finished.store(true, Ordering::Release);
            engine.input_tx.close();
            event_tx.close();
            return engine;
        }
        let _ = std::thread::Builder::new()
            .name("folio-speech-init-watch".into())
            .spawn(move || {
                if matches!(
                    init_rx.recv_timeout(timeout),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ) {
                    let mut snapshot = shared
                        .snapshot
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if snapshot.status == Status::Initializing {
                        let error = "Speech initialization timed out".to_string();
                        snapshot.status = Status::Failed(error.clone());
                        snapshot.last_error = Some(error);
                        snapshot.revision = snapshot.revision.wrapping_add(1);
                        shared.stopping.store(true, Ordering::Release);
                        shared.wake();
                        let _ = event_tx.try_send(Delivery {
                            generation: None,
                            event: Event::Ready(false),
                        });
                    }
                }
            });
        engine
    }
}

#[derive(Default)]
struct Registry {
    factory: Option<BackendFactory>,
    engine: Option<Engine>,
}

impl Registry {
    fn get(&mut self) -> Option<Engine> {
        if let Some(engine) = &self.engine
            && !engine.shared.finished.load(Ordering::Acquire)
        {
            return Some(engine.clone());
        }
        let factory = self.factory.clone().or_else(platform_factory)?;
        let engine = Engine::start(factory, Duration::from_secs(10));
        self.engine = Some(engine.clone());
        Some(engine)
    }
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

/// 在平台启动阶段安装；活动后端须先 shutdown 并完成释放。
pub fn install_backend(factory: BackendFactory) -> Result<(), String> {
    let mut registry = registry().lock().unwrap_or_else(|error| error.into_inner());
    if registry
        .engine
        .as_ref()
        .is_some_and(|engine| !engine.shared.finished.load(Ordering::Acquire))
    {
        return Err("Speech backend is still active".into());
    }
    registry.factory = Some(factory);
    Ok(())
}

pub fn engine() -> Option<Engine> {
    registry()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get()
}

/// 仅构造后端与枚举目录，不设置音色、不合成书文。
pub fn prewarm() -> Option<Engine> {
    engine()
}

pub fn shutdown() {
    if let Some(engine) = &registry()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .engine
    {
        engine.stop_worker();
    }
}

fn platform_factory() -> Option<BackendFactory> {
    #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
    {
        Some(Arc::new(crate::linux::create))
    }
    #[cfg(target_os = "windows")]
    {
        Some(Arc::new(crate::windows::create))
    }
    #[cfg(not(any(
        all(target_os = "linux", not(target_env = "ohos")),
        target_os = "windows"
    )))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use async_channel::{Receiver, Sender};

    use crate::engine::{Engine, Registry};
    use crate::{Backend, BackendEvent, BackendFactory, Command, Event, Status, Voice, VoiceKey};

    #[derive(Clone, Debug, PartialEq)]
    enum Record {
        Select(VoiceKey),
        Speak(u64, String),
        Stop,
        Shutdown,
    }

    #[derive(Default)]
    struct State {
        initializations: AtomicUsize,
        polls: AtomicUsize,
        fail_init: AtomicBool,
        fail_speak: AtomicBool,
        fail_select: AtomicBool,
        fail_rate: AtomicBool,
        voices: Mutex<Vec<Voice>>,
        records: Mutex<Vec<Record>>,
        callbacks: Mutex<Option<Sender<BackendEvent>>>,
    }

    impl State {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                voices: Mutex::new(vec![
                    voice("narrator", "en-US"),
                    voice("role", "zh-Hant-TW"),
                ]),
                ..Self::default()
            })
        }

        fn factory(self: &Arc<Self>) -> BackendFactory {
            let state = self.clone();
            Arc::new(move |events| {
                state.initializations.fetch_add(1, Ordering::Relaxed);
                if state.fail_init.load(Ordering::Relaxed) {
                    return Err("Initialization failed".into());
                }
                *state.callbacks.lock().unwrap() = Some(events);
                Ok(Box::new(Fake(state.clone())))
            })
        }

        fn callback(&self, event: BackendEvent) {
            self.callbacks
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .try_send(event)
                .unwrap();
        }

        fn utterances(&self) -> Vec<(u64, String)> {
            self.records
                .lock()
                .unwrap()
                .iter()
                .filter_map(|record| {
                    if let Record::Speak(id, text) = record {
                        Some((*id, text.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        }
    }

    struct Fake(Arc<State>);

    impl Backend for Fake {
        fn voices(&mut self) -> Result<Vec<Voice>, String> {
            Ok(self.0.voices.lock().unwrap().clone())
        }
        fn default_voice(&self) -> Option<VoiceKey> {
            self.0
                .voices
                .lock()
                .unwrap()
                .first()
                .map(|voice| voice.key.clone())
        }
        fn select_voice(&mut self, key: &VoiceKey) -> Result<(), String> {
            self.0
                .records
                .lock()
                .unwrap()
                .push(Record::Select(key.clone()));
            if self.0.fail_select.load(Ordering::Relaxed) {
                return Err("Voice selection failed".into());
            }
            Ok(())
        }
        fn speak(&mut self, id: u64, text: &str) -> Result<(), String> {
            self.0
                .records
                .lock()
                .unwrap()
                .push(Record::Speak(id, text.into()));
            if self.0.fail_speak.load(Ordering::Relaxed) {
                Err("Synthesis failed".into())
            } else {
                Ok(())
            }
        }
        fn stop(&mut self) -> Result<(), String> {
            self.0.records.lock().unwrap().push(Record::Stop);
            Ok(())
        }
        fn set_rate(&mut self, _value: f32) -> Result<(), String> {
            if self.0.fail_rate.load(Ordering::Relaxed) {
                Err("Rate failed".into())
            } else {
                Ok(())
            }
        }
        fn set_pitch(&mut self, _value: f32) -> Result<(), String> {
            Ok(())
        }
        fn shutdown(&mut self) {
            self.0.records.lock().unwrap().push(Record::Shutdown);
        }
        fn poll_events(&mut self) -> Vec<BackendEvent> {
            self.0.polls.fetch_add(1, Ordering::Relaxed);
            Vec::new()
        }
    }

    fn voice(id: &str, language: &str) -> Voice {
        Voice {
            key: VoiceKey {
                backend: "test".into(),
                engine: "engine".into(),
                id: id.into(),
            },
            name: id.into(),
            language: language.into(),
            network_required: Some(false),
            installed: Some(true),
        }
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let started = Instant::now();
        while !condition() {
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "Timed out waiting for test worker"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn receive<T>(receiver: &Receiver<T>) -> T {
        let mut value = None;
        wait_until(|| {
            value = receiver.try_recv().ok();
            value.is_some()
        });
        value.unwrap()
    }

    fn ready(state: &Arc<State>) -> Engine {
        let engine = Engine::start(state.factory(), Duration::from_secs(2));
        assert_eq!(receive(&engine.events()).event, Event::Ready(true));
        engine
    }

    fn finish(engine: &Engine) {
        engine.stop_worker();
        wait_until(|| engine.shared.finished.load(Ordering::Acquire));
    }

    #[test]
    fn speech_engine_prewarm_is_singleton_silent_and_snapshot_persists() {
        let state = State::new();
        let registry = Arc::new(Mutex::new(Registry {
            factory: Some(state.factory()),
            engine: None,
        }));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let registry = registry.clone();
            threads.push(std::thread::spawn(move || {
                registry.lock().unwrap().get().unwrap()
            }));
        }
        let engines: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        wait_until(|| engines[0].ready() == Some(true));
        assert!(
            engines
                .iter()
                .all(|engine| Arc::ptr_eq(&engines[0].shared, &engine.shared))
        );
        assert_eq!(state.initializations.load(Ordering::Relaxed), 1);
        assert!(state.records.lock().unwrap().is_empty());
        assert_eq!(receive(&engines[0].events()).event, Event::Ready(true));
        assert_eq!(engines[1].snapshot().status, Status::Ready);
        assert_eq!(engines[1].snapshot().voices.len(), 2);
        finish(&engines[0]);
        assert!(state.records.lock().unwrap().contains(&Record::Shutdown));
    }

    #[test]
    fn speech_engine_idle_worker_wakes_for_commands_selection_refresh_and_shutdown() {
        let state = State::new();
        let engine = Engine::start_with_idle(
            state.factory(),
            Duration::from_secs(2),
            Duration::from_secs(30),
        );
        assert_eq!(receive(&engine.events()).event, Event::Ready(true));
        wait_until(|| state.polls.load(Ordering::Relaxed) > 0);
        let polls = state.polls.load(Ordering::Relaxed);
        receive(&engine.select_voice(None, "en".into())).unwrap();
        wait_until(|| state.polls.load(Ordering::Relaxed) > polls);
        let revision = engine.snapshot().revision;
        let polls = state.polls.load(Ordering::Relaxed);
        engine.refresh_voices();
        wait_until(|| {
            engine.snapshot().revision > revision && state.polls.load(Ordering::Relaxed) > polls
        });
        engine.send(Command::Speak {
            text: "wake".into(),
        });
        wait_until(|| state.utterances().len() == 1);
        let id = state.utterances()[0].0;
        state.callback(BackendEvent::Done(id));
        assert_eq!(receive(&engine.events()).event, Event::VoiceChanged);
        assert_eq!(receive(&engine.events()).event, Event::Done);
        finish(&engine);
    }

    #[test]
    fn speech_engine_failed_initialization_retries_after_exit() {
        let state = State::new();
        state.fail_init.store(true, Ordering::Relaxed);
        let mut registry = Registry {
            factory: Some(state.factory()),
            engine: None,
        };
        let first = registry.get().unwrap();
        wait_until(|| first.shared.finished.load(Ordering::Acquire));
        assert_eq!(receive(&first.events()).event, Event::Ready(false));
        let ack = first.select_voice(None, "en".into());
        assert!(receive(&ack).is_err());
        state.fail_init.store(false, Ordering::Relaxed);
        let second = registry.get().unwrap();
        assert_eq!(receive(&second.events()).event, Event::Ready(true));
        assert!(!Arc::ptr_eq(&first.shared, &second.shared));
        assert_eq!(state.initializations.load(Ordering::Relaxed), 2);
        finish(&second);
    }

    #[test]
    fn speech_engine_timeout_cannot_stack_initialization_or_publish_late_ready() {
        let state = State::new();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let factory_state = state.clone();
        let factory: BackendFactory = Arc::new(move |events| {
            factory_state
                .initializations
                .fetch_add(1, Ordering::Relaxed);
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            *factory_state.callbacks.lock().unwrap() = Some(events);
            Ok(Box::new(Fake(factory_state.clone())))
        });
        let engine = Engine::start(factory.clone(), Duration::from_millis(20));
        let ack = engine.select_voice(None, "en".into());
        assert_eq!(receive(&engine.events()).event, Event::Ready(false));
        let mut registry = Registry {
            factory: Some(factory),
            engine: Some(engine.clone()),
        };
        assert!(Arc::ptr_eq(&engine.shared, &registry.get().unwrap().shared));
        release_tx.send(()).unwrap();
        wait_until(|| engine.shared.finished.load(Ordering::Acquire));
        assert!(matches!(engine.snapshot().status, Status::Failed(_)));
        assert!(engine.events().try_recv().is_err());
        assert!(receive(&ack).is_err());
        assert_eq!(state.initializations.load(Ordering::Relaxed), 1);
        assert_eq!(*state.records.lock().unwrap(), vec![Record::Shutdown]);
    }

    #[test]
    fn speech_engine_stale_callbacks_do_not_finish_new_utterances() {
        let state = State::new();
        let engine = ready(&state);
        engine.send(Command::Speak {
            text: "first".into(),
        });
        wait_until(|| state.utterances().len() == 1);
        let first = state.utterances()[0].0;
        engine.send(Command::Stop);
        engine.send(Command::Speak {
            text: "second".into(),
        });
        wait_until(|| state.utterances().len() == 2);
        let second = state.utterances()[1].0;
        state.callback(BackendEvent::Done(first));
        state.callback(BackendEvent::Begin(first));
        state.callback(BackendEvent::Begin(second));
        assert_eq!(receive(&engine.events()).event, Event::Begin);
        state.callback(BackendEvent::Done(second));
        assert_eq!(receive(&engine.events()).event, Event::Done);
        assert!(engine.events().try_recv().is_err());
        finish(&engine);
    }

    #[test]
    fn speech_engine_queued_done_is_rejected_after_new_playback_submission() {
        let state = State::new();
        let engine = ready(&state);
        engine.send(Command::Speak {
            text: "first".into(),
        });
        wait_until(|| state.utterances().len() == 1);
        let first_generation = engine.playback_generation();
        state.callback(BackendEvent::Done(state.utterances()[0].0));
        wait_until(|| !engine.events().is_empty());
        engine.send(Command::Speak {
            text: "second".into(),
        });
        let queued = receive(&engine.events());
        assert_eq!(queued.event, Event::Done);
        assert_eq!(queued.generation, Some(first_generation));
        assert!(!engine.accepts(&queued));
        wait_until(|| state.utterances().len() == 2);
        let current_generation = engine.playback_generation();
        engine.send(Command::SetRate(1.0));
        engine.refresh_voices();
        assert_eq!(engine.playback_generation(), current_generation);
        state.callback(BackendEvent::Begin(state.utterances()[1].0));
        let current = receive(&engine.events());
        assert_eq!(current.event, Event::Begin);
        assert_eq!(current.generation, Some(current_generation));
        assert!(engine.accepts(&current));
        engine.send(Command::Stop);
        assert!(!engine.accepts(&current));
        finish(&engine);
    }

    #[test]
    fn speech_engine_concurrent_submissions_keep_generation_and_queue_order() {
        let state = State::new();
        let engine = ready(&state);
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let engine = engine.clone();
                std::thread::spawn(move || engine.select_voice(None, "en".into()))
            })
            .collect();
        for thread in threads {
            receive(&thread.join().unwrap()).unwrap();
        }
        for generation in 1..=8 {
            let delivery = receive(&engine.events());
            assert_eq!(delivery.event, Event::VoiceChanged);
            assert_eq!(delivery.generation, Some(generation));
            assert_eq!(engine.accepts(&delivery), generation == 8);
        }
        finish(&engine);
    }

    #[test]
    fn speech_engine_synthesis_errors_pause_without_done() {
        let state = State::new();
        let engine = ready(&state);
        state.fail_speak.store(true, Ordering::Relaxed);
        engine.send(Command::Speak {
            text: "private book text".into(),
        });
        assert_eq!(
            receive(&engine.events()).event,
            Event::Failed("Synthesis failed".into())
        );
        let first = state.utterances()[0].0;
        state.callback(BackendEvent::Done(first));
        state.fail_speak.store(false, Ordering::Relaxed);
        engine.send(Command::Speak {
            text: "second".into(),
        });
        wait_until(|| state.utterances().len() == 2);
        let second = state.utterances()[1].0;
        state.callback(BackendEvent::Failed(second, "Backend failed".into()));
        assert_eq!(
            receive(&engine.events()).event,
            Event::Failed("Backend failed".into())
        );
        assert!(engine.events().try_recv().is_err());
        finish(&engine);
    }

    #[test]
    fn speech_engine_selection_acknowledges_only_applied_local_voice() {
        let state = State::new();
        let engine = ready(&state);
        let before = engine.snapshot().selected_voice;
        let role = state.voices.lock().unwrap()[1].key.clone();
        state.fail_select.store(true, Ordering::Relaxed);
        assert!(receive(&engine.select_voice(Some(role.clone()), "zh".into())).is_err());
        assert_eq!(engine.snapshot().selected_voice, before);
        assert!(matches!(receive(&engine.events()).event, Event::Failed(_)));
        state.fail_select.store(false, Ordering::Relaxed);
        assert_eq!(
            receive(&engine.select_voice(Some(role.clone()), "zh".into())),
            Ok(Some(role.clone()))
        );
        assert_eq!(receive(&engine.events()).event, Event::VoiceChanged);
        assert!(state.utterances().is_empty());
        let unknown = voice("missing", "zh").key;
        assert!(receive(&engine.select_voice(Some(unknown), "zh".into())).is_err());
        assert_eq!(engine.snapshot().selected_voice, Some(role));
        finish(&engine);
    }

    #[test]
    fn speech_engine_role_choice_and_speak_are_atomic_without_manual_event() {
        let state = State::new();
        let engine = ready(&state);
        let role = state.voices.lock().unwrap()[1].key.clone();
        engine.send(Command::SpeakWithVoice {
            text: "dialogue".into(),
            voice: Some(role.clone()),
            language: "zh-Hant-TW".into(),
            rate: 1.0,
            pitch: 1.0,
        });
        wait_until(|| state.utterances().len() == 1);
        let records = state.records.lock().unwrap().clone();
        assert_eq!(records[1], Record::Select(role));
        assert!(matches!(&records[2], Record::Speak(_, text) if text == "dialogue"));
        assert!(engine.events().try_recv().is_err());
        finish(&engine);
    }

    #[test]
    fn speech_engine_parameter_failure_does_not_speak_or_change_voice() {
        let state = State::new();
        let engine = ready(&state);
        state.fail_rate.store(true, Ordering::Relaxed);
        engine.send(Command::SpeakWithVoice {
            text: "must not speak".into(),
            voice: None,
            language: "en".into(),
            rate: 1.0,
            pitch: 1.0,
        });
        assert_eq!(
            receive(&engine.events()).event,
            Event::Failed("Rate failed".into())
        );
        assert!(state.utterances().is_empty());
        assert_eq!(engine.snapshot().selected_voice, None);
        finish(&engine);
    }

    #[test]
    fn speech_engine_partial_selection_failure_reapplies_previous_voice() {
        let state = State::new();
        let engine = ready(&state);
        let original = state.voices.lock().unwrap()[0].key.clone();
        receive(&engine.select_voice(Some(original.clone()), "en".into())).unwrap();
        assert_eq!(receive(&engine.events()).event, Event::VoiceChanged);
        let role = state.voices.lock().unwrap()[1].key.clone();
        state.fail_select.store(true, Ordering::Relaxed);
        assert!(receive(&engine.select_voice(Some(role), "zh".into())).is_err());
        assert_eq!(engine.snapshot().selected_voice, None);
        state.fail_select.store(false, Ordering::Relaxed);
        receive(&engine.select_voice(Some(original.clone()), "en".into())).unwrap();
        let selections = state
            .records
            .lock()
            .unwrap()
            .iter()
            .filter(|record| matches!(record, Record::Select(key) if key == &original))
            .count();
        assert_eq!(selections, 2);
        finish(&engine);
    }

    #[test]
    fn speech_engine_voice_disappearance_pauses_and_preserves_preference() {
        let state = State::new();
        let engine = ready(&state);
        let role = state.voices.lock().unwrap()[1].clone();
        receive(&engine.select_voice(Some(role.key.clone()), "zh".into())).unwrap();
        assert_eq!(receive(&engine.events()).event, Event::VoiceChanged);
        state.voices.lock().unwrap().pop();
        engine.refresh_voices();
        assert!(matches!(receive(&engine.events()).event, Event::Failed(_)));
        assert_ne!(engine.snapshot().selected_voice, Some(role.key.clone()));
        state.voices.lock().unwrap().push(role.clone());
        let revision = engine.snapshot().revision;
        engine.refresh_voices();
        wait_until(|| engine.snapshot().revision > revision);
        engine.send(Command::Speak {
            text: "restored".into(),
        });
        wait_until(|| state.utterances().len() == 1);
        assert_eq!(engine.snapshot().selected_voice, Some(role.key));
        finish(&engine);
    }
}
