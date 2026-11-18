---
name: folio-release-validation
description: 在 Folio 提交推送、依赖升级或平台打包时，核对版本来源、治理、目标构建、签名与产物证据。
---

遵循 [AGENTS.md](../../../AGENTS.md) 与 [工程规范](../../../doc/工程规范.md)，读取 Cargo/lock、CI 与 [打包说明](../../../packaging/README.md)。先识别 jj 和 `.jj/`；环境不符先完成独立验证并报告，不自动初始化，不用 git commit/checkout/stash。

`cargo xtask governance --base <真实基准>` 检查事实与零增长，`cargo xtask boundaries` 查 UI/crate；`cargo xtask release-check` 核对发布源，`--artifacts <目录>` 对账实际 SHA-256 清单。后者不证明签名、包内容、升级或目标平台运行，缺少证据要单列。GPUI 五条 patch 与 folio-curl rev 一起核对，Android 的 NDK API 保持 26。

按授权执行构建；用户要求推送时跑仓库全量测试，修复实际失败后用中文说明原因和实测事实。确认需要的改动均在待推送 change，先 dry-run 再真实推送；发布 tag、安装手机和启动操作遵循用户本次范围。未授权发布时不因推送顺带打 tag 或建 Release。
