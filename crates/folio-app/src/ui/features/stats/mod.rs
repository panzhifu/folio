//! 「统计」目的地：读者这一段时间读得怎么样。
//!
//! - [`model`] —— 数据快照：把库里的阅读会话折成一份可直接画的数；
//! - [`view`] —— 那一屏本身。
//!
//! 离库最近的一次读发生在**进来的时候**（`AppShell::go` 里那一格），不在 `render` 里：
//! 把画一屏变成做一轮 IO 是这条边界存在的理由。

mod model;
mod view;

pub use view::{StatsEvent, StatsView};
