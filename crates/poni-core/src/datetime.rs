//! Calendar helpers. The Windows layer supplies the local wall-clock time
//! (`GetLocalTime`); everything here is pure arithmetic.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalDateTime {
    pub year: i32,
    pub month: u8, // 1..=12
    pub day: u8,   // 1..=31
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl LocalDateTime {
    /// 0 = Sunday … 6 = Saturday.
    pub fn weekday(&self) -> u8 {
        weekday_from_days(days_from_civil(self.year, self.month, self.day))
    }
    /// `YYYY-MM-DD`, used as the key for daily logs.
    pub fn day_key(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
    /// Minutes since local midnight.
    pub fn minute_of_day(&self) -> u32 {
        self.hour as u32 * 60 + self.minute as u32
    }
    /// The same wall-clock day shifted by `delta` days.
    pub fn add_days(&self, delta: i64) -> LocalDateTime {
        let (y, m, d) = civil_from_days(days_from_civil(self.year, self.month, self.day) + delta);
        LocalDateTime { year: y, month: m, day: d, ..*self }
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant's algorithm).
pub fn days_from_civil(y: i32, m: u8, d: u8) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn civil_from_days(z: i64) -> (i32, u8, u8) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

/// 0 = Sunday.
pub fn weekday_from_days(z: i64) -> u8 {
    (if z >= -4 { (z + 4) % 7 } else { (z + 5) % 7 + 6 }) as u8
}

pub fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

pub const WEEKDAY_SHORT: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];
pub const MONTH_SHORT: [&str; 12] = ["1月", "2月", "3月", "4月", "5月", "6月", "7月", "8月", "9月", "10月", "11月", "12月"];

/// "10月24日 周六"
pub fn fmt_date_short(t: &LocalDateTime) -> String {
    format!(
        "{}月{}日 {}",
        t.month,
        t.day,
        WEEKDAY_SHORT[t.weekday() as usize]
    )
}

/// "19:05" or "7:05 PM"
pub fn fmt_clock(t: &LocalDateTime, use_24h: bool) -> String {
    if use_24h {
        format!("{:02}:{:02}", t.hour, t.minute)
    } else {
        let h = match t.hour % 12 {
            0 => 12,
            h => h,
        };
        format!("{}:{:02} {}", h, t.minute, if t.hour < 12 { "AM" } else { "PM" })
    }
}

/// "3:07", "1:02:03" (negative values clamp to 0).
pub fn fmt_duration(secs: f64) -> String {
    let s = secs.max(0.0).floor() as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Countdown "24:59" — rounds up so a timer shows 25:00 at start and 0:00 only at the end.
pub fn fmt_countdown(secs: f64) -> String {
    let s = secs.max(0.0).ceil() as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// Month grid with weeks starting on Monday: 6 rows × 7 columns of day
/// numbers (`None` outside the month).
pub fn month_grid(year: i32, month: u8) -> [[Option<u8>; 7]; 6] {
    let mut grid = [[None; 7]; 6];
    let first_wd = weekday_from_days(days_from_civil(year, month, 1)); // 0 = Sun
    let offset = (first_wd as usize + 6) % 7; // Monday-first column
    let n = days_in_month(year, month) as usize;
    for day in 1..=n {
        let cell = offset + day - 1;
        grid[cell / 7][cell % 7] = Some(day as u8);
    }
    grid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrip() {
        for z in [-800_000i64, -1, 0, 1, 19_000, 20_385, 2_000_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
    }

    #[test]
    fn known_weekday() {
        // 24 Oct 2026 is a Saturday.
        let t = LocalDateTime { year: 2026, month: 10, day: 24, ..Default::default() };
        assert_eq!(t.weekday(), 6);
        assert_eq!(fmt_date_short(&t), "10月24日 周六");
        // 1 Jan 1970 was a Thursday.
        assert_eq!(weekday_from_days(0), 4);
    }

    #[test]
    fn add_days_crosses_months_and_leap_years() {
        let t = LocalDateTime { year: 2028, month: 2, day: 28, ..Default::default() };
        assert_eq!(t.add_days(1).day_key(), "2028-02-29");
        assert_eq!(t.add_days(2).day_key(), "2028-03-01");
        assert_eq!(t.add_days(-59).day_key(), "2027-12-31");
    }

    #[test]
    fn formats() {
        let t = LocalDateTime { year: 2026, month: 10, day: 24, hour: 19, minute: 5, second: 0 };
        assert_eq!(fmt_clock(&t, true), "19:05");
        assert_eq!(fmt_clock(&t, false), "7:05 PM");
        assert_eq!(fmt_duration(187.9), "3:07");
        assert_eq!(fmt_duration(3723.0), "1:02:03");
        assert_eq!(fmt_countdown(1500.0), "25:00");
        assert_eq!(fmt_countdown(1499.2), "25:00");
        assert_eq!(fmt_countdown(0.0), "00:00");
    }

    #[test]
    fn grid_layout() {
        // October 2026 starts on a Thursday → Monday-first column 3.
        let g = month_grid(2026, 10);
        assert_eq!(g[0][3], Some(1));
        assert_eq!(g[0][2], None);
        let last = g.iter().flatten().filter_map(|d| *d).max().unwrap();
        assert_eq!(last, 31);
    }
}
