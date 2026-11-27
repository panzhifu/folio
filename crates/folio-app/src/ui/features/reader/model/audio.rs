//! 内嵌音频的播放服务：书的容器里带着音频字节，这条线负责「点一下就响」。
//!
//! 分层与平台口径：
//! - 排版层只给几何（`audio_row` 模块，48px 的块级行）；播放状态住这一层，
//!   改了不许重排。
//! - 后端按平台三叉（见 [`Backend`]）：桌面走 `audio_desktop` 的 rodio+symphonia；
//!   Android/iOS 走系统播放器（gpui-pre-mobile 的 `packages::audio`，Android 侧是
//!   `GpuiAudio.java` 的 MediaPlayer——解码、音频焦点、省电都交给系统）；OHOS 的
//!   ArkTS 音频桥尚未接入，表现为后端不可用。
//! - 容器内的音频字节在移动端要先落到 cache 目录的临时文件（系统播放器只认
//!   路径/URL），换曲与停播时尽力清理。

use std::time::Duration;

use gpui_kit::{Context, Pixels, Point, Window};

/// 播放状态的只读快照。绘制播放行、点击切换全靠这一份。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct AudioState {
    /// 正在（或最后一次）播放的资源 href；`None` = 空闲。
    pub current: Option<String>,
    pub playing: bool,
    /// 已播时长。暂停时冻结；seek 一期待进度条拖拽一起做。
    pub position: Duration,
    pub duration: Option<Duration>,
    /// 后端初始化或解码失败的原因。有值时点击不再重试，读者能看到「播不了」。
    pub error: Option<String>,
}

/// 面向 UI 的播放入口。平台差异全部关在后端里，UI 只认这四个方法加一个快照。
pub(crate) struct AudioPlayer {
    backend: Backend,
}

impl AudioPlayer {
    pub(crate) fn new() -> Self {
        Self {
            backend: Backend::select(),
        }
    }

    /// 开播（或换曲）。`bytes` 是容器里那份音频的完整字节。
    pub(crate) fn play(&mut self, href: &str, bytes: Vec<u8>) {
        self.backend.play(href, bytes);
    }

    /// 点击已在播（或已暂停）的那一条：播放中转暂停，暂停转继续。
    pub(crate) fn toggle(&mut self, href: &str) {
        self.backend.toggle(href);
    }

    /// 停播并清掉当前曲目。
    pub(crate) fn stop(&mut self) {
        self.backend.stop();
    }

    /// 当前的状态快照。桌面端的进度由共享计数器现读，移动端现问系统播放器。
    pub(crate) fn state(&self) -> AudioState {
        self.backend.state()
    }
}

impl crate::ui::features::reader::model::session::ReaderSession {
    /// 当前的音频状态快照（绘制侧读这一份：按钮的形状、进度条的长短）。
    pub(crate) fn audio_state(&self) -> AudioState {
        self.audio.state()
    }

    /// 这一击若落在音频行上，就切换它的播放，返回这一击是否被音频吃掉。
    ///
    /// 判定在 [`Document::audio_href_at`](crate::ui::features::reader::model::chapter_layout::Document::audio_href_at)
    /// （块盒几何）；字节经 `opened().resource_bytes` 从容器现取 —— 取不到就吃掉
    /// 这一击但不响（读者看见的还是静止的行，日志里有原因）。同一行再点一次是
    /// 暂停/续播，另一行是换曲。
    pub fn toggle_audio_at(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        at: Point<Pixels>,
    ) -> bool {
        let Some((chapter, x, y)) = self.to_layout(window, at) else {
            return false;
        };
        let Some(href) = self
            .book()
            .and_then(|book| book.span_doc(chapter))
            .and_then(|doc| doc.audio_href_at(x, y))
        else {
            return false;
        };
        // 同一条再点一次 = 暂停/续播；点另一条 = 换曲开播。
        if self.audio_state().current.as_deref() == Some(href.as_str()) {
            self.audio.toggle(&href);
        } else {
            let Some(bytes) = self
                .book()
                .map(|book| book.opened())
                .and_then(|doc| doc.resource_bytes(&href))
            else {
                tracing::warn!("音频资源取不到字节，忽略点击: {href}");
                return true;
            };
            self.audio.play(&href, bytes);
            self.keep_audio_row_fresh(cx);
        }
        cx.notify();
        true
    }

    /// 播放中驱动重绘：进度条与按钮形状跟着 state 走，每 250ms 一拍；
    /// 暂停/停播/换书（`playing` 为假）就收拍。`speech.rs` 等排版那一路同款写法。
    fn keep_audio_row_fresh(&mut self, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let playing = weak
                    .update(cx, |session, cx| {
                        let playing = session.audio_state().playing;
                        if playing {
                            cx.notify();
                        }
                        playing
                    })
                    .unwrap_or(false);
                if !playing {
                    break;
                }
            }
        })
        .detach();
    }
}

enum Backend {
    /// 桌面：rodio 出声 + symphonia 解码。
    #[cfg(not(any(target_os = "android", target_os = "ios", target_env = "ohos")))]
    Desktop(crate::ui::features::reader::model::audio_desktop::DesktopPlayer),
    /// Android/iOS：系统播放器（经 gpui-pre-mobile 的音频包，lib 名是 gpui_mobile）。
    #[cfg(any(target_os = "android", target_os = "ios"))]
    Mobile(mobile::MobilePlayer),
    /// OHOS 等尚未接音频桥的平台。
    #[cfg(target_env = "ohos")]
    Unsupported,
}

impl Backend {
    fn select() -> Self {
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            Self::Mobile(mobile::MobilePlayer::new())
        }
        #[cfg(not(any(target_os = "android", target_os = "ios", target_env = "ohos")))]
        {
            Self::Desktop(crate::ui::features::reader::model::audio_desktop::DesktopPlayer::new())
        }
        #[cfg(target_env = "ohos")]
        {
            Self::Unsupported
        }
    }

    fn play(&mut self, href: &str, bytes: Vec<u8>) {
        match self {
            #[cfg(not(any(target_os = "android", target_os = "ios", target_env = "ohos")))]
            Backend::Desktop(backend) => backend.play(href, bytes),
            #[cfg(any(target_os = "android", target_os = "ios"))]
            Backend::Mobile(backend) => backend.play(href, bytes),
            #[cfg(target_env = "ohos")]
            Backend::Unsupported => {}
        }
    }

    fn toggle(&mut self, href: &str) {
        match self {
            #[cfg(not(any(target_os = "android", target_os = "ios", target_env = "ohos")))]
            Backend::Desktop(backend) => backend.toggle(href),
            #[cfg(any(target_os = "android", target_os = "ios"))]
            Backend::Mobile(backend) => backend.toggle(href),
            #[cfg(target_env = "ohos")]
            Backend::Unsupported => {}
        }
    }

    fn stop(&mut self) {
        match self {
            #[cfg(not(any(target_os = "android", target_os = "ios", target_env = "ohos")))]
            Backend::Desktop(backend) => backend.stop(),
            #[cfg(any(target_os = "android", target_os = "ios"))]
            Backend::Mobile(backend) => backend.stop(),
            #[cfg(target_env = "ohos")]
            Backend::Unsupported => {}
        }
    }

    fn state(&self) -> AudioState {
        match self {
            #[cfg(not(any(target_os = "android", target_os = "ios", target_env = "ohos")))]
            Backend::Desktop(backend) => backend.state(),
            #[cfg(any(target_os = "android", target_os = "ios"))]
            Backend::Mobile(backend) => backend.state(),
            #[cfg(target_env = "ohos")]
            Backend::Unsupported => AudioState::default(),
        }
    }
}

// ---- Android/iOS：系统播放器 + 容器字节的临时文件交接 ----

#[cfg(any(target_os = "android", target_os = "ios"))]
mod mobile {
    use std::path::PathBuf;

    use super::{AudioState, Duration};

    /// 系统播放器的一层薄壳：管临时文件的生命周期与状态缓存。
    pub(super) struct MobilePlayer {
        /// 后端实例。构造失败（GpuiAudio 建不出来）时为 None，错误进 last_error。
        inner: Option<gpui_mobile::packages::audio::AudioPlayer>,
        /// 当前曲目的资源 href（临时文件名由它派生，换曲时旧文件一并清）。
        current: Option<String>,
        /// 交给系统播放器的那个临时文件；换曲或停播时删。
        temp: Option<PathBuf>,
        last_error: Option<String>,
        /// gpui-pre-mobile 的播放器在停播后仍保留实例；记一个「上一条播完了」
        /// 的本地旗子，避免每次 state() 都追问系统。
        finished: bool,
    }

    impl MobilePlayer {
        pub(super) fn new() -> Self {
            // fork 的 AudioPlayer::new() 会建 JNI 侧的播放器实例，失败要落到
            // state().error 让读者看见「播不了」，不能 panic。
            let (inner, last_error) = match gpui_mobile::packages::audio::AudioPlayer::new() {
                Ok(player) => (Some(player), None),
                Err(e) => (None, Some(e)),
            };
            Self {
                inner,
                current: None,
                temp: None,
                last_error,
                finished: false,
            }
        }

        pub(super) fn play(&mut self, href: &str, bytes: Vec<u8>) {
            if self.inner.is_none() {
                // 构造已失败：last_error 带着 new() 的错误，界面按「播不了」呈现。
                return;
            }
            match self.write_temp(href, &bytes) {
                Ok(temp) => {
                    self.drop_temp();
                    self.temp = Some(temp.clone());
                    if let Some(inner) = self.inner.as_ref() {
                        match inner.set_file_path(&temp.to_string_lossy()) {
                            Ok(_) => {
                                let played = inner.play().is_ok();
                                self.current = Some(href.to_owned());
                                self.finished = !played;
                                self.last_error = None;
                            }
                            Err(e) => {
                                self.last_error = Some(e);
                                self.current = Some(href.to_owned());
                            }
                        }
                    }
                }
                Err(e) => {
                    self.last_error = Some(e);
                    self.current = Some(href.to_owned());
                }
            }
        }

        pub(super) fn toggle(&mut self, href: &str) {
            if self.current.as_deref() != Some(href) {
                return;
            }
            // 播完的那条不再 toggle（重听走 play 换曲入口）。
            if self.finished {
                return;
            }
            let Some(inner) = self.inner.as_ref() else {
                return;
            };
            let playing = inner.is_playing().unwrap_or(false);
            let result = if playing { inner.pause() } else { inner.play() };
            self.last_error = result.err();
        }

        pub(super) fn stop(&mut self) {
            if let Some(inner) = self.inner.as_ref() {
                let _ = inner.stop();
            }
            self.drop_temp();
            self.current = None;
            self.finished = false;
        }

        pub(super) fn state(&self) -> AudioState {
            let playing = !self.finished
                && self.current.is_some()
                && self
                    .inner
                    .as_ref()
                    .map(|inner| inner.is_playing().unwrap_or(false))
                    .unwrap_or(false);
            AudioState {
                current: self.current.clone(),
                playing,
                position: Duration::from_millis(
                    self.inner
                        .as_ref()
                        .and_then(|inner| inner.position().ok())
                        .unwrap_or_default()
                        .min(u64::MAX / 2),
                ),
                duration: self
                    .inner
                    .as_ref()
                    .and_then(|inner| inner.duration().ok())
                    .map(|ms| Duration::from_millis(ms.min(u64::MAX / 2))),
                error: self.last_error.clone(),
            }
        }

        /// 音频字节落 cache 目录的临时文件。文件名取 href 的稳定散列——同名换曲
        /// 直接覆盖，不会越积越多。
        #[cfg(target_os = "android")]
        fn write_temp(&self, href: &str, bytes: &[u8]) -> Result<PathBuf, String> {
            use gpui_mobile::android::jni::{activity, with_env};

            let dir = with_env(|env| {
                let activity = activity(env)?;
                let file = env
                    .call_method(
                        &activity,
                        ::jni::jni_str!("getCacheDir"),
                        ::jni::jni_sig!("()Ljava/io/File;"),
                        &[],
                    )
                    .map_err(|e| format!("getCacheDir: {e:?}"))?
                    .l()
                    .map_err(|e| format!("getCacheDir: {e:?}"))?;
                let path = env
                    .call_method(
                        &file,
                        ::jni::jni_str!("getAbsolutePath"),
                        ::jni::jni_sig!("()Ljava/lang/String;"),
                        &[],
                    )
                    .map_err(|e| format!("cache path: {e:?}"))?
                    .l()
                    .map_err(|e| format!("cache path: {e:?}"))?;
                Ok(gpui_mobile::android::jni::get_string(env, &path))
            })?;
            let name = format!(
                "book-audio-{:016x}.audio",
                {
                    use std::hash::{Hash, Hasher};
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    href.hash(&mut hasher);
                    hasher.finish()
                }
            );
            let temp = PathBuf::from(dir).join(name);
            std::fs::write(&temp, bytes).map_err(|e| format!("临时文件写入失败: {e}"))?;
            Ok(temp)
        }

        #[cfg(target_os = "ios")]
        fn write_temp(&self, _href: &str, _bytes: &[u8]) -> Result<PathBuf, String> {
            // iOS 臂的 cache 目录要经它自己的平台助手取；接 iOS 播放那一轮一起做。
            Err("iOS 的音频临时文件路径尚未接入".to_owned())
        }

        fn drop_temp(&mut self) {
            if let Some(temp) = self.temp.take() {
                let _ = std::fs::remove_file(temp);
            }
        }
    }

    impl Drop for MobilePlayer {
        fn drop(&mut self) {
            self.stop();
        }
    }
}

/// 离开阅读页 = 停听 + 停播。谁在念跟着那一屏走，不跟着进程走。
///
/// （impl 原在 `speech.rs`，为行数基线拆到这一侧 —— 停播正是这里的状态；停听那半
/// 句转发给 [`speech`] 的收摊，通知与前台服务一并撤。）
///
/// [`speech`]: crate::ui::features::reader::model::speech
impl Drop for crate::ui::features::reader::model::session::ReaderSession {
    fn drop(&mut self) {
        self.stop_speech();
        self.audio.stop();
    }
}
