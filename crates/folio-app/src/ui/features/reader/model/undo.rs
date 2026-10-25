//! 删除的撤销：把删掉的那一整行暂存一份，界面上给一小段「撤销」的窗口。
//!
//! Okular 的命令层（`documentcommands_p.h`）给过的教训是撤销层要建在删除之前 ——
//! 这里照办：存储层保持只有真删、没有软删，列表、回显、统计都不必到处过滤「已删除」
//! 的行。撤销是**会话级**的：删的那一刻把整行抄进内存，窗口内按原行插回
//! （[`folio_core::Store::restore_annotation`] / `restore_bookmark`，id 与时间戳都不变），
//! 过了这一窗就真的没了。跨进程的回收站是另一个量级的功能，不在这里。
//!
//! 一次只留一槽：胶囊上那句说的是最近的一次删除，再删一次就顶掉上一次 —— 与
//! 「读者心里那句『哎手滑了』永远只对最近那一下」是同一件事。

use std::time::Duration;

use folio_core::store::{Annotation, Bookmark};
use gpui_kit::{SharedString, Task};

/// 撤销胶囊在屏上停留多久。给短了读者没看清就来不及了，给长了它就一直占着底栏
/// 上方那一块 —— 6 秒是「读完那句、手挪过去」的富余。
pub(in crate::ui::features::reader) const UNDO_TTL: Duration = Duration::from_secs(6);

/// 删掉的那一行。枚举而不是两个槽：批注与书签共用同一颗胶囊，后删的顶掉先删的。
pub(in crate::ui::features::reader) enum DeletedRow {
    Annotation(Annotation),
    Bookmark(Bookmark),
}

/// 一颗还在屏上的撤销胶囊。
pub(in crate::ui::features::reader) struct UndoSlot {
    pub(in crate::ui::features::reader) label: SharedString,
    pub(in crate::ui::features::reader) row: DeletedRow,
    /// 这一槽的号。到点的计时器只收**自己那一号**：第二次删除换了槽，先前的计时器
    /// 不该把新胶囊提前收走。
    pub(in crate::ui::features::reader) seq: u64,
    pub(in crate::ui::features::reader) _timer: Option<Task<()>>,
}
