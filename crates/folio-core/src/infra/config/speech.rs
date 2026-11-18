use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;

use crate::speech::VoiceKey;

/// 语速、音高以常速 1.0 的倍数保存，后端负责单位换算。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Speech {
    pub rate: f32,
    pub pitch: f32,
    pub auto_scroll: bool,
    /// 空值使用本机可用的离线音色；不可用的偏好仍保留。
    pub narrator_voice: Option<VoiceKey>,
    /// 完整 BCP-47 标签；空串让后端使用安全默认值。
    pub language: String,
    pub roles: Vec<SpeechRole>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpeechRole {
    pub id: String,
    pub name: String,
    /// 仅匹配段落开头，空串不参与匹配。
    pub prefix: String,
    /// 空值继承旁白。
    pub voice: Option<VoiceKey>,
}

impl Speech {
    pub const NORMAL: f32 = 1.0;
    pub const RATE_RANGE: RangeInclusive<f32> = 0.5..=2.0;
    pub const PITCH_RANGE: RangeInclusive<f32> = 0.5..=2.0;

    pub fn normalized(mut self) -> Self {
        self.rate = clamp_semantic(self.rate, &Self::RATE_RANGE);
        self.pitch = clamp_semantic(self.pitch, &Self::PITCH_RANGE);
        self
    }
}

impl Default for Speech {
    fn default() -> Self {
        Self {
            rate: Self::NORMAL,
            pitch: Self::NORMAL,
            auto_scroll: true,
            narrator_voice: None,
            language: String::new(),
            roles: Vec::new(),
        }
    }
}

/// 语义值（0.5–2.0）的归一：非有限退常速，其余夹进区间。
fn clamp_semantic(value: f32, range: &RangeInclusive<f32>) -> f32 {
    if !value.is_finite() {
        Speech::NORMAL
    } else {
        value.clamp(*range.start(), *range.end())
    }
}

#[cfg(test)]
mod tests {
    use crate::infra::config::{Speech, SpeechRole};
    use crate::speech::VoiceKey;

    #[test]
    fn legacy_speech_settings_keep_rate_pitch_and_scroll() {
        let speech: Speech =
            serde_json::from_str(r#"{"rate":1.25,"pitch":0.75,"auto_scroll":false}"#).unwrap();
        assert_eq!(speech.rate, 1.25);
        assert_eq!(speech.pitch, 0.75);
        assert!(!speech.auto_scroll);
        assert!(speech.narrator_voice.is_none());
        assert!(speech.language.is_empty());
        assert!(speech.roles.is_empty());
    }

    #[test]
    fn speech_roundtrip_preserves_scoped_voice_and_full_language() {
        let voice = VoiceKey {
            backend: "android".into(),
            engine: "fixture.local.engine".into(),
            id: "zh-tw-x-local".into(),
        };
        let settings = Speech {
            narrator_voice: Some(voice.clone()),
            language: "zh-Hant-TW".into(),
            roles: vec![SpeechRole {
                id: "role-one".into(),
                name: "阿青".into(),
                prefix: "阿青：".into(),
                voice: Some(voice),
            }],
            ..Speech::default()
        };
        let encoded = serde_json::to_string(&settings).unwrap();
        let decoded: Speech = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.normalized(), settings);
    }
}
