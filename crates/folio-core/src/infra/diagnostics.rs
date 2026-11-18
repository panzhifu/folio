//! 启动诊断只输出公开字段白名单；配置原文与用户提供的键名不得进入日志。

use crate::infra::budget::MemoryBudget;
use crate::infra::config::{Config, Provenance};

/// 非空且不为 `0` 时启用启动诊断。
pub const DUMP_ENV: &str = "FOLIO_DUMP";

pub fn enabled() -> bool {
    is_on(std::env::var(DUMP_ENV).ok().as_deref())
}

fn is_on(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty() && v.trim() != "0")
}

/// 输出公开配置、库版本、作业量与计数器。缺失指标保持未知，不用估值代替。
pub fn dump(
    config: &Config,
    provenance: Option<&Provenance>,
    db_version: Option<i64>,
    queued_jobs: Option<usize>,
    memory_budget: Option<MemoryBudget>,
) {
    tracing::info!(?db_version, ?queued_jobs, "诊断 dump：库版本 / 作业队列");
    let public = public_config(config);
    tracing::info!("公开配置：{}", serde_json::Value::Object(public.clone()));
    if let Some(provenance) = provenance {
        for key in public.keys() {
            tracing::info!(key, layer = provenance.layer_of(key).as_str(), "配置来源");
        }
    }
    match memory_budget {
        Some(budget) => tracing::info!(budget_bytes = budget.total, "内存预算（执行端尚未接线）"),
        None => tracing::info!("内存预算：未知（执行端尚未接线）"),
    }
    for (name, value) in crate::infra::metrics::snapshot() {
        tracing::info!(name, value, "计数器");
    }
    if std::env::var(crate::infra::logging::LEVEL_ENV)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
    {
        tracing::info!("FOLIO_LOG 已覆盖日志配置（不输出原文）");
    }
}

fn public_config(config: &Config) -> serde_json::Map<String, serde_json::Value> {
    // 逐字段列出，不能序列化整个节；新增凭据字段不会自动进入诊断。
    let values = [
        ("version", serde_json::json!(config.version)),
        (
            "language.locale",
            serde_json::json!(config.language.locale.code()),
        ),
        (
            "reader.font_size",
            serde_json::json!(config.reader.font_size),
        ),
        (
            "reader.line_height",
            serde_json::json!(config.reader.line_height),
        ),
        ("reader.margin_x", serde_json::json!(config.reader.margin_x)),
        ("reader.margin_y", serde_json::json!(config.reader.margin_y)),
        (
            "reader.image_cache_bytes",
            serde_json::json!(config.reader.image_cache_bytes),
        ),
        ("appearance.mode", serde_json::json!(config.appearance.mode)),
        ("ai.enabled", serde_json::json!(config.ai.enabled)),
        ("sync.enabled", serde_json::json!(config.sync.enabled)),
        ("sync.provider", serde_json::json!(config.sync.provider)),
        ("account.enabled", serde_json::json!(config.account.enabled)),
        ("logging.file", serde_json::json!(config.logging.file)),
        ("logging.stderr", serde_json::json!(config.logging.stderr)),
        (
            "logging.max_bytes",
            serde_json::json!(config.logging.max_bytes),
        ),
        ("logging.keep", serde_json::json!(config.logging.keep)),
    ];
    values
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use crate::infra::config::{Config, Layer, Provenance, Vendor};
    use crate::infra::diagnostics::{DUMP_ENV, dump, is_on};
    use crate::infra::logging::feedback_snapshot;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_switch_reads_like_a_flag() {
        for value in [None, Some(""), Some("   "), Some("0")] {
            assert!(!is_on(value));
        }
        assert!(is_on(Some("1")));
        assert!(is_on(Some("yes")));
        assert_eq!(DUMP_ENV, "FOLIO_DUMP");
    }

    #[test]
    fn diagnostic_logs_and_feedback_exclude_credentials_and_untrusted_keys() {
        let secret = "SENTINEL_KEY_PASSWORD_TOKEN";
        let mut config = Config::default();
        config.reader.font_size = 23.0;
        config.reader.font_family = secret.into();
        config.sync.password = secret.into();
        config.account.key = secret.into();
        config.translation.openai.api_key = secret.into();
        config.translation.deepl.api_key = secret.into();
        config.cloud.baidu.access_token = secret.into();
        config.cloud.aliyun.refresh_token = secret.into();
        config.logging.level = secret.into();
        config.logging.targets.push(secret.into());
        config.vendors.push(Vendor {
            id: secret.into(),
            name: secret.into(),
            base_url: format!("https://{secret}@example.test/?key={secret}"),
            api_key: secret.into(),
        });
        let mut provenance = Provenance::default();
        provenance.mark("reader.font_size", Layer::Book);
        provenance.mark(format!("reading_backgrounds.{secret}.path"), Layer::File);
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            dump(&config, Some(&provenance), Some(27), Some(3), None);
            tracing::info!(?config, "配置调试");
        });
        let bytes = captured.0.lock().unwrap().clone();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(!text.contains(secret), "诊断包含敏感值");
        assert!(text.contains("reader.font_size"));
        assert!(text.contains("23.0"));
        assert!(text.contains("book"));
        assert!(text.contains("db_version=Some(27)"));

        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("folio.log");
        std::fs::write(&log, bytes).unwrap();
        let feedback = String::from_utf8(feedback_snapshot(0, &log, 64 * 1024).unwrap()).unwrap();
        assert!(!feedback.contains(secret));
        assert!(feedback.contains("reader.font_size"));

        let path = dir.path().join("config.json");
        config.save_to(&path).unwrap();
        let saved: Config = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved, config, "脱敏不能改变配置与备份的序列化");
    }
}
