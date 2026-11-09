//! 日志：`tracing` 装在这里，**配置来自 `config.json` 的 `logging` 一节**（[`Logging`]）。
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
//! `level` 只管 folio 自己的代码（`folio_base` / `folio_core` / `folio_app`），依赖库固定在 warn 以上 ——
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

use serde::{Deserialize, Serialize};
use tracing::Subscriber;
use tracing_subscriber::Registry;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::{Layer, SubscriberExt};

use crate::error::Result;
use crate::paths;

/// 压过配置的那条环境变量，写法同 `RUST_LOG`。
pub const LEVEL_ENV: &str = "FOLIO_LOG";

/// folio 自己那几节的 target 前缀：`logging.level` 这一档只管它们。
///
/// 公开出去，是因为 Android 那层 logcat 也得按同一份口径挡一遍 —— 依赖库走 `log` facade
/// 直接落到 `android_logger`，**不经过**这里的 tracing 过滤，不挡的话 html5ever 会逐 token
/// 写 logcat（实测把一次导入从 0.3 s 拖到 21 s）。见 `ui::platform::android`。
///
/// 新开一个 folio crate 时要在这里加一格：漏了，它自己的 info / debug 会被当成依赖库
/// 压到 warn 以上 —— 症状是「这一层的日志在 debug 档下静默」。
pub const OWN_TARGETS: [&str; 3] = ["folio_base", "folio_core", "folio_app"];

/// 不是 folio 写的东西，只在 warn 以上说话。
pub const DEPS_LEVEL: LevelFilter = LevelFilter::WARN;

/// `logging.level` 认不出时的回落值。写坏一个词的代价是回到常见的这一档，不是没有日志。
const FALLBACK_LEVEL: LevelFilter = LevelFilter::INFO;

/// 日志：调试时要不要落盘、写到哪儿、每一支代码说到多细。
///
/// 这一节住在 `folio-base` 而不是配置的 schema 里：装配 subscriber 的代码要读它，
/// 而地基不能反过来依赖 `folio-core` 的配置模块。`Config.logging` 用的就是这个类型，
/// 落盘位置仍是 `config.json` 的 `logging` 一节。
///
/// 这一节存在理由是**不改代码就能调级**：真机上出问题往往是「这次什么都不显示」，
/// 而重新编一遍 Android 是十几分钟加一次签名重装。手改 `config.json` 里的一行、重启，
/// 就拿到现场了。（要连依赖库一起打开，仍然有 `FOLIO_LOG` 那条环境变量，见 [`crate::logging`]。）
///
/// 默认值分平台：桌面 stderr 本来就开着，而移动平台上没人对着控制台。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Logging {
    /// folio 自己的代码（`folio_base` / `folio_core` / `folio_app`）说到多细：
    /// `off` `error` `warn` `info` `debug` `trace`。**依赖库不跟着走**，它们固定在
    /// `warn` 以上 —— 否则「打开 debug」等于把整个 GPU 栈的日志一起打开，两秒就写满一份文件。
    /// 认不出的值回落 `info`，不是整档关掉。
    pub level: String,
    /// 按模块覆盖，写法同 `RUST_LOG` 的逗号分隔那一段：`"folio_core::layout=trace"`。
    /// 前缀要**整段对上模块边界**，最深的那一条赢。
    pub targets: Vec<String>,
    /// 落盘到 `<数据根>/logs/folio.log`（或 [`Self::dir`] 指的地点）。
    pub file: bool,
    /// 同时写 stderr。桌面默认开（是终端才带颜色），移动平台默认关。
    pub stderr: bool,
    /// 日志目录。空串 = 用默认的 `<数据根>/logs`。
    ///
    /// 单独给一格是因为设备上的数据根常常拿不到：Android 上把它指到
    /// `/storage/emulated/0/Android/data/dev.folio.reader/files` 那一类位置，
    /// `adb pull` 直接就能取走。
    pub dir: String,
    /// 一次运行最多写多少字节，写满就停手（`0` = 不限）。
    ///
    /// 上限是「本轮」的，不是文件的：上一次运行那份在开机时被挪成 `.1`，
    /// 所以磁盘上总量是这个数乘以 [`Self::keep`] 加一。
    pub max_bytes: u64,
    /// 保留几份历史运行（`.1`、`.2`……）。`0` 就只留当前这一份。
    pub keep: usize,
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            // debug 构建默认开到 debug：本地跑一次就该看见排版与容器里发生了什么。
            // release 默认 info：只留生命周期与「静默降级」那几条，不刷正文。
            level: if cfg!(debug_assertions) {
                "debug"
            } else {
                "info"
            }
            .into(),
            targets: Vec::new(),
            file: true,
            stderr: !cfg!(any(
                target_os = "android",
                target_os = "ios",
                target_env = "ohos"
            )),
            dir: String::new(),
            // 4 MiB 一份：一次长会话把 trace 全开也够用，而 `adb pull` 不用等。
            max_bytes: 4 * 1024 * 1024,
            keep: 3,
        }
    }
}

impl Logging {
    /// 去掉各处的手滑空白。`level` 的**取值**不在这里判：认不出是 [`crate::logging`]
    /// 装配过滤器时的事，那一处已经有「回落 info 并在 stderr 说一句」的口径，
    /// 在这里再判一遍就成了两份会各自漂移的规则。
    pub fn normalized(mut self) -> Self {
        self.level = self.level.trim().to_ascii_lowercase();
        self.dir = self.dir.trim().to_string();
        self.targets.retain(|piece| !piece.trim().is_empty());
        self.targets
            .iter_mut()
            .for_each(|piece| *piece = piece.trim().to_string());
        self
    }
}

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
pub fn own_level(logging: &Logging) -> LevelFilter {
    // 空的那一格要自己判掉再交给 `LevelFilter`：它把**空串读成 `error`**（`RUST_LOG=` 的
    // 历史包袱），而配置里空着的 `level` 意思是「没说」，不是「只留 error」。
    match logging.level.trim() {
        "" => FALLBACK_LEVEL,
        text => text.parse::<LevelFilter>().unwrap_or(FALLBACK_LEVEL),
    }
}

/// `target` 是不是 folio 自己的那两支（[`OWN_TARGETS`] 本身，或它们的子模块）。
///
/// 判据与 `Targets::with_target` 一致：前缀要么正好相等，要么后面紧跟 `::` —— 否则
/// `folio_core_foo` 这种别的 crate 会被误认成自己的。Android 的 logcat 那层按它决定
/// 「按配置级别放行」还是「只放 WARN 以上」，见 `ui::platform::android`。
pub fn is_own_target(target: &str) -> bool {
    OWN_TARGETS.iter().any(|prefix| {
        target
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
    })
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

/// 删掉轮转出来的**旧**日志（`folio.log.1`、`.2`……），当前正在写的那一份不动。
/// 返回删掉几份，并把还回去的字节一起交出，好让界面在重扫之前就能说一句「清掉了多少」。
///
/// 为什么当前那一份不给删：[`LogFile`] 攥着它的 fd，删掉名字之后字节仍继续写进一个没人看的
/// inode，症状是日志从此静默 —— 而那正是出事时最需要的东西。轮转出来的那些已经没有 fd
/// 指着，删掉只是清磁盘，所以这一颗按钮只碰它们。
///
/// 认「哪几份是旧的」按 [`LogFile::numbered`] 那个形状（`<本名>.<数字>`）在目录里挑，
/// 不数到 `keep` 为止：配置可以改小，改小之后多出来的那几份仍然该认得出来。
pub fn purge_rotated(active: &Path) -> (usize, u64) {
    let Some(dir) = active.parent() else {
        return (0, 0);
    };
    let Some(name) = active.file_name().and_then(|name| name.to_str()) else {
        return (0, 0);
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let prefix = format!("{name}.");
    let mut removed = 0usize;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(tail) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(&prefix))
        else {
            continue;
        };
        // 尾巴必须是纯数字：`folio.log.old` 那种是读者自己改的名，不在这一档里。
        if tail.is_empty() || !tail.chars().all(|digit| digit.is_ascii_digit()) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        // 同名的目录（`folio.log.9/`）不是「一份旧日志」，不碰。
        if !meta.is_file() {
            continue;
        }
        let size = meta.len();
        match std::fs::remove_file(&path) {
            Ok(()) => {
                removed += 1;
                bytes += size;
            }
            // 删不掉（权限、别的过程正拿着）不算错：这是清理，不是记账。
            Err(err) => tracing::warn!("logging: {} 没删掉：{err}", path.display()),
        }
    }
    (removed, bytes)
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
///
/// 用 `set_global_default` 而不是 `SubscriberInitExt::try_init`：后者在 `tracing-log`
/// feature 下还会顺手装 `LogTracer`（`log` → `tracing` 的桥），而进程里已经有平台自己的
/// `log` 后端了 —— Android 的 `android_logger` 就在这之前 `set_logger`，tracing-log 的
/// 那一步必然失败，`try_init` 于是报错，尽管 subscriber 已经装上了（它是先装后桥）。
/// 后果不只是多一条假警报：`init` 在 `report_startup` 之前返回，启动现场那几行没了。
/// 我们**不**需要 `LogTracer`：Android 那层是反着走的（tracing 事件递进 `log::logger()`），
/// 桌面的 stderr / 文件也不经过 `log` facade。
pub fn init(logging: &Logging, extra: Option<BoxedLayer>) -> Result<()> {
    tracing::subscriber::set_global_default(subscriber(logging, extra))?;
    // `try_init` 原本会在这一步顺手装 `LogTracer`（`log` → `tracing` 的桥）——
    // 桌面靠它把依赖库（wgpu / html5ever 那一串）通过 `log` facade 发的记录也收进同一份
    // 日志。这里显式补回来，且**允许失败**：Android 的 `android_logger` 已经占了 `log`
    // 的全局位，那一边不需要这条桥（tracing 的输出是反着递进 `log::logger()` 的，
    // 见 `ui::platform::android`）。
    let _ = tracing_log::LogTracer::init();

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
mod tests;
