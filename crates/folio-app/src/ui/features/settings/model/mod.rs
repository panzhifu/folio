//! 设置项的中立规格。
//!
//! 两端共用的**唯一数据源**：桌面把它映射回 `Settings` 组件的数据形状，手机直接渲染。
//! 加一格只改这里与对应的页面文件。

pub(in crate::ui::features::settings) mod spec;
