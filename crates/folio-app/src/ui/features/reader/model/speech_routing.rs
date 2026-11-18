use folio_core::infra::config::Speech;
use folio_core::speech::{Voice, VoiceKey};

/// 每次从完整段首匹配；不改变朗读文本或高亮的字符坐标。
pub(in crate::ui::features::reader) fn voice_for_paragraph(
    settings: &Speech,
    paragraph: &str,
    voices: &[Voice],
) -> Option<VoiceKey> {
    let role = settings.roles.iter().find(|role| {
        !role.prefix.trim().is_empty() && paragraph.trim_start().starts_with(role.prefix.trim())
    });
    role.and_then(|role| role.voice.as_ref())
        .filter(|key| {
            voices.iter().any(|voice| {
                voice.key == **key
                    && voice.installed == Some(true)
                    && voice.network_required == Some(false)
            })
        })
        .cloned()
        .or_else(|| settings.narrator_voice.clone())
}

#[cfg(test)]
mod tests {
    use folio_core::infra::config::{Speech, SpeechRole};
    use folio_core::speech::{Voice, VoiceKey};

    use crate::ui::features::reader::model::speech_routing::voice_for_paragraph;

    fn key(id: &str) -> VoiceKey {
        VoiceKey {
            backend: "test".into(),
            engine: "local".into(),
            id: id.into(),
        }
    }

    #[test]
    fn roles_require_explicit_prefix_and_preserve_unavailable_preference() {
        let narrator = key("narrator");
        let actor = key("actor");
        let settings = Speech {
            narrator_voice: Some(narrator.clone()),
            roles: vec![SpeechRole {
                id: "role-a".into(),
                name: "张三".into(),
                prefix: "张三：".into(),
                voice: Some(actor.clone()),
            }],
            ..Speech::default()
        };
        let mut voices = vec![Voice {
            key: actor.clone(),
            name: "同名音色".into(),
            language: "zh-Hans-CN".into(),
            network_required: Some(false),
            installed: Some(true),
        }];
        assert_eq!(
            voice_for_paragraph(&settings, "  张三：你好。再见。", &voices),
            Some(actor.clone())
        );
        assert_eq!(
            voice_for_paragraph(&settings, "他说张三：你好。", &voices),
            Some(narrator.clone())
        );
        voices[0].network_required = Some(true);
        assert_eq!(
            voice_for_paragraph(&settings, "张三：你好。", &voices),
            Some(narrator.clone())
        );
        assert_eq!(
            voice_for_paragraph(&settings, "张三：你好。", &[]),
            Some(narrator)
        );
        assert_eq!(settings.roles[0].voice, Some(actor));
    }

    #[test]
    fn blank_prefix_never_matches_and_first_explicit_rule_wins() {
        let settings = Speech {
            roles: vec![
                SpeechRole {
                    id: "empty".into(),
                    name: "空".into(),
                    prefix: " ".into(),
                    voice: Some(key("empty")),
                },
                SpeechRole {
                    id: "inherit".into(),
                    name: "继承".into(),
                    prefix: "角色：".into(),
                    voice: None,
                },
                SpeechRole {
                    id: "later".into(),
                    name: "后续".into(),
                    prefix: "角色：".into(),
                    voice: Some(key("later")),
                },
            ],
            ..Speech::default()
        };
        assert_eq!(voice_for_paragraph(&settings, "普通段落。", &[]), None);
        assert_eq!(voice_for_paragraph(&settings, "角色：你好。", &[]), None);
    }
}
