//! 阅读时长：把「这本书读了多久」记成一段一段的会话，喂给「统计」那一屏。
//!
//! 存储侧早就齐了（`folio_core::store::reading`：表、`record_session`、按本地日汇总、
//! 九个派生数），缺的一直是**写入**这一环 —— 采集没接上时统计页必然是一屏 0，
//! 而那一屏看起来像「读者还没读过书」，不像「功能没接」。这个文件补的就是那一环。
//!
//! 三条口径决定这里为什么长这样：
//!
//! 1. **一段一段写，不是一开一关写一行。** 每 [`SEGMENT_SECS`] 秒把已累计的那一段
//!    结账成一条 `reading_sessions`。表本来就是「一行 = 一段」的形状，所以不改 schema。
//!    理由不是好看：Android 上没有可靠的「进程最后一次写库」时机（`cx.on_app_quit` 只留
//!    100ms，见 `ui::shared::services` 的模块文档），杀掉进程、系统回收、崩溃这三种
//!    收尾都不在应用手里。定时结账是唯一靠得住的那条路，代价是进程被杀最多丢一段。
//! 2. **空闲不算阅读。** `ended - started` 会把「书开着、人去洗碗了」记成阅读时长，
//!    而那是这一屏最容易长出来的一句谎。所以结账时刻取
//!    `min(now, 最后一次交互 + IDLE_GRACE)`：那段没人在的空隙不进账，
//!    下一段从现在起算，中间的空白永久丢掉。
//! 3. **宽限给足，宁可多算一小截。** [`IDLE_GRACE`] 取五分钟而不是六十秒：读者盯着
//!    一页慢读三四分钟**没有任何输入**是常态，按六十秒裁就会每页少算一截，
//!    那是系统性地把读数压低。按五分钟裁，代价变成「每次离席最多多算五分钟」——
//!    一个有界的、一次性的偏差，比一条持续偏低的曲线诚实。

use std::time::Duration;

use gpui_kit::Context;

use super::super::session::ReaderSession;

/// 一段的结账间隔（秒）。也是「进程被杀最多丢多少」的上限。
const SEGMENT_SECS: i64 = 300;

/// 那条时钟多久走一次。比 [`SEGMENT_SECS`] 小就够 —— 它只负责「到点了没有」，
/// 再密也不会让账目更准，只会多空转。
const TICK: Duration = Duration::from_secs(60);

/// 最后一次交互之后还宽限多久算在读。见文件头第 3 条。
const IDLE_GRACE: i64 = 300;

impl ReaderSession {
    /// 开始为当前这本书计时。换书时**先** [`Self::settle_reading`] 再调这里，
    /// 否则上一本那一段会被算到新头上。
    pub(in crate::ui::features::reader) fn begin_reading(&mut self, cx: &mut Context<Self>) {
        let at = folio_core::infra::time::now();
        self.reading_segment_start = at;
        self.reading_last_activity = at;
        self.start_reading_clock(cx);
    }

    /// 读到过一下正文、或位置动过一下 —— 这就是「人还在」的证据。
    ///
    /// 挂在两条已有之路上（落库位置、点正文），而不是逐个输入口各插一刀：
    /// 那些入口最终都要落这两条，而漏插一个输入口的后果由 [`IDLE_GRACE`] 那五分钟吸收。
    pub(super) fn note_reading_activity(&mut self) {
        if self.reading_segment_start != 0 {
            self.reading_last_activity = folio_core::infra::time::now();
        }
    }

    /// 离开这本书：结掉最后一段，并把时钟停掉（`Task` drop 即取消计时）。
    pub(in crate::ui::features::reader) fn end_reading(&mut self) {
        self.settle_reading();
        self.reading_segment_start = 0;
        self.reading_clock = None;
    }

    /// 把「从 [`Self::reading_segment_start`] 到此刻、且不超过宽限线」那一段写进库，
    /// 然后把段首推到此刻。
    pub(in crate::ui::features::reader) fn settle_reading(&mut self) {
        let start = self.reading_segment_start;
        if start == 0 {
            return;
        }
        let now = folio_core::infra::time::now();
        // 段首推到 `now` 而不是结账点：宽限线到 `now` 之间那段是「没人在」的证据，
        // 留着它下一段就会把这段空白再算一遍。
        self.reading_segment_start = now;
        let Some(ended) = settled_at(start, self.reading_last_activity, now) else {
            // 整段都在宽限线之外（人早走了），或者时钟走到了一个空段上：不写零行。
            return;
        };
        let (Some(store), Some(book_id)) = (self.store.clone(), self.book_id) else {
            return;
        };
        // `chars` 给 0：它的口径还没定（`plans/Locator.md` 的开放问题：回翻怎么重复计、
        // 跨章怎么去重），而库里一个来源不明的数比一列 0 难查得多。界面那屏也相应地
        // 不显示字数。
        if let Err(err) = store.record_session(book_id, start, ended, 0) {
            tracing::warn!(
                book_id,
                error = %err,
                "record_session: 阅读时长没记上"
            );
        }
    }

    /// 那条 60 秒的时钟：到点就结一段。已经挂着就不再挂第二条（换书会走这里两次）。
    fn start_reading_clock(&mut self, cx: &mut Context<Self>) {
        if self.reading_clock.is_some() {
            return;
        }
        let weak = cx.entity().downgrade();
        self.reading_clock = Some(cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                // 会话没了（阅读页被弹掉、实体被 drop）就退出这条循环。
                let Ok(()) = weak.update(cx, |session, _| {
                    if session.reading_segment_start == 0 {
                        return;
                    }
                    if folio_core::infra::time::now() - session.reading_segment_start < SEGMENT_SECS {
                        return;
                    }
                    session.settle_reading();
                }) else {
                    return;
                };
            }
        }));
    }
}

/// 一段该结在哪一刻。`None` = 这一段没什么可写。
///
/// 抽成纯函数是因为它唯一值得钉的东西就是**这三个时刻之间的关系**，而把它接在
/// `settle_reading` 上测就得改系统时钟（做不到）或等五分钟（不该让 CI 等）。
/// 断言因此能用一组假数字写完，见下面的 `tests`。
fn settled_at(start: i64, last_activity: i64, now: i64) -> Option<i64> {
    let ended = (last_activity + IDLE_GRACE).min(now);
    (ended > start).then_some(ended)
}

#[cfg(test)]
mod tests {
    //! 结账那一步的算术。测试内联在被测代码所在文件底部，是本项目三条测试规矩之一；
    //! 点名导入也不用 glob，是因为 `gpui_kit` 根带出一个叫 `test` 的属性宏。

    use super::{IDLE_GRACE, SEGMENT_SECS, TICK, settled_at};

    #[test]
    fn an_active_reader_gets_the_whole_segment() {
        // 段首 0，最后一次活动就在结账这一刻。
        assert_eq!(settled_at(0, 300, 300), Some(300));
    }

    /// 离席的那一段不进账，而最多多算一个宽限期 —— 文件头第 3 条说的那个取舍。
    #[test]
    fn the_idle_tail_is_cut_at_the_grace_line() {
        // 人在 t=100 走了，t=500 才结账：只该记到 100 + 宽限，后面那段空白丢掉。
        assert_eq!(settled_at(0, 100, 500), Some(100 + IDLE_GRACE));
        // 结账点来得比宽限线早时，线还够不着：整段照记（这就是慢读不被裁掉的那一侧）。
        assert_eq!(settled_at(0, 100, 300), Some(300));
    }

    #[test]
    fn a_segment_nobody_is_left_in_returns_nothing_to_write() {
        // 走得很早，宽限线还落在段首之前：这一段是空的，不该写一条零行进去。
        assert_eq!(settled_at(1_000, 100, 2_000), None);
        // 正好压在段首上也不算（写进去是一条零长的行）。
        assert_eq!(settled_at(1_000, 700, 2_000), None);
    }

    #[test]
    fn a_reader_who_is_still_reading_gets_the_full_span() {
        // 刚活动过就结账：宽限线远在 `now` 之后，取的是 `now` —— 整段都算在读。
        assert_eq!(settled_at(0, 50, 60), Some(60));
    }

    /// 时钟必须比一段熟得快。这两条常量一旦反了（改数的时候很容易），一段会在它成熟之后
    /// 的最坏一整拍里没人去结它，进程被杀时丢的就不是「最多一段」而是「最多一段 + 一拍」。
    #[test]
    fn the_clock_ticks_faster_than_a_segment_matures() {
        let tick_secs = TICK.as_secs() as i64;
        assert!(
            tick_secs <= SEGMENT_SECS,
            "时钟一拍 {tick_secs} 秒不该长过一段 {SEGMENT_SECS} 秒"
        );
        // 宽限不该长过一段：否则每一段都会先被宽限线裁一刀，`SEGMENT_SECS` 就不是
        // 「最多丢多少」那个数了。
        assert!(SEGMENT_SECS >= IDLE_GRACE);
    }
}
