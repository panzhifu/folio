---
name: folio-change-workflow
description: 在 Folio 实施、修复或精简注释时，确定变更范围、事实来源和最小验证；仅要求计划或分析时保持只读。
---

遵循仓库 [AGENTS.md](../../../AGENTS.md) 与 [工程规范](../../../doc/工程规范.md)。用户最新的范围与授权优先，不因计划存在就实施。

先读实际 checkout 状态和相关源码：有 `.jj/` 时检查 jj change，没有时用只读 Git，并明确区别；不自动初始化 jj，不用 git commit/checkout/stash。保留其它改动，临时产物放已忽略的 target 或系统临时目录。

涉及行为、API 或数据时读取对应领域文档与原有测试；纯文案/注释按实际语义影响选择。`cargo xtask change-plan` 提供验证类别，不替你选虚构用例。规则/文档/依赖/skills 改动运行 `cargo xtask governance`；有历史基准时加 `--base <ref>`。架构改动运行 `cargo xtask boundaries`。本地只跑实际新增/受影响的用例；推送全量沿用 AGENTS，不把 CI 范围照搬到日常。

精简注释时先分清契约与历史：删代码复述、重复解释、开发流水；保留 SAFETY、单位、特殊值、兼容、平台与生命周期约束。纯注释变更用 `cargo xtask comment-check <base> <path>...` 辅助核对，另审 doc test 和工具指令。不机械去除 `//`，不为凑比例删测试，不把全部旧注释倒进另一份文档。

交付说明行为变化、注释减量、实际执行及未验证范围。当前环境缺依赖时报告具体阻断；隔离兼容版本验证不能冒充根 lock/CI 工具链通过。常规可逆步骤继续完成，不反复申请已给授权。
