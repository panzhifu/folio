//! Windows 听书后端：系统 SpeechSynthesizer 合成音频流，MediaPlayer 负责播放。
//!
//! 后端跑在 folio-speech 的专职线程上；WinRT 要求 STA，所以在这里初始化 COM，
//! 并在同一退出路径上取消初始化。合成是异步的，`poll_events` 在线程空闲时推进它，
//! 播放结束由 `MediaEnded` 回调送回事件通道。

use std::sync::{Arc, Mutex};

use async_channel::Sender;
use windows::Foundation::TypedEventHandler;
use windows::Media::Core::{IMediaSource, MediaSource};
use windows::Media::Playback::{MediaPlayer, MediaPlayerAudioCategory};
use windows::Media::SpeechSynthesis::{SpeechSynthesisStream, SpeechSynthesizer, VoiceInformation};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::core::{HSTRING, IInspectable, Interface};
use windows_future::{AsyncStatus, IAsyncOperation};

use crate::{Backend, BackendEvent, Voice, VoiceKey};

const BACKEND: &str = "windows-speech";

/// `poll_events` 一次最多推进多少条，避免合成任务把工作线程占住。
const MAX_PENDING_PER_POLL: usize = 32;

pub(crate) fn create(events: Sender<BackendEvent>) -> Result<Box<dyn Backend>, String> {
    let com = ComApartment::enter()?;
    let synthesizer = SpeechSynthesizer::new()
        .map_err(|error| format!("创建 Windows SpeechSynthesizer 失败：{error}"))?;
    let player =
        MediaPlayer::new().map_err(|error| format!("创建 Windows MediaPlayer 失败：{error}"))?;
    player
        .SetAudioCategory(MediaPlayerAudioCategory::Speech)
        .map_err(|error| format!("设置听书音频类别失败：{error}"))?;

    // 只有一个 MediaPlayer，因此只订阅一次。每次开播前写入 id，回调取出当前句子的 id。
    let active_playback = Arc::new(Mutex::new(None::<u64>));
    let callback_id = active_playback.clone();
    let event_tx = events.clone();
    let handler = TypedEventHandler::<MediaPlayer, IInspectable>::new(move |_, _| {
        let id = callback_id
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(id) = id {
            let _ = event_tx.try_send(BackendEvent::Done(id));
        }
        Ok(())
    });
    let token = player
        .MediaEnded(&handler)
        .map_err(|error| format!("订阅播放结束事件失败：{error}"))?;

    Ok(Box::new(Windows {
        _com: com,
        synthesizer,
        player,
        active_playback,
        _media_ended_token: token,
        voices: Vec::new(),
        rate: 1.0,
        pitch: 1.0,
        pending: Vec::new(),
    }))
}

/// COM 的初始化与释放必须由同一个对象配对；`S_FALSE` 表示本线程已经初始化过。
struct ComApartment(bool);

impl ComApartment {
    fn enter() -> Result<Self, String> {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        match hr.0 {
            0 => Ok(Self(true)),
            // S_FALSE：本线程已经初始化，不能在这里把它关掉。
            1 => Ok(Self(false)),
            // RPC_E_CHANGED_MODE：本线程已经是 MTA，不接管也不释放别人的 apartment。
            -2147417850 => Ok(Self(false)),
            _ => Err(format!("初始化 COM 失败：{hr}")),
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

enum Pending {
    Synthesizing {
        id: u64,
        operation: IAsyncOperation<SpeechSynthesisStream>,
    },
    /// 保留源对象直到播放结束，避免播放器引用被中途释放。
    Playing { _source: IMediaSource },
}

struct Windows {
    _com: ComApartment,
    synthesizer: SpeechSynthesizer,
    player: MediaPlayer,
    active_playback: Arc<Mutex<Option<u64>>>,
    _media_ended_token: i64,
    voices: Vec<(Voice, VoiceInformation)>,
    rate: f32,
    pitch: f32,
    pending: Vec<Pending>,
}

impl Backend for Windows {
    fn voices(&mut self) -> Result<Vec<Voice>, String> {
        let installed = SpeechSynthesizer::AllVoices()
            .map_err(|error| format!("列出 Windows 语音失败：{error}"))?;
        let mut voices = Vec::new();
        for voice in installed {
            let id = voice.Id().map(|id| id.to_string()).unwrap_or_else(|_| {
                voice
                    .DisplayName()
                    .map(|name| name.to_string())
                    .unwrap_or_default()
            });
            if id.is_empty() {
                continue;
            }
            let model = Voice {
                key: VoiceKey {
                    backend: BACKEND.into(),
                    engine: "system".into(),
                    id: id.clone(),
                },
                name: voice
                    .DisplayName()
                    .map(|name| name.to_string())
                    .unwrap_or_else(|_| id),
                language: voice
                    .Language()
                    .map(|language| language.to_string())
                    .unwrap_or_default(),
                // 系统声音来自本机已安装的语音包，不依赖网络。
                network_required: Some(false),
                installed: Some(true),
            };
            voices.push((model, voice));
        }
        self.voices = voices;
        Ok(self.voices.iter().map(|(voice, _)| voice.clone()).collect())
    }

    fn default_voice(&self) -> Option<VoiceKey> {
        self.voices.first().map(|(voice, _)| voice.key.clone())
    }

    fn select_voice(&mut self, key: &VoiceKey) -> Result<(), String> {
        let (_, voice) = self
            .voices
            .iter()
            .find(|(model, _)| &model.key == key)
            .ok_or_else(|| "所选 Windows 语音不可用".to_string())?;
        self.synthesizer
            .SetVoice(voice)
            .map_err(|error| format!("切换 Windows 语音失败：{error}"))
    }

    fn speak(&mut self, id: u64, text: &str) -> Result<(), String> {
        if text.contains('\0') {
            return Err("朗读文本含空字符".into());
        }
        let options = self
            .synthesizer
            .Options()
            .map_err(|error| format!("读取语音选项失败：{error}"))?;
        options
            .SetSpeakingRate(self.rate as f64)
            .map_err(|error| format!("设置语速失败：{error}"))?;
        // AudioPitch 以半音计；0.5× 到 2× 正好对应 -12 到 +12 个半音。
        options
            .SetAudioPitch(12.0 * (self.pitch as f64).log2())
            .map_err(|error| format!("设置音高失败：{error}"))?;
        let operation = self
            .synthesizer
            .SynthesizeTextToStreamAsync(&HSTRING::from(text))
            .map_err(|error| format!("合成语音失败：{error}"))?;
        self.pending.push(Pending::Synthesizing { id, operation });
        Ok(())
    }

    fn stop(&mut self) -> Result<(), String> {
        self.active_playback
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        self.player
            .Pause()
            .map_err(|error| format!("停止播放失败：{error}"))?;
        self.pending.clear();
        Ok(())
    }

    fn set_rate(&mut self, value: f32) -> Result<(), String> {
        if !value.is_finite() {
            return Err("语速不是有限数".into());
        }
        self.rate = value.clamp(0.5, 2.0);
        Ok(())
    }

    fn set_pitch(&mut self, value: f32) -> Result<(), String> {
        if !value.is_finite() {
            return Err("音高不是有限数".into());
        }
        self.pitch = value.clamp(0.5, 2.0);
        Ok(())
    }

    fn poll_events(&mut self) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        let mut i = 0;
        while i < self.pending.len() && i < MAX_PENDING_PER_POLL {
            let Pending::Synthesizing { id, operation } = &self.pending[i] else {
                i += 1;
                continue;
            };
            match operation.Status() {
                Ok(AsyncStatus::Completed) => match operation.GetResults() {
                    Ok(stream) => {
                        match play_stream(&self.player, &stream, &self.active_playback, *id) {
                            Ok(source) => {
                                events.push(BackendEvent::Begin(*id));
                                self.pending[i] = Pending::Playing { _source: source };
                            }
                            Err(error) => {
                                events.push(BackendEvent::Failed(*id, error));
                                self.pending.remove(i);
                                continue;
                            }
                        }
                    }
                    Err(error) => {
                        events.push(BackendEvent::Failed(
                            *id,
                            format!("取得语音流失败：{error}"),
                        ));
                        self.pending.remove(i);
                        continue;
                    }
                },
                Ok(AsyncStatus::Error) => {
                    events.push(BackendEvent::Failed(*id, "Windows 语音合成出错".into()));
                    self.pending.remove(i);
                    continue;
                }
                Ok(AsyncStatus::Canceled) => {
                    events.push(BackendEvent::Cancelled(*id));
                    self.pending.remove(i);
                    continue;
                }
                Ok(AsyncStatus::Started) | Ok(_) | Err(_) => {}
            }
            i += 1;
        }
        events
    }

    fn shutdown(&mut self) {
        let _ = self.player.Close();
        self.pending.clear();
        let _ = self.synthesizer.Close();
    }
}

fn play_stream(
    player: &MediaPlayer,
    stream: &SpeechSynthesisStream,
    active_playback: &Arc<Mutex<Option<u64>>>,
    id: u64,
) -> Result<IMediaSource, String> {
    // CloneStream 给播放器一份独立的随机访问流，避免合成流被播放端拿走后无法复用。
    let random = stream
        .CloneStream()
        .map_err(|error| format!("复制语音流失败：{error}"))?;
    random
        .Seek(0)
        .map_err(|error| format!("定位语音流失败：{error}"))?;
    let content_type = stream
        .ContentType()
        .unwrap_or_else(|_| HSTRING::from("audio/mpeg"));
    let source = MediaSource::CreateFromStream(&random, &content_type)
        .map_err(|error| format!("创建播放源失败：{error}"))?;
    active_playback
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .replace(id);
    let playback_source = source
        .cast::<IMediaSource>()
        .map_err(|error| format!("转换播放源失败：{error}"))?;
    player
        .SetMediaSource(&playback_source)
        .map_err(|error| format!("设置播放源失败：{error}"))?;
    player
        .Play()
        .map_err(|error| format!("开始播放失败：{error}"))?;
    Ok(playback_source)
}
