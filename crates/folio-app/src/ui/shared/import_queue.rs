//! 自动导入（监听、传书）回界面的**一条事件桥**。
//!
//! 导入作业池不在这里 —— 它是进程级常驻资源，归 [`super::services`] 管（`plans/基础设施.md`
//! §1.1「一条队列，不要三条」）。本模块只剩桥：自动导入的批次不是书架提交的，书架那侧的
//! 链接不到它们，所以服务把作业的事件逐条转发进这条通道，
//! [`crate::ui::features::shelf::ShelfView`] 开屏时订阅，读数照常出现。

use folio_core::{ImportOutcome, TaskEvent};

/// 一条自动导入事件通道的两端（`Sender` / `Receiver`），全进程共用一份。
type ImportChannel = (
    async_channel::Sender<TaskEvent<Vec<ImportOutcome>>>,
    async_channel::Receiver<TaskEvent<Vec<ImportOutcome>>>,
);

/// 全进程唯一的一条自动导入事件通道。
fn channel() -> &'static ImportChannel {
    static CHANNEL: std::sync::OnceLock<ImportChannel> = std::sync::OnceLock::new();
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
/// 的提交方用：事件的收干交给 [`folio_core::import::forward`]（它管线程创建），
/// 这里只管逐条往桥上送。
pub fn forward_to_auto_imports(handle: folio_core::task::TaskHandle<Vec<ImportOutcome>>) {
    let sender = channel().0.clone();
    folio_core::import::forward(handle, move |event| {
        // 接收端没了：没有要的人，收手（后面的事件也跟着没有意义）。
        let _ = sender.send_blocking(event);
    });
}
