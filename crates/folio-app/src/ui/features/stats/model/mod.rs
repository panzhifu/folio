//! 「统计」那一屏的数据：把库里全部的阅读会话折成界面直接能画的形状。
//!
//! 纯计算，不碰元素树 —— 那一屏只在进来时、切区间、翻页时取一次快照（见
//! [`Snapshot::load`]），`render` 一律不碰库。
//!
//! 为什么把**全部**会话一次读进来、而不是按窗口查几条 SQL：这一屏的区间是「总 / 日 /
//! 周 / 月 / 年」，切一次只是换一个切片；而「总」那几块还要看历史全量（相识天数、
//! 最近 30 天、单次时长分布）。一次读回同一批行再在内存里切，既省往返，也保证这一屏
//! 上的每个数来自同一份事实 —— 与 `Store::reading_summary` 的文件头同一条理由。

pub(in crate::ui::features::stats) mod snapshot;

pub(in crate::ui::features::stats) use snapshot::{Bucket, DayStat, Range, Snapshot};
