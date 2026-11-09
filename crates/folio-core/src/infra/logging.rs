//! 日志装配住在 `folio-base`；这里只留需要配置与界面语言的那两层壳。
//!
//! 为什么壳留在 core：`init_from_config` 要把「读配置 → 装日志 → 配置留着用」并成一步，
//! 而配置的 schema 在 core；反馈快照要写界面语言（`i18n`），文案表也在 core。
//! 地基不认识这两样，也不该认识。

pub use folio_base::logging::*;

use std::path::Path;

use crate::infra::config::{Config, Resolved};
use crate::infra::i18n;
use crate::infra::paths;
use crate::infra::time::civil_date;

/// 读默认配置并装日志，返回那份配置 —— 每个入口「读配置 → 装日志 → 配置留着后面用」
/// 这三步的收口（`plans/基础设施.md` §5.6 的「配置 → 日志」，以及 §2.3 的「启动读一次」）。
///
/// 合成一个函数的理由：三处入口各写一遍时容易漏掉其中一步，或者把配置读两次
/// （Android 上就曾读两次）。**时机仍由入口决定** —— Android 要先 `set_app_dirs`
/// 拿到应用专属目录，才读得到对的那份配置；这个函数不碰数据根，只是把两步并作一步。
///
/// 返回 `(Resolved, Result<()>)`：`Config::resolve` 永不失败（读不到就退回默认值），只有装
/// subscriber 可能失败（全局槽被占了）。这是**唯一**需要分开报的两种结果，所以分开回传，
/// 而不是压成一个 `Result<Config>` —— 后者会让人以为「日志装不上」等于「拿不到配置」。
/// 失败只影响诊断输出、不影响程序起来；怎么把这句话说出去是调用方的事
/// （这一刻 core 自己还没有日志可用，只能靠 `eprintln` / 平台控制台）。
///
/// 返回 [`Resolved`] 而不是光一份 `Config`：入口还要拿用户层去落盘、拿 provenance 去
/// 诊断（§2.1），这些都得把读盘那一刻的两层一起交出去。
pub fn init_from_config(extra: Option<BoxedLayer>) -> (Resolved, crate::Result<()>) {
    let resolved = Config::load_resolved();
    let installed = init(&resolved.effective.logging, extra).map_err(crate::Error::from);
    if installed.is_ok() {
        // §4.2：启动现场要能一眼看到「这份配置是哪个格式版本」。
        tracing::info!("配置版本 v{}", resolved.effective.version);
    }
    (resolved, installed)
}

/// 反馈用的日志快照：头部几行事实 + 日志的**尾部**。
///
/// 为什么要头部那几行：一条「打不开书」的反馈，缺这几样就得来回问 —— 版本号、平台、界面语言、
/// 两个目录（桌面上配置根与数据根是**两个地方**，读者报一个另一个就够误导）、日志本身在哪。
/// 其中两条目录正是「关于」那一屏撤掉「配置文件在哪儿」之后，读者还能拿到它们的唯一去处
/// （见 `folio-app` 的 `settings/view/pages/about.rs` 顶部）。
///
/// 为什么只给尾部：日志会一直长（上限是配置里的 `max_bytes`，写满停手，旧的轮转成 `.1`/`.2`），
/// 而出事总在最后。整本递给作者等于把读者**从头到尾看过的书名**一起递过去 ——
/// `import:` 那些行是带书名与落盘路径的。截尾既是最有用的那一段，也顺带少带一截历史。
///
/// 时间戳用 **UTC**：日志行本身不带时间戳（`Targets` 那层没开 timer），这一串只是给文件名
/// 一个能排序、能防撞的形状，不是给读者对时刻用的。
pub fn feedback_snapshot(now: i64, log_path: &Path, tail: u64) -> std::io::Result<Vec<u8>> {
    let bytes = std::fs::read(log_path)?;
    let kept = if tail > 0 && (bytes.len() as u64) > tail {
        &bytes[bytes.len() - tail as usize..]
    } else {
        &bytes[..]
    };
    let stamp = format_stamp(now);
    let mut out = Vec::with_capacity(kept.len() + 512);
    let header = format!(
        "folio 反馈日志\nversion: {}\nplatform: {} {}\nlocale: {}\nconfig_dir: {}\ndata_dir: {}\nlog_file: {} ({} bytes, last {} kept)\ntaken: {} UTC\n----\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        i18n::current().code(),
        paths::config_dir().display(),
        paths::data_dir().display(),
        log_path.display(),
        bytes.len(),
        kept.len(),
        stamp,
    );
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(kept);
    Ok(out)
}

/// 快照的文件名：`folio-log-<版本>-<平台>-<YYYYMMDD>-<HHMM>.txt`。
///
/// 名字里带版本与平台是省一轮问答：邮件里读者常忘了说自己在装什么，而这两个恰好是
/// 看日志之前就要知道的事。
pub fn snapshot_name(now: i64) -> String {
    let stamp = format_stamp(now);
    format!(
        "folio-log-{}-{}-{}.txt",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        stamp.replace(['-', ':'], "")
    )
}

/// `now`（unix 秒）→ `2026-10-24 13:30`，UTC。
fn format_stamp(now: i64) -> String {
    let day = now.div_euclid(86_400);
    let secs = now.rem_euclid(86_400);
    format!(
        "{} {:02}:{:02}",
        civil_date(day),
        secs / 3_600,
        (secs % 3_600) / 60,
    )
}

#[cfg(test)]
mod tests {
    use super::{feedback_snapshot, snapshot_name};

    /// 反馈快照：头部那几样事实必须在，尾部必须是**结尾**那一截。
    #[test]
    fn a_snapshot_carries_the_facts_and_keeps_the_end_of_the_log() {
        let dir = std::env::temp_dir().join(format!("folio-snap-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("folio.log");
        // 60 行、每行 7 字节（`line00` + 换行），共 420 字节；只留 100 字节时该看到的是最后十四行。
        let body: String = (0..60).map(|n| format!("line{n:02}\n")).collect();
        std::fs::write(&file, body.as_bytes()).unwrap();

        let snap = feedback_snapshot(1_760_000_000, &file, 100).unwrap();
        let text = String::from_utf8(snap).unwrap();
        for fact in [
            "version:",
            "platform:",
            "locale:",
            "config_dir:",
            "data_dir:",
            "log_file:",
            "taken:",
        ] {
            assert!(text.contains(fact), "快照头部少了 {fact} 那一行：\n{text}");
        }
        assert!(text.contains("line59"), "尾部没留住最后一行");
        assert!(!text.contains("line00"), "截尾留的是结尾，开头那行不该还在");
        // 说清了「一共多少、留了多少」，读者才知道自己递出去的是不是一份。
        assert!(text.contains("420 bytes, last 100 kept"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_short_log_is_kept_whole() {
        let dir = std::env::temp_dir().join(format!("folio-snap-short-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("folio.log");
        std::fs::write(&file, b"short log\n").unwrap();
        let text = String::from_utf8(feedback_snapshot(0, &file, 1024).unwrap()).unwrap();
        assert!(
            text.ends_with("short log\n"),
            "比上限短的日志不该被切掉尾巴：\n{text}"
        );
        assert!(text.contains("last 10 kept"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_log_file_is_an_error_not_an_empty_snapshot() {
        // 「日志没开」与「日志是空的」是两件事：递一份只有头部的空快照给作者，
        // 两边都以为对方拿到了东西。
        let dir = std::env::temp_dir().join(format!("folio-snap-none-{}", std::process::id()));
        let err = feedback_snapshot(0, &dir.join("nope.log"), 1024).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn the_snapshot_name_is_sortable_and_names_the_platform() {
        let early = snapshot_name(1_760_000_000);
        let late = snapshot_name(1_760_003_600);
        assert!(early.starts_with("folio-log-"), "{early}");
        assert!(early.ends_with(".txt"), "{early}");
        assert!(early.contains(std::env::consts::OS), "{early}");
        // 字典序即时间序：作者收到一堆同名前缀的文件时，排一下就是按时间排。
        assert!(early < late, "{early} 该排在 {late} 前面");
        assert_ne!(early, late);
    }
}
