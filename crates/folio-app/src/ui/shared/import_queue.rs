//! 导入的进程级基础设施：**一条导入队列**与**一条自动导入事件通道**。
//!
//! 队列整个进程一份、一条工作线程：副本按原文件名落进书库、同名即覆盖，
//! 两批并发写同一个目标文件会拷出半本（手工导入、监听、传书都排这一条队，
//! 谁也别并发）。挂在 `static` 里它就活到进程结束 —— `TaskManager` 被丢弃会
//! `join` 工作线程，挂谁身上谁都得等。
//!
//! 事件通道是自动导入（监听、传书）回界面的桥：这些批次不是书架提交的，
//! 书架那侧的链接不到它们 —— 服务把作业的事件逐条转发进这条通道，
//! [`crate::ui::features::shelf::ShelfView`] 开屏时订阅，读数照常出现。

use std::sync::{Arc, OnceLock};

use folio_core::{ImportOutcome, TaskEvent, TaskManager};

/// 导入作业的工作线程池：整个进程一份、一条线程。
pub fn import_queue() -> &'static Arc<TaskManager<Vec<ImportOutcome>>> {
    static QUEUE: OnceLock<Arc<TaskManager<Vec<ImportOutcome>>>> = OnceLock::new();
    QUEUE.get_or_init(|| Arc::new(TaskManager::new(1)))
}

/// 一条自动导入事件通道的两端（`Sender` / `Receiver`），全进程共用一份。
type ImportChannel = (
    async_channel::Sender<TaskEvent<Vec<ImportOutcome>>>,
    async_channel::Receiver<TaskEvent<Vec<ImportOutcome>>>,
);

/// 全进程唯一的一条自动导入事件通道。
fn channel() -> &'static ImportChannel {
    static CHANNEL: OnceLock<ImportChannel> = OnceLock::new();
    CHANNEL.get_or_init(async_channel::unbounded)
}

/// 自动导入事件的接收端。书架开屏时订阅一次：之后每批自动导入的事件都会流到这里，
/// 进 [`ShelfView`](crate::ui::features::shelf::ShelfView) 的同一套事件处理。
pub fn auto_import_events() -> async_channel::Receiver<TaskEvent<Vec<ImportOutcome>>> {
    channel().1.clone()
}

/// 自动导入事件的发送端。给自带「事件后处理」的提交方用（传书导入成功要清
/// 收件箱原件）—— 先检查事件再转发。
pub(crate) fn auto_import_sender() -> async_channel::Sender<TaskEvent<Vec<ImportOutcome>>> {
    channel().0.clone()
}

/// 把一条后台作业的事件转发进自动导入通道。给监听 / 传书这类「没有界面可接」
/// 的提交方用：一个作业一条转发线程，作业结束（通道关闭）线程随之退出。
pub fn forward_to_auto_imports(
    handle: folio_core::task::TaskHandle<Vec<ImportOutcome>>,
) {
    let sender = channel().0.clone();
    std::thread::Builder::new()
        .name("folio-auto-import".to_string())
        .spawn(move || {
            for event in handle.events.iter() {
                if sender.send_blocking(event).is_err() {
                    break; // 接收端没了：没有要的人，收手。
                }
            }
        })
        .expect("起自动导入转发线程");
}
