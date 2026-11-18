use std::sync::Arc;

use async_channel::Sender;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct VoiceKey {
    pub backend: String,
    pub engine: String,
    pub id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Voice {
    pub key: VoiceKey,
    pub name: String,
    pub language: String,
    pub network_required: Option<bool>,
    pub installed: Option<bool>,
}

impl Voice {
    pub fn is_local(&self) -> bool {
        self.network_required == Some(false) && self.installed == Some(true)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Status {
    Initializing,
    Ready,
    Failed(String),
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub status: Status,
    pub voices: Vec<Voice>,
    pub default_voice: Option<VoiceKey>,
    pub selected_voice: Option<VoiceKey>,
    pub revision: u64,
    pub last_error: Option<String>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            status: Status::Initializing,
            voices: Vec::new(),
            default_voice: None,
            selected_voice: None,
            revision: 0,
            last_error: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Speak {
        text: String,
    },
    SpeakWithVoice {
        text: String,
        voice: Option<VoiceKey>,
        language: String,
        rate: f32,
        pitch: f32,
    },
    Stop,
    /// 语义倍率，范围 0.5–2.0；1.0 为常速/常调。
    SetRate(f32),
    SetPitch(f32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    Ready(bool),
    Begin,
    Done,
    Failed(String),
    /// 仅用户手动选择发送；逐句角色切换不打断阅读会话。
    VoiceChanged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delivery {
    /// None 表示引擎生命周期事件；播放事件归属提交时的代数。
    pub generation: Option<u64>,
    pub event: Event,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendEvent {
    Begin(u64),
    Done(u64),
    Failed(u64, String),
    Cancelled(u64),
}

/// 工厂在专职线程构造后端，应在有限时间内返回；句柄不要求 Send。
pub type BackendFactory =
    Arc<dyn Fn(Sender<BackendEvent>) -> Result<Box<dyn Backend>, String> + Send + Sync>;

pub trait Backend {
    fn voices(&mut self) -> Result<Vec<Voice>, String>;
    fn default_voice(&self) -> Option<VoiceKey>;
    fn select_voice(&mut self, key: &VoiceKey) -> Result<(), String>;
    fn speak(&mut self, id: u64, text: &str) -> Result<(), String>;
    fn stop(&mut self) -> Result<(), String>;
    fn set_rate(&mut self, value: f32) -> Result<(), String>;
    fn set_pitch(&mut self, value: f32) -> Result<(), String>;
    fn poll_events(&mut self) -> Vec<BackendEvent> {
        Vec::new()
    }
    fn shutdown(&mut self);
}
