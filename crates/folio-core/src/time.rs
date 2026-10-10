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

    use crate::time::{DAY, civil_date, day_start, local_day};

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
}
