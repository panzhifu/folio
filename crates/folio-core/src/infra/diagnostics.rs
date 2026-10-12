//! 诊断 dump：把「现在到底是什么状态」打成一串日志（`plans/基础设施.md` §5.8）。
//!
//! 触发方式是**环境变量**（§六待决 #6 推荐的那一种，成本最低）：`FOLIO_DUMP=1` 启动时
//! 打一次。排障几乎全靠它 —— 读者报「什么都看不见」时，这一串能把「配置没读到」「日志
//! 没开」「数据根不对」三种完全不同的事故分开。
//!
//! 这层只依赖 `tracing` 与 `serde_json`，不碰库、不碰 gpui：库版本与队列长度由调用方
//! 作为参数递进来（core 这层不知道谁持着库）。
//!
//! 还没接上的一块：**各缓存的命中率 / 占用**（§5.3 的内存账）—— 各缓存还报不出字节数。
//! `BlockCache` 报得出 hits / misses，但那两份挂在阅读会话持有的实体上，启动这一刻还没有
//! 任何缓存可报；要给它们一个出口，得是退出时或界面里的入口，不是这一条开机 dump。
//! [`crate::infra::metrics`] 那三个计数器倒是打得了，已经收进 [`dump`]。

use crate::infra::budget::MemoryBudget;
use crate::infra::config::{Config, Provenance};

/// 打开诊断 dump 的环境变量。非空且不为 `0` 即为开。
pub const DUMP_ENV: &str = "FOLIO_DUMP";

/// 环境变量开着没有。
pub fn enabled() -> bool {
    is_on(std::env::var(DUMP_ENV).ok().as_deref())
}

/// [`enabled`] 的纯判据（把环境变量拆出来，测试不动全局环境）。
fn is_on(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        let v = v.trim();
        !v.is_empty() && v != "0"
    })
}

/// 打一份诊断：生效配置 + 每一项的来源 + 库 schema 版本 + 作业队列长度 + 内存预算 + 计数器。
///
/// 各个 `Option` 都是「还没拿到」的合法状态：库没打开、队列还没建、物理内存没接，
/// 就各给 `None`，免得为了打一份诊断去逼调用方先造出那几个东西。
/// **拿不到就报「未知」，绝不拿兜底值冒充真值。**
pub fn dump(
    config: &Config,
    provenance: Option<&Provenance>,
    db_version: Option<i64>,
    queued_jobs: Option<usize>,
    memory_budget: Option<MemoryBudget>,
) {
    tracing::info!(
        db_version = ?db_version,
        queued_jobs = ?queued_jobs,
        "诊断 dump：库版本 / 作业队列"
    );
    // 生效的配置整份打出来：字段即「生效值」。
    match serde_json::to_string_pretty(config) {
        Ok(json) => tracing::info!("生效配置：\n{json}"),
        Err(err) => tracing::warn!("诊断 dump：配置序列化失败：{err}"),
    }
    // 内存预算（§5.3）：物理内存由平台层给，**拿不到就报「未知」**——报一个兜底数会骗人。
    match memory_budget {
        Some(budget) => {
            tracing::info!(
                budget_bytes = budget.total,
                "内存预算（§5.3 策略；执行端尚未接线）"
            )
        }
        None => tracing::info!("内存预算：未知（没接物理内存；§5.3 执行端也未接线）"),
    }
    // 「每一项来自哪一层」（§2.1）：键是点分路径，跟上面那份 JSON 对得上。
    if let Some(provenance) = provenance {
        let table: Vec<String> = provenance
            .entries()
            .map(|(key, layer)| format!("  {key} ← {}", layer.as_str()))
            .collect();
        tracing::info!("配置来源（键 ← 层）：\n{}", table.join("\n"));
    }
    // 计数器（`crate::infra::metrics`）：同一批数在测试里是断言，在真机上是「这一轮到底重解析了
    // 几章、开了几次容器」。只有进程总量（不是线程局部），因为要看的是整轮运行的账。
    let counters: Vec<String> = crate::infra::metrics::snapshot()
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    tracing::info!("计数器：{}", counters.join(" "));
    // `FOLIO_LOG`（§2.1）：它按 RUST_LOG 写法解释、还能连依赖库一起开，落不进
    // `config.logging` 那两格，所以不在 `resolve` 里。这里把原文单独说一句，免得上面的
    // provenance 让人以为 `logging.*` 的「生效值」就是最终生效的。
    if let Ok(spec) = std::env::var(crate::infra::logging::LEVEL_ENV)
        && !spec.trim().is_empty()
    {
        tracing::info!(
            "{}=`{spec}` 压过 logging.level / logging.targets（连依赖库一起）",
            crate::infra::logging::LEVEL_ENV
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{DUMP_ENV, is_on};

    /// 「有没有开」的判据：空串、`0` 都是关；别的都算开。
    #[test]
    fn the_switch_reads_like_a_flag() {
        assert!(!is_on(None), "没设就是关");
        assert!(!is_on(Some("")), "空串是关");
        assert!(!is_on(Some("   ")), "全空白是关");
        assert!(!is_on(Some("0")), "`0` 是关");
        assert!(is_on(Some("1")), "`1` 是开");
        assert!(is_on(Some("yes")), "别的字符串都算开");
        assert_eq!(DUMP_ENV, "FOLIO_DUMP");
    }
}
