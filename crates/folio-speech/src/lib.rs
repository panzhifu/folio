//! 单线程语音后端；状态快照、选择确认与独占播放事件分别传递。

mod engine;
mod model;
mod voices;
mod worker;

#[cfg(all(target_os = "linux", not(target_env = "ohos")))]
mod linux;

pub use engine::{Engine, engine, install_backend, prewarm, shutdown};
pub use model::{
    Backend, BackendEvent, BackendFactory, Command, Delivery, Event, Snapshot, Status, Voice,
    VoiceKey,
};
pub use voices::resolve_voice;
