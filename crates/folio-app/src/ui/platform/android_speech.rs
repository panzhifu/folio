use folio_core::speech::{BackendEvent, Voice, VoiceKey};
use serde_json::Value;

#[cfg(target_os = "android")]
pub(crate) fn install() {
    use std::sync::Arc;
    if let Err(error) = folio_core::speech::install_backend(Arc::new(|_| {
        AndroidSpeech::initialize()
            .map(|backend| Box::new(backend) as Box<dyn folio_core::speech::Backend>)
    })) {
        tracing::warn!("Android speech backend registration failed: {error}");
    }
}

#[cfg(target_os = "android")]
fn java_json(request: &Value) -> Result<Value, String> {
    use ::jni::objects::JValue;
    use gpui_mobile::android::jni::{activity, find_app_class, get_string, with_env};
    let json = with_env(|env| {
        let activity = activity(env)?;
        let class = find_app_class(env, "dev.gpui.mobile.FolioSpeech")?;
        let argument = env
            .new_string(request.to_string())
            .map_err(|_| "Speech request could not be encoded".to_string())?;
        let result = env
            .call_static_method(
                &class,
                ::jni::jni_str!("request"),
                ::jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;)Ljava/lang/String;"),
                &[JValue::Object(&activity), JValue::Object(&argument)],
            )
            .and_then(|value| value.l())
            .map_err(|_| {
                env.exception_clear();
                "Android speech bridge call failed".to_string()
            })?;
        if result.is_null() {
            return Err("Android speech bridge returned no response".to_string());
        }
        Ok(get_string(env, &result))
    })?;
    let value: Value = serde_json::from_str(&json)
        .map_err(|_| "Android speech response is invalid".to_string())?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(error.to_owned());
    }
    Ok(value)
}

fn voice_key(value: &Value, scope: &str) -> Result<VoiceKey, String> {
    let backend = value["backend"].as_str();
    let engine = value["engine"].as_str();
    let id = value["id"].as_str();
    match (backend, engine, id) {
        (Some("android"), Some(engine), Some(id))
            if engine == scope
                && !id.is_empty()
                && scope
                    .strip_prefix("requested:")
                    .is_some_and(|package| !package.is_empty()) =>
        {
            Ok(VoiceKey {
                backend: "android".to_owned(),
                engine: engine.to_owned(),
                id: id.to_owned(),
            })
        }
        _ => Err("Android speech voice identity is invalid".to_owned()),
    }
}

fn parse_voices(value: &Value, scope: &str) -> Result<Vec<Voice>, String> {
    let voices = value["voices"]
        .as_array()
        .filter(|voices| voices.len() <= 2048)
        .ok_or_else(|| "Android speech directory is invalid".to_owned())?;
    voices
        .iter()
        .map(|voice| {
            Ok(Voice {
                key: voice_key(&voice["key"], scope)?,
                name: voice["name"]
                    .as_str()
                    .ok_or_else(|| "Android speech voice name is invalid".to_owned())?
                    .to_owned(),
                language: voice["language"]
                    .as_str()
                    .ok_or_else(|| "Android speech voice language is invalid".to_owned())?
                    .to_owned(),
                network_required: voice["network_required"].as_bool(),
                installed: voice["installed"].as_bool(),
            })
        })
        .collect()
}

fn parse_events(value: &Value) -> Result<Vec<BackendEvent>, String> {
    let events = value["events"]
        .as_array()
        .ok_or_else(|| "Android speech events are invalid".to_owned())?;
    events
        .iter()
        .map(|event| {
            let id = event["id"]
                .as_str()
                .and_then(|id| id.parse::<u64>().ok())
                .ok_or_else(|| "Android speech utterance identity is invalid".to_owned())?;
            match event["kind"].as_str() {
                Some("begin") => Ok(BackendEvent::Begin(id)),
                Some("done") => Ok(BackendEvent::Done(id)),
                Some("cancelled") => Ok(BackendEvent::Cancelled(id)),
                Some("failed") => Ok(BackendEvent::Failed(
                    id,
                    event["error"]
                        .as_str()
                        .unwrap_or("Speech synthesis failed")
                        .to_owned(),
                )),
                _ => Err("Android speech event kind is invalid".to_owned()),
            }
        })
        .collect()
}

#[cfg(target_os = "android")]
struct AndroidSpeech {
    session: u64,
    scope: String,
    default: Option<VoiceKey>,
    active_utterance: Option<u64>,
    closed: bool,
}

#[cfg(target_os = "android")]
impl AndroidSpeech {
    fn initialize() -> Result<Self, String> {
        let value = java_json(&serde_json::json!({"op": "init"}))?;
        let session = value["session"]
            .as_u64()
            .ok_or_else(|| "Android speech session identity is invalid".to_owned())?;
        let mut backend = Self {
            session,
            scope: value["engine"].as_str().unwrap_or_default().to_owned(),
            default: None,
            active_utterance: None,
            closed: false,
        };
        if !value["default_voice"].is_null() {
            backend.default = Some(voice_key(&value["default_voice"], &backend.scope)?);
        }
        Ok(backend)
    }

    fn request(&self, mut argument: Value) -> Result<Value, String> {
        argument["session"] = self.session.into();
        java_json(&argument)
    }
}

#[cfg(target_os = "android")]
impl folio_core::speech::Backend for AndroidSpeech {
    fn voices(&mut self) -> Result<Vec<Voice>, String> {
        parse_voices(
            &self.request(serde_json::json!({"op": "voices"}))?,
            &self.scope,
        )
    }

    fn default_voice(&self) -> Option<VoiceKey> {
        self.default.clone()
    }

    fn select_voice(&mut self, voice: &VoiceKey) -> Result<(), String> {
        if voice.backend != "android" || voice.engine != self.scope {
            return Err("The selected voice belongs to another speech engine".to_owned());
        }
        self.request(serde_json::json!({"op": "select", "voice": voice.id}))?;
        Ok(())
    }

    fn speak(&mut self, id: u64, text: &str) -> Result<(), String> {
        self.request(
            serde_json::json!({"op": "speak", "utterance": id.to_string(), "text": text}),
        )?;
        self.active_utterance = Some(id);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), String> {
        self.active_utterance = None;
        self.request(serde_json::json!({"op": "stop"}))?;
        Ok(())
    }

    fn set_rate(&mut self, value: f32) -> Result<(), String> {
        self.request(serde_json::json!({"op": "rate", "value": value}))?;
        Ok(())
    }

    fn set_pitch(&mut self, value: f32) -> Result<(), String> {
        self.request(serde_json::json!({"op": "pitch", "value": value}))?;
        Ok(())
    }

    fn poll_events(&mut self) -> Vec<BackendEvent> {
        if self.active_utterance.is_none() {
            return Vec::new();
        }
        match self
            .request(serde_json::json!({"op": "events"}))
            .and_then(|value| parse_events(&value))
        {
            Ok(events) => {
                for event in &events {
                    let terminal = match event {
                        BackendEvent::Done(id)
                        | BackendEvent::Failed(id, _)
                        | BackendEvent::Cancelled(id) => Some(*id),
                        BackendEvent::Begin(_) => None,
                    };
                    if terminal.is_some() && terminal == self.active_utterance {
                        self.active_utterance = None;
                    }
                }
                events
            }
            Err(error) => self
                .active_utterance
                .take()
                .map(|id| vec![BackendEvent::Failed(id, error)])
                .unwrap_or_default(),
        }
    }

    fn shutdown(&mut self) {
        if !self.closed {
            self.closed = true;
            let _ = self.request(serde_json::json!({"op": "shutdown"}));
        }
    }
}

#[cfg(target_os = "android")]
impl Drop for AndroidSpeech {
    fn drop(&mut self) {
        folio_core::speech::Backend::shutdown(self);
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_events, parse_voices};
    use folio_core::speech::BackendEvent;
    use serde_json::json;

    #[test]
    fn android_failure_and_cancellation_never_become_completion() {
        let events = parse_events(&json!({"events": [
            {"id": "42", "kind": "failed", "error": "engine error"},
            {"id": "41", "kind": "cancelled"},
            {"id": "43", "kind": "done"}
        ]}))
        .unwrap();
        assert!(matches!(&events[0], BackendEvent::Failed(42, error) if error == "engine error"));
        assert!(matches!(events[1], BackendEvent::Cancelled(41)));
        assert!(matches!(events[2], BackendEvent::Done(43)));
        assert!(parse_events(&json!({"events": [{"id": "-1", "kind": "done"}]})).is_err());
    }

    #[test]
    fn android_directory_retains_unknown_installation_and_language_script() {
        let directory = json!({"voices": [{
            "key": {"backend": "android", "engine": "requested:example.tts", "id": "voice-1"},
            "name": "Voice", "language": "zh-Hant-TW", "network_required": false,
            "installed": null
        }]});
        let voices = parse_voices(&directory, "requested:example.tts").unwrap();
        assert_eq!(voices[0].language, "zh-Hant-TW");
        assert_eq!(voices[0].installed, None);
        assert_eq!(voices[0].network_required, Some(false));
        assert!(parse_voices(&directory, "requested:other.tts").is_err());
    }
}
