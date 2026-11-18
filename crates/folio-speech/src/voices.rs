use crate::{Voice, VoiceKey};

/// 只返回已确认安装的离线音色；保留偏好的责任在配置层。
pub fn resolve_voice<'a>(
    voices: &'a [Voice],
    preferred: Option<&VoiceKey>,
    language: &str,
    default: Option<&VoiceKey>,
) -> Option<&'a Voice> {
    let local = |voice: &&Voice| voice.is_local();
    if let Some(voice) = voices
        .iter()
        .filter(local)
        .find(|voice| Some(&voice.key) == preferred)
    {
        return Some(voice);
    }
    let language = normalized_language(language);
    if !language.is_empty() {
        if let Some(voice) = voices
            .iter()
            .filter(local)
            .find(|voice| normalized_language(&voice.language) == language)
        {
            return Some(voice);
        }
        let primary = language.split('-').next();
        if let Some(voice) = voices
            .iter()
            .filter(local)
            .find(|voice| normalized_language(&voice.language).split('-').next() == primary)
        {
            return Some(voice);
        }
    }
    voices
        .iter()
        .filter(local)
        .find(|voice| Some(&voice.key) == default)
}

fn normalized_language(language: &str) -> String {
    language.trim().replace('_', "-").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use crate::{Voice, VoiceKey, resolve_voice};

    fn voice(id: &str, language: &str) -> Voice {
        Voice {
            key: VoiceKey {
                backend: "test".into(),
                engine: "local".into(),
                id: id.into(),
            },
            name: id.into(),
            language: language.into(),
            network_required: Some(false),
            installed: Some(true),
        }
    }

    #[test]
    fn selection_preserves_full_language_and_engine_scope() {
        let mut voices = vec![
            voice("same", "zh-Hans-CN"),
            voice("same", "zh-Hant-TW"),
            voice("en", "en-US"),
        ];
        voices[1].key.engine = "second".into();
        assert_eq!(
            resolve_voice(&voices, None, "zh-Hant-TW", None),
            Some(&voices[1])
        );
        assert_eq!(
            resolve_voice(&voices, None, "zh_Hant_tw", None),
            Some(&voices[1])
        );
        assert_eq!(
            resolve_voice(&voices, None, "zh-Hant-HK", None),
            Some(&voices[0])
        );
        assert_eq!(
            resolve_voice(&voices, Some(&voices[1].key), "en", None),
            Some(&voices[1])
        );
        assert_eq!(
            resolve_voice(&voices, None, "xx", Some(&voices[2].key)),
            Some(&voices[2])
        );
    }

    #[test]
    fn selection_rejects_unknown_network_and_missing_data() {
        let mut voices = vec![
            voice("unknown", "en"),
            voice("network", "en"),
            voice("download", "en"),
        ];
        voices[0].network_required = None;
        voices[1].network_required = Some(true);
        voices[2].installed = Some(false);
        assert_eq!(
            resolve_voice(&voices, Some(&voices[0].key), "en", Some(&voices[1].key)),
            None
        );
        voices.push(voice("safe", "en"));
        assert_eq!(
            resolve_voice(&voices, Some(&voices[1].key), "en", None),
            Some(&voices[3])
        );
    }
}
