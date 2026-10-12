//! 日历换算：Unix 秒 ↔ 本地日序号 ↔ `YYYY-MM-DD`。
//!
//! core 里只有阅读统计这一处需要「今天是哪天」。为它拉一棵 `chrono` / `time` 依赖树
//! 不划算，而这件事本身就是一段几十行的定点算术 —— Hinnant 的 `civil_from_days`
//! 已经是公开领域的标准解法，逐键测清楚比信任某个库的默认行为更容易。
//!
//! **时区一律是调用方给的偏移秒数**，不用系统时区：core 不该依赖进程环境，
//! 而「读者在哪个时区」是界面侧才知道的事（设备可能刚跨过日界线）。

/// 一天的秒数。放在这里，省得 `86400` 这个魔数散落到各调用点。
pub const DAY: i64 = 86_400;

/// 当前 Unix 秒。
///
/// 全项目只走这一个时钟：库里那条 `store::now` 就是它（一次转发），而阅读界面记会话
/// 起止、统计页算「今天」也读它。两处各读一遍 `SystemTime` 迟早会漂出几十秒,
/// 而那种漂移在屏幕上表现为「时长比自己算的少一点」，没有任何报错。
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 一小时的秒数。与 [`DAY`] 配对，省得 `3600` 散落到各调用点。
pub const HOUR: i64 = 3_600;

/// 一天的小时数 = 时段分布那张表的宽度（`store::reading` 的 `hour_distribution`）。
pub const HOURS_PER_DAY: usize = 24;

/// Unix 秒 + 时区偏移（秒）→ 本地小时（0..24）。
///
/// 先取**日内余数**再除：`(x / 3600) % 24` 那种写法在 `x` 为负时（偏移把时刻推过 1970
/// 年元旦之前）会算出负小时，而那个返回值直接拿去索引一张 24 格的表。
pub fn local_hour(unix_secs: i64, tz_offset_secs: i64) -> usize {
    (unix_secs + tz_offset_secs).rem_euclid(DAY) as usize / HOUR as usize
}

/// 一段时长的**分量**，不带任何文字。
///
/// 为什么只拆数不拼字符串：措辞属于文案，不属于日历算术（`plans/基础设施.md` §五.4）。
/// 中文是「1 小时 24 分钟」，英文是 `1h 24m`，日文是「1 時間 24 分」，而「不足一小时就说
/// 分钟」那条规则各语言也不一样。在这里写死一句，等于替三门语言挑了一门。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DurationParts {
    pub hours: i64,
    /// 去掉整小时之后的余下分钟。
    pub minutes: i64,
    /// 去掉整分钟之后的余下秒。
    pub seconds: i64,
    /// 不足一分钟。为 0 时界面只报分钟，不然「0 分 30 秒」这种读数比「0 分钟」还难懂。
    pub sub_minute: bool,
}

/// 秒 → 时/分/秒的分量。负数夹到 0（时长不可能是负的，但两个时刻相减可能是）。
pub fn duration_parts(secs: i64) -> DurationParts {
    let secs = secs.max(0);
    DurationParts {
        hours: secs / 3_600,
        minutes: secs / 60 % 60,
        seconds: secs % 60,
        sub_minute: secs < 60,
    }
}

/// Unix 秒 + 时区偏移（秒）→ 本地日序号（1970-01-01 是第 0 天）。
///
/// `div_euclid` 而不是 `/`：1970 年以前的秒数除以一天会往 0 取整，
/// 结果是「负数的那几天」全被并进第 0 天。
pub fn local_day(unix_secs: i64, tz_offset_secs: i64) -> i64 {
    (unix_secs + tz_offset_secs).div_euclid(DAY)
}

/// 本地日序号 → 当天 00:00 的 Unix 秒。`local_day` 的逆。
pub fn day_start(day: i64, tz_offset_secs: i64) -> i64 {
    day * DAY - tz_offset_secs
}

/// 日序号 → `YYYY-MM-DD`（公历，Hinnant `civil_from_days`）。
///
/// 不处理儒略历：Unix 时间戳的用途里没有 1582 年以前的书。
pub fn civil_date(epoch_day: i64) -> String {
    // 把纪元挪到 0000-03-01，让闰日落在年末，后面的除法才是齐的。
    let z = epoch_day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]，3 月是 0
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    //! 日历换算：已知日期、闰年、以及 1970 年前后的负日序号。

    use crate::time::{DAY, civil_date, day_start, duration_parts, local_day, local_hour};

    #[test]
    fn known_days_round_trip_to_the_right_date() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(1), "1970-01-02");
        assert_eq!(civil_date(365), "1971-01-01", "1970 不是闰年");
        assert_eq!(civil_date(10_957), "2000-01-01", "2000 是闰年");
        assert_eq!(civil_date(19_723), "2024-01-01");
        assert_eq!(civil_date(19_782), "2024-02-29", "2024 是闰年");
        assert_eq!(civil_date(-1), "1969-12-31", "负日序号也要对");
    }

    #[test]
    fn local_day_is_the_inverse_of_day_start() {
        for offset in [0, 8 * 3600, -5 * 3600, 5 * 3600 + 1800] {
            let at = 1_700_000_000i64;
            let day = local_day(at, offset);
            let start = day_start(day, offset);
            assert!(start <= at && at - start < DAY, "偏移 {offset} 落回当天内");
            assert_eq!(local_day(start, offset), day);
            assert_eq!(local_day(start - 1, offset), day - 1, "前一秒算前一天");
        }
    }

    /// 时区偏移决定日界线：同一个 Unix 秒，在东八区已经是第二天。
    #[test]
    fn the_timezone_offset_decides_which_day_it_is() {
        // 2024-01-01 00:30 UTC。
        let at = day_start(19_723, 0) + 30 * 60;
        assert_eq!(local_day(at, 0), 19_723);
        assert_eq!(local_day(at, 8 * 3600), 19_723, "东八区还没到当天 08:00");
        // 2023-12-31 23:30 UTC = 东八区 2024-01-01 07:30。
        let late = day_start(19_723, 0) - 30 * 60;
        assert_eq!(civil_date(local_day(late, 0)), "2023-12-31");
        assert_eq!(civil_date(local_day(late, 8 * 3600)), "2024-01-01");
    }

    /// 小时桶跟着偏移走，且**永远落在 0..24**。
    ///
    /// 后面那条断言钉的是越界下标：时段那张图按 24 格索引，一个负小时就是 panic。
    #[test]
    fn the_hour_follows_the_offset_and_stays_in_range() {
        // 2024-01-01 00:30 UTC。
        let at = day_start(19_723, 0) + 30 * 60;
        assert_eq!(local_hour(at, 0), 0);
        assert_eq!(local_hour(at + 8 * 3600 - 30 * 60, 0), 8, "同一天的第 8 小时");
        // 东八区看那一刻是 08:30 ⇒ 8 点。
        assert_eq!(local_hour(at, 8 * 3600), 8);
        // 西五区看那一刻是前一天 19:30 ⇒ 19 点，且不能是负数。
        assert_eq!(local_hour(at, -5 * 3600), 19);
        for offset in [0, 8 * 3600, -11 * 3600, 13 * 3600 + 45 * 60] {
            for shift in 0..48 {
                assert!(local_hour(at + shift * 3_600, offset) < 24);
            }
        }
        // 1970 年以前（负秒数）也要落在 range 里。
        assert!(local_hour(-7_000_000_000, 0) < 24);
    }

    #[test]
    fn durations_split_into_parts_without_any_words() {
        let parts = duration_parts(5_040);
        assert_eq!((parts.hours, parts.minutes, parts.seconds), (1, 24, 0));
        assert!(!parts.sub_minute);

        let just = duration_parts(45);
        assert!(just.sub_minute, "不足一分钟要能问出来，界面向这之下才说秒");
        assert_eq!((just.hours, just.minutes, just.seconds), (0, 0, 45));

        assert_eq!(duration_parts(3_600).minutes, 0, "整点不额外报 60 分");
        assert_eq!(duration_parts(-500).seconds, 0, "负时长夹到 0");
    }
}
