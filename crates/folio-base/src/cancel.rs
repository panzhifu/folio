//! 协作式取消标志：置位、由作业在检查点自己看见。
//!
//! 它既不属于网络，也不属于作业池：远程下载、导入、排版预热共用同一个旗标，
//! 所以住在两者之下。语义是**请求取消**而不是抢占 —— 最多晚一个检查点生效；
//! 取消的最终定性由作业层给出（返回时旗标已置位就落成取消）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 取消标志。界面上的「取消」按钮置位，传输在下一次块边界看见它。
#[derive(Debug, Default, Clone)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn trigger(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn triggered(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}
