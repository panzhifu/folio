//! 删除的撤销：把删掉的那一整行暂存一份，界面上给一小段「撤销」的窗口。
//!
//! Okular 的命令层（`documentcommands_p.h`）给过的教训是撤销层要建在删除之前 ——
//! 这里照办：存储层保持只有真删、没有软删，列表、回显、统计都不必到处过滤「已删除」
//! 的行。撤销是**会话级**的：删的那一刻把整行抄进内存，窗口内按原行插回
//! （[`folio_core::Store::restore_bookmark`]，id 与时间戳都不变），过了这一窗就真的没了。
//! 跨进程的回收站是另一个量级的功能，不在这里。
//!
//! **今天只有书签走这条路**：取消划线是一次真删，不弹胶囊（2026-11-02 读者的口径 ——
//! 点「取消划线」的时候手里就带着那条选区，反悔的路是重新划一次，而不是屏底再飘一条
//! 横条；顶栏那颗星是盲按的，拔错了看不见，所以书签留着这一窗）。
//!
//! 一次只留一槽：胶囊上那句说的是最近的一次删除，再删一次就顶掉上一次 —— 与
//! 「读者心里那句『哎手滑了』永远只对最近那一下」是同一件事。

use std::time::Duration;

use folio_core::store::Bookmark;
use gpui_kit::{SharedString, Task};

/// 撤销胶囊在屏上停留多久。给短了读者没看清就来不及了，给长了它就一直占着底栏
/// 上方那一块 —— 6 秒是「读完那句、手挪过去」的富余。
pub(in crate::ui::features::reader) const UNDO_TTL: Duration = Duration::from_secs(6);

/// 一颗还在屏上的撤销胶囊。
pub(in crate::ui::features::reader) struct UndoSlot {
    pub(in crate::ui::features::reader) label: SharedString,
    pub(in crate::ui::features::reader) row: Bookmark,
    /// 这一槽的号。到点的计时器只收**自己那一号**：第二次删除换了槽，先前的计时器
    /// 不该把新胶囊提前收走。
    pub(in crate::ui::features::reader) seq: u64,
    pub(in crate::ui::features::reader) _timer: Option<Task<()>>,
}
