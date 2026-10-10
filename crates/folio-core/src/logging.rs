//! 日志：`tracing` 装在这里，**配置来自 [`crate::Config::logging`]**。
//!
//! 用现成的库而不是自己排一行文本：级别过滤、时间戳、字段渲染、span 耗时这些都有人做好了，
//! 而它们恰好就是调试时想看的东西。依赖树上也没多东西 —— `tracing` 与 `tracing-subscriber`
//! 都在 gpui 那条链里编过（`gpui-pre-ztracing` 用的就是同一份 `tracing-subscriber`），
//! 见根 `Cargo.toml` 上那条说明。
//!
//! 输出口由几层拼成，共用同一个 [`Targets`] 过滤器：
//!
//! ```text
//! 文件     <数据根>/logs/folio.log —— 开机轮转成 .1 / .2，本轮写满 `max_bytes` 就停手
//! stderr   桌面默认开（是终端才带色），移动平台默认关
//! 宿主     由平台入口递进来的那一层；Android 是 logcat（见 `ui::platform::android`）
//! ```
//!
//! 级别写在 `config.json` 的 `logging` 一节：**改完重启就生效，不用重新编译** ——
//! 这是把配置放进 `Config` 而不是只放环境变量的理由，设备上手改一份 JSON 比注入环境变量容易得多。
//!
//! ```json
//! { "logging": { "level": "debug", "targets": ["folio_core::layout=trace"] } }
//! ```
//!
//! `level` 只管 folio 自己的代码（`folio_core` / `folio_app`），依赖库固定在 warn 以上 ——
//! 否则「打开 debug」等于把整个 GPU 栈的日志一起打开。要连依赖一起看，用 [`LEVEL_ENV`]，
//! 它按 `RUST_LOG` 那套写法解释，并且压过配置里的同类项：
//!
//! ```sh
//! FOLIO_LOG=debug                 # 一切 debug，包括 wgpu
//! FOLIO_LOG=info,wgpu_core=error  # 只把 GPU 那几行按下去
//! ```
//!
//! 装的位置只有一个约束：**每个入口在拿到配置之后、任何一行日志之前**调一次 [`init`]。
//! `tracing` 全程只允许一个全局 subscriber，装第二次会失败 —— 调用方只需要不当心，
//! 日志装不上不影响程序起来。gpui 那份 `ztracing` 只在 profiler 构建里抢这个位置，
//! 常规桌面与 Android 构建里它是空函数，所以这个槽位是我们的。

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use tracing::Subscriber;
use tracing_subscriber::Registry;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::Logging;
use crate::error::Result;
use crate::paths;

/// 压过配置的那条环境变量，写法同 `RUST_LOG`。
pub const LEVEL_ENV: &str = "FOLIO_LOG";

/// folio 自己那两节的 target 前缀：`logging.level` 这一档只管它们。
const OWN_TARGETS: [&str; 2] = ["folio_core", "folio_app"];

/// 不是 folio 写的东西，只在 warn 以上说话。
const DEPS_LEVEL: LevelFilter = LevelFilter::WARN;

/// `logging.level` 认不出时的回落值。写坏一个词的代价是回到常见的这一档，不是没有日志。
const FALLBACK_LEVEL: LevelFilter = LevelFilter::INFO;

/// 平台自己那一层的类型（Android 的 logcat 就是这么递进来的）。
pub type BoxedLayer = Box<dyn Layer<Registry> + Send + Sync>;

/// 一条写法（`logging.targets` 的一项，或 [`LEVEL_ENV`] 里以逗号分隔的一段）解出来的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Directive {
    /// 光一个级别：管全局。
    Default(LevelFilter),
    /// `前缀=级别`：管这一支，以及以它为前缀的更深的模块。
    Target(String, LevelFilter),
    /// 认不出。留着原文是为了把话说清楚，而不是静默吞掉一条配置。
    Junk(String),
}

impl Directive {
    /// 解 `trace` / `folio_core::layout=debug` 这两种写法。
    pub fn parse(piece: &str) -> Self {
        let piece = piece.trim();
        match piece.split_once('=') {
            Some((target, level)) => {
                let (target, level) = (target.trim(), level.trim());
                match level.parse::<LevelFilter>() {
                    Ok(level) if !target.is_empty() => Self::Target(target.to_string(), level),
                    _ => Self::Junk(piece.to_string()),
                }
            }
            None if piece.is_empty() => Self::Default(FALLBACK_LEVEL),
            None => match piece.parse::<LevelFilter>() {
                Ok(level) => Self::Default(level),
                Err(_) => Self::Junk(piece.to_string()),
            },
        }
    }
}

/// 把配置的一节装配成过滤器，`FOLIO_LOG` 按实际环境读进来。
pub fn filter(logging: &Logging) -> Targets {
    let from_env = std::env::var(LEVEL_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty());
    build(logging, from_env.as_deref())
}

/// 配置里 `level` 那一格**实际生效**的那一档。
///
/// 空与认不出都按回落值算，所以启动现场报的是这个数，而不是原始字符串：
/// 读者手改坏一个词时，日志里要看到的是「现在是什么」，不是「你以为是什么」。
fn own_level(logging: &Logging) -> LevelFilter {
    // 空的那一格要自己判掉再交给 `LevelFilter`：它把**空串读成 `error`**（`RUST_LOG=` 的
    // 历史包袱），而配置里空着的 `level` 意思是「没说」，不是「只留 error」。
    match logging.level.trim() {
        "" => FALLBACK_LEVEL,
        text => text.parse::<LevelFilter>().unwrap_or(FALLBACK_LEVEL),
    }
}

/// [`filter`] 的「环境变量由外面给」那一份。拆开是因为测试不动全局环境：
/// 环境变量是进程级的，并发跑的用例里一改就会让别的用例读到错的那份（同 `paths` 的口径）。
fn build(logging: &Logging, env: Option<&str>) -> Targets {
    // 空片段先滤掉：`FOLIO_LOG=warn,` 尾巴那个空串会被 `Directive::parse` 读成「没说」，
    // 跟着落下去就把刚设的 warn 换回回落值。
    let pieces: Vec<&str> = env
        .map(|spec| {
            spec.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default();
    // 环境变量里**光一个级别**那一条，意思是「一切按这个档」，所以它连 folio 自己那一支
    // 一起改掉 —— 否则 `FOLIO_LOG=warn` 遇上配置里的 `level = debug` 仍然刷 debug，
    // 而写这条环境变量的人要的就是「安静下来」。点名的 `前缀=级别` 仍然最后落。
    let mut own = own_level(logging);
    for piece in &pieces {
        if let Directive::Default(level) = Directive::parse(piece) {
            own = level;
        }
    }

    let mut out = Targets::new().with_default(DEPS_LEVEL);
    for target in OWN_TARGETS {
        out = out.with_target(target, own);
    }
    out = apply(out, logging.targets.iter().map(String::as_str));
    // 后写的压先写的：一条 `FOLIO_LOG` 就能盖过设备上手改的那份 JSON。
    apply(out, pieces.iter().copied())
}

fn apply<'a>(mut out: Targets, pieces: impl IntoIterator<Item = &'a str>) -> Targets {
    for piece in pieces {
        match Directive::parse(piece) {
            Directive::Default(level) => out = out.with_default(level),
            Directive::Target(target, level) => out = out.with_target(target, level),
            Directive::Junk(text) => {
                eprintln!("日志配置里「{text}」看不懂，跳过那一条（要的是 `级别` 或 `前缀=级别`）")
            }
        }
    }
    out
}

/// 日志会落在哪个文件；`logging.file` 关掉时 `None`。
///
/// `dir` 为空就用 `<数据根>/logs`。`FOLIO_DATA_DIR` 仍然在最上面，因为它挪走的是整个数据根。
pub fn log_file(logging: &Logging) -> Option<PathBuf> {
    if !logging.file {
        return None;
    }
    let dir = logging.dir.trim();
    Some(if dir.is_empty() {
        paths::log_dir().join("folio.log")
    } else {
        Path::new(dir).join("folio.log")
    })
}

/// 把配置里的三层加平台那一层，拼成一个 subscriber。
///
/// 形状是「一条 `.with` 链」而不是 `Vec<Box<dyn Layer>>`，这不是审美：`Vec` 的层实现
/// 把成员层的 `Interest::never` **升级**成 `sometimes`（源码里那句注释叫 "Return highest
/// level of interest"），于是过滤器说「这个调用点关掉」没人听，依赖库的 TRACE 照样落笔 ——
/// 实机跑十几秒就刷掉半份日志文件，回归测试
/// [`the_built_subscriber_filters_the_dependency_noise`](tests::the_built_subscriber_filters_the_dependency_noise)
/// 钉住这一条。
///
/// 因此 `extra`（平台那一层，`dyn Layer<Registry>`）必须**最先**接：链一旦走过一层，
/// subscriber 的类型就成了 `Layered<…>`，而 `dyn Layer<Registry>` 只对 `Registry` 本身是层，
/// 对 `Layered<…>` 不是。剩下几层的类型编译期已知，所以能一路具体类型走到底。
fn subscriber(
    logging: &Logging,
    extra: Option<BoxedLayer>,
) -> impl Subscriber + Send + Sync + 'static {
    // stderr 只在真的是终端时上色：重定向进文件的那份带一堆 ESC 只会更难读。
    let ansi = io::stderr().is_terminal();
    Registry::default()
        .with(extra)
        // 过滤器紧跟其后：之后每一层拿到的都是它筛过的东西。
        .with(filter(logging))
        .with(if logging.stderr {
            Some(
                tracing_subscriber::fmt::layer()
                    .with_ansi(ansi)
                    .with_writer(io::stderr)
                    // `FmtSpan::CLOSE` 是「排版慢在哪一步」的唯一来源：span 结束时打一行
                    // `close time.busy=…`，所以 `#[instrument]` 过的函数不必自己掐表。
                    .with_span_events(FmtSpan::CLOSE),
            )
        } else {
            None
        })
        .with(log_file(logging).map(|path| {
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(LogFile::new(path, logging.max_bytes, logging.keep))
                .with_span_events(FmtSpan::CLOSE)
        }))
}

/// 装 subscriber。**每个入口调一次**，在拿到配置之后、任何一行日志之前。
///
/// `extra` 是平台自己那一层：Android 递一个把事件转给 logcat 的 `Layer` 进来，core 因此
/// 不需要知道每个平台的控制台长什么样。
pub fn init(logging: &Logging, extra: Option<BoxedLayer>) -> Result<()> {
    subscriber(logging, extra).try_init()?;

    // 从这里往后的日志才有去处，所以启动现场只能报在这里。
    report_startup(logging);
    Ok(())
}

/// 把「现在是什么配置」写一条。
///
/// 设备上敲不到 `ps`、看不到 cwd、也不知道环境变量传进去没有 —— 出问题先靠这几行确认
/// 日志到底装上了没有、写到哪儿、下限卡在哪。
fn report_startup(logging: &Logging) {
    tracing::info!(
        "folio {} · {} 构建 · {} {} · folio 的日志下限 {own}，依赖库 {DEPS_LEVEL}",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        std::env::consts::OS,
        std::env::consts::ARCH,
        own = own_level(logging),
    );
    tracing::info!(
        "配置 {} · 数据 {}",
        paths::config_path().display(),
        paths::data_dir().display()
    );
    match log_file(logging) {
        Some(path) => tracing::info!("日志文件 {}", path.display()),
        None => tracing::info!("不落盘（logging.file = false）"),
    }
    if !logging.targets.is_empty() {
        tracing::info!("按模块覆盖 {:?}", logging.targets);
    }
    if let Ok(spec) = std::env::var(LEVEL_ENV)
        && !spec.trim().is_empty()
    {
        tracing::info!("{LEVEL_ENV}={spec} 压过配置");
    }
}

/// 日志文件的输出口。
///
/// 不用 `tracing-appender`：它按**天**轮转，还要为 `non_blocking` 多拉一条
/// crossbeam-channel 并起一个后台线程。这里的规则只有两条，都不值得那点代价：
///
/// 1. **开机轮转**：第一次落笔前把上一次运行那份挪成 `.1`，历史依次后移，留 `keep` 份。
///    一次运行正好一个文件，`adb pull` 拿到的就是这次的现场。
/// 2. **本轮封顶**：写到 `max_bytes` 就停手并补一行说明，免得一次失控的循环把磁盘吃掉。
///
/// 直接写 `File` 不套 `BufWriter`：`fmt` 层每条事件 `write_all` 一次但**从不 flush**，
/// 缓冲里那几行会在崩溃时正好丢掉 —— 而那几行恰恰是要看的东西。
/// [`MakeWriter`] 就是 `tracing-subscriber` 留给这种场景的口子。
#[derive(Clone)]
pub struct LogFile {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    state: Mutex<Writing>,
}

enum Writing {
    /// 还没落过笔：第一次写时才建目录、轮转、开文件。
    Pending,
    Open {
        file: std::fs::File,
        written: u64,
    },
    /// 开不了，或者本轮已经写满。之后的 `write` 一律返回成功（假装写了）。
    Off,
}

impl LogFile {
    pub fn new(path: impl Into<PathBuf>, max_bytes: u64, keep: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                path: path.into(),
                max_bytes,
                keep,
                state: Mutex::new(Writing::Pending),
            }),
        }
    }

    /// `folio.log` 的第 `i` 份历史：`folio.log.1`、`folio.log.2`……
    fn numbered(&self, i: usize) -> PathBuf {
        let name = format!(
            "{}.{i}",
            self.inner
                .path
                .file_name()
                .map_or("", |n| n.to_str().unwrap_or(""))
        );
        self.inner.path.with_file_name(name)
    }

    /// 上一次那份挪走，开一份空的。
    ///
    /// 改名必须**从最老的那份开始**，否则 `.1` 先盖掉 `.2`，两轮之前那次的现场就没了。
    /// 改名失败要往外传：这时候若继续开当前文件，丢的是唯一那一份现场。
    fn rotate_and_open(&self) -> io::Result<Writing> {
        if let Some(dir) = self.inner.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if self.inner.path.exists() {
            for i in (1..self.inner.keep).rev() {
                let _ = std::fs::rename(self.numbered(i), self.numbered(i + 1));
            }
            if self.inner.keep > 0 {
                std::fs::rename(&self.inner.path, self.numbered(1))?;
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true);
        if self.inner.keep == 0 {
            // 「不留历史」读作「这一份只属于本轮」：那就截断重写，而不是往同一个文件里
            // 无限追加 —— 后者既不轮转，也没有上限。
            options.write(true).truncate(true);
        } else {
            options.append(true);
        }
        let file = options.open(&self.inner.path)?;
        Ok(Writing::Open { file, written: 0 })
    }

    fn append(&self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if matches!(*state, Writing::Off) {
            return Ok(buf.len());
        }
        if matches!(*state, Writing::Pending) {
            *state = match self.rotate_and_open() {
                Ok(opened) => opened,
                Err(err) => {
                    eprintln!(
                        "日志文件 {} 写不了，本轮不落盘：{err}",
                        self.inner.path.display()
                    );
                    Writing::Off
                }
            };
        }
        let Writing::Open { file, written } = &mut *state else {
            return Ok(buf.len());
        };
        if self.inner.max_bytes > 0 && *written + buf.len() as u64 > self.inner.max_bytes {
            let _ = writeln!(
                file,
                "…… 本轮已写 {written} 字节，到 `logging.max_bytes` 上限，停止写入"
            );
            *state = Writing::Off;
            return Ok(buf.len());
        }
        file.write_all(buf)?;
        *written += buf.len() as u64;
        Ok(buf.len())
    }
}

impl Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.append(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogFile {
    type Writer = LogFile;

    fn make_writer(&'a self) -> LogFile {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{Directive, LogFile, build, log_file, subscriber};
    use crate::config::Logging;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::Registry;
    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::fmt::MakeWriter as _;
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

    /// 一支 target 的五档级别各发一条。
    ///
    /// 只能写成宏：`tracing` 的 `target:` 要求**字面量**，运行时换个字符串换不掉
    /// 编译期定下的 Metadata。
    macro_rules! probe {
        ($target:literal) => {
            tracing::error!(target: $target, "error");
            tracing::warn!(target: $target, "warn");
            tracing::info!(target: $target, "info");
            tracing::debug!(target: $target, "debug");
            tracing::trace!(target: $target, "trace");
        };
    }

    /// 四支探针：自己的两支、界面的一支、依赖库的一支。
    macro_rules! probe_all {
        () => {
            || {
                probe!("folio_core::layout");
                probe!("folio_core::store");
                probe!("folio_app::ui::shelf");
                probe!("wgpu_core");
            }
        };
    }

    /// 把过滤器真的装成 subscriber、发一遍 `probes`、返回真的落了笔的那些 `目标@级别`。
    ///
    /// 问的是「装好之后到底有没有那一行」，而不是 `Targets` 的判断结果 —— 级别过滤真正的
    /// 形状是宏 → callsite → Interest → 层，中间任何一环没接上，屏幕上就是没有。
    ///
    /// `probes` 必须由调用方用 [`probe_all!`] 现场展开，不能在这个函数里发：callsite 是
    /// 编译期静态的，而它的 Interest 会被**缓存**，共用一份就等于第一个跑的测试替后面
    /// 所有测试定下结论。用 `set_default`（线程局部），所以并发用例互不影响，
    /// 也完全不碰进程的环境变量。
    fn captured(logging: &Logging, env: Option<&str>, probes: impl FnOnce()) -> Vec<String> {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let subscriber = Registry::default()
            .with(build(logging, env))
            .with(Capture(seen.clone()));
        {
            let _guard = tracing::subscriber::set_default(subscriber);
            probes();
        }
        std::mem::take(
            &mut *seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// 只记账、不输出的那层。
    struct Capture(Arc<Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> Layer<S> for Capture {
        fn on_event(&self, event: &tracing::Event<'_>, _cx: Context<'_, S>) {
            let meta = event.metadata();
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(format!("{}@{}", meta.target(), meta.level().as_str()));
        }
    }

    /// `level` 只管 folio 自己那一支，依赖库固定在 warn：这是「打开 debug」不会把整个
    /// GPU 栈一起打开的那道保障。
    #[test]
    fn the_config_level_covers_folio_only() {
        let logging = Logging {
            level: "trace".into(),
            ..Default::default()
        };
        let seen = captured(&logging, None, probe_all!());
        for mine in [
            "folio_core::layout",
            "folio_core::store",
            "folio_app::ui::shelf",
        ] {
            assert!(
                seen.contains(&format!("{mine}@TRACE")),
                "{mine} 该被打开：{seen:?}"
            );
        }
        let deps: Vec<&String> = seen
            .iter()
            .filter(|n| n.starts_with("wgpu_core@"))
            .collect();
        assert_eq!(
            deps,
            vec!["wgpu_core@ERROR", "wgpu_core@WARN"],
            "依赖库只留 warn 以上"
        );
    }

    /// 同一条前缀上，环境变量压过配置文件。
    #[test]
    fn an_env_directive_beats_the_configured_one() {
        let logging = Logging {
            level: "warn".into(),
            targets: vec!["folio_core::layout=info".into()],
            ..Default::default()
        };
        let by_config = captured(&logging, None, probe_all!());
        assert!(by_config.contains(&"folio_core::layout@INFO".to_string()));
        assert!(!by_config.contains(&"folio_core::layout@DEBUG".to_string()));

        let by_env = captured(&logging, Some("folio_core::layout=debug"), probe_all!());
        assert!(
            by_env.contains(&"folio_core::layout@DEBUG".to_string()),
            "{by_env:?}"
        );
    }

    /// `FOLIO_LOG=debug` 按 `RUST_LOG` 的意思解释：一切 debug，**包括依赖库**。
    /// 这是「配置里那一档只管自己」的另一面 —— 想越过去看别人，得明说。
    #[test]
    fn a_bare_env_level_reaches_the_dependencies_too() {
        let seen = captured(&Logging::default(), Some("debug"), probe_all!());
        assert!(seen.contains(&"wgpu_core@DEBUG".to_string()), "{seen:?}");
    }

    /// 点名的那一支打开，没点名的照旧。
    #[test]
    fn a_target_directive_opens_exactly_that_branch() {
        let logging = Logging {
            level: "warn".into(),
            targets: vec!["folio_core::store=debug".into()],
            ..Default::default()
        };
        let seen = captured(&logging, None, probe_all!());
        assert!(
            seen.contains(&"folio_core::store@DEBUG".to_string()),
            "{seen:?}"
        );
        assert!(!seen.contains(&"folio_core::layout@DEBUG".to_string()));
    }

    /// 一条写坏只带走它自己，同批其余照常生效。
    #[test]
    fn one_bad_piece_costs_only_itself() {
        let logging = Logging {
            level: "off".into(),
            targets: vec!["junk".into(), "folio_core::layout=trace".into()],
            ..Default::default()
        };
        let seen = captured(&logging, None, probe_all!());
        assert!(seen.contains(&"folio_core::layout@TRACE".to_string()));
        assert!(
            !seen.iter().any(|n| n.starts_with("folio_core::store@")),
            "{seen:?}"
        );
    }

    /// 级别那一格写坏（手滑的 `verbose`）回落 info，而不是整档关掉 —— 没有日志的
    /// 程序比日志少一档的程序难查得多。
    #[test]
    fn an_unreadable_level_falls_back_instead_of_going_silent() {
        let logging = Logging {
            level: "verbose".into(),
            ..Default::default()
        };
        let seen = captured(&logging, None, probe_all!());
        assert!(seen.contains(&"folio_core::store@INFO".to_string()));
        assert!(!seen.contains(&"folio_core::store@DEBUG".to_string()));
    }

    /// `"off"` 是一档合法取值：调试时想彻底安静，不该被迫去关 `file` / `stderr`。
    #[test]
    fn off_silences_folios_own_code() {
        let logging = Logging {
            level: "off".into(),
            ..Default::default()
        };
        let seen = captured(&logging, None, probe_all!());
        assert!(!seen.iter().any(|n| n.starts_with("folio_")), "{seen:?}");
    }

    #[test]
    fn the_four_writings_parse_and_the_rest_stays_visible() {
        assert_eq!(
            Directive::parse("trace"),
            Directive::Default(LevelFilter::TRACE)
        );
        assert_eq!(
            Directive::parse(" folio_core::layout = debug "),
            Directive::Target("folio_core::layout".into(), LevelFilter::DEBUG)
        );
        assert_eq!(
            Directive::parse("nonsense"),
            Directive::Junk("nonsense".into())
        );
        assert_eq!(
            Directive::parse("folio_core=maybe"),
            Directive::Junk("folio_core=maybe".into())
        );
        // 空的那一条不是「关掉」，是「没说」—— 回落常见的那一档。
        assert_eq!(Directive::parse(""), Directive::Default(LevelFilter::INFO));
    }

    #[test]
    fn the_log_file_follows_the_dir_field_but_defaults_under_the_data_root() {
        let logging = Logging {
            file: true,
            ..Default::default()
        };
        assert_eq!(
            log_file(&logging),
            Some(crate::paths::log_dir().join("folio.log"))
        );
        let logging = Logging {
            dir: "/tmp/folio-logs".into(),
            ..logging
        };
        assert_eq!(
            log_file(&logging),
            Some(std::path::PathBuf::from("/tmp/folio-logs/folio.log"))
        );
        assert_eq!(
            log_file(&Logging {
                file: false,
                ..logging
            }),
            None,
            "关掉就没有文件这一层"
        );
    }

    /// 空串那一条走的是 `build` 里 `FALLBACK_LEVEL` 那个分支，与 `"verbose"` 同一档。
    #[test]
    fn an_empty_level_is_the_fallback_one() {
        let logging = Logging {
            level: "  ".into(),
            ..Default::default()
        };
        let seen = captured(&logging, None, probe_all!());
        assert!(
            seen.contains(&"folio_core::store@INFO".to_string()),
            "{seen:?}"
        );
    }

    /// 开机轮转：上一次运行那份让位给 `.1`，新的从空开始。
    #[test]
    fn the_previous_run_moves_aside_before_the_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folio.log");
        std::fs::write(&path, "上一轮").unwrap();

        LogFile::new(&path, 0, 3)
            .make_writer()
            .write_all("这一轮".as_bytes())
            .unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "这一轮");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("folio.log.1")).unwrap(),
            "上一轮"
        );
    }

    /// 历史是从老往新挪的：`keep = 2` 时最老那份被挤掉，而不是把 `.2` 盖住又丢掉。
    #[test]
    fn history_shifts_from_the_oldest_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folio.log");
        for run in 1..=4 {
            std::fs::write(&path, format!("第{run}轮")).unwrap();
            LogFile::new(&path, 0, 2)
                .make_writer()
                .write_all(b"x")
                .unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("folio.log.1")).unwrap(),
            "第4轮"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("folio.log.2")).unwrap(),
            "第3轮"
        );
        assert!(
            !std::fs::exists(dir.path().join("folio.log.3")).unwrap(),
            "只留 keep 份"
        );
    }

    /// `keep = 0` 是「这一份只属于本轮」：旧的直接截掉，也不产生 `.1`。
    #[test]
    fn keeping_no_history_overwrites_the_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folio.log");
        std::fs::write(&path, "上一轮").unwrap();
        LogFile::new(&path, 0, 0)
            .make_writer()
            .write_all("这一轮".as_bytes())
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "这一轮");
        assert!(!std::fs::exists(dir.path().join("folio.log.1")).unwrap());
    }

    /// 写满之后**停手**而不是继续长 —— 这是「一次失控的 debug 循环吃掉磁盘」的兜底，
    /// 而且要在文件里说一句为什么停了，不然读日志的人会以为程序没做事。
    #[test]
    fn a_run_stops_writing_at_its_own_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folio.log");
        let mut sink = LogFile::new(&path, 30, 1).make_writer();
        for n in 0..50 {
            sink.write_all(format!("第{n}行，够长的内容\n").as_bytes())
                .unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.len() < 130, "封顶没起作用：{} 字节", text.len());
        assert!(text.contains("停止写入"), "要留下为什么说停了：{text}");
    }

    /// 目录建不出来（只读路径）时只是没有文件日志，程序照跑：`write` 返回成功，
    /// 因为日志写不了不该让排版那条路径报错。
    #[test]
    fn an_unwritable_path_costs_the_file_and_nothing_else() {
        let path = std::path::Path::new("/proc/definitely-not-writable/folio.log");
        let mut sink = LogFile::new(path, 0, 1).make_writer();
        assert!(sink.write_all("照样返回成功".as_bytes()).is_ok());
        assert!(sink.write_all("第二次也不报错".as_bytes()).is_ok());
    }

    /// `fmt` 层每条事件都重新 `make_writer` 一份：状态必须是共享的，
    /// 否则每轮都会重新轮转一次、只留下最后一行。
    #[test]
    fn the_shared_state_survives_a_fresh_writer_per_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("folio.log");
        let sink = LogFile::new(&path, 0, 1);
        for _ in 0..3 {
            sink.make_writer().write_all(b"ab").unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ababab");
    }

    /// `FOLIO_LOG=warn` 遇上配置里的 `level = debug` 仍然要安静下来 —— 写这条环境变量的人
    /// 要的就是「别再刷了」，所以**光一个级别**那一条连 folio 自己那一支一起改掉。
    /// 点名的 `前缀=级别` 仍然照旧只影响那一支。
    #[test]
    fn a_bare_env_level_quiets_folios_own_code_as_well() {
        let logging = Logging {
            level: "debug".into(),
            ..Default::default()
        };
        let seen = captured(&logging, Some("warn"), probe_all!());
        assert!(seen.contains(&"folio_core::store@WARN".to_string()));
        assert!(
            !seen
                .iter()
                .any(|n| n.starts_with("folio_core::store@DEBUG")),
            "warn 没压过配置的 debug：{seen:?}"
        );
    }

    /// 尾巴多打的一个逗号不该把刚设的档位换回回落值（空片段会被读成「没说」）。
    #[test]
    fn a_trailing_comma_costs_nothing() {
        let logging = Logging {
            level: "debug".into(),
            ..Default::default()
        };
        let seen = captured(&logging, Some("warn,"), probe_all!());
        assert!(
            !seen.iter().any(|n| n.starts_with("folio_app@DEBUG")),
            "{seen:?}"
        );
    }

    /// 过滤器必须**真的**管得住依赖库，而不是「配置里说了、装配时没听见」。
    ///
    /// 走 `subscriber()` 这个生产入口而不是手拼层，是因为这一份曾经把三层装进
    /// `Vec<Box<dyn Layer>>`：`Vec` 不把成员层的 `Interest::never` 往外报，于是
    /// `level = warn` 的下限管不住 zbus / calloop 的 TRACE，实机跑十几秒就刷掉半份日志文件。
    /// `Option` 与元组那两层转发得老实 —— 这条就是钉住这件事。
    #[test]
    fn the_built_subscriber_filters_the_dependency_noise() {
        let dir = tempfile::tempdir().unwrap();
        let logging = Logging {
            level: "warn".into(),
            dir: dir.path().to_string_lossy().into_owned(),
            stderr: false,
            ..Default::default()
        };
        {
            let _guard = tracing::subscriber::set_default(subscriber(&logging, None));
            tracing::trace!(target: "zz_dependency", "这行不该进文件");
            tracing::warn!(target: "zz_dependency", "这行该进");
        }
        let text = std::fs::read_to_string(dir.path().join("folio.log")).unwrap();
        assert!(
            !text.contains("这行不该进文件"),
            "warn 的下限没管住依赖的 TRACE：{text}"
        );
        assert!(text.contains("这行该进"), "该留的那行也没留下：{text}");
    }
}
