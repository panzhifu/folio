---
name: folio-architecture-review
description: 评审或修改 Folio 的 crate 依赖、公共 API、跨层引用与状态所有权；不因文件较长就发起全仓重构。
---

遵循 [AGENTS.md](../../../AGENTS.md)、[工程规范](../../../doc/工程规范.md) 和 [软件架构](../../../doc/软件架构.md)。只读评审请求不改实现。

先核对 Cargo 清单、实际 re-export、调用点和相关契约测试。引擎不得引入 GPUI；model 仅容纳多个 crate 共用的数据。模块迁移默认保持公共路径与 serde 形状，配置到参数的转换留在入口层。

UI 目标方向为 shell → features → components → services → foundation。跨功能用事件、共享数据或服务，不由基础服务调用页面内部实现。平台差异通过 host/platform 能力传递。需要改变界面时先读 gpui-kit 与 gpui-kit-design-guides；它们的规范与原生 API 优先于 Web 技能示例。

`cargo xtask boundaries` 检查显式路径、部分导入别名和生产 cfg；不展开普通宏，不证明完整类型归属。现有例外按精确基线只减不增，不能通过上调 baseline 让越界通过。提出新边界时同时说明 owner、依赖变化、兼容影响和会失败的负向用例。

文件长时分别看注释量、职责和状态耦合：先去重复历史说明，再决定是否按真实能力拆分。保留解释坐标、锁、资源释放与缓存失效的必要注释；避免把注释减少当成架构完成。

结论区分已确认问题、推导风险和待实测项；给出最小修复与定向验证。实施时更新对应现状文档，只读评审则提出更新建议，不增加无关 crate 或通用框架。
