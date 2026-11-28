//! 桌面的音频后端：symphonia 惰性解码成样本流，rodio 出声。
//!
//! 解码是**迭代器式的**：rodio 的输出引擎推一帧，这里才解一帧的包——十分钟的
//! 音频不会整段展开进内存。播放位置由样本计数器报（[`SymphoniaSource`] 推进、
//! [`Shared`] 存档），暂停时引擎不再拉取，计数自然冻结。

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use rodio::{Sink, Source as _};
use symphonia::core::audio::{SampleBuffer, SignalSpec};
use symphonia::core::codecs::{CODEC_TYPE_NULL, CodecRegistry, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::default;

/// 主线程与播放线程之间共享的实时读数。UI 的状态快照全部现读这里，没有第二条
/// 需要对账的状态。
#[derive(Default)]
struct Shared {
    href: Mutex<Option<String>>,
    playing: AtomicBool,
    /// 已被输出引擎消费掉的样本数。[`SymphoniaSource`] 推着走。
    played: AtomicU64,
    /// 当前曲目的格式：`采样率 << 8 | 声道数`，0 = 没有曲目在播。
    format: AtomicU32,
    duration: Mutex<Option<Duration>>,
    error: Mutex<Option<String>>,
}

impl Shared {
    fn position(&self) -> Duration {
        let format = self.format.load(Ordering::Relaxed);
        if format == 0 {
            return Duration::ZERO;
        }
        let rate = (format >> 8) as u64;
        let channels = (format & 0xff) as u64;
        let per_sec = (rate * channels).max(1);
        Duration::from_secs_f64(self.played.load(Ordering::Relaxed) as f64 / per_sec as f64)
    }

    fn set_format(&self, rate: u32, channels: u16) {
        self.format
            .store((rate << 8) | channels as u32, Ordering::Relaxed);
    }
}

/// 主线程手里的桌面播放器。线程随它一起活：`drop` 后通道断开，播放线程退出。
pub(super) struct DesktopPlayer {
    shared: Arc<Shared>,
    cmd: Option<mpsc::Sender<Cmd>>,
}

enum Cmd {
    Play { href: String, bytes: Arc<[u8]> },
    Toggle,
    Stop,
}

impl DesktopPlayer {
    pub(super) fn new() -> Self {
        let shared = Arc::new(Shared::default());
        let cmd = spawn_worker(Arc::clone(&shared));
        Self {
            shared,
            cmd: Some(cmd),
        }
    }

    pub(super) fn play(&mut self, href: &str, bytes: Vec<u8>) {
        self.send(Cmd::Play {
            href: href.to_owned(),
            bytes: bytes.into(),
        });
    }

    pub(super) fn toggle(&mut self, _href: &str) {
        self.send(Cmd::Toggle);
    }

    pub(super) fn stop(&mut self) {
        self.send(Cmd::Stop);
    }

    pub(super) fn state(&self) -> super::audio::AudioState {
        super::audio::AudioState {
            current: self.shared.href.lock().map(|h| h.clone()).unwrap_or(None),
            playing: self.shared.playing.load(Ordering::Relaxed),
            position: self.shared.position(),
            duration: self.shared.duration.lock().map(|d| *d).unwrap_or(None),
            error: self.shared.error.lock().map(|e| e.clone()).unwrap_or(None),
        }
    }

    fn send(&self, cmd: Cmd) {
        if let Some(cmd_tx) = &self.cmd
            && cmd_tx.send(cmd).is_err()
        {
            // 播放线程已经没了（设备恢复失败之类）：状态里留着原因，
            // 下一次 play 重建不了就报错给读者看。
            *self
                .shared
                .error
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some("播放线程已退出，重启应用后可再试".to_owned());
        }
    }
}

/// 起播放线程。设备初始化失败不致命：错误进共享槽，播放指令进来后被直接
/// 报错丢掉，正文阅读完全不受影响。
fn spawn_worker(shared: Arc<Shared>) -> mpsc::Sender<Cmd> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    std::thread::Builder::new()
        .name("audio-playback".into())
        .spawn(move || {
            let output = rodio::OutputStream::try_default();
            let Ok((_stream, sink)) = output.map(|(stream, handle)| {
                (stream, Sink::try_new(&handle).expect("sink 新建不会失败"))
            }) else {
                *shared
                    .error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    Some("音频设备不可用".to_owned());
                // 通道排空到断开为止：主线程的指令全部落进错误槽。
                for cmd in cmd_rx {
                    if let Cmd::Play { href, .. } = cmd {
                        *shared
                            .href
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(href);
                        *shared
                            .error
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                            Some("音频设备不可用".to_owned());
                    }
                }
                return;
            };

            let mut queued: Option<Cmd> = None;
            loop {
                let cmd = queued.take().unwrap_or(match cmd_rx.recv() {
                    Ok(cmd) => cmd,
                    // 主线程放下了播放器：线程跟着退，sink 一并释放。
                    Err(_) => return,
                });
                match cmd {
                    Cmd::Play { href, bytes } => {
                        sink.clear();
                        shared.played.store(0, Ordering::Relaxed);
                        *shared
                            .href
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(href.clone());
                        *shared
                            .error
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                        match SymphoniaSource::new(bytes) {
                            Ok(source) => {
                                shared.set_format(source.rate, source.channels);
                                *shared
                                    .duration
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                    source.total_duration();
                                sink.append(source);
                                sink.play();
                                shared.playing.store(true, Ordering::Relaxed);
                                // 盯着这一条：播完、暂停/继续、换曲、停播。
                                loop {
                                    if sink.empty() {
                                        shared.playing.store(false, Ordering::Relaxed);
                                        break;
                                    }
                                    match cmd_rx.recv_timeout(Duration::from_millis(120)) {
                                        Ok(Cmd::Toggle) => {
                                            if sink.is_paused() {
                                                sink.play();
                                                shared.playing.store(true, Ordering::Relaxed);
                                            } else {
                                                sink.pause();
                                                shared.playing.store(false, Ordering::Relaxed);
                                            }
                                        }
                                        Ok(play @ Cmd::Play { .. }) => {
                                            queued = Some(play);
                                            break;
                                        }
                                        Ok(Cmd::Stop) => {
                                            sink.clear();
                                            shared.playing.store(false, Ordering::Relaxed);
                                            break;
                                        }
                                        // 超时＝没有任何指令，继续盯。
                                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                                            shared.playing.store(false, Ordering::Relaxed);
                                            return;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                *shared
                                    .error
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                    Some(format!("解码失败: {e}"));
                                shared.playing.store(false, Ordering::Relaxed);
                            }
                        }
                    }
                    Cmd::Toggle => {
                        // 空闲时没有可切的。
                    }
                    Cmd::Stop => {
                        sink.clear();
                        shared.playing.store(false, Ordering::Relaxed);
                    }
                }
            }
        })
        .expect("起音频线程");
    cmd_tx
}

/// symphonia 的惰性样本源：`next()` 推一个交错样本，包尽读下一包。
struct SymphoniaSource {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    pending: std::collections::VecDeque<f32>,
    buffer: SampleBuffer<f32>,
    rate: u32,
    channels: u16,
    total_frames: Option<u64>,
    played: Arc<AtomicU64>,
}

impl SymphoniaSource {
    fn new(bytes: Arc<[u8]>) -> Result<Self, String> {
        let media = MediaSourceStream::new(
            Box::new(Cursor::new(bytes)),
            symphonia::core::io::MediaSourceStreamOptions::default(),
        );
        let probed = default::get_probe()
            .format(
                &Hint::new(),
                media,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(|e| format!("{e:?}"))?;
        let format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or("没有音轨")?
            .clone();
        let rate = track.codec_params.sample_rate.ok_or("缺采样率")?;
        let channels = track
            .codec_params
            .channels
            .ok_or("缺声道数")?
            .count()
            .clamp(1, 8) as u16;
        let total_frames = track.codec_params.n_frames;
        let mut codec_registry = CodecRegistry::new();
        default::register_enabled_codecs(&mut codec_registry);
        let decoder = codec_registry
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| format!("{e:?}"))?;
        let buffer = SampleBuffer::<f32>::new(
            track.codec_params.max_frames_per_packet.unwrap_or(4096),
            SignalSpec::new(rate, track.codec_params.channels.unwrap_or_default()),
        );
        Ok(Self {
            format,
            decoder,
            pending: std::collections::VecDeque::new(),
            buffer,
            rate,
            channels,
            total_frames,
            played: Arc::new(AtomicU64::new(0)),
        })
    }

    /// 解下一包交错样本进 `pending`。坏包容忍：跳过继续，直到有样本或到头。
    fn decode_next_packet(&mut self) -> bool {
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::ResetRequired) | Err(SymphoniaError::IoError(_)) => {
                    return false;
                }
                Err(e) => {
                    log::warn!("audio: 读包失败 {e:?}");
                    return false;
                }
            };
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    self.buffer.copy_interleaved_ref(decoded);
                    self.pending.extend(
                        self.buffer
                            .samples()
                            .iter()
                            .filter(|s| s.is_finite())
                            .copied(),
                    );
                    return true;
                }
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(e) => {
                    log::warn!("audio: 解码失败 {e:?}");
                    return false;
                }
            }
        }
    }
}

impl Iterator for SymphoniaSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        while self.pending.is_empty() && self.decode_next_packet() {}
        let sample = self.pending.pop_front()?;
        self.played.fetch_add(1, Ordering::Relaxed);
        Some(sample)
    }
}

impl rodio::Source for SymphoniaSource {
    fn current_frame_len(&self) -> Option<usize> {
        // 与 rodio 自家的 SamplesBuffer 同一条：None = 引擎自己管帧界。
        None
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_frames
            .map(|frames| Duration::from_secs_f64(frames as f64 / self.rate as f64))
    }
}
