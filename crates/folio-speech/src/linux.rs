use std::collections::HashMap;

use async_channel::{Receiver, Sender};
use speech_dispatcher::{Connection, Mode, Priority};

use crate::{Backend, BackendEvent, Voice, VoiceKey};

pub(crate) fn create(_events: Sender<BackendEvent>) -> Result<Box<dyn Backend>, String> {
    let connection = Connection::open("Folio", "reading", "folio", Mode::Threaded)
        .map_err(|_| "Could not connect to Speech Dispatcher".to_string())?;
    let (raw_tx, raw_rx) = async_channel::unbounded();
    let begin_tx = raw_tx.clone();
    connection.on_begin(Some(Box::new(move |id, _| {
        let _ = begin_tx.try_send((id as u64, Kind::Begin));
    })));
    let done_tx = raw_tx.clone();
    connection.on_end(Some(Box::new(move |id, _| {
        let _ = done_tx.try_send((id as u64, Kind::Done));
    })));
    connection.on_cancel(Some(Box::new(move |id, _| {
        let _ = raw_tx.try_send((id as u64, Kind::Cancelled));
    })));
    Ok(Box::new(Linux {
        connection,
        raw_rx,
        ids: HashMap::new(),
        voices: Vec::new(),
        module: String::new(),
    }))
}

enum Kind {
    Begin,
    Done,
    Cancelled,
}

struct Linux {
    connection: Connection,
    raw_rx: Receiver<(u64, Kind)>,
    ids: HashMap<u64, u64>,
    voices: Vec<(Voice, speech_dispatcher::Voice)>,
    module: String,
}

impl Backend for Linux {
    fn voices(&mut self) -> Result<Vec<Voice>, String> {
        let module = response_value(self.connection.send_data("GET OUTPUT_MODULE\r\n", true))
            .ok_or_else(|| "Speech Dispatcher did not identify its output module".to_string())?;
        if module.contains(['\0', '\r', '\n']) {
            return Err("Invalid Speech Dispatcher module".into());
        }
        let voices = self
            .connection
            .list_synthesis_voices()
            .map_err(|_| "Could not list Speech Dispatcher voices".to_string())?;
        let modules = self.connection.list_output_modules().unwrap_or_default();
        // SSIP 可跨模块回退；只有全部已列出的候选均已知本地时才允许自动选择。
        let local = local_modules(&modules, &module);
        self.voices = voices
            .into_iter()
            .filter(|voice| !voice.name.contains('\0') && !voice.language.contains('\0'))
            .map(|voice| {
                let model = Voice {
                    key: VoiceKey {
                        backend: "speech-dispatcher".into(),
                        engine: format!("configured:{module}"),
                        id: voice.name.clone(),
                    },
                    name: voice.name.clone(),
                    language: voice.language.clone(),
                    network_required: local.then_some(false),
                    installed: local.then_some(true),
                };
                (model, voice)
            })
            .collect();
        self.module = module;
        Ok(self.voices.iter().map(|(voice, _)| voice.clone()).collect())
    }

    fn default_voice(&self) -> Option<VoiceKey> {
        // SSIP 只公开默认 voice_type，不能据此伪造具体 synthesis voice ID。
        None
    }

    fn select_voice(&mut self, key: &VoiceKey) -> Result<(), String> {
        self.verify_local_modules()?;
        let (_, voice) = self
            .voices
            .iter()
            .find(|(voice, _)| &voice.key == key && voice.is_local())
            .ok_or_else(|| "The requested Speech Dispatcher voice is unavailable".to_string())?;
        self.connection
            .set_language(voice.language.clone())
            .map_err(|_| "Could not select the speech language".to_string())?;
        // set_language 可能按服务配置切换模块，之后再锁定 key 所属模块。
        self.connection
            .set_output_module(self.module.clone())
            .map_err(|_| "Could not select the speech module".to_string())?;
        if response_value(self.connection.send_data("GET OUTPUT_MODULE\r\n", true)).as_deref()
            != Some(self.module.as_str())
        {
            return Err("Speech Dispatcher selected a different output module".into());
        }
        self.connection
            .set_synthesis_voice(voice)
            .map_err(|_| "Could not select the speech voice".to_string())
    }

    fn speak(&mut self, id: u64, text: &str) -> Result<(), String> {
        self.verify_local_modules()?;
        if text.contains('\0') {
            return Err("Speech text contains a null character".into());
        }
        let backend_id = self
            .connection
            .say(Priority::Text, text)
            .ok_or_else(|| "Speech Dispatcher rejected the utterance".to_string())?;
        self.ids.insert(backend_id, id);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), String> {
        self.ids.clear();
        self.connection
            .cancel()
            .map_err(|_| "Could not stop Speech Dispatcher".to_string())
    }

    fn set_rate(&mut self, value: f32) -> Result<(), String> {
        self.connection
            .set_voice_rate(((value.clamp(0.5, 2.0) - 1.0) * 100.0).round() as i32)
            .map_err(|_| "Could not set the speech rate".to_string())
    }

    fn set_pitch(&mut self, value: f32) -> Result<(), String> {
        self.connection
            .set_voice_pitch(((value.clamp(0.5, 2.0) - 1.0) * 100.0).round() as i32)
            .map_err(|_| "Could not set the speech pitch".to_string())
    }

    fn poll_events(&mut self) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        while let Ok((backend_id, kind)) = self.raw_rx.try_recv() {
            let Some(&id) = self.ids.get(&backend_id) else {
                continue;
            };
            events.push(match kind {
                Kind::Begin => BackendEvent::Begin(id),
                Kind::Done => {
                    self.ids.remove(&backend_id);
                    BackendEvent::Done(id)
                }
                Kind::Cancelled => {
                    self.ids.remove(&backend_id);
                    BackendEvent::Cancelled(id)
                }
            });
        }
        events
    }

    fn shutdown(&mut self) {
        let _ = self.stop();
        self.connection.on_begin(None);
        self.connection.on_end(None);
        self.connection.on_cancel(None);
        // Connection::drop 负责关闭；显式 close 后再 drop 会双重释放。
    }
}

impl Linux {
    fn verify_local_modules(&self) -> Result<(), String> {
        let modules = self
            .connection
            .list_output_modules()
            .map_err(|_| "Could not verify local speech modules".to_string())?;
        if !local_modules(&modules, &self.module) {
            return Err(
                "Speech Dispatcher fallback modules cannot be confirmed offline; refresh voices"
                    .into(),
            );
        }
        Ok(())
    }
}

fn local_modules(modules: &[String], configured: &str) -> bool {
    modules.iter().any(|module| module == configured)
        && modules.iter().all(|module| {
            matches!(
                module.as_str(),
                "espeak" | "espeak-ng" | "festival" | "flite" | "pico" | "rhvoice"
            )
        })
}

fn response_value(response: Option<String>) -> Option<String> {
    response?.lines().find_map(|line| {
        let (code, value) = line.split_once('-')?;
        (code.len() == 3 && code.starts_with('2') && !value.is_empty()).then(|| value.to_string())
    })
}

#[cfg(test)]
mod tests {
    use crate::linux::{local_modules, response_value};

    #[test]
    fn speech_dispatcher_response_requires_success_data() {
        assert_eq!(
            response_value(Some("211-espeak-ng\r\n211 OK\r\n".into())),
            Some("espeak-ng".into())
        );
        assert_eq!(response_value(Some("500 UNKNOWN COMMAND\r\n".into())), None);
        assert_eq!(response_value(Some("500-error\r\n".into())), None);
    }

    #[test]
    fn speech_dispatcher_unknown_fallback_is_not_classified_offline() {
        assert!(local_modules(&["espeak".into(), "flite".into()], "espeak"));
        assert!(!local_modules(
            &["espeak".into(), "remote".into()],
            "espeak"
        ));
        assert!(!local_modules(&["espeak".into()], "missing"));
        assert!(!local_modules(&[], "espeak"));
    }
}
