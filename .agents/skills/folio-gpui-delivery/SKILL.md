---
name: folio-gpui-delivery
description: 修改 Folio 原生 GPUI 页面、弹层、交互或平台能力，落实状态、事件、焦点归属与真实组件验证。
---

遵循 [AGENTS.md](../../../AGENTS.md) 与 [工程规范](../../../doc/工程规范.md)。先读已安装的 gpui-kit 与 gpui-kit-design-guides 及其 Coding/Design Guides，再读相应 feature、host 和组件的当前实现/API。

先说清用户动作、状态 owner、异步结果身份与焦点返回路径。页面发语义事件给 shell；services 不调用 feature 内部实现；生产平台差异由 host/platform 提供能力。恢复等跨文件操作放服务编排，界面展示实际准备、错误和待重启状态，不能把后台任务开始当成功。

复用 GPUI Kit 的组件、主题 token、稳定 ID 与 overlay 行为；用真实组件输入验证返回键、遮罩、禁用、焦点和过期结果，必要时读取 `gpui-kit/references/gpui/test.md`。使用 `cargo xtask boundaries` 核对新边界，按实际用例选择定向测试。桌面构建、无头交互、GPU窗口和Android行为分别报告；不主动查看手机，不搬入 Web 动效实现。
