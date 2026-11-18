//! 配置与语言相关的日志入口；subscriber 装配在 folio-base。

pub use folio_base::logging::*;

use std::path::Path;

use crate::infra::config::{Config, Resolved};
use crate::infra::i18n;
use crate::infra::paths;
use crate::infra::time::civil_date;

/// 读取一次配置并装日志；宿主须先设置数据根。
/// 装配失败仍返回有效配置，由入口报告日志不可用。
pub fn init_from_config(extra: Option<BoxedLayer>) -> (Resolved, crate::Result<()>) {
    let resolved = Config::load_resolved();
    let installed = init(&resolved.effective.logging, extra).map_err(crate::Error::from);
    if installed.is_ok() {
        tracing::info!("配置版本 v{}", resolved.effective.version);
    }
    (resolved, installed)
}

/// 反馈快照包含版本、平台、目录与日志尾部；时间戳为 UTC。
/// 生产者负责脱敏，这里不尝试从任意文本猜测 secret。
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

/// 带版本、平台与 UTC 时间的反馈文件名。
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
